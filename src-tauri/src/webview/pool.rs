//! 可见 WebView 池 + 高层采集桥接 `CollectBridge`。
//!
//! `WebviewPool` 维护「(平台, 账号) -> 可见窗口」映射。设计要点:
//! - **单窗口既登录又采集**:同一账号用同一窗口,登录态天然延续到采集;
//! - **多账号隔离**:每账号一个独立 `data_directory`(WebView2 用户数据目录),
//!   使不同账号的 Cookie / localStorage 互不覆盖,实现「同平台多账号」并存;
//! - **窗口可见**:用户能看到 RPA 操作过程,必要时手动过验证码。
//!
//! `CollectBridge` 在池之上对外暴露「关键词 → 拦截到的接口响应集合」的统一采集调用。

use veltrix_core::config::{CollectConfig, PlatformConfig, RpaStep};
use veltrix_core::error::{CrawlerError, Result};
use crate::adapter::{FetchContext, PlatformAdapter};
use crate::model::TaskKind;
use crate::webview::native_intercept::{self, ResponseSink};
use crate::webview::{
    build_detail_eval, build_human_rpa_script, build_hud_init_script, build_hud_keyword_eval,
    build_hud_log_eval, build_hud_session_eval, build_hud_status_eval, build_intercept_init_script,
    build_scroll_eval, build_search_eval, build_select_eval, build_set_session_eval,
    emit_collect_log, CollectControl, InterceptChannel, InterceptedResponse, RpaChannel,
};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindow, WebviewWindowBuilder};

/// 「接口有响应却无新增」连续 N 轮 → 起逐轮预警(疑似到底/风控,可手动验证)。
const STAGNANT_LIMIT: u32 = 1;
/// 「已采到内容后连续无新增」N 轮 → 判定已到底,结束本次采集(保留已采内容)。
/// 取 3:到底 / 全是重复场景快速收尾不空等,同时容忍一点网络抖动。
const STAGNANT_STOP: u32 = 3;
/// 评论专用的「无新增即到底」轮数,比内容采集更激进(取 2):评论分页轻、到底信号明确,
/// 连 2 轮无新增即收尾去采下一个,避免在单条内容评论区空滚浪费时间。
const COMMENT_STAGNANT_STOP: u32 = 2;
/// 「接口连响应都没有」(网络慢/请求未返回)连续 N 轮 → 兜底结束。与 STAGNANT_STOP 分开:
/// 慢网络请求往返久,容忍更大轮数,避免网络抖动被误判成「没数据」而提前结束。
const NO_RESPONSE_STOP: u32 = 8;
/// 检测到安全验证弹窗后,等待用户手动完成的最长时长:超时仍未完成则结束本次采集(已采数据已保留)。
const VERIFY_WAIT_MAX: Duration = Duration::from_secs(180);
/// 验证弹窗等待期间的轮询间隔。
const VERIFY_POLL: Duration = Duration::from_secs(2);
/// 每次滚动后的拟人停顿区间(毫秒):2~6 秒随机,避免匀速快速滚动触发风控。
/// 下限取 2s:信息流接口往返常需 ~2s,停顿过短会在数据返回前就读快照,
/// 表现为「新增 0 条」空转,并连带误报风控。
const SCROLL_PAUSE_MIN_MS: u64 = 2000;
const SCROLL_PAUSE_SPAN_MS: u64 = 4000;
/// 评论区滚动停顿区间:1~2 秒随机,比内容滚动(2~6s)更短(评论分页更轻、需更快翻完)。
const COMMENT_PAUSE_MIN_MS: u64 = 1000;
const COMMENT_PAUSE_SPAN_MS: u64 = 1000;

/// 生成 2~6 秒的随机滚动停顿。无 rand 依赖,用系统时间纳秒做廉价熵源,拟人足够。
fn random_scroll_pause() -> Duration {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as u64)
        .unwrap_or(0);
    Duration::from_millis(SCROLL_PAUSE_MIN_MS + nanos % SCROLL_PAUSE_SPAN_MS)
}

/// 生成 1~2 秒的随机评论区滚动停顿(比内容滚动更短)。
fn random_comment_scroll_pause() -> Duration {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as u64)
        .unwrap_or(0);
    Duration::from_millis(COMMENT_PAUSE_MIN_MS + nanos % COMMENT_PAUSE_SPAN_MS)
}

/// 排序方式 → 结果页排序按钮文案候选(多候选覆盖平台差异)。synthetic(综合)默认不点。
fn sort_labels(sort_mode: &str) -> Vec<String> {
    match sort_mode {
        "hottest" => vec!["最热".into(), "最多点赞".into(), "最多收藏".into()],
        "latest" => vec!["最新".into(), "最新发布".into()],
        _ => Vec::new(),
    }
}

/// 发布时间范围 → 结果页时间筛选文案候选。any(不限)默认不点。
fn time_labels(time_range: &str) -> Vec<String> {
    match time_range {
        "1d" => vec!["一天内".into(), "近一天".into(), "24小时".into()],
        "1w" => vec!["一周内".into(), "近一周".into(), "最近一周".into()],
        "6m" => vec!["半年内".into(), "近半年".into(), "最近半年".into()],
        _ => Vec::new(),
    }
}

/// 在结果页按任务排序/时间做 RPA 文案点击(综合/不限默认不点)。点击后留停顿等结果刷新。
async fn apply_sort_time(window: &WebviewWindow, sort_mode: &str, time_range: &str) {
    let sort_lbls = sort_labels(sort_mode);
    if !sort_lbls.is_empty() {
        let _ = window.eval(&build_select_eval(&sort_lbls));
        let _ = window.eval(&build_hud_log_eval("info", &format!("应用排序:{sort_mode}")));
        tokio::time::sleep(Duration::from_millis(1800)).await;
    }
    let time_lbls = time_labels(time_range);
    if !time_lbls.is_empty() {
        let _ = window.eval(&build_select_eval(&time_lbls));
        let _ = window.eval(&build_hud_log_eval(
            "info",
            &format!("应用时间筛选:{time_range}"),
        ));
        tokio::time::sleep(Duration::from_millis(1800)).await;
    }
}

/// 按平台配置 + 任务 sort_mode/time_range 拼搜索 URL 追加的 query(如 sort_type=1&publish_time=7)。
/// 未配参数名或映射缺失则该项不追加(综合/不限默认无参数)。
fn build_search_query(collect: &CollectConfig, sort_mode: &str, time_range: &str) -> String {
    let mut parts: Vec<String> = Vec::new();
    if !collect.sort_query_key.is_empty() {
        if let Some(v) = collect.sort_query_map.get(sort_mode) {
            if !v.is_empty() {
                parts.push(format!("{}={}", collect.sort_query_key, v));
            }
        }
    }
    if !collect.time_query_key.is_empty() {
        if let Some(v) = collect.time_query_map.get(time_range) {
            if !v.is_empty() {
                parts.push(format!("{}={}", collect.time_query_key, v));
            }
        }
    }
    parts.join("&")
}

/// 导航到搜索页后、注入 session 前的等待(毫秒)。
/// 给页面完成导航 + 挂载 hook 留时间;此前命中的首屏请求由页内缓冲兜底,不会漏抓。
const NAV_SETTLE_MS: u64 = 2500;

/// 拟人 RPA 单次运行的最长等待(毫秒)。拟人节奏慢(逐字输入 + 持续滚到底 + 停顿),
/// 给足上限(滚到底可能数十轮);超时仅告警并取已拦截部分,不硬失败。
const RPA_RUN_TIMEOUT_MS: u64 = 180_000;

/// RPA 跑完注入 session 回放页内缓冲后的收尾等待(毫秒),让回放的 `intercept_push` 到齐再取走。
const REPLAY_SETTLE_MS: u64 = 1500;

/// WebView 窗口标签拼装规则。统一前缀便于在 Tauri 端区分管理类窗口。
fn window_label(platform: &str, account_id: &str) -> String {
    format!("veltrix-{platform}-{account_id}")
}

/// 拦截到的响应里是否有 URL 命中验证特征(响应侧风控检测)。patterns 空时恒 false。
fn response_hits_verify(
    responses: &[crate::webview::InterceptedResponse],
    patterns: &[String],
) -> bool {
    if patterns.is_empty() {
        return false;
    }
    responses.iter().any(|r| {
        let url = r.url.to_lowercase();
        patterns
            .iter()
            .any(|p| !p.is_empty() && url.contains(&p.to_lowercase()))
    })
}

