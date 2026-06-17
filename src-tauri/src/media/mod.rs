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

/// 单条内容的素材处理结果。回写到 contents 表,供前端展示与失败重试。
/// 只反映「主素材」:视频内容 = 视频下载 + 音频提取;图文内容 = 图片下载。
/// 封面 / 头像属副产品,失败仅告警,不影响这里的成败判定。
#[derive(Debug, Clone)]
pub struct MediaOutcome {
    /// 主素材是否就绪(视频已下载 / 图片全部下载;无可下载素材也视为成功)
    pub ok: bool,
    /// 音频是否提取成功:仅「视频 + 开启提取」有意义,其余为 None
    pub audio_extracted: Option<bool>,
    /// 失败原因(下载/提取任一失败时记录,供前端提示)
    pub error: Option<String>,
    /// 封面本地绝对路径(下载成功),供回写 contents.cover_path
    pub cover_path: Option<String>,
    /// 作者头像本地绝对路径(下载成功/已存在),供回写 contents.avatar_path
    pub avatar_path: Option<String>,
    /// 视频转出的音频(mp3)本地绝对路径,供后续语音转写读取;None=非视频/转码失败
    pub audio_path: Option<String>,
    /// 视频文件是否下载成功(仅 video + ai_extract);None=非视频/未尝试
    pub video_downloaded: Option<bool>,
    /// 图文图片总数 / 已成功下载数(仅 image)
    pub image_total: Option<i32>,
    pub image_done: Option<i32>,
}

/// 视频子流程结果:下载是否成功、音频是否提取成功、失败原因、音频路径。
struct VideoOutcome {
    downloaded: bool,
    audio_extracted: Option<bool>,
    error: Option<String>,
    /// 转出的音频本地路径(转码成功时填,供转写)
    audio_path: Option<String>,
}

/// output_dir 为空时的回退子目录名(相对配置目录)。
const FALLBACK_MEDIA_DIR: &str = "media";
/// 视频形态目录名(目录「类目」按内容形态划分:视频 / 图文)。路径统一用英文。
const DIR_VIDEO: &str = "video";
/// 图文形态目录名。
const DIR_IMAGE: &str = "image";
/// 作者头像分组目录名。头像按作者去重存一份,不随内容/日期/形态分散。
const DIR_AVATAR: &str = "avatar";
/// 视频转出的音频单独分组目录:不与封面/视频同目录,便于检索与转写读取。
const DIR_AUDIO: &str = "audio";
/// 作者头像本地缓存有效期(秒):超过则删旧重下,保证头像不长期陈旧。7 天。
const AVATAR_TTL_SECS: u64 = 7 * 24 * 3600;
/// 文件名中需替换掉的非法字符(Windows 文件系统保留 + 路径分隔符)。
const ILLEGAL_FILENAME_CHARS: &[char] = &['/', '\\', ':', '*', '?', '"', '<', '>', '|'];
/// 文件名前缀最大字符数:content_id / uid 来自平台响应(外部输入),
/// 过长会触发 Windows 260 字符路径上限导致整条素材写入失败。
const MAX_FILENAME_PREFIX_CHARS: usize = 120;
/// 视频拉流转音频的最大尝试次数:抖音等 CDN 偶发「收到请求不返响应直接断」,失败再原样重试。
const MAX_EXTRACT_ATTEMPTS: usize = 2;
/// 拉流转音频两次尝试之间的退避(毫秒),给 CDN 短暂喘息后重试。
const EXTRACT_RETRY_DELAY_MS: u64 = 500;

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

/// 防盗链 Referer 映射:这些平台的 CDN 校验 Referer,缺失会 403。
/// 按 URL 子串命中;未命中的域名保持原行为(不加任何头),不影响既有平台。
const REFERER_BY_CDN: &[(&str, &str)] = &[
    // B站图片(hdslb)与音视频流(bilivideo)
    ("hdslb.com", "https://www.bilibili.com/"),
    ("bilivideo.com", "https://www.bilibili.com/"),
    ("tiktokcdn", "https://www.tiktok.com/"),
    ("ytimg.com", "https://www.youtube.com/"),
    ("googlevideo.com", "https://www.youtube.com/"),
];

/// 防盗链 Referer 按「内容所属平台」映射:抖音/快手/小红书等视频 CDN 缺 Referer 直接 403。
/// 这些平台的视频 CDN 域名多变(douyinvod / kwaicdn / sns-video 等),按 CDN 子串匹配易漏,
/// 而采集时 content.platform 是确定的——故优先按平台解析,比 REFERER_BY_CDN 更稳。
const REFERER_BY_PLATFORM: &[(&str, &str)] = &[
    ("douyin", "https://www.douyin.com/"),
    ("kuaishou", "https://www.kuaishou.com/"),
    ("xhs", "https://www.xiaohongshu.com/"),
    ("bilibili", "https://www.bilibili.com/"),
    ("tiktok", "https://www.tiktok.com/"),
    ("youtube", "https://www.youtube.com/"),
];

