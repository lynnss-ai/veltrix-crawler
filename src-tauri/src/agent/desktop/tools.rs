//! 🖱️ 桌面操作工具(独立工具模块,供任意 Agent 挂载复用)。
//!
//! 区别于浏览器 RPA(只在内嵌 webview 内操作):本模块操作**整个桌面**——
//! 截屏 / 剪贴板 / 鼠标 / 键盘 / 窗口 / 启动程序。跨平台底座:
//! enigo(鼠标键盘)、xcap(截屏 + 列窗口)、arboard(剪贴板);窗口「控制」(激活/最大化/关闭)
//! 目前仅 Windows 用 win32 实现,其它平台返回未实现提示(`list_windows` 跨平台可用)。
//!
//! 所有平台调用是同步阻塞且部分句柄非 Send,统一在 `spawn_blocking` 里「创建→用→丢」,不跨 await 持有。

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};
use tauri::{AppHandle, Manager};

use crate::agent::core::{Tool, ToolDef, ToolRegistry, ToolResult};

/// 列窗口最多返回条数(防刷屏)。
const WINDOW_LIST_CAP: usize = 60;

/// 构造桌面操作工具注册表。`app` 仅用于截图落盘目录定位。
pub fn build_registry(app: AppHandle) -> ToolRegistry {
    let ctx = DesktopCtx { app };
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(ScreenshotTool { ctx: ctx.clone() }));
    registry.register(Arc::new(ListWindowsTool));
    registry.register(Arc::new(ReadClipboardTool));
    registry.register(Arc::new(WriteClipboardTool));
    registry.register(Arc::new(MouseMoveTool));
    registry.register(Arc::new(MouseClickTool));
    registry.register(Arc::new(MouseScrollTool));
    registry.register(Arc::new(MouseDragTool));
    registry.register(Arc::new(TypeTextTool));
    registry.register(Arc::new(PressKeysTool));
    registry.register(Arc::new(FocusWindowTool));
    registry.register(Arc::new(ControlWindowTool));
    registry.register(Arc::new(LaunchProgramTool));
    registry.register(Arc::new(OpenPathTool));
    registry
}

#[derive(Clone)]
struct DesktopCtx {
    app: AppHandle,
}

/// 把 JoinError / 闭包内的 Result<String,String> 收敛成 ToolResult。
fn blocking_result(joined: Result<Result<String, String>, tokio::task::JoinError>) -> ToolResult {
    match joined {
        Ok(Ok(msg)) => ToolResult::ok(msg),
        Ok(Err(e)) => ToolResult::err(e),
        Err(e) => ToolResult::err(format!("后台任务异常: {e}")),
    }
}

// ===================== enigo:鼠标 / 键盘 =====================

/// 在 blocking 线程里新建一个 Enigo(失败给友好错误,含各平台常见原因引导)。
fn new_enigo() -> Result<enigo::Enigo, String> {
    enigo::Enigo::new(&enigo::Settings::default()).map_err(|e| {
        format!(
            "初始化输入设备失败: {e}\
             (macOS 需在 系统设置 → 隐私与安全性 → 辅助功能 授权本应用;Linux 需图形环境)"
        )
    })
}

/// 解析鼠标键名。
fn parse_button(name: &str) -> enigo::Button {
    match name.trim().to_lowercase().as_str() {
        "right" => enigo::Button::Right,
        "middle" => enigo::Button::Middle,
        _ => enigo::Button::Left,
    }
}