/// 把 label 规整为文件系统安全的目录名。
/// account_id 可能由用户自定义、含 `/` `:` 空格等非法路径字符,
/// 直接拼路径会破坏多账号隔离(建目录失败或落到意外位置),故统一替换为下划线。
fn sanitize_dir_name(label: &str) -> String {
    label
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// 由账号 label 派生稳定的 16 字节 WKWebView 数据存储标识(macOS 账号隔离用)。
/// 同一账号每次启动得到相同 id → 登录态延续;不同账号 id 不同 → Cookie / 存储互不串。
/// 用 FNV-1a 双种子拼成 128 位、并写入 UUIDv4 的版本/变体位,确保是 WKWebsiteDataStore
/// 接受的合法 UUID(非全零)。无外部依赖、跨进程稳定。
#[cfg(target_os = "macos")]
fn account_store_id(label: &str) -> [u8; 16] {
    const FNV_PRIME: u64 = 0x0000_0100_0000_01B3;
    fn fnv1a(seed: u64, bytes: &[u8]) -> u64 {
        let mut hash = seed;
        for &b in bytes {
            hash ^= b as u64;
            hash = hash.wrapping_mul(FNV_PRIME);
        }
        hash
    }
    let hi = fnv1a(0xcbf2_9ce4_8422_2325, label.as_bytes());
    let lo = fnv1a(0x8422_2325_cbf2_9ce4, label.as_bytes());
    let mut id = [0u8; 16];
    id[..8].copy_from_slice(&hi.to_be_bytes());
    id[8..].copy_from_slice(&lo.to_be_bytes());
    // 写入 UUID 版本(4)与变体位,符合 WKWebsiteDataStore 对 identifier 的格式要求
    id[6] = (id[6] & 0x0F) | 0x40;
    id[8] = (id[8] & 0x3F) | 0x80;
    id
}

/// macOS:清空某账号 data_store_identifier 对应 WKWebsiteDataStore 的全部网站数据
/// (Cookie / localStorage 等登录凭据),等价于 Windows 删账号独立数据目录。
/// WebKit 删除是异步的(完成回调在主队列触发),此处 fire-and-forget——调用方随后重开
/// 登录窗口时已是干净态;WKWebsiteDataStore 的类方法需在主线程访问,故经 run_on_main_thread 派发。
#[cfg(target_os = "macos")]
fn wipe_account_data_store(app: &AppHandle, store_id: [u8; 16]) -> Result<()> {
    use objc2::MainThreadMarker;
    use objc2_foundation::{NSDate, NSUUID};
    use objc2_web_kit::WKWebsiteDataStore;

    app.run_on_main_thread(move || {
        let Some(mtm) = MainThreadMarker::new() else {
            tracing::warn!("非主线程,mac 清登录态未执行");
            return;
        };
        // SAFETY: 在主线程上访问 WebKit 数据存储 COM 接口
        unsafe {
            let uuid = NSUUID::from_bytes(store_id);
            let store = WKWebsiteDataStore::dataStoreForIdentifier(&uuid, mtm);
            let types = WKWebsiteDataStore::allWebsiteDataTypes(mtm);
            // distantPast = 删除该日期之后修改的全部数据 = 清空整库
            let since = NSDate::distantPast();
            // 完成回调:删完即触发,无后续动作
            let handler = block2::RcBlock::new(|| {});
            store.removeDataOfTypes_modifiedSince_completionHandler(&types, &since, &handler);
        }
    })
    .map_err(|e| CrawlerError::Config(format!("派发主线程清登录态失败: {e}")))?;
    Ok(())
}

/// 把已存在的窗口显示并置于前台。复用窗口可能处于隐藏 / 后台,采集 / 登录时需弹到前台。
/// show / set_focus 失败不致命,仅告警不阻断流程。
fn bring_to_front(window: &WebviewWindow) {
    if let Err(e) = window.show() {
        tracing::warn!("显示复用窗口失败: {e}");
    }
    if let Err(e) = window.maximize() {
        tracing::warn!("最大化复用窗口失败: {e}");
    }
    if let Err(e) = window.set_focus() {
        tracing::warn!("聚焦复用窗口失败: {e}");
    }
}

/// Windows 真实滚轮:给 WebView2 渲染子窗口投递 WM_MOUSEWHEEL,触发页面懒加载。
/// 程序 `scrollBy` 对监听真实滚轮的页面(如小红书)无效,故走窗口消息级真实滚轮。
#[cfg(windows)]
mod win_wheel {
    use veltrix_core::error::{CrawlerError, Result};
    use windows::core::BOOL;
    use windows::Win32::Foundation::{HWND, LPARAM, RECT, WPARAM};
    use windows::Win32::UI::WindowsAndMessaging::{
        EnumChildWindows, GetClassNameW, GetWindowRect, PostMessageW, WM_MOUSEWHEEL,
    };

    /// 滚轮每档的标准位移量(WHEEL_DELTA)。
    const WHEEL_DELTA: i32 = 120;

    /// EnumChildWindows 回调:找到 WebView2 的渲染子窗口(承接输入消息的那个)即停止。
    unsafe extern "system" fn find_render_widget(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let mut buf = [0u16; 128];
        let len = GetClassNameW(hwnd, &mut buf);
        let class = String::from_utf16_lossy(&buf[..len as usize]);
        if class == "Chrome_RenderWidgetHostHWND" {
            // lparam 指向调用方的 Option<HWND>,写回找到的句柄
            let out = &mut *(lparam.0 as *mut Option<HWND>);
            *out = Some(hwnd);
            return BOOL(0); // 停止枚举
        }
        BOOL(1) // 继续枚举
    }

    /// 给 `parent`(Tauri 窗口)下的 WebView2 渲染子窗口投递一次滚轮。notches 负值下滚。
    pub fn real_wheel(parent: HWND, notches: i32) -> Result<()> {
        let mut target: Option<HWND> = None;
        unsafe {
            let _ = EnumChildWindows(
                Some(parent),
                Some(find_render_widget),
                LPARAM(&mut target as *mut _ as isize),
            );
        }
        // 找不到渲染子窗口时退回父窗口
        let hwnd = target.unwrap_or(parent);

        // WM_MOUSEWHEEL 的 lParam 用屏幕坐标的鼠标位置,取目标窗口中心
        let mut rect = RECT::default();
        unsafe {
            GetWindowRect(hwnd, &mut rect)
                .map_err(|e| CrawlerError::Config(format!("GetWindowRect 失败: {e}")))?;
        }
        let x = ((rect.left + rect.right) / 2) & 0xFFFF;
        let y = (rect.top + rect.bottom) / 2;
        let lparam = LPARAM(((y << 16) | x) as isize);

        // wParam 高 16 位 = 有符号滚轮位移(负=下滚),低 16 位 = 按键状态(0)
        let delta = (notches * WHEEL_DELTA) as i16;
        let wparam = WPARAM((((delta as i32) << 16) as u32) as usize);
        unsafe {
            PostMessageW(Some(hwnd), WM_MOUSEWHEEL, wparam, lparam)
                .map_err(|e| CrawlerError::Config(format!("PostMessage 滚轮失败: {e}")))?;
        }
        Ok(())
    }
}

/// 创建 / 复用窗口所需的描述。集中成结构体以遵守「参数 ≤ 4」。
struct WindowSpec<'a> {
    platform: &'a str,
    account_id: &'a str,
    /// 首次创建时加载的初始页(通常是登录页);窗口已存在时忽略。
    initial_url: &'a str,
    /// 该平台需拦截的接口 URL 特征,编译进早期注入脚本。
    patterns: &'a [String],
    /// 是否注入采集 HUD 浮层。登录 / 访问平台窗口不是采集,应为 false。
    with_hud: bool,
    /// 窗口标题:平台 - 账号名称。
    title: &'a str,
    /// 登录态自检脚本(登录窗口注入,周期性回传登录态);None=不检测。
    login_check_script: Option<String>,
}

/// 可见 WebView 池。`tauri::WebviewWindow` 是 `Clone` 句柄,可安全跨任务持有。
#[derive(Default)]
pub struct WebviewPool {
    /// label -> WebviewWindow。串行化操作避免并发创建同 label 窗口。
    windows: Mutex<HashMap<String, WebviewWindow>>,
    /// label -> 原生网络拦截缓冲。每窗口装一次拦截器,采集时清空/取走。
    sinks: Mutex<HashMap<String, ResponseSink>>,
}

impl WebviewPool {
    pub fn new() -> Self {
        Self::default()
    }

    /// 确保指定账号的可见 WebView 存在;已存在则复用(保留登录态)。
    fn ensure_window(&self, app: &AppHandle, spec: &WindowSpec<'_>) -> Result<WebviewWindow> {
        let label = window_label(spec.platform, spec.account_id);

        // 复用只信 Tauri 权威句柄:窗口真实存活才返回 Some,二次进入不重建、登录态延续。
        // 不能用本地 windows 缓存复用——窗口被关闭后缓存句柄已失效,会导致
        // 「采集窗口关了之后再采就打不开」。复用时弹到前台,避免藏在后台像没触发。
        if let Some(existing) = app.get_webview_window(&label) {
            self.remember(&label, existing.clone())?;
            bring_to_front(&existing);
            self.ensure_intercept(&label, &existing, spec.patterns);
            return Ok(existing);
        }
        // 走到这:窗口从未创建,或上次采集后已被关闭。清掉可能残留的失效句柄与拦截缓冲,
        // 确保下面「每次采集都重新打开 WebView2」并重新安装原生拦截器
        // (否则 sinks 残留旧 entry 会让新窗口漏装拦截 = 采不到数据)。
        self.forget(&label);

        let parsed: tauri::Url = spec
            .initial_url
            .parse()
            .map_err(|e| CrawlerError::Config(format!("非法 URL {}: {e}", spec.initial_url)))?;
        // 协议白名单:平台配置可被用户编辑,javascript:/data: 等协议会在窗口内执行任意脚本
        if !matches!(parsed.scheme(), "http" | "https") {
            return Err(CrawlerError::Config(format!(
                "非法 URL 协议「{}」:仅允许 http/https",
                parsed.scheme()
            )));
        }

        // 每账号独立用户数据目录,隔离 Cookie / 登录态(WebView2 在 Windows 生效)
        let data_dir = self.account_data_dir(app, &label)?;
        tracing::info!(
            label = %label,
            data_dir = %data_dir.display(),
            "创建账号隔离 WebView(独立用户数据目录)"
        );

        let mut builder = WebviewWindowBuilder::new(app, &label, WebviewUrl::External(parsed))
            .title(spec.title)
            .visible(true)
            .maximized(true)
            // 早期注入拦截 hook,命中平台接口特征的响应回传 Rust
            .initialization_script(build_intercept_init_script(spec.patterns));
        // 账号隔离:Windows / Linux 用独立用户数据目录;macOS 的 WKWebView 不认 data_directory,
        // 改用 data_store_identifier(macOS≥14)按账号派生独立数据存储,并额外注入原生拦截脚本
        // (经 WKScriptMessageHandler 回传,等价于 Windows 的 WebResourceResponseReceived)。
        #[cfg(not(target_os = "macos"))]
        {
            builder = builder.data_directory(data_dir);
        }
        #[cfg(target_os = "macos")]
        {
            let _ = &data_dir; // mac 不用目录隔离,但仍保留路径计算以兼容 clear_login_data
            builder = builder
                .data_store_identifier(account_store_id(&label))
                .initialization_script(crate::webview::build_native_intercept_init_script_mac(
                    spec.patterns,
                ));
        }
        // 采集 HUD 浮层只有采集窗口需要;登录 / 访问平台窗口不注入,避免出现采集日志
        if spec.with_hud {
            builder = builder.initialization_script(build_hud_init_script());
        }
        // 登录态自检脚本:登录窗口注入,每次导航都重挂,持续判断真实登录态
        if let Some(script) = &spec.login_check_script {
            builder = builder.initialization_script(script.clone());
        }
        let window = builder
            .build()
            .map_err(|e| CrawlerError::Config(format!("创建 WebView 失败: {e}")))?;

        self.remember(&label, window.clone())?;
        self.ensure_intercept(&label, &window, spec.patterns);
        Ok(window)
    }

