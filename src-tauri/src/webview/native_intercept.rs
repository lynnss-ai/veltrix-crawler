//! Windows WebView2 原生网络拦截。
//!
//! 在 Rust 端直接监听 `WebResourceResponseReceived`,把命中平台 `intercept_patterns`
//! 的接口响应体读出来推入窗口级缓冲。**完全不依赖页面 JS hook,也不走 Tauri invoke**,
//! 因此规避了远程页面 IPC 权限(capabilities)与 hook 注入时序两类导致「拦截 0 条」的问题。
//!
//! collect 流程:采集前清空缓冲 → RPA 触发搜索/滚动加载 → 取走缓冲里这一轮命中的响应。

use super::InterceptedResponse;
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Webview};

/// 命中响应的窗口级缓冲。每个采集窗口一份,采集前清空、采集后取走。
pub type ResponseSink = Arc<Mutex<Vec<InterceptedResponse>>>;

/// 拦截命中后向前端实时推送 `agent-network` 事件的上下文。仅浏览器 Agent 用(采集传 None,
/// 只写 sink 不推事件)。`emit.is_some()` 同时表示「全量拦截 + sink 限长」的 Agent 模式。
#[derive(Clone)]
pub struct EmitCtx {
    pub app: AppHandle,
    pub conversation_id: String,
}

/// 推给前端的响应体截断长度(仅展示用,避免大响应撑爆事件通道)。
#[cfg(windows)]
const EMIT_BODY_CAP: usize = 16 * 1024;
/// Agent 模式下网络缓冲最多保留条数(长会话防无限增长;采集每轮清空、传 None 不限长)。
#[cfg(windows)]
const SINK_MAX_ENTRIES: usize = 300;

/// 给 webview 安装原生响应拦截器。非 Windows 平台为空实现(退回页面 hook 路径)。
/// `patterns` 为空 = 全量拦截(仅 content-type 含 json 的响应),用于浏览器 Agent;
/// 非空 = 仅放行 URL 命中特征的响应(采集行为不变)。`emit` 见 [`EmitCtx`]。
#[cfg(windows)]
pub fn install(
    webview: &Webview,
    patterns: Arc<Vec<String>>,
    sink: ResponseSink,
    emit: Option<EmitCtx>,
) {
    // with_webview 把闭包调度到 WebView 线程执行;失败仅告警,不阻断采集
    if let Err(e) = webview.with_webview(move |pw| {
        // SAFETY: 在 WebView2 自身线程上访问其 COM 接口
        unsafe { win::install(pw, patterns, sink, emit) }
    }) {
        tracing::warn!("安装原生网络拦截失败(退回页面 hook): {e}");
    }
}

/// macOS:注册 WKScriptMessageHandler,接收注入脚本经 `webkit.messageHandlers` 回传的
/// 命中响应,填入同一窗口级 `sink`(与 Windows 的 `WebResourceResponseReceived` 等价)。
/// URL 命中过滤在注入脚本里完成,故此处不需要 patterns。emit 暂未在 mac 路径接通。
#[cfg(target_os = "macos")]
pub fn install(
    webview: &Webview,
    _patterns: Arc<Vec<String>>,
    sink: ResponseSink,
    _emit: Option<EmitCtx>,
) {
    if let Err(e) = webview.with_webview(move |pw| {
        // SAFETY: with_webview 在 macOS 主线程回调,可安全访问 WKWebView / UCC 的 AppKit 接口
        unsafe { mac::install(pw, sink) }
    }) {
        tracing::warn!("安装 mac 原生网络拦截失败(退回页面 invoke 兜底): {e}");
    }
}

/// 其余平台(Linux 等)无原生拦截,退回页面 invoke 兜底路径。
#[cfg(not(any(windows, target_os = "macos")))]
pub fn install(
    _webview: &Webview,
    _patterns: Arc<Vec<String>>,
    _sink: ResponseSink,
    _emit: Option<EmitCtx>,
) {
}