/// 解析单个按键名(用于 press_keys 的主键与修饰键)。返回 None 表示无法识别。
fn parse_key(name: &str) -> Option<enigo::Key> {
    use enigo::Key;
    let n = name.trim().to_lowercase();
    let key = match n.as_str() {
        "ctrl" | "control" => Key::Control,
        "alt" | "option" => Key::Alt,
        "shift" => Key::Shift,
        "cmd" | "command" | "meta" | "win" | "super" => Key::Meta,
        "enter" | "return" => Key::Return,
        "tab" => Key::Tab,
        "space" => Key::Space,
        "backspace" => Key::Backspace,
        "delete" | "del" => Key::Delete,
        "esc" | "escape" => Key::Escape,
        "up" => Key::UpArrow,
        "down" => Key::DownArrow,
        "left" => Key::LeftArrow,
        "right" => Key::RightArrow,
        "home" => Key::Home,
        "end" => Key::End,
        "pageup" | "pgup" => Key::PageUp,
        "pagedown" | "pgdn" => Key::PageDown,
        "f1" => Key::F1,
        "f2" => Key::F2,
        "f3" => Key::F3,
        "f4" => Key::F4,
        "f5" => Key::F5,
        "f6" => Key::F6,
        "f7" => Key::F7,
        "f8" => Key::F8,
        "f9" => Key::F9,
        "f10" => Key::F10,
        "f11" => Key::F11,
        "f12" => Key::F12,
        _ => {
            // 单字符按 Unicode 键处理(如 "a"、"c"、"1")
            let mut chars = n.chars();
            let c = chars.next()?;
            if chars.next().is_some() {
                return None; // 多字符且非已知键名
            }
            Key::Unicode(c)
        }
    };
    Some(key)
}

struct MouseMoveTool;
#[async_trait]
impl Tool for MouseMoveTool {
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "mouse_move".into(),
            description: "把鼠标移动到屏幕绝对坐标 (x, y)(左上角为原点)".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "x": { "type": "integer", "description": "目标 X 像素坐标" },
                    "y": { "type": "integer", "description": "目标 Y 像素坐标" }
                },
                "required": ["x", "y"]
            }),
        }
    }
    async fn run(&self, args: Value) -> ToolResult {
        let (Some(x), Some(y)) = (
            args.get("x").and_then(Value::as_i64),
            args.get("y").and_then(Value::as_i64),
        ) else {
            return ToolResult::err("缺少参数 x / y");
        };
        let joined = tokio::task::spawn_blocking(move || {
            use enigo::{Coordinate, Mouse};
            let mut e = new_enigo()?;
            e.move_mouse(x as i32, y as i32, Coordinate::Abs)
                .map_err(|err| format!("移动鼠标失败: {err}"))?;
            Ok(format!("鼠标已移动到 ({x}, {y})"))
        })
        .await;
        blocking_result(joined)
    }
}

struct MouseClickTool;
#[async_trait]
impl Tool for MouseClickTool {
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "mouse_click".into(),
            description: "在当前(或指定 x,y)位置点击鼠标。button:left/right/middle;double=true 为双击".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "button": { "type": "string", "description": "left(默认)/right/middle", "enum": ["left", "right", "middle"] },
                    "double": { "type": "boolean", "description": "是否双击(默认 false)" },
                    "x": { "type": "integer", "description": "可选:点击前先移动到的 X 坐标" },
                    "y": { "type": "integer", "description": "可选:点击前先移动到的 Y 坐标" }
                }
            }),
        }
    }
    async fn run(&self, args: Value) -> ToolResult {
        let button = parse_button(args.get("button").and_then(Value::as_str).unwrap_or("left"));
        let double = args.get("double").and_then(Value::as_bool).unwrap_or(false);
        let x = args.get("x").and_then(Value::as_i64);
        let y = args.get("y").and_then(Value::as_i64);
        let joined = tokio::task::spawn_blocking(move || {
            use enigo::{Coordinate, Direction, Mouse};
            let mut e = new_enigo()?;
            if let (Some(px), Some(py)) = (x, y) {
                e.move_mouse(px as i32, py as i32, Coordinate::Abs)
                    .map_err(|err| format!("移动鼠标失败: {err}"))?;
            }
            let clicks = if double { 2 } else { 1 };
            for _ in 0..clicks {
                e.button(button, Direction::Click)
                    .map_err(|err| format!("点击失败: {err}"))?;
            }
            Ok(format!("已{}点击", if double { "双" } else { "" }))
        })
        .await;
        blocking_result(joined)
    }
}