    /// 清掉某 label 的缓存窗口句柄与拦截缓冲。窗口已关闭时调用,确保下次彻底重建。
    fn forget(&self, label: &str) {
        if let Ok(mut map) = self.windows.lock() {
            map.remove(label);
        }
        if let Ok(mut map) = self.sinks.lock() {
            map.remove(label);
        }
    }

    /// 给窗口确保装了原生响应拦截器(同 label 只装一次),返回其缓冲。
    fn ensure_intercept(
        &self,
        label: &str,
        window: &WebviewWindow,
        patterns: &[String],
    ) -> ResponseSink {
        if let Ok(map) = self.sinks.lock() {
            if let Some(sink) = map.get(label) {
                return sink.clone();
            }
        }
        let sink: ResponseSink = Arc::new(Mutex::new(Vec::new()));
        native_intercept::install(window, Arc::new(patterns.to_vec()), sink.clone());
        if let Ok(mut map) = self.sinks.lock() {
            map.insert(label.to_string(), sink.clone());
        }
        sink
    }

    /// 取某窗口的原生拦截缓冲(collect 用于清空/取走本轮命中响应)。
    fn window_sink(&self, label: &str) -> Option<ResponseSink> {
        self.sinks.lock().ok().and_then(|m| m.get(label).cloned())
    }

    /// 解析某账号的 WebView 独立数据目录并确保存在。
    fn account_data_dir(&self, app: &AppHandle, label: &str) -> Result<PathBuf> {
        let dir = self.account_data_dir_path(app, label)?;
        std::fs::create_dir_all(&dir)?;
        Ok(dir)
    }

    /// 仅计算某账号 WebView 数据目录路径,不创建(清空登录态时用,避免删前又建)。
    fn account_data_dir_path(&self, app: &AppHandle, label: &str) -> Result<PathBuf> {
        let base = app
            .path()
            .app_config_dir()
            .map_err(|e| CrawlerError::Config(format!("获取应用数据目录失败: {e}")))?;
        Ok(base
            .join("veltrix-crawler")
            .join("webview-data")
            .join(sanitize_dir_name(label)))
    }

    /// 清空某账号的登录态(Cookie / localStorage 等登录凭据),清后下次登录从干净状态开始。
    /// 先关窗释放 WebView 对存储的占用,再按平台清除:
    /// - Windows / Linux:删该账号独立数据目录(句柄释放有延迟,做短重试);
    /// - macOS:WKWebView 的登录态在 data_store_identifier 对应的 WKWebsiteDataStore,
    ///   不在目录里,故清该 store 的全部网站数据。
    pub async fn clear_login_data(
        &self,
        app: &AppHandle,
        platform: &str,
        account_id: &str,
    ) -> Result<()> {
        let label = window_label(platform, account_id);
        // 先关窗并清掉缓存句柄 / 拦截缓冲,释放对存储的占用
        if let Ok(mut map) = self.windows.lock() {
            if let Some(win) = map.remove(&label) {
                let _ = win.close();
            }
        }
        self.forget(&label);

        // macOS:登录态在 WKWebsiteDataStore(data_store_identifier),不在数据目录
        #[cfg(target_os = "macos")]
        {
            return wipe_account_data_store(app, account_store_id(&label));
        }
        // Windows / Linux:删账号独立数据目录(WebView2 句柄释放有延迟,最多重试 5 次)
        #[cfg(not(target_os = "macos"))]
        {
            let dir = self.account_data_dir_path(app, &label)?;
            if !dir.exists() {
                return Ok(());
            }
            for attempt in 0..5 {
                match std::fs::remove_dir_all(&dir) {
                    Ok(()) => return Ok(()),
                    Err(_) if attempt < 4 => {
                        tokio::time::sleep(Duration::from_millis(300)).await;
                    }
                    Err(e) => {
                        return Err(CrawlerError::Config(format!(
                            "清空登录数据失败(目录可能仍被占用,请关闭该账号窗口后重试): {e}"
                        )));
                    }
                }
            }
            Ok(())
        }
    }

    fn remember(&self, label: &str, window: WebviewWindow) -> Result<()> {
        self.windows
            .lock()
            .map_err(|_| CrawlerError::Config("WebView 池锁异常".into()))?
            .insert(label.to_string(), window);
        Ok(())
    }

    /// 打开某账号的登录窗口:创建可见窗口并加载登录页,用户在其中扫码 / 输入。
    /// 登录态写入该账号独立数据目录,后续采集复用同窗口即带登录态。
    pub fn open_login(
        &self,
        app: &AppHandle,
        platform: &str,
        account_id: &str,
        account_label: &str,
        cfg: &PlatformConfig,
    ) -> Result<WebviewWindow> {
        let title = format!("{} - {}", cfg.name, account_label);
        // 配置了登录检测信号时注入自检脚本,真实判断登录态;否则沿用乐观行为
        let login_check_script = if cfg.login_check.is_enabled() {
            Some(crate::webview::build_login_check_script(
                account_id,
                &cfg.login_check.logged_in_selectors,
                &cfg.login_check.logged_out_texts,
                &cfg.login_check.login_cookie_names,
            ))
        } else {
            None
        };
        let spec = WindowSpec {
            platform,
            account_id,
            initial_url: &cfg.login_url,
            patterns: &cfg.collect.intercept_patterns,
            // 登录 / 访问平台不是采集,不注入采集 HUD 浮层
            with_hud: false,
            title: &title,
            login_check_script: login_check_script.clone(),
        };
        tracing::info!(
            platform,
            account_id,
            login_url = %cfg.login_url,
            "打开账号窗口"
        );
        let window = self.ensure_window(app, &spec)?;
        window
            .show()
            .map_err(|e| CrawlerError::Config(format!("显示窗口失败: {e}")))?;
        // 复用已存在窗口时 initialization_script 不会重挂,这里对当前页面补一次 eval,
        // 确保「再次点登录复用旧窗口」也能立即开始自检
        if let Some(script) = &login_check_script {
            let _ = window.eval(script);
        }
        Ok(window)
    }

    /// 关闭并移除某窗口(账号被删除时调用,避免句柄泄漏)。
    pub fn drop_window(&self, platform: &str, account_id: &str) -> Result<()> {
        let label = window_label(platform, account_id);
        if let Ok(mut map) = self.windows.lock() {
            if let Some(win) = map.remove(&label) {
                let _ = win.close();
            }
        }
        Ok(())
    }
}

/// 高层采集桥接:绑定 WebView 池与拦截通道,屏蔽底层细节,供命令层直接调用。
#[derive(Clone)]
pub struct CollectBridge {
    pool: Arc<WebviewPool>,
    channel: Arc<InterceptChannel>,
    /// 拟人 RPA 运行结果通道;旧的「改 URL + 盲滚」路径不用,仅节点级 RPA 用。
    rpa: Arc<RpaChannel>,
    /// 采集中断控制:HUD「结束」按钮触发后,采集循环据此优雅停止。
    control: Arc<CollectControl>,
}

/// 一次 `collect` 的结果。无论成败都带回出错前已拦截的响应,使调用方在失败路径也能
/// 兜底解析落库 + 落媒体,不丢已采数据;`error` 为 `Some` 表示采集中途异常。
pub struct CollectOutcome {
    pub responses: Vec<InterceptedResponse>,
    pub error: Option<CrawlerError>,
}

impl CollectOutcome {
    /// 采集尚未拦截到任何响应即失败(如开窗 / 开会话失败):空响应 + 错误。
    fn failed(e: CrawlerError) -> Self {
        Self {
            responses: Vec::new(),
            error: Some(e),
        }
    }
}

/// 一次采集调用的参数。集中成结构体以遵守「参数 ≤ 4」。
pub struct CollectRequest<'a> {
    pub account_id: &'a str,
    pub keyword: &'a str,
    /// 平台配置:提供登录页、搜索 URL 模板、拦截特征与滚动参数。
    pub platform_cfg: &'a PlatformConfig,
    /// 所属任务 id;Some 时采集日志经事件推给前端面板,None(联调单采)只走窗口 HUD。
    pub task_id: Option<&'a str>,
    /// 目标采集数量(per_keyword_limit)。仅作为「停止滚动」依据:边滚边按去重数量统计,
    /// 达标或到底即停,不再盲目滚固定轮数。落库不截断——多出的、不重复的也照常存。
    /// 0(或无适配器)时退回配置的固定 `scroll_rounds`。
    pub target_count: usize,
    /// 平台适配器:滚动循环用它边滚边解析、按去重 content_id 计数判断何时停。
    pub adapter: Option<Arc<dyn PlatformAdapter>>,
    /// 该任务数据库中已存在的 content_id 集合(运行开始时的快照)。
    /// 智能停止只对「不在此集合」的新内容计入 target_count;库里已有的内容仍照常增量落库
    /// (更新点赞/评论等统计),但不占「新增」配额——避免重跑/持续监听时旧内容塞满目标数。
    pub existing_ids: Option<&'a std::collections::HashSet<String>>,
    /// 每轮新增内容的出口。为何用 channel 而非等采集结束再返回:滚动循环要持续到
    /// 达标/风控/到底,在循环内同步等待落库会拖慢滚动节奏并阻塞拟人停顿;改用
    /// UnboundedSender 把「本轮新增 Content」即时发给消费端边收边落库,实现增量入库。
    /// `None` 时退回原行为(不发增量,仅最终一次性返回拦截响应给调用方解析)。
    pub content_tx: Option<tokio::sync::mpsc::UnboundedSender<Vec<crate::model::Content>>>,
    /// 排序方式(任务 sort_mode:synthetic/hottest/latest),结果页 RPA 文案点击用。
    pub sort_mode: &'a str,
    /// 发布时间范围(任务 time_range:any/1d/1w/6m),结果页 RPA 文案点击用。
    pub time_range: &'a str,
    /// 最低点赞数:点赞数低于此值的内容不计入目标数、不增量发出、不落库(0=不限)。
    /// like_count 为 None(平台未返回点赞数)时放行,避免误删有效内容。
    pub min_likes: i32,
}

