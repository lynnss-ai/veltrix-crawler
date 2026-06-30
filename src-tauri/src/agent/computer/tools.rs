//! 🧰 电脑操作 Agent(GUI 自动化):聚合「看屏 + 操作 GUI」三个模块成一个注册表 + 系统提示词 + 危险工具判定 + 看屏幕工具。
//!
//! 这是"组装"层:desktop(鼠标键盘/窗口/剪贴板/启程序)/ ocr(读屏文字)/ uia(控件)各自仍是独立模块(可被别处复用),
//! 本 agent 用 `ToolRegistry::merge` 把它们装到一起,再加一个 `capture_screen`(截屏回灌视觉模型)。
//! 文件 / 进程 / 终端 / 网络 类工具已拆到「本机助手(local)」与「rpa」,不在本 agent。
//! 危险工具(关窗口/启动程序/点控件)由 `is_dangerous` 标出,ReAct 循环据此接入确认链路。

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};
use tauri::AppHandle;

use crate::agent::core::{Tool, ToolDef, ToolRegistry, ToolResult};

/// 电脑操作 Agent 系统提示词(GUI 自动化:看屏 + 操作鼠标键盘/窗口/控件)。
pub const SYSTEM_PROMPT: &str = "你是一个「电脑操作」智能体,通过**看屏幕 + 操作鼠标键盘 / 窗口 / 控件**在这台电脑上亲自完成 GUI 任务,而不是教用户怎么做。\n\
你能用的能力(工具)分两类:\n\
- 看屏幕:capture_screen(截当前屏幕,你会直接看到画面)、screenshot(截图存文件)、ocr_screen/ocr_region(把屏幕文字识别成文本)、list_windows/read_ui_tree(读窗口与界面控件)。\n\
- 操作:mouse_move/mouse_click/mouse_scroll/mouse_drag/type_text/press_keys(鼠标键盘)、focus_window/control_window(窗口)、find_control/click_control(按控件名精确点击,比坐标可靠)、launch_program/open_path(启动程序/打开文件或URL)、read_clipboard/write_clipboard。\n\
工作方法(重要):\n\
- **先吃透目标**:动手前弄清用户到底要达成什么、怎样算完成(界面到了某状态?程序已就绪?信息已看到?)。有歧义且影响大方向时先简短确认一句。\n\
- **先看清再动手**:动手前先 capture_screen 或 read_ui_tree / ocr_screen 看清当前界面状态,不要凭空猜坐标或控件。\n\
- **优先可靠定位**:点界面元素优先 find_control/click_control(按控件名),其次 read_ui_tree 给的坐标,最后才用裸坐标 mouse_click。\n\
- **每步必核对**:每个操作后用 capture_screen / read_ui_tree 确认真的生效(窗口变了、文本填进去了)再继续;没生效就换方式重试,不要想当然往下走。\n\
- **危险操作谨慎**:关闭窗口、启动程序、点击可能触发不可逆动作的控件,执行前先用一句话说明要做什么、为什么。\n\
- **真正达成、不留半成品**:要把用户目标实际落地(界面已到位、程序已就绪、信息已得出),而不是「演示了几步」就停;中途失败要么修复、要么如实说明卡在哪、缺什么。\n\
- **范围之外的请转交**:读写 / 删除本机文件、查看 / 结束进程、跑终端命令、调 HTTP 接口这类**不看屏幕**的任务不在本智能体范围,请提示用户改用「本机助手」;网页内自动化请用「rpa」。\n\
- 全部完成后用简洁中文总结:做了哪些操作、最终结果如何(此时不再调用工具)。";

/// 危险工具:不可逆或影响大,ReAct 循环在执行前应走确认链路(确认链路的接入点)。
/// 仅含本 agent(GUI)实际挂载的工具;文件 / 进程类危险工具随模块拆走,见 local agent。
pub const DANGEROUS_TOOLS: &[&str] = &[
    "control_window",
    "launch_program",
    "click_control",
];

/// 是否为危险工具(需执行前确认)。
pub fn is_dangerous(tool_name: &str) -> bool {
    DANGEROUS_TOOLS.contains(&tool_name)
}

/// 构造电脑操作 Agent(GUI)的工具注册表:聚合 desktop + ocr + uia + 看屏幕工具。
/// 文件 / 进程 / 终端 / 网络 类工具已拆走(见 local / rpa agent)。
/// `app` 透传给 desktop(截图落盘目录定位)。
pub fn build_registry(app: AppHandle) -> ToolRegistry {
    let mut registry = ToolRegistry::new();
    registry.merge(crate::agent::desktop::tools::build_registry(app));
    registry.merge(crate::agent::ocr::tools::build_registry());
    registry.merge(crate::agent::uia::tools::build_registry());
    registry.register(Arc::new(CaptureScreenTool));
    registry
}

/// 看屏幕:截主屏 → data URL 放进 ToolResult.content。
/// 约定:`commands::send_computer_message` 的 ReAct 循环拦截 capture_screen,取出 data URL 作
/// `ChatMsg::UserWithImages` 注入下一轮(让视觉模型看到画面),tool 消息本身只落库简短文本。
struct CaptureScreenTool;
#[async_trait]
impl Tool for CaptureScreenTool {
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "capture_screen".into(),
            description: "看一眼当前桌面屏幕:调用后你会在随后的消息里直接看到屏幕画面(图片)。需模型具备视觉能力。".into(),
            input_schema: json!({ "type": "object", "properties": {} }),
        }
    }
    async fn run(&self, _args: Value) -> ToolResult {
        let joined =
            tokio::task::spawn_blocking(|| crate::agent::desktop::tools::capture_screen_data_url(""))
                .await;
        match joined {
            Ok(Ok(data_url)) => ToolResult::ok(data_url),
            Ok(Err(e)) => ToolResult::err(format!("截屏失败: {e}")),
            Err(e) => ToolResult::err(format!("截屏任务异常: {e}")),
        }
    }
}