struct MouseScrollTool;
#[async_trait]
impl Tool for MouseScrollTool {
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "mouse_scroll".into(),
            description: "滚动鼠标滚轮。amount 为步数(正=下/右,负=上/左),axis 默认 vertical".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "amount": { "type": "integer", "description": "滚动步数,正下负上" },
                    "axis": { "type": "string", "description": "vertical(默认)/horizontal", "enum": ["vertical", "horizontal"] }
                },
                "required": ["amount"]
            }),
        }
    }
    async fn run(&self, args: Value) -> ToolResult {
        let Some(amount) = args.get("amount").and_then(Value::as_i64) else {
            return ToolResult::err("缺少参数 amount");
        };
        let horizontal = args.get("axis").and_then(Value::as_str) == Some("horizontal");
        let joined = tokio::task::spawn_blocking(move || {
            use enigo::{Axis, Mouse};
            let mut e = new_enigo()?;
            let axis = if horizontal { Axis::Horizontal } else { Axis::Vertical };
            e.scroll(amount as i32, axis)
                .map_err(|err| format!("滚动失败: {err}"))?;
            Ok(format!("已滚动 {amount} 步"))
        })
        .await;
        blocking_result(joined)
    }
}

struct MouseDragTool;
#[async_trait]
impl Tool for MouseDragTool {
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "mouse_drag".into(),
            description: "按住左键从 (from_x, from_y) 拖拽到 (to_x, to_y) 后松开".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "from_x": { "type": "integer" },
                    "from_y": { "type": "integer" },
                    "to_x": { "type": "integer" },
                    "to_y": { "type": "integer" }
                },
                "required": ["from_x", "from_y", "to_x", "to_y"]
            }),
        }
    }
    async fn run(&self, args: Value) -> ToolResult {
        let get = |k: &str| args.get(k).and_then(Value::as_i64);
        let (Some(fx), Some(fy), Some(tx), Some(ty)) =
            (get("from_x"), get("from_y"), get("to_x"), get("to_y"))
        else {
            return ToolResult::err("缺少参数 from_x/from_y/to_x/to_y");
        };
        let joined = tokio::task::spawn_blocking(move || {
            use enigo::{Button, Coordinate, Direction, Mouse};
            let mut e = new_enigo()?;
            e.move_mouse(fx as i32, fy as i32, Coordinate::Abs)
                .map_err(|err| format!("移动鼠标失败: {err}"))?;
            e.button(Button::Left, Direction::Press)
                .map_err(|err| format!("按下左键失败: {err}"))?;
            e.move_mouse(tx as i32, ty as i32, Coordinate::Abs)
                .map_err(|err| format!("拖拽移动失败: {err}"))?;
            e.button(Button::Left, Direction::Release)
                .map_err(|err| format!("松开左键失败: {err}"))?;
            Ok(format!("已从 ({fx}, {fy}) 拖拽到 ({tx}, {ty})"))
        })
        .await;
        blocking_result(joined)
    }
}

struct TypeTextTool;
#[async_trait]
impl Tool for TypeTextTool {
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "type_text".into(),
            description: "在当前焦点处输入一段文本(逐字符模拟键入,支持中文 / Unicode)".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "text": { "type": "string", "description": "要输入的文本" }
                },
                "required": ["text"]
            }),
        }
    }
    async fn run(&self, args: Value) -> ToolResult {
        let text = args.get("text").and_then(Value::as_str).unwrap_or("").to_string();
        if text.is_empty() {
            return ToolResult::err("text 不能为空");
        }
        let joined = tokio::task::spawn_blocking(move || {
            use enigo::Keyboard;
            let mut e = new_enigo()?;
            e.text(&text).map_err(|err| format!("输入文本失败: {err}"))?;
            Ok(format!("已输入 {} 个字符", text.chars().count()))
        })
        .await;
        blocking_result(joined)
    }
}