/// 一次「单视频评论采集」调用的参数。集中成结构体以遵守「参数 ≤ 4」。
pub struct CommentCollectRequest<'a> {
    pub account_id: &'a str,
    /// 目标内容 ID(抖音 aweme_id / 小红书 note_id),用于导航到详情页。
    pub content_id: &'a str,
    /// 内容标题(日志展示用,已截断),替代裸 content_id 让日志更可读。
    pub title: &'a str,
    /// 详情页鉴权 token(小红书 xsec_token;抖音留空)。
    pub xsec_token: &'a str,
    /// 平台配置:提供详情页 URL 模板、拦截特征与滚动参数。
    pub platform_cfg: &'a PlatformConfig,
    /// 所属任务 id;Some 时采集日志经事件推给前端面板。
    pub task_id: Option<&'a str>,
    /// 单视频一级评论上限:>0 时作为滚动停止依据(去重数达标即停),0 不限(滚到底)。
    pub limit: usize,
    /// 平台适配器:边滚边解析评论、按去重 comment_id 计数判断何时停。
    pub adapter: Arc<dyn PlatformAdapter>,
    /// 该内容所属采集关键词:评论日志归到对应关键词 HUD tab(评论不单独成 tab)。
    pub keyword: &'a str,
}

/// 一次「作者主页画像补采」调用的参数。集中成结构体以遵守「参数 ≤ 4」。
pub struct ProfileCollectRequest<'a> {
    pub account_id: &'a str,
    /// 作者 UID:导航到主页用,也作为解析时画像的归属 id(经 ctx.keyword 传入适配器)。
    pub uid: &'a str,
    /// 作者昵称(日志展示用),替代裸 uid 让日志更可读。
    pub nickname: &'a str,
    /// 主页鉴权 token(小红书 xsec_token;其余平台留空)。
    pub xsec_token: &'a str,
    /// 平台配置:提供主页 URL 模板与拦截特征。
    pub platform_cfg: &'a PlatformConfig,
    /// 所属任务 id;Some 时采集日志经事件推给前端面板(画像补采暂不带 task)。
    pub task_id: Option<&'a str>,
    /// 平台适配器:把主页拦截到的画像接口响应解析为 Author。
    pub adapter: Arc<dyn PlatformAdapter>,
}

impl CollectBridge {
    pub fn new(
        pool: Arc<WebviewPool>,
        channel: Arc<InterceptChannel>,
        rpa: Arc<RpaChannel>,
        control: Arc<CollectControl>,
    ) -> Self {
        Self {
            pool,
            channel,
            rpa,
            control,
        }
    }

    /// 用关键词在某账号的 WebView 内执行一次 RPA 采集,返回拦截到的接口响应集合。
    ///
    /// 流程:复用登录态窗口 → 导航到搜索结果页 → 注入会话 ID 并回放首屏缓冲 →
    /// 循环滚动触发分页接口 → 取走本次会话拦截到的全部响应。
    ///
    /// 返回 `CollectOutcome`:即便中途出错(窗口被关 / 导航失败等),`responses` 仍带回出错前
    /// 已拦截的部分响应,`error` 标识是否异常。调用方据此「失败也兜底解析落库 + 落媒体」,不丢已采数据。
    pub async fn collect(&self, app: &AppHandle, req: CollectRequest<'_>) -> CollectOutcome {
        let cfg = req.platform_cfg;
        let title = format!("{} - {}", cfg.name, req.account_id);
        let spec = WindowSpec {
            platform: &cfg.id,
            account_id: req.account_id,
            initial_url: &cfg.login_url,
            patterns: &cfg.collect.intercept_patterns,
            with_hud: true,
            title: &title,
            // 采集窗口不做登录自检(登录检测只在登录窗口)
            login_check_script: None,
        };
        // 开窗失败:此时尚无任何响应,直接带空响应 + 错误返回
        let window = match self.pool.ensure_window(app, &spec) {
            Ok(w) => w,
            Err(e) => return CollectOutcome::failed(e),
        };

        // 原生拦截缓冲:采集前清空,采集后取走本轮命中的响应(主通道,不依赖页面 invoke)
        let label = window_label(&cfg.id, req.account_id);
        let sink = self.pool.window_sink(&label);
        if let Some(s) = &sink {
            if let Ok(mut buf) = s.lock() {
                buf.clear();
            }
        }

        // HUD 切到该关键字的 tab,后续日志按关键字分组;再置「采集中」并记开始
        let _ = window.eval(&build_hud_keyword_eval(req.keyword));
        let _ = window.eval(&build_hud_status_eval(
            &format!("采集中:{}", req.keyword),
            true,
        ));
        self.log_step(
            app,
            &window,
            req.task_id,
            "info",
            &format!(
                "启动采集 · 关键词「{}」· 平台 {} · 目标 {} 条",
                req.keyword, cfg.name, req.target_count
            ),
        );

        let session_id = match self.channel.open_session() {
            Ok(s) => s,
            Err(e) => return CollectOutcome::failed(e),
        };
        // 把会话 id 绑给 HUD,「结束」按钮据此通知后端停止本次采集
        let _ = window.eval(&build_hud_session_eval(session_id));

        // 配置了节点级拟人 RPA 步骤则走拟人路径,否则回退内置「改 URL + 滚动翻页」。
        // 结果先存住不早退:无论成败都必须取走会话并清停止标志,
        // 否则窗口被手动关闭等异常路径会让会话缓冲与标志永久残留(内存泄漏)。
        let run_result = if cfg.collect.rpa_steps.is_empty() {
            self.run_legacy_scroll(
                &window,
                cfg,
                req.keyword,
                session_id,
                sink.as_ref(),
                req.adapter.as_ref(),
                req.target_count,
                req.content_tx.as_ref(),
                req.existing_ids,
                req.sort_mode,
                req.time_range,
                req.min_likes,
            )
            .await
        } else {
            self.run_human_rpa(
                &window,
                &cfg.collect.rpa_steps,
                req.keyword,
                session_id,
                req.sort_mode,
                req.time_range,
            )
            .await
        };

        // 原生拦截为主,页面 hook(session 通道)兜底,合并后由适配器按 content_id 去重
        let mut responses = self.channel.take_session(session_id);
        if let Some(s) = &sink {
            if let Ok(mut buf) = s.lock() {
                responses.append(&mut buf);
            }
        }
        // 清理本会话的停止标志,避免 session_id 复用时误判(成败都要清)
        self.control.clear(session_id);
        if let Err(e) = run_result {
            // 异常结束:仍把已拦截的部分响应带回,让调用方兜底解析落库 + 落媒体,保住已采数据
            let _ = window.eval(&build_hud_status_eval(
                &format!("采集异常结束:{}", req.keyword),
                false,
            ));
            return CollectOutcome {
                responses,
                error: Some(e),
            };
        }
        self.log_step(
            app,
            &window,
            req.task_id,
            "info",
            &format!(
                "「{}」采集结束 · 共加载 {} 批数据 · 整理入库中",
                req.keyword,
                responses.len()
            ),
        );
        let was_stopped = self.control.is_stopping(session_id);
        let _ = window.eval(&build_hud_status_eval(
            &format!(
                "{}:{}",
                if was_stopped { "已手动结束" } else { "本轮完成" },
                req.keyword
            ),
            false,
        ));
        CollectOutcome {
            responses,
            error: None,
        }
    }

    /// 记一条采集日志:更新窗口 HUD,并在有 task 上下文时推送前端事件(双端共用一条)。
    fn log_step(
        &self,
        app: &AppHandle,
        window: &WebviewWindow,
        task_id: Option<&str>,
        level: &str,
        message: &str,
    ) {
        let _ = window.eval(&build_hud_log_eval(level, message));
        if let Some(tid) = task_id {
            emit_collect_log(app, tid, level, message);
        }
    }

