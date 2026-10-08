//! 采集窗口「轻载模式」:请求层拦截页面的视频 / 字体子资源(**图片放行**)。
//!
//! 采集只消费拦截到的接口 JSON,页面里自动播放的视频、字体是纯载荷:视频解码是弱机
//! CPU 的最大单项,字体白费流量。图片放行是刻意取舍:① 窗口要人眼盯(过滑块验证码、
//! 确认页面状态),全灰块不可用——滑块验证码的拼图就是 IMAGE 请求,拦了人工验证无法完成;
//! ② 布局由图片撑起,放行最稳。与素材下载互不影响——封面/音频/视频素材由 Rust 侧
//! reqwest 客户端直接从 CDN 拉,根本不经过页面网络栈(见 media::DOWNLOAD_CLIENT);
//! 接口请求(XHR/FETCH)全放行,不触碰 intercept_patterns 的拦截链路。
//!
//! 实现走 WebView2 原生 `WebResourceRequested`(filter `*` 全上下文注册,handler 内按
//! 资源上下文细筛),命中 MEDIA / FONT 且开关打开时回 404 空响应。开关是每窗口一把的
//! `Arc<AtomicBool>`(任务在开窗前按任务配置拨动,见 pool::set_light_load),
//! 关闭时 handler 首行即返回,零开销放行。仅 Windows 生效;macOS 可后续用
//! WKContentRuleList 实现,行为上只是不省流。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use tauri::Webview;

/// 命中即拦的子资源上下文:音视频、字体(图片刻意放行,见模块注释)。
/// SCRIPT/STYLESHEET(页面结构必需)、XHR/FETCH(接口数据源)、DOCUMENT(页面本体)
/// 一律放行;WebSocket / SSE 等零散上下文也放行,避免误伤长连接类页面逻辑。
#[cfg(windows)]
const BLOCKED_CONTEXTS: [webview2_com::Microsoft::Web::WebView2::Win32::COREWEBVIEW2_WEB_RESOURCE_CONTEXT; 2] = [
    webview2_com::Microsoft::Web::WebView2::Win32::COREWEBVIEW2_WEB_RESOURCE_CONTEXT_MEDIA,
    webview2_com::Microsoft::Web::WebView2::Win32::COREWEBVIEW2_WEB_RESOURCE_CONTEXT_FONT,
];

/// 给采集窗口装媒体拦截器。`flag` 由调用方持有(pool 按窗口 label 存一把),
/// 任务开窗前拨动即可,装完无需重装。
#[cfg(windows)]
pub fn install(webview: &Webview, flag: Arc<AtomicBool>) {
    use webview2_com::Microsoft::Web::WebView2::Win32::{
        ICoreWebView2_2, ICoreWebView2Environment,
        COREWEBVIEW2_WEB_RESOURCE_CONTEXT_ALL,
    };
    use webview2_com::WebResourceRequestedEventHandler;
    use windows::core::{HSTRING, Interface};

    let webview = webview.clone();
    if let Err(e) = webview.with_webview(move |pw| {
        // SAFETY:回调在 WebView2 主线程执行,COM 接口访问与 native_intercept 同款
        unsafe {
            install_on_platform_webview(&pw, flag);
        }
    }) {
        tracing::warn!("轻载模式安装失败(页面媒体不做请求层拦截): {e}");
    }

    /// 实际装拦截器:拿到 CoreWebView2 → 注册全量过滤器 → 挂 handler。
    unsafe fn install_on_platform_webview(
        pw: &tauri::webview::PlatformWebview,
        flag: Arc<AtomicBool>,
    ) {
        // controller() 直接返回 ICoreWebView2Controller(wry 已确保非空)
        let controller = pw.controller();
        let Ok(core) = controller.CoreWebView2() else {
            tracing::warn!("取 CoreWebView2 失败,轻载模式未启用");
            return;
        };
        // Environment 在 ICoreWebView2_2 上(同 CookieManager / 拦截观察口),用于构造空响应
        let env: ICoreWebView2Environment = match core.cast::<ICoreWebView2_2>() {
            Ok(c) => match c.Environment() {
                Ok(e) => e,
                Err(e) => {
                    tracing::warn!("取 WebView2 Environment 失败,轻载模式未启用: {e}");
                    return;
                }
            },
            Err(e) => {
                tracing::warn!("取 ICoreWebView2_2 失败,轻载模式未启用: {e}");
                return;
            }
        };
        // 先注册过滤器事件才会触发;`*` 全上下文注册,handler 内再按上下文细筛
        if let Err(e) =
            core.AddWebResourceRequestedFilter(&HSTRING::from("*"), COREWEBVIEW2_WEB_RESOURCE_CONTEXT_ALL)
        {
            tracing::warn!("注册 WebResourceRequested 过滤器失败,轻载模式未启用: {e}");
            return;
        }
        let handler = WebResourceRequestedEventHandler::create(Box::new(move |_, args| {
            let Some(args) = args else {
                return Ok(());
            };
            // 开关关着:首行放行,不碰任何 COM 调用
            if !flag.load(Ordering::Relaxed) {
                return Ok(());
            }
            let mut ctx =
                webview2_com::Microsoft::Web::WebView2::Win32::COREWEBVIEW2_WEB_RESOURCE_CONTEXT(0);
            args.ResourceContext(&mut ctx)?;
            if !BLOCKED_CONTEXTS.contains(&ctx) {
                return Ok(());
            }
            // 404 空响应:页面按「资源不存在」处理,不重试、不下载、不解码
            let response =
                env.CreateWebResourceResponse(None, 404, &HSTRING::from("Blocked"), &HSTRING::new())?;
            args.SetResponse(&response)?;
            Ok(())
        }));
        let mut token: i64 = 0;
        if let Err(e) = core.add_WebResourceRequested(&handler, &mut token) {
            tracing::warn!("注册 WebResourceRequested 失败,轻载模式未启用: {e}");
        } else {
            tracing::info!("轻载模式已启用:页面视频/字体请求层拦截(图片放行)");
        }
    }
}

/// 非 Windows 平台:暂无请求层拦截能力,行为退化为「不省流」。
#[cfg(not(windows))]
pub fn install(_webview: &Webview, _flag: Arc<AtomicBool>) {}
