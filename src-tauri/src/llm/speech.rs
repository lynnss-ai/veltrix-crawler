//! 语音识别(ASR / 转写)。按 provider code 分发;目前支持小米 MiMo、智谱 GLM。
//!
//! 可扩展:新增厂商 = 在 `transcribe_single` 的 match 加分支 + 一个 `*_transcribe` 函数,
//! 并在 `provider::provider_supports_asr` 放开该 code、`asr_limits` 补对应限制。

use std::path::{Path, PathBuf};

use base64::Engine;
use serde_json::{json, Value};
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
    // GLM 仅接受 wav/mp3 且时长 ≤30s;MiMo 同样只认 wav/mp3(400: input_audio.format must be one of wav, mp3)。
    let (audio_path, converted) = prepare_asr_audio(&req).await?;
    let result = transcribe_inner(&req, &audio_path).await;
    // 转码临时文件无论成败都清理(失败忽略,不影响转写结果)
    if let Some(path) = converted {
        let _ = tokio::fs::remove_file(&path).await;
    }
    result
}

/// 流式转写:语音小片段专用(录音期间每几秒一段,无大音频切片路径——大文件仍走 transcribe)。
/// 识别增量经 `on_delta` 逐段实时回传,返回完整文本与用量(口径同 transcribe,供调用方记账单)。
/// 不支持流式的厂商回退非流式:整段文本一次性经 on_delta 回传,调用方/前端无感。
pub async fn transcribe_stream(
    req: TranscribeRequest<'_>,
    mut on_delta: impl FnMut(String) + Send,
) -> Result<TranscribeOutcome> {
    if !provider_supports_asr(req.provider_code) {
        return Err(CrawlerError::Config(format!(
            "厂商「{}」不支持语音转写",
            req.provider_code
        )));
    }
    // GLM/MiMo 均仅接受 wav/mp3:与 transcribe 同口径先转码(录音小片段通常是 webm)
    let (audio_path, converted) = prepare_asr_audio(&req).await?;
    // 人声门禁:无人声的片段(静音 / 底噪 / 气口)不上行 ASR——省配额,也避免空音频诱发幻觉文本。
    // VAD 判定走 spawn_blocking(ffmpeg 同步解码);判定异常保守放行(join 失败视为有语音)。
    let path_for_vad = audio_path.to_path_buf();
    let program_for_vad = req.ffmpeg_path.map(str::to_string);
    let has_voice = tokio::task::spawn_blocking(move || {
        let program = program_for_vad
            .as_deref()
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .unwrap_or("ffmpeg")
            .to_string();
        crate::media::has_human_speech(&program, &path_for_vad)
    })
    .await
    .unwrap_or(true);
    if !has_voice {
        tracing::info!("语音片段没有人声,跳过 ASR 请求");
        if let Some(path) = converted {
            let _ = tokio::fs::remove_file(&path).await;
        }
        return Ok(TranscribeOutcome {
            text: String::new(),
            usages: Vec::new(),
        });
    }
    let result = match req.provider_code {
        "mimo" => mimo_transcribe_stream(&req, &audio_path, &mut on_delta).await,
        "glm" => glm_transcribe_stream(&req, &audio_path, &mut on_delta).await,
        // 回退:非流式整段转写,全文一次性回传
        _ => transcribe_single(&req, &audio_path).await.map(|(text, usage)| {
            if !text.is_empty() {
                on_delta(text.clone());
            }
            (text, usage)
        }),
    };
    // 转码临时文件无论成败都清理(失败忽略,不影响转写结果)
    if let Some(path) = converted {
        let _ = tokio::fs::remove_file(&path).await;
    }
    let (text, usage) = result?;
    Ok(TranscribeOutcome {
        text,
        usages: vec![usage],
    })
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

/// 该厂商的 ASR 接口是否只接受 wav/mp3(非 mp3 输入需先转码)。
/// GLM 与 MiMo 官方均限制 wav/mp3;wav 直传可解,其余(webm/ogg/aac 等)一律转 mp3。
fn provider_requires_mp3(code: &str) -> bool {
    matches!(code, "glm" | "mimo")
}

/// 按厂商 ASR 要求预处理音频,返回 (可用音频路径, 待清理的转码临时文件)。
/// 非 mp3 一律转码为 96kbps 单声道 mp3:既满足格式要求,也把 GLM「时长 ≤30s」换算成可靠的体积阈值(300KB≈25s)。
async fn prepare_asr_audio(req: &TranscribeRequest<'_>) -> Result<(PathBuf, Option<PathBuf>)> {
    if !provider_requires_mp3(req.provider_code) {
        return Ok((req.audio_path.to_path_buf(), None));
    }
    let need_convert = if is_mp3(req.audio_path) {
        // GLM transcriptions 只收单声道(错误码 1214):库里「原声直链下载」的 mp3 未过 ffmpeg 转码,
        // 可能是立体声;扩展名判断不足以放行,需按实际声道数决定是否归一转码
        req.provider_code == "glm" && !is_mono_audio(req.audio_path, req.ffmpeg_path).await
    } else {
        true
    };
    if need_convert {
        let mp3 = convert_to_mp3(req.audio_path, req.ffmpeg_path).await?;
        return Ok((mp3.clone(), Some(mp3)));
    }
    Ok((req.audio_path.to_path_buf(), None))
}

/// 判断音频是否单声道:ffmpeg -i 的 stderr 即含流信息(形如 "Audio: mp3, 22050 Hz, mono"),
/// 无需 ffprobe(桌面端只捆绑 ffmpeg.exe)。解析不出声道信息按非单声道处理:
/// 多转一次码只是慢,漏判立体声则 GLM 直接 1214 报错。
async fn is_mono_audio(path: &Path, ffmpeg_path: Option<&str>) -> bool {
    let program = ffmpeg_path
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .unwrap_or("ffmpeg")
        .to_string();
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let mut cmd = std::process::Command::new(&program);
        crate::media::hide_console_window(&mut cmd);
        // 只读流信息,无输出文件 ffmpeg 退出码非 0 属预期,不影响 stderr 内容
        let Ok(out) = cmd.arg("-i").arg(&path).output() else {
            return false;
        };
        let info = String::from_utf8_lossy(&out.stderr);
        let Some(stream) = info.lines().find(|l| l.contains("Audio:")) else {
            return false;
        };
        stream.contains("mono") || stream.contains("1 channel")
    })
    .await
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
        let mut cmd = std::process::Command::new(&program);
        crate::media::hide_console_window(&mut cmd);
        let status = cmd
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

