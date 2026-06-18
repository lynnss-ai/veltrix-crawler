//! 浏览器 Agent(RPA):工具集(导航 / 点击 / 输入 / 等待 / 读取页面)+ 系统提示词。
//! ReAct 循环在 `commands::browser::send_browser_message`(便于逐步落库 + 推前端进度)。
//!
//! 与早期 MVP 的区别:动作不再是 fire-and-forget。每个动作做完后**回读页面结果**
//! (命中与否 / 落地 url / title / 可见交互元素清单),经 `AgentActionChannel` 按 req_id 配对
//! 回传 Rust,工具据此把真实结果反馈给模型 —— Agent 从「盲发动作」升级为「看页面、确认、再决策」。

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{json, Value};
use tauri::AppHandle;

use crate::llm::agent::{Tool, ToolDef, ToolRegistry, ToolResult};
use crate::webview::pool::WebviewPool;
use crate::webview::{
    build_agent_click_eval, build_agent_probe_eval, build_agent_read_eval, build_agent_type_eval,
    build_agent_wait_eval, build_navigate_eval, AgentActionChannel, AgentActionOutcome,
    AGENT_READ_ELEMENT_CAP,
};

/// 浏览器 Agent 默认起始页:`ensure_agent_window` 首次建窗需要一个合法 http/https URL;
/// 真实目标由 navigate 工具设定。取中性主页,避免落到任何平台登录态。
pub const AGENT_START_URL: &str = "https://www.bing.com/";

/// navigate 后等待导航落地的时长(毫秒):assign 拆毁上下文,等页面切换 + 首屏渲染后再 probe 回读。
const NAV_SETTLE_MS: u64 = 2500;
/// navigate 落地探测(probe)等待上限(毫秒)。
const PROBE_TIMEOUT_MS: u64 = 8000;
/// click / type / read_page 等同步动作的回读等待上限(毫秒)。
const ACTION_TIMEOUT_MS: u64 = 8000;
/// wait_for 默认等待时长与上限(毫秒):防止 Agent 传入过大值把单步拖死。
const WAIT_DEFAULT_MS: u64 = 8000;
const WAIT_MAX_MS: u64 = 30000;
/// Rust 侧等待 = 页面轮询超时 + 余量(毫秒):确保页面先回传 timeout,而非 Rust 先超时。
const WAIT_RUST_BUFFER_MS: u64 = 3000;

/// 浏览器 Agent 系统提示词。
pub const SYSTEM_PROMPT: &str = "你是一个浏览器自动化 Agent,在一个可见的浏览器窗口内**亲自操作网页**来完成任务,而不是教用户怎么做。\n\
你有这些工具:\n\
- navigate(导航到一个网址):返回落地页的标题与 URL。\n\
- read_page(读取当前页面):返回标题、URL、**可见交互元素清单**(每个元素带一个稳定的 selector,形如 `[data-veltrix-id=\"3\"]`)、以及正文摘要。这是你的「眼睛」。\n\
- click(按 CSS 选择器点击元素):返回是否命中。\n\
- type(向输入框写入文本):返回是否命中。\n\
- wait_for(等待某元素出现):返回是否在超时内出现(用于等异步加载的内容)。\n\
工作方法(重要):\n\
- 这些动作现在**可回读结果**:命中与否、落地 URL、页面有哪些可操作元素你都看得到,请据此一步步推进,不要凭空猜选择器。\n\
- 标准流程:navigate 打开页面 → **read_page 看清页面有哪些元素** → 用 read_page 返回的 `[data-veltrix-id=\"N\"]` 选择器去 click / type(这种选择器最可靠)→ 操作后再 read_page 确认结果 → 直到任务完成。\n\
- 内容是异步加载、read_page 没看到目标时,用 wait_for 等它出现再操作。\n\
- click / type 也可用你自己写的标准 CSS 选择器(如 `input[name=\"q\"]`、`#search`),但优先用 read_page 给出的 data-veltrix-id 选择器。\n\
- 全部完成后用简洁中文总结「做了哪些操作、最终页面是什么、结果如何」(此时不再调用工具)。";

/// 构造浏览器 Agent 的工具注册表。所有工具共享同一会话窗口 + 同一动作回读通道。
/// `app` / `pool` / `channel` 均为可跨任务持有的 Clone/Arc 句柄。
pub fn build_registry(
    app: AppHandle,
    pool: Arc<WebviewPool>,
    channel: Arc<AgentActionChannel>,
    session_id: String,
) -> ToolRegistry {
    let ctx = AgentCtx {
        app,
        pool,
        channel,
        session_id,
    };
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(NavigateTool { ctx: ctx.clone() }));
    registry.register(Arc::new(ReadPageTool { ctx: ctx.clone() }));
    registry.register(Arc::new(ClickTool { ctx: ctx.clone() }));
    registry.register(Arc::new(TypeTool { ctx: ctx.clone() }));
    registry.register(Arc::new(WaitForTool { ctx }));
    registry
}

/// 工具共享上下文:窗口隔离 key + 回读通道 + 句柄。各工具内嵌一份(均为廉价 Clone)。
#[derive(Clone)]
struct AgentCtx {
    app: AppHandle,
    pool: Arc<WebviewPool>,
    channel: Arc<AgentActionChannel>,
    session_id: String,
}

