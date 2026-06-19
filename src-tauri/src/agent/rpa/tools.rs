//! 浏览器 Agent(RPA):工具集(导航 / 点击 / 输入 / 等待 / 读取页面 / 看接口)+ 系统提示词。
//! ReAct 循环在 `agent::rpa::commands::send_browser_message`(便于逐步落库 + 推前端进度)。
//!
//! 与早期 MVP 的区别:
//! - **不再弹独立窗口**:动作作用于内嵌主窗口右栏的 `agent` 子 webview(`WebviewPool::ensure_agent_webview`)。
//! - **回读不靠页面 invoke**:动作脚本是同步 IIFE 返回对象,经 `script_eval::eval_json`
//!   (WebView2 ExecuteScript)直接取回 JSON,任意域名可用(原 invoke 回传受 capabilities 限制)。
//! - **能看接口**:子 webview 装了全量原生拦截,`get_network` 工具让 Agent 读到页面发出的 JSON 响应。

use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde_json::{json, Value};
use tauri::AppHandle;

use crate::agent::core::{Tool, ToolDef, ToolRegistry, ToolResult};
use crate::webview::pool::WebviewPool;
use crate::webview::script_eval;
use crate::webview::{
    build_agent_click_eval, build_agent_exists_eval, build_agent_probe_eval, build_agent_read_eval,
    build_agent_type_eval, build_navigate_eval, InterceptedResponse, AGENT_READ_ELEMENT_CAP,
};

/// 浏览器 Agent 默认起始页:`ensure_agent_webview` 首次建 webview 需要一个合法 http/https URL;
/// 真实目标由 navigate 工具设定。取中性主页,避免落到任何平台登录态。
pub const AGENT_START_URL: &str = "https://www.bing.com/";

/// navigate 后等待导航落地的时长(毫秒):assign 拆毁上下文,等页面切换 + 首屏渲染后再 probe 回读。
const NAV_SETTLE_MS: u64 = 2500;
/// wait_for 默认等待时长与上限(毫秒):防止 Agent 传入过大值把单步拖死。
const WAIT_DEFAULT_MS: u64 = 8000;
const WAIT_MAX_MS: u64 = 30000;
/// wait_for 轮询间隔(毫秒):Rust 侧按此间隔反复检查元素是否出现。
const WAIT_POLL_MS: u64 = 300;
/// get_network 最多展示的接口响应条数与单条响应体截断长度。
const NET_SHOW_MAX: usize = 8;
const NET_BODY_CAP: usize = 1000;

/// 浏览器 Agent 系统提示词。
pub const SYSTEM_PROMPT: &str = "你是一个浏览器自动化 Agent,在应用右侧一个可见的内嵌浏览器里**亲自操作网页**来完成任务,而不是教用户怎么做。\n\
你有这些工具:\n\
- navigate(导航到一个网址):返回落地页的标题与 URL。\n\
- read_page(读取当前页面):返回标题、URL、**可见交互元素清单**(每个元素带一个稳定的 selector,形如 `[data-veltrix-id=\"3\"]`)、以及正文摘要。这是你的「眼睛」。\n\
- click(按 CSS 选择器点击元素):返回是否命中。\n\
- type(向输入框写入文本):返回是否命中。\n\
- wait_for(等待某元素出现):返回是否在超时内出现(用于等异步加载的内容)。\n\
- get_network(查看页面发出的接口响应):返回页面已加载的 XHR/fetch 的 JSON 响应(可用 url_contains 过滤),用于直接读取页面背后的数据而非只看 DOM。\n\
- capture_screen(看一眼当前桌面屏幕):调用后你会在随后的消息里**直接看到屏幕画面图片**,用于确认页面真实渲染效果、或查看浏览器窗口之外的桌面内容(需当前模型具备视觉能力)。\n\
工作方法(重要):\n\
- 这些动作**可回读结果**:命中与否、落地 URL、页面有哪些可操作元素、以及接口返回了什么数据你都看得到,请据此一步步推进,不要凭空猜选择器。\n\
- 标准流程:navigate 打开页面 → **read_page 看清页面有哪些元素** → 用 read_page 返回的 `[data-veltrix-id=\"N\"]` 选择器去 click / type(这种选择器最可靠)→ 操作后再 read_page 确认结果 → 直到任务完成。\n\
- 需要页面背后的结构化数据(列表、详情等)时,用 get_network 看页面自己发出的接口响应,往往比抠 DOM 更准。\n\
- 内容是异步加载、read_page 没看到目标时,用 wait_for 等它出现再操作。\n\
- click / type 也可用你自己写的标准 CSS 选择器(如 `input[name=\"q\"]`、`#search`),但优先用 read_page 给出的 data-veltrix-id 选择器。\n\
- 全部完成后用简洁中文总结「做了哪些操作、最终页面是什么、结果如何」(此时不再调用工具)。";