#[cfg(windows)]
mod win {
    use super::{EmitCtx, ResponseSink, EMIT_BODY_CAP, SINK_MAX_ENTRIES};
    use crate::webview::InterceptedResponse;
    use std::sync::Arc;
    use tauri::webview::PlatformWebview;
    use tauri::Emitter;
    use webview2_com::Microsoft::Web::WebView2::Win32::{
        ICoreWebView2_2, ICoreWebView2WebResourceResponseReceivedEventArgs,
        ICoreWebView2WebResourceResponseView,
    };
    use webview2_com::{
        WebResourceResponseReceivedEventHandler, WebResourceResponseViewGetContentCompletedHandler,
    };
    use windows::core::{w, Interface, PWSTR};
    use windows::Win32::System::Com::{CoTaskMemFree, IStream};

    pub unsafe fn install(
        webview: PlatformWebview,
        patterns: Arc<Vec<String>>,
        sink: ResponseSink,
        emit: Option<EmitCtx>,
    ) {
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

        // emit.is_some() = 浏览器 Agent 模式:patterns 空时全量拦截(只留 json)、缓冲限长、推前端事件。
        let agent_mode = emit.is_some();
        let handler = WebResourceResponseReceivedEventHandler::create(Box::new(
            move |_core, args: Option<ICoreWebView2WebResourceResponseReceivedEventArgs>| {
                let Some(args) = args else { return Ok(()) };

                // 取请求 URL(out 参数)
                let request = args.Request()?;
                let mut uri = PWSTR::null();
                request.Uri(&mut uri)?;
                let url = pwstr_take(uri);

                let response = args.Response()?;
                // 放行判定:patterns 非空(采集)→ URL 命中特征;patterns 空(Agent 全量)→ 仅 content-type 含 json
                let pass = if patterns.is_empty() {
                    is_json_response(&response)
                } else {
                    patterns.iter().any(|p| url.contains(p.as_str()))
                };
                if !pass {
                    return Ok(());
                }

                // 异步取响应内容流;拿到后读成字符串推入缓冲(+ Agent 模式推前端)
                let sink = sink.clone();
                let emit = emit.clone();
                let completed = WebResourceResponseViewGetContentCompletedHandler::create(Box::new(
                    move |_result: windows::core::Result<()>, stream: Option<IStream>| {
                        let Some(stream) = stream else { return Ok(()) };
                        let body = read_stream(&stream);
                        if let Ok(mut buf) = sink.lock() {
                            buf.push(InterceptedResponse {
                                url: url.clone(),
                                body: body.clone(),
                            });
                            // Agent 长会话防缓冲无限增长:超限丢最旧(采集传 None 不限长)
                            if agent_mode && buf.len() > SINK_MAX_ENTRIES {
                                let overflow = buf.len() - SINK_MAX_ENTRIES;
                                buf.drain(0..overflow);
                            }
                        }
                        // 实时推前端拦截面板(截断响应体,仅展示)
                        if let Some(ctx) = &emit {
                            let preview: String = body.chars().take(EMIT_BODY_CAP).collect();
                            let _ = ctx.app.emit(
                                "agent-network",
                                serde_json::json!({
                                    "conversationId": ctx.conversation_id,
                                    "url": url,
                                    "body": preview,
                                }),
                            );
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

    /// 响应 content-type 是否为 JSON(全量拦截模式下据此过滤掉 html/js/css/图片等噪声)。
    unsafe fn is_json_response(response: &ICoreWebView2WebResourceResponseView) -> bool {
        let Ok(headers) = response.Headers() else {
            return false;
        };
        let name = w!("Content-Type");
        // Contains 缺省时 GetHeader 会失败,故先判存在
        let mut has = windows::core::BOOL::default();
        if headers.Contains(name, &mut has).is_err() || !has.as_bool() {
            return false;
        }
        let mut val = PWSTR::null();
        if headers.GetHeader(name, &mut val).is_err() {
            return false;
        }
        pwstr_take(val).to_lowercase().contains("json")
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