struct PressKeysTool;
#[async_trait]
impl Tool for PressKeysTool {
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "press_keys".into(),
            description: "按下一个组合键。keys 用 + 连接,如 `ctrl+c`、`alt+tab`、`ctrl+shift+s`、`enter`。\
                修饰键(ctrl/alt/shift/cmd)按住,最后一个为主键点击后再松开修饰键。"
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "keys": { "type": "string", "description": "组合键,如 ctrl+c、enter、f5、alt+tab" }
                },
                "required": ["keys"]
            }),
        }
    }
    async fn run(&self, args: Value) -> ToolResult {
        let keys = args.get("keys").and_then(Value::as_str).unwrap_or("").trim().to_string();
        if keys.is_empty() {
            return ToolResult::err("keys 不能为空");
        }
        // 解析:最后一段是主键,前面都是修饰键
        let parts: Vec<&str> = keys.split('+').map(str::trim).filter(|s| !s.is_empty()).collect();
        let Some((main_name, mod_names)) = parts.split_last() else {
            return ToolResult::err("keys 解析为空");
        };
        let Some(main_key) = parse_key(main_name) else {
            return ToolResult::err(format!("无法识别的按键: {main_name}"));
        };
        let mut mods = Vec::with_capacity(mod_names.len());
        for m in mod_names {
            match parse_key(m) {
                Some(k) => mods.push(k),
                None => return ToolResult::err(format!("无法识别的修饰键: {m}")),
            }
        }
        let joined = tokio::task::spawn_blocking(move || {
            use enigo::{Direction, Keyboard};
            let mut e = new_enigo()?;
            for m in &mods {
                e.key(*m, Direction::Press).map_err(|err| format!("按下修饰键失败: {err}"))?;
            }
            let main_res = e.key(main_key, Direction::Click).map_err(|err| format!("按主键失败: {err}"));
            // 无论主键是否成功,都要松开已按下的修饰键,避免卡键
            for m in mods.iter().rev() {
                let _ = e.key(*m, Direction::Release);
            }
            main_res?;
            Ok(format!("已按下组合键 {keys}"))
        })
        .await;
        blocking_result(joined)
    }
}

// ===================== arboard:剪贴板 =====================

struct ReadClipboardTool;
#[async_trait]
impl Tool for ReadClipboardTool {
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "read_clipboard".into(),
            description: "读取系统剪贴板里的文本".into(),
            input_schema: json!({ "type": "object", "properties": {} }),
        }
    }
    async fn run(&self, _args: Value) -> ToolResult {
        let joined = tokio::task::spawn_blocking(move || {
            let mut cb = arboard::Clipboard::new().map_err(|e| format!("打开剪贴板失败: {e}"))?;
            match cb.get_text() {
                Ok(t) if t.is_empty() => Ok("(剪贴板为空)".to_string()),
                Ok(t) => Ok(format!("剪贴板内容:\n{t}")),
                Err(e) => Err(format!("读取剪贴板失败(可能非文本): {e}")),
            }
        })
        .await;
        blocking_result(joined)
    }
}

struct WriteClipboardTool;
#[async_trait]
impl Tool for WriteClipboardTool {
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "write_clipboard".into(),
            description: "把一段文本写入系统剪贴板".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "text": { "type": "string", "description": "要写入剪贴板的文本" }
                },
                "required": ["text"]
            }),
        }
    }
    async fn run(&self, args: Value) -> ToolResult {
        let text = args.get("text").and_then(Value::as_str).unwrap_or("").to_string();
        let joined = tokio::task::spawn_blocking(move || {
            let mut cb = arboard::Clipboard::new().map_err(|e| format!("打开剪贴板失败: {e}"))?;
            cb.set_text(text.clone()).map_err(|e| format!("写入剪贴板失败: {e}"))?;
            Ok(format!("已写入剪贴板({} 字符)", text.chars().count()))
        })
        .await;
        blocking_result(joined)
    }
}

// ===================== xcap:截屏 / 列窗口 =====================

