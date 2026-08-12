//! 采集素材下载与「视频转音频」处理。
//!
//! 时机:采集落库后由 run_task 在后台触发,对每条内容下载全部素材
//! (封面、作者头像、图文图片;视频则下载后用 ffmpeg 转音频并删除原视频)。
//!
//! 设计取向:单条/单步素材失败只告警不中断——素材是「采集的副产品」,
//! 任一 URL 失效或网络抖动都不应拖垮整条内容乃至整批的素材处理。

use crate::model::{Content, ContentKind};
use chrono::Local;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};
use veltrix_core::config::MediaConfig;
use veltrix_core::error::{CrawlerError, Result};
use webrtc_vad::{SampleRate, Vad, VadMode};

/// Windows 打包(GUI 子系统)后 spawn 控制台子进程(ffmpeg 等)会弹出黑色终端窗口,
/// 统一加 CREATE_NO_WINDOW 抑制;非 Windows 平台为 no-op。
pub(crate) fn hide_console_window(cmd: &mut std::process::Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(not(windows))]
    let _ = cmd;
}

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
    /// 视频文件是否下载成功(仅 video + 音频提取);None=非视频/未尝试
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
/// 单次拉流转音频的整体超时:长视频正常提取也就几分钟,10 分钟未完视为挂起,杀进程记失败。
const FFMPEG_EXTRACT_MAX: std::time::Duration = std::time::Duration::from_secs(600);
/// ffmpeg 子进程并发上限:素材下载已按 10 路并发,但视频转音频每个都起一个 ffmpeg;
/// 全开会打满 CPU / 出口带宽并放大 CDN 并发限制,故对 ffmpeg 单独限流,与 HTTP 下载解耦。
const MAX_FFMPEG_CONCURRENCY: usize = 3;
static FFMPEG_SEMAPHORE: LazyLock<Arc<tokio::sync::Semaphore>> = LazyLock::new(|| {
    Arc::new(tokio::sync::Semaphore::new(MAX_FFMPEG_CONCURRENCY))
});

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

/// 代理配置为该值(忽略大小写)时表示「关闭代理」——即便本机有系统代理也强制直连。
const PROXY_DISABLED: &str = "off";

/// 按用户配置解析实际代理(仅在命中海外 CDN 时才会被调用):
/// 空 → 自动探测本机代理(`detect_proxy`);`"off"` → None(直连);其余 → 按该 URL(补 scheme)。
fn resolve_proxy(setting: &str) -> Option<String> {
    let setting = setting.trim();
    if setting.is_empty() {
        detect_proxy()
    } else if setting.eq_ignore_ascii_case(PROXY_DISABLED) {
        None
    } else {
        Some(normalize_proxy_url(setting))
    }
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

/// 素材下载连接 / 整体超时(秒):避免个别 hang 住的 CDN 直链无限阻塞,拖垮整批素材下载。
const DOWNLOAD_CONNECT_TIMEOUT_SECS: u64 = 15;
const DOWNLOAD_TOTAL_TIMEOUT_SECS: u64 = 120;

/// 素材下载共享 HTTP 客户端:reqwest Client 内部自带连接池,全局唯一才吃得到 keep-alive;
/// 此前每次下载都新建客户端,同一 CDN 的每张图都重做一次 TCP+TLS 握手。
/// 构建失败(TLS 后端初始化异常等)时保留 Err,由 download_to_file 逐次报错,与旧行为一致。
static DOWNLOAD_CLIENT: LazyLock<reqwest::Result<reqwest::Client>> = LazyLock::new(|| {
    reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(DOWNLOAD_CONNECT_TIMEOUT_SECS))
        .timeout(std::time::Duration::from_secs(DOWNLOAD_TOTAL_TIMEOUT_SECS))
        .build()
});

