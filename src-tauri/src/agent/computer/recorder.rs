//! 🎥 电脑操作 Agent 的屏幕录制:用 ffmpeg 录全屏视频 + 悬浮控制条。
//!
//! 交互流程(对应前端「录屏」按钮):点击 → 最小化主窗口到任务栏(让录屏不录进本程序界面)→
//! 起 ffmpeg 抓桌面写 MP4 → 弹出无边框置顶的悬浮窗(显示计时与「停止」)。停止时优雅结束 ffmpeg
//! (向其 stdin 写 `q` 触发写完 mp4 moov,否则文件不可播放)、关悬浮窗、还原主窗口,并通知主窗口弹保存提示。
//!
//! 复用项目既有的 ffmpeg 体系([[media::probe_ffmpeg]] / 配置里的 `media.ffmpeg_path`),不引入新依赖。
//! 仅录视频不录音频:跨平台音频设备枚举差异大且易失败,先保证最稳的视频录制(后续可扩展)。

use std::io::Write;
use std::path::Path;
use std::process::{Child, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::json;
use tauri::{AppHandle, Emitter, Manager, State, WebviewUrl, WebviewWindowBuilder};

use crate::commands::{lock_config, AppState};

/// 主窗口 label(与 lib.rs 中保持一致;最小化 / 还原目标)。
const MAIN_WINDOW_LABEL: &str = "main";
/// 录屏悬浮窗 label(前端 main.tsx 按此渲染 RecordingOverlay)。
pub const RECORDING_OVERLAY_LABEL: &str = "recording-overlay";

/// 悬浮窗逻辑尺寸(与前端 RecordingOverlay 容器一致;尽量小巧)。
/// 高度要比卡片本身略高,给居中卡片的圆角上下边留出空隙,否则边框会被窗口边缘裁掉(看不见上下线)。
const OVERLAY_W: f64 = 200.0;
const OVERLAY_H: f64 = 52.0;
/// 录制帧率:15fps 兼顾流畅度与 CPU / 文件体积。
const FRAMERATE: &str = "15";
/// 停止时等待 ffmpeg 正常收尾的上限,超时则强杀(避免界面卡在「停止中」)。
/// 取较宽:收尾除写 trailer 外还要做一次 faststart 重排(把 moov 移到文件头),长录制需要更多时间,
/// 若中途被杀会留下 moov 缺失/损坏的文件(时长 0:00)。
const STOP_GRACE: Duration = Duration::from_secs(20);

/// 一次进行中的录制会话。
struct RecordingSession {
    /// ffmpeg 子进程(stdin 已接管,用于优雅停止)。
    child: Child,
    /// 输出 MP4 绝对路径。
    output_path: std::path::PathBuf,
    /// 开始时间(Unix 秒),供悬浮窗计时。
    started_at: i64,
}

/// 录屏全局状态:同一时刻只允许一个录制会话。挂在 AppState 上跨命令共享。
pub struct RecordingState {
    inner: Mutex<Option<RecordingSession>>,
    /// ffmpeg 是否可用:程序启动时探测一次写入,后续录屏命令直接读此标记,不再每次启子进程探测。
    /// `check_ffmpeg` 命令(用户在设置里手动检测)也会刷新它,免重启即可生效。
    ffmpeg_available: AtomicBool,
}

impl RecordingState {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(None),
            ffmpeg_available: AtomicBool::new(false),
        }
    }

    /// 写入 ffmpeg 可用性标记(启动探测 / 手动检测后调用)。
    pub fn set_ffmpeg_available(&self, available: bool) {
        self.ffmpeg_available.store(available, Ordering::Relaxed);
    }

    /// 读取 ffmpeg 可用性标记(录屏命令据此放行,不再每次探测)。
    pub fn ffmpeg_available(&self) -> bool {
        self.ffmpeg_available.load(Ordering::Relaxed)
    }
}

impl Default for RecordingState {
    fn default() -> Self {
        Self::new()
    }
}

/// 录屏状态回传给前端(camelCase 对齐 TS RecordingStatus)。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordingStatus {
    /// 是否正在录制。
    pub recording: bool,
    /// 开始时间(Unix 秒),未录制为 null。
    pub started_at: Option<i64>,
    /// 输出文件路径,未录制为 null。
    pub output_path: Option<String>,
}

impl RecordingStatus {
    fn idle() -> Self {
        Self {
            recording: false,
            started_at: None,
            output_path: None,
        }
    }
}