struct ScreenshotTool {
    ctx: DesktopCtx,
}
#[async_trait]
impl Tool for ScreenshotTool {
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "screenshot".into(),
            description: "截屏并保存为 PNG,返回文件路径与尺寸。target 留空=主显示器全屏;\
                填窗口标题子串=截该窗口。(macOS 需『屏幕录制』权限,否则可能截到黑屏;\
                注:本工具只产出图片文件,Agent 要『看懂』需配合视觉模型)"
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "target": { "type": "string", "description": "可选:窗口标题子串(截指定窗口);留空截主屏" }
                }
            }),
        }
    }
    async fn run(&self, args: Value) -> ToolResult {
        // 截图落盘目录:<app_data>/agent-screenshots/
        let dir = match self.ctx.app.path().app_data_dir() {
            Ok(d) => d.join("agent-screenshots"),
            Err(e) => return ToolResult::err(format!("定位数据目录失败: {e}")),
        };
        let target = args.get("target").and_then(Value::as_str).unwrap_or("").trim().to_string();
        let joined = tokio::task::spawn_blocking(move || capture_to_file(&dir, &target)).await;
        blocking_result(joined)
    }
}

/// 抓取屏幕画面。target 空=主显示器全屏;否则按窗口标题子串匹配第一个窗口。
/// 返回 xcap 的 RgbaImage(image crate),供「存文件」与「base64 回传」两条路复用。
fn grab_image(target: &str) -> Result<image::RgbaImage, String> {
    if target.is_empty() {
        let monitors = xcap::Monitor::all().map_err(|e| format!("枚举显示器失败: {e}"))?;
        let m = monitors.into_iter().next().ok_or("未找到显示器")?;
        m.capture_image().map_err(|e| format!("截屏失败: {e}"))
    } else {
        let needle = target.to_lowercase();
        let windows = xcap::Window::all().map_err(|e| format!("枚举窗口失败: {e}"))?;
        let win = windows
            .into_iter()
            .find(|w| {
                w.title()
                    .map(|t| t.to_lowercase().contains(&needle))
                    .unwrap_or(false)
            })
            .ok_or_else(|| format!("未找到标题含「{target}」的窗口"))?;
        win.capture_image().map_err(|e| format!("截窗口失败: {e}"))
    }
}

/// 截屏并保存 PNG,返回「尺寸 + 落盘路径」文本(供 screenshot 工具用)。
fn capture_to_file(dir: &std::path::Path, target: &str) -> Result<String, String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("创建截图目录失败: {e}"))?;
    let file = dir.join(format!("shot-{}.png", uuid::Uuid::new_v4()));
    let image = grab_image(target)?;
    let (w, h) = (image.width(), image.height());
    image.save(&file).map_err(|e| format!("保存 PNG 失败: {e}"))?;
    Ok(format!("已截图 {}x{},保存到:{}", w, h, file.display()))
}

/// 截屏并**回传** PNG 的 base64 data URL(`data:image/png;base64,...`),不落盘。
/// 用于把屏幕画面直接交给调用方:前端显示预览,或将来作为多模态消息喂给视觉模型。
/// target 空=主屏;否则按窗口标题子串截该窗口。(Windows 首要支持;xcap 本身跨平台)
pub fn capture_screen_data_url(target: &str) -> Result<String, String> {
    use base64::Engine;
    use image::ImageEncoder;
    let image = grab_image(target)?;
    // 在内存里编码 PNG(不落盘),再 base64 成 data URL
    let mut png: Vec<u8> = Vec::new();
    image::codecs::png::PngEncoder::new(&mut png)
        .write_image(
            image.as_raw(),
            image.width(),
            image.height(),
            image::ExtendedColorType::Rgba8,
        )
        .map_err(|e| format!("编码 PNG 失败: {e}"))?;
    let b64 = base64::engine::general_purpose::STANDARD.encode(&png);
    Ok(format!("data:image/png;base64,{b64}"))
}