/// 下载 URL 到本地文件。reqwest 拉取字节后整体写入;失败返回错误供调用方告警。
pub async fn download_to_file(url: &str, path: &Path) -> Result<()> {
    if url.trim().is_empty() {
        return Err(CrawlerError::Parse("下载地址为空".into()));
    }
    let client = DOWNLOAD_CLIENT
        .as_ref()
        .map_err(|e| CrawlerError::Parse(format!("初始化下载客户端失败: {e}")))?;
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

/// 同作者头像下载互斥(键:平台-uid):并发批量处理内容时,同作者的多条内容会同时发现
/// 头像「不新鲜」而各自重复下载、互相覆盖写同一文件;加锁让首个任务下载,其余等锁后经
/// 新鲜检查命中、直接复用。锁表随进程累积(每作者一个空 Mutex 的 Arc),量级可忽略。
static AVATAR_LOCKS: LazyLock<Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// 取(或建)某作者头像的下载锁。锁表中毒时接管内层数据继续(表内只是 Arc,无不变量可破坏)。
fn avatar_lock(key: &str) -> Arc<tokio::sync::Mutex<()>> {
    let mut locks = AVATAR_LOCKS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    locks.entry(key.to_string()).or_default().clone()
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
    proxy_setting: &str,
    // 取消标志:任务被手动停止时置位,等待循环 500ms 内感知并强杀 ffmpeg 子进程
    cancel: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
) -> Result<()> {
    let program = ffmpeg_path
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .unwrap_or("ffmpeg");
    let mut cmd = std::process::Command::new(program);
    hide_console_window(&mut cmd);
    cmd.arg("-y"); // 覆盖已存在的输出,避免交互确认卡住
    // CDN 偶发中途断流:让 ffmpeg 自行重连续传,避免一断就整条失败(须在 -i 之前作为输入选项)
    // -rw_timeout(微秒):单次 I/O 停滞上限,CDN 建立连接后滴流/不返数据时 30s 无数据即报错退出,
    // 否则 ffmpeg 会在这种连接上挂住数小时(整体兜底见下方等待循环)
    cmd.args([
        "-reconnect",
        "1",
        "-reconnect_streamed",
        "1",
        "-reconnect_delay_max",
        "2",
        "-rw_timeout",
        "30000000",
    ]);
    // 海外 CDN 地域限制:仅对海外平台直链补代理(国内 CDN 直连,避免全局节点把国内流量绕远/拒绝);
    // ffmpeg 子进程不走系统代理,故按用户配置(空=自动探测)显式补一条代理(-i 之前)
    if url_needs_proxy(url) {
        if let Some(proxy) = resolve_proxy(proxy_setting) {
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
    // 整体超时兜底:-rw_timeout 管单次 I/O 停滞,这里管总时长(长视频正常提取也就几分钟)。
    // 超时杀子进程记失败——挂起的 ffmpeg 会占死 spawn_blocking 线程,10 路并发全挂即拖垮整个下载阶段
    let mut child = cmd
        .spawn()
        .map_err(|e| CrawlerError::Parse(format!("启动 ffmpeg 失败: {e}")))?;
    let deadline = std::time::Instant::now() + FFMPEG_EXTRACT_MAX;
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break s,
            Ok(None) => {
                // 任务手动停止:立即强杀子进程,不让在飞的 ffmpeg 继续拖
                if cancel
                    .as_ref()
                    .map(|c| c.load(std::sync::atomic::Ordering::Relaxed))
                    .unwrap_or(false)
                {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(CrawlerError::Parse("已手动停止,ffmpeg 已终止".into()));
                }
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(CrawlerError::Parse(format!(
                        "ffmpeg 拉流转音频超时(超过 {} 分钟),已终止",
                        FFMPEG_EXTRACT_MAX.as_secs() / 60
                    )));
                }
                std::thread::sleep(std::time::Duration::from_millis(500));
            }
            Err(e) => return Err(CrawlerError::Parse(format!("等待 ffmpeg 退出失败: {e}"))),
        }
    };
    if !status.success() {
        return Err(CrawlerError::Parse(format!(
            "ffmpeg 拉流转音频失败:{}",
            describe_ffmpeg_exit(status.code())
        )));
    }
    Ok(())
}

