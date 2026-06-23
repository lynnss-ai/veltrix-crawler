//! WebView2 同步取 JS 返回值(浏览器 Agent 回读用)。
//!
//! 经 `with_webview` 把 `ICoreWebView2::ExecuteScript` 调度到 WebView2 线程,完成回调拿到脚本
//! 返回值的 **JSON 序列化串**(返回对象 → 标准 JSON),用 oneshot 跨回 async。
//! **不依赖页面 invoke / capabilities 远程白名单**,任意域名可用——这是浏览器 Agent 回读
//! 从「页面 invoke 回传」改造过来的关键(原路径在非白名单域调不通)。
//!
//! 注意:ExecuteScript **不会 await Promise**,故所有回读脚本必须是同步 IIFE 返回对象;
//! 「等元素出现」等异步语义由 Rust 侧轮询多次调用本函数实现(见 `agent::rpa::tools`)。
//! 非 Windows 平台返回 None(回读退化,后续可接 WKWebView evaluateJavaScript)。

use tauri::Webview;

/// 回读等待上限:ExecuteScript 通常很快;超时即放弃本次(上层据此报「页面未响应」)。
#[cfg(windows)]
const EVAL_TIMEOUT_SECS: u64 = 10;

/// 在 webview 当前页面执行 `js`(应为返回一个对象的**同步** IIFE),返回其 JSON 序列化串。
/// 失败 / 不支持平台返回 None。
#[cfg(windows)]
pub async fn eval_json(webview: &Webview, js: &str) -> Option<String> {
    let (tx, rx) = tokio::sync::oneshot::channel::<String>();
    let script = js.to_string();
    // with_webview 把闭包调度到 WebView2 线程;调度失败(webview 已销毁)直接返回 None
    if webview
        .with_webview(move |pw| {
            // SAFETY: 在 WebView2 自身线程上访问其 COM 接口
            unsafe { win::exec(pw, &script, tx) }
        })
        .is_err()
    {
        return None;
    }
    match tokio::time::timeout(std::time::Duration::from_secs(EVAL_TIMEOUT_SECS), rx).await {
        Ok(Ok(s)) => Some(s),
        _ => None,
    }
}

/// 非 Windows:暂无回读实现(浏览器 Agent 在该平台回读退化为 None)。
#[cfg(not(windows))]
pub async fn eval_json(_webview: &Webview, _js: &str) -> Option<String> {
    None
}

#[cfg(windows)]
mod win {
    use tauri::webview::PlatformWebview;
    use tokio::sync::oneshot;
    use webview2_com::ExecuteScriptCompletedHandler;
    use windows::core::HSTRING;

    /// 在 WebView2 线程上发起一次 ExecuteScript;完成回调把结果 JSON 串经 `tx` 送出。
    pub unsafe fn exec(webview: PlatformWebview, script: &str, tx: oneshot::Sender<String>) {
        let core = match webview.controller().CoreWebView2() {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!("回读取 CoreWebView2 失败: {e}");
                return; // tx 随之 drop → 接收端得 Err,上层返回 None
            }
        };
        // 完成回调类型为 FnMut;用 Option::take 确保只送一次。
        // webview2-com 已把结果 LPCWSTR 转成 Rust String(脚本返回值的 JSON 序列化串)。
        let mut tx_opt = Some(tx);
        let handler = ExecuteScriptCompletedHandler::create(Box::new(
            move |result: windows::core::Result<()>, json: String| -> windows::core::Result<()> {
                if result.is_ok() {
                    if let Some(tx) = tx_opt.take() {
                        let _ = tx.send(json);
                    }
                }
                Ok(())
            },
        ));
        let hscript = HSTRING::from(script);
        if let Err(e) = core.ExecuteScript(&hscript, &handler) {
            tracing::warn!("ExecuteScript 调用失败: {e}");
        }
    }
}