struct ListWindowsTool;
#[async_trait]
impl Tool for ListWindowsTool {
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "list_windows".into(),
            description: "列出当前打开的窗口:标题、所属程序、是否最小化(供 focus_window / screenshot 按标题定位)".into(),
            input_schema: json!({ "type": "object", "properties": {} }),
        }
    }
    async fn run(&self, _args: Value) -> ToolResult {
        let joined = tokio::task::spawn_blocking(list_windows_text).await;
        blocking_result(joined)
    }
}

fn list_windows_text() -> Result<String, String> {
    let windows = xcap::Window::all().map_err(|e| format!("枚举窗口失败: {e}"))?;
    let mut lines = Vec::new();
    for w in windows {
        let title = w.title().unwrap_or_default();
        if title.trim().is_empty() {
            continue; // 跳过无标题的系统窗口
        }
        let app = w.app_name().unwrap_or_default();
        let min = if w.is_minimized().unwrap_or(false) { " [最小化]" } else { "" };
        lines.push(format!("- {title}（{app}）{min}"));
        if lines.len() >= WINDOW_LIST_CAP {
            break;
        }
    }
    if lines.is_empty() {
        return Ok("(未发现可见窗口)".to_string());
    }
    Ok(format!("当前窗口({} 个):\n{}", lines.len(), lines.join("\n")))
}

// ===================== 窗口控制(focus / 最大化 / 最小化 / 关闭) =====================

struct FocusWindowTool;
#[async_trait]
impl Tool for FocusWindowTool {
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "focus_window".into(),
            description: "把标题含指定子串的窗口切到前台并激活(Windows 用 win32;macOS 用 AppleScript,需『辅助功能』权限)".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "title": { "type": "string", "description": "目标窗口标题的子串(用 list_windows 查标题)" }
                },
                "required": ["title"]
            }),
        }
    }
    async fn run(&self, args: Value) -> ToolResult {
        let title = args.get("title").and_then(Value::as_str).unwrap_or("").trim().to_string();
        if title.is_empty() {
            return ToolResult::err("title 不能为空");
        }
        let joined = tokio::task::spawn_blocking(move || platform_window::control(&title, "focus")).await;
        blocking_result(joined)
    }
}

struct ControlWindowTool;
#[async_trait]
impl Tool for ControlWindowTool {
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "control_window".into(),
            description: "对标题含指定子串的窗口执行操作:maximize / minimize / restore / close(Windows 用 win32;macOS 用 AppleScript,需『辅助功能』权限)".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "title": { "type": "string", "description": "目标窗口标题的子串" },
                    "action": { "type": "string", "description": "maximize / minimize / restore / close", "enum": ["maximize", "minimize", "restore", "close"] }
                },
                "required": ["title", "action"]
            }),
        }
    }
    async fn run(&self, args: Value) -> ToolResult {
        let title = args.get("title").and_then(Value::as_str).unwrap_or("").trim().to_string();
        let action = args.get("action").and_then(Value::as_str).unwrap_or("").trim().to_string();
        if title.is_empty() || action.is_empty() {
            return ToolResult::err("缺少参数 title / action");
        }
        let joined = tokio::task::spawn_blocking(move || platform_window::control(&title, &action)).await;
        blocking_result(joined)
    }
}

// ===================== 启动程序 / 打开文件或 URL =====================

struct LaunchProgramTool;
#[async_trait]
impl Tool for LaunchProgramTool {
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "launch_program".into(),
            description: "启动一个本机程序(可带参数),不等待其退出。如 notepad、code、/usr/bin/firefox".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "program": { "type": "string", "description": "可执行文件路径或名称" },
                    "args": { "type": "array", "items": { "type": "string" }, "description": "可选参数列表" }
                },
                "required": ["program"]
            }),
        }
    }
    async fn run(&self, args: Value) -> ToolResult {
        let Some(program) = args.get("program").and_then(Value::as_str) else {
            return ToolResult::err("缺少参数 program");
        };
        let program = program.trim().to_string();
        if program.is_empty() {
            return ToolResult::err("program 不能为空");
        }
        let extra: Vec<String> = args
            .get("args")
            .and_then(Value::as_array)
            .map(|arr| arr.iter().filter_map(|v| v.as_str().map(String::from)).collect())
            .unwrap_or_default();
        let joined = tokio::task::spawn_blocking(move || {
            std::process::Command::new(&program)
                .args(&extra)
                .spawn()
                .map(|child| format!("已启动 {program}(pid {})", child.id()))
                .map_err(|e| format!("启动程序失败: {e}"))
        })
        .await;
        blocking_result(joined)
    }
}

