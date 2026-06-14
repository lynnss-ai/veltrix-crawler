//! Windows WebView2 原生网络拦截。
//!
//! 在 Rust 端直接监听 `WebResourceResponseReceived`,把命中平台 `intercept_patterns`
//! 的接口响应体读出来推入窗口级缓冲。**完全不依赖页面 JS hook,也不走 Tauri invoke**,
//! 因此规避了远程页面 IPC 权限(capabilities)与 hook 注入时序两类导致「拦截 0 条」的问题。
//!
//! collect 流程:采集前清空缓冲 → RPA 触发搜索/滚动加载 → 取走缓冲里这一轮命中的响应。

use super::InterceptedResponse;
use std::sync::{Arc, Mutex};
use tauri::WebviewWindow;

/// 命中响应的窗口级缓冲。每个采集窗口一份,采集前清空、采集后取走。
pub type ResponseSink = Arc<Mutex<Vec<InterceptedResponse>>>;

/// 给窗口安装原生响应拦截器。非 Windows 平台为空实现(退回页面 hook 路径)。
#[cfg(windows)]
pub fn install(window: &WebviewWindow, patterns: Arc<Vec<String>>, sink: ResponseSink) {
    // with_webview 把闭包调度到 WebView 线程执行;失败仅告警,不阻断采集
    if let Err(e) = window.with_webview(move |webview| {
        // SAFETY: 在 WebView2 自身线程上访问其 COM 接口
        unsafe { win::install(webview, patterns, sink) }
    }) {
        tracing::warn!("安装原生网络拦截失败(退回页面 hook): {e}");
    }
}

/// macOS:注册 WKScriptMessageHandler,接收注入脚本经 `webkit.messageHandlers` 回传的
/// 命中响应,填入同一窗口级 `sink`(与 Windows 的 `WebResourceResponseReceived` 等价)。
/// URL 命中过滤在注入脚本里完成,故此处不需要 patterns。
#[cfg(target_os = "macos")]
pub fn install(window: &WebviewWindow, _patterns: Arc<Vec<String>>, sink: ResponseSink) {
    if let Err(e) = window.with_webview(move |webview| {
        // SAFETY: with_webview 在 macOS 主线程回调,可安全访问 WKWebView / UCC 的 AppKit 接口
        unsafe { mac::install(webview, sink) }
    }) {
        tracing::warn!("安装 mac 原生网络拦截失败(退回页面 invoke 兜底): {e}");
    }
}

/// 其余平台(Linux 等)无原生拦截,退回页面 invoke 兜底路径。
#[cfg(not(any(windows, target_os = "macos")))]
pub fn install(_window: &WebviewWindow, _patterns: Arc<Vec<String>>, _sink: ResponseSink) {}

#[cfg(windows)]
mod win {
    use super::ResponseSink;
    use crate::webview::InterceptedResponse;
    use std::sync::Arc;
    use tauri::webview::PlatformWebview;
    use webview2_com::Microsoft::Web::WebView2::Win32::{
        ICoreWebView2_2, ICoreWebView2WebResourceResponseReceivedEventArgs,
    };
    use webview2_com::{
        WebResourceResponseReceivedEventHandler, WebResourceResponseViewGetContentCompletedHandler,
    };
    use windows::core::{Interface, PWSTR};
    use windows::Win32::System::Com::{CoTaskMemFree, IStream};

    pub unsafe fn install(webview: PlatformWebview, patterns: Arc<Vec<String>>, sink: ResponseSink) {
        let core = match webview.controller().CoreWebView2() {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!("取 CoreWebView2 失败,原生拦截未启用: {e}");
                return;
            }
        };
        // WebResourceResponseReceived 定义在 ICoreWebView2_2 上
        let core2: ICoreWebView2_2 = match core.cast() {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!("ICoreWebView2_2 不可用,原生拦截未启用: {e}");
                return;
            }
        };

        let handler = WebResourceResponseReceivedEventHandler::create(Box::new(
            move |_core, args: Option<ICoreWebView2WebResourceResponseReceivedEventArgs>| {
                let Some(args) = args else { return Ok(()) };

                // 取请求 URL(out 参数),只放行命中 patterns 的接口
                let request = args.Request()?;
                let mut uri = PWSTR::null();
                request.Uri(&mut uri)?;
                let url = pwstr_take(uri);
                if !patterns.iter().any(|p| url.contains(p.as_str())) {
                    return Ok(());
                }

                // 异步取响应内容流;拿到后读成字符串推入缓冲
                let response = args.Response()?;
                let sink = sink.clone();
                let completed = WebResourceResponseViewGetContentCompletedHandler::create(Box::new(
                    move |_result: windows::core::Result<()>, stream: Option<IStream>| {
                        if let Some(stream) = stream {
                            let body = read_stream(&stream);
                            if let Ok(mut buf) = sink.lock() {
                                buf.push(InterceptedResponse {
                                    url: url.clone(),
                                    body,
                                });
                            }
                        }
                        Ok(())
                    },
                ));
                response.GetContent(&completed)?;
                Ok(())
            },
        ));

        let mut token: i64 = 0;
        if let Err(e) = core2.add_WebResourceResponseReceived(&handler, &mut token) {
            tracing::warn!("注册 WebResourceResponseReceived 失败: {e}");
        }
    }

    /// 取出 WebView2 返回的 PWSTR 内容并释放其内存(由调用方 CoTaskMemFree)。
    unsafe fn pwstr_take(p: PWSTR) -> String {
        if p.is_null() {
            return String::new();
        }
        let s = p.to_string().unwrap_or_default();
        CoTaskMemFree(Some(p.as_ptr() as *const _));
        s
    }

    /// 把响应内容流读成字符串(UTF-8 lossy);响应体通常是 JSON 文本。
    unsafe fn read_stream(stream: &IStream) -> String {
        let mut data: Vec<u8> = Vec::new();
        let mut chunk = [0u8; 16384];
        loop {
            let mut read: u32 = 0;
            let hr = stream.Read(
                chunk.as_mut_ptr() as *mut core::ffi::c_void,
                chunk.len() as u32,
                Some(&mut read),
            );
            if read > 0 {
                data.extend_from_slice(&chunk[..read as usize]);
            }
            // read==0 即读完(S_FALSE);出错也停止,取已读部分
            if read == 0 || hr.is_err() {
                break;
            }
        }
        String::from_utf8_lossy(&data).into_owned()
    }
}