/// 把本地音频按时长切片(语音转写用:单文件超过 ASR 体积上限时先切再逐段转写)。
/// 用 ffmpeg 的 segment muxer、`-c copy` 不重编码(快且无损);切片命名为 chunk_0001.mp3 起,
/// 输出目录由调用方创建/清理。返回按文件名排序的切片路径(顺序即时间顺序)。
/// ffmpeg_path 为空用系统 PATH 的 `ffmpeg`,退出码非 0 或无产出切片视为失败。
pub fn split_audio(
    audio: &Path,
    out_dir: &Path,
    segment_seconds: u32,
    ffmpeg_path: Option<&str>,
) -> Result<Vec<PathBuf>> {
    let program = ffmpeg_path
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .unwrap_or("ffmpeg");
    std::fs::create_dir_all(out_dir)
        .map_err(|e| CrawlerError::Parse(format!("创建切片目录失败: {e}")))?;
    let pattern = out_dir.join("chunk_%04d.mp3");
    let mut cmd = std::process::Command::new(program);
    hide_console_window(&mut cmd);
    let status = cmd
        .arg("-y") // 覆盖已存在切片,避免交互确认卡住
        .arg("-i")
        .arg(audio)
        .arg("-f")
        .arg("segment")
        .arg("-segment_time")
        .arg(segment_seconds.to_string())
        .arg("-c")
        .arg("copy") // 不重编码:按帧边界切,速度快、音质无损
        .arg("-threads")
        .arg("1")
        .arg(&pattern)
        .status()
        .map_err(|e| CrawlerError::Parse(format!("启动 ffmpeg 失败: {e}")))?;
    if !status.success() {
        return Err(CrawlerError::Parse(format!(
            "ffmpeg 音频切片失败:{}",
            describe_ffmpeg_exit(status.code())
        )));
    }
    let mut chunks: Vec<PathBuf> = std::fs::read_dir(out_dir)
        .map_err(|e| CrawlerError::Parse(format!("读取切片目录失败: {e}")))?
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|p| {
            p.extension()
                .and_then(|e| e.to_str())
                .map(|e| e.eq_ignore_ascii_case("mp3"))
                .unwrap_or(false)
        })
        .collect();
    // 文件名零填充序号,字典序即时间序
    chunks.sort();
    if chunks.is_empty() {
        return Err(CrawlerError::Parse("ffmpeg 音频切片无产出".into()));
    }
    Ok(chunks)
}

// ---- 静音点切片(语音转写优化) ----

/// silencedetect 的判定参数:噪声门限 -35dB、最短静音 0.5s。
/// 口播类音频的常用起点:门限太松会把气口当句子边界,太严会检测不到静音退化为硬切。
const SILENCE_DETECT_FILTER: &str = "silencedetect=noise=-35dB:d=0.5";
/// 切点距当前段起点至少 1 秒,避免切出过短的碎片段。
const MIN_CHUNK_SECS: f64 = 1.0;

/// 一段静音区间(秒,绝对时间轴)。
#[derive(Debug, Clone, Copy, PartialEq)]
struct SilenceRange {
    start: f64,
    end: f64,
}

/// 语音间隙优先切片(ASR 用):先 WebRTC VAD 逐帧判定人声,把连续非人声 ≥0.5s 的区间作为
/// 可切间隙(BGM 不算人声,带背景音乐的视频也能找到句间缝隙);VAD 失败/全程无间隙时降级
/// silencedetect 能量静音探测,仍无结果回退 `split_audio` 按时长硬切,保证任何音频都有产出。
/// 切点选在「不超过时长上限的最后一个间隙中点」,避免硬切把句中/词中劈开导致 ASR 边界错字。
/// 返回切片路径(顺序即时间顺序),目录由调用方清理。
pub fn split_audio_for_asr(
    audio: &Path,
    out_dir: &Path,
    max_seconds: u32,
    ffmpeg_path: Option<&str>,
) -> Result<Vec<PathBuf>> {
    let program = ffmpeg_path
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .unwrap_or("ffmpeg");
    // 探测 / 时长读取 / 切点规划任一步失败都沿「VAD → 静音 → 硬切」回退,行为不差于原硬切逻辑
    let fallback = || split_audio(audio, out_dir, max_seconds, ffmpeg_path);
    let gaps = match detect_speech_gaps(program, audio) {
        Ok(g) if !g.is_empty() => g,
        other => {
            match &other {
                Ok(_) => tracing::info!("VAD 未检出人声间隙,尝试静音探测"),
                Err(e) => tracing::warn!("VAD 探测失败({e}),尝试静音探测"),
            }
            match detect_silences(program, audio) {
                Ok(s) if !s.is_empty() => s,
                Ok(_) => {
                    tracing::info!("未检测到静音段,回退按时长硬切");
                    return fallback();
                }
                Err(e) => {
                    tracing::warn!("静音探测失败({e}),回退按时长硬切");
                    return fallback();
                }
            }
        }
    };
    let duration = match probe_duration(program, audio) {
        Some(d) if d > 0.0 => d,
        _ => {
            tracing::warn!("读取音频时长失败,回退按时长硬切");
            return fallback();
        }
    };
    let cuts = plan_silence_cuts(duration, f64::from(max_seconds), &gaps);
    if cuts.is_empty() {
        return fallback();
    }
    cut_audio_at(program, audio, out_dir, &cuts).or_else(|e| {
        tracing::warn!("按语音间隙切片失败({e}),回退按时长硬切");
        // 清掉可能已产出的半成品切片,避免与硬切产物混在同一目录被一起收走
        let _ = std::fs::remove_dir_all(out_dir);
        fallback()
    })
}

