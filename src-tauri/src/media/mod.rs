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

// ffmpeg(libavformat)拉流失败时进程退出码即 AVERROR 负值。HTTP 错误形如
// `-MKTAG(0xF8,'4','0','3')`,直接看是「魔法负数」。这里登记常见几种,把退出码翻译成
// 可读 HTTP 状态——典型:海外 CDN(TikTok)防盗链/地域限制返回 403。
const FFMPEG_HTTP_401: i32 = -825242872; // -MKTAG(0xF8,'4','0','1') 未授权
const FFMPEG_HTTP_403: i32 = -858797304; // -MKTAG(0xF8,'4','0','3') 禁止访问(防盗链/地域)
const FFMPEG_HTTP_404: i32 = -875574520; // -MKTAG(0xF8,'4','0','4') 直链失效
const FFMPEG_HTTP_4XX: i32 = -1482175736; // -MKTAG(0xF8,'4','X','X') 其它 4xx
const FFMPEG_HTTP_5XX: i32 = -1482175992; // -MKTAG(0xF8,'5','X','X') 服务端 5xx

/// 把 ffmpeg 退出码翻译成可读说明(识别上面登记的 AVERROR HTTP 码),便于排查;
/// 未登记的码原样回显,被信号终止(无退出码)单独标注。
fn describe_ffmpeg_exit(code: Option<i32>) -> String {
    match code {
        Some(FFMPEG_HTTP_403) => "HTTP 403 拒绝(防盗链/地域限制:缺会话 Cookie 或未走代理)".to_string(),
        Some(FFMPEG_HTTP_401) => "HTTP 401 未授权".to_string(),
        Some(FFMPEG_HTTP_404) => "HTTP 404 直链已失效".to_string(),
        Some(FFMPEG_HTTP_4XX) => "HTTP 4xx 客户端错误".to_string(),
        Some(FFMPEG_HTTP_5XX) => "HTTP 5xx 服务端错误".to_string(),
        Some(code) => format!("退出码 {code}"),
        None => "进程被信号终止".to_string(),
    }
}

/// ffmpeg 拉流要走的代理:子进程**不读 Windows「系统代理」(注册表)**,也未必继承大小写各异的
/// 代理环境变量,故显式探测后用 `-http_proxy` 传给它——否则 TikTok 等海外 CDN 会用本机直连 IP
/// 按地域 403(浏览器/WebView 走系统代理能采到,ffmpeg 直连却被拒)。探测顺序:
/// 常见代理环境变量(各种大小写)→ Windows 系统代理(注册表)。都没有则返回 None(行为不变)。
fn detect_proxy() -> Option<String> {
    // 1) 环境变量:覆盖 TUN / 手动 export 代理的场景(大小写都查)
    const ENV_KEYS: &[&str] = &[
        "all_proxy",
        "ALL_PROXY",
        "https_proxy",
        "HTTPS_PROXY",
        "http_proxy",
        "HTTP_PROXY",
    ];
    for key in ENV_KEYS {
        if let Ok(val) = std::env::var(key) {
            let val = val.trim();
            if !val.is_empty() {
                return Some(normalize_proxy_url(val));
            }
        }
    }
    // 2) Windows 系统代理:与浏览器 / WebView 同源,采集能成说明它有效
    #[cfg(windows)]
    if let Some(proxy) = windows_system_proxy() {
        return Some(normalize_proxy_url(&proxy));
    }
    None
}

/// 仅海外平台 CDN 需要走代理(域名子串命中)。国内 CDN(抖音/快手/小红书/B站)直连,
/// 不受系统代理影响,避免「全局节点」把国内流量绕到境外反而变慢 / 被拒。
const OVERSEAS_CDN_MARKERS: &[&str] = &["tiktok", "ytimg.com", "googlevideo.com"];

/// 该直链是否属于需要代理的海外 CDN。
fn url_needs_proxy(url: &str) -> bool {
    OVERSEAS_CDN_MARKERS.iter().any(|marker| url.contains(marker))
}

/// 代理串补全 scheme:ffmpeg 的 `-http_proxy` 需要带 scheme 的 URL;`host:port` 形态补 `http://`。
fn normalize_proxy_url(raw: &str) -> String {
    let raw = raw.trim();
    if raw.contains("://") {
        raw.to_string()
    } else {
        format!("http://{raw}")
    }
}

