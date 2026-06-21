//! Agent 地基:统一的 LLMProvider + Tool 接口(场景无关)。
//!
//! 设计目标(接口先行):上层 Agent(编程 / RPA / …)永远只面对 `LlmProvider` 与 `Tool`/`ToolRegistry`,
//! 屏蔽各厂商 function-calling / 消息 / 流式差异;换模型只改 `ProviderRef`。
//! 现状:OpenAI 兼容(DeepSeek/Qwen/MiMo/GLM/MiniMax)已实现;Anthropic 原生先占位。
//! 不影响现有 `llm::chat`(意向 / 标题 / 摘要 / 记忆提取仍走旧函数)。
#![allow(dead_code)] // Anthropic 原生实现等为占位,暂未全部启用

use async_trait::async_trait;
use serde_json::{json, Value};
use std::sync::Arc;
use veltrix_core::error::{CrawlerError, Result};

use crate::llm::http;

/// 厂商协议类型:决定请求 / 响应 / 工具 / 流式的具体格式。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProviderKind {
    OpenAiCompatible,
    Anthropic,
}

impl ProviderKind {
    /// 由厂商 code 推断协议:anthropic/claude → Anthropic,其余 → OpenAI 兼容。
    pub fn from_code(code: &str) -> Self {
        match code {
            "anthropic" | "claude" => ProviderKind::Anthropic,
            _ => ProviderKind::OpenAiCompatible,
        }
    }
}

/// 厂商引用(从 providers 表 + kind 解析得到,运行时传给 provider)。
#[derive(Clone, Debug)]
pub struct ProviderRef {
    pub kind: ProviderKind,
    pub api_url: String,
    pub api_key: String,
    pub model: String,
}

/// 工具定义(场景无关的统一格式;input_schema 为 JSON Schema)。
#[derive(Clone, Debug)]
pub struct ToolDef {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

/// 模型要求的一次工具调用。
#[derive(Clone, Debug)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: Value,
}

