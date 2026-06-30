//! 语音识别(ASR / 转写)。按 provider code 分发;目前仅小米 MiMo。
//!
//! 可扩展:新增厂商 = 在 `transcribe` 的 match 加分支 + 一个 `*_transcribe` 函数,
//! 并在 `provider::provider_supports_asr` 放开该 code。

use std::path::Path;

use base64::Engine;
use serde_json::json;
use veltrix_core::error::{CrawlerError, Result};

use super::chat::{chat_completion, ChatRequest};
use super::http;
use super::provider::provider_supports_asr;

/// 一次转写请求参数(遵守「参数 ≤ 4」封装为结构体)。
pub struct TranscribeRequest<'a> {
    pub provider_code: &'a str,
    pub api_url: &'a str,
    pub api_key: &'a str,
    pub model: &'a str,
    /// 本地音频文件(通常是视频转出的 mp3)。
    pub audio_path: &'a Path,
}

/// 把本地音频转写为文本。按 provider_code 选实现;不支持 ASR 的厂商返回明确错误。
pub async fn transcribe(req: TranscribeRequest<'_>) -> Result<String> {
    if !provider_supports_asr(req.provider_code) {
        return Err(CrawlerError::Config(format!(
            "厂商「{}」不支持语音转写",
            req.provider_code
        )));
    }
    match req.provider_code {
        "mimo" => mimo_transcribe(&req).await,
        other => Err(CrawlerError::Config(format!("未实现的转写厂商: {other}"))),
    }
}

/// 小米 MiMo ASR:走 `/chat/completions`,messages 内联 input_audio(base64),
/// 带 `asr_options.language=auto`,model 通常为 `mimo-v2.5-asr`;
/// 结果在 `choices[0].message.content`(复用通用 chat 实现)。
async fn mimo_transcribe(req: &TranscribeRequest<'_>) -> Result<String> {
    let bytes = tokio::fs::read(req.audio_path)
        .await
        .map_err(|e| CrawlerError::Config(format!("读取音频失败: {e}")))?;
    // mp3 的 MIME 为 audio/mpeg;内联为 data url(不打印到日志,避免污染 + 泄露)
    // 按音频实际扩展名推 MIME 与 format(audio_format 配置可能非 mp3,如 wav/aac)
    let ext = req
        .audio_path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("mp3")
        .to_ascii_lowercase();
    let mime = match ext.as_str() {
        "wav" => "audio/wav",
        "aac" => "audio/aac",
        "m4a" => "audio/mp4",
        "ogg" => "audio/ogg",
        "flac" => "audio/flac",
        _ => "audio/mpeg", // mp3 及未知默认
    };
    let data_url = format!(
        "data:{mime};base64,{}",
        base64::engine::general_purpose::STANDARD.encode(&bytes)
    );
    let messages = json!([
        {
            "role": "user",
            "content": [
                {
                    "type": "input_audio",
                    "input_audio": { "data": data_url, "format": ext }
                }
            ]
        }
    ]);
    let extra = json!({ "asr_options": { "language": "auto" } });
    chat_completion(ChatRequest {
        api_url: req.api_url,
        api_key: req.api_key,
        model: req.model,
        messages,
        extra_body: Some(extra),
        timeout_secs: http::ASR_TIMEOUT_SECS,
        // ASR 请求体是整段 base64 音频,关闭 429/5xx 重试避免重复上传 + 重复计费
        retry_server_errors: false,
    })
    .await
    .map(|o| o.content)
}
