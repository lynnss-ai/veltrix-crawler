//! 通用 OpenAI 兼容 chat completions。
//!
//! 5 家厂商(DeepSeek / Qwen / MiMo / GLM / MiniMax)均 OpenAI 兼容,差异仅 base url。
//! 意向分析与语音识别(MiMo 走 input_audio chat)都复用本实现;能力特有字段经 `extra_body` 注入。

use serde_json::{json, Value};
use std::sync::Arc;
use veltrix_core::error::{CrawlerError, Result};

use super::http;

/// Token 用量(从 API 响应的 usage 字段解析)。
#[derive(Clone, Copy, Debug, Default)]
pub struct TokenUsage {
    pub prompt: u32,
    pub completion: u32,
}

/// 一次 chat 调用的参数(遵守「参数 ≤ 4」封装为结构体)。
pub struct ChatRequest<'a> {
    /// 厂商 base url(如 `https://api.deepseek.com`),内部拼 `/chat/completions`。
    pub api_url: &'a str,
    pub api_key: &'a str,
    pub model: &'a str,
    /// messages 数组(JSON);content 可为字符串或多模态数组(承载 input_audio)。
    pub messages: Value,
    /// 浅合并进请求体的额外字段(如 temperature、asr_options)。
    pub extra_body: Option<Value>,
    /// 请求总超时(秒):普通 chat 用 http::CHAT_TIMEOUT_SECS,语音转写用 http::ASR_TIMEOUT_SECS。
    pub timeout_secs: u64,
    /// 是否对 429/5xx 重试:普通 chat 体小可开;ASR 体大(base64 音频)应关,避免重复上传/计费。
    pub retry_server_errors: bool,
}

/// 拼接 chat completions endpoint;api_url 已含该路径(用户填了完整 URL)时不重复拼。
fn chat_endpoint(api_url: &str) -> String {
    http::join_endpoint(api_url, "/chat/completions")
}

/// chat completion 的完整产出:正文 + token 用量。
pub struct ChatOutcome {
    pub content: String,
    pub usage: TokenUsage,
}

/// 调用 `{api_url}/chat/completions`,返回正文与 token 用量。
pub async fn chat_completion(req: ChatRequest<'_>) -> Result<ChatOutcome> {
    if req.api_url.trim().is_empty() {
        return Err(CrawlerError::Config("厂商 api_url 为空".into()));
    }
    let endpoint = chat_endpoint(req.api_url);

    let mut body = json!({ "model": req.model, "messages": req.messages });
    // extra_body 浅合并:把 temperature / asr_options 等并入顶层。
    // 禁止覆盖 model / messages 这两个核心字段,避免调用方误传导致请求被改写。
    if let Some(extra) = req.extra_body {
        if let (Some(obj), Some(extra_obj)) = (body.as_object_mut(), extra.as_object()) {
            for (key, value) in extra_obj {
                if key == "model" || key == "messages" {
                    continue;
                }
                obj.insert(key.clone(), value.clone());
            }
        }
    }

    let client = http::shared_client(req.timeout_secs)?;
    let resp = http::send_with_retry(
        || client.post(&endpoint).bearer_auth(req.api_key).json(&body),
        "大模型 chat",
        req.retry_server_errors,
    )
    .await?;

    let payload: Value = resp
        .json()
        .await
        .map_err(|e| CrawlerError::Config(format!("解析大模型响应失败: {e}")))?;

    // 解析 token 用量(部分厂商可能不返回 usage 字段,默认 0)
    let usage = payload
        .get("usage")
        .map(|u| TokenUsage {
            prompt: u.get("prompt_tokens").and_then(Value::as_u64).unwrap_or(0) as u32,
            completion: u.get("completion_tokens").and_then(Value::as_u64).unwrap_or(0) as u32,
        })
        .unwrap_or_default();

    // OpenAI 兼容:choices[0].message.content 为模型输出文本。
    // 分步诊断:空 choices 多为被风控拦截 / 配额不足,与"缺 content"区分,便于排查。
    match payload.get("choices").and_then(Value::as_array) {
        Some(arr) if !arr.is_empty() => {
            let content = arr[0]
                .get("message")
                .and_then(|m| m.get("content"))
                .and_then(Value::as_str)
                .map(str::to_string)
                .ok_or_else(|| CrawlerError::Config("大模型响应缺少 message.content".into()))?;
            Ok(ChatOutcome { content, usage })
        }
        _ => Err(CrawlerError::Config(
            "大模型未返回 choices(可能被风控拦截或配额不足)".into(),
        )),
    }
}

/// 流式 chat 的完整产出:正文 + 思考过程(推理型模型才有 reasoning) + token 用量。
pub struct StreamOutcome {
    pub content: String,
    pub reasoning: Option<String>,
    pub usage: TokenUsage,
}

