//! Windows WebView2 原生网络拦截。
//!
//! 在 Rust 端直接监听 `WebResourceResponseReceived`,把命中平台 `intercept_patterns`
//! 的接口响应体读出来推入窗口级缓冲。**完全不依赖页面 JS hook,也不走 Tauri invoke**,
//! 因此规避了远程页面 IPC 权限(capabilities)与 hook 注入时序两类导致「拦截 0 条」的问题。
//!
//! collect 流程:采集前清空缓冲 → RPA 触发搜索/滚动加载 → 取走缓冲里这一轮命中的响应。

use super::{CollectControl, InterceptedResponse};
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Emitter, Manager, Webview};

/// 命中响应的窗口级缓冲。每个采集窗口一份,采集前清空、采集后取走。
pub type ResponseSink = Arc<Mutex<Vec<InterceptedResponse>>>;

/// 页内信号桥接上下文:页面 → Rust 的「控制信号」走 WebView 原生消息通道
/// (Windows `chrome.webview.postMessage` / mac `webkit.messageHandlers`),
/// 不走 Tauri invoke——远程页面(平台站点)的 invoke 会被 ACL 拒绝
/// ("not allowed. Plugin not found",远程源不允许触达自定义命令)。
#[derive(Clone)]
pub struct SignalCtx {
    pub app: AppHandle,
    pub control: Arc<CollectControl>,
}

/// 处理页内信号(信封 `{"__veltrix": kind, ...}`),Win/mac 两通道共用。
/// - `api_done`:评论/画像直采脚本完成回传(等价 `comment_api_done` 命令)
/// - `verify`:验证弹窗出现/解除(等价 `report_collect_verify`)
/// - `stop`:HUD「结束」按钮(等价 `stop_collect`)
fn handle_signal(ctx: &SignalCtx, text: &str) {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(text) else {
        return;
    };
    let kind = v.get("__veltrix").and_then(|k| k.as_str()).unwrap_or("");
    if kind.is_empty() {
        return;
    }
    let sid = v.get("sessionId").and_then(|x| x.as_u64());
    match kind {
        "api_done" => {
            if let (Some(sid), Some(result)) = (sid, v.get("result").and_then(|r| r.as_str())) {
                // 留痕:区分「页内没发」与「桥收到但 poll 侧没取走」(直采回传丢失排查)
                tracing::info!(session = sid, len = result.len(), "收到 api_done(原生桥)");
                ctx.control.set_api_done(sid, result.to_string());
            } else {
                tracing::warn!("api_done 信号缺 sessionId/result,已丢弃");
            }
        }
        "verify" => {
            if let Some(sid) = sid {
                let present = v.get("present").and_then(|x| x.as_bool()).unwrap_or(false);
                tracing::info!("验证检测(原生桥):session={sid} present={present}");
                ctx.control.set_verifying(sid, present);
                let _ = ctx.app.emit(
                    "collect-verify",
                    serde_json::json!({ "present": present, "sessionId": sid }),
                );
            }
        }
        "stop" => {
            if let Some(sid) = sid {
                ctx.control.request_stop(sid);
            }
            if let Some(tid) = v.get("taskId").and_then(|x| x.as_str()) {
                if !tid.is_empty() {
                    ctx.control.request_stop_task(tid);
                }
            }
        }
        _ => {}
    }
}

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
/// Agent 模式 / 非采集窗口的网络缓冲最多保留条数(长会话防无限增长;
/// 采集窗口不丢——滚动循环按游标增量消费,丢了会漏解析)。
#[cfg(windows)]
const SINK_MAX_ENTRIES: usize = 300;

