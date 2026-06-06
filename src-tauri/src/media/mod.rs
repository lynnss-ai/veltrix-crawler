//! 采集素材下载与「视频转音频」处理。
//!
//! 时机:采集落库后由 run_task 在后台触发,对每条内容下载全部素材
//! (封面、作者头像、图文图片;视频则下载后用 ffmpeg 转音频并删除原视频)。
//!
//! 设计取向:单条/单步素材失败只告警不中断——素材是「采集的副产品」,
//! 任一 URL 失效或网络抖动都不应拖垮整条内容乃至整批的素材处理。

use crate::model::{Content, ContentKind};
use chrono::Local;
use std::path::{Path, PathBuf};
use veltrix_core::config::MediaConfig;
use veltrix_core::error::{CrawlerError, Result};

/// output_dir 为空时的回退子目录名(相对配置目录)。
const FALLBACK_MEDIA_DIR: &str = "media";
/// 视频形态目录名(目录「类目」按内容形态划分:视频 / 图文)。路径统一用英文。
const DIR_VIDEO: &str = "video";
/// 图文形态目录名。
const DIR_IMAGE: &str = "image";
/// 作者头像分组目录名。头像按作者去重存一份,不随内容/日期/形态分散。
const DIR_AVATAR: &str = "avatar";
/// 文件名中需替换掉的非法字符(Windows 文件系统保留 + 路径分隔符)。
const ILLEGAL_FILENAME_CHARS: &[char] = &['/', '\\', ':', '*', '?', '"', '<', '>', '|'];

/// 解析媒体根目录:output_dir 为绝对路径时直接用,否则落到配置目录下。
/// output_dir 为空回退 `{config_dir}/media`,非空相对路径则 `{config_dir}/{output_dir}`。
pub fn media_root(config_dir: &Path, media: &MediaConfig) -> PathBuf {
    let dir = media.output_dir.trim();
    if dir.is_empty() {
        return config_dir.join(FALLBACK_MEDIA_DIR);
    }
    let path = Path::new(dir);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        config_dir.join(path)
    }
}

/// 下载 URL 到本地文件。reqwest 拉取字节后整体写入;失败返回错误供调用方告警。
pub async fn download_to_file(url: &str, path: &Path) -> Result<()> {
    if url.trim().is_empty() {
        return Err(CrawlerError::Parse("下载地址为空".into()));
    }
    // 带超时:避免某条 hang 住的 CDN 直链无限阻塞,拖垮整批素材下载
    let client = reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(15))
        .timeout(std::time::Duration::from_secs(120))
        .build()?;
    let resp = client.get(url).send().await?.error_for_status()?;
    let bytes = resp.bytes().await?;
    tokio::fs::write(path, &bytes).await?;
    Ok(())
}

/// 用 ffmpeg 把视频转为音频:`-y -i <video> -vn <audio>`。
/// ffmpeg_path 为空时用系统 PATH 的 `ffmpeg`。退出码非 0 视为失败。
pub fn extract_audio(video: &Path, audio: &Path, ffmpeg_path: Option<&str>) -> Result<()> {
    let program = ffmpeg_path
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .unwrap_or("ffmpeg");
    let status = std::process::Command::new(program)
        .arg("-y") // 覆盖已存在的输出文件,避免转码卡在交互确认
        .arg("-i")
        .arg(video)
        .arg("-vn") // 丢弃视频流,只保留音频
        .arg(audio)
        .status()
        .map_err(|e| CrawlerError::Parse(format!("启动 ffmpeg 失败: {e}")))?;
    if !status.success() {
        return Err(CrawlerError::Parse(format!(
            "ffmpeg 转音频失败,退出码: {:?}",
            status.code()
        )));
    }
    Ok(())
}

/// 把内容 ID 清洗为合法文件名前缀:替换非法字符为 `_`,空值兜底为 "unknown"。
fn sanitize_filename(raw: &str) -> String {
    let cleaned: String = raw
        .chars()
        .map(|c| if ILLEGAL_FILENAME_CHARS.contains(&c) { '_' } else { c })
        .collect();
    let trimmed = cleaned.trim().trim_matches('.');
    if trimmed.is_empty() {
        "unknown".to_string()
    } else {
        trimmed.to_string()
    }
}

