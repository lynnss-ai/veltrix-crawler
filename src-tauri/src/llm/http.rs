//! 统一 HTTP 客户端构造 + 重试(指数退避),供 chat / speech 复用。
//!
//! 稳定/高可用取向:连接与总超时分离;仅对「网络错误 / 429 / 5xx」退避重试,
//! 4xx(除 429)立即返回——客户端错误(key 错/参数错/体过大被拒)重试无意义。

use std::time::Duration;
use veltrix_core::error::{CrawlerError, Result};

/// 连接超时:握手阶段卡住快速失败。
pub const CONNECT_TIMEOUT_SECS: u64 = 15;
/// chat 总超时:大模型推理较慢,给足。
pub const CHAT_TIMEOUT_SECS: u64 = 120;
/// 语音识别总超时:音频上传 + 转写更慢,给足。
pub const ASR_TIMEOUT_SECS: u64 = 300;

/// 最大重试次数(总尝试 = 1 + MAX_RETRIES)。
const MAX_RETRIES: u32 = 3;
/// 指数退避基数(毫秒)。
const RETRY_BASE_MS: u64 = 800;

/// 构造带连接/总超时的 reqwest client。
pub fn build_client(total_timeout_secs: u64) -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(CONNECT_TIMEOUT_SECS))
        .timeout(Duration::from_secs(total_timeout_secs))
        .build()
        .map_err(|e| CrawlerError::Config(format!("创建 HTTP 客户端失败: {e}")))
}

/// 发送请求 + 重试。`build` 每次重试重建 RequestBuilder(请求体不可跨次复用)。
/// `label` 用于错误上下文(不含敏感信息)。
pub async fn send_with_retry(
    build: impl Fn() -> reqwest::RequestBuilder,
    label: &str,
    // 是否对 429/5xx 重试。大请求体(如 ASR 音频)应关闭,避免限流时重复上传 + 重复计费;
    // 无论该开关如何,connect/timeout 等网络错误始终重试(瞬时且 connect 失败时未上传)。
    retry_server_errors: bool,
) -> Result<reqwest::Response> {
    let mut attempt = 0u32;
    loop {
        match build().send().await {
            Ok(resp) => {
                let status = resp.status();
                if status.is_success() {
                    return Ok(resp);
                }
                let code = status.as_u16();
                // 429(限流)与 5xx(服务端) 可重试(仅当调用方允许);其余 4xx 立即返回
                let retryable =
                    retry_server_errors && (code == 429 || (500..=599).contains(&code));
                if retryable && attempt < MAX_RETRIES {
                    attempt += 1;
                    backoff_sleep(attempt).await;
                    continue;
                }
                let body = resp.text().await.unwrap_or_default();
                return Err(CrawlerError::Config(format!(
                    "{label} 返回错误状态 {code}: {}",
                    truncate(&body, 300)
                )));
            }
            Err(e) => {
                // 连接 / 超时 / 请求层网络错误可重试
                let retryable = e.is_timeout() || e.is_connect() || e.is_request();
                if retryable && attempt < MAX_RETRIES {
                    attempt += 1;
                    backoff_sleep(attempt).await;
                    continue;
                }
                return Err(CrawlerError::Config(format!("{label} 请求失败: {e}")));
            }
        }
    }
}

/// 指数退避 + 轻抖动。抖动复用系统时间纳秒作廉价熵,不引入 rand 依赖。
async fn backoff_sleep(attempt: u32) {
    let base = RETRY_BASE_MS.saturating_mul(1u64 << attempt.min(6));
    let jitter = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| u64::from(d.subsec_nanos()))
        .unwrap_or(0)
        % 300;
    tokio::time::sleep(Duration::from_millis(base + jitter)).await;
}

/// 按字符截断,避免错误信息把超长响应体灌进日志。
fn truncate(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        text.to_string()
    } else {
        text.chars().take(max_chars).collect::<String>() + "…"
    }
}
