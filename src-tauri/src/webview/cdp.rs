//! 经 WebView2 DevTools 协议(CDP)给页面 `<input type=file>` 塞本地文件(自动发布上传用)。
//!
//! 为什么不用注入 JS:出于安全,浏览器禁止脚本给 file input 赋值 `files`;
//! CDP 的 `DOM.setFileInputFiles` 走的是浏览器内部通道,不受此限——Playwright /
//! Puppeteer 的「上传文件」也是同款机制。因此自动发布的素材上传必须在 Rust 侧
//! 经 `ICoreWebView2::CallDevToolsProtocolMethod` 发起,而非页内脚本模拟。
//!
//! 调用链固定为三步(后一步依赖前一步的 nodeId):
//! `DOM.getDocument` → `DOM.querySelector` → `DOM.setFileInputFiles`,
//! 每次发起后都用 oneshot 同步等完成回调(模式同 `script_eval`)。

use anyhow::{anyhow, Result};
use tauri::WebviewWindow;

/// CDP 单次调用的等待上限:DOM 查询通常毫秒级;超时即放弃(上层按「页面未响应」处理)。
#[cfg(windows)]
const CDP_TIMEOUT_SECS: u64 = 10;

/// 把 `files`(本地绝对路径)设置到 `selector` 命中的文件输入框上。
///
/// 前置检查所有文件存在(不存在直接报错,避免把无效路径发给 CDP 后拿到含糊的协议错误);
/// selector 未命中返回「未找到文件输入框」错误,供上层换选择器重试或提示人工干预。
#[cfg(windows)]
pub async fn set_file_input_files(
    window: &WebviewWindow,
    selector: &str,
    files: Vec<String>,
) -> Result<()> {
    for f in &files {
        if !std::path::Path::new(f).exists() {
            return Err(anyhow!("待上传文件不存在: {f}"));
        }
    }

    // 1. 取文档根节点 nodeId(querySelector 的搜索起点)
    let doc = cdp_call(window, "DOM.getDocument", r#"{"depth":-1}"#).await?;
    let root_id = serde_json::from_str::<serde_json::Value>(&doc)
        .ok()
        .and_then(|v| v.pointer("/result/root/nodeId").and_then(|n| n.as_i64()))
        .ok_or_else(|| anyhow!("DOM.getDocument 返回缺少 root.nodeId: {doc}"))?;

    // 2. 按选择器找文件输入框;CDP 约定未命中返回 nodeId=0
    let query_params = serde_json::json!({ "nodeId": root_id, "selector": selector }).to_string();
    let found = cdp_call(window, "DOM.querySelector", &query_params).await?;
    let node_id = serde_json::from_str::<serde_json::Value>(&found)
        .ok()
        .and_then(|v| v.pointer("/result/nodeId").and_then(|n| n.as_i64()))
        .filter(|id| *id > 0)
        .ok_or_else(|| anyhow!("未找到文件输入框: {selector}"))?;

    // 3. 塞文件;返回 {} 即成功(协议级错误已在 cdp_call 内解析)
    let set_params = serde_json::json!({ "files": files, "nodeId": node_id }).to_string();
    cdp_call(window, "DOM.setFileInputFiles", &set_params).await?;
    Ok(())
}

/// 非 Windows:WebView2 CDP 仅 Windows 可用,其余平台显式报错(不做静默降级,
/// 避免发布流程误以为上传成功)。
#[cfg(not(windows))]
pub async fn set_file_input_files(
    _window: &WebviewWindow,
    _selector: &str,
    _files: Vec<String>,
) -> Result<()> {
    Err(anyhow!("仅 Windows 支持"))
}

/// 发起一次 CDP 调用并等待完成回调,返回响应 JSON 串。
///
/// 三层错误都在此收敛:调度失败(窗口已销毁)/ 超时 / CDP 协议级错误
/// (响应 JSON 顶层带 "error" 字段),上层拿到 Ok 即视为该步成功。
#[cfg(windows)]
async fn cdp_call(window: &WebviewWindow, method: &str, params: &str) -> Result<String> {
    let (tx, rx) = tokio::sync::oneshot::channel::<Result<String>>();
    let method_owned = method.to_string();
    let params_owned = params.to_string();
    // with_webview 把闭包调度到 WebView2 线程;调度失败(窗口已销毁)直接报错
    window
        .with_webview(move |pw| {
            // SAFETY: 在 WebView2 自身线程上访问其 COM 接口
            unsafe { win::call(pw, &method_owned, &params_owned, tx) }
        })
        .map_err(|_| anyhow!("WebView 已销毁,无法发起 CDP 调用: {method}"))?;

    let json = match tokio::time::timeout(std::time::Duration::from_secs(CDP_TIMEOUT_SECS), rx)
        .await
    {
        Ok(Ok(res)) => res?,
        Ok(Err(_)) => return Err(anyhow!("CDP {method} 回调通道异常(发送端被丢弃)")),
        Err(_) => return Err(anyhow!("CDP {method} 超时({CDP_TIMEOUT_SECS}s)")),
    };
    // CDP 错误不体现在 HRESULT,而是响应 JSON 的 "error" 字段,必须解析出来
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(&json) {
        if let Some(err) = v.get("error") {
            return Err(anyhow!("CDP {method} 返回错误: {err}"));
        }
    }
    Ok(json)
}

#[cfg(windows)]
mod win {
    use anyhow::{anyhow, Result};
    use tauri::webview::PlatformWebview;
    use tokio::sync::oneshot;
    use webview2_com::CallDevToolsProtocolMethodCompletedHandler;
    use windows::core::HSTRING;

    /// 在 WebView2 线程上发起一次 CallDevToolsProtocolMethod;
    /// 完成回调把响应 JSON 串(或 HRESULT 错误)经 `tx` 送出。
    pub unsafe fn call(
        webview: PlatformWebview,
        method: &str,
        params: &str,
        tx: oneshot::Sender<Result<String>>,
    ) {
        let core = match webview.controller().CoreWebView2() {
            Ok(c) => c,
            Err(e) => {
                let _ = tx.send(Err(anyhow!("取 CoreWebView2 失败: {e}")));
                return;
            }
        };
        // 完成回调为 FnMut;用 Option::take 确保只送一次。Arc<Mutex> 共享给闭包外侧:
        // 若下方 CallDevToolsProtocolMethod 调用本身失败,回调不会触发,必须能在外侧
        // 取回发送端把错误送回,否则接收端只能干等超时。
        // webview2-com 已把结果 LPCWSTR 转成 Rust String(CDP 响应的完整 JSON,
        // 形如 {"id":1,"result":{...}} 或 {"id":1,"error":{...}})。
        let tx_shared = std::sync::Arc::new(std::sync::Mutex::new(Some(tx)));
        let tx_in_cb = tx_shared.clone();
        let handler = CallDevToolsProtocolMethodCompletedHandler::create(Box::new(
            move |result: windows::core::Result<()>, json: String| -> windows::core::Result<()> {
                let tx = tx_in_cb.lock().ok().and_then(|mut slot| slot.take());
                if let Some(tx) = tx {
                    let res = match result {
                        Ok(()) => Ok(json),
                        Err(e) => Err(anyhow!("CDP 调用 HRESULT 失败: {e}")),
                    };
                    let _ = tx.send(res);
                }
                Ok(())
            },
        ));
        let hmethod = HSTRING::from(method);
        let hparams = HSTRING::from(params);
        if let Err(e) = core.CallDevToolsProtocolMethod(&hmethod, &hparams, &handler) {
            if let Ok(mut slot) = tx_shared.lock() {
                if let Some(tx) = slot.take() {
                    let _ = tx.send(Err(anyhow!("CallDevToolsProtocolMethod 调用失败: {e}")));
                }
            }
        }
    }
}
