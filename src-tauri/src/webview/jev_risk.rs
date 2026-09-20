//! 评论直采结果含糊时,让 Jev 判断是否值得在新页面环境中再试一次。
//! 只传枚举和计数;模型不接触接口响应、页面正文、URL 或登录信息。

use std::time::Duration;

use serde_json::{json, Value};

const JEV_ENDPOINT: &str = "https://api.typesafe.ai/v1/systemone";
const JEV_MODEL: &str = "jev-1.13.0";
const MIN_PROBABILITY: f64 = 0.80;

pub fn error_kind(error: &str) -> &'static str {
    if error.starts_with("blocked-html") {
        "blocked-html"
    } else if error.starts_with("blocked-redirect") {
        "blocked-redirect"
    } else if error.starts_with("retry-exhausted: http-429") {
        "rate-limited"
    } else if error.starts_with("retry-exhausted") {
        "retry-exhausted"
    } else if error.starts_with("empty-first-page") {
        "empty-first-page"
    } else if error.starts_with("timeout") {
        "timeout"
    } else if error.starts_with("stall") {
        "stall"
    } else if error.starts_with("sign-failed") {
        "sign-failed"
    } else if error.starts_with("no-signer") {
        "no-signer"
    } else if error.starts_with("http-") {
        "http-error"
    } else if error.starts_with("status-") {
        "platform-status"
    } else if error.starts_with("bad-json") {
        "bad-json"
    } else {
        "other"
    }
}

pub fn is_ambiguous(kind: &str) -> bool {
    matches!(
        kind,
        "timeout" | "stall" | "retry-exhausted" | "empty-first-page"
    )
}

/// 高置信度且模型明确建议恢复页面环境时才重试;其余情况保留已采数据并结束本批。
pub async fn should_retry(kind: &str, response_pages: usize) -> bool {
    if !is_ambiguous(kind) {
        return false;
    }
    let Some(key) = std::env::var("TYPESAFE_API_KEY")
        .ok()
        .filter(|s| !s.trim().is_empty())
    else {
        return false;
    };
    let request = json!({
        "model": JEV_MODEL,
        "state": {
            "platform": "douyin",
            "phase": "comment_api",
            "error_kind": kind,
            "response_pages": response_pages.min(500),
            "attempt": 1
        },
        "questions": {"action": {
            "type": "choice",
            "instructions": "判断这次评论接口失败是否可能通过导航到同一视频详情页、刷新页面环境后重试一次恢复。只有证据充分才选 retry_once;网络或平台限制无法判断时选 stop。不可建议绕过验证码、改变身份或继续批量重试。",
            "criteria": {
                "retry_once": "可能是当前页面环境失效,允许导航到同一视频详情页后重试一次",
                "stop": "结束本批,保留已经采到的评论;后续由用户处理或重新运行任务"
            }
        }}
    });
    let Ok(client) = reqwest::Client::builder()
        .timeout(Duration::from_secs(6))
        .build()
    else {
        return false;
    };
    let response = match client
        .post(JEV_ENDPOINT)
        .bearer_auth(key)
        .json(&request)
        .send()
        .await
    {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!("Jev 评论风控判断请求失败: {e}");
            return false;
        }
    };
    if !response.status().is_success() {
        tracing::warn!("Jev 评论风控判断返回 HTTP {}", response.status());
        return false;
    }
    let Ok(body) = response.json::<Value>().await else {
        return false;
    };
    let Some(answer) = body.pointer("/answers/action") else {
        return false;
    };
    let choice = answer
        .get("choice")
        .and_then(Value::as_str)
        .unwrap_or("stop");
    let probability = answer
        .get("probabilities")
        .and_then(|v| v.get(choice))
        .and_then(Value::as_f64)
        .unwrap_or(0.0);
    let retry = choice == "retry_once" && probability.is_finite() && probability >= MIN_PROBABILITY;
    tracing::info!(
        kind,
        response_pages,
        choice,
        probability,
        retry,
        "Jev 评论风控判断"
    );
    retry
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_ambiguous_failures_are_sent_to_jev() {
        assert_eq!(error_kind("blocked-html(http-200)"), "blocked-html");
        assert_eq!(error_kind("retry-exhausted: timeout"), "retry-exhausted");
        assert_eq!(error_kind("retry-exhausted: http-429"), "rate-limited");
        assert!(!is_ambiguous("blocked-html"));
        assert!(is_ambiguous("stall"));
        assert!(!is_ambiguous("sign-failed"));
    }
}