/// VAD 帧长:WebRTC VAD 只接受 10/20/30ms 帧,取 30ms(判定次数最少,精度足够)。
const VAD_FRAME_SECS: f64 = 0.03;
/// 连续非人声帧达到该时长才算可切的人声间隙:0.5s 覆盖正常句间停顿,更短的视为气口。
const VAD_MIN_GAP_SECS: f64 = 0.5;

/// VAD 探测人声间隙:ffmpeg 解码为 16kHz 16bit 单声道 PCM(VAD 只接受 8/16/32/48kHz),
/// 逐 30ms 帧判定人声,连续非人声 ≥ VAD_MIN_GAP_SECS 的区间记为可切间隙(复用 SilenceRange)。
/// Aggressive 模式把背景音乐更多地判为非人声,适合带 BGM 的短视频。
fn detect_speech_gaps(program: &str, audio: &Path) -> Result<Vec<SilenceRange>> {
    let voiced = detect_voiced_frames(program, audio)?;
    Ok(gaps_from_voiced_frames(&voiced, VAD_FRAME_SECS, VAD_MIN_GAP_SECS))
}

/// 语音门禁的最低人声量:累计人声 ≥0.3s(10 帧)才算有语音,过滤气口 / 底噪 / 纯静音段。
const MIN_SPEECH_SECS: f64 = 0.3;

/// 人声门禁(语音输入分段发 ASR 前调用):音频中是否含人声。
/// 累计人声 ≥ MIN_SPEECH_SECS 判有;解码 / 判定失败保守返回 true(宁可多送一次 ASR,不误杀)。
pub fn has_human_speech(program: &str, audio: &Path) -> bool {
    match detect_voiced_frames(program, audio) {
        Ok(frames) => voiced_duration_secs(&frames, VAD_FRAME_SECS) >= MIN_SPEECH_SECS,
        Err(e) => {
            tracing::warn!("人声门禁判定失败({e}),保守放行");
            true
        }
    }
}

/// 累计人声时长(秒):逐帧判定为 true 的帧数 × 帧长(纯函数,便于单测)。
fn voiced_duration_secs(frames: &[bool], frame_secs: f64) -> f64 {
    frames.iter().filter(|&&v| v).count() as f64 * frame_secs
}

/// ffmpeg 解码为 16kHz 16bit 单声道 PCM,逐 30ms 帧做人声判定(判定失败的帧按人声保守处理)。
fn detect_voiced_frames(program: &str, audio: &Path) -> Result<Vec<bool>> {
    let mut cmd = std::process::Command::new(program);
    hide_console_window(&mut cmd);
    let output = cmd
        .arg("-hide_banner")
        .arg("-v")
        .arg("error")
        .arg("-i")
        .arg(audio)
        .arg("-vn")
        .arg("-ac")
        .arg("1")
        .arg("-ar")
        .arg("16000")
        .arg("-f")
        .arg("s16le")
        .arg("-")
        .output()
        .map_err(|e| CrawlerError::Parse(format!("启动 ffmpeg 失败: {e}")))?;
    if !output.status.success() {
        return Err(CrawlerError::Parse(format!(
            "ffmpeg 解码 PCM 失败:{}",
            describe_ffmpeg_exit(output.status.code())
        )));
    }
    // s16le 字节流 → i16 样本(末尾不足 2 字节的尾巴丢弃)
    let pcm: Vec<i16> = output
        .stdout
        .chunks_exact(2)
        .map(|b| i16::from_le_bytes([b[0], b[1]]))
        .collect();
    let frame_len = (16000.0 * VAD_FRAME_SECS) as usize; // 480 样本
    let mut vad = Vad::new_with_rate_and_mode(SampleRate::Rate16kHz, VadMode::Aggressive);
    let mut voiced = Vec::with_capacity(pcm.len() / frame_len);
    for frame in pcm.chunks_exact(frame_len) {
        // 判定失败的帧按人声处理(保守:不拿可疑位置当切点)
        voiced.push(vad.is_voice_segment(frame).unwrap_or(true));
    }
    Ok(voiced)
}

