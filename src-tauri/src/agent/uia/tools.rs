//! 🪟 UI 控件读取工具实现(Windows = UI Automation 客户端;其它平台 stub)。
//!
//! 三件套:read_ui_tree / find_control / click_control。控件树遍历有深度与计数上限(防自绘巨树卡死)。
//! 所有 COM 调用都在 `spawn_blocking` 闭包里「CoInitialize → 建 IUIAutomation → 用完即丢 → CoUninitialize」,
//! 不跨 await 持有 COM 句柄(IUIAutomation* 非 Send)。

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};

use crate::agent::core::{Tool, ToolDef, ToolRegistry, ToolResult};

/// 控件树遍历上限:深度与控件总数。自绘应用(Canvas/部分游戏)可能产出超大或超深树,必须截断防卡死。
const MAX_DEPTH: usize = 6;
const MAX_NODES: usize = 200;

/// 构造 UI 控件读取工具注册表。无外部上下文(直接读前台窗口)。
pub fn build_registry() -> ToolRegistry {
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(ReadUiTreeTool));
    registry.register(Arc::new(FindControlTool));
    registry.register(Arc::new(ClickControlTool));
    registry
}

/// 把 JoinError / 闭包内的 Result<String,String> 收敛成 ToolResult。
fn blocking_result(joined: Result<Result<String, String>, tokio::task::JoinError>) -> ToolResult {
    match joined {
        Ok(Ok(msg)) => ToolResult::ok(msg),
        Ok(Err(e)) => ToolResult::err(e),
        Err(e) => ToolResult::err(format!("后台任务异常: {e}")),
    }
}

struct ReadUiTreeTool;
#[async_trait]
impl Tool for ReadUiTreeTool {
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "read_ui_tree".into(),
            description: format!(
                "读取当前前台窗口的控件树(限深度 {MAX_DEPTH}、控件总数 {MAX_NODES}),\
                 返回每个控件的『名称 / 控件类型 / 是否可用』紧凑清单,供模型理解界面并据此用 find_control / click_control 操作。\
                 注:自绘应用(部分游戏 / Canvas / 某些 Electron)可能没有 UIA 控件。仅 Windows 支持。"
            ),
            input_schema: json!({ "type": "object", "properties": {} }),
        }
    }
    async fn run(&self, _args: Value) -> ToolResult {
        let joined = tokio::task::spawn_blocking(platform_uia::read_ui_tree).await;
        blocking_result(joined)
    }
}

struct FindControlTool;
#[async_trait]
impl Tool for FindControlTool {
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "find_control".into(),
            description: "在前台窗口控件树里按名称子串(忽略大小写)查找控件,返回匹配项的名称 / 类型 / 屏幕坐标(BoundingRectangle,绝对像素)。仅 Windows 支持。".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "name": { "type": "string", "description": "控件名称的子串(忽略大小写)" }
                },
                "required": ["name"]
            }),
        }
    }
    async fn run(&self, args: Value) -> ToolResult {
        let name = args.get("name").and_then(Value::as_str).unwrap_or("").trim().to_string();
        if name.is_empty() {
            return ToolResult::err("name 不能为空");
        }
        let joined = tokio::task::spawn_blocking(move || platform_uia::find_control(&name)).await;
        blocking_result(joined)
    }
}

struct ClickControlTool;
#[async_trait]
impl Tool for ClickControlTool {
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "click_control".into(),
            description: "在前台窗口里找到首个名称含指定子串的控件并点击:优先用 UIA InvokePattern 触发(更可靠),拿不到 InvokePattern 则回退到『移动到控件中心并左键点击』。仅 Windows 支持。".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "name": { "type": "string", "description": "目标控件名称的子串(忽略大小写)" }
                },
                "required": ["name"]
            }),
        }
    }
    async fn run(&self, args: Value) -> ToolResult {
        let name = args.get("name").and_then(Value::as_str).unwrap_or("").trim().to_string();
        if name.is_empty() {
            return ToolResult::err("name 不能为空");
        }
        let joined = tokio::task::spawn_blocking(move || platform_uia::click_control(&name)).await;
        blocking_result(joined)
    }
}

// ===================== Windows:UI Automation 客户端 =====================

