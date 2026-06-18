//! WebView 窗口截图(应用内预览用)。
//!
//! Windows:经 WebView2 `ICoreWebView2::CapturePreview` 把当前画面渲染成 PNG 到内存流,
//! 读回字节。复用 `with_webview` 把闭包调度到 WebView2 线程访问其 COM 接口(同 native_intercept)。
//! CapturePreview 是异步的(完成回调在 WebView2 消息循环触发),故用 oneshot 把字节跨回 async。
//! 其它平台暂无实现(返回 None,前端预览退化为占位)。

use tauri::WebviewWindow;

/// 截图等待上限:CapturePreview 通常很快,给 5s 足够,超时即放弃本帧(下次轮询再试)。
#[cfg(windows)]
const CAPTURE_TIMEOUT_SECS: u64 = 5;

/// 截取窗口当前画面为 PNG 字节;失败 / 不支持的平台返回 None。
#[cfg(windows)]
pub async fn capture_png(window: &WebviewWindow) -> Option<Vec<u8>> {
    let (tx, rx) = tokio::sync::oneshot::channel::<Vec<u8>>();
    // with_webview 把闭包调度到 WebView2 线程;调度失败(窗口已销毁)直接返回 None
    if window
        .with_webview(move |webview| {
            // SAFETY: 在 WebView2 自身线程上访问其 COM 接口
            unsafe { win::capture(webview, tx) }
        })
        .is_err()
    {
        return None;
    }
    match tokio::time::timeout(std::time::Duration::from_secs(CAPTURE_TIMEOUT_SECS), rx).await {
        Ok(Ok(bytes)) if !bytes.is_empty() => Some(bytes),
        _ => None,
    }
}

/// 非 Windows:暂无截图实现(前端预览退化为占位提示)。
#[cfg(not(windows))]
pub async fn capture_png(_window: &WebviewWindow) -> Option<Vec<u8>> {
    None
}

#[cfg(windows)]
mod win {
    use tauri::webview::PlatformWebview;
    use tokio::sync::oneshot;
    use webview2_com::CapturePreviewCompletedHandler;
    use webview2_com::Microsoft::Web::WebView2::Win32::COREWEBVIEW2_CAPTURE_PREVIEW_IMAGE_FORMAT_PNG;
    use windows::Win32::Foundation::HGLOBAL;
    use windows::Win32::System::Com::StructuredStorage::CreateStreamOnHGlobal;
    use windows::Win32::System::Com::{IStream, STREAM_SEEK_SET};

    /// 在 WebView2 线程上发起一次 CapturePreview;完成回调读回 PNG 字节并经 `tx` 送出。
    pub unsafe fn capture(webview: PlatformWebview, tx: oneshot::Sender<Vec<u8>>) {
        let core = match webview.controller().CoreWebView2() {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!("截图取 CoreWebView2 失败: {e}");
                return; // tx 随之 drop → 接收端得 Err,回退占位
            }
        };
        // 内存流(fdeleteonrelease=TRUE:释放即回收 HGLOBAL)
        let stream = match CreateStreamOnHGlobal(HGLOBAL(std::ptr::null_mut()), true) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!("创建截图内存流失败: {e}");
                return;
            }
        };
        let stream_for_read = stream.clone();
        // 完成回调可能多次类型为 FnMut;用 Option::take 确保只送一次
        let mut tx_opt = Some(tx);
        let handler =
            CapturePreviewCompletedHandler::create(Box::new(move |result: windows::core::Result<()>| {
                if result.is_ok() {
                    let bytes = read_all(&stream_for_read);
                    if let Some(tx) = tx_opt.take() {
                        let _ = tx.send(bytes);
                    }
                }
                Ok(())
            }));
        if let Err(e) =
            core.CapturePreview(COREWEBVIEW2_CAPTURE_PREVIEW_IMAGE_FORMAT_PNG, &stream, &handler)
        {
            tracing::warn!("CapturePreview 调用失败: {e}");
        }
    }

    /// 从流头读出全部字节(CapturePreview 写完后游标在末尾,先 Seek 回 0)。
    unsafe fn read_all(stream: &IStream) -> Vec<u8> {
        let _ = stream.Seek(0, STREAM_SEEK_SET, None);
        let mut data: Vec<u8> = Vec::new();
        let mut chunk = [0u8; 65536];
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
            if read == 0 || hr.is_err() {
                break;
            }
        }
        data
    }
}