/// 构造浏览器 Agent 的工具注册表。所有工具共享同一会话子 webview(按 conversation_id 隔离)。
pub fn build_registry(app: AppHandle, pool: Arc<WebviewPool>, conversation_id: String) -> ToolRegistry {
    let ctx = AgentCtx {
        app,
        pool,
        conversation_id,
    };
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(NavigateTool { ctx: ctx.clone() }));
    registry.register(Arc::new(ReadPageTool { ctx: ctx.clone() }));
    registry.register(Arc::new(ClickTool { ctx: ctx.clone() }));
    registry.register(Arc::new(TypeTool { ctx: ctx.clone() }));
    registry.register(Arc::new(WaitForTool { ctx: ctx.clone() }));
    registry.register(Arc::new(GetNetworkTool { ctx }));
    registry.register(Arc::new(CaptureScreenTool));
    registry
}

/// 工具共享上下文:窗口隔离 key(会话 id)+ 句柄。各工具内嵌一份(均为廉价 Clone)。
#[derive(Clone)]
struct AgentCtx {
    app: AppHandle,
    pool: Arc<WebviewPool>,
    conversation_id: String,
}

/// 执行一次「(可选)前置动作 + 同步回读」:确保子 webview → (可选)先 eval pre_eval 并等 settle →
/// 用 ExecuteScript 跑同步回读脚本拿回对象。返回回读到的 JSON Value 或错误描述。
///
/// pre_eval 用于 navigate(assign 会拆毁上下文,回读交给随后单独执行的 probe);
/// 同步动作(click/type/read/exists)pre_eval 传 None、settle 传 0,回读脚本自身即返回结果。
async fn eval_action(
    ctx: &AgentCtx,
    pre_eval: Option<String>,
    settle_ms: u64,
    read_js: &str,
) -> std::result::Result<Value, String> {
    let webview = ctx
        .pool
        .ensure_agent_webview(&ctx.app, &ctx.conversation_id, AGENT_START_URL)
        .map_err(|e| format!("打开浏览器 Agent 失败: {e}"))?;

    if let Some(script) = pre_eval {
        webview
            .eval(&script)
            .map_err(|e| format!("注入动作脚本失败(webview 可能已关闭): {e}"))?;
        if settle_ms > 0 {
            tokio::time::sleep(Duration::from_millis(settle_ms)).await;
        }
    }

    let raw = script_eval::eval_json(&webview, read_js)
        .await
        .ok_or_else(|| "页面未响应或回读超时".to_string())?;
    serde_json::from_str::<Value>(&raw).map_err(|e| {
        let head: String = raw.chars().take(200).collect();
        format!("回读结果解析失败: {e}(原始: {head})")
    })
}

/// 取 JSON 对象里某字符串字段(缺省空串)。
fn val_str(data: &Value, key: &str) -> String {
    data.get(key).and_then(Value::as_str).unwrap_or("").to_string()
}