    /// 内置采集路径:URL 直达搜索页 + 盲滚翻页。用于未配置 `rpa_steps` 的平台。
    /// 验证暂停期间轮询等待其解除(用户手动完成)。返回 true=已解除、继续采集;
    /// false=超时未完成 / 暂停期间被手动结束。两条解除路径:
    /// ① overlay 弹窗(同源):重注入的自检脚本回到正常页报 present=false 解除;
    /// ② 整页跳转验证(可能跨域,invoke 被拦):Rust 侧轮询 window.url(),「曾进验证页、现已离开」
    ///    即判定完成解除——不依赖验证页内的 invoke,跨域也可靠。
    /// 每轮重注入自检脚本兼顾 ①;每 ~15s 在 HUD 提示剩余等待。
    async fn wait_verify_cleared(
        &self,
        window: &WebviewWindow,
        session_id: u64,
        verify_eval: &str,
        verify_url_patterns: &[String],
    ) -> bool {
        let _ = window.eval(&build_hud_status_eval("⚠ 等待手动完成安全验证…", true));
        let _ = window.eval(&build_hud_log_eval(
            "warn",
            "检测到安全验证 · 已暂停采集 · 请在本窗口手动完成验证,完成后自动恢复",
        ));
        let start = std::time::Instant::now();
        let mut last_hint: u64 = 0;
        // 是否曾观察到窗口处于验证页(用于跳转式验证的「离开即解除」判定)
        let mut saw_verify_page = false;
        while self.control.is_verifying(session_id) {
            // 暂停期间用户点 HUD「结束」→ 视为放弃,交由上层结束(已采数据保留)
            if self.control.is_stopping(session_id) {
                return false;
            }
            let elapsed = start.elapsed();
            if elapsed >= VERIFY_WAIT_MAX {
                let _ = window.eval(&build_hud_log_eval(
                    "error",
                    "安全验证超时未完成 · 结束本次采集(已采数据已保留)",
                ));
                return false;
            }
            // 重注入自检脚本:跳转到验证页后原页脚本随导航销毁,需重装才能在新页判定;
            // 回到正常页后它会 report present=false,从而解除本暂停(同源 overlay 场景)。
            if !verify_eval.is_empty() {
                let _ = window.eval(verify_eval);
            }
            // Rust 侧 URL 轮询(跨域跳转式验证的解除路径,不依赖页面 invoke):
            // 曾进过验证页、现已离开 → 判定验证完成,主动清除验证态恢复采集。
            if !verify_url_patterns.is_empty() {
                if let Ok(u) = window.url() {
                    let url = u.as_str().to_lowercase();
                    let on_verify = verify_url_patterns
                        .iter()
                        .any(|p| !p.is_empty() && url.contains(&p.to_lowercase()));
                    if on_verify {
                        saw_verify_page = true;
                    } else if saw_verify_page {
                        self.control.set_verifying(session_id, false);
                        break;
                    }
                }
            }
            let secs = elapsed.as_secs();
            if secs.saturating_sub(last_hint) >= 15 {
                last_hint = secs;
                let remaining = VERIFY_WAIT_MAX.as_secs().saturating_sub(secs);
                let _ = window.eval(&build_hud_log_eval(
                    "warn",
                    &format!("仍在等待手动完成安全验证 · 剩余约 {remaining}s"),
                ));
            }
            tokio::time::sleep(VERIFY_POLL).await;
        }
        let _ = window.eval(&build_hud_log_eval("info", "安全验证已完成 · 恢复采集"));
        let _ = window.eval(&build_hud_status_eval("采集中(已恢复)", true));
        true
    }

    async fn run_legacy_scroll(
        &self,
        window: &WebviewWindow,
        cfg: &PlatformConfig,
        keyword: &str,
        session_id: u64,
        sink: Option<&ResponseSink>,
        adapter: Option<&Arc<dyn PlatformAdapter>>,
        target_count: usize,
        // 本轮新增内容的出口;Some 时每轮把新增 Content 发出去供消费端增量落库,None 退回原行为
        content_tx: Option<&tokio::sync::mpsc::UnboundedSender<Vec<crate::model::Content>>>,
        // 数据库已存在的 content_id 快照;只对不在其中的新内容计入 target,旧内容仍增量落库更新统计
        existing_ids: Option<&HashSet<String>>,
        sort_mode: &str,
        time_range: &str,
        // 最低点赞数:低于此值的内容不计入 target、不发增量(0=不限;点赞数缺失放行)
        min_likes: i32,
    ) -> Result<()> {
        // 导航到搜索结果页;新页面会重挂 hook,session 未就绪期间命中响应进页内缓冲
        let search_template = &cfg.collect.search_url_template;
        if search_template.is_empty() {
            return Err(CrawlerError::Config(format!(
                "平台 {} 未配置 search_url_template",
                cfg.id
            )));
        }
        // 排序/时间映射成搜索 URL 参数(抖音 sort_type/publish_time),拼到搜索 URL 直接生效
        let extra_query = build_search_query(&cfg.collect, sort_mode, time_range);
        if !extra_query.is_empty() {
            let _ = window.eval(&build_hud_log_eval(
                "info",
                &format!("应用筛选 · 排序 {sort_mode} · 时间 {time_range}"),
            ));
        }
        window
            .eval(&build_search_eval(search_template, keyword, &extra_query))
            .map_err(|e| CrawlerError::Config(format!("导航搜索页失败: {e}")))?;

        // 等导航与 hook 就绪后注入会话 ID,触发首屏缓冲回放
        tokio::time::sleep(Duration::from_millis(NAV_SETTLE_MS)).await;
        window
            .eval(&build_set_session_eval(session_id))
            .map_err(|e| CrawlerError::Config(format!("注入采集会话失败: {e}")))?;
        // 注入安全验证自检(平台配了验证特征时):检测到弹窗 / 跳到验证页即经 report_collect_verify
        // 回传,本循环据此暂停;脚本为空(未配特征)时 eval 空串无副作用
        let verify_eval = crate::webview::build_verify_check_eval(
            session_id,
            &cfg.collect.verify_selectors,
            &cfg.collect.verify_texts,
            &cfg.collect.verify_url_patterns,
        );
        if !verify_eval.is_empty() {
            let _ = window.eval(&verify_eval);
        }

        // 智能停止:有适配器 + 原生缓冲 + 目标数量时,边滚边按「去重 content_id」计数,
        // 达标即停;若连续无新增疑似风控,则预警并继续重试,达 STAGNANT_STOP 轮仍无新增则自动结束,
        // 关闭采集窗口可终止。无目标/无适配器时退回配置的固定轮数盲滚。
        // 注意:这里的计数只为决定何时停,落库另算、不截断。
        let smart = matches!((sink, adapter), (Some(_), Some(_))) && target_count > 0;
        let max_rounds = cfg.collect.scroll_rounds; // 仅非智能模式用作固定轮数

        let mut seen: HashSet<String> = HashSet::new();
        // 去重且「库里尚不存在」的新内容数:作为 target 达标依据。
        // 旧内容(existing_ids 命中)仍会增量落库更新统计,但不计入此数,避免占满目标配额。
        let mut new_count: usize = 0;
        // stagnant:接口有响应却无新内容(疑似风控/到底)的连续轮数;
        // waiting:接口响应未增长(请求尚未返回)的连续轮数,二者分开计避免短停顿误报风控。
        let mut stagnant: u32 = 0;
        let mut waiting: u32 = 0;
        let _ = window.eval(&build_hud_log_eval(
            "info",
            &if smart {
                format!("结果页就绪 · 智能停止模式(达标即停)· 目标 {target_count} 条 · 开始翻页")
            } else {
                format!("结果页就绪 · 固定轮数模式 · 计划翻页 {max_rounds} 轮 · 开始翻页")
            },
        ));

        let mut round: u32 = 0;
        loop {
            round += 1;
            // 手动结束:HUD「结束」按钮触发后优雅停止,保留已采内容(作为正常完成)
            if self.control.is_stopping(session_id) {
                let _ = window.eval(&build_hud_log_eval(
                    "info",
                    "已手动结束 · 停止翻页 · 保留已采内容",
                ));
                break;
            }
            // 安全验证:检测到则暂停滚动,等用户在窗口手动完成、验证消失后自动恢复;
            // 超时仍未完成 → 报错结束本次采集(已采数据已由增量通道 / 兜底解析保住)。
            if self.control.is_verifying(session_id) {
                if !self
                    .wait_verify_cleared(
                        window,
                        session_id,
                        &verify_eval,
                        &cfg.collect.verify_url_patterns,
                    )
                    .await
                {
                    return Err(CrawlerError::Config(
                        "检测到安全验证未在限时内完成,采集中止(已保留已采数据)".into(),
                    ));
                }
            }
            // 滚动失败(常见于采集窗口被手动关闭)即终止本次采集
            window.eval(&build_scroll_eval()).map_err(|e| {
                CrawlerError::Config(format!("执行滚动失败(采集窗口可能已关闭): {e}"))
            })?;
            // 真实滚轮(WM_MOUSEWHEEL)兜底:快手/小红书等页面的结果列表在内部滚动容器里、
            // 且只认真实滚轮事件,程序 scrollTo 滚不动 document.body → 不触发翻页懒加载(卡在首屏)。
            // 必须额外投递真实滚轮才会加载下一页。HWND 非 Send,即取即用,不跨 await 持有。
            #[cfg(windows)]
            if let Ok(parent) = window.hwnd() {
                let _ = win_wheel::real_wheel(parent, -3);
            }
            // 非 Windows(mac 等):用合成 WheelEvent 触发懒加载,作为真实滚轮的对等实现
            #[cfg(not(windows))]
            let _ = window.eval(&crate::webview::build_wheel_eval());
            // 分页型结果页(B站等):滚动不触发翻页,滚到底后按文案点「下一页」请求下一页数据;
            // 按钮不存在 / 置灰(最后一页)时点击为空操作,由下方停滞判定兜底结束
            if !cfg.collect.next_page_texts.is_empty() {
                let _ = window.eval(&build_select_eval(&cfg.collect.next_page_texts));
            }
            // 拟人:每次滚动后随机停顿 2~6 秒,不匀速快速滚动
            let mut pause = random_scroll_pause();
            // 风控等待期间:每多等一轮,停顿额外 +15s 逐轮拉长,降低请求频率给手动验证留时间
            if stagnant >= STAGNANT_LIMIT {
                let extra_s = (stagnant - STAGNANT_LIMIT + 1) as u64 * 15;
                pause += Duration::from_secs(extra_s);
            }
            // 首屏尚未出内容:多为搜索接口还没返回,额外多等,避免「没等数据就空滚」漏抓(首次采集常见)
            if smart && seen.is_empty() {
                pause += Duration::from_secs(3);
            }
            let pause_ms = pause.as_millis();
            // 先打印「已下拉 + 即将停顿」再 sleep:让日志里的停顿值描述「接下来」的等待,
            // 与画面观感一致(此前在 sleep 后才打印,看起来「说停顿却马上滚」)。
            let scroll_log = if smart {
                format!("第 {round} 轮 · 已下拉,拟人停顿 {pause_ms}ms 等待接口返回")
            } else {
                format!("翻页 {round}/{max_rounds} · 已下拉,拟人停顿 {pause_ms}ms")
            };
            let _ = window.eval(&build_hud_log_eval("info", &scroll_log));
            tokio::time::sleep(pause).await;

            // 非智能模式:固定轮数盲滚
            if !smart {
                if round >= max_rounds {
                    break;
                }
                continue;
            }

            // 解析当前已拦截的累计响应,统计去重后的内容数(只为判断进度,不落库)
            // 原生拦截缓冲(sink)为主,并入 invoke 通道(channel)已收的本会话数据:
            // 使智能停止不依赖单一通道——mac 原生路径异常时仍能据 channel 计数,
            // 不会因 sink 空而误判「无数据」跑满 NO_RESPONSE_STOP。后续 adapter 按 id 去重。
            let mut snapshot = sink
                .and_then(|s| s.lock().ok().map(|buf| buf.clone()))
                .unwrap_or_default();
            snapshot.extend(self.channel.peek_session(session_id));
            let resp_count = snapshot.len();
            // 响应侧风控检测:拦截到的接口 URL 命中验证特征(覆盖整页跳转到验证中心、DOM 选择器
            // 抓不到的场景)→ 置验证态,下一轮顶部即暂停;清除交给注入脚本(回正常页报 present=false)。
            if !self.control.is_verifying(session_id)
                && response_hits_verify(&snapshot, &cfg.collect.verify_url_patterns)
            {
                self.control.set_verifying(session_id, true);
                let _ = window.eval(&build_hud_log_eval(
                    "warn",
                    "接口返回命中安全验证特征 · 疑似触发风控 · 将暂停采集等待手动验证",
                ));
            }
            let before_seen = seen.len();
            let before_new = new_count;
            if let Some(adapter) = adapter {
                let ctx = FetchContext {
                    keyword: keyword.to_string(),
                    responses: snapshot,
                };
                if let Ok(output) = adapter.parse(&TaskKind::Search, &ctx).await {
                    // 收集本轮新增(seen.insert 返回 true 即首次出现)的内容,供增量落库。
                    // 这里既维护去重计数(seen)又攒出 fresh,语义与原「now-before」一致。
                    let mut fresh: Vec<crate::model::Content> = Vec::new();
                    for c in &output.contents {
                        if seen.insert(c.content_id.clone()) {
                            // 最低点赞数过滤:低于阈值的不发增量、不计目标数,使
                            // 「目标数 / 进度 / 实际入库」三者口径一致(点赞数缺失放行)
                            let passes_likes = c
                                .stats
                                .like_count
                                .map(|likes| likes >= min_likes as i64)
                                .unwrap_or(true);
                            if !passes_likes {
                                continue;
                            }
                            fresh.push(c.clone());
                            // 库里已有的不占「新增」配额(仍随 fresh 增量落库更新统计)
                            let is_new = existing_ids
                                .map(|ids| !ids.contains(&c.content_id))
                                .unwrap_or(true);
                            if is_new {
                                new_count += 1;
                            }
                        }
                    }
                    // 有出口且本轮确有新增才发;send 失败说明消费端已退出,增量入库中断,
                    // 但采集结束后调用方仍会对全量响应兜底解析落库,故仅告警不中断滚动
                    if let (Some(tx), false) = (content_tx, fresh.is_empty()) {
                        if tx.send(fresh).is_err() {
                            tracing::warn!("增量入库通道已关闭,本轮新增改由采集结束后的兜底解析落库");
                        }
                    }
                }
            }
            // seen_added:会话内新见的去重内容(含库里已有的)→ 决定风控/到底是否停滞;
            // now:有效新增(排除库里已有)= 达标与进度;added 取 now 的增量,与日志「累计」自洽,
            // 避免出现「新增 9 却累计 8」这种因两种口径混用而显得没去重的怪数。
            let seen_added = seen.len() - before_seen;
            let now = new_count;
            let added = now - before_new;

            // 此前疑似风控、本轮恢复增长(会话见到新内容)→ 提示已解除
            if seen_added > 0 && stagnant >= STAGNANT_LIMIT {
                let _ = window.eval(&build_hud_log_eval(
                    "info",
                    "内容恢复增长 · 风控疑似已解除 · 恢复翻页采集",
                ));
            }

            let progress_pct = now * 100 / target_count;
            // 本轮新见内容里命中数据库已有的条数:重复内容,只更新点赞/评论等统计、不占新增配额
            let dup_existing = seen_added - added;
            let _ = window.eval(&build_hud_log_eval(
                "info",
                &format!(
                    "  新增 {added} · 重复 {dup_existing} · 累计 {now}/{target_count}({progress_pct}%)· 已加载 {resp_count} 批"
                ),
            ));

            // 达标即停:落库由调用方对最终全部响应解析,多出的不重复内容也会一并存
            if now >= target_count {
                let _ = window.eval(&build_hud_log_eval(
                    "info",
                    &format!("已达目标 · 累计 {now}/{target_count} 条 · 共翻页 {round} 轮 · 停止翻页"),
                ));
                break;
            }

            // 停止判定按「是否已采到内容」分流,比看接口响应数增长更可靠:
            //   ① 首屏还没出内容(seen 空):多为搜索接口未返回 / 页面渲染中,耐心等加载,
            //      不误判风控;连续 NO_RESPONSE_STOP 轮仍无内容才兜底结束(未登录/无结果)。
            //   ② 已采到内容后连续无新增:到底 / 全是重复 / 被风控,STAGNANT_STOP 轮快速收尾,
            //      不再苦等(到底时页面往往不再发新请求,响应数也不会再增长)。
            if seen_added > 0 {
                stagnant = 0;
                waiting = 0;
            } else if seen.is_empty() {
                waiting += 1;
                let _ = window.eval(&build_hud_log_eval(
                    "info",
                    &format!(
                        "首屏数据加载中 · 接口未返回内容(已等待 {waiting}/{NO_RESPONSE_STOP} 轮)"
                    ),
                ));
                if waiting >= NO_RESPONSE_STOP {
                    let _ = window.eval(&build_hud_log_eval(
                        "warn",
                        &format!(
                            "连续 {NO_RESPONSE_STOP} 轮无内容 · 自动结束(网络异常 / 未登录 / 无结果,已采 {now} 条)"
                        ),
                    ));
                    break;
                }
            } else {
                stagnant += 1;
                if stagnant >= STAGNANT_STOP {
                    let _ = window.eval(&build_hud_log_eval(
                        "warn",
                        &format!(
                            "连续 {STAGNANT_STOP} 轮无新增 · 判定已到底,结束本次采集(已采 {now} 条,目标 {target_count})"
                        ),
                    ));
                    break;
                }
                if stagnant >= STAGNANT_LIMIT {
                    // 逐轮预警并显示剩余轮数;被风控则可趁这几轮在采集窗口手动验证
                    let remaining = STAGNANT_STOP - stagnant;
                    let _ = window.eval(&build_hud_log_eval(
                        "warn",
                        &format!(
                            "疑似到底或风控 · 连续 {stagnant} 轮无新增内容 · 如被风控请在采集窗口手动验证;再 {remaining} 轮仍无新增将结束。"
                        ),
                    ));
                }
            }
        }
        // 收尾汇总去重情况(仅智能模式有逐内容计数):直观看到新增 vs 重复占比
        if smart {
            let dup_total = seen.len().saturating_sub(new_count);
            let _ = window.eval(&build_hud_log_eval(
                "info",
                &format!(
                    "去重统计 · 新增 {new_count} 条 · 重复(库中已有){dup_total} 条 · 会话去重共 {} 条",
                    seen.len()
                ),
            ));
        }
        Ok(())
    }