/// 用 IUIAutomation 读前台窗口控件树并定位 / 触发控件。
/// COM 生命周期:每个工具调用在 blocking 线程内 CoInitializeEx → 建实例 → 用完 → CoUninitialize 配对。
#[cfg(windows)]
mod platform_uia {
    use windows::core::Interface;
    use windows::Win32::Foundation::{HWND, RECT};
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER,
        COINIT_APARTMENTTHREADED,
    };
    use windows::Win32::UI::Accessibility::{
        CUIAutomation, IUIAutomation, IUIAutomationElement, IUIAutomationInvokePattern,
        IUIAutomationTreeWalker, UIA_InvokePatternId,
    };
    use windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow;

    use super::{MAX_DEPTH, MAX_NODES};

    /// 进入 COM 作用域:CoInitializeEx 成功后执行 `body`,返回前 CoUninitialize 配对。
    /// 用 APARTMENTTHREADED(STA):UIA 客户端在 STA 下行为最稳。遇 RPC_E_CHANGED_MODE
    /// (线程已被别处初始化为别的套间)时不视为失败,也跳过 uninit,避免破坏既有套间。
    fn with_com<T>(body: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
        // SAFETY: 单线程内成对调用 CoInitializeEx / CoUninitialize,句柄不跨线程。
        let hr = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
        // S_OK / S_FALSE(已初始化)都算成功,需 uninit;RPC_E_CHANGED_MODE 表示套间已被占用,跳过 uninit。
        let need_uninit = hr.is_ok();
        if !hr.is_ok() && hr != windows::Win32::Foundation::RPC_E_CHANGED_MODE {
            return Err(format!("CoInitializeEx 失败: {hr:?}"));
        }
        let result = body();
        if need_uninit {
            // SAFETY: 与上面的成功 init 配对。
            unsafe { CoUninitialize() };
        }
        result
    }

    /// 建 IUIAutomation 实例 + 取前台窗口根元素 + ControlView 遍历器。三者打包返回供各工具复用。
    fn root_context() -> Result<(IUIAutomation, IUIAutomationElement, IUIAutomationTreeWalker), String> {
        // SAFETY: 标准 COM 调用,参数来自常量,返回值经 Result 检查。
        unsafe {
            let automation: IUIAutomation =
                CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER)
                    .map_err(|e| format!("创建 UIAutomation 失败: {e}"))?;
            let hwnd: HWND = GetForegroundWindow();
            if hwnd.0.is_null() {
                return Err("没有前台窗口(请先激活一个窗口)".to_string());
            }
            let root = automation
                .ElementFromHandle(hwnd)
                .map_err(|e| format!("获取前台窗口根元素失败: {e}"))?;
            // ControlView 只含对用户有意义的控件,比 RawView 干净;遍历器从 IUIAutomation 直接取。
            let walker = automation
                .ControlViewWalker()
                .map_err(|e| format!("获取控件遍历器失败: {e}"))?;
            Ok((automation, root, walker))
        }
    }

    /// 一个控件的精简快照(给模型看 / 给 click 回退用)。
    struct ControlInfo {
        name: String,
        control_type: String,
        enabled: bool,
        rect: RECT,
    }

    /// 读单个元素的快照;任一属性失败给安全缺省(不让整棵树因一个控件挂掉)。
    /// SAFETY: 调用方保证 `el` 在 COM 作用域内有效。
    unsafe fn read_info(el: &IUIAutomationElement) -> ControlInfo {
        let name = el.CurrentName().map(|b| b.to_string()).unwrap_or_default();
        let control_type = el
            .CurrentControlType()
            .map(|t| control_type_name(t.0))
            .unwrap_or_else(|_| "Unknown".to_string());
        let enabled = el.CurrentIsEnabled().map(|b| b.as_bool()).unwrap_or(false);
        let rect = el.CurrentBoundingRectangle().unwrap_or(RECT {
            left: 0,
            top: 0,
            right: 0,
            bottom: 0,
        });
        ControlInfo { name, control_type, enabled, rect }
    }

    /// UIA 控件类型 id(50000 起)→ 可读名。覆盖常见类型,其余回退 "Type<id>"。
    fn control_type_name(id: i32) -> String {
        let name = match id {
            50000 => "Button",
            50001 => "Calendar",
            50002 => "CheckBox",
            50003 => "ComboBox",
            50004 => "Edit",
            50005 => "Hyperlink",
            50006 => "Image",
            50007 => "ListItem",
            50008 => "List",
            50009 => "Menu",
            50010 => "MenuBar",
            50011 => "MenuItem",
            50012 => "ProgressBar",
            50013 => "RadioButton",
            50014 => "ScrollBar",
            50015 => "Slider",
            50016 => "Spinner",
            50017 => "StatusBar",
            50018 => "Tab",
            50019 => "TabItem",
            50020 => "Text",
            50021 => "ToolBar",
            50022 => "ToolTip",
            50023 => "Tree",
            50024 => "TreeItem",
            50025 => "Custom",
            50026 => "Group",
            50027 => "Thumb",
            50028 => "DataGrid",
            50029 => "DataItem",
            50030 => "Document",
            50031 => "SplitButton",
            50032 => "Window",
            50033 => "Pane",
            50034 => "Header",
            50035 => "HeaderItem",
            50036 => "Table",
            50037 => "TitleBar",
            50038 => "Separator",
            50039 => "SemanticZoom",
            50040 => "AppBar",
            other => return format!("Type{other}"),
        };
        name.to_string()
    }

    /// 深度优先遍历控件树,对每个节点调用 `visit`;`visit` 返回 false 即整体提前停止(命中即止)。
    /// 自身维护深度与全局计数上限,防自绘巨树把 COM 调用打满。
    /// 返回是否「被 visit 主动叫停」(true=找到目标提前结束)。
    /// SAFETY: 在 COM 作用域内调用;walker 与 element 均有效。
    unsafe fn walk(
        walker: &IUIAutomationTreeWalker,
        element: &IUIAutomationElement,
        depth: usize,
        count: &mut usize,
        visit: &mut dyn FnMut(&IUIAutomationElement, usize) -> bool,
    ) -> bool {
        if *count >= MAX_NODES {
            return false;
        }
        *count += 1;
        if !visit(element, depth) {
            return true; // visit 叫停:已命中目标,无需继续
        }
        if depth >= MAX_DEPTH {
            return false;
        }
        // 取第一个子节点,再沿兄弟链横扫;每步都受 MAX_NODES 约束。
        let mut child = match walker.GetFirstChildElement(element) {
            Ok(c) => c,
            Err(_) => return false, // 无子节点或取子失败,正常收尾
        };
        loop {
            if *count >= MAX_NODES {
                return false;
            }
            if walk(walker, &child, depth + 1, count, visit) {
                return true; // 子树里已命中,逐层上抛叫停
            }
            child = match walker.GetNextSiblingElement(&child) {
                Ok(next) => next,
                Err(_) => break, // 兄弟链到头
            };
        }
        false
    }

    /// read_ui_tree:把控件树渲染成缩进文本清单。
    pub fn read_ui_tree() -> Result<String, String> {
        with_com(|| {
            let (_automation, root, walker) = root_context()?;
            let mut lines: Vec<String> = Vec::new();
            let mut count = 0usize;
            // SAFETY: root / walker 在本 COM 作用域内有效;闭包只读元素属性。
            unsafe {
                let mut collect = |el: &IUIAutomationElement, depth: usize| -> bool {
                    let info = read_info(el);
                    // 名称为空的纯结构容器对模型噪声大,但保留其层级缩进有助理解嵌套,这里只过滤「名称空且非根」的叶子噪声
                    let name = if info.name.trim().is_empty() {
                        "(无名)".to_string()
                    } else {
                        info.name.clone()
                    };
                    let indent = "  ".repeat(depth);
                    let state = if info.enabled { "可用" } else { "禁用" };
                    lines.push(format!("{indent}- [{}] {name}（{state}）", info.control_type));
                    true // 永不叫停,遍历至上限
                };
                walk(&walker, &root, 0, &mut count, &mut collect);
            }
            if lines.is_empty() {
                return Ok("(前台窗口没有可读控件;可能是自绘应用,UIA 无控件)".to_string());
            }
            let truncated = if count >= MAX_NODES {
                format!("\n（已达 {MAX_NODES} 控件上限,可能有省略）")
            } else {
                String::new()
            };
            Ok(format!(
                "前台窗口控件树({} 个控件):\n{}{}",
                lines.len(),
                lines.join("\n"),
                truncated
            ))
        })
    }

    /// find_control:返回首个名称含子串(忽略大小写)的控件信息文本。
    pub fn find_control(needle: &str) -> Result<String, String> {
        let needle_lower = needle.to_lowercase();
        with_com(|| {
            let (_automation, root, walker) = root_context()?;
            let mut found: Option<ControlInfo> = None;
            let mut count = 0usize;
            // SAFETY: COM 作用域内;命中即把 ControlInfo 拷出(owned),不持有 COM 句柄出作用域。
            unsafe {
                let mut matcher = |el: &IUIAutomationElement, _depth: usize| -> bool {
                    let info = read_info(el);
                    if info.name.to_lowercase().contains(&needle_lower) {
                        found = Some(info);
                        return false; // 叫停遍历
                    }
                    true
                };
                walk(&walker, &root, 0, &mut count, &mut matcher);
            }
            match found {
                Some(c) => {
                    let r = c.rect;
                    Ok(format!(
                        "找到控件:名称「{}」,类型 {},{}\n屏幕坐标:left={} top={} right={} bottom={}(中心 {},{})",
                        c.name,
                        c.control_type,
                        if c.enabled { "可用" } else { "禁用" },
                        r.left,
                        r.top,
                        r.right,
                        r.bottom,
                        (r.left + r.right) / 2,
                        (r.top + r.bottom) / 2,
                    ))
                }
                None => Err(format!("未找到名称含「{needle}」的控件")),
            }
        })
    }

    /// click_control:找到首个匹配控件,优先 InvokePattern,失败回退坐标点击。
    /// 返回 (描述, 中心坐标 Option):坐标在 COM 作用域内算好带出,回退点击在作用域外用 enigo,避免 COM 句柄跨出。
    pub fn click_control(needle: &str) -> Result<String, String> {
        let needle_lower = needle.to_lowercase();
        // 第一阶段(COM 作用域内):定位控件 → 试 Invoke;成功直接返回,失败带出中心坐标供回退。
        let outcome: Result<ClickOutcome, String> = with_com(|| {
            let (_automation, root, walker) = root_context()?;
            let mut result: Option<ClickOutcome> = None;
            let mut count = 0usize;
            // SAFETY: COM 作用域内读元素 / 取 pattern;命中即处理后叫停,不带 COM 句柄出作用域。
            unsafe {
                let mut act = |el: &IUIAutomationElement, _depth: usize| -> bool {
                    let info = read_info(el);
                    if !info.name.to_lowercase().contains(&needle_lower) {
                        return true; // 继续找
                    }
                    // 命中:先试 InvokePattern。GetCurrentPattern 拿到 IUnknown 再 cast 成 InvokePattern。
                    let invoked = el
                        .GetCurrentPattern(UIA_InvokePatternId)
                        .ok()
                        .and_then(|unk| unk.cast::<IUIAutomationInvokePattern>().ok())
                        .map(|pat| pat.Invoke());
                    match invoked {
                        Some(Ok(())) => {
                            result = Some(ClickOutcome::Invoked(info.name.clone()));
                        }
                        _ => {
                            // 拿不到 InvokePattern 或 Invoke 失败:带出中心坐标,作用域外用 enigo 回退点击
                            let cx = (info.rect.left + info.rect.right) / 2;
                            let cy = (info.rect.top + info.rect.bottom) / 2;
                            result = Some(ClickOutcome::FallbackTo {
                                name: info.name.clone(),
                                x: cx,
                                y: cy,
                            });
                        }
                    }
                    false // 已处理首个匹配,叫停
                };
                walk(&walker, &root, 0, &mut count, &mut act);
            }
            result.ok_or_else(|| format!("未找到名称含「{needle}」的控件"))
        });

        // 第二阶段(COM 作用域外):Invoke 成功直接报告;回退分支用 enigo 移动并左键点击。
        match outcome? {
            ClickOutcome::Invoked(name) => Ok(format!("已通过 InvokePattern 触发控件「{name}」")),
            ClickOutcome::FallbackTo { name, x, y } => {
                fallback_click(x, y).map(|_| {
                    format!("控件「{name}」无 InvokePattern,已回退到坐标点击中心 ({x}, {y})")
                })
            }
        }
    }

    /// click_control 第一阶段的结果:要么已 Invoke 成功,要么需在作用域外按坐标回退点击。
    enum ClickOutcome {
        Invoked(String),
        FallbackTo { name: String, x: i32, y: i32 },
    }

    /// 回退点击:移动鼠标到 (x,y) 并左键单击。复用 enigo(项目已依赖),不持有 COM。
    fn fallback_click(x: i32, y: i32) -> Result<(), String> {
        use enigo::{Button, Coordinate, Direction, Enigo, Mouse, Settings};
        let mut e = Enigo::new(&Settings::default())
            .map_err(|err| format!("初始化输入设备失败: {err}"))?;
        e.move_mouse(x, y, Coordinate::Abs)
            .map_err(|err| format!("移动鼠标失败: {err}"))?;
        e.button(Button::Left, Direction::Click)
            .map_err(|err| format!("点击失败: {err}"))?;
        Ok(())
    }
}

// ===================== 非 Windows:UI Automation 不可用 =====================

/// 其它平台(macOS / Linux):UI Automation 是 Windows 专有,统一返回未实现提示。
#[cfg(not(windows))]
mod platform_uia {
    const UNSUPPORTED: &str =
        "UI 控件读取(read_ui_tree / find_control / click_control)依赖 Windows UI Automation,当前平台不支持";

    pub fn read_ui_tree() -> Result<String, String> {
        Err(UNSUPPORTED.to_string())
    }
    pub fn find_control(_needle: &str) -> Result<String, String> {
        Err(UNSUPPORTED.to_string())
    }
    pub fn click_control(_needle: &str) -> Result<String, String> {
        Err(UNSUPPORTED.to_string())
    }
}
