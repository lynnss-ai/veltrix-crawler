//! 智能剪辑场景检测：按需调用安装包内置 FFmpeg，避免主进程启动时加载整套 OpenCV DLL。
use crate::commands::{lock_config, AppState};
use tauri::State;
use veltrix_core::error::{CrawlerError, Result};

/// 按 2fps 采样并使用 FFmpeg scene score 检测切点；合并 1 秒内近邻切点。
/// threshold 缺省 0.5（0~1，越大越保守），与原 OpenCV 入口保持同一交互口径。
#[tauri::command]
pub async fn creation_detect_scenes(
    state: State<'_, AppState>,
    input_path: String,
    threshold: Option<f64>,
) -> Result<Vec<f64>> {
    let program = {
        let cfg = lock_config(&state).map_err(|e| CrawlerError::Config(e.to_string()))?;
        cfg.media
            .ffmpeg_path
            .as_deref()
            .map(str::trim)
            .filter(|path| !path.is_empty())
            .unwrap_or("ffmpeg")
            .to_string()
    };
    let threshold = threshold.unwrap_or(0.5).clamp(0.01, 0.99);
    tauri::async_runtime::spawn_blocking(move || {
        detect_scenes_impl(&program, &input_path, threshold)
    })
    .await
    .map_err(|e| CrawlerError::Config(format!("场景检测异常: {e}")))?
}

fn detect_scenes_impl(program: &str, input_path: &str, threshold: f64) -> Result<Vec<f64>> {
    const MERGE_GAP_SECS: f64 = 1.0;
    let input = std::path::Path::new(input_path);
    if !input.is_file() {
        return Err(CrawlerError::Config(format!("源视频不存在: {input_path}")));
    }

    let filter = format!("fps=2,select=gt(scene\\,{threshold:.6}),showinfo");
    let mut cmd = std::process::Command::new(program);
    crate::media::hide_console_window(&mut cmd);
    let output = cmd
        .args(["-hide_banner", "-nostdin", "-i"])
        .arg(input)
        .args(["-an", "-vf", &filter, "-f", "null", "-"])
        .output()
        .map_err(|e| CrawlerError::Config(format!("启动场景检测失败: {e}")))?;
    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr);
        let last = detail
            .lines()
            .rev()
            .find(|line| !line.trim().is_empty())
            .unwrap_or("未知错误")
            .trim();
        return Err(CrawlerError::Config(format!("场景检测失败: {last}")));
    }

    let stderr = String::from_utf8_lossy(&output.stderr);
    let mut cuts = Vec::new();
    for line in stderr.lines().filter(|line| line.contains("showinfo")) {
        let Some(rest) = line.split("pts_time:").nth(1) else {
            continue;
        };
        let Some(raw) = rest.split_whitespace().next() else {
            continue;
        };
        let Ok(time) = raw.parse::<f64>() else {
            continue;
        };
        if time > 0.01
            && cuts
                .last()
                .map(|last| time - last > MERGE_GAP_SECS)
                .unwrap_or(true)
        {
            cuts.push(time);
        }
    }
    Ok(cuts)
}

// 集成测试：testsrc(3s)+纯红(3s)拼接视频，场景切换点应落在 3s 附近。
// ffmpeg 不可用或样片生成失败时跳过。
#[cfg(test)]
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
            .args([
                "-f",
                "lavfi",
                "-i",
                "testsrc=duration=3:size=160x90:rate=30",
            ])
            .args([
                "-f",
                "lavfi",
                "-i",
                "color=red:duration=3:size=160x90:rate=30",
            ])
            .args([
                "-filter_complex",
                "[0:v][1:v]concat=n=2:v=1[v]",
                "-map",
                "[v]",
            ])
            .args(["-c:v", "libx264", "-pix_fmt", "yuv420p"])
            .arg(&video)
            .output()
            .expect("生成测试视频失败");
        if !status.status.success() {
            eprintln!("skip: 测试视频生成失败");
            let _ = std::fs::remove_dir_all(&dir);
            return;
        }
        let cuts =
            super::detect_scenes_impl(&ffmpeg.to_string_lossy(), &video.to_string_lossy(), 0.35)
                .expect("场景检测应成功");
        assert!(
            cuts.iter().any(|time| *time > 2.0 && *time < 4.5),
            "应在 3s 附近检测到切点: {cuts:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