/// 给 webview 安装原生响应拦截器。非 Windows 平台为空实现(退回页面 hook 路径)。
/// `patterns` 为空 = 全量拦截(仅 content-type 含 json 的响应),用于浏览器 Agent;
/// 非空 = 仅放行 URL 命中特征的响应(采集行为不变)。`emit` 见 [`EmitCtx`]。
/// `cap_entries` = 缓冲限长(非采集窗口,如登录/访问平台,长期开着防内存线性膨胀);采集窗口传 false。
/// `signals` 非空时同时注册页内信号桥(WebMessageReceived),接收 api_done / verify / stop。
#[cfg(windows)]
pub fn install(
    webview: &Webview,
    patterns: Arc<Vec<String>>,
    sink: ResponseSink,
    emit: Option<EmitCtx>,
    cap_entries: bool,
    signals: Option<SignalCtx>,
) {
    // 空 stream 兜底(页内重取):仅采集窗口启用——有信号桥回传(signals)、
    // 非 Agent 全量(emit 只作展示)、非登录/访问窗口(cap_entries 漏捕无碍)。
    // GetContent 对缓存命中 / Service Worker 应答的响应常返回空流,漏捕后由页面
    // 重新 fetch 同一条已签名 URL 补回,经 intercept_sink_push 命令落进 sink。
    let fallback = if signals.is_some() && emit.is_none() && !cap_entries {
        Some(win::FallbackCtx::new(
            webview.app_handle().clone(),
            webview.label().to_string(),
        ))
    } else {
        None
    };
    // with_webview 把闭包调度到 WebView 线程执行;失败仅告警,不阻断采集
    if let Err(e) = webview.with_webview(move |pw| {
        // SAFETY: 在 WebView2 自身线程上访问其 COM 接口
        unsafe { win::install(pw, patterns, sink, emit, cap_entries, signals, fallback) }
    }) {
        tracing::warn!("安装原生网络拦截失败(退回页面 hook): {e}");
    }
}

/// 读取站点 Cookie(含 HttpOnly——页内 `document.cookie` 读不到它们)。
/// 评论直采凭空构造请求时用真实 msToken / 指纹 Cookie(s_v_web_id)补齐公共参数,
/// 缩小与页面真实请求的差距,降低被风控直接回 HTML 验证页的概率。
/// 仅 Windows 有实现(WebView2 CookieManager);其他平台返回空,脚本退回 document.cookie。
#[cfg(windows)]
pub async fn get_cookies(webview: &Webview, uri: &str, names: &[&str]) -> Vec<(String, String)> {
    let (tx, rx) = tokio::sync::oneshot::channel::<Vec<(String, String)>>();
    let uri = uri.to_string();
    let names: Vec<String> = names.iter().map(|s| s.to_string()).collect();
    if webview
        .with_webview(move |pw| {
            // SAFETY: 在 WebView2 自身线程上访问其 COM 接口
            unsafe { win::get_cookies(pw, &uri, &names, tx) }
        })
        .is_err()
    {
        return Vec::new();
    }
    // 3s 兜底:COM 回调不返回(窗口销毁等)时不能挂住采集流程
    match tokio::time::timeout(std::time::Duration::from_secs(3), rx).await {
        Ok(Ok(v)) => v,
        _ => Vec::new(),
    }
}