/// 统一消息(含工具往返 + 多模态视觉)。
#[derive(Clone, Debug)]
pub enum ChatMsg {
    System(String),
    User(String),
    /// 带图片的用户消息(多模态):`text` 为正文,`images` 为图片 data URL(`data:image/png;base64,...`)。
    /// 让走统一链路的 Agent 把截屏 / 图片喂给视觉模型(需模型具备 vision 能力)。
    UserWithImages {
        text: String,
        images: Vec<String>,
    },
    Assistant {
        text: Option<String>,
        tool_calls: Vec<ToolCall>,
    },
    Tool {
        tool_call_id: String,
        content: String,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FinishReason {
    Stop,
    ToolCalls,
    Length,
    Other,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct TokenUsage {
    pub prompt: u32,
    pub completion: u32,
}

/// 模型一次输出:文本 / 要求调用工具。
#[derive(Clone, Debug)]
pub struct LlmResponse {
    pub content: Option<String>,
    /// 模型推理内容(思考过程):Claude thinking 块 / OpenAI 兼容厂商的 reasoning_content。
    /// 仅推理型模型非空;供「思考过程」展示与落库,不参与后续对话上下文重建。
    pub reasoning: Option<String>,
    pub tool_calls: Vec<ToolCall>,
    pub finish_reason: FinishReason,
    pub usage: TokenUsage,
}

/// 调用选项(可选项,缺省由厂商默认)。
#[derive(Clone, Debug, Default)]
pub struct LlmOptions {
    pub temperature: Option<f32>,
    pub max_tokens: Option<u32>,
}

/// 一次请求(参数封装为结构体,符合「参数 ≤ 4」规范;借用避免拷贝)。
pub struct LlmRequest<'a> {
    pub provider: &'a ProviderRef,
    pub messages: &'a [ChatMsg],
    pub tools: &'a [ToolDef],
    pub options: &'a LlmOptions,
}

/// 核心接口:屏蔽各家差异。
#[async_trait]
pub trait LlmProvider: Send + Sync {
    async fn chat(&self, req: LlmRequest<'_>) -> Result<LlmResponse>;

    /// 流式 chat:文本增量经 `on_delta` 逐段回传(打字机效果),同时累积工具调用,返回完整 LlmResponse。
    /// 默认退化为非流式(整段文本经 on_delta 回传一次);OpenAI 兼容实现覆盖为真·流式 SSE 解析。
    /// 这是「流式 ReAct」统一 Agent 的底座:既要逐字输出,又要拿到 finish_reason=tool_calls 后驱动工具循环。
    async fn chat_stream(
        &self,
        req: LlmRequest<'_>,
        on_delta: &mut (dyn FnMut(String) + Send),
    ) -> Result<LlmResponse> {
        let resp = self.chat(req).await?;
        if let Some(t) = &resp.content {
            if !t.is_empty() {
                on_delta(t.clone());
            }
        }
        Ok(resp)
    }
}

/// 按 kind 取一个 provider 实现。
pub fn provider_for(kind: ProviderKind) -> Box<dyn LlmProvider> {
    match kind {
        ProviderKind::OpenAiCompatible => Box::new(OpenAiCompatibleProvider),
        ProviderKind::Anthropic => Box::new(AnthropicProvider),
    }
}

// ===================== OpenAI 兼容实现 =====================

struct OpenAiCompatibleProvider;

#[async_trait]
impl LlmProvider for OpenAiCompatibleProvider {
    async fn chat(&self, req: LlmRequest<'_>) -> Result<LlmResponse> {
        if req.provider.api_url.trim().is_empty() {
            return Err(CrawlerError::Config("厂商 api_url 为空".into()));
        }
        let endpoint = chat_endpoint(&req.provider.api_url);

        let mut body = json!({
            "model": req.provider.model,
            "messages": req.messages.iter().map(msg_to_openai).collect::<Vec<_>>(),
        });
        if !req.tools.is_empty() {
            body["tools"] = Value::Array(req.tools.iter().map(tool_to_openai).collect());
            // 显式 auto:部分 OpenAI 兼容厂商不带此字段时不主动触发工具
            body["tool_choice"] = json!("auto");
        }
        if let Some(t) = req.options.temperature {
            body["temperature"] = json!(t);
        }
        if let Some(m) = req.options.max_tokens {
            body["max_tokens"] = json!(m);
        }

        // 复用 CHAT 档共享 client(连接池保活,免每步重握手)+ 统一 429/5xx 退避重试,
        // 与 llm/chat.rs、llm/embedding.rs 一致;send_with_retry 已处理非 2xx 的错误体提取。
        let client = http::shared_client(http::CHAT_TIMEOUT_SECS)?;
        let api_key = &req.provider.api_key;
        let resp = http::send_with_retry(
            || client.post(&endpoint).bearer_auth(api_key).json(&body),
            "大模型",
            true,
        )
        .await?;
        let payload: Value = resp
            .json()
            .await
            .map_err(|e| CrawlerError::Config(format!("解析大模型响应失败: {e}")))?;
        parse_openai_response(&payload)
    }

    /// 真·流式实现:SSE 逐块解析,文本增量即时回传,工具调用按 index 累积分片(id / name 设一次,arguments 拼接)。
    async fn chat_stream(
        &self,
        req: LlmRequest<'_>,
        on_delta: &mut (dyn FnMut(String) + Send),
    ) -> Result<LlmResponse> {
        use futures_util::StreamExt;
        if req.provider.api_url.trim().is_empty() {
            return Err(CrawlerError::Config("厂商 api_url 为空".into()));
        }
        let endpoint = chat_endpoint(&req.provider.api_url);
        let mut body = json!({
            "model": req.provider.model,
            "messages": req.messages.iter().map(msg_to_openai).collect::<Vec<_>>(),
            "stream": true,
        });
        if !req.tools.is_empty() {
            body["tools"] = Value::Array(req.tools.iter().map(tool_to_openai).collect());
            body["tool_choice"] = json!("auto");
        }
        if let Some(t) = req.options.temperature {
            body["temperature"] = json!(t);
        }
        if let Some(m) = req.options.max_tokens {
            body["max_tokens"] = json!(m);
        }

        // 流式:复用 CHAT 档共享 client(连接池保活);流式重试涉及半截流,故此处不退避,仅连接层快速失败。
        let client = http::shared_client(http::CHAT_TIMEOUT_SECS)?;
        let resp = client
            .post(&endpoint)
            .bearer_auth(&req.provider.api_key)
            .json(&body)
            .send()
            .await
            .map_err(|e| CrawlerError::Config(format!("大模型请求失败: {e}")))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(CrawlerError::Config(format!(
                "大模型返回 {status}: {}",
                text.chars().take(300).collect::<String>()
            )));
        }

        let mut stream = resp.bytes_stream();
        let mut buf = String::new();
        let mut text = String::new();
        // 推理内容增量累积(reasoning_content / reasoning):推理型模型分帧返回,拼成完整思考过程
        let mut reasoning = String::new();
        // 工具调用按 index 累积:(id, name, arguments 字符串分片)
        let mut tool_acc: Vec<(String, String, String)> = Vec::new();
        let mut finish = FinishReason::Stop;
        let mut usage = TokenUsage::default();

        while let Some(chunk) = stream.next().await {
            let bytes = chunk.map_err(|e| CrawlerError::Config(format!("读取流失败: {e}")))?;
            buf.push_str(&String::from_utf8_lossy(&bytes));
            while let Some(pos) = buf.find('\n') {
                let line = buf[..pos].trim().to_string();
                buf.drain(..=pos);
                let Some(data) = line.strip_prefix("data:") else {
                    continue;
                };
                let data = data.trim();
                if data.is_empty() || data == "[DONE]" {
                    continue;
                }
                let Ok(v) = serde_json::from_str::<Value>(data) else {
                    continue;
                };
                if let Some(choice) = v.get("choices").and_then(|c| c.get(0)) {
                    if let Some(delta) = choice.get("delta") {
                        if let Some(t) = delta
                            .get("content")
                            .and_then(Value::as_str)
                            .filter(|s| !s.is_empty())
                        {
                            // 拷成 owned 再回传:避免文本增量借用 v,把 on_delta 的生命周期绑到整段流上
                            let piece = t.to_string();
                            text.push_str(&piece);
                            on_delta(piece);
                        }
                        // 推理增量:厂商字段不一,优先 reasoning_content,回退 reasoning(仅累积,不经 on_delta)
                        if let Some(r) = delta
                            .get("reasoning_content")
                            .or_else(|| delta.get("reasoning"))
                            .and_then(Value::as_str)
                            .filter(|s| !s.is_empty())
                        {
                            reasoning.push_str(r);
                        }
                        if let Some(tcs) = delta.get("tool_calls").and_then(Value::as_array) {
                            for tc in tcs {
                                // 厂商给了 index 就按 index;没给时不能一律塞 slot 0(会把并行工具调用
                                // 的参数串到一起):有非空 id 视为新调用开一个槽,否则续接最后一个槽。
                                let has_new_id = tc
                                    .get("id")
                                    .and_then(Value::as_str)
                                    .is_some_and(|s| !s.is_empty());
                                let idx = match tc.get("index").and_then(Value::as_u64) {
                                    Some(i) => i as usize,
                                    None if has_new_id || tool_acc.is_empty() => tool_acc.len(),
                                    None => tool_acc.len() - 1,
                                };
                                while tool_acc.len() <= idx {
                                    tool_acc.push((String::new(), String::new(), String::new()));
                                }
                                let slot = &mut tool_acc[idx];
                                if let Some(id) = tc.get("id").and_then(Value::as_str) {
                                    if !id.is_empty() {
                                        slot.0 = id.to_string();
                                    }
                                }
                                if let Some(f) = tc.get("function") {
                                    if let Some(n) = f.get("name").and_then(Value::as_str) {
                                        if !n.is_empty() {
                                            slot.1 = n.to_string();
                                        }
                                    }
                                    if let Some(a) = f.get("arguments").and_then(Value::as_str) {
                                        slot.2.push_str(a);
                                    }
                                }
                            }
                        }
                    }
                    if let Some(fr) = choice.get("finish_reason").and_then(Value::as_str) {
                        finish = match fr {
                            "tool_calls" => FinishReason::ToolCalls,
                            "stop" => FinishReason::Stop,
                            "length" => FinishReason::Length,
                            _ => FinishReason::Other,
                        };
                    }
                }
                // 部分厂商在流末单独给一帧 usage
                if let Some(u) = v.get("usage") {
                    usage = TokenUsage {
                        prompt: u.get("prompt_tokens").and_then(Value::as_u64).unwrap_or(0) as u32,
                        completion: u.get("completion_tokens").and_then(Value::as_u64).unwrap_or(0)
                            as u32,
                    };
                }
            }
        }

        // 累积分片 → ToolCall;arguments 字符串解析为 JSON(解析失败给空对象兜底)
        let tool_calls: Vec<ToolCall> = tool_acc
            .into_iter()
            .filter(|(_, name, _)| !name.is_empty())
            .map(|(id, name, args)| ToolCall {
                id,
                name,
                arguments: serde_json::from_str(&args).unwrap_or_else(|_| json!({})),
            })
            .collect();
        // 有工具调用但厂商没标 finish_reason 时,按 ToolCalls 处理,确保循环会执行工具
        if !tool_calls.is_empty() && finish != FinishReason::ToolCalls {
            finish = FinishReason::ToolCalls;
        }
        Ok(LlmResponse {
            content: if text.is_empty() { None } else { Some(text) },
            reasoning: if reasoning.is_empty() { None } else { Some(reasoning) },
            tool_calls,
            finish_reason: finish,
            usage,
        })
    }
}

/// 统一消息 → OpenAI 消息 JSON。
fn msg_to_openai(m: &ChatMsg) -> Value {
    match m {
        ChatMsg::System(s) => json!({ "role": "system", "content": s }),
        ChatMsg::User(s) => json!({ "role": "user", "content": s }),
        // 多模态:正文 + 各图片拼成 OpenAI content 数组(text part + image_url part)
        ChatMsg::UserWithImages { text, images } => {
            let mut parts: Vec<Value> = Vec::new();
            if !text.is_empty() {
                parts.push(json!({ "type": "text", "text": text }));
            }
            for url in images {
                parts.push(json!({ "type": "image_url", "image_url": { "url": url } }));
            }
            json!({ "role": "user", "content": parts })
        }
        ChatMsg::Assistant { text, tool_calls } => {
            let mut v = json!({ "role": "assistant", "content": text });
            if !tool_calls.is_empty() {
                v["tool_calls"] = Value::Array(
                    tool_calls
                        .iter()
                        .map(|tc| {
                            json!({
                                "id": tc.id,
                                "type": "function",
                                "function": {
                                    "name": tc.name,
                                    "arguments": tc.arguments.to_string(),
                                }
                            })
                        })
                        .collect(),
                );
            }
            v
        }
        ChatMsg::Tool { tool_call_id, content } => json!({
            "role": "tool",
            "tool_call_id": tool_call_id,
            "content": content,
        }),
    }
}

/// 工具定义 → OpenAI tool JSON。
fn tool_to_openai(t: &ToolDef) -> Value {
    json!({
        "type": "function",
        "function": {
            "name": t.name,
            "description": t.description,
            "parameters": t.input_schema,
        }
    })
}

/// 解析 OpenAI 兼容响应 → LlmResponse。
fn parse_openai_response(payload: &Value) -> Result<LlmResponse> {
    let choice = payload
        .get("choices")
        .and_then(|c| c.get(0))
        .ok_or_else(|| CrawlerError::Config("大模型未返回 choices(可能被风控或配额不足)".into()))?;
    let message = choice
        .get("message")
        .ok_or_else(|| CrawlerError::Config("大模型响应缺少 message".into()))?;

    let content = message
        .get("content")
        .and_then(Value::as_str)
        .map(str::to_string)
        .filter(|s| !s.is_empty());

    // 推理内容(思考过程):厂商字段不一,优先 reasoning_content,回退 reasoning
    let reasoning = message
        .get("reasoning_content")
        .or_else(|| message.get("reasoning"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .filter(|s| !s.is_empty());

    let tool_calls = message
        .get("tool_calls")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|tc| {
                    let id = tc.get("id").and_then(Value::as_str)?.to_string();
                    let f = tc.get("function")?;
                    let name = f.get("name").and_then(Value::as_str)?.to_string();
                    let args_str = f.get("arguments").and_then(Value::as_str).unwrap_or("{}");
                    let arguments = serde_json::from_str(args_str)
                        .unwrap_or_else(|_| Value::Object(Default::default()));
                    Some(ToolCall { id, name, arguments })
                })
                .collect()
        })
        .unwrap_or_default();

    let finish_reason = match choice.get("finish_reason").and_then(Value::as_str) {
        Some("tool_calls") => FinishReason::ToolCalls,
        Some("length") => FinishReason::Length,
        Some("stop") => FinishReason::Stop,
        _ => FinishReason::Other,
    };
    let usage = payload
        .get("usage")
        .map(|u| TokenUsage {
            prompt: u.get("prompt_tokens").and_then(Value::as_u64).unwrap_or(0) as u32,
            completion: u.get("completion_tokens").and_then(Value::as_u64).unwrap_or(0) as u32,
        })
        .unwrap_or_default();

    Ok(LlmResponse {
        content,
        reasoning,
        tool_calls,
        finish_reason,
        usage,
    })
}

/// 拼接 chat completions endpoint(api_url 已含该路径时不重复拼)。
fn chat_endpoint(api_url: &str) -> String {
    http::join_endpoint(api_url, "/chat/completions")
}

// ===================== Anthropic 原生(占位) =====================

struct AnthropicProvider;

#[async_trait]
impl LlmProvider for AnthropicProvider {
    async fn chat(&self, req: LlmRequest<'_>) -> Result<LlmResponse> {
        if req.provider.api_url.trim().is_empty() {
            return Err(CrawlerError::Config("厂商 api_url 为空".into()));
        }
        let endpoint = anthropic_endpoint(&req.provider.api_url);
        // Anthropic 与 OpenAI 差异:system 是顶层字段;工具往返用 tool_use / tool_result 内容块;
        // 一轮里多个 tool_result(+截图)须并进同一条 user 消息(user/assistant 必须交替)。
        let (system, msgs) = msgs_to_anthropic(req.messages);
        let mut body = json!({
            "model": req.provider.model,
            "max_tokens": req.options.max_tokens.unwrap_or(4096), // Anthropic 必填
            "messages": msgs,
        });
        if !system.is_empty() {
            body["system"] = json!(system);
        }
        if !req.tools.is_empty() {
            body["tools"] = Value::Array(req.tools.iter().map(tool_to_anthropic).collect());
        }
        if let Some(t) = req.options.temperature {
            body["temperature"] = json!(t);
        }

        let client = http::shared_client(http::CHAT_TIMEOUT_SECS)?;
        let api_key = &req.provider.api_key;
        let resp = http::send_with_retry(
            || {
                client
                    .post(&endpoint)
                    .header("x-api-key", api_key.as_str())
                    .header("anthropic-version", "2023-06-01")
                    .json(&body)
            },
            "Anthropic",
            true,
        )
        .await?;
        let payload: Value = resp
            .json()
            .await
            .map_err(|e| CrawlerError::Config(format!("解析 Anthropic 响应失败: {e}")))?;
        parse_anthropic_response(&payload)
    }
}

/// 拼接 Anthropic Messages endpoint:兼容用户填 base / base+/v1 / 完整 URL 三种写法。
fn anthropic_endpoint(api_url: &str) -> String {
    let trimmed = api_url.trim_end_matches('/');
    if trimmed.ends_with("/messages") {
        trimmed.to_string()
    } else if trimmed.ends_with("/v1") {
        format!("{trimmed}/messages")
    } else {
        format!("{trimmed}/v1/messages")
    }
}

/// 统一消息序列 → (system 文本, Anthropic messages 数组)。
/// 关键:相邻 user 内容(普通文本 / 图片 / tool_result)合并进同一条 user 消息,保证 user/assistant 交替。
fn msgs_to_anthropic(messages: &[ChatMsg]) -> (String, Vec<Value>) {
    let mut system = String::new();
    let mut out: Vec<Value> = Vec::new();
    // 把一个内容块追加到「当前 user 消息」:上一条已是 user 则并入,否则新开一条
    fn push_user(out: &mut Vec<Value>, block: Value) {
        if let Some(last) = out.last_mut() {
            if last.get("role").and_then(Value::as_str) == Some("user") {
                if let Some(arr) = last.get_mut("content").and_then(Value::as_array_mut) {
                    arr.push(block);
                    return;
                }
            }
        }
        out.push(json!({ "role": "user", "content": [block] }));
    }
    for m in messages {
        match m {
            ChatMsg::System(s) => {
                if !system.is_empty() {
                    system.push_str("\n\n");
                }
                system.push_str(s);
            }
            ChatMsg::User(s) => push_user(&mut out, json!({ "type": "text", "text": s })),
            ChatMsg::UserWithImages { text, images } => {
                if !text.is_empty() {
                    push_user(&mut out, json!({ "type": "text", "text": text }));
                }
                for img in images {
                    if let Some(block) = anthropic_image_block(img) {
                        push_user(&mut out, block);
                    }
                }
            }
            ChatMsg::Assistant { text, tool_calls } => {
                let mut blocks: Vec<Value> = Vec::new();
                if let Some(t) = text {
                    if !t.is_empty() {
                        blocks.push(json!({ "type": "text", "text": t }));
                    }
                }
                for tc in tool_calls {
                    blocks.push(json!({
                        "type": "tool_use",
                        "id": tc.id,
                        "name": tc.name,
                        "input": tc.arguments,
                    }));
                }
                if !blocks.is_empty() {
                    out.push(json!({ "role": "assistant", "content": blocks }));
                }
            }
            ChatMsg::Tool { tool_call_id, content } => push_user(
                &mut out,
                json!({
                    "type": "tool_result",
                    "tool_use_id": tool_call_id,
                    "content": content,
                }),
            ),
        }
    }
    (system, out)
}

/// data URL(`data:image/png;base64,...`)→ Anthropic image 块;解析失败返回 None。
fn anthropic_image_block(data_url: &str) -> Option<Value> {
    let rest = data_url.strip_prefix("data:")?;
    let (meta, data) = rest.split_once(',')?;
    let media_type = meta.strip_suffix(";base64")?;
    Some(json!({
        "type": "image",
        "source": { "type": "base64", "media_type": media_type, "data": data },
    }))
}

/// 工具定义 → Anthropic tool JSON(注意是 input_schema,非 OpenAI 的 parameters)。
fn tool_to_anthropic(t: &ToolDef) -> Value {
    json!({
        "name": t.name,
        "description": t.description,
        "input_schema": t.input_schema,
    })
}

/// 解析 Anthropic Messages 响应 → LlmResponse(text 块拼正文,tool_use 块转 ToolCall)。
fn parse_anthropic_response(payload: &Value) -> Result<LlmResponse> {
    let blocks = payload
        .get("content")
        .and_then(Value::as_array)
        .ok_or_else(|| CrawlerError::Config("Anthropic 响应缺少 content(可能被风控或鉴权失败)".into()))?;
    let mut text = String::new();
    let mut reasoning = String::new();
    let mut tool_calls: Vec<ToolCall> = Vec::new();
    for b in blocks {
        match b.get("type").and_then(Value::as_str) {
            Some("text") => {
                if let Some(t) = b.get("text").and_then(Value::as_str) {
                    text.push_str(t);
                }
            }
            // 思考块:正文在 thinking 字段(redacted_thinking 已加密,无明文可展示,跳过)
            Some("thinking") => {
                if let Some(t) = b.get("thinking").and_then(Value::as_str) {
                    reasoning.push_str(t);
                }
            }
            Some("tool_use") => {
                let id = b.get("id").and_then(Value::as_str).unwrap_or_default().to_string();
                let name = b.get("name").and_then(Value::as_str).unwrap_or_default().to_string();
                let arguments = b.get("input").cloned().unwrap_or_else(|| json!({}));
                if !name.is_empty() {
                    tool_calls.push(ToolCall { id, name, arguments });
                }
            }
            _ => {}
        }
    }
    let finish_reason = match payload.get("stop_reason").and_then(Value::as_str) {
        Some("tool_use") => FinishReason::ToolCalls,
        Some("end_turn") | Some("stop_sequence") => FinishReason::Stop,
        Some("max_tokens") => FinishReason::Length,
        _ => FinishReason::Other,
    };
    let usage = payload
        .get("usage")
        .map(|u| TokenUsage {
            prompt: u.get("input_tokens").and_then(Value::as_u64).unwrap_or(0) as u32,
            completion: u.get("output_tokens").and_then(Value::as_u64).unwrap_or(0) as u32,
        })
        .unwrap_or_default();
    Ok(LlmResponse {
        content: if text.is_empty() { None } else { Some(text) },
        reasoning: if reasoning.is_empty() { None } else { Some(reasoning) },
        tool_calls,
        finish_reason,
        usage,
    })
}

// ===================== 工具:定义 + 执行 + 注册表 =====================

/// 工具执行结果。工具内部错误编码进 `is_error`(作为 tool 结果回灌给模型),不打断 Agent 循环。
#[derive(Clone, Debug)]
pub struct ToolResult {
    pub content: String,
    pub is_error: bool,
}

impl ToolResult {
    pub fn ok(content: impl Into<String>) -> Self {
        Self { content: content.into(), is_error: false }
    }
    pub fn err(content: impl Into<String>) -> Self {
        Self { content: content.into(), is_error: true }
    }
}

/// 工具:定义(给 LLM)+ 执行。具体工具用自身字段携带上下文(如工作区路径)。
#[async_trait]
pub trait Tool: Send + Sync {
    fn def(&self) -> ToolDef;
    async fn run(&self, args: Value) -> ToolResult;
}

/// 工具注册表:按 name 提供 schema 列表 + 分发执行。
#[derive(Default)]
pub struct ToolRegistry {
    tools: Vec<Arc<dyn Tool>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self { tools: Vec::new() }
    }
    pub fn register(&mut self, tool: Arc<dyn Tool>) {
        self.tools.push(tool);
    }
    /// 合并另一个注册表的全部工具:组装「全能 Agent」时把多个独立工具模块聚合成一个注册表。
    pub fn merge(&mut self, other: ToolRegistry) {
        self.tools.extend(other.tools);
    }
    /// 所有工具定义(发给 LLM)。
    pub fn defs(&self) -> Vec<ToolDef> {
        self.tools.iter().map(|t| t.def()).collect()
    }
    /// 按 name 执行;未知工具返回 is_error 结果(回灌模型而非中断)。
    pub async fn run(&self, name: &str, args: Value) -> ToolResult {
        match self.tools.iter().find(|t| t.def().name == name) {
            Some(t) => t.run(args).await,
            None => ToolResult::err(format!("未知工具: {name}")),
        }
    }
}
