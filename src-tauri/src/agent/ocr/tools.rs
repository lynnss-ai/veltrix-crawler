//! 屏幕 OCR 取字:截屏 → 识别文字 → 纯文本回传。
//!
//! 截屏走 xcap(已依赖,跨平台拿 `image::RgbaImage`);识别 Windows 走 WinRT `Windows.Media.Ocr`。
//! WinRT 的 `IAsyncOperation` 用 `.get()` 同步等待,且 COM 对象非 `Send`、不能跨 await——
//! 故整条「编码 PNG → 解码成 SoftwareBitmap → OCR」链路都收敛在一次 `spawn_blocking` 里,创建即用即丢。

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};

use crate::agent::core::{Tool, ToolDef, ToolRegistry, ToolResult};

/// 构造屏幕 OCR 工具注册表。无外部上下文(直接截当前屏幕)。
pub fn build_registry() -> ToolRegistry {
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(OcrScreenTool));
    registry.register(Arc::new(OcrRegionTool));
    registry
}

/// 把 JoinError / 闭包内的 Result<String,String> 收敛成 ToolResult。
fn blocking_result(joined: Result<Result<String, String>, tokio::task::JoinError>) -> ToolResult {
    match joined {
        Ok(Ok(text)) if text.trim().is_empty() => ToolResult::ok("(未识别到文字)"),
        Ok(Ok(text)) => ToolResult::ok(text),
        Ok(Err(e)) => ToolResult::err(e),
        Err(e) => ToolResult::err(format!("OCR 任务异常: {e}")),
    }
}

/// 截主显示器全屏,得到 RGBA 图。复用 xcap(与 desktop 截屏同源),失败带上下文。
fn grab_primary_screen() -> Result<image::RgbaImage, String> {
    let monitors = xcap::Monitor::all().map_err(|e| format!("枚举显示器失败: {e}"))?;
    let monitor = monitors.into_iter().next().ok_or("未找到显示器")?;
    monitor.capture_image().map_err(|e| format!("截屏失败: {e}"))
}

struct OcrScreenTool;
#[async_trait]
impl Tool for OcrScreenTool {
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "ocr_screen".into(),
            description: "对当前主显示器全屏做 OCR,识别并返回画面里的全部文字(纯文本)。\
                适合让 Agent『读屏』取字。仅 Windows 支持(系统自带 OCR);\
                中文识别需系统装有中文 OCR 语言包。"
                .into(),
            input_schema: json!({ "type": "object", "properties": {} }),
        }
    }
    async fn run(&self, _args: Value) -> ToolResult {
        let joined = tokio::task::spawn_blocking(move || {
            let image = grab_primary_screen()?;
            recognize_rgba(&image)
        })
        .await;
        blocking_result(joined)
    }
}

struct OcrRegionTool;
#[async_trait]
impl Tool for OcrRegionTool {
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "ocr_region".into(),
            description: "对屏幕上指定矩形区域(x, y, width, height,整数像素,左上角为原点)做 OCR,\
                识别并返回该区域内的全部文字(纯文本)。仅 Windows 支持;\
                中文识别需系统装有中文 OCR 语言包。"
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "x": { "type": "integer", "description": "区域左上角 X 像素坐标" },
                    "y": { "type": "integer", "description": "区域左上角 Y 像素坐标" },
                    "width": { "type": "integer", "description": "区域宽度(像素,>0)" },
                    "height": { "type": "integer", "description": "区域高度(像素,>0)" }
                },
                "required": ["x", "y", "width", "height"]
            }),
        }
    }
    async fn run(&self, args: Value) -> ToolResult {
        let get_i = |key: &str| args.get(key).and_then(Value::as_i64);
        let (Some(x), Some(y), Some(width), Some(height)) =
            (get_i("x"), get_i("y"), get_i("width"), get_i("height"))
        else {
            return ToolResult::err("缺少参数 x / y / width / height");
        };
        if width <= 0 || height <= 0 {
            return ToolResult::err("width / height 必须为正整数");
        }
        // 负坐标按 0 夹紧(多屏负坐标不在主屏范围内,这里只处理主屏)
        let x = x.max(0) as u32;
        let y = y.max(0) as u32;
        let width = width as u32;
        let height = height as u32;

        let joined = tokio::task::spawn_blocking(move || {
            let full = grab_primary_screen()?;
            let region = crop_region(&full, x, y, width, height)?;
            recognize_rgba(&region)
        })
        .await;
        blocking_result(joined)
    }
}