/// 把逐帧人声判定折叠成间隙区间(纯函数,便于单测):
/// 连续非人声(false)帧数 ≥ 最小间隙帧数才记一段;区间即该段首/尾帧的时间边界。
fn gaps_from_voiced_frames(frames: &[bool], frame_secs: f64, min_gap_secs: f64) -> Vec<SilenceRange> {
    let min_frames = (min_gap_secs / frame_secs).ceil() as usize;
    let mut out = Vec::new();
    let mut run_start: Option<usize> = None;
    for (i, &v) in frames.iter().enumerate() {
        if v {
            if let Some(s) = run_start.take() {
                if i - s >= min_frames {
                    out.push(SilenceRange {
                        start: s as f64 * frame_secs,
                        end: i as f64 * frame_secs,
                    });
                }
            }
        } else if run_start.is_none() {
            run_start = Some(i);
        }
    }
    // 结尾的非人声段同样可记(切点规划只取中点,尾部间隙 midpoint 超死线自然不会被选中)
    if let Some(s) = run_start {
        if frames.len() - s >= min_frames {
            out.push(SilenceRange {
                start: s as f64 * frame_secs,
                end: frames.len() as f64 * frame_secs,
            });
        }
    }
    out
}

/// 跑 silencedetect 探测静音区间(整段解码一遍,纯音频很快)。
/// 该命令正常退出码为 0;静音信息在 stderr 的 `silence_start:` / `silence_end:` 行。
fn detect_silences(program: &str, audio: &Path) -> Result<Vec<SilenceRange>> {
    let mut cmd = std::process::Command::new(program);
    hide_console_window(&mut cmd);
    let output = cmd
        .arg("-hide_banner")
        .arg("-i")
        .arg(audio)
        .arg("-af")
        .arg(SILENCE_DETECT_FILTER)
        .arg("-f")
        .arg("null")
        .arg("-")
        .output()
        .map_err(|e| CrawlerError::Parse(format!("启动 ffmpeg 失败: {e}")))?;
    if !output.status.success() {
        return Err(CrawlerError::Parse(format!(
            "ffmpeg 静音探测失败:{}",
            describe_ffmpeg_exit(output.status.code())
        )));
    }
    Ok(parse_silences(&String::from_utf8_lossy(&output.stderr)))
}

/// 解析 silencedetect stderr 里的静音区间。行形如:
/// `[silencedetect @ ...] silence_start: 12.34` / `... silence_end: 15.67 | silence_duration: 3.33`
fn parse_silences(stderr: &str) -> Vec<SilenceRange> {
    fn number_after(line: &str, key: &str) -> Option<f64> {
        let pos = line.find(key)?;
        let num: String = line[pos + key.len()..]
            .chars()
            .take_while(|c| c.is_ascii_digit() || *c == '.' || *c == '-')
            .collect();
        num.parse().ok()
    }
    let mut out = Vec::new();
    let mut pending_start: Option<f64> = None;
    for line in stderr.lines() {
        if let Some(s) = number_after(line, "silence_start: ") {
            pending_start = Some(s);
        } else if let Some(e) = number_after(line, "silence_end: ") {
            // 成对出现才登记;孤立的 start(结尾余音未起)丢弃
            if let Some(s) = pending_start.take() {
                out.push(SilenceRange { start: s, end: e });
            }
        }
    }
    out
}

/// 读音频总时长(秒):`ffmpeg -i` 不产出文件,stderr 头部带 `Duration: HH:MM:SS.cc`。
/// 无输出文件时 ffmpeg 退出码非 0,属预期,不视为失败;解析不到返回 None 由调用方回退。
fn probe_duration(program: &str, audio: &Path) -> Option<f64> {
    let mut cmd = std::process::Command::new(program);
    hide_console_window(&mut cmd);
    let output = cmd
        .arg("-hide_banner")
        .arg("-i")
        .arg(audio)
        .output()
        .ok()?;
    parse_ffmpeg_duration(&String::from_utf8_lossy(&output.stderr))
}