struct OpenPathTool;
#[async_trait]
impl Tool for OpenPathTool {
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "open_path".into(),
            description: "用系统默认方式打开文件 / 文件夹 / URL(Windows→start,macOS→open,Linux→xdg-open)".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "target": { "type": "string", "description": "文件 / 目录路径或 URL" }
                },
                "required": ["target"]
            }),
        }
    }
    async fn run(&self, args: Value) -> ToolResult {
        let Some(target) = args.get("target").and_then(Value::as_str) else {
            return ToolResult::err("缺少参数 target");
        };
        let target = target.trim().to_string();
        if target.is_empty() {
            return ToolResult::err("target 不能为空");
        }
        let joined = tokio::task::spawn_blocking(move || {
            let result = if cfg!(windows) {
                // 直接 spawn explorer.exe(target 作单个 argv,不经 cmd):避免 `cmd /C start`
                // 让 cmd 重新解析 & | > ^ 等元字符导致命令注入(BatBadBut / CVE-2024-24576 类)。
                // explorer 不是 shell,对文件/文件夹/URL 都按关联程序打开。
                std::process::Command::new("explorer.exe").arg(&target).spawn()
            } else if cfg!(target_os = "macos") {
                std::process::Command::new("open").arg(&target).spawn()
            } else {
                std::process::Command::new("xdg-open").arg(&target).spawn()
            };
            result
                .map(|_| format!("已用默认程序打开:{target}"))
                .map_err(|e| format!("打开失败: {e}"))
        })
        .await;
        blocking_result(joined)
    }
}

// ===================== 平台相关:窗口控制 =====================

/// Windows:用 win32 按标题子串找顶层可见窗口并控制(focus / 最大化 / 最小化 / 还原 / 关闭)。
#[cfg(windows)]
mod platform_window {
    use windows::core::BOOL;
    use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
    use windows::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetWindowTextW, IsWindowVisible, PostMessageW, SetForegroundWindow,
        ShowWindow, SW_MAXIMIZE, SW_MINIMIZE, SW_RESTORE, WM_CLOSE,
    };

    struct FindState {
        needle: String,
        found: Option<HWND>,
    }

    unsafe extern "system" fn enum_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let state = &mut *(lparam.0 as *mut FindState);
        if !IsWindowVisible(hwnd).as_bool() {
            return BOOL(1); // 继续枚举
        }
        let mut buf = [0u16; 512];
        let len = GetWindowTextW(hwnd, &mut buf);
        if len > 0 {
            let title = String::from_utf16_lossy(&buf[..len as usize]).to_lowercase();
            if title.contains(&state.needle) {
                state.found = Some(hwnd);
                return BOOL(0); // 命中,停止枚举
            }
        }
        BOOL(1)
    }

    fn find_window(needle: &str) -> Option<HWND> {
        let mut state = FindState {
            needle: needle.to_lowercase(),
            found: None,
        };
        unsafe {
            let _ = EnumWindows(Some(enum_proc), LPARAM(&mut state as *mut _ as isize));
        }
        state.found
    }

    pub fn control(title: &str, action: &str) -> Result<String, String> {
        let hwnd = find_window(title).ok_or_else(|| format!("未找到标题含「{title}」的窗口"))?;
        unsafe {
            match action {
                "focus" => {
                    let _ = SetForegroundWindow(hwnd);
                    Ok(format!("已激活窗口「{title}」"))
                }
                "maximize" => {
                    let _ = ShowWindow(hwnd, SW_MAXIMIZE);
                    Ok(format!("已最大化窗口「{title}」"))
                }
                "minimize" => {
                    let _ = ShowWindow(hwnd, SW_MINIMIZE);
                    Ok(format!("已最小化窗口「{title}」"))
                }
                "restore" => {
                    let _ = ShowWindow(hwnd, SW_RESTORE);
                    Ok(format!("已还原窗口「{title}」"))
                }
                "close" => {
                    PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0))
                        .map_err(|e| format!("关闭窗口失败: {e}"))?;
                    Ok(format!("已请求关闭窗口「{title}」"))
                }
                other => Err(format!("不支持的窗口操作: {other}")),
            }
        }
    }
}

