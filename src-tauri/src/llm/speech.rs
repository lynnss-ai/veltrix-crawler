//! 语音识别(ASR / 转写)。按 provider code 分发;目前支持小米 MiMo、智谱 GLM。
//!
//! 可扩展:新增厂商 = 在 `transcribe_single` 的 match 加分支 + 一个 `*_transcribe` 函数,
//! 并在 `provider::provider_supports_asr` 放开该 code、`asr_limits` 补对应限制。

use std::path::{Path, PathBuf};

use base64::Engine;
use serde_json::json;
use veltrix_core::error::{CrawlerError, Result};

use super::chat::{chat_completion, ChatRequest, TokenUsage};
use super::http;
use super::provider::provider_supports_asr;

/// 一次转写的完整产出:文本 + 各次 ASR API 请求的 token 用量。
/// 一段音频一次请求,大音频切片则多次;GLM 等非 chat 通道响应无 usage 字段,token 恒为 0
/// (该通道按时长/次数计费,账单侧体现为请求次数)。
pub struct TranscribeOutcome {
    pub text: String,
    /// 每次 ASR API 请求对应的 token 用量(请求次数 = usages.len())。
    pub usages: Vec<TokenUsage>,
}

/// 一次转写请求参数(遵守「参数 ≤ 4」封装为结构体)。
pub struct TranscribeRequest<'a> {
    pub provider_code: &'a str,
    pub api_url: &'a str,
    pub api_key: &'a str,
    pub model: &'a str,
    /// 本地音频文件(通常是视频转出的 mp3)。
    pub audio_path: &'a Path,
    /// ffmpeg 可执行路径(空/None 用系统 PATH 的 `ffmpeg`);仅大音频切片/转码时用。
    pub ffmpeg_path: Option<&'a str>,
}

/// 厂商级接口限制:单段直传的体积上限与切片时长(秒)。
struct AsrLimits {
    max_direct_bytes: u64,
    chunk_seconds: u32,
}

/// 各厂商 ASR 接口的单条限制(体积 + 时长),据此决定直传还是切片。
fn asr_limits(provider_code: &str) -> AsrLimits {
    match provider_code {
        // 智谱 GLM-ASR-2512:单条 ≤25MB 且时长 ≤30 秒(时长是硬约束,见官方文档)。
        // 入库前统一归一化为 96kbps 单声道 mp3(≈0.72MB/分钟):300KB ≈ 25 秒,处于安全区;
        // 切片取 25 秒,给 30 秒上限留余量。
        "glm" => AsrLimits {
            max_direct_bytes: 300 * 1024,
            chunk_seconds: 25,
        },
        // 小米 MiMo ASR(mimo-v2.5-asr):实测单条超过约 3 分钟就截断输出(只回开头几个字):
        // 180 秒段转写完整,300 秒段已截断。96kbps 单声道 mp3 ≈ 0.72MB/分钟,
        // 2MB ≈ 170 秒,处于实测安全区;注意这是「时长」约束,不是接口体积约束。
        _ => AsrLimits {
            max_direct_bytes: 2 * 1024 * 1024,
            chunk_seconds: 180,
        },
    }
}

/// 把本地音频转写为文本。按 provider_code 选实现;不支持 ASR 的厂商返回明确错误。
/// 音频超过厂商单条上限时自动切片转写拼接,调用方无需关心体积。
pub async fn transcribe(req: TranscribeRequest<'_>) -> Result<TranscribeOutcome> {
    if !provider_supports_asr(req.provider_code) {
        return Err(CrawlerError::Config(format!(
            "厂商「{}」不支持语音转写",
            req.provider_code
        )));
    }
    // GLM 仅接受 wav/mp3 且时长 ≤30s:非 mp3 一律先转码为 96kbps 单声道 mp3,
    // 既满足格式要求,也把「时长 ≤30s」换算成可靠的体积阈值(300KB≈25s)。
    let mut converted: Option<PathBuf> = None;
    let audio_path: &Path = if req.provider_code == "glm" && !is_mp3(req.audio_path) {
        let mp3 = convert_to_mp3(req.audio_path, req.ffmpeg_path).await?;
        converted = Some(mp3);
        converted.as_deref().unwrap()
    } else {
        req.audio_path
    };
    let result = transcribe_inner(&req, audio_path).await;
    // 转码临时文件无论成败都清理(失败忽略,不影响转写结果)
    if let Some(path) = converted {
        let _ = tokio::fs::remove_file(&path).await;
    }
    result
}