/// 防盗链 CDN 同时校验 UA,配 Referer 一起带上浏览器 UA。
/// 抖音 CDN 对「半成品 UA」会直接 close TCP 不返响应,故必须带完整 AppleWebKit...Safari 后缀。
const BROWSER_UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0.0.0 Safari/537.36";

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
    let mut req = client.get(url);
    if let Some((_, referer)) = REFERER_BY_CDN.iter().find(|(cdn, _)| url.contains(cdn)) {
        req = req
            .header(reqwest::header::REFERER, *referer)
            .header(reqwest::header::USER_AGENT, BROWSER_UA);
    }
    let resp = req.send().await?.error_for_status()?;
    let bytes = resp.bytes().await?;
    tokio::fs::write(path, &bytes).await?;
    Ok(())
}

/// 文件存在且修改时间在 ttl 秒内为「新鲜」。读元数据 / 系统时间失败按不新鲜处理(触发重下)。
async fn is_file_fresh(path: &Path, ttl_secs: u64) -> bool {
    let Ok(meta) = tokio::fs::metadata(path).await else {
        return false;
    };
    match meta.modified() {
        Ok(modified) => modified
            .elapsed()
            .map(|age| age.as_secs() < ttl_secs)
            .unwrap_or(false),
        Err(_) => false,
    }
}

/// 用 ffmpeg 直接从视频直链拉流转音频(不落地视频文件):
/// `-y -reconnect... [-user_agent UA -headers "Referer/Origin"] -i <url> -vn [mp3优化参数] <audio>`。
/// 防盗链 CDN 需带 Referer + 完整 UA(口径与 download_to_file 一致);HTTP 直链可被 ffmpeg 按 range
/// 寻址,故不受 mp4 moov 在文件尾部影响。输出为 mp3 时按语音转写优化(单声道 22kHz 96k,体积减半、
/// 转码更快,-threads 1 防并发互抢)。ffmpeg_path 为空用系统 PATH 的 `ffmpeg`,退出码非 0 视为失败。
pub fn extract_audio_from_url(
    url: &str,
    audio: &Path,
    ffmpeg_path: Option<&str>,
    referer: Option<&str>,
) -> Result<()> {
    let program = ffmpeg_path
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .unwrap_or("ffmpeg");
    let mut cmd = std::process::Command::new(program);
    cmd.arg("-y"); // 覆盖已存在的输出,避免交互确认卡住
    // CDN 偶发中途断流:让 ffmpeg 自行重连续传,避免一断就整条失败(须在 -i 之前作为输入选项)
    cmd.args([
        "-reconnect",
        "1",
        "-reconnect_streamed",
        "1",
        "-reconnect_delay_max",
        "2",
    ]);
    // 防盗链直链:UA + Referer/Origin 必须作为「输入选项」放在 -i 之前
    if let Some(ref_url) = referer {
        let origin = ref_url.trim_end_matches('/'); // Origin 不带末尾斜杠
        cmd.arg("-user_agent")
            .arg(BROWSER_UA)
            .arg("-headers")
            .arg(format!("Referer: {ref_url}\r\nOrigin: {origin}\r\n"));
    }
    cmd.arg("-i").arg(url).arg("-vn"); // -vn 丢视频流,只保留音频
    // mp3 输出按语音转写优化:单声道 22kHz 96k 足够 ASR,体积/转码成本减半
    let is_mp3 = audio
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.eq_ignore_ascii_case("mp3"))
        .unwrap_or(false);
    if is_mp3 {
        cmd.args([
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
        ]);
    }
    cmd.arg(audio);
    let status = cmd
        .status()
        .map_err(|e| CrawlerError::Parse(format!("启动 ffmpeg 失败: {e}")))?;
    if !status.success() {
        return Err(CrawlerError::Parse(format!(
            "ffmpeg 拉流转音频失败,退出码: {:?}",
            status.code()
        )));
    }
    Ok(())
}

/// 探测 ffmpeg 是否可用:用 `<program> -version` 起一次进程,退出码 0 视为可用,
/// 返回版本信息首行(形如 "ffmpeg version ...")。program 解析口径与 extract_audio 一致:
/// ffmpeg_path 为空时探测系统 PATH 的 `ffmpeg`。探测失败 / 找不到可执行文件统一返回 None。
pub fn probe_ffmpeg(ffmpeg_path: Option<&str>) -> Option<String> {
    let program = ffmpeg_path
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .unwrap_or("ffmpeg");
    let output = std::process::Command::new(program)
        .arg("-version")
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    stdout
        .lines()
        .next()
        .map(|line| line.trim().to_string())
        .filter(|line| !line.is_empty())
}

