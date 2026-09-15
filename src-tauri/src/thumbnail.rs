//! 素材缩略图生成。
//!
//! 约定:缩略图与源文件同目录,命名为「源文件名去扩展名 + `_thumb.jpg`」
//! (如 `xx_cover.jpg` → `xx_cover_thumb.jpg`),宽 480px 等比缩放(原图更窄不放大),
//! JPEG 质量 80。前端瀑布流卡片只加载缩略图,避免解码原图卡顿。
//!
//! 两个生成入口:素材下载成功后立即生成(media/mod.rs),以及文件服务惰性生成
//! (file_server.rs,兼容存量数据)——两处都汇聚到本模块,保证规格只有一份。

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

/// 缩略图目标宽度:瀑布流卡片约 300px 宽,480 兼顾高分屏缩放,再大收益递减、体积陡增
pub const THUMB_WIDTH: u32 = 480;
/// JPEG 质量 80:画质 / 体积平衡点,缩略图场景再低会出现可见块效应
const THUMB_QUALITY: u8 = 80;
/// 源图体积上限:超过则跳过缩略图(调用方回源)。解码大图内存峰值约为宽×高×4,
/// 50MB 的 PNG 可展开成数 GB 位图,必须按落盘体积先挡一道
const MAX_THUMB_SRC_BYTES: u64 = 20 * 1024 * 1024;

/// 临时文件序号:同进程并发请求同一缩略图时,pid 无法区分线程,
/// 叠加进程内自增序号保证临时文件名唯一
static TMP_SEQ: AtomicU64 = AtomicU64::new(0);

/// 由源文件路径推导缩略图路径:`xx_cover.jpg` → `xx_cover_thumb.jpg`
pub fn thumb_path_for(source: &Path) -> PathBuf {
    let stem = source
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    source.with_file_name(format!("{stem}_thumb.jpg"))
}

/// 确保缩略图存在:已存在直接返回;不存在则解码源图、缩放、落盘后返回。
/// 解码 / 缩放在阻塞线程池执行,不占异步运行时工作线程。
/// 失败(源图损坏、超限、IO 错误)仅记日志并返回 None,由调用方决定回源或跳过——
/// 缩略图是性能优化产物,不应阻断素材主流程。
pub async fn ensure(source: PathBuf) -> Option<PathBuf> {
    match tokio::task::spawn_blocking(move || ensure_blocking(&source)).await {
        Ok(result) => result,
        Err(e) => {
            tracing::warn!("生成缩略图任务异常: {e}");
            None
        }
    }
}