/// 执行一次「动作 + 回读」:确保窗口 → (可选)先发 pre_eval 并等 settle → 注入回读脚本 →
/// 按 req_id 等待页面回传结果(带超时)。返回回读数据或错误描述。
///
/// pre_eval 用于 navigate(assign 会拆毁上下文,回读交给随后单独 eval 的 probe);
/// 同步动作(click/type/read)pre_eval 传 None、settle 传 0,动作脚本自身即回传。
async fn run_action(
    ctx: &AgentCtx,
    pre_eval: Option<String>,
    settle_ms: u64,
    build_eval: impl FnOnce(u64) -> String,
    timeout_ms: u64,
) -> std::result::Result<AgentActionOutcome, String> {
    let window = ctx
        .pool
        .ensure_agent_window(&ctx.app, &ctx.session_id, AGENT_START_URL)
        .map_err(|e| format!("打开浏览器 Agent 窗口失败: {e}"))?;

    if let Some(script) = pre_eval {
        window
            .eval(&script)
            .map_err(|e| format!("注入动作脚本失败(窗口可能已关闭): {e}"))?;
        if settle_ms > 0 {
            tokio::time::sleep(Duration::from_millis(settle_ms)).await;
        }
    }

    let (req_id, rx) = ctx
        .channel
        .open_action()
        .map_err(|e| format!("开启动作回读失败: {e}"))?;
    if let Err(e) = window.eval(&build_eval(req_id)) {
        ctx.channel.cancel(req_id);
        return Err(format!("注入回读脚本失败(窗口可能已关闭): {e}"));
    }

    match tokio::time::timeout(Duration::from_millis(timeout_ms), rx).await {
        Ok(Ok(outcome)) => Ok(outcome),
        Ok(Err(_)) => {
            // 发送端被 drop(理论上不会):当作无结果
            Err("动作未返回结果(页面回传通道异常)".into())
        }
        Err(_) => {
            ctx.channel.cancel(req_id);
            Err("动作超时未返回(页面可能未响应或已跳转)".into())
        }
    }
}

/// 取 JSON 对象里某字符串字段(缺省空串)。
fn data_str(data: &Value, key: &str) -> String {
    data.get(key).and_then(Value::as_str).unwrap_or("").to_string()
}

struct NavigateTool {
    ctx: AgentCtx,
}
#[async_trait]
impl Tool for NavigateTool {
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "navigate".into(),
            description: "在浏览器 Agent 窗口内导航到一个网址(仅支持 http/https),返回落地页标题与 URL".into(),
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
        match run_action(
            &self.ctx,
            Some(build_navigate_eval(url)),
            NAV_SETTLE_MS,
            build_agent_probe_eval,
            PROBE_TIMEOUT_MS,
        )
        .await
        {
            Ok(o) => {
                let title = data_str(&o.data, "title");
                let landed = data_str(&o.data, "url");
                ToolResult::ok(format!(
                    "已导航。当前页面:{}({})",
                    if title.is_empty() { "(无标题)" } else { &title },
                    if landed.is_empty() { url } else { &landed }
                ))
            }
            Err(e) => ToolResult::err(format!("导航失败:{e}")),
        }
    }
}

struct ReadPageTool {
    ctx: AgentCtx,
}
#[async_trait]
impl Tool for ReadPageTool {
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "read_page".into(),
            description: "读取当前页面:返回标题、URL、可见交互元素清单(每个带可靠的 data-veltrix-id 选择器,供 click/type 使用)与正文摘要。动手前先用它看清页面。".into(),
            input_schema: json!({ "type": "object", "properties": {} }),
        }
    }
    async fn run(&self, _args: Value) -> ToolResult {
        match run_action(
            &self.ctx,
            None,
            0,
            |rid| build_agent_read_eval(rid, AGENT_READ_ELEMENT_CAP),
            ACTION_TIMEOUT_MS,
        )
        .await
        {
            Ok(o) => ToolResult::ok(format_page(&o.data)),
            Err(e) => ToolResult::err(format!("读取页面失败:{e}")),
        }
    }
}

/// 把 read_page 回读的数据渲染成给模型看的紧凑文本。
fn format_page(data: &Value) -> String {
    let title = data_str(data, "title");
    let url = data_str(data, "url");
    let mut s = format!(
        "当前页面:{}\nURL:{}\n",
        if title.is_empty() { "(无标题)" } else { &title },
        url
    );
    let elements = data.get("elements").and_then(Value::as_array);
    match elements {
        Some(els) if !els.is_empty() => {
            s.push_str("可见交互元素(用 selector 配合 click/type):\n");
            for el in els {
                let tag = data_str(el, "tag");
                let ty = data_str(el, "type");
                let text = data_str(el, "text");
                let selector = data_str(el, "selector");
                let kind = if ty.is_empty() { tag.clone() } else { format!("{tag}:{ty}") };
                s.push_str(&format!(
                    "- [{kind}] {}  →  {selector}\n",
                    if text.is_empty() { "(无文本)" } else { &text }
                ));
            }
        }
        _ => s.push_str("(未发现可见交互元素)\n"),
    }
    let body = data_str(data, "text");
    if !body.is_empty() {
        s.push_str("正文摘要:");
        s.push_str(&body);
    }
    s
}