/// 把内容 ID 清洗为合法文件名前缀:替换非法字符为 `_`,限长,空值兜底为 "unknown"。
fn sanitize_filename(raw: &str) -> String {
    let cleaned: String = raw
        .chars()
        .take(MAX_FILENAME_PREFIX_CHARS)
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
/// 目录结构 `{root}/{platform}/{今天 YYYY-MM-DD}/{video|image}/`(封面/图文图片),
/// 视频转出的音频另存 `.../{今天}/audio/`,文件名以 content_id 为前缀。
/// 副产品(封面/头像/图片)失败仅 `tracing::warn!`;主素材成败汇总进 `MediaOutcome` 返回供回写。
pub async fn process_content(
    content: &Content,
    root: &Path,
    media: &MediaConfig,
    ai_extract: bool,
) -> MediaOutcome {
    let kind_dir = if content.kind == ContentKind::Video {
        DIR_VIDEO
    } else {
        DIR_IMAGE
    };
    // 用本机当天日期分目录,便于按天归档检索
    let today = Local::now().format("%Y-%m-%d").to_string();
    let dir = root.join(&content.platform).join(&today).join(kind_dir);
    if let Err(e) = tokio::fs::create_dir_all(&dir).await {
        tracing::warn!(content_id = %content.content_id, "创建素材目录失败,跳过该条: {e}");
        return MediaOutcome {
            ok: false,
            audio_extracted: None,
            error: Some(format!("创建素材目录失败: {e}")),
            cover_path: None,
            avatar_path: None,
            audio_path: None,
            video_downloaded: None,
            image_total: None,
            image_done: None,
        };
    }

    let prefix = sanitize_filename(&content.content_id);

    // 封面:下载成功记录本地绝对路径,供前端本地优先显示
    let mut cover_path = None;
    if let Some(cover) = content.cover_url.as_deref().filter(|s| !s.is_empty()) {
        let path = dir.join(format!("{prefix}_cover.jpg"));
        match download_to_file(cover, &path).await {
            Ok(()) => cover_path = Some(path.to_string_lossy().into_owned()),
            Err(e) => tracing::warn!(content_id = %content.content_id, "下载封面失败: {e}"),
        }
    }

    // 作者头像:单独 avatar 分组,按作者 uid 命名去重(同作者多条内容共用一份,已存在则不重下)
    let mut avatar_path = None;
    if let Some(avatar) = content.author.avatar.as_deref().filter(|s| !s.is_empty()) {
        let uid = sanitize_filename(&content.author.uid);
        if uid != "unknown" {
            let avatar_dir = root.join(&content.platform).join(DIR_AVATAR);
            match tokio::fs::create_dir_all(&avatar_dir).await {
                Ok(()) => {
                    let path = avatar_dir.join(format!("{uid}.jpg"));
                    // 头像 7 天节流:未过期则复用;过期(或不存在)则删旧重下,避免头像长期陈旧
                    if is_file_fresh(&path, AVATAR_TTL_SECS).await {
                        avatar_path = Some(path.to_string_lossy().into_owned());
                    } else {
                        // 过期先删旧再下新(文件不存在时删除失败可忽略)
                        let _ = tokio::fs::remove_file(&path).await;
                        match download_to_file(avatar, &path).await {
                            Ok(()) => avatar_path = Some(path.to_string_lossy().into_owned()),
                            Err(e) => tracing::warn!(content_id = %content.content_id, "下载头像失败: {e}"),
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!(content_id = %content.content_id, "创建头像目录失败: {e}");
                }
            }
        }
    }

    // 主素材成败:视频内容以视频/音频为准,图文内容以图片为准
    let mut outcome = MediaOutcome {
        ok: true,
        audio_extracted: None,
        error: None,
        cover_path,
        avatar_path,
        audio_path: None,
        video_downloaded: None,
        image_total: None,
        image_done: None,
    };

    // 视频:仅当任务开启「AI 文案提取」才下载并转音频(只留音频);
    // 未开则视频不下载、不存储——不需要文案就不留视频/音频。
    if content.kind == ContentKind::Video && ai_extract {
        match content.video_url.as_deref().filter(|s| !s.is_empty()) {
            Some(video_url) => {
                // 音频单独存到 audio 目录(与封面/视频分开),便于检索与转写读取
                let audio_dir = root.join(&content.platform).join(&today).join(DIR_AUDIO);
                if let Err(e) = tokio::fs::create_dir_all(&audio_dir).await {
                    tracing::warn!(content_id = %content.content_id, "创建音频目录失败: {e}");
                    outcome.ok = false;
                    outcome.error = Some(format!("创建音频目录失败: {e}"));
                    outcome.video_downloaded = Some(false);
                } else {
                    let video = process_video(content, &audio_dir, &prefix, video_url, media).await;
                    outcome.ok = video.downloaded;
                    outcome.audio_extracted = video.audio_extracted;
                    outcome.error = video.error;
                    outcome.audio_path = video.audio_path;
                    outcome.video_downloaded = Some(video.downloaded);
                }
            }
            None => {
                // 视频内容却无直链:多为详情解析失败,标记失败(重试需重新采集刷新链接)
                outcome.ok = false;
                outcome.error = Some("无视频直链".to_string());
                outcome.video_downloaded = Some(false);
            }
        }
    }

    // 图文图片:逐张下载。统计总数/成功数,任一张失败即记失败,供重试。
    let mut image_failed = false;
    let mut image_error: Option<String> = None;
    let mut img_total = 0i32;
    let mut img_done = 0i32;
    for (idx, img_url) in content.image_urls.iter().enumerate() {
        if img_url.is_empty() {
            continue;
        }
        img_total += 1;
        let path = dir.join(format!("{prefix}_img{idx}.jpg"));
        match download_to_file(img_url, &path).await {
            Ok(()) => img_done += 1,
            Err(e) => {
                tracing::warn!(content_id = %content.content_id, index = idx, "下载图片失败: {e}");
                image_failed = true;
                image_error = Some(format!("下载图片失败: {e}"));
            }
        }
    }
    // 非视频内容(图文/文章/未知)以图片下载结果为准
    if content.kind != ContentKind::Video {
        outcome.ok = !image_failed;
        outcome.error = image_error;
        outcome.image_total = Some(img_total);
        outcome.image_done = Some(img_done);
    }

    outcome
}

/// 视频子流程:不落地视频,直接让 ffmpeg 从视频直链拉流转音频并保存到 audio 目录(只留音频)。
/// ffmpeg 在阻塞线程池(spawn_blocking)执行,不占异步运行时工作线程。
async fn process_video(
    content: &Content,
    audio_dir: &Path,
    prefix: &str,
    video_url: &str,
    media: &MediaConfig,
) -> VideoOutcome {
    let audio_format = if media.audio_format.trim().is_empty() {
        "mp3"
    } else {
        media.audio_format.trim()
    };
    let audio_path = audio_dir.join(format!("{prefix}.{audio_format}"));

    // 防盗链 Referer 优先按内容所属平台解析(视频 CDN 域名多变,按平台比按 CDN 子串更稳),
    // 平台未命中再退回 CDN 子串匹配。referer 是 &'static str,可直接进 spawn_blocking 闭包。
    let referer = REFERER_BY_PLATFORM
        .iter()
        .find(|(platform, _)| content.platform == *platform)
        .map(|(_, r)| *r)
        .or_else(|| {
            REFERER_BY_CDN
                .iter()
                .find(|(cdn, _)| video_url.contains(cdn))
                .map(|(_, r)| *r)
        });

    // ffmpeg 同步阻塞,挪到阻塞线程池;直接从直链拉流转音频,不下载/不落地视频文件。
    // 抖音等 CDN 偶发「收到请求不返响应直接断」,失败后短暂退避再原样重试一次。
    let mut last_error: Option<String> = None;
    for attempt in 1..=MAX_EXTRACT_ATTEMPTS {
        let url_for_task = video_url.to_string();
        let audio_for_task = audio_path.clone();
        let ffmpeg_for_task = media.ffmpeg_path.clone();
        let result = tokio::task::spawn_blocking(move || {
            extract_audio_from_url(
                &url_for_task,
                &audio_for_task,
                ffmpeg_for_task.as_deref(),
                referer,
            )
        })
        .await;

        match result {
            Ok(Ok(())) => {
                return VideoOutcome {
                    downloaded: true,
                    audio_extracted: Some(true),
                    error: None,
                    audio_path: Some(audio_path.to_string_lossy().into_owned()),
                };
            }
            Ok(Err(e)) => {
                tracing::warn!(content_id = %content.content_id, attempt, "视频拉流转音频失败: {e}");
                last_error = Some(format!("音频提取失败: {e}"));
            }
            Err(e) => {
                tracing::warn!(content_id = %content.content_id, attempt, "转码任务异常: {e}");
                last_error = Some(format!("转码任务异常: {e}"));
            }
        }

        if attempt < MAX_EXTRACT_ATTEMPTS {
            tokio::time::sleep(std::time::Duration::from_millis(EXTRACT_RETRY_DELAY_MS)).await;
        }
    }

    VideoOutcome {
        downloaded: false,
        audio_extracted: Some(false),
        error: last_error,
        audio_path: None,
    }
}
