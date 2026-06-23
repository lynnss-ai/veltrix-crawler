//! 🌐 HTTP 请求工具(独立工具模块,供任意 Agent 挂载复用)。
//!
//! 复用项目已有的 reqwest(async,rustls),给 Agent 查 API / 拉取网页 / 调 webhook 的通用能力。
//! 高可用 / 高性能:超时上限、响应体截断防爆、url 协议白名单(只 http/https)。reqwest 本身 async,无需 spawn_blocking。

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{json, Value};

use crate::agent::core::{Tool, ToolDef, ToolRegistry, ToolResult};

/// 默认 / 上限超时(秒)。
const DEFAULT_TIMEOUT: u64 = 30;
const MAX_TIMEOUT: u64 = 120;
/// 响应体最多展示字符数(防超大响应刷屏 / 爆上下文)。
const BODY_CAP: usize = 8000;

/// 构造 HTTP 工具注册表。无外部上下文(每次请求新建短超时客户端)。
pub fn build_registry() -> ToolRegistry {
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(HttpRequestTool));
    registry
}

struct HttpRequestTool;
#[async_trait]
impl Tool for HttpRequestTool {
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "http_request".into(),
            description: "发起一个 HTTP 请求,返回状态码、关键响应头与响应体(截断)。仅支持 http/https。\
                适合查 REST API、拉网页、调 webhook。"
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "url": { "type": "string", "description": "完整 URL(http:// 或 https://)" },
                    "method": { "type": "string", "description": "请求方法,缺省 GET", "enum": ["GET", "POST", "PUT", "DELETE", "PATCH", "HEAD"] },
                    "headers": { "type": "object", "description": "可选:请求头键值对(值为字符串)" },
                    "body": { "type": "string", "description": "可选:请求体(POST/PUT/PATCH 用)" },
                    "timeout_secs": { "type": "integer", "description": "超时秒数,缺省 30,上限 120" }
                },
                "required": ["url"]
            }),
        }
    }
    async fn run(&self, args: Value) -> ToolResult {
        let Some(url) = args.get("url").and_then(Value::as_str) else {
            return ToolResult::err("缺少参数 url");
        };
        let url = url.trim().to_string();
        // 协议白名单:LLM 给的 url 不可信,只放行 http/https(挡掉 file:// 等本地协议)
        let lower = url.to_lowercase();
        if !(lower.starts_with("http://") || lower.starts_with("https://")) {
            return ToolResult::err("url 仅支持 http/https");
        }
        let method = args.get("method").and_then(Value::as_str).unwrap_or("GET").to_uppercase();
        let http_method = match reqwest::Method::from_bytes(method.as_bytes()) {
            Ok(m) => m,
            Err(_) => return ToolResult::err(format!("非法 method: {method}")),
        };
        let timeout = args
            .get("timeout_secs")
            .and_then(Value::as_u64)
            .unwrap_or(DEFAULT_TIMEOUT)
            .clamp(1, MAX_TIMEOUT);

        let client = match reqwest::Client::builder().timeout(Duration::from_secs(timeout)).build() {
            Ok(c) => c,
            Err(e) => return ToolResult::err(format!("构造 HTTP 客户端失败: {e}")),
        };
        let mut req = client.request(http_method, &url);
        if let Some(obj) = args.get("headers").and_then(Value::as_object) {
            for (k, v) in obj {
                if let Some(vs) = v.as_str() {
                    req = req.header(k, vs);
                }
            }
        }
        if let Some(b) = args.get("body").and_then(Value::as_str) {
            req = req.body(b.to_string());
        }

        let resp = match req.send().await {
            Ok(r) => r,
            Err(e) => return ToolResult::err(format!("请求失败: {e}")),
        };
        let status = resp.status();
        // 只回关键响应头,避免一堆噪声头刷屏
        let mut head_lines = String::new();
        for key in ["content-type", "content-length", "location"] {
            if let Some(v) = resp.headers().get(key).and_then(|h| h.to_str().ok()) {
                head_lines.push_str(&format!("{key}: {v}\n"));
            }
        }
        let text = match resp.text().await {
            Ok(t) => t,
            Err(e) => return ToolResult::err(format!("读取响应体失败: {e}")),
        };
        let body = if text.chars().count() > BODY_CAP {
            let head: String = text.chars().take(BODY_CAP).collect();
            format!("{head}\n…(响应体已截断,共 {} 字符)", text.chars().count())
        } else {
            text
        };
        ToolResult {
            content: format!("HTTP {status}\n{head_lines}\n{body}"),
            // 非 2xx 标记为 error 回灌(模型可据此重试 / 换参),但仍带完整响应
            is_error: !status.is_success(),
        }
    }
}