/// 从全屏图里裁出指定矩形(越界则与屏幕实际范围求交,完全越界则报错)。
fn crop_region(
    full: &image::RgbaImage,
    x: u32,
    y: u32,
    width: u32,
    height: u32,
) -> Result<image::RgbaImage, String> {
    let (full_w, full_h) = (full.width(), full.height());
    if x >= full_w || y >= full_h {
        return Err(format!(
            "区域左上角 ({x}, {y}) 超出主屏范围 {full_w}x{full_h}"
        ));
    }
    // 与屏幕范围求交,避免裁剪越界 panic
    let clamped_w = width.min(full_w - x);
    let clamped_h = height.min(full_h - y);
    // crop_imm 不修改原图,返回的子视图转成独立 RgbaImage
    Ok(image::imageops::crop_imm(full, x, y, clamped_w, clamped_h).to_image())
}

// ===================== Windows:WinRT Windows.Media.Ocr =====================

/// 把 RGBA 图交给系统 OCR 识别,返回纯文本。
#[cfg(windows)]
fn recognize_rgba(image: &image::RgbaImage) -> Result<String, String> {
    use image::ImageEncoder;
    use windows::Graphics::Imaging::BitmapDecoder;
    use windows::Media::Ocr::OcrEngine;
    use windows::Storage::Streams::{DataWriter, InMemoryRandomAccessStream};

    // 1) RgbaImage → 内存 PNG 字节(BitmapDecoder 能直接吃 PNG,免手工拼 BGRA 像素与跨步)
    let mut png: Vec<u8> = Vec::new();
    image::codecs::png::PngEncoder::new(&mut png)
        .write_image(
            image.as_raw(),
            image.width(),
            image.height(),
            image::ExtendedColorType::Rgba8,
        )
        .map_err(|e| format!("编码 PNG 失败: {e}"))?;

    // 2) PNG 字节 → InMemoryRandomAccessStream(经 DataWriter 写入并 flush,再把读指针归零)
    let stream =
        InMemoryRandomAccessStream::new().map_err(|e| format!("创建内存流失败: {e}"))?;
    let writer = DataWriter::CreateDataWriter(&stream)
        .map_err(|e| format!("创建 DataWriter 失败: {e}"))?;
    writer
        .WriteBytes(&png)
        .map_err(|e| format!("写入 PNG 字节失败: {e}"))?;
    writer
        .StoreAsync()
        .map_err(|e| format!("提交写入失败: {e}"))?
        .get()
        .map_err(|e| format!("等待写入完成失败: {e}"))?;
    writer
        .FlushAsync()
        .map_err(|e| format!("flush 失败: {e}"))?
        .get()
        .map_err(|e| format!("等待 flush 完成失败: {e}"))?;
    // 解绑 stream:DataWriter 持有 stream 句柄,后续解码要重新从头读,先把底层流读指针归零
    let _ = writer.DetachStream();
    stream
        .Seek(0)
        .map_err(|e| format!("重置流位置失败: {e}"))?;

    // 3) 流 → BitmapDecoder → SoftwareBitmap
    let decoder = BitmapDecoder::CreateAsync(&stream)
        .map_err(|e| format!("创建 BitmapDecoder 失败: {e}"))?
        .get()
        .map_err(|e| format!("解码图片失败: {e}"))?;
    let bitmap = decoder
        .GetSoftwareBitmapAsync()
        .map_err(|e| format!("获取 SoftwareBitmap 失败: {e}"))?
        .get()
        .map_err(|e| format!("等待 SoftwareBitmap 失败: {e}"))?;

    // 4) OCR 引擎:按用户系统语言创建。系统无可用 OCR 语言包时,WinRT 返回 null 指针,
    //    windows-rs 把它转成 Err(E_POINTER)——此时给「装语言包」的可操作提示,而非裸 HRESULT。
    let engine = OcrEngine::TryCreateFromUserProfileLanguages().map_err(|e| {
        format!(
            "无法创建系统 OCR 引擎(很可能未安装对应语言的 OCR 包): {e}。\
             请到 设置 → 时间和语言 → 语言和区域 → 对应语言「语言选项」中安装『光学字符识别 (OCR)』功能。"
        )
    })?;
    let result = engine
        .RecognizeAsync(&bitmap)
        .map_err(|e| format!("发起 OCR 识别失败: {e}"))?
        .get()
        .map_err(|e| format!("等待 OCR 结果失败: {e}"))?;
    let text = result
        .Text()
        .map_err(|e| format!("读取 OCR 文本失败: {e}"))?;
    Ok(text.to_string())
}

/// 非 Windows:系统 OCR 不可用(本模块依赖 WinRT Windows.Media.Ocr)。
#[cfg(not(windows))]
fn recognize_rgba(_image: &image::RgbaImage) -> Result<String, String> {
    Err("屏幕 OCR 仅 Windows 支持(依赖系统自带 Windows.Media.Ocr)".to_string())
}