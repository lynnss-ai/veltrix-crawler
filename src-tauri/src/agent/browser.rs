//! 浏览器 Agent(RPA 雏形):工具集(导航 / 点击 / 输入)+ 系统提示词。
//! ReAct 循环在 `commands::browser::send_browser_message`(便于逐步落库 + 推前端进度)。
//!
//! MVP 范围(严格):只「发出动作」不回读 DOM——Tauri `window.eval` 是 fire-and-forget、
//! 取不到返回值,故工具执行成功只代表「动作脚本已注入」,不代表元素一定命中。
//! 取 DOM 回读 / 截图多模态 / 敏感操作暂停链路均留待后续(见命令文件 leftover 注释)。

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};
use tauri::AppHandle;

use crate::llm::agent::{Tool, ToolDef, ToolRegistry, ToolResult};
use crate::webview::pool::WebviewPool;

/// 浏览器 Agent 默认起始页:`ensure_agent_window` 首次建窗需要一个合法 http/https URL;
/// 真实目标由 navigate 工具设定。取中性主页,避免落到任何平台登录态。
pub const AGENT_START_URL: &str = "https://www.bing.com/";

/// 浏览器 Agent 系统提示词(MVP 版)。
pub const SYSTEM_PROMPT: &str = "你是一个浏览器自动化 Agent,在一个可见的浏览器窗口内**亲自操作网页**来完成任务,而不是教用户怎么做。\n\
你有这些工具:navigate(导航到一个网址)、click(按 CSS 选择器点击页面元素)、type(向某个输入框写入文本)。\n\
重要约束(当前为最小可用版本):\n\
- 这些动作是「只发出、不回读」的:工具返回成功仅表示动作脚本已注入页面,**无法**确认元素是否真的命中,也读不到页面内容 / 截图。\n\
- 因此请按常识规划步骤(如:先 navigate 打开站点 → 用常见选择器 type 输入 → click 提交),并在回复里说明你做了哪些操作、用户需要在窗口里核对什么。\n\
- click / type 的 selector 用标准 CSS 选择器(如 `input[name=\"q\"]`、`#search`、`.submit-btn`)。\n\
- 全部操作完成后用简洁中文总结「做了哪些动作、用户该在浏览器窗口里看什么 / 核对什么」(此时不再调用工具)。";

/// 构造浏览器 Agent 的工具注册表。所有工具共享同一会话窗口(`ensure_agent_window` 的隔离 key)。
/// `app` 与 `pool` 均为可跨任务持有的 Clone/Arc 句柄。
pub fn build_registry(app: AppHandle, pool: Arc<WebviewPool>, session_id: String) -> ToolRegistry {
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(NavigateTool {
        app: app.clone(),
        pool: pool.clone(),
        session_id: session_id.clone(),
    }));
    registry.register(Arc::new(ClickTool {
        app: app.clone(),
        pool: pool.clone(),
        session_id: session_id.clone(),
    }));
    registry.register(Arc::new(TypeTool {
        app,
        pool,
        session_id,
    }));
    registry
}

/// 取(或首次创建)本会话的浏览器 Agent 窗口,并对其 eval 一段动作脚本。
/// 集中复用「确保窗口 + 注入脚本」逻辑;窗口被关闭后会自动重建(ensure_agent_window 内已处理)。
fn eval_in_agent_window(
    app: &AppHandle,
    pool: &Arc<WebviewPool>,
    session_id: &str,
    script: &str,
) -> ToolResult {
    let window = match pool.ensure_agent_window(app, session_id, AGENT_START_URL) {
        Ok(w) => w,
        Err(e) => return ToolResult::err(format!("打开浏览器 Agent 窗口失败: {e}")),
    };
    // eval 仅在脚本无法投递(窗口已销毁等)时报错;脚本内部命中与否取不到结果(fire-and-forget)
    match window.eval(script) {
        Ok(()) => ToolResult::ok("动作已发出(无法回读页面结果,请在浏览器窗口核对)"),
        Err(e) => ToolResult::err(format!("注入动作脚本失败(窗口可能已关闭): {e}")),
    }
}

struct NavigateTool {
    app: AppHandle,
    pool: Arc<WebviewPool>,
    session_id: String,
}
#[async_trait]
impl Tool for NavigateTool {
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "navigate".into(),
            description: "在浏览器 Agent 窗口内导航到一个网址(仅支持 http/https)".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "url": { "type": "string", "description": "要打开的完整网址,如 https://example.com" }
                },
                "required": ["url"]
            }),
        }
    }
    async fn run(&self, args: Value) -> ToolResult {
        let Some(url) = args.get("url").and_then(Value::as_str) else {
            return ToolResult::err("缺少参数 url");
        };
        let url = url.trim();
        // 协议白名单:LLM 给出的 URL 不可信,javascript:/data: 等会在窗口内执行任意脚本
        let lower = url.to_lowercase();
        if !(lower.starts_with("http://") || lower.starts_with("https://")) {
            return ToolResult::err("url 仅支持 http/https");
        }
        eval_in_agent_window(
            &self.app,
            &self.pool,
            &self.session_id,
            &crate::webview::build_navigate_eval(url),
        )
    }
}

struct ClickTool {
    app: AppHandle,
    pool: Arc<WebviewPool>,
    session_id: String,
}
#[async_trait]
impl Tool for ClickTool {
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "click".into(),
            description: "在浏览器 Agent 窗口内按 CSS 选择器点击第一个匹配元素".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "selector": { "type": "string", "description": "目标元素的 CSS 选择器,如 #submit、.btn-primary" }
                },
                "required": ["selector"]
            }),
        }
    }
    async fn run(&self, args: Value) -> ToolResult {
        let Some(selector) = args.get("selector").and_then(Value::as_str) else {
            return ToolResult::err("缺少参数 selector");
        };
        if selector.trim().is_empty() {
            return ToolResult::err("selector 不能为空");
        }
        eval_in_agent_window(
            &self.app,
            &self.pool,
            &self.session_id,
            &crate::webview::build_click_eval(selector),
        )
    }
}

struct TypeTool {
    app: AppHandle,
    pool: Arc<WebviewPool>,
    session_id: String,
}
#[async_trait]
impl Tool for TypeTool {
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "type".into(),
            description: "在浏览器 Agent 窗口内向某个输入框(CSS 选择器)写入文本".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "selector": { "type": "string", "description": "目标输入框的 CSS 选择器,如 input[name=\"q\"]" },
                    "text": { "type": "string", "description": "要写入的文本" }
                },
                "required": ["selector", "text"]
            }),
        }
    }
    async fn run(&self, args: Value) -> ToolResult {
        let Some(selector) = args.get("selector").and_then(Value::as_str) else {
            return ToolResult::err("缺少参数 selector");
        };
        if selector.trim().is_empty() {
            return ToolResult::err("selector 不能为空");
        }
        let text = args.get("text").and_then(Value::as_str).unwrap_or("");
        eval_in_agent_window(
            &self.app,
            &self.pool,
            &self.session_id,
            &crate::webview::build_type_eval(selector, text),
        )
    }
}