/// 非 Windows:无 CookieManager 等价物,返回空(脚本退回 document.cookie 现取)。
#[cfg(not(windows))]
pub async fn get_cookies(_webview: &Webview, _uri: &str, _names: &[&str]) -> Vec<(String, String)> {
    Vec::new()
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
    _cap_entries: bool,
    signals: Option<SignalCtx>,
) {
    if let Err(e) = webview.with_webview(move |pw| {
        // SAFETY: with_webview 在 macOS 主线程回调,可安全访问 WKWebView / UCC 的 AppKit 接口
        unsafe { mac::install(pw, sink, signals) }
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
    _cap_entries: bool,
    _signals: Option<SignalCtx>,
) {
}

#[cfg(windows)]
mod win {
    use super::{handle_signal, EmitCtx, ResponseSink, SignalCtx, EMIT_BODY_CAP, SINK_MAX_ENTRIES};
    use crate::webview::InterceptedResponse;
    use std::sync::Arc;
    use tauri::webview::PlatformWebview;
    use tauri::Emitter;
    use webview2_com::Microsoft::Web::WebView2::Win32::{
        ICoreWebView2CookieList, ICoreWebView2WebMessageReceivedEventArgs, ICoreWebView2_2,
        ICoreWebView2WebResourceResponseReceivedEventArgs, ICoreWebView2WebResourceResponseView,
    };
    use webview2_com::{
        GetCookiesCompletedHandler, WebMessageReceivedEventHandler,
        WebResourceResponseReceivedEventHandler, WebResourceResponseViewGetContentCompletedHandler,
    };
    use windows::core::{w, HSTRING, Interface, PCWSTR, PWSTR};
    use windows::Win32::System::Com::{CoTaskMemFree, IStream};

    /// 空 stream 兜底上下文:AppHandle + 窗口 label(用于页内 eval 重取)+ 已重取 URL 去重集合。
    #[derive(Clone)]
    pub struct FallbackCtx {
        app: tauri::AppHandle,
        label: String,
        seen: Arc<std::sync::Mutex<std::collections::HashSet<String>>>,
    }

    impl FallbackCtx {
        pub fn new(app: tauri::AppHandle, label: String) -> Self {
            Self {
                app,
                label,
                seen: Arc::new(std::sync::Mutex::new(std::collections::HashSet::new())),
            }
        }
    }

    /// 在页面上下文重取已签名 URL(带会话 Cookie,禁缓存),响应体经
    /// `intercept_sink_push` 命令回传该窗口拦截缓冲。
    /// 回传走 Tauri invoke 而非 chrome.webview.postMessage:后者在采集窗口实测不送达
    /// (WebMessageReceived 从未触发,api_done 同样靠 invoke 兜底)。
    /// a_bogus / msToken 覆盖 URL 参数与短时时间窗,同 URL 立即重放一般仍被放行。
    fn eval_refetch(app: &tauri::AppHandle, label: &str, url: &str) {
        use tauri::Manager;
        let Some(window) = app.get_webview_window(label) else {
            return;
        };
        let Ok(url_js) = serde_json::to_string(url) else {
            return;
        };
        let Ok(label_js) = serde_json::to_string(label) else {
            return;
        };
        let js = format!(
            r#"(function(){{var u={url_js},l={label_js};function push(t){{try{{window.__TAURI_INTERNALS__.invoke('intercept_sink_push',{{label:l,url:u,body:t}});}}catch(e){{}}}}fetch(u,{{credentials:'include',cache:'no-store'}}).then(function(r){{return r.text();}}).then(push).catch(function(){{push('');}});}})();"#
        );
        let _ = window.eval(&js);
    }

    pub unsafe fn install(
        webview: PlatformWebview,
        patterns: Arc<Vec<String>>,
        sink: ResponseSink,
        emit: Option<EmitCtx>,
        cap_entries: bool,
        signals: Option<SignalCtx>,
        fallback: Option<FallbackCtx>,
    ) {
        let core = match webview.controller().CoreWebView2() {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!("取 CoreWebView2 失败,原生拦截未启用: {e}");
                return;
            }
        };
        // 页内信号桥:WebMessageReceived 接收 chrome.webview.postMessage 的控制信号
        // (api_done / verify / stop),不经 Tauri invoke,规避远程页面 ACL 拒绝
        if let Some(ctx) = signals {
            let msg_handler = WebMessageReceivedEventHandler::create(Box::new(
                move |_core, args: Option<ICoreWebView2WebMessageReceivedEventArgs>| {
                    let Some(args) = args else { return Ok(()) };
                    let mut json = PWSTR::null();
                    args.WebMessageAsJson(&mut json)?;
                    let text = pwstr_take(json);
                    handle_signal(&ctx, &text);
                    Ok(())
                },
            ));
            let mut msg_token: i64 = 0;
            if let Err(e) = core.add_WebMessageReceived(&msg_handler, &mut msg_token) {
                tracing::warn!("注册 WebMessageReceived 失败(页内信号桥不可用): {e}");
            }
        }
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

                // 诊断:记录疑似风控/验证相关请求(WebResourceResponseReceived 帧无关,
                // 跨域验证码 iframe 的请求也能看到)。用于定位「被拦截」到底是弹验证码还是静默不返回。
                // 仅命中关键词才打,避免刷屏;这是定位风控形态的关键信号。
                {
                    let lu = url.to_ascii_lowercase();
                    // 只对 path 部分(? 之前)匹配:抖音正常业务接口(评论 / 收藏 / 详情等)的 query 里
                    // 普遍带 x-secsdk-web-signature / verifyFp=verify_... / fp=verify_... 等签名参数,
                    // 含 "secsdk" / "verify" 子串。拿整条 URL 匹配会把正常接口全误判成风控刷屏。
                    // 真正的验证码 SDK 请求特征在 path 里(rc-verifycenter/rmc-nocaptcha 等)。
                    let path = lu.split('?').next().unwrap_or(lu.as_str());
                    // redcaptcha/v2/getconfig 是小红书每次都预加载的验证码 SDK 配置(良性,非真验证),
                    // 排除掉,避免误报"疑似风控"刷屏。真正的验证挑战会走其它 redcaptcha 接口。
                    let is_benign_preload = path.contains("redcaptcha/v2/getconfig");
                    // 验证码 SDK 的静态资源(CDN 上的 .js/.html 等,如 rc-verifycenter / rmc-nocaptcha /
                    // security-secsdk 的 bundle)是每次页面加载的正常预载,并非真触发验证;
                    // 真风控是接口调用形态(如 verify.zijieapi.com/captcha/verify),path 无扩展名。
                    // 排除静态资源,避免每次开窗刷屏误报。
                    let is_static_asset = [
                        ".js", ".html", ".css", ".png", ".jpg", ".jpeg", ".svg", ".gif", ".woff",
                        ".woff2", ".ttf", ".map",
                    ]
                    .iter()
                    .any(|ext| path.ends_with(ext));
                    if !is_benign_preload
                        && !is_static_asset
                        && ["captcha", "verifycenter", "vc_captcha", "secsdk", "shark"]
                            .iter()
                            .any(|k| path.contains(k))
                    {
                        tracing::info!("拦截诊断:疑似风控请求 {url}");
                    }
                }

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
                let mut status: i32 = 0;
                let _ = response.StatusCode(&mut status);
                let sink = sink.clone();
                let emit = emit.clone();
                let fallback = fallback.clone();
                let completed = WebResourceResponseViewGetContentCompletedHandler::create(Box::new(
                    move |_result: windows::core::Result<()>, stream: Option<IStream>| {
                        let Some(stream) = stream else {
                            // GetContent 返回空 stream(命中缓存 / Service Worker 应答 / body 已被消费,
                            // 拿不到响应体):打 warn 留痕后,由页面以会话 Cookie 重取同一条已签名 URL
                            // 兜底补回(intercept_sink_push 命令 → sink)。seen 去重防「重取响应再空 stream」
                            // 死循环;仅 200 值得重取(204/304/重定向本就无业务 body)
                            tracing::warn!(url = %url, status, "拦截器 GetContent 返回空 stream,漏捕该响应");
                            if status == 200 {
                                if let Some(fb) = &fallback {
                                    let first = fb
                                        .seen
                                        .lock()
                                        .map(|mut s| s.insert(url.clone()))
                                        .unwrap_or(false);
                                    if first {
                                        tracing::info!(url = %url, "空 stream → 触发页内重取兜底");
                                        eval_refetch(&fb.app, &fb.label, &url);
                                    }
                                }
                            }
                            return Ok(());
                        };
                        let body = read_stream(&stream, STREAM_READ_CAP, &url);
                        if let Ok(mut buf) = sink.lock() {
                            buf.push(InterceptedResponse {
                                url: url.clone(),
                                body: body.clone(),
                            });
                            // 缓冲限长:Agent 长会话(emit)与非采集窗口(登录/访问,cap_entries)
                            // 超限丢最旧;采集窗口不丢(滚动循环按游标增量消费,丢了会漏解析)
                            if (agent_mode || cap_entries) && buf.len() > SINK_MAX_ENTRIES {
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

    /// 在 WebView2 线程上经 CookieManager 读取 uri 的站点 Cookie(含 HttpOnly),
    /// 只挑 names 里列出的项,经 oneshot 送出;任一步失败送空(调用方已按空兜底)。
    pub unsafe fn get_cookies(
        webview: PlatformWebview,
        uri: &str,
        names: &[String],
        tx: tokio::sync::oneshot::Sender<Vec<(String, String)>>,
    ) {
        let send_empty = |tx: tokio::sync::oneshot::Sender<Vec<(String, String)>>| {
            let _ = tx.send(Vec::new());
        };
        let core = match webview.controller().CoreWebView2() {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!("读 Cookie 取 CoreWebView2 失败: {e}");
                return send_empty(tx);
            }
        };
        let core2: ICoreWebView2_2 = match core.cast() {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!("读 Cookie: ICoreWebView2_2 不可用: {e}");
                return send_empty(tx);
            }
        };
        let mgr = match core2.CookieManager() {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!("读 Cookie: CookieManager 不可用: {e}");
                return send_empty(tx);
            }
        };
        let wanted: Vec<String> = names.to_vec();
        // 共享 tx:GetCookies 调用本身失败时回调不会触发,需在闭包外主动送空防干等
        let tx_shared = std::sync::Arc::new(std::sync::Mutex::new(Some(tx)));
        let tx_in_handler = tx_shared.clone();
        let handler = GetCookiesCompletedHandler::create(Box::new(
            move |result: windows::core::Result<()>, list: Option<ICoreWebView2CookieList>| {
                let mut out: Vec<(String, String)> = Vec::new();
                if result.is_ok() {
                    if let Some(list) = list {
                        let mut count: u32 = 0;
                        if list.Count(&mut count).is_ok() {
                            for i in 0..count {
                                let Ok(cookie) = list.GetValueAtIndex(i) else {
                                    continue;
                                };
                                let mut n = PWSTR::null();
                                let mut v = PWSTR::null();
                                let name = if cookie.Name(&mut n).is_ok() {
                                    pwstr_take(n)
                                } else {
                                    String::new()
                                };
                                let value = if cookie.Value(&mut v).is_ok() {
                                    pwstr_take(v)
                                } else {
                                    String::new()
                                };
                                if wanted.iter().any(|w| *w == name) {
                                    out.push((name, value));
                                }
                            }
                        }
                    }
                }
                if let Some(tx) = tx_in_handler.lock().ok().and_then(|mut g| g.take()) {
                    let _ = tx.send(out);
                }
                Ok(())
            },
        ));
        let uri_h = HSTRING::from(uri);
        if let Err(e) = mgr.GetCookies(PCWSTR::from_raw(uri_h.as_ptr()), &handler) {
            tracing::warn!("读 Cookie: GetCookies 调用失败: {e}");
            // 调用失败时回调不会触发,主动送空防调用方干等
            if let Some(tx) = tx_shared.lock().ok().and_then(|mut g| g.take()) {
                let _ = tx.send(Vec::new());
            }
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

    /// 采集模式下响应体最大读取字节数(约 16MB),超出截断。
    /// WebView2 GetContent 回调在 UI 线程执行,大响应需限制读取量防卡顿与内存膨胀。
    /// 注意:抖音主页作品接口(/aweme/v1/web/aweme/post/)单页 18 条带完整元数据,
    /// 实测首页可超 2MB;截断会让整页 JSON 解析失败、整页作品丢失,故上限需明显高于单页体积。
    const STREAM_READ_CAP: usize = 16 * 1024 * 1024;

    /// 把响应内容流读成字符串(UTF-8 lossy);响应体通常是 JSON 文本。
    /// `cap` 为 0 时不限长;非 0 时超出截断丢弃并打 warn(带 URL,截断的半个 JSON
    /// 会在采集侧解析失败,由 `report_profile_collect_audit` 计入「解析失败页数」)。
    unsafe fn read_stream(stream: &IStream, cap: usize, url: &str) -> String {
        let mut data: Vec<u8> = Vec::new();
        let mut chunk = [0u8; 16384];
        let max = if cap > 0 { cap } else { usize::MAX };
        loop {
            let mut read: u32 = 0;
            let hr = stream.Read(
                chunk.as_mut_ptr() as *mut core::ffi::c_void,
                chunk.len() as u32,
                Some(&mut read),
            );
            if read > 0 {
                let remaining = max.saturating_sub(data.len());
                let take = (read as usize).min(remaining).min(chunk.len());
                data.extend_from_slice(&chunk[..take]);
            }
            // read==0 即读完(S_FALSE);出错也停止,取已读部分
            if read == 0 || hr.is_err() {
                break;
            }
            if data.len() >= max {
                tracing::warn!(url = %url, "响应体超过 {} 字节,已截断", max);
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
    use super::{handle_signal, ResponseSink, SignalCtx};
    use crate::webview::InterceptedResponse;
    use objc2::rc::Retained;
    use objc2::runtime::{NSObject, NSObjectProtocol, ProtocolObject};
    use objc2::{define_class, msg_send, DefinedClass, MainThreadMarker, MainThreadOnly};
    use objc2_foundation::NSString;
    use objc2_web_kit::{WKScriptMessage, WKScriptMessageHandler, WKUserContentController};
    use tauri::webview::PlatformWebview;

    /// message handler 名,与 `build_native_intercept_init_script_mac` 注入脚本里的一致。
    const HANDLER_NAME: &str = "veltrixNative";

    /// handler 状态:拦截缓冲 + 页内信号桥(可选)。
    struct MacIvars {
        sink: ResponseSink,
        signals: Option<SignalCtx>,
    }

    define_class!(
        // WKScriptMessageHandler 协议要求 MainThreadOnly;ivar 持有窗口级缓冲
        #[unsafe(super(NSObject))]
        #[thread_kind = MainThreadOnly]
        #[name = "VeltrixMsgHandler"]
        #[ivars = MacIvars]
        struct MsgHandler;

        unsafe impl NSObjectProtocol for MsgHandler {}

        unsafe impl WKScriptMessageHandler for MsgHandler {
            #[unsafe(method(userContentController:didReceiveScriptMessage:))]
            fn did_receive_message(
                &self,
                _ucc: &WKUserContentController,
                message: &WKScriptMessage,
            ) {
                // body 为注入脚本 postMessage 的 JSON 字符串:
                // 拦截响应 {"u":url,"b":body};控制信号 {"__veltrix":kind,...}
                let body = unsafe { message.body() };
                if let Some(text) = body.downcast_ref::<NSString>() {
                    let text = text.to_string();
                    if text.contains("__veltrix") {
                        if let Some(ctx) = &self.ivars().signals {
                            handle_signal(ctx, &text);
                        }
                    } else {
                        push_message(&self.ivars().sink, &text);
                    }
                }
            }
        }
    );

    impl MsgHandler {
        fn new(
            mtm: MainThreadMarker,
            sink: ResponseSink,
            signals: Option<SignalCtx>,
        ) -> Retained<Self> {
            let this = Self::alloc(mtm).set_ivars(MacIvars { sink, signals });
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
    pub unsafe fn install(webview: PlatformWebview, sink: ResponseSink, signals: Option<SignalCtx>) {
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
        let handler = MsgHandler::new(mtm, sink, signals);
        let name = NSString::from_str(HANDLER_NAME);
        unsafe {
            // 防御:同名 handler 重复注册会抛 NSException;先移除(不存在则 no-op)再注册,
            // 避免窗口复用 / 重入等边界把进程搞崩
            ucc.removeScriptMessageHandlerForName(&name);
            ucc.addScriptMessageHandler_name(ProtocolObject::from_ref(&*handler), &name);
        }
    }
}
