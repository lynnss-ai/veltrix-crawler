//! 可见 WebView 池 + 高层采集桥接 `CollectBridge`。
//!
//! `WebviewPool` 维护「(平台, 账号) -> 可见窗口」映射。设计要点:
//! - **单窗口既登录又采集**:同一账号用同一窗口,登录态天然延续到采集;
//! - **多账号隔离**:每账号一个独立 `data_directory`(WebView2 用户数据目录),
//!   使不同账号的 Cookie / localStorage 互不覆盖,实现「同平台多账号」并存;
//! - **窗口可见**:用户能看到 RPA 操作过程,必要时手动过验证码。
//!
//! `CollectBridge` 在池之上对外暴露「关键词 → 拦截到的接口响应集合」的统一采集调用。

use veltrix_core::config::{PlatformConfig, RpaStep};
use veltrix_core::error::{CrawlerError, Result};
use crate::adapter::{FetchContext, PlatformAdapter};
use crate::model::TaskKind;
use crate::webview::native_intercept::{self, ResponseSink};
use crate::webview::{
    build_human_rpa_script, build_hud_init_script, build_hud_keyword_eval, build_hud_log_eval,
    build_hud_session_eval, build_hud_status_eval, build_intercept_init_script, build_scroll_eval,
    build_search_eval, build_set_session_eval, emit_collect_log, CollectControl, InterceptChannel,
    InterceptedResponse, RpaChannel,
};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindow, WebviewWindowBuilder};

/// 连续 N 轮已采集数无新增 → 判定「疑似风控/加载停滞」,发预警并继续重试。
const STAGNANT_LIMIT: u32 = 2;
/// 连续 N 轮无新增仍未恢复 → 自动结束本次采集(保留已采内容),不再无限等待。
const STAGNANT_STOP: u32 = 8;
/// 每次滚动后的拟人停顿区间(毫秒):1~5 秒随机,避免匀速快速滚动触发风控。
const SCROLL_PAUSE_MIN_MS: u64 = 1000;
const SCROLL_PAUSE_SPAN_MS: u64 = 4000;

