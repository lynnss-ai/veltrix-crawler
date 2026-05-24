//! 可见 WebView 池 + 高层采集桥接 `CollectBridge`。
//!
//! `WebviewPool` 维护「(平台, 账号) -> 可见窗口」映射。设计要点:
//! - **单窗口既登录又采集**:同一账号用同一窗口,登录态天然延续到采集;
//! - **多账号隔离**:每账号一个独立 `data_directory`(WebView2 用户数据目录),
//!   使不同账号的 Cookie / localStorage 互不覆盖,实现「同平台多账号」并存;
//! - **窗口可见**:用户能看到 RPA 操作过程,必要时手动过验证码。
//!
//! `CollectBridge` 在池之上对外暴露「关键词 → 拦截到的接口响应集合」的统一采集调用。

use veltrix_core::config::PlatformConfig;
use veltrix_core::error::{CrawlerError, Result};
use crate::webview::{
    build_intercept_init_script, build_scroll_eval, build_search_eval, build_set_session_eval,
    InterceptChannel, InterceptedResponse,
};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindow, WebviewWindowBuilder};

/// 导航到搜索页后、注入 session 前的等待(毫秒)。
/// 给页面完成导航 + 挂载 hook 留时间;此前命中的首屏请求由页内缓冲兜底,不会漏抓。
const NAV_SETTLE_MS: u64 = 2500;

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
}

impl WebviewPool {
    pub fn new() -> Self {
        Self::default()
    }

    /// 确保指定账号的可见 WebView 存在;已存在则复用(保留登录态)。
    fn ensure_window(&self, app: &AppHandle, spec: &WindowSpec<'_>) -> Result<WebviewWindow> {
        let label = window_label(spec.platform, spec.account_id);

        // 先复用:Tauri 持久化窗口句柄,二次进入不重建,登录态延续
        if let Some(existing) = app.get_webview_window(&label) {
            self.remember(&label, existing.clone())?;
            return Ok(existing);
        }
        if let Some(win) = self.lookup(&label)? {
            return Ok(win);
        }

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
            .data_directory(data_dir)
            // 早期注入拦截 hook,命中平台接口特征的响应回传 Rust
            .initialization_script(build_intercept_init_script(spec.patterns))
            .build()
            .map_err(|e| CrawlerError::Config(format!("创建 WebView 失败: {e}")))?;

        self.remember(&label, window.clone())?;
        Ok(window)
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

    fn lookup(&self, label: &str) -> Result<Option<WebviewWindow>> {
        Ok(self
            .windows
            .lock()
            .map_err(|_| CrawlerError::Config("WebView 池锁异常".into()))?
            .get(label)
            .cloned())
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
}

/// 一次采集调用的参数。集中成结构体以遵守「参数 ≤ 4」。
pub struct CollectRequest<'a> {
    pub account_id: &'a str,
    pub keyword: &'a str,
    /// 平台配置:提供登录页、搜索 URL 模板、拦截特征与滚动参数。
    pub platform_cfg: &'a PlatformConfig,
}

impl CollectBridge {
    pub fn new(pool: Arc<WebviewPool>, channel: Arc<InterceptChannel>) -> Self {
        Self { pool, channel }
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

        let session_id = self.channel.open_session()?;

        // 导航到搜索结果页;新页面会重挂 hook,session 未就绪期间命中响应进页内缓冲
        let search_template = &cfg.collect.search_url_template;
        if search_template.is_empty() {
            return Err(CrawlerError::Config(format!(
                "平台 {} 未配置 search_url_template",
                cfg.id
            )));
        }
        window
            .eval(&build_search_eval(search_template, req.keyword))
            .map_err(|e| CrawlerError::Config(format!("导航搜索页失败: {e}")))?;

        // 等导航与 hook 就绪后注入会话 ID,触发首屏缓冲回放
        tokio::time::sleep(Duration::from_millis(NAV_SETTLE_MS)).await;
        window
            .eval(&build_set_session_eval(session_id))
            .map_err(|e| CrawlerError::Config(format!("注入采集会话失败: {e}")))?;

        // RPA 翻页:逐轮滚动 + 间隔等待,节奏受配置控制以降低风控
        let interval = Duration::from_millis(cfg.collect.scroll_interval_ms);
        for _ in 0..cfg.collect.scroll_rounds {
            window
                .eval(&build_scroll_eval())
                .map_err(|e| CrawlerError::Config(format!("执行滚动失败: {e}")))?;
            tokio::time::sleep(interval).await;
        }

        Ok(self.channel.take_session(session_id))
    }
}
