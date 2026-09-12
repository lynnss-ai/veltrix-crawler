//! 封面图片文字识别(OCR):当前仅接入智谱 OCR 工具 API。
//!
//! 接口:POST {api_url}/files/ocr,multipart/form-data 上传图片
//! (file + tool_type=hand_write + language_type=CHN_ENG),同步返回
//! words_result[].words 逐行文本,按行拼接为多行字符串。
//! 与转写三态约定一致:图片无文字时返回空串(调用方以「空串=已识别但无文字」落库),
//! 仅请求/识别失败才 Err(原因落 cover_ocr_error,可重试)。
//! 文档:<https://docs.bigmodel.cn/cn/guide/tools/zhipu-ocr>(单次 ≤8M,PNG/JPG/JPEG/BMP,0.01 元/次)。

use std::path::Path;

use veltrix_core::error::{CrawlerError, Result};

use super::http;

/// 单次 OCR 请求参数。provider 决定走哪家实现(目前仅智谱 glm)。
pub struct OcrRequest<'a> {
    pub provider: &'a str,
    pub api_url: &'a str,
    pub api_key: &'a str,
    /// 待识别图片本地路径(封面图,采集素材阶段已落盘 cover_path)
    pub image_path: &'a Path,
}

/// 按 provider code 分发 OCR 实现,识别文本按行拼接返回(无文字返回空串)。
pub async fn ocr_recognize(req: &OcrRequest<'_>) -> Result<String> {
    match req.provider {
        "glm" => zhipu_ocr(req).await,
        other => Err(CrawlerError::Config(format!(
            "封面 OCR 暂不支持厂商 {other}(目前仅智谱 glm)"
        ))),
    }
}

/// 智谱 OCR 工具 API:/files/ocr,multipart 上传图片,tool_type 固定 hand_write。
/// 成功响应 status=succeeded、words_result 逐行文本;失败响应 message 存原因(如格式错误)。
/// 该接口按次计费、响应无 token 用量(账单侧以请求次数体现)。
async fn zhipu_ocr(req: &OcrRequest<'_>) -> Result<String> {
    let bytes = tokio::fs::read(req.image_path).await.map_err(|e| {
        CrawlerError::Config(format!(
            "智谱封面 OCR 读取图片失败({}): {e}",
            req.image_path.display()
        ))
    })?;
    // 接口单次上限 8M,本地先拦一道,避免无效上传
    if bytes.len() > 8 * 1024 * 1024 {
        return Err(CrawlerError::Config(format!(
            "智谱封面 OCR 图片超过 8M 上限({} 字节)",
            bytes.len()
        )));
    }
    let file_name = req
        .image_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("cover.jpg")
        .to_string();
    let url = http::join_endpoint(req.api_url, "/files/ocr");
    // 智谱网关偶发把服务端瞬时故障标成 400 {"message":"Internal System Error"}(实测批量约两成请求),
    // 通用重试只覆盖 429/5xx 挡不住它;这类错误退避后重发基本能过,额外补 2 次重试
    let mut attempt = 0u32;
    loop {
        match zhipu_ocr_once(req, &bytes, &file_name, &url).await {
            Ok(text) => return Ok(text),
            Err(e) if attempt < 2 && e.to_string().contains("Internal System Error") => {
                attempt += 1;
                tracing::warn!("智谱封面 OCR 网关瞬时错误(Internal System Error),第 {attempt} 次退避重试");
                tokio::time::sleep(std::time::Duration::from_millis(800 << attempt)).await;
            }
            Err(e) => return Err(e),
        }
    }
}

/// 单次智谱 OCR 请求(供 zhipu_ocr 的「Internal System Error」重试循环复用)。
async fn zhipu_ocr_once(
    req: &OcrRequest<'_>,
    bytes: &[u8],
    file_name: &str,
    url: &str,
) -> Result<String> {
    let client = http::shared_client(http::CHAT_TIMEOUT_SECS)?;
    let resp = http::send_with_retry(
        || {
            // multipart Form 不可 Clone,每次(含重试)重建;封面图一般数百 KB,克隆开销可忽略
            let part = reqwest::multipart::Part::bytes(bytes.to_vec()).file_name(file_name.to_string());
            let form = reqwest::multipart::Form::new()
                .text("tool_type", "hand_write")
                .text("language_type", "CHN_ENG")
                .part("file", part);
            client.post(url).bearer_auth(req.api_key).multipart(form)
        },
        "智谱封面 OCR",
        // 打开 429/5xx 重试:网关瞬时抖动退避可过;仅失败才重发,成功不重复计费
        true,
    )
    .await?;
    let body: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| CrawlerError::Config(format!("智谱封面 OCR 响应解析失败: {e}")))?;
    let status = body.get("status").and_then(|s| s.as_str()).unwrap_or("");
    if status != "succeeded" {
        let msg = body
            .get("message")
            .and_then(|m| m.as_str())
            .unwrap_or("未知错误");
        return Err(CrawlerError::Config(format!("智谱封面 OCR 识别失败: {msg}")));
    }
    let lines: Vec<&str> = body
        .get("words_result")
        .and_then(|w| w.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|item| item.get("words").and_then(|w| w.as_str()))
                .filter(|s| !s.trim().is_empty())
                .collect()
        })
        .unwrap_or_default();
    Ok(lines.join("\n"))
}