/// 大音频转写:VAD 语音间隙优先切片(降级:静音探测 → 按时长硬切)→ 逐段转写 → 按时间序拼接。
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
                    crate::media::split_audio_for_asr(&audio_path, &dir2, chunk_seconds, ffmpeg_path.as_deref())?;
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

/// MiMo ASR 的 messages 载荷:内联 input_audio(base64 data url,不打印到日志,避免污染 + 泄露),
/// 按音频实际扩展名推 MIME 与 format(audio_format 配置可能非 mp3,如 wav/aac)。流式/非流式共用。
async fn mimo_asr_messages(audio_path: &Path) -> Result<Value> {
    let bytes = tokio::fs::read(audio_path)
        .await
        .map_err(|e| CrawlerError::Config(format!("读取音频失败: {e}")))?;
    // mp3 的 MIME 为 audio/mpeg
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
    Ok(json!([
        {
            "role": "user",
            "content": [
                {
                    "type": "input_audio",
                    "input_audio": { "data": data_url, "format": ext }
                }
            ]
        }
    ]))
}

/// 小米 MiMo ASR:走 `/chat/completions`,messages 内联 input_audio(base64),
/// 带 `asr_options.language=auto`,model 通常为 `mimo-v2.5-asr`;
/// 结果在 `choices[0].message.content`(复用通用 chat 实现),usage 一并带出供账单统计。
async fn mimo_transcribe(
    req: &TranscribeRequest<'_>,
    audio_path: &Path,
) -> Result<(String, TokenUsage)> {
    let messages = mimo_asr_messages(audio_path).await?;
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

/// 小米 MiMo ASR 流式:请求同 mimo_transcribe 加 `"stream": true`;
/// SSE 帧为 OpenAI 式 `choices[0].delta.content` 增量,`data: [DONE]` 结束。
/// 增量经 on_delta 实时回传;静音段全文为空是合法产出(与非流式口径一致),不报错。
/// 流式不复用 send_with_retry:字节流读取 + 重试语义复杂,单次请求即可(3 秒小片段,失败下段再来)。
async fn mimo_transcribe_stream(
    req: &TranscribeRequest<'_>,
    audio_path: &Path,
    on_delta: &mut (dyn FnMut(String) + Send),
) -> Result<(String, TokenUsage)> {
    let messages = mimo_asr_messages(audio_path).await?;
    let body = json!({
        "model": req.model,
        "messages": messages,
        "stream": true,
        "asr_options": { "language": "auto" },
    });
    let url = http::join_endpoint(req.api_url, "/chat/completions");
    let client = http::streaming_client()?;
    let resp = client
        .post(&url)
        .bearer_auth(req.api_key)
        .json(&body)
        .send()
        .await
        .map_err(|e| CrawlerError::Config(format!("小米 MiMo 流式转写请求失败: {e}")))?;
    if !resp.status().is_success() {
        return Err(CrawlerError::Config(stream_error_body("小米 MiMo 流式转写", resp).await));
    }
    let mut full = String::new();
    let mut usage = TokenUsage::default();
    let mut frames = 0u32;
    read_sse_lines(resp, |line| {
        let Some(data) = sse_data(line) else { return };
        frames += 1;
        if frames == 1 {
            // 诊断:首帧原文(截断),确认厂商 SSE 实际格式
            tracing::info!(first_frame = %data.chars().take(300).collect::<String>(), "MiMo 流式首帧");
        }
        let Ok(v) = serde_json::from_str::<Value>(data) else {
            return;
        };
        // 部分厂商流末单独发一帧 usage(OpenAI stream_options 风格),给了就记
        if let Some(u) = v.get("usage") {
            usage = TokenUsage {
                prompt: u.get("prompt_tokens").and_then(Value::as_u64).unwrap_or(0) as u32,
                completion: u.get("completion_tokens").and_then(Value::as_u64).unwrap_or(0) as u32,
            };
        }
        if let Some(piece) = mimo_frame_delta(&v) {
            full.push_str(&piece);
            on_delta(piece);
        }
    })
    .await?;
    tracing::info!(frames, text_len = full.len(), "MiMo 流式转写汇总");
    Ok((full, usage))
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

/// 智谱 GLM ASR 流式:multipart 同 glm_transcribe,form 字段 `stream` 改 "true";
/// SSE 逐帧返回 JSON,`text` 字段可能是累计全量也可能是增量,由 glm_frame_delta 归一为增量。
/// 该接口响应无 usage 字段,token 恒为 0(账单侧以请求次数体现)。
async fn glm_transcribe_stream(
    req: &TranscribeRequest<'_>,
    audio_path: &Path,
    on_delta: &mut (dyn FnMut(String) + Send),
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
    let client = http::streaming_client()?;
    let part = reqwest::multipart::Part::bytes(bytes).file_name(file_name);
    let form = reqwest::multipart::Form::new()
        .text("model", req.model.to_string())
        .text("stream", "true")
        .part("file", part);
    let resp = client
        .post(&url)
        .bearer_auth(req.api_key)
        .multipart(form)
        .send()
        .await
        .map_err(|e| CrawlerError::Config(format!("智谱 GLM 流式转写请求失败: {e}")))?;
    if !resp.status().is_success() {
        return Err(CrawlerError::Config(stream_error_body("智谱 GLM 流式转写", resp).await));
    }
    let mut full = String::new();
    // 上一帧原始 text:累计形态据此 diff,增量形态基本不影响(见 glm_frame_delta)
    let mut prev_frame = String::new();
    let mut frames = 0u32;
    read_sse_lines(resp, |line| {
        let Some(data) = sse_data(line) else { return };
        frames += 1;
        if frames == 1 {
            // 诊断:首帧原文(截断),确认厂商 SSE 实际格式
            tracing::info!(first_frame = %data.chars().take(300).collect::<String>(), "GLM 流式首帧");
        }
        let Some(text) = glm_frame_text(data) else { return };
        let delta = glm_frame_delta(&prev_frame, &text);
        prev_frame = text;
        if !delta.is_empty() {
            full.push_str(&delta);
            on_delta(delta);
        }
    })
    .await?;
    tracing::info!(frames, text_len = full.len(), "GLM 流式转写汇总");
    Ok((full, TokenUsage::default()))
}

/// 流式响应非 2xx:读出错误体(截断)拼错误信息,便于排查鉴权/配额/模型名等问题。
async fn stream_error_body(label: &str, resp: reqwest::Response) -> String {
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    format!(
        "{label} 返回错误状态 {status}: {}",
        body.chars().take(300).collect::<String>()
    )
}

/// 读 SSE 响应流:按行交付 on_line(一行可能跨多个网络包,只处理含完整换行的部分)。
/// 流式不设总超时;存活判死靠 idle 超时(STREAM_IDLE_TIMEOUT_SECS 无新数据判停滞)。
async fn read_sse_lines(resp: reqwest::Response, mut on_line: impl FnMut(&str)) -> Result<()> {
    use futures_util::StreamExt;
    let mut stream = resp.bytes_stream();
    let mut buf = String::new();
    let idle = std::time::Duration::from_secs(http::STREAM_IDLE_TIMEOUT_SECS);
    loop {
        let chunk = match tokio::time::timeout(idle, stream.next()).await {
            Ok(Some(c)) => c,
            Ok(None) => break,
            Err(_) => {
                return Err(CrawlerError::Config(format!(
                    "语音转写流停滞超时({} 秒无新数据),已中断",
                    http::STREAM_IDLE_TIMEOUT_SECS
                )))
            }
        };
        let bytes = chunk.map_err(|e| CrawlerError::Config(format!("读取转写流失败: {e}")))?;
        buf.push_str(&String::from_utf8_lossy(&bytes));
        while let Some(pos) = buf.find('\n') {
            let line = buf[..pos].trim().to_string();
            buf.drain(..=pos);
            on_line(&line);
        }
    }
    Ok(())
}

/// SSE 行 → data 载荷;非 data 行 / 空载荷 / [DONE] 结束标记返回 None。
fn sse_data(line: &str) -> Option<&str> {
    let data = line.trim().strip_prefix("data:")?.trim();
    if data.is_empty() || data == "[DONE]" {
        None
    } else {
        Some(data)
    }
}

/// MiMo(OpenAI 式)SSE 帧 → 文本增量:`choices[0].delta.content`;缺字段/空串返回 None。
fn mimo_frame_delta(v: &Value) -> Option<String> {
    let piece = v
        .get("choices")?
        .get(0)?
        .get("delta")?
        .get("content")?
        .as_str()?;
    if piece.is_empty() {
        None
    } else {
        Some(piece.to_string())
    }
}

/// GLM SSE 帧 → `text` 字段原文(可能是累计全量也可能是增量,由 glm_frame_delta 归一)。
fn glm_frame_text(data: &str) -> Option<String> {
    let v: Value = serde_json::from_str(data).ok()?;
    v.get("text")?.as_str().map(str::to_string)
}

/// GLM 流式 text 兼容:新帧以上一帧为前缀则视为累计全量,diff 取后缀作增量;
/// 否则整帧视为增量追加。`prev_frame` 为上一帧原始 text(首帧传空串,等价整帧作增量)。
/// 已知局限:增量形态下某帧恰好以上一帧全文开头时会被误判为累计(概率极低,影响仅限该帧)。
fn glm_frame_delta(prev_frame: &str, frame: &str) -> String {
    if frame.starts_with(prev_frame) {
        frame[prev_frame.len()..].to_string()
    } else {
        frame.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sse_data_extracts_payload() {
        assert_eq!(sse_data("data: {\"a\":1}"), Some("{\"a\":1}"));
        assert_eq!(sse_data("data:{\"a\":1}"), Some("{\"a\":1}"));
        assert_eq!(sse_data("data: [DONE]"), None);
        assert_eq!(sse_data("data:"), None);
        assert_eq!(sse_data(""), None);
        assert_eq!(sse_data(": 注释行"), None);
        assert_eq!(sse_data("event: message"), None);
    }

    #[test]
    fn mimo_delta_from_openai_style_frame() {
        let v: Value = serde_json::from_str(r#"{"choices":[{"delta":{"content":"你好"}}]}"#).unwrap();
        assert_eq!(mimo_frame_delta(&v), Some("你好".to_string()));
        // 缺 content / 空 content / 无 choices 都不出增量
        let v: Value = serde_json::from_str(r#"{"choices":[{"delta":{}}]}"#).unwrap();
        assert_eq!(mimo_frame_delta(&v), None);
        let v: Value = serde_json::from_str(r#"{"choices":[{"delta":{"content":""}}]}"#).unwrap();
        assert_eq!(mimo_frame_delta(&v), None);
        let v: Value = serde_json::from_str(r#"{"usage":{"prompt_tokens":1}}"#).unwrap();
        assert_eq!(mimo_frame_delta(&v), None);
    }

    #[test]
    fn glm_cumulative_frames_diff_to_increment() {
        // 累计形态:每帧 text 是截至目前的全量,diff 拼接应还原文本
        let mut prev = String::new();
        let mut out = String::new();
        for frame in ["你", "你好", "你好世界", "你好世界"] {
            let d = glm_frame_delta(&prev, frame);
            prev = frame.to_string();
            out.push_str(&d);
        }
        assert_eq!(out, "你好世界");
    }

    #[test]
    fn glm_incremental_frames_append_verbatim() {
        // 增量形态:每帧 text 就是新增片段,整帧追加
        let mut prev = String::new();
        let mut out = String::new();
        for frame in ["你好", ",世界", "。"] {
            let d = glm_frame_delta(&prev, frame);
            prev = frame.to_string();
            out.push_str(&d);
        }
        assert_eq!(out, "你好,世界。");
    }

    #[test]
    fn glm_frame_text_parses_json() {
        assert_eq!(
            glm_frame_text(r#"{"text":"你好"}"#),
            Some("你好".to_string())
        );
        assert_eq!(glm_frame_text(r#"{"usage":{}}"#), None);
        assert_eq!(glm_frame_text("not json"), None);
    }
}