/// macOS WKWebView 原生网络拦截:注册 WKScriptMessageHandler,接收注入脚本经
/// `webkit.messageHandlers.veltrixNative` 回传的命中响应,推入窗口级缓冲。
/// 不走 Tauri invoke → 不受外部页面 capabilities / 注入时序影响(对应 Windows 原生拦截)。
#[cfg(target_os = "macos")]
mod mac {
    use super::ResponseSink;
    use crate::webview::InterceptedResponse;
    use objc2::rc::Retained;
    use objc2::runtime::{NSObject, NSObjectProtocol, ProtocolObject};
    use objc2::{define_class, msg_send, DefinedClass, MainThreadMarker, MainThreadOnly};
    use objc2_foundation::NSString;
    use objc2_web_kit::{WKScriptMessage, WKScriptMessageHandler, WKUserContentController};
    use tauri::webview::PlatformWebview;

    /// message handler 名,与 `build_native_intercept_init_script_mac` 注入脚本里的一致。
    const HANDLER_NAME: &str = "veltrixNative";

    define_class!(
        // WKScriptMessageHandler 协议要求 MainThreadOnly;ivar 持有窗口级缓冲
        #[unsafe(super(NSObject))]
        #[thread_kind = MainThreadOnly]
        #[name = "VeltrixMsgHandler"]
        #[ivars = ResponseSink]
        struct MsgHandler;

        unsafe impl NSObjectProtocol for MsgHandler {}

        unsafe impl WKScriptMessageHandler for MsgHandler {
            #[unsafe(method(userContentController:didReceiveScriptMessage:))]
            fn did_receive_message(
                &self,
                _ucc: &WKUserContentController,
                message: &WKScriptMessage,
            ) {
                // body 为注入脚本 postMessage 的 JSON 字符串:{"u":url,"b":body}
                let body = unsafe { message.body() };
                if let Some(text) = body.downcast_ref::<NSString>() {
                    push_message(self.ivars(), &text.to_string());
                }
            }
        }
    );

    impl MsgHandler {
        fn new(mtm: MainThreadMarker, sink: ResponseSink) -> Retained<Self> {
            let this = Self::alloc(mtm).set_ivars(sink);
            unsafe { msg_send![super(this), init] }
        }
    }

    /// 解析注入脚本回传的 JSON 并推入缓冲。解析失败仅告警,不影响采集。
    fn push_message(sink: &ResponseSink, json: &str) {
        #[derive(serde::Deserialize)]
        struct Msg {
            u: String,
            b: String,
        }
        match serde_json::from_str::<Msg>(json) {
            Ok(msg) => {
                if let Ok(mut buf) = sink.lock() {
                    buf.push(InterceptedResponse {
                        url: msg.u,
                        body: msg.b,
                    });
                }
            }
            Err(e) => tracing::warn!("解析 mac 拦截回传失败: {e}"),
        }
    }

    /// 给 WKWebView 的 userContentController 注册响应回传处理器。
    pub unsafe fn install(webview: PlatformWebview, sink: ResponseSink) {
        let Some(mtm) = MainThreadMarker::new() else {
            tracing::warn!("非主线程,mac 原生拦截未安装");
            return;
        };
        // controller() 返回 WKUserContentController 指针;retain 取得临时持有句柄
        let ucc_ptr = webview.controller() as *mut WKUserContentController;
        let Some(ucc) = (unsafe { Retained::retain(ucc_ptr) }) else {
            tracing::warn!("取 WKUserContentController 失败,mac 原生拦截未安装");
            return;
        };
        // UCC 内部会 retain handler,故本地 Retained 随 install 结束释放无碍
        let handler = MsgHandler::new(mtm, sink);
        let name = NSString::from_str(HANDLER_NAME);
        unsafe {
            // 防御:同名 handler 重复注册会抛 NSException;先移除(不存在则 no-op)再注册,
            // 避免窗口复用 / 重入等边界把进程搞崩
            ucc.removeScriptMessageHandlerForName(&name);
            ucc.addScriptMessageHandler_name(ProtocolObject::from_ref(&*handler), &name);
        }
    }
}
