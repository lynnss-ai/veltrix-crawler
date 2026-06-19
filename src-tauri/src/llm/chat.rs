//! 通用 OpenAI 兼容 chat completions。
//!
//! 5 家厂商(DeepSeek / Qwen / MiMo / GLM / MiniMax)均 OpenAI 兼容,差异仅 base url。
//! 意向分析与语音识别(MiMo 走 input_audio chat)都复用本实现;能力特有字段经 `extra_body` 注入。

use serde_json::{json, Value};
use veltrix_core::error::{CrawlerError, Result};

use super::http;

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
    let trimmed = api_url.trim_end_matches('/');
    if trimmed.ends_with("/chat/completions") {
        trimmed.to_string()
    } else {
        format!("{trimmed}/chat/completions")
    }
}

/// 调用 `{api_url}/chat/completions`,返回 `choices[0].message.content`。
pub async fn chat_completion(req: ChatRequest<'_>) -> Result<String> {
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

    // OpenAI 兼容:choices[0].message.content 为模型输出文本。
    // 分步诊断:空 choices 多为被风控拦截 / 配额不足,与"缺 content"区分,便于排查。
    match payload.get("choices").and_then(Value::as_array) {
        Some(arr) if !arr.is_empty() => arr[0]
            .get("message")
            .and_then(|m| m.get("content"))
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| CrawlerError::Config("大模型响应缺少 message.content".into())),
        _ => Err(CrawlerError::Config(
            "大模型未返回 choices(可能被风控拦截或配额不足)".into(),
        )),
    }
}

/// 流式 chat:请求体带 `stream:true`,按 SSE 逐块解析 `choices[0].delta.content`,
/// 每拿到一段增量就调 `on_delta`(供命令层 emit 给前端实现打字机效果),返回拼接后的完整文本。
///
/// 不走 send_with_retry:流式响应需按字节流读取且重试语义复杂,这里单次请求即可。
pub async fn chat_completion_stream<F>(req: ChatRequest<'_>, mut on_delta: F) -> Result<String>
where
    F: FnMut(&str),
{
    use futures_util::StreamExt;

    if req.api_url.trim().is_empty() {
        return Err(CrawlerError::Config("厂商 api_url 为空".into()));
    }
    let endpoint = chat_endpoint(req.api_url);

    let mut body = json!({ "model": req.model, "messages": req.messages, "stream": true });
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
        let text = resp.text().await.unwrap_or_default();
        return Err(CrawlerError::Config(format!(
            "大模型返回 {status}: {}",
            text.chars().take(300).collect::<String>()
        )));
    }

    let mut stream = resp.bytes_stream();
    let mut full = String::new();
    let mut buf = String::new();
    while let Some(chunk) = stream.next().await {
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
                if let Some(delta) = v
                    .get("choices")
                    .and_then(|c| c.get(0))
                    .and_then(|c| c.get("delta"))
                    .and_then(|d| d.get("content"))
                    .and_then(Value::as_str)
                {
                    if !delta.is_empty() {
                        full.push_str(delta);
                        on_delta(delta);
                    }
                }
            }
        }
    }

    if full.is_empty() {
        return Err(CrawlerError::Config(
            "大模型未返回内容(可能被风控拦截或配额不足)".into(),
        ));
    }
    Ok(full)
}