/// 体积判断 + 分发(直传 / 切片),与 transcribe 拆开便于统一处理转码临时文件。
async fn transcribe_inner(
    req: &TranscribeRequest<'_>,
    audio_path: &Path,
) -> Result<TranscribeOutcome> {
    let limits = asr_limits(req.provider_code);
    let size = tokio::fs::metadata(audio_path)
        .await
        .map(|m| m.len())
        .map_err(|e| CrawlerError::Config(format!("读取音频信息失败: {e}")))?;
    if size > limits.max_direct_bytes {
        return transcribe_chunked(req, audio_path, limits.chunk_seconds).await;
    }
    let (text, usage) = transcribe_single(req, audio_path).await?;
    Ok(TranscribeOutcome {
        text,
        usages: vec![usage],
    })
}

/// 是否 mp3 文件(按扩展名判断)。
fn is_mp3(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("mp3"))
        .unwrap_or(false)
}

/// 把任意本地音频转码为 96kbps 单声道 mp3(GLM ASR 用,参数与 media 模块抽音频口径一致)。
/// 输出为系统临时目录下的唯一文件,由调用方负责清理;ffmpeg 缺失/失败返回明确错误。
/// ffmpeg 同步执行包在 spawn_blocking 以避免阻塞 tokio 工作线程。
async fn convert_to_mp3(audio_path: &Path, ffmpeg_path: Option<&str>) -> Result<PathBuf> {
    let program = ffmpeg_path
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .unwrap_or("ffmpeg")
        .to_string();
    let audio_path = audio_path.to_path_buf();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    let out = std::env::temp_dir().join(format!(
        "veltrix-asr-{}-{nanos}.mp3",
        std::process::id()
    ));
    let out2 = out.clone();
    tokio::task::spawn_blocking(move || {
        let status = std::process::Command::new(&program)
            .arg("-y") // 覆盖已存在文件,避免交互确认卡住
            .arg("-i")
            .arg(&audio_path)
            .arg("-vn")
            .args([
                "-acodec",
                "libmp3lame",
                "-ab",
                "96k",
                "-ar",
                "22050",
                "-ac",
                "1",
                "-threads",
                "1",
            ])
            .arg(&out2)
            .status()
            .map_err(|e| {
                CrawlerError::Config(format!(
                    "智谱 GLM 转写需先把音频转码为 mp3,启动 ffmpeg 失败: {e}"
                ))
            })?;
        if !status.success() {
            return Err(CrawlerError::Config(format!(
                "音频转码 mp3 失败(ffmpeg 退出码 {:?})",
                status.code()
            )));
        }
        Ok(out2)
    })
    .await
    .map_err(|e| CrawlerError::Config(format!("转码任务异常: {e}")))?
}

/// 单文件转写分发(音频体积在上限内);返回文本与本次请求的 token 用量。
async fn transcribe_single(
    req: &TranscribeRequest<'_>,
    audio_path: &Path,
) -> Result<(String, TokenUsage)> {
    match req.provider_code {
        "mimo" => mimo_transcribe(req, audio_path).await,
        "glm" => glm_transcribe(req, audio_path).await,
        other => Err(CrawlerError::Config(format!("未实现的转写厂商: {other}"))),
    }
}

/// 大音频转写:ffmpeg 按时长切片 → 逐段转写 → 按时间序拼接文本。
/// 切片为临时产物,无论成败都清理;任一段失败则整体失败(已转文本不留,避免半截文案)。
async fn transcribe_chunked(
    req: &TranscribeRequest<'_>,
    audio_path: &Path,
    chunk_seconds: u32,
) -> Result<TranscribeOutcome> {
    // 切片目录:与音频同级的临时子目录(唯一后缀防并发同名)
    let parent = audio_path.parent().unwrap_or_else(|| Path::new("."));
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    let dir = parent.join(format!("chunks-{}-{nanos}", std::process::id()));
    let result = async {
        // ffmpeg 切片是同步阻塞操作,包在 spawn_blocking 避免阻塞 tokio 工作线程
        let (chunks, total) = {
            let audio_path = audio_path.to_path_buf();
            let dir2 = dir.clone();
            let ffmpeg_path = req.ffmpeg_path.map(|s| s.to_string());
            tokio::task::spawn_blocking(move || {
                let chunks =
                    crate::media::split_audio(&audio_path, &dir2, chunk_seconds, ffmpeg_path.as_deref())?;
                let total = chunks.len();
                Ok::<_, CrawlerError>((chunks, total))
            })
            .await
            .map_err(|e| CrawlerError::Config(format!("音频切片异常: {e}")))??
        };
        tracing::info!(chunks = total, "音频超过直传上限,已切片逐段转写");
        let mut texts: Vec<String> = Vec::with_capacity(total);
        // 每段一次 ASR 请求:用量逐段留存(空文本段请求已发出、照常计费,usage 也要记)
        let mut usages: Vec<TokenUsage> = Vec::with_capacity(total);
        // 逐段串行:外层 transcribe_for_contents 已按配置的内容并发数在飞,段内再并发会打爆 ASR rate limit
        for (idx, chunk) in chunks.iter().enumerate() {
            let (text, usage) = transcribe_single(req, chunk).await.map_err(|e| {
                CrawlerError::Config(format!("第 {}/{total} 段转写失败: {e}", idx + 1))
            })?;
            usages.push(usage);
            let text = text.trim();
            if !text.is_empty() {
                texts.push(text.to_string());
            }
        }
        if texts.is_empty() {
            return Err(CrawlerError::Config("切片转写无有效文本".into()));
        }
        Ok(TranscribeOutcome {
            text: texts.join("\n"),
            usages,
        })
    }
    .await;
    // 清理切片临时目录(失败忽略,不影响转写结果)
    let _ = tokio::fs::remove_dir_all(&dir).await;
    result
}

