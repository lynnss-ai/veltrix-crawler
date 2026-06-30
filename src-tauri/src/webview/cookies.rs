//! 从存活的 WebView2 会话读取 Cookie(含 httponly)。
//!
//! 用途:TikTok 等平台的视频直链(playAddr)由 CDN 校验**采集会话的 `tt_chain_token`**——
//! 它是 httponly Cookie(注入 JS 的 `document.cookie` 读不到),且直链死绑该会话 token
//! (换一个新 token 也 403)。故素材下载时(采集窗口尚未关闭)经
//! `ICoreWebView2CookieManager::GetCookies` 取真实 Cookie 交给 ffmpeg。
//! 非 Windows 平台返回 None(后续可接 WKWebView `httpCookieStore` 读取)。

use tauri::WebviewWindow;

/// 读 Cookie 的等待上限:GetCookies 通常很快,超时即放弃(降级为不带 Cookie,不阻断下载)。
#[cfg(windows)]
const COOKIE_TIMEOUT_SECS: u64 = 8;

/// 读取该 webview 会话中适用于 `uri` 的全部 Cookie,拼成 `name=value; name2=value2` 形式。
/// 失败 / 无 Cookie / 不支持平台返回 None。
#[cfg(windows)]
pub async fn read_cookies(window: &WebviewWindow, uri: &str) -> Option<String> {
    let (tx, rx) = tokio::sync::oneshot::channel::<String>();
    let uri = uri.to_string();
    // with_webview 把闭包调度到 WebView2 线程;调度失败(窗口已销毁)直接返回 None
    if window
        .with_webview(move |pw| {
            // SAFETY: 在 WebView2 自身线程上访问其 COM 接口
            unsafe { win::get_cookies(pw, &uri, tx) }
        })
        .is_err()
    {
        return None;
    }
    match tokio::time::timeout(std::time::Duration::from_secs(COOKIE_TIMEOUT_SECS), rx).await {
        Ok(Ok(cookie)) if !cookie.is_empty() => Some(cookie),
        _ => None,
    }
}

/// 非 Windows:暂无实现(读 Cookie 退化为 None)。
#[cfg(not(windows))]
pub async fn read_cookies(_window: &WebviewWindow, _uri: &str) -> Option<String> {
    None
}

#[cfg(windows)]
mod win {
    use tauri::webview::PlatformWebview;
    use tokio::sync::oneshot;
    use webview2_com::GetCookiesCompletedHandler;
    use webview2_com::Microsoft::Web::WebView2::Win32::{ICoreWebView2CookieList, ICoreWebView2_2};
    use windows::core::{Interface, HSTRING, PWSTR};
    use windows::Win32::System::Com::CoTaskMemFree;

    /// 在 WebView2 线程上发起 GetCookies;完成回调把 "name=value; ..." 串经 `tx` 送出。
    pub unsafe fn get_cookies(webview: PlatformWebview, uri: &str, tx: oneshot::Sender<String>) {
        let core = match webview.controller().CoreWebView2() {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!("取 CoreWebView2 失败,读 Cookie 跳过: {e}");
                return; // tx drop → 接收端 Err,上层 None
            }
        };
        // CookieManager 定义在 ICoreWebView2_2 上
        let core2: ICoreWebView2_2 = match core.cast() {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!("ICoreWebView2_2 不可用,读 Cookie 跳过: {e}");
                return;
            }
        };
        let manager = match core2.CookieManager() {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!("取 CookieManager 失败: {e}");
                return;
            }
        };

        // 完成回调为 FnMut;用 Option::take 确保只送一次。HRESULT 由 webview2-com 转成 Result<()>。
        let mut tx_opt = Some(tx);
        let handler = GetCookiesCompletedHandler::create(Box::new(
            move |result: windows::core::Result<()>,
                  list: Option<ICoreWebView2CookieList>|
                  -> windows::core::Result<()> {
                let mut out = String::new();
                if result.is_ok() {
                    if let Some(list) = list {
                        let mut count: u32 = 0;
                        if list.Count(&mut count).is_ok() {
                            for i in 0..count {
                                let Ok(cookie) = list.GetValueAtIndex(i) else {
                                    continue;
                                };
                                let mut name_p = PWSTR::null();
                                let mut value_p = PWSTR::null();
                                if cookie.Name(&mut name_p).is_err()
                                    || cookie.Value(&mut value_p).is_err()
                                {
                                    continue;
                                }
                                let name = pwstr_take(name_p);
                                let value = pwstr_take(value_p);
                                if name.is_empty() {
                                    continue;
                                }
                                if !out.is_empty() {
                                    out.push_str("; ");
                                }
                                out.push_str(&name);
                                out.push('=');
                                out.push_str(&value);
                            }
                        }
                    }
                }
                if let Some(tx) = tx_opt.take() {
                    let _ = tx.send(out);
                }
                Ok(())
            },
        ));

        let huri = HSTRING::from(uri);
        if let Err(e) = manager.GetCookies(&huri, &handler) {
            tracing::warn!("GetCookies 调用失败: {e}");
        }
    }

    /// 取出 WebView2 返回的 PWSTR 内容并释放其内存(口径同 native_intercept::pwstr_take)。
    unsafe fn pwstr_take(p: PWSTR) -> String {
        if p.is_null() {
            return String::new();
        }
        let s = p.to_string().unwrap_or_default();
        CoTaskMemFree(Some(p.as_ptr() as *const _));
        s
    }
}
