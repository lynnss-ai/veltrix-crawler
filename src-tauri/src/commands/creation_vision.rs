//! 智能剪辑(计算机视觉):基于 OpenCV 的场景检测切分。
//! 仅 Windows 接入(OpenCV 预编译包路径见 .cargo/config.toml [env]);其它平台返回未支持。
use veltrix_core::error::{CrawlerError, Result};

/// 场景检测切点:按 2fps 采样,相邻帧灰度直方图 Bhattacharyya 距离超阈值记切点,
/// 合并 1s 内近邻切点,返回切点秒数(升序)。threshold 缺省 0.5(0~1,越大越保守)。
#[tauri::command]
pub async fn creation_detect_scenes(
    input_path: String,
    threshold: Option<f64>,
) -> Result<Vec<f64>> {
    let t = threshold.unwrap_or(0.5);
    tauri::async_runtime::spawn_blocking(move || detect_scenes_impl(&input_path, t))
        .await
        .map_err(|e| CrawlerError::Config(format!("场景检测异常: {e}")))?
}

/// 降采样到 160x90 后算 64bin 灰度直方图(L1 归一化,供 compare_hist)。
#[cfg(windows)]
fn gray_hist(gray: &opencv::core::Mat) -> Result<opencv::core::Mat> {
    use opencv::{core, imgproc};
    let images: core::Vector<core::Mat> = core::Vector::from(vec![gray.clone()]);
    let mut hist = core::Mat::default();
    imgproc::calc_hist(
        &images,
        &core::Vector::from(vec![0]),
        &core::Mat::default(),
        &mut hist,
        &core::Vector::from(vec![64]),
        &core::Vector::from(vec![0.0, 256.0]),
        false,
    )
    .map_err(|e| CrawlerError::Parse(format!("直方图计算失败: {e}")))?;
    let mut normed = core::Mat::default();
    core::normalize(
        &hist,
        &mut normed,
        1.0,
        0.0,
        core::NORM_L1,
        -1,
        &core::Mat::default(),
    )
    .map_err(|e| CrawlerError::Parse(format!("直方图归一化失败: {e}")))?;
    Ok(normed)
}

#[cfg(windows)]
fn detect_scenes_impl(input_path: &str, threshold: f64) -> Result<Vec<f64>> {
    use opencv::{core, imgproc, prelude::*, videoio};
    const SAMPLE_FPS: f64 = 2.0;
    const MERGE_GAP_SECS: f64 = 1.0;
    if !std::path::Path::new(input_path).is_file() {
        return Err(CrawlerError::Config(format!("源视频不存在: {input_path}")));
    }
    let mut cap = videoio::VideoCapture::from_file(input_path, videoio::CAP_ANY)
        .map_err(|e| CrawlerError::Config(format!("打开视频失败: {e}")))?;
    if !cap.is_opened().unwrap_or(false) {
        return Err(CrawlerError::Config(format!("无法打开视频: {input_path}")));
    }
    let fps = cap.get(videoio::CAP_PROP_FPS).unwrap_or(0.0);
    let fps = if fps > 0.0 { fps } else { 30.0 };
    let step = (fps / SAMPLE_FPS).max(1.0).round() as i64;
    let mut cuts: Vec<f64> = Vec::new();
    let mut prev_hist: Option<core::Mat> = None;
    let mut frame = core::Mat::default();
    let mut frame_idx: i64 = 0;
    loop {
        if !cap.read(&mut frame).unwrap_or(false) || frame.empty() {
            break;
        }
        if frame_idx % step == 0 {
            let mut small = core::Mat::default();
            imgproc::resize(
                &frame,
                &mut small,
                core::Size::new(160, 90),
                0.0,
                0.0,
                imgproc::INTER_AREA,
            )
            .map_err(|e| CrawlerError::Parse(format!("缩放采样帧失败: {e}")))?;
            let mut gray = core::Mat::default();
            imgproc::cvt_color(&small, &mut gray, imgproc::COLOR_BGR2GRAY, 0)
                .map_err(|e| CrawlerError::Parse(format!("灰度转换失败: {e}")))?;
            let hist = gray_hist(&gray)?;
            if let Some(prev) = &prev_hist {
                let dist = imgproc::compare_hist(prev, &hist, imgproc::HISTCMP_BHATTACHARYYA)
                    .map_err(|e| CrawlerError::Parse(format!("直方图对比失败: {e}")))?;
                if dist >= threshold {
                    let t = frame_idx as f64 / fps;
                    if cuts.last().map(|last| t - last > MERGE_GAP_SECS).unwrap_or(true) {
                        cuts.push(t);
                    }
                }
            }
            prev_hist = Some(hist);
        }
        frame_idx += 1;
    }
    Ok(cuts)
}

#[cfg(not(windows))]
fn detect_scenes_impl(_input_path: &str, _threshold: f64) -> Result<Vec<f64>> {
    Err(CrawlerError::Config(
        "当前平台暂未接入 OpenCV 场景检测".into(),
    ))
}

// 集成测试:testsrc(3s)+ 纯红(3s)拼接视频,场景切换点应落在 3s 附近。
// ffmpeg 不可用或样片生成失败时跳过。
#[cfg(all(test, windows))]
mod tests {
    #[test]
    fn detect_scene_change() {
        let dir = std::env::temp_dir().join(format!("veltrix-cv-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let video = dir.join("scenes.mp4");
        let ffmpeg = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("resources")
            .join("ffmpeg.exe");
        let status = std::process::Command::new(&ffmpeg)
            .arg("-y")
            .args(["-f", "lavfi", "-i", "testsrc=duration=3:size=160x90:rate=30"])
            .args(["-f", "lavfi", "-i", "color=red:duration=3:size=160x90:rate=30"])
            .args(["-filter_complex", "[0:v][1:v]concat=n=2:v=1[v]", "-map", "[v]"])
            .args(["-c:v", "libx264", "-pix_fmt", "yuv420p"])
            .arg(&video)
            .output()
            .expect("生成测试视频失败");
        if !status.status.success() {
            eprintln!("skip: 测试视频生成失败");
            let _ = std::fs::remove_dir_all(&dir);
            return;
        }
        let cuts = super::detect_scenes_impl(&video.to_string_lossy(), 0.5).expect("场景检测应成功");
        assert!(
            cuts.iter().any(|t| *t > 2.0 && *t < 4.5),
            "应在 3s 附近检测到切点: {cuts:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