/// 小米 MiMo ASR:走 `/chat/completions`,messages 内联 input_audio(base64),
/// 带 `asr_options.language=auto`,model 通常为 `mimo-v2.5-asr`;
/// 结果在 `choices[0].message.content`(复用通用 chat 实现),usage 一并带出供账单统计。
async fn mimo_transcribe(
    req: &TranscribeRequest<'_>,
    audio_path: &Path,
) -> Result<(String, TokenUsage)> {
    let bytes = tokio::fs::read(audio_path)
        .await
        .map_err(|e| CrawlerError::Config(format!("读取音频失败: {e}")))?;
    // mp3 的 MIME 为 audio/mpeg;内联为 data url(不打印到日志,避免污染 + 泄露)
    // 按音频实际扩展名推 MIME 与 format(audio_format 配置可能非 mp3,如 wav/aac)
    let ext = audio_path
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
        // 开启 429/5xx 重试:切片场景一段失败整篇报废的代价远大于重试,
        // 且重试仅在失败时发生(成功不重复计费)。429 优先读 Retry-After 头退避。
        retry_server_errors: true,
    })
    .await
    .map(|o| (o.content, o.usage))
}

/// 智谱 GLM ASR:走 `/audio/transcriptions`,multipart/form-data 上传文件
/// (model + stream=false + file),非流式响应 JSON 的 `text` 字段即完整转写文本。
/// 入参音频保证已是 mp3(见 transcribe 的转码预处理);接口单条 ≤25MB 且 ≤30s,
/// 超过上限的音频已在 transcribe_inner 切片,到这里都是可直接上传的小段。
/// 该接口响应无 usage 字段,token 用量按 0 记(账单侧以请求次数体现)。
async fn glm_transcribe(
    req: &TranscribeRequest<'_>,
    audio_path: &Path,
) -> Result<(String, TokenUsage)> {
    let bytes = tokio::fs::read(audio_path)
        .await
        .map_err(|e| CrawlerError::Config(format!("读取音频失败: {e}")))?;
    let file_name = audio_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("audio.mp3")
        .to_string();
    let url = http::join_endpoint(req.api_url, "/audio/transcriptions");
    let client = http::shared_client(http::ASR_TIMEOUT_SECS)?;
    let resp = http::send_with_retry(
        || {
            // multipart Form 不可 Clone,每次(含网络错误重试)重建;音频 ≤300KB,克隆开销可忽略
            let part = reqwest::multipart::Part::bytes(bytes.clone()).file_name(file_name.clone());
            let form = reqwest::multipart::Form::new()
                .text("model", req.model.to_string())
                .text("stream", "false")
                .part("file", part);
            client.post(&url).bearer_auth(req.api_key).multipart(form)
        },
        "智谱 GLM 语音转写",
        // 打开 429/5xx 重试:智谱网关的 500「操作失败」多为瞬时/限流抖动,退避后可过;
        // 仅失败才重发(成功不重复计费),且分段 ≤300KB 重传代价小——
        // 远比「一段 500 整篇报废、十几段全部重来」省。重试 3 次仍失败才向上报错。
        true,
    )
    .await?;
    let body: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| CrawlerError::Config(format!("解析智谱 GLM 转写响应失败: {e}")))?;
    body.get("text")
        .and_then(|t| t.as_str())
        .map(|s| (s.to_string(), TokenUsage::default()))
        .ok_or_else(|| CrawlerError::Config("智谱 GLM 转写响应缺少 text 字段".into()))
}
