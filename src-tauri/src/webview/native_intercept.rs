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

#[cfg(not(windows))]
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