    /// 节点级拟人 RPA 路径:注入步骤执行器(在搜索框逐字输入、点击、等待、分段滚动),
    /// 全程在页面内拟人自驱动,跑完经 `rpa_done` 回传 ack。
    ///
    /// session 时机:RPA 跑完(结果页已渲染、hook 必已挂)后才注入 session,回放滚动期间
    /// 命中的页内缓冲,避免依赖新窗口首页 hook 是否就绪。
    ///
    /// ⚠️ 联调假设:RPA 步骤在单个页面上下文内一段跑完。若平台「点搜索」触发**整页导航**
    /// 会销毁脚本上下文导致 ack 永不回传(超时降级)——届时需把步骤按导航点拆分多段注入,
    /// 或改用 sessionStorage 持久化 session + initialization_script 续跑。小红书搜索为
    /// 客户端路由(SPA),预期不整页刷新,先按一段式跑通。
    #[allow(clippy::too_many_arguments)]
    async fn run_human_rpa(
        &self,
        window: &WebviewWindow,
        steps: &[RpaStep],
        keyword: &str,
        session_id: u64,
        sort_mode: &str,
        time_range: &str,
    ) -> Result<()> {
        // 窗口可能刚创建、首页仍在导航中;此时 eval 的脚本会随导航被清除 = 等于没注入,
        // 表现为不打字也不滚动。故先等首页加载稳定再注入;复用的已加载窗口多等片刻无害,
        // RPA 首步 waitFor 仍会兜底轮询。
        tokio::time::sleep(Duration::from_millis(NAV_SETTLE_MS)).await;

        // 滚动从 JS 步骤中分离:输入/搜索/等待由 JS 拟人执行,滚动由 Rust 用真实滚轮
        // (WM_MOUSEWHEEL)驱动——小红书靠真实滚轮事件触发懒加载,程序 scrollBy 无效。
        let scroll_rounds = steps.iter().find_map(|s| match s {
            RpaStep::Scroll { segments } => Some(*segments),
            _ => None,
        });
        let pre_steps: Vec<RpaStep> = steps
            .iter()
            .filter(|s| !matches!(s, RpaStep::Scroll { .. }))
            .cloned()
            .collect();

        let (run_id, rx) = self.rpa.open_run()?;
        window
            .eval(&build_human_rpa_script(&pre_steps, keyword, run_id))
            .map_err(|e| CrawlerError::Config(format!("注入拟人 RPA 脚本失败: {e}")))?;

        // 等输入/搜索完成(到结果页就绪);超时 / 失败仅告警,仍继续后续滚动与取数
        match tokio::time::timeout(Duration::from_millis(RPA_RUN_TIMEOUT_MS), rx).await {
            Ok(Ok(outcome)) if !outcome.ok => tracing::warn!(
                step = outcome.failed_step,
                msg = %outcome.message,
                "RPA 步骤失败,采集可能不完整"
            ),
            Ok(Ok(_)) => {}
            Ok(Err(_)) => tracing::warn!("RPA 结果通道提前关闭"),
            Err(_) => {
                // 超时后页面 ack 可能永不回传,主动清掉待回传条目防泄漏
                self.rpa.cancel(run_id);
                tracing::warn!("RPA 执行超时,取已拦截部分");
            }
        }

        // 结果页就绪后按任务排序/时间做 RPA 文案点击(综合/不限默认不点)
        apply_sort_time(window, sort_mode, time_range).await;

        // 真实滚轮翻页:逐轮投递 WM_MOUSEWHEEL,拟人间隔,触发分页懒加载
        if let Some(rounds) = scroll_rounds {
            self.scroll_with_real_wheel(window, rounds, session_id).await;
        }

        // 结果页已就绪,注入 session 回放页内缓冲,再等收尾让回放的 push 到齐
        window
            .eval(&build_set_session_eval(session_id))
            .map_err(|e| CrawlerError::Config(format!("注入采集会话失败: {e}")))?;
        tokio::time::sleep(Duration::from_millis(REPLAY_SETTLE_MS)).await;
        Ok(())
    }