struct ClickTool {
    ctx: AgentCtx,
}
#[async_trait]
impl Tool for ClickTool {
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "click".into(),
            description: "在浏览器 Agent 窗口内按 CSS 选择器点击元素(优先用 read_page 给的 data-veltrix-id 选择器),返回是否命中".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "selector": { "type": "string", "description": "目标元素的 CSS 选择器,如 [data-veltrix-id=\"3\"]、#submit、.btn-primary" }
                },
                "required": ["selector"]
            }),
        }
    }
    async fn run(&self, args: Value) -> ToolResult {
        let Some(selector) = args.get("selector").and_then(Value::as_str) else {
            return ToolResult::err("缺少参数 selector");
        };
        let selector = selector.trim().to_string();
        if selector.is_empty() {
            return ToolResult::err("selector 不能为空");
        }
        let sel = selector.clone();
        match run_action(
            &self.ctx,
            None,
            0,
            move |rid| build_agent_click_eval(rid, &sel),
            ACTION_TIMEOUT_MS,
        )
        .await
        {
            Ok(o) if o.ok => {
                let tag = data_str(&o.data, "tag");
                let text = data_str(&o.data, "text");
                ToolResult::ok(format!(
                    "已点击命中元素 {selector}{}{}",
                    if tag.is_empty() { String::new() } else { format!(" <{tag}>") },
                    if text.is_empty() { String::new() } else { format!(":{text}") }
                ))
            }
            Ok(o) => {
                let err = data_str(&o.data, "error");
                if err.is_empty() {
                    ToolResult::err(format!("未找到元素 {selector},点击未执行(可先 read_page 看可用元素)"))
                } else {
                    ToolResult::err(format!("点击失败:{err}"))
                }
            }
            Err(e) => ToolResult::err(format!("点击失败:{e}")),
        }
    }
}

struct TypeTool {
    ctx: AgentCtx,
}
#[async_trait]
impl Tool for TypeTool {
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "type".into(),
            description: "在浏览器 Agent 窗口内向某个输入框(CSS 选择器)写入文本,返回是否命中".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "selector": { "type": "string", "description": "目标输入框的 CSS 选择器,如 input[name=\"q\"]、[data-veltrix-id=\"1\"]" },
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
        let selector = selector.trim().to_string();
        if selector.is_empty() {
            return ToolResult::err("selector 不能为空");
        }
        let text = args.get("text").and_then(Value::as_str).unwrap_or("").to_string();
        let sel = selector.clone();
        match run_action(
            &self.ctx,
            None,
            0,
            move |rid| build_agent_type_eval(rid, &sel, &text),
            ACTION_TIMEOUT_MS,
        )
        .await
        {
            Ok(o) if o.ok => ToolResult::ok(format!("已向 {selector} 写入文本")),
            Ok(o) => {
                let err = data_str(&o.data, "error");
                if err.is_empty() {
                    ToolResult::err(format!("未找到输入框 {selector}(可先 read_page 看可用元素)"))
                } else {
                    ToolResult::err(format!("输入失败:{err}"))
                }
            }
            Err(e) => ToolResult::err(format!("输入失败:{e}")),
        }
    }
}

struct WaitForTool {
    ctx: AgentCtx,
}
#[async_trait]
impl Tool for WaitForTool {
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "wait_for".into(),
            description: "等待某元素出现(用于等异步加载的内容),返回是否在超时内出现".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "selector": { "type": "string", "description": "要等待出现的元素 CSS 选择器" },
                    "timeout_ms": { "type": "integer", "description": "最长等待毫秒数,缺省 8000,上限 30000" }
                },
                "required": ["selector"]
            }),
        }
    }
    async fn run(&self, args: Value) -> ToolResult {
        let Some(selector) = args.get("selector").and_then(Value::as_str) else {
            return ToolResult::err("缺少参数 selector");
        };
        let selector = selector.trim().to_string();
        if selector.is_empty() {
            return ToolResult::err("selector 不能为空");
        }
        let timeout_ms = args
            .get("timeout_ms")
            .and_then(Value::as_u64)
            .unwrap_or(WAIT_DEFAULT_MS)
            .min(WAIT_MAX_MS);
        let sel = selector.clone();
        match run_action(
            &self.ctx,
            None,
            0,
            move |rid| build_agent_wait_eval(rid, &sel, timeout_ms),
            timeout_ms + WAIT_RUST_BUFFER_MS,
        )
        .await
        {
            Ok(o) if o.ok => {
                let text = data_str(&o.data, "text");
                ToolResult::ok(format!(
                    "元素已出现:{selector}{}",
                    if text.is_empty() { String::new() } else { format!("({text})") }
                ))
            }
            Ok(_) => ToolResult::err(format!("等待超时,元素未出现:{selector}")),
            Err(e) => ToolResult::err(format!("等待失败:{e}")),
        }
    }
}