fn ensure_blocking(source: &Path) -> Option<PathBuf> {
    let thumb = thumb_path_for(source);
    if thumb.is_file() {
        return Some(thumb);
    }
    let size = std::fs::metadata(source).ok()?.len();
    if size > MAX_THUMB_SRC_BYTES {
        tracing::debug!(path = %source.display(), bytes = size, "源图超过缩略图体积上限,跳过生成");
        return None;
    }
    // 按魔数嗅探真实格式:部分平台(如抖音图集)直接返回 WebP,落盘却按约定命名 .jpg,
    // 用 image::open 按扩展名选解码器会报 "Illegal start bytes"(5249 = "RI" 即 RIFF/WebP 头)
    let img = match image::ImageReader::open(source).and_then(|reader| reader.with_guessed_format())
    {
        Ok(reader) => match reader.decode() {
            Ok(img) => img,
            Err(e) => {
                tracing::warn!(path = %source.display(), "解码源图失败,跳过缩略图: {e}");
                return None;
            }
        },
        Err(e) => {
            tracing::warn!(path = %source.display(), "打开源图失败,跳过缩略图: {e}");
            return None;
        }
    };
    // resize 保持宽高比:目标宽 480,高度给 u32::MAX 表示只按宽约束;原图更窄则不放大
    let resized = if img.width() > THUMB_WIDTH {
        img.resize(THUMB_WIDTH, u32::MAX, image::imageops::FilterType::Triangle)
    } else {
        img
    };
    // 先写临时文件再 rename:崩溃 / 并发不会留下半截 .jpg 被当成完整产物
    let tmp = thumb.with_file_name(format!(
        "{}.tmp-{}-{}",
        thumb.file_name()?.to_string_lossy(),
        std::process::id(),
        TMP_SEQ.fetch_add(1, Ordering::Relaxed),
    ));
    let encode_result = std::fs::File::create(&tmp)
        .map_err(|e| e.to_string())
        .and_then(|mut file| {
            let mut encoder =
                image::codecs::jpeg::JpegEncoder::new_with_quality(&mut file, THUMB_QUALITY);
            encoder.encode_image(&resized).map_err(|e| e.to_string())
        });
    if let Err(e) = encode_result {
        tracing::warn!(path = %source.display(), "编码缩略图失败: {e}");
        let _ = std::fs::remove_file(&tmp);
        return None;
    }
    match std::fs::rename(&tmp, &thumb) {
        Ok(()) => Some(thumb),
        // Windows 上 rename 不能覆盖已存在文件:并发生成时另一个请求已先落盘,复用其产物即可
        Err(_) if thumb.is_file() => {
            let _ = std::fs::remove_file(&tmp);
            Some(thumb)
        }
        Err(e) => {
            tracing::warn!(path = %source.display(), "缩略图落盘失败: {e}");
            let _ = std::fs::remove_file(&tmp);
            None
        }
    }
}

/// 存量回填:启动后后台扫描 media_root,给缺缩略图的素材补生成。
/// 没有它,存量数据的首张解码成本全压在用户浏览路径上(惰性生成);回填把它挪到启动空闲期。
/// 限流 4 路并发,避免与采集窗口 / 前端解码抢 CPU;失败项留给文件服务惰性兜底,不重试。
pub async fn backfill_missing(root: PathBuf) {
    let sources = match tokio::task::spawn_blocking(move || collect_missing(&root)).await {
        Ok(sources) => sources,
        Err(e) => {
            tracing::warn!("扫描存量素材失败,跳过缩略图回填: {e}");
            return;
        }
    };
    if sources.is_empty() {
        return;
    }
    let total = sources.len();
    let semaphore = Arc::new(tokio::sync::Semaphore::new(4));
    let mut set = tokio::task::JoinSet::new();
    for source in sources {
        // 入队前取信号量形成背压:JoinSet 不会随素材量无限膨胀
        let Ok(permit) = semaphore.clone().acquire_owned().await else {
            break;
        };
        set.spawn(async move {
            let _permit = permit;
            ensure(source).await;
        });
    }
    while set.join_next().await.is_some() {}
    tracing::info!("存量素材缩略图回填完成: {total} 张");
}

/// 递归收集缺缩略图的图片素材:仅常见图片扩展名,排除缩略图自身与生成中的临时文件
fn collect_missing(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_dir() {
                stack.push(path);
                continue;
            }
            if !file_type.is_file() {
                continue;
            }
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            let lower = name.to_ascii_lowercase();
            let is_image = ["jpg", "jpeg", "png", "webp"]
                .iter()
                .any(|ext| lower.ends_with(&format!(".{ext}")));
            if !is_image || lower.ends_with("_thumb.jpg") || lower.contains(".tmp-") {
                continue;
            }
            if !thumb_path_for(&path).is_file() {
                out.push(path);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::thumb_path_for;
    use std::path::Path;

    #[test]
    fn thumb_path_replaces_extension_with_suffix() {
        assert_eq!(
            thumb_path_for(Path::new("a/b/xx_cover.jpg")),
            Path::new("a/b/xx_cover_thumb.jpg")
        );
        assert_eq!(
            thumb_path_for(Path::new("a/xx_img0.png")),
            Path::new("a/xx_img0_thumb.jpg")
        );
    }
}