/// macOS:用 AppleScript(osascript)经 System Events 按标题子串找窗口并控制。
/// 需用户在「系统设置 → 隐私与安全性 → 辅助功能」授权本应用,否则 System Events 报错。
/// 注:Mac 无 Windows 式「最大化」,maximize 映射为绿钮 zoom(AXZoomWindow,行为依 App 而定);未实机验证。
#[cfg(target_os = "macos")]
mod platform_window {
    use std::process::Command;

    /// 转义 AppleScript 字符串字面量里的反斜杠与双引号(needle 来自 LLM,不可信)。
    fn escape_as(s: &str) -> String {
        s.replace('\\', "\\\\").replace('"', "\\\"")
    }

    pub fn control(title: &str, action: &str) -> Result<String, String> {
        // 把 action 映射成对 theWin 的 AppleScript 操作语句(focus 含两句)
        let op = match action {
            "focus" => "set frontmost of theProc to true\nperform action \"AXRaise\" of theWin",
            "minimize" => "set value of attribute \"AXMinimized\" of theWin to true",
            "restore" => "set value of attribute \"AXMinimized\" of theWin to false",
            "maximize" => "perform action \"AXZoomWindow\" of theWin",
            "close" => {
                "perform action \"AXPress\" of (first button of theWin whose subrole is \"AXCloseButton\")"
            }
            other => return Err(format!("不支持的窗口操作: {other}")),
        };
        let needle = escape_as(title);
        // 遍历有 UI 的进程及其窗口,命中标题子串即执行操作;返回 ok / notfound 供 Rust 侧判断
        let script = [
            format!("set theNeedle to \"{needle}\""),
            "tell application \"System Events\"".to_string(),
            "repeat with theProc in (application processes whose background only is false)".to_string(),
            "repeat with theWin in (windows of theProc)".to_string(),
            "if name of theWin contains theNeedle then".to_string(),
            op.to_string(),
            "return \"ok\"".to_string(),
            "end if".to_string(),
            "end repeat".to_string(),
            "end repeat".to_string(),
            "end tell".to_string(),
            "return \"notfound\"".to_string(),
        ]
        .join("\n");

        let out = Command::new("osascript")
            .arg("-e")
            .arg(&script)
            .output()
            .map_err(|e| format!("调用 osascript 失败: {e}"))?;
        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr);
            return Err(format!(
                "窗口操作失败(若提示未授权,请到 系统设置 → 隐私与安全性 → 辅助功能 勾选本应用): {}",
                stderr.trim()
            ));
        }
        if String::from_utf8_lossy(&out.stdout).trim() == "ok" {
            Ok(format!("已对窗口「{title}」执行 {action}"))
        } else {
            Err(format!("未找到标题含「{title}」的窗口"))
        }
    }
}

/// 其它平台(Linux 等):窗口控制暂未实现(list_windows 仍可用)。
#[cfg(not(any(windows, target_os = "macos")))]
mod platform_window {
    pub fn control(_title: &str, _action: &str) -> Result<String, String> {
        Err("当前平台暂未实现窗口控制(focus / 最大化 / 关闭),可用 list_windows 查看窗口".to_string())
    }
}