/// 处理单条内容的全部素材:封面、作者头像、图文图片、视频转音频。
/// 目录结构 `{root}/{platform}/{今天 YYYY-MM-DD}/{视频|图文}/`,文件名以 content_id 为前缀。
/// 任一素材失败仅 `tracing::warn!`,不返回错误、不中断后续素材与其它内容。
pub async fn process_content(content: &Content, root: &Path, media: &MediaConfig) {
    let kind_dir = if content.kind == ContentKind::Video {
        DIR_VIDEO
    } else {
        DIR_IMAGE
    };
    // 用本机当天日期分目录,便于按天归档检索
    let today = Local::now().format("%Y-%m-%d").to_string();
    let dir = root.join(&content.platform).join(today).join(kind_dir);
    if let Err(e) = tokio::fs::create_dir_all(&dir).await {
        tracing::warn!(content_id = %content.content_id, "创建素材目录失败,跳过该条: {e}");
        return;
    }

    let prefix = sanitize_filename(&content.content_id);

    // 封面
    if let Some(cover) = content.cover_url.as_deref().filter(|s| !s.is_empty()) {
        let path = dir.join(format!("{prefix}_cover.jpg"));
        if let Err(e) = download_to_file(cover, &path).await {
            tracing::warn!(content_id = %content.content_id, "下载封面失败: {e}");
        }
    }

    // 作者头像:单独 avatar 分组,按作者 uid 命名去重(同作者多条内容共用一份,已存在则不重下)
    if let Some(avatar) = content.author.avatar.as_deref().filter(|s| !s.is_empty()) {
        let uid = sanitize_filename(&content.author.uid);
        if uid != "unknown" {
            let avatar_dir = root.join(&content.platform).join(DIR_AVATAR);
            match tokio::fs::create_dir_all(&avatar_dir).await {
                Ok(()) => {
                    let path = avatar_dir.join(format!("{uid}.jpg"));
                    // 已下过同一作者头像则跳过,避免重复请求
                    if !path.exists() {
                        if let Err(e) = download_to_file(avatar, &path).await {
                            tracing::warn!(content_id = %content.content_id, "下载头像失败: {e}");
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!(content_id = %content.content_id, "创建头像目录失败: {e}");
                }
            }
        }
    }

    // 视频:下载 → 转音频 → 删除视频(用户决策:只留音频)
    if content.kind == ContentKind::Video {
        if let Some(video_url) = content.video_url.as_deref().filter(|s| !s.is_empty()) {
            process_video(content, &dir, &prefix, video_url, media).await;
        }
    }

    // 图文图片:逐张下载
    for (idx, img_url) in content.image_urls.iter().enumerate() {
        if img_url.is_empty() {
            continue;
        }
        let path = dir.join(format!("{prefix}_img{idx}.jpg"));
        if let Err(e) = download_to_file(img_url, &path).await {
            tracing::warn!(content_id = %content.content_id, index = idx, "下载图片失败: {e}");
        }
    }
}

/// 视频子流程:下载到 mp4 → ffmpeg 转音频 → 删除 mp4。
/// 受 `enable_audio_extract` 控制:关闭时仅下载视频不转码、不删除,保证用户仍拿得到素材。
async fn process_video(
    content: &Content,
    dir: &Path,
    prefix: &str,
    video_url: &str,
    media: &MediaConfig,
) {
    let video_path = dir.join(format!("{prefix}.mp4"));
    if let Err(e) = download_to_file(video_url, &video_path).await {
        tracing::warn!(content_id = %content.content_id, "下载视频失败: {e}");
        return;
    }

    // 开关关闭时保留原视频、不转码——尊重配置,但素材照样落地
    if !media.enable_audio_extract {
        return;
    }

    let audio_format = if media.audio_format.trim().is_empty() {
        "mp3"
    } else {
        media.audio_format.trim()
    };
    let audio_path = dir.join(format!("{prefix}.{audio_format}"));

    // ffmpeg 是同步阻塞调用,挪到阻塞线程池,避免占用异步运行时工作线程
    let video_for_task = video_path.clone();
    let audio_for_task = audio_path.clone();
    let ffmpeg_path = media.ffmpeg_path.clone();
    let result = tokio::task::spawn_blocking(move || {
        extract_audio(&video_for_task, &audio_for_task, ffmpeg_path.as_deref())
    })
    .await;

    match result {
        Ok(Ok(())) => {
            // 转码成功才删原视频(用户决策:只留音频)
            if let Err(e) = tokio::fs::remove_file(&video_path).await {
                tracing::warn!(content_id = %content.content_id, "删除已转码视频失败: {e}");
            }
        }
        Ok(Err(e)) => {
            tracing::warn!(content_id = %content.content_id, "视频转音频失败,保留原视频: {e}");
        }
        Err(e) => {
            tracing::warn!(content_id = %content.content_id, "转码任务异常: {e}");
        }
    }
}
