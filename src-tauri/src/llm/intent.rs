//! 评论意向分析:把一批评论喂给 chat 模型,解析逐条意向等级 + 理由。
//!
//! 复用通用 `chat::chat_completion`(统一超时/重试),不再自建 HTTP 客户端。

use serde_json::{json, Value};
use veltrix_core::error::Result;

use super::chat::{chat_completion, ChatRequest};
use super::http;

/// 单条评论的意向分析结果。
#[derive(Debug, Clone)]
pub struct IntentVerdict {
    pub comment_id: String,
    /// 归一化后的意向等级:high / medium / low / none。
    pub level: String,
    pub reason: String,
}

/// 一次意向分析调用的参数。集中成结构体以遵守「参数 ≤ 4」。
pub struct IntentRequest<'a> {
    pub api_url: &'a str,
    pub api_key: &'a str,
    pub model: &'a str,
    /// 系统提示词(来自 prompts 表)。
    pub system_prompt: &'a str,
    /// 本批评论:(comment_id, text)。
    pub comments: &'a [(String, String)],
}

/// 对一批评论做意向分析,返回每条判定。
/// 模型可能少返回 / 错序,调用方据 `comment_id` 对齐回写,缺失条目保持未分析。
pub async fn analyze_intent(req: IntentRequest<'_>) -> Result<Vec<IntentVerdict>> {
    if req.comments.is_empty() {
        return Ok(Vec::new());
    }

    // 把评论带 id 编号喂给模型,要求按 comment_id 原样回带,避免错位
    let listing = req
        .comments
        .iter()
        .map(|(id, text)| json!({ "comment_id": id, "text": text }))
        .collect::<Vec<_>>();
    let user_content = format!(
        "请分析下列评论作者的购买 / 咨询 / 合作意向强度,对每条给出 intent_level(必须是 \
         high/medium/low/none 之一)与简短中文 reason。只返回 JSON 数组,每项形如 \
         {{\"comment_id\":\"...\",\"intent_level\":\"...\",\"reason\":\"...\"}},不要额外解释文字。\n\
         评论列表:\n{}",
        serde_json::to_string(&listing).unwrap_or_default()
    );

    let messages = json!([
        { "role": "system", "content": req.system_prompt },
        { "role": "user", "content": user_content }
    ]);

    let content = chat_completion(ChatRequest {
        api_url: req.api_url,
        api_key: req.api_key,
        model: req.model,
        messages,
        // 低温度让分类更稳定
        extra_body: Some(json!({ "temperature": 0.2 })),
        timeout_secs: http::CHAT_TIMEOUT_SECS,
        // 评论文本体小,429/5xx 重试代价低
        retry_server_errors: true,
    })
    .await?;

    Ok(parse_verdicts(&content))
}

/// 从模型输出文本解析意向数组。容忍 ```json 代码块包裹与前后噪声:截取首个 '[' 到末个 ']'。
fn parse_verdicts(content: &str) -> Vec<IntentVerdict> {
    let slice = match (content.find('['), content.rfind(']')) {
        (Some(start), Some(end)) if end > start => &content[start..=end],
        _ => return Vec::new(),
    };
    let Ok(arr) = serde_json::from_str::<Vec<Value>>(slice) else {
        return Vec::new();
    };
    arr.iter()
        .filter_map(|v| {
            let comment_id = v.get("comment_id").and_then(Value::as_str)?.to_string();
            let level =
                normalize_level(v.get("intent_level").and_then(Value::as_str).unwrap_or("none"));
            let reason = v
                .get("reason")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            Some(IntentVerdict {
                comment_id,
                level,
                reason,
            })
        })
        .collect()
}

/// 归一化意向等级,非法 / 未知值兜底 none。
fn normalize_level(raw: &str) -> String {
    match raw.trim().to_ascii_lowercase().as_str() {
        "high" => "high",
        "medium" | "mid" => "medium",
        "low" => "low",
        _ => "none",
    }
    .to_string()
}