/// 开始录屏:校验 ffmpeg → 最小化主窗口 → 起 ffmpeg → 弹悬浮窗。仅录视频,不录音频。
#[tauri::command]
pub async fn start_screen_recording(
    state: State<'_, AppState>,
    app: AppHandle,
) -> std::result::Result<RecordingStatus, String> {
    // 已在录制:直接返回当前状态,避免起第二个 ffmpeg
    {
        let guard = state
            .recording
            .inner
            .lock()
            .map_err(|_| "录屏状态锁异常".to_string())?;
        if guard.is_some() {
            return Err("已经在录制中".to_string());
        }
    }

    // 可用性走启动时探测的标记,不再每次启子进程探测
    if !state.recording.ffmpeg_available() {
        return Err(
            "未检测到 ffmpeg,无法录屏。请先安装 ffmpeg,或在「系统配置」中设置 ffmpeg 路径。"
                .to_string(),
        );
    }
    // 解析 ffmpeg 路径(配置为空则用系统 PATH 的 ffmpeg),供下方拼命令实际运行
    let ffmpeg_path = {
        let cfg = lock_config(&state).map_err(|e| e.to_string())?;
        cfg.media.ffmpeg_path.clone()
    };

    // 输出目录:<app_data>/recordings/recording-<时间戳>.mp4
    let dir = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("定位数据目录失败: {e}"))?
        .join("recordings");
    std::fs::create_dir_all(&dir).map_err(|e| format!("创建录屏目录失败: {e}"))?;
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
    let output_path = dir.join(format!("recording-{stamp}.mp4"));

    // 先最小化主窗口,让随后录到的画面不含本程序
    if let Some(main) = app.get_webview_window(MAIN_WINDOW_LABEL) {
        let _ = main.minimize();
    }

    // 起 ffmpeg(stdin 接管以便后续写 'q' 优雅停止;输出丢弃)
    let program = ffmpeg_path
        .as_deref()
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .unwrap_or("ffmpeg")
        .to_string();
    // 水印第二行:录制开始时间(年-月-日 时:分)
    let watermark_time = chrono::Local::now().format("%Y-%m-%d %H:%M").to_string();
    let mut cmd = build_ffmpeg_command(&program, &output_path, &watermark_time);
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let child = cmd
        .spawn()
        .map_err(|e| format!("启动 ffmpeg 录屏失败: {e}"))?;
    let started_at = chrono::Local::now().timestamp();

    // 存会话;若期间已被别的调用抢先(竞态),杀掉本次新进程并报错,避免出现两个录制
    {
        let mut guard = state
            .recording
            .inner
            .lock()
            .map_err(|_| "录屏状态锁异常".to_string())?;
        if guard.is_some() {
            let mut child = child;
            let _ = child.kill();
            let _ = child.wait();
            return Err("已经在录制中".to_string());
        }
        *guard = Some(RecordingSession {
            child,
            output_path: output_path.clone(),
            started_at,
        });
    }

    // 弹悬浮控制条(失败不影响录制本身,仅记日志)
    if let Err(e) = open_overlay(&app) {
        tracing::warn!("创建录屏悬浮窗失败: {e}");
    }

    Ok(RecordingStatus {
        recording: true,
        started_at: Some(started_at),
        output_path: Some(output_path.to_string_lossy().to_string()),
    })
}

/// 停止录屏:优雅结束 ffmpeg(写完 MP4)→ 关悬浮窗 → 还原主窗口 → 通知主窗口弹保存提示。
/// 幂等:即使当前无录制也会清理悬浮窗 / 还原主窗口。
#[tauri::command]
pub async fn stop_screen_recording(
    state: State<'_, AppState>,
    app: AppHandle,
) -> std::result::Result<RecordingStatus, String> {
    // 取出会话置空(不跨 await 持锁)
    let session = {
        let mut guard = state
            .recording
            .inner
            .lock()
            .map_err(|_| "录屏状态锁异常".to_string())?;
        guard.take()
    };

    let Some(session) = session else {
        close_overlay(&app);
        restore_main(&app);
        return Ok(RecordingStatus::idle());
    };

    let output_path = session.output_path.clone();
    // 优雅停止 + 阻塞 wait,放 blocking 线程
    tauri::async_runtime::spawn_blocking(move || finalize_ffmpeg(session))
        .await
        .map_err(|e| format!("停止录屏异常: {e}"))?;

    close_overlay(&app);
    restore_main(&app);

    // 通知主窗口(其 Toaster 才能弹提示并提供「打开所在文件夹」)
    let _ = app.emit_to(
        MAIN_WINDOW_LABEL,
        "recording-saved",
        json!({ "path": output_path.to_string_lossy() }),
    );

    Ok(RecordingStatus {
        recording: false,
        started_at: None,
        output_path: Some(output_path.to_string_lossy().to_string()),
    })
}