/// 解析 ffmpeg stderr 里的 `Duration: 00:03:21.45` 为秒数。
fn parse_ffmpeg_duration(stderr: &str) -> Option<f64> {
    let pos = stderr.find("Duration: ")?;
    let ts: String = stderr[pos + "Duration: ".len()..]
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == ':' || *c == '.')
        .collect();
    let mut parts = ts.splitn(3, ':');
    let h: f64 = parts.next()?.parse().ok()?;
    let m: f64 = parts.next()?.parse().ok()?;
    let s: f64 = parts.next()?.parse().ok()?;
    Some(h * 3600.0 + m * 60.0 + s)
}

/// 规划切点(纯函数,便于单测):沿时间轴贪心,每段在不超过 max_secs 的前提下
/// 切在「最后一个可用静音中点」;该区间内无静音则硬切一刀兜底,保证不超上限。
/// 返回切点绝对时间(升序);总时长不超过上限时返回空(无需切)。
fn plan_silence_cuts(duration: f64, max_secs: f64, silences: &[SilenceRange]) -> Vec<f64> {
    let midpoints: Vec<f64> = silences
        .iter()
        .map(|s| (s.start + s.end) / 2.0)
        .collect();
    let mut cuts = Vec::new();
    let mut start = 0.0;
    while duration - start > max_secs {
        let deadline = start + max_secs;
        let pick = midpoints
            .iter()
            .copied()
            .filter(|&m| m > start + MIN_CHUNK_SECS && m <= deadline)
            .last();
        let cut = pick.unwrap_or(deadline);
        cuts.push(cut);
        start = cut;
    }
    cuts
}

/// 按切点逐段出片:`-ss` 前置快速定位 + `-t` 限长,`-c copy` 不重编码。
/// 切片命名与 `split_audio` 一致(chunk_0001.mp3 起),下游无需区分两种切法。
fn cut_audio_at(program: &str, audio: &Path, out_dir: &Path, cuts: &[f64]) -> Result<Vec<PathBuf>> {
    std::fs::create_dir_all(out_dir)
        .map_err(|e| CrawlerError::Parse(format!("创建切片目录失败: {e}")))?;
    let mut starts: Vec<f64> = Vec::with_capacity(cuts.len() + 1);
    starts.push(0.0);
    starts.extend_from_slice(cuts);
    let mut chunks = Vec::with_capacity(starts.len());
    for (idx, &seg_start) in starts.iter().enumerate() {
        let out = out_dir.join(format!("chunk_{:04}.mp3", idx + 1));
        let mut cmd = std::process::Command::new(program);
        hide_console_window(&mut cmd);
        cmd.arg("-y")
            .arg("-ss")
            .arg(format!("{seg_start:.3}"))
            .arg("-i")
            .arg(audio);
        // 非末段限长到下一切点;末段跑到文件尾
        if let Some(&next) = starts.get(idx + 1) {
            cmd.arg("-t").arg(format!("{:.3}", next - seg_start));
        }
        cmd.arg("-c").arg("copy").arg("-threads").arg("1").arg(&out);
        let status = cmd
            .status()
            .map_err(|e| CrawlerError::Parse(format!("启动 ffmpeg 失败: {e}")))?;
        if !status.success() {
            return Err(CrawlerError::Parse(format!(
                "ffmpeg 音频切片失败:{}",
                describe_ffmpeg_exit(status.code())
            )));
        }
        chunks.push(out);
    }
    Ok(chunks)
}

/// 探测 ffmpeg 是否可用:用 `<program> -version` 起一次进程,退出码 0 视为可用,
/// 返回版本信息首行(形如 "ffmpeg version ...")。program 解析口径与 extract_audio 一致:
/// ffmpeg_path 为空时探测系统 PATH 的 `ffmpeg`。探测失败 / 找不到可执行文件统一返回 None。
pub fn probe_ffmpeg(ffmpeg_path: Option<&str>) -> Option<String> {
    let program = ffmpeg_path
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .unwrap_or("ffmpeg");
    let mut cmd = std::process::Command::new(program);
    hide_console_window(&mut cmd);
    let output = cmd
        .arg("-version")
        .output()
        .ok()?;    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    stdout
        .lines()
        .next()
        .map(|line| line.trim().to_string())
        .filter(|line| !line.is_empty())
}