    /// 持续下滚 rounds 轮,拟人间隔。Windows 用真实滚轮(WM_MOUSEWHEEL),
    /// 非 Windows(mac 等)用合成 WheelEvent 作对等实现。
    async fn scroll_with_real_wheel(&self, window: &WebviewWindow, rounds: u32, session_id: u64) {
        #[cfg(windows)]
        {
            for i in 0..rounds {
                // 手动结束:HUD「结束」按钮触发后停止真实滚轮翻页
                if self.control.is_stopping(session_id) {
                    let _ = window.eval(&build_hud_log_eval(
                        "info",
                        "已手动结束 · 停止翻页 · 保留已采内容",
                    ));
                    break;
                }
                // HWND 非 Send,不能跨 await 持有:每轮即取即用,await 前作用域结束自动丢弃
                match window.hwnd() {
                    Ok(parent) => {
                        // 每轮下滚 3 档
                        if let Err(e) = win_wheel::real_wheel(parent, -3) {
                            tracing::warn!("真实滚轮失败,停止滚动: {e}");
                            break;
                        }
                    }
                    Err(e) => {
                        tracing::warn!("获取窗口 HWND 失败,跳过滚动: {e}");
                        return;
                    }
                }
                let _ = window.eval(&build_hud_log_eval(
                    "info",
                    &format!("真实滚轮翻页 {}/{}", i + 1, rounds),
                ));
                // 拟人间隔:无 rand 依赖,用下标做伪随机扰动(800~1700ms)
                let ms = 800 + ((i as u64).wrapping_mul(263) % 900);
                tokio::time::sleep(Duration::from_millis(ms)).await;
            }
        }
        #[cfg(not(windows))]
        {
            for i in 0..rounds {
                // 手动结束:HUD「结束」按钮触发后停止翻页
                if self.control.is_stopping(session_id) {
                    let _ = window.eval(&build_hud_log_eval(
                        "info",
                        "已手动结束 · 停止翻页 · 保留已采内容",
                    ));
                    break;
                }
                // 合成 WheelEvent 触发懒加载(真实滚轮的非 Windows 对等实现)
                let _ = window.eval(&crate::webview::build_wheel_eval());
                let _ = window.eval(&build_hud_log_eval(
                    "info",
                    &format!("合成滚轮翻页 {}/{}", i + 1, rounds),
                ));
                // 拟人间隔:用下标做伪随机扰动(800~1700ms)
                let ms = 800 + ((i as u64).wrapping_mul(263) % 900);
                tokio::time::sleep(Duration::from_millis(ms)).await;
            }
        }
    }

    /// 在某账号的 WebView 内导航到内容详情页,滚动评论区采集**一级评论**,返回拦截到的接口响应。
    ///
    /// 流程:复用登录态窗口 → 导航详情页 → 注入会话回放首屏评论 → 滚动评论区触发分页 →
    /// 取走本视频命中的评论响应。时间范围过滤与精确截断由调用方(run_task)负责,此处只管采。
    pub async fn collect_comments(
        &self,
        app: &AppHandle,
        req: CommentCollectRequest<'_>,
    ) -> Result<Vec<InterceptedResponse>> {
        let cfg = req.platform_cfg;
        if cfg.collect.detail_url_template.is_empty() {
            return Err(CrawlerError::Config(format!(
                "平台 {} 未配置 detail_url_template,无法采集评论",
                cfg.id
            )));
        }
        let title = format!("{} - {}", cfg.name, req.account_id);
        let spec = WindowSpec {
            platform: &cfg.id,
            account_id: req.account_id,
            initial_url: &cfg.login_url,
            patterns: &cfg.collect.intercept_patterns,
            with_hud: true,
            title: &title,
            // 采集窗口不做登录自检(登录检测只在登录窗口)
            login_check_script: None,
        };
        let window = self.pool.ensure_window(app, &spec)?;

        // 原生拦截缓冲:采集前清空,采集后取走本视频命中的评论响应
        let label = window_label(&cfg.id, req.account_id);
        let sink = self.pool.window_sink(&label);
        if let Some(s) = &sink {
            if let Ok(mut buf) = s.lock() {
                buf.clear();
            }
        }

        // 评论日志归到该内容所属关键词的 HUD tab(评论不单独成 tab,与内容采集同档)。
        // 必须在导航详情页前切 tab:否则评论日志会落到上一个 tab,导航后当前视图看不到。
        let kw_tab = if req.keyword.is_empty() {
            "评论"
        } else {
            req.keyword
        };
        let _ = window.eval(&build_hud_keyword_eval(kw_tab));
        let session_id = self.channel.open_session()?;
        // 绑定会话给 HUD「结束」按钮,并置采集中状态
        let _ = window.eval(&build_hud_session_eval(session_id));
        let _ = window.eval(&build_hud_status_eval(
            &format!("评论采集:{}", req.title),
            true,
        ));
        self.log_step(
            app,
            &window,
            req.task_id,
            "info",
            &format!(
                "采集评论 ·「{}」· 平台 {} · 上限 {} 条",
                req.title, cfg.name, req.limit
            ),
        );

        // 结果先存住不早退:与 collect 同理,失败路径也必须取走会话并清停止标志防泄漏
        let run_result = self
            .run_comment_scroll(
                &window,
                cfg,
                req.content_id,
                req.xsec_token,
                session_id,
                sink.as_ref(),
                &req.adapter,
                req.limit,
            )
            .await;

        // 原生拦截为主,页面 hook(session 通道)兜底,合并后由适配器按 comment_id 去重
        let mut responses = self.channel.take_session(session_id);
        if let Some(s) = &sink {
            if let Ok(mut buf) = s.lock() {
                responses.append(&mut buf);
            }
        }
        if let Err(e) = run_result {
            self.control.clear(session_id);
            let _ = window.eval(&build_hud_status_eval(
                &format!("评论采集异常结束:{}", req.title),
                false,
            ));
            return Err(e);
        }
        let was_stopped = self.control.is_stopping(session_id);
        let _ = window.eval(&build_hud_status_eval(
            &format!(
                "{}:{}",
                if was_stopped { "已手动结束" } else { "本视频评论完成" },
                req.title
            ),
            false,
        ));
        // 清理本会话停止标志,避免 session_id 复用误判
        self.control.clear(session_id);
        Ok(responses)
    }