/// 取 JSON 对象里某布尔字段(缺省 false)。
fn val_bool(data: &Value, key: &str) -> bool {
    data.get(key).and_then(Value::as_bool).unwrap_or(false)
}

/// 按字符截断(保 UTF-8 边界),给模型看的回读结果防刷屏。
fn truncate_chars(s: &str, cap: usize) -> String {
    if s.chars().count() <= cap {
        s.to_string()
    } else {
        let head: String = s.chars().take(cap).collect();
        format!("{head}…(已截断)")
    }
}

struct NavigateTool {
    ctx: AgentCtx,
}
#[async_trait]
impl Tool for NavigateTool {
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "navigate".into(),
            description: "在内嵌浏览器里导航到一个网址(仅支持 http/https),返回落地页标题与 URL".into(),
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
        // 协议白名单:LLM 给出的 URL 不可信,javascript:/data: 等会在 webview 内执行任意脚本
        let lower = url.to_lowercase();
        if !(lower.starts_with("http://") || lower.starts_with("https://")) {
            return ToolResult::err("url 仅支持 http/https");
        }
        match eval_action(
            &self.ctx,
            Some(build_navigate_eval(url)),
            NAV_SETTLE_MS,
            &build_agent_probe_eval(),
        )
        .await
        {
            Ok(v) => {
                if let Some(err) = v.get("error").and_then(Value::as_str) {
                    return ToolResult::err(format!("导航失败:{err}"));
                }
                let title = val_str(&v, "title");
                let landed = val_str(&v, "url");
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
        match eval_action(&self.ctx, None, 0, &build_agent_read_eval(AGENT_READ_ELEMENT_CAP)).await {
            Ok(v) => {
                if let Some(err) = v.get("error").and_then(Value::as_str) {
                    return ToolResult::err(format!("读取页面失败:{err}"));
                }
                ToolResult::ok(format_page(&v))
            }
            Err(e) => ToolResult::err(format!("读取页面失败:{e}")),
        }
    }
}

/// 把 read_page 回读的数据渲染成给模型看的紧凑文本。
fn format_page(data: &Value) -> String {
    let title = val_str(data, "title");
    let url = val_str(data, "url");
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
                let tag = val_str(el, "tag");
                let ty = val_str(el, "type");
                let text = val_str(el, "text");
                let selector = val_str(el, "selector");
                let kind = if ty.is_empty() { tag.clone() } else { format!("{tag}:{ty}") };
                s.push_str(&format!(
                    "- [{kind}] {}  →  {selector}\n",
                    if text.is_empty() { "(无文本)" } else { &text }
                ));
            }
        }
        _ => s.push_str("(未发现可见交互元素)\n"),
    }
    let body = val_str(data, "text");
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
            description: "在内嵌浏览器里按 CSS 选择器点击元素(优先用 read_page 给的 data-veltrix-id 选择器),返回是否命中".into(),
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
        match eval_action(&self.ctx, None, 0, &build_agent_click_eval(&selector)).await {
            Ok(v) => {
                if let Some(err) = v.get("error").and_then(Value::as_str) {
                    return ToolResult::err(format!("点击失败:{err}"));
                }
                if val_bool(&v, "matched") {
                    let tag = val_str(&v, "tag");
                    let text = val_str(&v, "text");
                    ToolResult::ok(format!(
                        "已点击命中元素 {selector}{}{}",
                        if tag.is_empty() { String::new() } else { format!(" <{tag}>") },
                        if text.is_empty() { String::new() } else { format!(":{text}") }
                    ))
                } else {
                    ToolResult::err(format!("未找到元素 {selector},点击未执行(可先 read_page 看可用元素)"))
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
            description: "在内嵌浏览器里向某个输入框(CSS 选择器)写入文本,返回是否命中".into(),
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
        match eval_action(&self.ctx, None, 0, &build_agent_type_eval(&selector, &text)).await {
            Ok(v) => {
                if let Some(err) = v.get("error").and_then(Value::as_str) {
                    return ToolResult::err(format!("输入失败:{err}"));
                }
                if val_bool(&v, "matched") {
                    ToolResult::ok(format!("已向 {selector} 写入文本"))
                } else {
                    ToolResult::err(format!("未找到输入框 {selector}(可先 read_page 看可用元素)"))
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
        // Rust 侧轮询:ExecuteScript 不 await Promise,故不在页面内 setTimeout 轮询,改这里按间隔反复检查。
        let exists_js = build_agent_exists_eval(&selector);
        let deadline = Instant::now() + Duration::from_millis(timeout_ms);
        loop {
            match eval_action(&self.ctx, None, 0, &exists_js).await {
                Ok(v) => {
                    if let Some(err) = v.get("error").and_then(Value::as_str) {
                        return ToolResult::err(format!("等待失败:{err}"));
                    }
                    if val_bool(&v, "matched") {
                        let text = val_str(&v, "text");
                        return ToolResult::ok(format!(
                            "元素已出现:{selector}{}",
                            if text.is_empty() { String::new() } else { format!("({text})") }
                        ));
                    }
                }
                Err(e) => return ToolResult::err(format!("等待失败:{e}")),
            }
            if Instant::now() >= deadline {
                return ToolResult::err(format!("等待超时,元素未出现:{selector}"));
            }
            tokio::time::sleep(Duration::from_millis(WAIT_POLL_MS)).await;
        }
    }
}

struct GetNetworkTool {
    ctx: AgentCtx,
}
#[async_trait]
impl Tool for GetNetworkTool {
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "get_network".into(),
            description: "查看当前页面已发出的接口(XHR/fetch)JSON 响应,可选 url_contains 过滤。用于直接读取页面背后的结构化数据。".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "url_contains": { "type": "string", "description": "可选:只看 URL 含此子串的响应(如 'api'、'/search')" }
                }
            }),
        }
    }
    async fn run(&self, args: Value) -> ToolResult {
        let needle = args
            .get("url_contains")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_lowercase();
        let Some(sink) = self.ctx.pool.agent_sink(&self.ctx.conversation_id) else {
            return ToolResult::ok("(尚无拦截到的接口响应。先 navigate 打开页面并触发其数据加载)");
        };
        // 锁内拷贝出匹配项,尽快释放锁(无 await)
        let matched: Vec<InterceptedResponse> = match sink.lock() {
            Ok(buf) => buf
                .iter()
                .filter(|r| needle.is_empty() || r.url.to_lowercase().contains(&needle))
                .cloned()
                .collect(),
            Err(_) => return ToolResult::err("读取拦截缓冲失败(锁异常)"),
        };
        if matched.is_empty() {
            return ToolResult::ok(format!(
                "(没有匹配的接口响应{})",
                if needle.is_empty() { "" } else { ",试试去掉或更换过滤词" }
            ));
        }
        let total = matched.len();
        let shown = total.min(NET_SHOW_MAX);
        let mut s = format!("拦截到 {total} 条接口响应,显示最近 {shown} 条:\n\n");
        for r in matched.iter().rev().take(NET_SHOW_MAX).rev() {
            s.push_str(&format!("● {}\n{}\n\n", r.url, truncate_chars(&r.body, NET_BODY_CAP)));
        }
        ToolResult::ok(s)
    }
}

/// 看屏幕:截取当前桌面主屏,把 PNG 的 base64 data URL 放进 ToolResult.content。
/// 约定:`commands::send_browser_message` 的 ReAct 循环会**拦截 capture_screen**,
/// 取出 data URL 作为 `ChatMsg::UserWithImages` 注入下一轮(让视觉模型看到画面),
/// 而 tool 消息本身只落库简短文本(不存超长 base64)。
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
        // 截屏是阻塞调用,放 blocking 线程;复用 desktop 模块的截屏→data URL 能力
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