/// 随包捆绑的 ffmpeg 路径(存在时优先于系统 PATH,免用户安装)。
/// 生产:安装目录资源(tauri.conf.json 的 bundle.resources);开发:资源不打进 target,
/// 直接读源码目录 src-tauri/resources/。两侧都不存在返回 None,调用方退回系统 PATH。
pub fn bundled_ffmpeg_path(app: &tauri::AppHandle) -> Option<PathBuf> {
    use tauri::Manager;
    let rel = if cfg!(windows) {
        "resources/ffmpeg.exe"
    } else {
        "resources/ffmpeg"
    };
    if let Ok(p) = app
        .path()
        .resolve(rel, tauri::path::BaseDirectory::Resource)
    {
        if p.exists() {
            return Some(p);
        }
    }
    let dev = Path::new(env!("CARGO_MANIFEST_DIR")).join(rel);
    if dev.exists() {
        return Some(dev);
    }
    None
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
    audio_extract: bool,
    cookie: Option<&str>,
    // 取消标志:任务手动停止时置位,在飞的 ffmpeg 拉流转码会被强杀(500ms 内感知)
    cancel: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
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
                    // 同作者互斥:首个任务下载,其余等它完成后由下方新鲜检查命中、直接复用
                    let lock = avatar_lock(&format!("{}-{uid}", content.platform));
                    let _avatar_guard = lock.lock().await;
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

    // 视频:仅当任务开启「音频提取」(AI 文案提取隐含开启)才下载并转音频(只留音频);
    // 未开则视频不下载、不存储——不需要音频/文案就不留视频。
    if content.kind == ContentKind::Video && audio_extract {
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
                        process_video(content, &audio_dir, &prefix, video_url, media, cookie, cancel).await;
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
    cancel: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
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
        // 任务已手动停止:不再(重)试,直接以取消收尾
        if cancel
            .as_ref()
            .map(|c| c.load(std::sync::atomic::Ordering::Relaxed))
            .unwrap_or(false)
        {
            return VideoOutcome {
                downloaded: false,
                audio_extracted: Some(false),
                error: Some("已手动停止".into()),
                audio_path: None,
            };
        }
        // ffmpeg 全局限流:同一时刻最多 MAX_FFMPEG_CONCURRENCY 个子进程在转码。
        // 排队等待比并发打满更稳(CPU / 带宽 / CDN 并发限制),permit 随本次尝试结束释放。
        // 信号量不会关闭,Err 仅理论路径;ok() 拿到 Option 持有 permit,随本次尝试结束释放
        let _ffmpeg_permit = FFMPEG_SEMAPHORE.acquire().await.ok();
        let url_for_task = video_url.to_string();
        let audio_for_task = audio_path.clone();
        let ffmpeg_for_task = media.ffmpeg_path.clone();
        // cookie / proxy 是借用,而 spawn_blocking 闭包要求 'static,故转 owned 再 move 进去
        let cookie_for_task = cookie.map(str::to_string);
        let proxy_for_task = media.proxy.clone();
        let cancel_for_task = cancel.clone();
        let result = tokio::task::spawn_blocking(move || {
            extract_audio_from_url(
                &url_for_task,
                &audio_for_task,
                ffmpeg_for_task.as_deref(),
                referer,
                cookie_for_task.as_deref(),
                &proxy_for_task,
                cancel_for_task,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn sil(start: f64, end: f64) -> SilenceRange {
        SilenceRange { start, end }
    }

    #[test]
    fn parse_silences_pairs_start_and_end() {
        let stderr = r"
[silencedetect @ 000001] silence_start: 12.34
[silencedetect @ 000001] silence_end: 15.67 | silence_duration: 3.33
[silencedetect @ 000001] silence_start: 40
[silencedetect @ 000001] silence_end: 41.2 | silence_duration: 1.2
";
        let v = parse_silences(stderr);
        assert_eq!(v, vec![sil(12.34, 15.67), sil(40.0, 41.2)]);
    }

    #[test]
    fn parse_silences_drops_orphan_start() {
        // 结尾只有 silence_start(余音未起)不成对,丢弃
        let stderr = "silence_start: 9.5\n";
        assert!(parse_silences(stderr).is_empty());
    }

    #[test]
    fn parse_duration_hh_mm_ss() {
        let stderr = "Input #0, mp3, from 'a.mp3':\n  Duration: 01:02:03.50, start: 0.0, bitrate: 96 kb/s\n";
        let d = parse_ffmpeg_duration(stderr).unwrap();
        assert!((d - 3723.5).abs() < 1e-6);
    }

    #[test]
    fn parse_duration_missing() {
        assert_eq!(parse_ffmpeg_duration("no duration here"), None);
    }

    #[test]
    fn plan_cuts_prefers_latest_silence_before_deadline() {
        // 上限 25s:第一刀切在 24s 的静音中点(而非 10s 或硬切 25s);
        // 第二段从 24s 起,48.8s 的静音中点 ≤ 49s 死线 → 切 48.8s
        let cuts = plan_silence_cuts(60.0, 25.0, &[sil(9.5, 10.5), sil(23.5, 24.5), sil(48.6, 49.0)]);
        assert_eq!(cuts, vec![24.0, 48.8]);
    }

    #[test]
    fn plan_cuts_hard_cut_when_no_silence_in_window() {
        // 0~25s 内无静音 → 25s 硬切;26.5s 的静音可用 → 切 26.5s;之后再无静音 → 51.5s 硬切
        let cuts = plan_silence_cuts(55.0, 25.0, &[sil(26.2, 26.8)]);
        assert_eq!(cuts, vec![25.0, 26.5, 51.5]);
    }

    #[test]
    fn plan_cuts_skips_silence_too_close_to_start() {
        // 起点 0.5s 处的静音不满足最小段长,硬切在 25s
        let cuts = plan_silence_cuts(40.0, 25.0, &[sil(0.4, 0.6)]);
        assert_eq!(cuts, vec![25.0]);
    }

    #[test]
    fn plan_cuts_empty_when_within_limit() {
        assert!(plan_silence_cuts(20.0, 25.0, &[sil(9.0, 10.0)]).is_empty());
    }

    // 30ms/帧、最小间隙 0.5s → 连续 ≥17 帧非人声才算间隙
    fn voiced_frames(spec: &[(usize, bool)]) -> Vec<bool> {
        let len = spec.iter().map(|(n, _)| n).sum();
        let mut v = Vec::with_capacity(len);
        for (n, voiced) in spec {
            v.extend(std::iter::repeat_n(*voiced, *n));
        }
        v
    }

    #[test]
    fn vad_gaps_detects_long_gap() {
        // 10 帧人声 + 20 帧(0.6s)间隙 + 10 帧人声 → 间隙 (0.30, 0.90)
        let frames = voiced_frames(&[(10, true), (20, false), (10, true)]);
        let gaps = gaps_from_voiced_frames(&frames, 0.03, 0.5);
        assert_eq!(gaps.len(), 1);
        assert!((gaps[0].start - 0.30).abs() < 1e-6);
        assert!((gaps[0].end - 0.90).abs() < 1e-6);
    }

    #[test]
    fn vad_gaps_ignores_short_gap() {
        // 16 帧(0.48s)不足 0.5s,不记
        let frames = voiced_frames(&[(10, true), (16, false), (10, true)]);
        assert!(gaps_from_voiced_frames(&frames, 0.03, 0.5).is_empty());
    }

    #[test]
    fn vad_gaps_all_voiced() {
        let frames = voiced_frames(&[(100, true)]);
        assert!(gaps_from_voiced_frames(&frames, 0.03, 0.5).is_empty());
    }

    #[test]
    fn vad_gaps_trailing_run_counted() {
        // 结尾 20 帧非人声也记为间隙(0.60 ~ 1.20)
        let frames = voiced_frames(&[(20, true), (20, false)]);
        let gaps = gaps_from_voiced_frames(&frames, 0.03, 0.5);
        assert_eq!(gaps.len(), 1);
        assert!((gaps[0].start - 0.60).abs() < 1e-6);
        assert!((gaps[0].end - 1.20).abs() < 1e-6);
    }

    #[test]
    fn voiced_duration_counts_only_voiced_frames() {
        // 10 帧人声(0.3s)+ 90 帧非人声 → 0.3s,恰好达到门禁下限
        let frames = voiced_frames(&[(10, true), (90, false)]);
        assert!((voiced_duration_secs(&frames, 0.03) - 0.3).abs() < 1e-6);
        // 9 帧人声(0.27s)< MIN_SPEECH_SECS:应被门禁拦下
        let frames = voiced_frames(&[(9, true), (91, false)]);
        assert!(voiced_duration_secs(&frames, 0.03) < MIN_SPEECH_SECS);
    }
}