/// 生成 1~5 秒的随机滚动停顿。无 rand 依赖,用系统时间纳秒做廉价熵源,拟人足够。
fn random_scroll_pause() -> Duration {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as u64)
        .unwrap_or(0);
    Duration::from_millis(SCROLL_PAUSE_MIN_MS + nanos % SCROLL_PAUSE_SPAN_MS)
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

        let parsed = spec
            .initial_url
            .parse()
            .map_err(|e| CrawlerError::Config(format!("非法 URL {}: {e}", spec.initial_url)))?;

        // 每账号独立用户数据目录,隔离 Cookie / 登录态(WebView2 在 Windows 生效)
        let data_dir = self.account_data_dir(app, &label)?;
        tracing::info!(
            label = %label,
            data_dir = %data_dir.display(),
            "创建账号隔离 WebView(独立用户数据目录)"
        );

        let window = WebviewWindowBuilder::new(app, &label, WebviewUrl::External(parsed))
            .title(format!("veltrix · {} · {}", spec.platform, spec.account_id))
            .visible(true)
            .maximized(true)
            .data_directory(data_dir)
            // 早期注入拦截 hook,命中平台接口特征的响应回传 Rust
            .initialization_script(build_intercept_init_script(spec.patterns))
            // 注入采集 HUD 浮层:每次文档加载自重建 + sessionStorage 恢复历史日志
            .initialization_script(build_hud_init_script())
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
        let base = app
            .path()
            .app_config_dir()
            .map_err(|e| CrawlerError::Config(format!("获取应用数据目录失败: {e}")))?;
        let dir = base
            .join("veltrix-crawler")
            .join("webview-data")
            .join(sanitize_dir_name(label));
        std::fs::create_dir_all(&dir)?;
        Ok(dir)
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
        cfg: &PlatformConfig,
    ) -> Result<WebviewWindow> {
        let spec = WindowSpec {
            platform,
            account_id,
            initial_url: &cfg.login_url,
            patterns: &cfg.collect.intercept_patterns,
        };
        let window = self.ensure_window(app, &spec)?;
        window
            .show()
            .map_err(|e| CrawlerError::Config(format!("显示窗口失败: {e}")))?;
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
    /// 每轮新增内容的出口。为何用 channel 而非等采集结束再返回:滚动循环要持续到
    /// 达标/风控/到底,在循环内同步等待落库会拖慢滚动节奏并阻塞拟人停顿;改用
    /// UnboundedSender 把「本轮新增 Content」即时发给消费端边收边落库,实现增量入库。
    /// `None` 时退回原行为(不发增量,仅最终一次性返回拦截响应给调用方解析)。
    pub content_tx: Option<tokio::sync::mpsc::UnboundedSender<Vec<crate::model::Content>>>,
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
    pub async fn collect(
        &self,
        app: &AppHandle,
        req: CollectRequest<'_>,
    ) -> Result<Vec<InterceptedResponse>> {
        let cfg = req.platform_cfg;
        let spec = WindowSpec {
            platform: &cfg.id,
            account_id: req.account_id,
            initial_url: &cfg.login_url,
            patterns: &cfg.collect.intercept_patterns,
        };
        let window = self.pool.ensure_window(app, &spec)?;

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
                "▶ 启动采集 · 关键词「{}」· 平台 {} · 账号 {} · 目标 {} 条",
                req.keyword, cfg.id, req.account_id, req.target_count
            ),
        );

        let session_id = self.channel.open_session()?;
        // 把会话 id 绑给 HUD,「结束」按钮据此通知后端停止本次采集
        let _ = window.eval(&build_hud_session_eval(session_id));

        // 配置了节点级拟人 RPA 步骤则走拟人路径,否则回退内置「改 URL + 滚动翻页」
        if cfg.collect.rpa_steps.is_empty() {
            self.run_legacy_scroll(
                &window,
                cfg,
                req.keyword,
                session_id,
                sink.as_ref(),
                req.adapter.as_ref(),
                req.target_count,
                req.content_tx.as_ref(),
            )
            .await?;
        } else {
            self.run_human_rpa(&window, &cfg.collect.rpa_steps, req.keyword, session_id)
                .await?;
        }

        // 原生拦截为主,页面 hook(session 通道)兜底,合并后由适配器按 content_id 去重
        let mut responses = self.channel.take_session(session_id);
        if let Some(s) = &sink {
            if let Ok(mut buf) = s.lock() {
                responses.append(&mut buf);
            }
        }
        self.log_step(
            app,
            &window,
            req.task_id,
            "info",
            &format!(
                "■ 采集结束 · 关键词「{}」· 累计拦截接口响应 {} 条 · 移交适配器解析去重入库",
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
        // 清理本会话的停止标志,避免 session_id 复用时误判
        self.control.clear(session_id);
        Ok(responses)
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
    ) -> Result<()> {
        // 导航到搜索结果页;新页面会重挂 hook,session 未就绪期间命中响应进页内缓冲
        let search_template = &cfg.collect.search_url_template;
        if search_template.is_empty() {
            return Err(CrawlerError::Config(format!(
                "平台 {} 未配置 search_url_template",
                cfg.id
            )));
        }
        window
            .eval(&build_search_eval(search_template, keyword))
            .map_err(|e| CrawlerError::Config(format!("导航搜索页失败: {e}")))?;

        // 等导航与 hook 就绪后注入会话 ID,触发首屏缓冲回放
        tokio::time::sleep(Duration::from_millis(NAV_SETTLE_MS)).await;
        window
            .eval(&build_set_session_eval(session_id))
            .map_err(|e| CrawlerError::Config(format!("注入采集会话失败: {e}")))?;

        // 智能停止:有适配器 + 原生缓冲 + 目标数量时,边滚边按「去重 content_id」计数,
        // 达标即停;若连续无新增疑似风控,则预警并继续重试,达 STAGNANT_STOP 轮仍无新增则自动结束,
        // 关闭采集窗口可终止。无目标/无适配器时退回配置的固定轮数盲滚。
        // 注意:这里的计数只为决定何时停,落库另算、不截断。
        let smart = matches!((sink, adapter), (Some(_), Some(_))) && target_count > 0;
        let max_rounds = cfg.collect.scroll_rounds; // 仅非智能模式用作固定轮数

        let mut seen: HashSet<String> = HashSet::new();
        let mut stagnant: u32 = 0;
        let _ = window.eval(&build_hud_log_eval(
            "info",
            &if smart {
                format!("● 结果页就绪 · 智能停止模式(达标即停)· 目标 {target_count} 条 · 开始翻页")
            } else {
                format!("● 结果页就绪 · 固定轮数模式 · 计划翻页 {max_rounds} 轮 · 开始翻页")
            },
        ));

        let mut round: u32 = 0;
        loop {
            round += 1;
            // 手动结束:HUD「结束」按钮触发后优雅停止,保留已采内容(作为正常完成)
            if self.control.is_stopping(session_id) {
                let _ = window.eval(&build_hud_log_eval(
                    "info",
                    "■ 已手动结束 · 停止翻页 · 保留已采内容",
                ));
                break;
            }
            // 滚动失败(常见于采集窗口被手动关闭)即终止本次采集
            window.eval(&build_scroll_eval()).map_err(|e| {
                CrawlerError::Config(format!("执行滚动失败(采集窗口可能已关闭): {e}"))
            })?;
            // 拟人:每次滚动后随机停顿 1~5 秒,不匀速快速滚动
            let mut pause = random_scroll_pause();
            // 风控等待期间:每多等一轮,停顿额外 +5s 逐轮拉长,降低请求频率给手动验证留时间
            if stagnant >= STAGNANT_LIMIT {
                let extra_s = (stagnant - STAGNANT_LIMIT + 1) as u64 * 5;
                pause += Duration::from_secs(extra_s);
            }
            let pause_ms = pause.as_millis();
            tokio::time::sleep(pause).await;

            // 非智能模式:固定轮数盲滚
            if !smart {
                let _ = window.eval(&build_hud_log_eval(
                    "info",
                    &format!("↓ 翻页 {round}/{max_rounds} 完成 · 拟人停顿 {pause_ms}ms"),
                ));
                if round >= max_rounds {
                    break;
                }
                continue;
            }

            // 解析当前已拦截的累计响应,统计去重后的内容数(只为判断进度,不落库)
            let snapshot = sink
                .and_then(|s| s.lock().ok().map(|buf| buf.clone()))
                .unwrap_or_default();
            let resp_count = snapshot.len();
            let before = seen.len();
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
                            fresh.push(c.clone());
                        }
                    }
                    // 有出口且本轮确有新增才发;send 失败(消费端已结束)安全忽略,不中断滚动
                    if let (Some(tx), false) = (content_tx, fresh.is_empty()) {
                        let _ = tx.send(fresh);
                    }
                }
            }
            // now 仍取 seen.len():达标/风控/到底判断与 HUD 日志全部沿用原语义,不受增量发送影响
            let now = seen.len();
            let added = now - before;

            // 此前疑似风控、本轮恢复增长 → 提示已解除
            if added > 0 && stagnant >= STAGNANT_LIMIT {
                let _ = window.eval(&build_hud_log_eval(
                    "info",
                    "✓ 内容恢复增长 · 风控疑似已解除 · 恢复翻页采集",
                ));
            }

            let progress_pct = now * 100 / target_count;
            let _ = window.eval(&build_hud_log_eval(
                "info",
                &format!(
                    "↓ 第 {round} 轮翻页 · 新增 {added} 条 · 累计 {now}/{target_count}({progress_pct}%)· 接口响应 {resp_count} 条 · 拟人停顿 {pause_ms}ms"
                ),
            ));

            // 达标即停:落库由调用方对最终全部响应解析,多出的不重复内容也会一并存
            if now >= target_count {
                let _ = window.eval(&build_hud_log_eval(
                    "info",
                    &format!("✓ 已达目标 · 累计 {now}/{target_count} 条 · 共翻页 {round} 轮 · 停止翻页"),
                ));
                break;
            }

            // 连续无新增:疑似风控 → 预警并继续重试;达 STAGNANT_STOP 轮仍无新增则自动结束
            if added == 0 {
                stagnant += 1;
                // 达上限:自动结束本次采集,保留已采内容
                if stagnant >= STAGNANT_STOP {
                    let _ = window.eval(&build_hud_log_eval(
                        "warn",
                        &format!(
                            "■ 连续 {STAGNANT_STOP} 轮无新增 · 自动结束本次采集(已采 {now} 条,目标 {target_count})"
                        ),
                    ));
                    break;
                }
                if stagnant == STAGNANT_LIMIT {
                    let _ = window.eval(&build_hud_log_eval(
                        "warn",
                        &format!(
                            "⚠ 风控预警 · 连续 {STAGNANT_LIMIT} 轮无新增内容 · 疑似触发平台风控。请在采集窗口手动完成验证(验证码 / 登录等);程序将继续重试,连续 {STAGNANT_STOP} 轮仍无新增则自动结束。"
                        ),
                    ));
                } else if stagnant > STAGNANT_LIMIT {
                    let _ = window.eval(&build_hud_log_eval(
                        "warn",
                        &format!(
                            "⏳ 等待风控解除 · 已等待 {stagnant}/{STAGNANT_STOP} 轮 · 当前 {now}/{target_count} 条"
                        ),
                    ));
                }
            } else {
                stagnant = 0;
            }
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
    async fn run_human_rpa(
        &self,
        window: &WebviewWindow,
        steps: &[RpaStep],
        keyword: &str,
        session_id: u64,
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
            Err(_) => tracing::warn!("RPA 执行超时,取已拦截部分"),
        }

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

    /// 用真实滚轮(WM_MOUSEWHEEL)持续下滚 rounds 轮,拟人间隔。非 Windows 平台跳过。
    async fn scroll_with_real_wheel(&self, window: &WebviewWindow, rounds: u32, session_id: u64) {
        #[cfg(windows)]
        {
            for i in 0..rounds {
                // 手动结束:HUD「结束」按钮触发后停止真实滚轮翻页
                if self.control.is_stopping(session_id) {
                    let _ = window.eval(&build_hud_log_eval(
                        "info",
                        "■ 已手动结束 · 停止翻页 · 保留已采内容",
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
                    &format!("↓ 真实滚轮翻页 {}/{}", i + 1, rounds),
                ));
                // 拟人间隔:无 rand 依赖,用下标做伪随机扰动(800~1700ms)
                let ms = 800 + ((i as u64).wrapping_mul(263) % 900);
                tokio::time::sleep(Duration::from_millis(ms)).await;
            }
        }
        #[cfg(not(windows))]
        {
            let _ = (window, rounds, session_id);
            tracing::warn!("非 Windows 平台不支持真实滚轮,跳过滚动");
        }
    }
}