/// 读 Windows「系统代理」(Internet Settings 注册表):ProxyEnable=1 时取 ProxyServer。
/// ProxyServer 可能是统一 `host:port`,也可能是分协议列表 `http=h:p;https=h:p;...`。
#[cfg(windows)]
fn windows_system_proxy() -> Option<String> {
    use windows::core::w;
    use windows::Win32::Foundation::ERROR_SUCCESS;
    use windows::Win32::System::Registry::{
        RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RRF_RT_REG_SZ,
    };
    let subkey = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Internet Settings");

    // ProxyEnable(DWORD):0 / 读取失败都视为未开代理
    let mut enabled: u32 = 0;
    let mut dword_size = std::mem::size_of::<u32>() as u32;
    let enable_ret = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            subkey,
            w!("ProxyEnable"),
            RRF_RT_REG_DWORD,
            None,
            Some(&mut enabled as *mut u32 as *mut std::ffi::c_void),
            Some(&mut dword_size),
        )
    };
    if enable_ret != ERROR_SUCCESS || enabled == 0 {
        return None;
    }

    // ProxyServer(REG_SZ):先探长度(字节)再按长度取宽字符串
    let mut byte_len: u32 = 0;
    let probe_ret = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            subkey,
            w!("ProxyServer"),
            RRF_RT_REG_SZ,
            None,
            None,
            Some(&mut byte_len),
        )
    };
    if probe_ret != ERROR_SUCCESS || byte_len == 0 {
        return None;
    }
    let mut buf = vec![0u16; (byte_len as usize) / 2];
    let mut buf_size = byte_len;
    let read_ret = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            subkey,
            w!("ProxyServer"),
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr() as *mut std::ffi::c_void),
            Some(&mut buf_size),
        )
    };
    if read_ret != ERROR_SUCCESS {
        return None;
    }
    let server = String::from_utf16_lossy(&buf);
    let server = server.trim_end_matches('\0').trim();
    if server.is_empty() {
        None
    } else {
        Some(pick_proxy_entry(server))
    }
}

/// 从 ProxyServer 串取出可用代理:含 `=` 的是分协议列表,优先 https= 再 http=;否则整串即统一代理。
#[cfg(windows)]
fn pick_proxy_entry(raw: &str) -> String {
    if !raw.contains('=') {
        return raw.to_string();
    }
    let mut http_proxy = None;
    for part in raw.split(';') {
        let part = part.trim();
        if let Some(value) = part.strip_prefix("https=") {
            return value.trim().to_string();
        }
        if let Some(value) = part.strip_prefix("http=") {
            http_proxy = Some(value.trim().to_string());
        }
    }
    http_proxy.unwrap_or_else(|| raw.to_string())
}

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
/// `-y -reconnect... [-http_proxy P] [-user_agent UA -headers "Referer/Origin/Cookie"] -i <url> -vn ...`。
/// 口径对齐浏览器:防盗链 CDN 需带 Referer + 完整 UA + 会话 Cookie(TikTok 等校验会话);海外 CDN 还需
/// 与 WebView 同源的代理(子进程不读系统代理,见 detect_proxy)。HTTP 直链可被 ffmpeg 按 range 寻址,
/// 故不受 mp4 moov 在文件尾部影响。输出为 mp3 时按语音转写优化(单声道 22kHz 96k,体积减半、转码更快,
/// -threads 1 防并发互抢)。ffmpeg_path 为空用系统 PATH 的 `ffmpeg`,退出码非 0 视为失败。
pub fn extract_audio_from_url(
    url: &str,
    audio: &Path,
    ffmpeg_path: Option<&str>,
    referer: Option<&str>,
    cookie: Option<&str>,
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
    // 海外 CDN 地域限制:仅对海外平台直链补代理(国内 CDN 直连,避免全局节点把国内流量绕远/拒绝);
    // ffmpeg 子进程不走系统代理,故显式补一条与浏览器同源的代理(-i 之前)
    if url_needs_proxy(url) {
        if let Some(proxy) = detect_proxy() {
            cmd.arg("-http_proxy").arg(proxy);
        }
    }
    // 防盗链 / 会话头:Referer+Origin+Cookie 一起带,作为「输入选项」放在 -i 之前,口径对齐浏览器
    let mut header_lines: Vec<String> = Vec::new();
    if let Some(ref_url) = referer {
        let origin = ref_url.trim_end_matches('/'); // Origin 不带末尾斜杠
        header_lines.push(format!("Referer: {ref_url}"));
        header_lines.push(format!("Origin: {origin}"));
    }
    if let Some(ck) = cookie.map(str::trim).filter(|c| !c.is_empty()) {
        header_lines.push(format!("Cookie: {ck}"));
    }
    if !header_lines.is_empty() {
        // ffmpeg 的 -headers 各行以 \r\n 分隔(含末行),UA 单独走 -user_agent
        let headers: String = header_lines.iter().map(|line| format!("{line}\r\n")).collect();
        cmd.arg("-user_agent").arg(BROWSER_UA).arg("-headers").arg(headers);
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
            "ffmpeg 拉流转音频失败:{}",
            describe_ffmpeg_exit(status.code())
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
    cookie: Option<&str>,
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
                    let video =
                        process_video(content, &audio_dir, &prefix, video_url, media, cookie).await;
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
    cookie: Option<&str>,
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
        // cookie 是借用,而 spawn_blocking 闭包要求 'static,故转 owned 再 move 进去
        let cookie_for_task = cookie.map(str::to_string);
        let result = tokio::task::spawn_blocking(move || {
            extract_audio_from_url(
                &url_for_task,
                &audio_for_task,
                ffmpeg_for_task.as_deref(),
                referer,
                cookie_for_task.as_deref(),
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