/// 流式 chat:请求体带 `stream:true`,按 SSE 逐块解析 `choices[0].delta`,正文走 content、
/// 思考过程走 reasoning_content/reasoning。每拿到一段增量就调 `on_delta(kind, piece)`
///(kind ∈ {"content","reasoning"},供命令层按类型 emit 给前端),返回拼接后的正文与完整思考过程。
///
/// 不走 send_with_retry:流式响应需按字节流读取且重试语义复杂,这里单次请求即可。
///
/// `cancel_flag` 为可选的取消标志(AtomicBool);设置后会在下一个 chunk 到达时中断流式读取,
/// 返回已累积的内容(不报错)。
pub async fn chat_completion_stream<F>(
    req: ChatRequest<'_>,
    mut on_delta: F,
    cancel_flag: Option<Arc<std::sync::atomic::AtomicBool>>,
) -> Result<StreamOutcome>
where
    F: FnMut(&str, &str),
{
    use futures_util::StreamExt;

    if req.api_url.trim().is_empty() {
        return Err(CrawlerError::Config("厂商 api_url 为空".into()));
    }
    let endpoint = chat_endpoint(req.api_url);

    // stream_options.include_usage:让 OpenAI 兼容厂商在流末单独发一帧 usage,
    // 否则严格实现的网关不返回 usage,账单只能记 0(下方 SSE 解析已处理该帧)。
    let mut body = json!({
        "model": req.model,
        "messages": req.messages,
        "stream": true,
        "stream_options": { "include_usage": true },
    });
    if let Some(extra) = req.extra_body {
        if let (Some(obj), Some(extra_obj)) = (body.as_object_mut(), extra.as_object()) {
            for (key, value) in extra_obj {
                if key == "model" || key == "messages" || key == "stream" {
                    continue;
                }
                obj.insert(key.clone(), value.clone());
            }
        }
    }

    let client = http::shared_client(req.timeout_secs)?;
    let resp = client
        .post(&endpoint)
        .bearer_auth(req.api_key)
        .json(&body)
        .send()
        .await
        .map_err(|e| CrawlerError::Config(format!("大模型请求失败: {e}")))?;

    // 非 2xx:读出错误体便于排查(配额 / 鉴权 / 模型名错等)
    if !resp.status().is_success() {
        let status = resp.status();
        let code = status.as_u16();
        let msg = match code {
            401 => "大模型鉴权失败:API Key 无效或已过期".to_string(),
            402 => "大模型账户余额不足,请充值后重试".to_string(),
            403 => "大模型访问被拒绝:可能无权使用该模型".to_string(),
            429 => "大模型请求过于频繁,请稍后再试".to_string(),
            _ => {
                let text = resp.text().await.unwrap_or_default();
                format!(
                    "大模型返回 {status}: {}",
                    text.chars().take(300).collect::<String>()
                )
            }
        };
        return Err(CrawlerError::Config(msg));
    }

    let mut stream = resp.bytes_stream();
    let mut full = String::new();
    let mut reasoning = String::new();
    let mut buf = String::new();
    let mut cancelled = false;
    let mut usage = TokenUsage::default();

    // 每处理一批 chunk 后检查取消标志
    let check_cancel = |flag: &Option<Arc<std::sync::atomic::AtomicBool>>| -> bool {
        if let Some(f) = flag {
            f.load(std::sync::atomic::Ordering::Relaxed)
        } else {
            false
        }
    };

    while let Some(chunk) = stream.next().await {
        // 检查取消标志
        if check_cancel(&cancel_flag) {
            cancelled = true;
            break;
        }
        let bytes = chunk.map_err(|e| CrawlerError::Config(format!("读取流失败: {e}")))?;
        buf.push_str(&String::from_utf8_lossy(&bytes));
        // 按行处理 SSE;一行可能跨多个网络包,故只处理含完整换行的部分
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
            if let Ok(v) = serde_json::from_str::<Value>(data) {
                // 部分厂商在流末单独给一帧 usage(如 OpenAI stream_options.include_usage)
                if let Some(u) = v.get("usage") {
                    usage = TokenUsage {
                        prompt: u.get("prompt_tokens").and_then(Value::as_u64).unwrap_or(0) as u32,
                        completion: u.get("completion_tokens").and_then(Value::as_u64).unwrap_or(0) as u32,
                    };
                }
                let delta = v
                    .get("choices")
                    .and_then(|c| c.get(0))
                    .and_then(|c| c.get("delta"));
                let Some(delta) = delta else { continue };
                if let Some(piece) = delta.get("content").and_then(Value::as_str) {
                    if !piece.is_empty() {
                        full.push_str(piece);
                        on_delta("content", piece);
                    }
                }
                // 思考过程增量:字段不一,优先 reasoning_content,回退 reasoning
                if let Some(piece) = delta
                    .get("reasoning_content")
                    .or_else(|| delta.get("reasoning"))
                    .and_then(Value::as_str)
                {
                    if !piece.is_empty() {
                        reasoning.push_str(piece);
                        on_delta("reasoning", piece);
                    }
                }
            }
        }
    }

    // 用户主动取消时，即使内容为空也返回成功（不报错）
    if cancelled {
        return Ok(StreamOutcome {
            content: if full.is_empty() { "(已停止)".to_string() } else { full },
            reasoning: if reasoning.is_empty() {
                None
            } else {
                Some(reasoning)
            },
            usage,
        });
    }

    // 仅有思考过程而无正文也算异常(正常收尾必有正文);两者皆空才报错
    if full.is_empty() {
        return Err(CrawlerError::Config(
            "大模型未返回内容(可能被风控拦截或配额不足)".into(),
        ));
    }
    Ok(StreamOutcome {
        content: full,
        reasoning: if reasoning.is_empty() {
            None
        } else {
            Some(reasoning)
        },
        usage,
    })
}