/// 打开录屏悬浮控制条(**不立即开始录制**):录制由悬浮条上的「开始」按钮手动触发。
/// 先预检 ffmpeg(主窗口能弹 toast 引导),不可用就不弹悬浮窗。此时不最小化主窗口(按下「开始」才最小化)。
#[tauri::command]
pub async fn open_recording_overlay(
    state: State<'_, AppState>,
    app: AppHandle,
) -> std::result::Result<(), String> {
    // 先弹出悬浮窗(独立轻量入口,秒开)。可用性走启动时探测的标记,不再每次启子进程探测。
    open_overlay(&app).map_err(|e| format!("打开录屏悬浮窗失败: {e}"))?;
    // ffmpeg 不可用:撤掉悬浮窗 + 还原主窗口,并返回错误(主窗口 hook 据此弹 toast 引导)
    if !state.recording.ffmpeg_available() {
        close_overlay(&app);
        restore_main(&app);
        return Err(
            "未检测到 ffmpeg,无法录屏。请先安装 ffmpeg,或在「系统配置」中设置 ffmpeg 路径。"
                .to_string(),
        );
    }
    Ok(())
}

/// 取消录屏(尚未开始录制时):关闭悬浮窗并还原主窗口,不产出文件。
/// 录制进行中则忽略(此时悬浮条只给「停止」,应走 stop_screen_recording)。
#[tauri::command]
pub fn cancel_recording_overlay(state: State<'_, AppState>, app: AppHandle) {
    let is_recording = state
        .recording
        .inner
        .lock()
        .map(|g| g.is_some())
        .unwrap_or(false);
    if is_recording {
        return;
    }
    close_overlay(&app);
    restore_main(&app);
}

/// 查询当前录屏状态(主页面 / 悬浮窗轮询用)。
#[tauri::command]
pub fn get_recording_status(state: State<'_, AppState>) -> RecordingStatus {
    match state.recording.inner.lock() {
        Ok(guard) => match guard.as_ref() {
            Some(s) => RecordingStatus {
                recording: true,
                started_at: Some(s.started_at),
                output_path: Some(s.output_path.to_string_lossy().to_string()),
            },
            None => RecordingStatus::idle(),
        },
        Err(_) => RecordingStatus::idle(),
    }
}

/// 按平台拼 ffmpeg 录屏命令(全屏 + libx264 + 两行水印)。仅录视频,不录音频。
/// Windows 用 gdigrab(首要支持);macOS 用 avfoundation(需『屏幕录制』权限,设备索引因机器而异,未实机验证);Linux 用 x11grab。
fn build_ffmpeg_command(
    program: &str,
    output: &Path,
    watermark_time: &str,
) -> std::process::Command {
    let mut cmd = std::process::Command::new(program);
    cmd.arg("-y"); // 覆盖同名输出,避免交互确认卡住

    // 视频输入(全屏;不采集音频)
    #[cfg(windows)]
    {
        cmd.args(["-f", "gdigrab", "-framerate", FRAMERATE, "-i", "desktop"]);
    }
    #[cfg(target_os = "macos")]
    {
        // avfoundation:屏幕 0,音频置 none(只录画面)。索引可能因机器而异。
        cmd.args([
            "-f",
            "avfoundation",
            "-framerate",
            FRAMERATE,
            "-i",
            "Capture screen 0:none",
        ]);
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        cmd.args(["-f", "x11grab", "-framerate", FRAMERATE, "-i", ":0.0"]);
    }

    // 视频滤镜:偶数尺寸(yuv420p 要求)+ 两行水印
    cmd.args(["-vf", &build_video_filter(watermark_time)]);
    // 编码:视频 ultrafast 降 CPU
    cmd.args(["-c:v", "libx264", "-preset", "ultrafast", "-pix_fmt", "yuv420p"]);
    // moov 原子移到文件头(收尾时做一次 faststart 重排):否则 <video> 经 asset 协议读不到时长、
    // 显示 0:00 且不可拖动。必须放在 output 之前。
    cmd.args(["-movflags", "+faststart"]);
    cmd.arg(output);
    cmd
}

