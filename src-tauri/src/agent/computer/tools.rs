//! 🧰 电脑操作 Agent:把 7 个独立工具模块聚合成一个注册表 + 系统提示词 + 危险工具判定 + 看屏幕工具。
//!
//! 这是"组装"层:desktop/fs/system/ocr/uia/net/shell 各自仍是独立模块(可被别处复用),
//! 本 agent 用 `ToolRegistry::merge` 把它们装到一起,再加一个 `capture_screen`(截屏回灌视觉模型)。
//! 危险工具(删文件/结束进程/关窗口…)由 `is_dangerous` 标出,ReAct 循环据此接入确认链路。

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};
use tauri::AppHandle;

use crate::agent::core::{Tool, ToolDef, ToolRegistry, ToolResult};

/// 电脑操作 Agent 系统提示词。
pub const SYSTEM_PROMPT: &str = "你是一个「电脑操作」智能体,在这台电脑上**亲自动手**完成任务,而不是教用户怎么做。\n\
你能用的能力(工具)分几类:\n\
- 看屏幕:capture_screen(截当前屏幕,你会直接看到画面)、screenshot(截图存文件)、ocr_screen/ocr_region(把屏幕文字识别成文本)、list_windows/read_ui_tree(读窗口与界面控件)。\n\
- 操作:mouse_move/mouse_click/mouse_scroll/mouse_drag/type_text/press_keys(鼠标键盘)、focus_window/control_window(窗口)、find_control/click_control(按控件名精确点击,比坐标可靠)、launch_program/open_path(启动程序/打开文件或URL)、read_clipboard/write_clipboard。\n\
- 文件:find_files/read_file/write_file/list_dir/file_info/copy_file/move_path/make_dir/delete_path。\n\
- 系统:list_processes/find_process/kill_process/system_info/get_env/which。\n\
- 终端 / 网络:run_command(执行终端命令)、http_request(调接口)。\n\
工作方法(重要):\n\
- **先看清再动手**:动手前先 capture_screen 或 read_ui_tree / ocr_screen 看清当前界面状态,不要凭空猜坐标或控件。\n\
- **优先用可靠定位**:点界面元素优先 find_control/click_control(按控件名),其次 read_ui_tree 给的坐标,最后才用裸坐标 mouse_click。\n\
- **危险操作要谨慎**:删除文件、结束进程、关闭窗口、启动程序这类不可逆或影响大的操作,执行前先用一句话说明你要做什么、为什么。\n\
- 每步操作后用 capture_screen / read_ui_tree 确认结果再继续,直到任务完成。\n\
- 全部完成后用简洁中文总结做了哪些操作、最终结果如何(此时不再调用工具)。";

/// 危险工具:不可逆或影响大,ReAct 循环在执行前应走确认链路(确认链路的接入点)。
pub const DANGEROUS_TOOLS: &[&str] = &[
    "delete_path",
    "kill_process",
    "write_file",
    "move_path",
    "control_window",
    "launch_program",
    "click_control",
];

/// 是否为危险工具(需执行前确认)。
pub fn is_dangerous(tool_name: &str) -> bool {
    DANGEROUS_TOOLS.contains(&tool_name)
}

/// 构造电脑操作 Agent 的完整工具注册表:聚合 7 个独立工具模块 + 看屏幕工具。
/// `app` 透传给 desktop(截图落盘目录定位)。
pub fn build_registry(app: AppHandle) -> ToolRegistry {
    let mut registry = ToolRegistry::new();
    registry.merge(crate::agent::desktop::tools::build_registry(app));
    registry.merge(crate::agent::fs::tools::build_registry());
    registry.merge(crate::agent::system::tools::build_registry());
    registry.merge(crate::agent::ocr::tools::build_registry());
    registry.merge(crate::agent::uia::tools::build_registry());
    registry.merge(crate::agent::net::tools::build_registry());
    registry.merge(crate::agent::shell::tools::build_registry());
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