    /// 评论采集滚动:导航详情页后滚动评论区,边滚边按去重 comment_id 计数,
    /// 达到 limit / 连续无新增到底 / 连续无响应 / 手动停 即停。复用 legacy 的智能停止骨架。
    #[allow(clippy::too_many_arguments)]
    async fn run_comment_scroll(
        &self,
        window: &WebviewWindow,
        cfg: &PlatformConfig,
        content_id: &str,
        xsec_token: &str,
        session_id: u64,
        sink: Option<&ResponseSink>,
        adapter: &Arc<dyn PlatformAdapter>,
        limit: usize,
    ) -> Result<()> {
        // 导航到内容详情页;新页面重挂 hook,session 未就绪期间命中响应进页内缓冲
        window
            .eval(&build_detail_eval(
                &cfg.collect.detail_url_template,
                content_id,
                xsec_token,
            ))
            .map_err(|e| CrawlerError::Config(format!("导航详情页失败: {e}")))?;

        // 等导航与 hook 就绪后注入会话 ID,触发首屏(含首屏评论)缓冲回放
        tokio::time::sleep(Duration::from_millis(NAV_SETTLE_MS)).await;
        window
            .eval(&build_set_session_eval(session_id))
            .map_err(|e| CrawlerError::Config(format!("注入采集会话失败: {e}")))?;
        // 评论详情页同样注入安全验证自检(覆盖评论采集路径)
        let verify_eval = crate::webview::build_verify_check_eval(
            session_id,
            &cfg.collect.verify_selectors,
            &cfg.collect.verify_texts,
            &cfg.collect.verify_url_patterns,
        );
        if !verify_eval.is_empty() {
            let _ = window.eval(&verify_eval);
        }

        // limit>0 时按量停止,否则按配置固定轮数兜底(评论无目标时不宜无限滚)
        let smart = sink.is_some() && limit > 0;
        let max_rounds = cfg.collect.scroll_rounds.max(1);

        let mut seen: HashSet<String> = HashSet::new();
        let mut stagnant: u32 = 0;
        let mut waiting: u32 = 0;
        let _ = window.eval(&build_hud_log_eval(
            "info",
            &if smart {
                format!("详情页就绪 · 评论智能停止 · 目标 {limit} 条 · 开始滚动评论区")
            } else {
                format!("详情页就绪 · 固定 {max_rounds} 轮滚动评论区")
            },
        ));

        let mut round: u32 = 0;
        loop {
            round += 1;
            if self.control.is_stopping(session_id) {
                let _ = window.eval(&build_hud_log_eval("info", "已手动结束 · 停止评论采集"));
                break;
            }
            // 安全验证:评论采集同样暂停等手动完成,验证消失后自动恢复;超时则结束本视频评论采集
            if self.control.is_verifying(session_id)
                && !self
                    .wait_verify_cleared(
                        window,
                        session_id,
                        &verify_eval,
                        &cfg.collect.verify_url_patterns,
                    )
                    .await
            {
                let _ = window.eval(&build_hud_log_eval(
                    "warn",
                    "安全验证未完成 · 结束本视频评论采集(已采评论已保留)",
                ));
                break;
            }
            // 评论区滚动:程序 scrollTo + Windows 真实滚轮兜底。
            // ⚠️ 抖音详情页评论区多为右侧内部滚动容器,这两种通用滚动未必精准命中,
            // 需本机抓包/实测后按真实容器选择器调整(与 search/rpa 同属待校准点)。
            let _ = window.eval(&build_scroll_eval());
            #[cfg(windows)]
            if let Ok(parent) = window.hwnd() {
                let _ = win_wheel::real_wheel(parent, -3);
            }
            // 非 Windows(mac 等):合成 WheelEvent 触发评论区懒加载
            #[cfg(not(windows))]
            let _ = window.eval(&crate::webview::build_wheel_eval());

            // 拟人停顿(评论区用更短的 1~2s);疑似风控时逐轮拉长,给手动验证留时间
            let mut pause = random_comment_scroll_pause();
            if stagnant >= STAGNANT_LIMIT {
                let extra_s = (stagnant - STAGNANT_LIMIT + 1) as u64 * 5;
                pause += Duration::from_secs(extra_s);
            }
            // 首屏尚未出评论:多为评论接口还没返回,额外多等,避免没等数据就空滚
            if smart && seen.is_empty() {
                pause += Duration::from_secs(3);
            }
            tokio::time::sleep(pause).await;

            // 非智能模式:固定轮数滚动
            if !smart {
                if round >= max_rounds {
                    break;
                }
                continue;
            }

            // 解析当前累计响应,按去重 comment_id 统计(只为判断何时停,落库另算)
            // 原生拦截缓冲(sink)为主,并入 invoke 通道(channel)已收的本会话数据:
            // 使智能停止不依赖单一通道——mac 原生路径异常时仍能据 channel 计数,
            // 不会因 sink 空而误判「无数据」跑满 NO_RESPONSE_STOP。后续 adapter 按 id 去重。
            let mut snapshot = sink
                .and_then(|s| s.lock().ok().map(|buf| buf.clone()))
                .unwrap_or_default();
            snapshot.extend(self.channel.peek_session(session_id));
            let resp_count = snapshot.len();
            // 响应侧风控检测(同搜索路径):评论接口 URL 命中验证特征即置验证态,下一轮顶部暂停
            if !self.control.is_verifying(session_id)
                && response_hits_verify(&snapshot, &cfg.collect.verify_url_patterns)
            {
                self.control.set_verifying(session_id, true);
                let _ = window.eval(&build_hud_log_eval(
                    "warn",
                    "评论接口命中安全验证特征 · 疑似触发风控 · 将暂停等待手动验证",
                ));
            }
            let before_seen = seen.len();
            let ctx = FetchContext {
                keyword: content_id.to_string(),
                responses: snapshot,
            };
            if let Ok(output) = adapter.parse(&TaskKind::Comments, &ctx).await {
                for c in &output.comments {
                    seen.insert(c.comment_id.clone());
                }
            }
            let added = seen.len() - before_seen;
            let now = seen.len();
            let _ = window.eval(&build_hud_log_eval(
                "info",
                &format!("  评论 +{added} · 累计 {now}/{limit} · 已加载 {resp_count} 批"),
            ));

            // 达上限即停:落库由调用方对最终全部响应解析、按 limit 精确截断
            if now >= limit {
                let _ = window.eval(&build_hud_log_eval(
                    "info",
                    &format!("评论已达上限 {now}/{limit} · 共滚动 {round} 轮 · 停止"),
                ));
                break;
            }

            // 停止判定与内容采集一致:首屏未出评论耐心等加载,已采到评论后连续无新增即快速收尾
            if added > 0 {
                stagnant = 0;
                waiting = 0;
            } else if seen.is_empty() {
                waiting += 1;
                let _ = window.eval(&build_hud_log_eval(
                    "info",
                    &format!("评论加载中 · 接口未返回(已等待 {waiting}/{NO_RESPONSE_STOP} 轮)"),
                ));
                if waiting >= NO_RESPONSE_STOP {
                    let _ = window.eval(&build_hud_log_eval(
                        "warn",
                        &format!("连续 {NO_RESPONSE_STOP} 轮无评论 · 结束(该视频可能无评论 / 接口未命中,已采 {now} 条)"),
                    ));
                    break;
                }
            } else {
                stagnant += 1;
                if stagnant >= COMMENT_STAGNANT_STOP {
                    let _ = window.eval(&build_hud_log_eval(
                        "warn",
                        &format!("连续 {COMMENT_STAGNANT_STOP} 轮无新增评论 · 判定已到底,结束(已采 {now} 条)"),
                    ));
                    break;
                }
            }
        }
        Ok(())
    }

    /// 作者主页画像补采:复用登录态窗口 → 导航作者主页 → 注入会话回放首屏 →
    /// 短滚动几轮触发并等待画像接口返回 → 取走本会话拦截到的画像响应。
    /// 解析与落库由调用方(enrich_authors)负责,此处只管把响应采回来。
    pub async fn collect_profile(
        &self,
        app: &AppHandle,
        req: ProfileCollectRequest<'_>,
    ) -> Result<Vec<InterceptedResponse>> {
        let cfg = req.platform_cfg;
        if cfg.collect.profile_url_template.is_empty() {
            return Err(CrawlerError::Config(format!(
                "平台 {} 未配置 profile_url_template,无法补采画像",
                cfg.id
            )));
        }
        let title = format!("{} - {}", cfg.name, req.account_id);
        let spec = WindowSpec {
            platform: &cfg.id,
            account_id: req.account_id,
            initial_url: &cfg.login_url,
            patterns: &cfg.collect.intercept_patterns,
            with_hud: true,
            title: &title,
            // 采集窗口不做登录自检(登录检测只在登录窗口)
            login_check_script: None,
        };
        let window = self.pool.ensure_window(app, &spec)?;

        // 原生拦截缓冲:采集前清空,采集后取走本作者主页命中的画像响应
        let label = window_label(&cfg.id, req.account_id);
        let sink = self.pool.window_sink(&label);
        if let Some(s) = &sink {
            if let Ok(mut buf) = s.lock() {
                buf.clear();
            }
        }

        // 画像补采日志归到独立 HUD tab(不与关键词/评论混档)
        let _ = window.eval(&build_hud_keyword_eval("画像补采"));
        let session_id = self.channel.open_session()?;
        let _ = window.eval(&build_hud_session_eval(session_id));
        let _ = window.eval(&build_hud_status_eval(
            &format!("画像补采:{}", req.nickname),
            true,
        ));
        self.log_step(
            app,
            &window,
            req.task_id,
            "info",
            &format!("补采作者画像 ·「{}」· 平台 {}", req.nickname, cfg.name),
        );

        // 结果先存住不早退:失败路径也必须取走会话并清停止标志防泄漏(与 collect_comments 同理)
        let run_result = self
            .run_profile_dwell(
                &window,
                cfg,
                req.uid,
                req.xsec_token,
                session_id,
                sink.as_ref(),
            )
            .await;

        let mut responses = self.channel.take_session(session_id);
        if let Some(s) = &sink {
            if let Ok(mut buf) = s.lock() {
                responses.append(&mut buf);
            }
        }
        if let Err(e) = run_result {
            self.control.clear(session_id);
            let _ = window.eval(&build_hud_status_eval(
                &format!("画像补采异常结束:{}", req.nickname),
                false,
            ));
            return Err(e);
        }
        let _ = window.eval(&build_hud_status_eval(
            &format!("画像补采完成:{}", req.nickname),
            false,
        ));
        self.control.clear(session_id);
        Ok(responses)
    }

    /// 主页停留:导航 → 注入会话 → 短滚动几轮触发懒加载并等画像接口返回。
    /// 画像接口多在加载即发,拦到响应即提前结束;固定上限兜底避免空等。
    async fn run_profile_dwell(
        &self,
        window: &WebviewWindow,
        cfg: &PlatformConfig,
        uid: &str,
        xsec_token: &str,
        session_id: u64,
        sink: Option<&ResponseSink>,
    ) -> Result<()> {
        // 复用详情页导航脚本:模板 {id}=uid,{token}=xsec_token(无 token 占位的平台传空无害)
        window
            .eval(&build_detail_eval(
                &cfg.collect.profile_url_template,
                uid,
                xsec_token,
            ))
            .map_err(|e| CrawlerError::Config(format!("导航作者主页失败: {e}")))?;

        // 等导航与 hook 就绪后注入会话,回放首屏(含画像接口)缓冲
        tokio::time::sleep(Duration::from_millis(NAV_SETTLE_MS)).await;
        window
            .eval(&build_set_session_eval(session_id))
            .map_err(|e| CrawlerError::Config(format!("注入采集会话失败: {e}")))?;

        // 主页画像接口通常加载即发;滚动几轮辅助懒加载(YouTube/快手),拦到即停
        const PROFILE_ROUNDS: u32 = 4;
        for round in 0..PROFILE_ROUNDS {
            if self.control.is_stopping(session_id) {
                break;
            }
            let _ = window.eval(&build_scroll_eval());
            #[cfg(windows)]
            if let Ok(parent) = window.hwnd() {
                let _ = win_wheel::real_wheel(parent, -2);
            }
            #[cfg(not(windows))]
            let _ = window.eval(&crate::webview::build_wheel_eval());
            tokio::time::sleep(Duration::from_secs(2)).await;

            // 已拦到画像响应即可提前收尾(sink 或 channel 任一有数据);
            // 留至少 1 轮再判,避免首屏接口尚未返回就误判「无数据」
            let got = sink
                .and_then(|s| s.lock().ok().map(|b| !b.is_empty()))
                .unwrap_or(false)
                || !self.channel.peek_session(session_id).is_empty();
            if got && round >= 1 {
                break;
            }
        }
        Ok(())
    }
}