/// 拼视频滤镜:偶数尺寸 + 两行水印(第一行 VeltrixLoop,第二行录制开始时间)。
/// 需 ffmpeg 带 libfreetype(drawtext);字体用各平台系统自带(水印为 ASCII,无需中文字体)。
/// Windows 字体路径里的盘符冒号要转义为 `C\:`(filtergraph 的选项分隔符也是冒号)。
fn build_video_filter(watermark_time: &str) -> String {
    #[cfg(windows)]
    let font = "C\\:/Windows/Fonts/arial.ttf";
    #[cfg(target_os = "macos")]
    let font = "/System/Library/Fonts/Supplemental/Arial.ttf";
    #[cfg(all(unix, not(target_os = "macos")))]
    let font = "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf";
    format!(
        "scale=trunc(iw/2)*2:trunc(ih/2)*2,\
         drawtext=fontfile={font}:text='VeltrixLoop':x=24:y=22:fontsize=26:fontcolor=white:borderw=2:bordercolor=black@0.6,\
         drawtext=fontfile={font}:text='{watermark_time}':x=24:y=58:fontsize=20:fontcolor=white@0.85:borderw=2:bordercolor=black@0.6"
    )
}

/// Windows:把悬浮窗排除出屏幕捕获(WDA_EXCLUDEFROMCAPTURE)——用户仍看得到,但 gdigrab 录不进去。
/// 需 Win10 2004+;旧系统调用失败则忽略(此时悬浮窗会被录进去,但不影响录制本身)。
#[cfg(windows)]
fn exclude_overlay_from_capture(overlay: &tauri::WebviewWindow) {
    use windows::Win32::UI::WindowsAndMessaging::{
        SetWindowDisplayAffinity, WDA_EXCLUDEFROMCAPTURE,
    };
    if let Ok(hwnd) = overlay.hwnd() {
        unsafe {
            let _ = SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE);
        }
    }
}

/// 优雅停止 ffmpeg:向 stdin 写 `q` 让其写完 MP4 moov;超时未退则强杀。
fn finalize_ffmpeg(mut session: RecordingSession) {
    // 关键:gdigrab→mp4 必须让 ffmpeg 自己收尾,直接 kill 会留下缺 moov 的废文件
    if let Some(mut stdin) = session.child.stdin.take() {
        let _ = stdin.write_all(b"q");
        let _ = stdin.flush();
        drop(stdin); // 关闭管道,促使 ffmpeg 退出
    }
    let deadline = Instant::now() + STOP_GRACE;
    loop {
        match session.child.try_wait() {
            Ok(Some(_)) => break, // 正常退出
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = session.child.kill();
                    let _ = session.child.wait();
                    break;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(_) => {
                let _ = session.child.kill();
                break;
            }
        }
    }
}

/// 创建(或显示)录屏悬浮窗:无边框 / 透明 / 不进任务栏 / 置顶,放主显示器顶部居中。
fn open_overlay(app: &AppHandle) -> tauri::Result<()> {
    if let Some(w) = app.get_webview_window(RECORDING_OVERLAY_LABEL) {
        let _ = w.show();
        let _ = w.set_focus();
        return Ok(());
    }
    // 加载独立轻量入口(recording-overlay.html),而非整个应用(index.html),避免弹窗等好几秒
    let overlay = WebviewWindowBuilder::new(
        app,
        RECORDING_OVERLAY_LABEL,
        WebviewUrl::App("recording-overlay.html".into()),
    )
    .title("录屏")
    .inner_size(OVERLAY_W, OVERLAY_H)
    .decorations(false)
    .transparent(true)
    .skip_taskbar(true)
    .always_on_top(true)
    .resizable(false)
    .shadow(false)
    .visible(true)
    .build()?;
    position_overlay(&overlay);
    // 把悬浮条排除出屏幕捕获,使其不被录进视频(Windows)
    #[cfg(windows)]
    exclude_overlay_from_capture(&overlay);
    Ok(())
}

/// 把悬浮窗摆到主显示器顶部居中(高 DPI 下用缩放因子换算物理像素)。
fn position_overlay(overlay: &tauri::WebviewWindow) {
    let scale = overlay.scale_factor().unwrap_or(1.0);
    let w = OVERLAY_W * scale;
    if let Ok(Some(monitor)) = overlay.primary_monitor() {
        let pos = *monitor.position();
        let size = *monitor.size();
        let x = pos.x as f64 + (size.width as f64 - w) / 2.0;
        let y = pos.y as f64 + 16.0 * scale;
        let _ = overlay.set_position(tauri::PhysicalPosition::new(x, y));
    }
}

/// 关闭录屏悬浮窗(不存在则忽略)。
fn close_overlay(app: &AppHandle) {
    if let Some(w) = app.get_webview_window(RECORDING_OVERLAY_LABEL) {
        let _ = w.close();
    }
}

/// 还原并聚焦主窗口(录屏结束回到应用)。
fn restore_main(app: &AppHandle) {
    if let Some(main) = app.get_webview_window(MAIN_WINDOW_LABEL) {
        let _ = main.unminimize();
        let _ = main.show();
        let _ = main.set_focus();
    }
}
