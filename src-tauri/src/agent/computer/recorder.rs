//! 🎥 电脑操作 Agent 的屏幕录制:用 ffmpeg 录全屏视频 + 悬浮控制条。
//!
//! 交互流程(对应前端「录屏」按钮):点击 → (默认)最小化主窗口到任务栏(让录屏不录进本程序界面;
//! 悬浮条上开「含本程序」则跳过最小化,本程序的操作一并入镜)→ 起 ffmpeg 抓桌面写 MP4 →
//! 弹出无边框置顶的悬浮窗(显示计时与「暂停 / 停止」;悬浮窗自身已排除出捕获,不会入镜)。
//!
//! 暂停 / 继续 = 分段录制:ffmpeg 本身不支持暂停,暂停即优雅结束当前段(写完 moov),
//! 继续即起新进程录下一段;停止时把各段用 `-c copy` 拼接(不重编码),再后台做 faststart 重排。
//! 停止时优雅结束 ffmpeg(向其 stdin 写 `q` 触发写完 mp4 moov,否则文件不可播放)、关悬浮窗、
//! 还原主窗口,并通知主窗口弹保存提示。
//!
//! 复用项目既有的 ffmpeg 体系([[media::probe_ffmpeg]] / 配置里的 `media.ffmpeg_path`),不引入新依赖。
//! 音频默认开(悬浮条可关):Windows 用 dshow 采麦克风,滤镜链做降噪/动态放大;枚举不到麦克风
//! 或设备打开失败时自动降级为纯视频录制(麦克风问题不应拖垮录屏)。macOS/Linux 暂不采音频。

use std::io::Write;
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
/// 宽度 260:开始 + 麦克风开关 + 含本程序开关 + 屏幕选择 + 取消 五个控件。
const OVERLAY_W: f64 = 260.0;
const OVERLAY_H: f64 = 52.0;
/// 设置面板展开时窗口增加的高度(含本程序开关 + 音频设备选择 + 音频测试)。
const PANEL_H: f64 = 220.0;
/// 预览确认态的悬浮窗逻辑尺寸(平铺各屏缩略图,点开始录制前选择)。
const PREVIEW_W: f64 = 600.0;
const PREVIEW_H: f64 = 540.0;
/// 单屏预览缩略图最大宽度(物理像素),超出等比缩小:缩略图不需要原尺寸,控制 base64 载荷。
const PREVIEW_MAX_W: u32 = 480;
/// 录制帧率:15fps 兼顾流畅度与 CPU / 文件体积。
const FRAMERATE: &str = "15";
/// 停止时等待 ffmpeg 正常收尾的上限,超时则强杀(避免界面卡在「停止中」)。
/// 收尾只写 trailer + 刷缓冲(faststart 重排已挪到停止成功后后台做),通常很快;
/// 若中途被杀会留下 moov 缺失/损坏的文件(时长 0:00)。
const STOP_GRACE: Duration = Duration::from_secs(20);

/// 一次进行中的录制会话(支持暂停 / 继续:暂停 = 优雅收尾当前段,继续 = 起新分段)。
struct RecordingSession {
    /// 当前分段的 ffmpeg 子进程(stdin 已接管,用于优雅停止 / 暂停);暂停时为 None。
    child: Option<Child>,
    /// 已优雅收尾并校验有效的分段文件(暂停切出的);停止时与当前段一起拼接
    segments: Vec<std::path::PathBuf>,
    /// 当前分段输出路径(录制中 = 正在写的文件;暂停时 = 最近一次分段路径)
    current_path: std::path::PathBuf,
    /// 输出目录与文件名干,分段命名用:首段 {stem}.mp4,后续段 {stem}.part{N}.mp4
    dir: std::path::PathBuf,
    stem: String,
    /// 下一个分段号(首段为 1 且无后缀,从 2 开始带 .partN)
    next_part: u32,
    /// 开始时间(Unix 秒),展示用。
    started_at: i64,
    /// 本次是否在采麦克风(悬浮窗录制中状态据此显示麦克风指示;启动后不可改,ffmpeg 输入已固定)。
    with_mic: bool,
    /// 本次录制的屏幕下标;None = 全部屏幕。
    screen_index: Option<u32>,
    /// 恢复录制时重建 ffmpeg 命令所需的材料(与 start 时一致,分段参数完全相同才能 -c copy 拼接)
    program: String,
    mic: Option<String>,
    region: Option<ScreenRegion>,
    encoder: VideoEncoder,
    ddagrab: bool,
    /// 计时:累计活跃时长 + 本次活跃起点(暂停时 None);显示时长 = 两者之和
    active_elapsed: Duration,
    active_since: Option<Instant>,
    /// 当前分段 ffmpeg stderr 尾部(draining 线程持续写入,只留最后几 KB),失败时摘进错误信息。
    stderr_tail: std::sync::Arc<Mutex<String>>,
}

impl RecordingSession {
    /// 是否处于暂停态(无活跃 ffmpeg 进程)。
    fn is_paused(&self) -> bool {
        self.child.is_none()
    }

    /// 已录制的活跃秒数(不含暂停时段)。
    fn elapsed_secs(&self) -> f64 {
        let mut d = self.active_elapsed;
        if let Some(since) = self.active_since {
            d += since.elapsed();
        }
        d.as_secs_f64()
    }

    /// 生成下一段的输出路径并推进分段号。
    fn next_segment_path(&mut self) -> std::path::PathBuf {
        let name = if self.next_part == 1 {
            format!("{}.mp4", self.stem)
        } else {
            format!("{}.part{}.mp4", self.stem, self.next_part)
        };
        self.next_part += 1;
        self.dir.join(name)
    }

    /// 组装回传前端的录制状态。
    fn status(&self) -> RecordingStatus {
        RecordingStatus {
            recording: true,
            with_mic: self.with_mic,
            screen_index: self.screen_index,
            started_at: Some(self.started_at),
            output_path: Some(self.current_path.to_string_lossy().to_string()),
            paused: self.is_paused(),
            elapsed_secs: self.elapsed_secs(),
        }
    }
}

/// 接管 ffmpeg stderr:后台线程持续排空(防管道写满卡死 ffmpeg),只保留尾部若干 KB 供诊断。
/// ffmpeg 把进度与错误都打到 stderr,不排空长录制会把管道撑满。
fn drain_stderr(child: &mut Child) -> std::sync::Arc<Mutex<String>> {
    use std::io::Read;
    let tail = std::sync::Arc::new(Mutex::new(String::new()));
    if let Some(mut stderr) = child.stderr.take() {
        let tail = std::sync::Arc::clone(&tail);
        std::thread::spawn(move || {
            let mut buf = [0u8; 2048];
            loop {
                match stderr.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if let Ok(mut t) = tail.lock() {
                            t.push_str(&String::from_utf8_lossy(&buf[..n]));
                            let over = t.len().saturating_sub(4096);
                            if over > 0 {
                                t.drain(..over);
                            }
                        }
                    }
                }
            }
        });
    }
    tail
}

/// 从 stderr 尾部摘最后一行非空文本,拼进给用户的错误信息(没有则返回空串)。
fn stderr_last_line(tail: &Mutex<String>) -> String {
    tail.lock()
        .map(|t| {
            t.lines()
                .rev()
                .find(|l| !l.trim().is_empty())
                .map(|l| l.trim().to_string())
                .unwrap_or_default()
        })
        .unwrap_or_default()
}

/// 录屏全局状态:同一时刻只允许一个录制会话。挂在 AppState 上跨命令共享。
pub struct RecordingState {
    inner: Mutex<Option<RecordingSession>>,
    /// ffmpeg 是否可用:程序启动时探测一次写入,后续录屏命令直接读此标记,不再每次启子进程探测。
    /// `check_ffmpeg` 命令(用户在设置里手动检测)也会刷新它,免重启即可生效。
    ffmpeg_available: AtomicBool,
    /// 默认麦克风名缓存(启动后后台预热填入):设备枚举要起一次 ffmpeg 子进程(数百毫秒),
    /// 是「开始录制」等待的大头;probed 前为 None,开始录制时现场枚举并回填
    default_mic: Mutex<Option<String>>,
    /// default_mic 是否已探测过(区分「未探测」与「探测过但无可用设备」)
    mic_probed: AtomicBool,
    /// 视频编码器选择(启动后后台实测初始化探测:nvenc / qsv / amf,皆不可用回退 libx264)。
    /// 硬编把 4K 编码从 CPU 挪到 GPU,是慢机录高分辨率不卡的关键
    video_encoder: Mutex<VideoEncoder>,
    /// ddagrab(DDA 桌面复制抓屏)是否可用:比 gdigrab 的 GDI BitBlt 省 CPU,可用则优先
    ddagrab_ok: AtomicBool,
    /// 编码器 / 抓屏方式是否已探测过(未探测时开始录制现场探测并回填)
    video_probed: AtomicBool,
}

impl RecordingState {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(None),
            ffmpeg_available: AtomicBool::new(false),
            default_mic: Mutex::new(None),
            mic_probed: AtomicBool::new(false),
            video_encoder: Mutex::new(VideoEncoder::Software),
            ddagrab_ok: AtomicBool::new(false),
            video_probed: AtomicBool::new(false),
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

    /// 读取麦克风缓存:None = 尚未探测(调用方需现场枚举);
    /// Some(x) = 已探测,x 为设备名(无可用设备时为 None,直接降级纯视频)
    pub fn probed_mic(&self) -> Option<Option<String>> {
        if !self.mic_probed.load(Ordering::Relaxed) {
            return None;
        }
        Some(self.default_mic.lock().ok().and_then(|m| m.clone()))
    }

    /// 写入麦克风探测结果(启动预热 / 现场枚举回填 / 设备失效后刷新)。
    pub fn set_default_mic(&self, mic: Option<String>) {
        if let Ok(mut m) = self.default_mic.lock() {
            *m = mic;
        }
        self.mic_probed.store(true, Ordering::Relaxed);
    }

    /// 读取编码器 / 抓屏方式探测结果:None = 尚未探测(调用方需现场探测并回填)。
    pub fn probed_video(&self) -> Option<(VideoEncoder, bool)> {
        if !self.video_probed.load(Ordering::Relaxed) {
            return None;
        }
        let enc = self
            .video_encoder
            .lock()
            .map(|e| *e)
            .unwrap_or(VideoEncoder::Software);
        Some((enc, self.ddagrab_ok.load(Ordering::Relaxed)))
    }

    /// 写入编码器 / 抓屏方式探测结果(启动预热 / 现场探测回填)。
    pub fn set_video_probe(&self, encoder: VideoEncoder, ddagrab: bool) {
        if let Ok(mut e) = self.video_encoder.lock() {
            *e = encoder;
        }
        self.ddagrab_ok.store(ddagrab, Ordering::Relaxed);
        self.video_probed.store(true, Ordering::Relaxed);
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
    /// 是否在采麦克风(录制中状态展示用;启动后不可改)。
    pub with_mic: bool,
    /// 本次录制的屏幕下标;null = 全部屏幕。
    pub screen_index: Option<u32>,
    /// 开始时间(Unix 秒),未录制为 null。
    pub started_at: Option<i64>,
    /// 输出文件路径,未录制为 null。
    pub output_path: Option<String>,
    /// 是否处于暂停态(ffmpeg 分段已收尾,等「继续」起新段)。
    pub paused: bool,
    /// 已录制的活跃秒数(不含暂停时段),悬浮窗计时以此为基准本地走秒。
    pub elapsed_secs: f64,
}

impl RecordingStatus {
    fn idle() -> Self {
        Self {
            recording: false,
            with_mic: false,
            screen_index: None,
            started_at: None,
            output_path: None,
            paused: false,
            elapsed_secs: 0.0,
        }
    }
}

/// 显示器信息(回传前端 camelCase 对齐 TS ScreenInfo)。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScreenInfo {
    /// 枚举下标(开始录制时按它回选)。
    pub index: u32,
    /// 系统显示器名(如 \\.\DISPLAY1),仅展示参考。
    pub name: Option<String>,
    pub width: u32,
    pub height: u32,
    /// 是否主屏。
    pub primary: bool,
}

/// 枚举到的显示器条目:展示字段 + 物理像素位置(录屏区域用)。
struct ScreenEntry {
    index: u32,
    name: Option<String>,
    width: u32,
    height: u32,
    x: i32,
    y: i32,
    primary: bool,
}

/// 枚举当前显示器。坐标/尺寸为物理像素,与 gdigrab 的虚拟桌面坐标系一致
/// (前提是进程 DPI 感知,Tauri 默认满足);多屏时主屏左侧/上方的屏幕 x/y 为负,属正常。
fn enumerate_screens(app: &AppHandle) -> Vec<ScreenEntry> {
    let primary = app.primary_monitor().ok().flatten();
    app.available_monitors()
        .unwrap_or_default()
        .into_iter()
        .enumerate()
        .map(|(i, m)| {
            let pos = *m.position();
            let size = *m.size();
            let primary = primary
                .as_ref()
                .is_some_and(|p| *p.position() == pos && *p.size() == size);
            ScreenEntry {
                index: i as u32,
                name: m.name().cloned(),
                width: size.width,
                height: size.height,
                x: pos.x,
                y: pos.y,
                primary,
            }
        })
        .collect()
}

/// 列出可录制的显示器(悬浮条的屏幕选择用;单屏时前端隐藏选择器)。
#[tauri::command]
pub fn list_screens(app: AppHandle) -> Vec<ScreenInfo> {
    enumerate_screens(&app)
        .into_iter()
        .map(|s| ScreenInfo {
            index: s.index,
            name: s.name,
            width: s.width,
            height: s.height,
            primary: s.primary,
        })
        .collect()
}

/// 开始录屏:校验 ffmpeg → (默认)最小化主窗口 → 起 ffmpeg → 弹悬浮窗。
/// `with_mic` 为真时采麦克风;`screen_index` 为 Some 时只录该显示器,None 录全部屏幕;
/// `include_app` 为真时不最小化主窗口——本程序的界面与操作一并入镜(演示本软件用),
/// 悬浮条自身已排除出捕获,任何模式下都不会被录进去;
/// `mic_device` 指定麦克风设备名(设置面板里选的),空 / None = 自动挑默认设备。
#[tauri::command]
pub async fn start_screen_recording(
    state: State<'_, AppState>,
    app: AppHandle,
    with_mic: bool,
    screen_index: Option<u32>,
    include_app: bool,
    mic_device: Option<String>,
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

    // 输出目录:<app_data>/recordings/;首段 recording-<时间戳>.mp4,暂停后继续的分段带 .partN 后缀
    let dir = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("定位数据目录失败: {e}"))?
        .join("recordings");
    std::fs::create_dir_all(&dir).map_err(|e| format!("创建录屏目录失败: {e}"))?;
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
    let stem = format!("recording-{stamp}");
    let output_path = dir.join(format!("{stem}.mp4"));

    // 先最小化主窗口,让随后录到的画面不含本程序(开「含本程序」时跳过,本程序一并入镜)
    if !include_app {
        if let Some(main) = app.get_webview_window(MAIN_WINDOW_LABEL) {
            let _ = main.minimize();
            // 等最小化真正落定再起 ffmpeg:动画期间 DWM 桌面合成未稳定,
            // gdigrab 开头会抓到黑帧/过渡帧(慢机上动画更慢,黑段更长)。
            // 轮询窗口状态而非固定 sleep:快机不等,慢机兜底 1.5s 不硬卡
            let deadline = Instant::now() + Duration::from_millis(1500);
            while Instant::now() < deadline {
                if main.is_minimized().unwrap_or(false) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            // 动画收尾 + DWM 重新合成一帧的余量
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }

    // 起 ffmpeg(stdin 接管以便后续写 'q' 优雅停止;stderr 接管排空,留尾部供诊断)
    let program = ffmpeg_path
        .as_deref()
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .unwrap_or("ffmpeg")
        .to_string();
    // 多屏:None=整虚拟桌面;Some(i) 取该显示器的物理像素区域。
    // 下标越界(选择后显示器被拔/枚举顺序变了)降级全屏,不让录屏失败
    let (region, screen_index) = match screen_index {
        Some(idx) => match enumerate_screens(&app).into_iter().find(|s| s.index == idx) {
            Some(s) => (
                Some(ScreenRegion {
                    x: s.x,
                    y: s.y,
                    width: s.width,
                    height: s.height,
                }),
                Some(idx),
            ),
            None => {
                tracing::warn!("屏幕下标 {idx} 已失效,降级为录制全部屏幕");
                (None, None)
            }
        },
        None => (None, None),
    };
    // 麦克风:用户在设置面板指定了设备就直接用(不走默认缓存);
    // 未指定走启动预热缓存(枚举要起一次 ffmpeg 设备列表子进程,是「开始」等待的大头);
    // 缓存未就绪(启动后立刻点录屏)才现场枚举并回填。枚举不到设备降级纯视频。
    let mic = if with_mic {
        match mic_device.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            Some(named) => Some(named.to_string()),
            None => match state.recording.probed_mic() {
                Some(cached) => cached,
                None => {
                    let p = program.clone();
                    let mic = tauri::async_runtime::spawn_blocking(move || default_microphone(&p))
                        .await
                        .ok()
                        .flatten();
                    state.recording.set_default_mic(mic.clone());
                    mic
                }
            },
        }
    } else {
        None
    };
    if with_mic && mic.is_none() {
        tracing::warn!("未找到可用麦克风设备,本次录屏降级为纯视频");
    }
    // 编码器 / 抓屏方式走启动预热缓存(实测初始化探测要起几个小子进程);
    // 缓存未就绪(启动后立刻点录屏)才现场探测并回填
    let (encoder, ddagrab) = match state.recording.probed_video() {
        Some(probed) => probed,
        None => {
            let p = program.clone();
            let probed = tauri::async_runtime::spawn_blocking(move || {
                (probe_video_encoder(&p), probe_ddagrab(&p))
            })
            .await
            .unwrap_or((VideoEncoder::Software, false));
            state.recording.set_video_probe(probed.0, probed.1);
            probed
        }
    };
    tracing::info!("录屏参数:编码器={} 抓屏={}", encoder.label(), if ddagrab { "ddagrab" } else { "gdigrab" });
    let mut spec = RecordSpec {
        output: output_path.clone(),
        mic,
        region,
        screen_index,
        encoder,
        ddagrab,
    };
    let mut child = spawn_ffmpeg(&program, &spec)?;
    let mut stderr_tail = drain_stderr(&mut child);

    // 启动后短暂确认 ffmpeg 存活:输入设备打开失败等会让它立即退出,
    // 不检查就会「假录制」——计时在走、实际无产出,最终视频 0:00。
    tokio::time::sleep(Duration::from_millis(1000)).await;
    if let Ok(Some(_)) | Err(_) = child.try_wait() {
        // 麦克风设备打开失败(被占用 / 系统隐私设置拦截)不应拖垮录屏:降级纯视频重试一次
        if spec.mic.take().is_some() {
            tracing::warn!("麦克风采集启动失败,降级纯视频重试: {}", stderr_last_line(&stderr_tail));
            // 缓存的设备名可能已失效(热插拔 / 被占用):后台重新探测刷新,避免下次还踩同一设备
            let app = app.clone();
            tauri::async_runtime::spawn(async move {
                warm_recording_probes(app).await;
            });
            let _ = child.kill();
            let _ = child.wait();
            child = spawn_ffmpeg(&program, &spec)?;
            stderr_tail = drain_stderr(&mut child);
            tokio::time::sleep(Duration::from_millis(1000)).await;
        }
    }
    if let Ok(Some(_)) | Err(_) = child.try_wait() {
        let detail = stderr_last_line(&stderr_tail);
        let _ = child.kill();
        let _ = child.wait();
        restore_main(&app);
        close_overlay(&app);
        let msg = if detail.is_empty() {
            "ffmpeg 启动后立即退出,录屏未开始".to_string()
        } else {
            format!("ffmpeg 启动后立即退出: {detail}")
        };
        let _ = app.emit_to(
            MAIN_WINDOW_LABEL,
            "recording-failed",
            json!({ "message": msg }),
        );
        return Err(msg);
    }
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
            child: Some(child),
            segments: Vec::new(),
            current_path: output_path.clone(),
            dir,
            stem,
            next_part: 2, // 首段无后缀已占用,后续分段从 .part2 起
            started_at,
            with_mic: spec.mic.is_some(),
            screen_index: spec.screen_index,
            program,
            mic: spec.mic.clone(),
            region: spec.region,
            encoder: spec.encoder,
            ddagrab: spec.ddagrab,
            active_elapsed: Duration::ZERO,
            active_since: Some(Instant::now()),
            stderr_tail,
        });
    }

    // 弹悬浮控制条(失败不影响录制本身,仅记日志);可能刚从预览确认态过来,顺手缩回小条尺寸
    if let Err(e) = open_overlay(&app) {
        tracing::warn!("创建录屏悬浮窗失败: {e}");
    }
    if let Some(w) = app.get_webview_window(RECORDING_OVERLAY_LABEL) {
        let _ = w.set_size(tauri::LogicalSize::new(OVERLAY_W, OVERLAY_H));
        position_overlay(&w, OVERLAY_W);
    }

    Ok(RecordingStatus {
        recording: true,
        with_mic: spec.mic.is_some(),
        screen_index: spec.screen_index,
        started_at: Some(started_at),
        output_path: Some(output_path.to_string_lossy().to_string()),
        paused: false,
        elapsed_secs: 0.0,
    })
}

/// 停止录屏:优雅收尾当前分段(写完 MP4)→ 关悬浮窗 → 还原主窗口 →
/// 后台拼接分段(有过暂停时)+ faststart 重排 → 完成后通知主窗口弹保存提示。
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

    let Some(mut session) = session else {
        close_overlay(&app);
        restore_main(&app);
        return Ok(RecordingStatus::idle());
    };

    // 优雅收尾当前分段(暂停态没有活跃进程,直接跳过);阻塞 wait 放 blocking 线程
    if let Some(child) = session.child.take() {
        tauri::async_runtime::spawn_blocking(move || finalize_child(child))
            .await
            .map_err(|e| format!("停止录屏异常: {e}"))?;
    }
    // 当前段校验入列:文件缺失 / 过小说明该段录制中途已失败(如 ffmpeg 崩溃),丢弃不拼接
    if valid_segment(&session.current_path) {
        session.segments.push(session.current_path.clone());
    } else {
        let _ = std::fs::remove_file(&session.current_path);
    }

    close_overlay(&app);
    restore_main(&app);

    if session.segments.is_empty() {
        let detail = stderr_last_line(&session.stderr_tail);
        let msg = if detail.is_empty() {
            "录屏失败:ffmpeg 未产出有效视频".to_string()
        } else {
            format!("录屏失败: {detail}")
        };
        tracing::warn!("{msg}");
        let _ = app.emit_to(
            MAIN_WINDOW_LABEL,
            "recording-failed",
            json!({ "message": msg }),
        );
        return Ok(RecordingStatus::idle());
    }

    // 最终文件:单段即该段本身;多段(有过暂停)拼接到首段文件名
    let segments = session.segments;
    let final_path = if segments.len() > 1 {
        session.dir.join(format!("{}.mp4", session.stem))
    } else {
        segments[0].clone()
    };

    // 拼接 + faststart 重排(moov 搬到文件头,<video> 经 asset 协议才能读时长 / 拖动)都放后台:
    // 「停止」秒回;全部完成才通知主窗口弹保存提示,保证用户打开的一定是修好的文件
    let program = session.program.clone();
    {
        let app = app.clone();
        let path = final_path.clone();
        tauri::async_runtime::spawn(async move {
            // 多段先 -c copy 拼接(不重编码,很快);拼接失败退化为第一段,不丢全部内容
            let merged = if segments.len() > 1 {
                concat_segments(&program, &segments, &path).await
            } else {
                true
            };
            let target = if merged { path } else { segments[0].clone() };
            remux_faststart(&program, &target).await;
            // 通知主窗口(其 Toaster 才能弹提示并提供「打开所在文件夹」)
            let _ = app.emit_to(
                MAIN_WINDOW_LABEL,
                "recording-saved",
                json!({ "path": target.to_string_lossy() }),
            );
        });
    }

    Ok(RecordingStatus {
        output_path: Some(final_path.to_string_lossy().to_string()),
        ..RecordingStatus::idle()
    })
}

/// 暂停 / 继续录制(悬浮条「暂停」按钮;ffmpeg 不支持真暂停,用分段实现):
/// 暂停 = 优雅收尾当前分段(写完 moov)并挂起计时;继续 = 用完全相同的参数起新分段,
/// 停止时把各段 -c copy 拼接,暂停时段不进成片。
#[tauri::command]
pub async fn toggle_recording_pause(
    state: State<'_, AppState>,
    app: AppHandle,
) -> std::result::Result<RecordingStatus, String> {
    /// 锁内只做判定与字段变更,耗时的进程操作放锁外执行
    enum Pending {
        Pause(Child, std::path::PathBuf),
        Resume(RecordSpec, String),
    }
    let pending = {
        let mut guard = state
            .recording
            .inner
            .lock()
            .map_err(|_| "录屏状态锁异常".to_string())?;
        let Some(session) = guard.as_mut() else {
            return Err("当前没有进行中的录制".to_string());
        };
        if let Some(child) = session.child.take() {
            // 暂停:累计活跃时长,计时挂起
            if let Some(since) = session.active_since.take() {
                session.active_elapsed += since.elapsed();
            }
            Pending::Pause(child, session.current_path.clone())
        } else {
            let spec = RecordSpec {
                output: session.next_segment_path(),
                mic: session.mic.clone(),
                region: session.region,
                screen_index: session.screen_index,
                encoder: session.encoder,
                ddagrab: session.ddagrab,
            };
            Pending::Resume(spec, session.program.clone())
        }
    };

    match pending {
        Pending::Pause(child, path) => {
            tauri::async_runtime::spawn_blocking(move || finalize_child(child))
                .await
                .map_err(|e| format!("暂停录屏异常: {e}"))?;
            // 分段校验入列;无效段(刚开始就暂停 / ffmpeg 已崩)丢弃,不让坏文件混进拼接
            if valid_segment(&path) {
                let mut guard = state
                    .recording
                    .inner
                    .lock()
                    .map_err(|_| "录屏状态锁异常".to_string())?;
                if let Some(session) = guard.as_mut() {
                    session.segments.push(path);
                }
            } else {
                let _ = std::fs::remove_file(&path);
                tracing::warn!("暂停分段无效(过小 / 缺失),已丢弃: {}", path.display());
            }
            let guard = state
                .recording
                .inner
                .lock()
                .map_err(|_| "录屏状态锁异常".to_string())?;
            Ok(guard
                .as_ref()
                .map(|s| s.status())
                .unwrap_or_else(RecordingStatus::idle))
        }
        Pending::Resume(spec, program) => {
            let path = spec.output.clone();
            let mut child = spawn_ffmpeg(&program, &spec)?;
            let stderr_tail = drain_stderr(&mut child);
            // 与开始同理:确认新分段进程存活(麦克风设备可能这次打开失败),失败保持暂停态可重试
            tokio::time::sleep(Duration::from_millis(1000)).await;
            if let Ok(Some(_)) | Err(_) = child.try_wait() {
                let detail = stderr_last_line(&stderr_tail);
                let _ = child.kill();
                let _ = child.wait();
                let _ = std::fs::remove_file(&path);
                let msg = if detail.is_empty() {
                    "恢复录制失败:ffmpeg 启动后立即退出".to_string()
                } else {
                    format!("恢复录制失败: {detail}")
                };
                let _ = app.emit_to(
                    MAIN_WINDOW_LABEL,
                    "recording-failed",
                    json!({ "message": msg }),
                );
                return Err(msg);
            }
            let mut guard = state
                .recording
                .inner
                .lock()
                .map_err(|_| "录屏状态锁异常".to_string())?;
            let Some(session) = guard.as_mut() else {
                // 竞态:恢复期间被停止了——杀掉新进程,丢弃该段
                let mut child = child;
                let _ = child.kill();
                let _ = child.wait();
                let _ = std::fs::remove_file(&path);
                return Err("录制已停止".to_string());
            };
            session.child = Some(child);
            session.current_path = path;
            session.stderr_tail = stderr_tail;
            session.active_since = Some(Instant::now());
            Ok(session.status())
        }
    }
}

/// 开 / 关录屏悬浮控制条(**不立即开始录制**):录制由悬浮条上的「开始」按钮手动触发。
/// 开关语义:悬浮窗未开 → 打开;已开且未在录制 → 关闭(再次点击入口 = 收起);录制中 → 只把它带到前面。
/// 先预检 ffmpeg(主窗口能弹 toast 引导),不可用就不弹悬浮窗。此时不最小化主窗口(按下「开始」才最小化)。
#[tauri::command]
pub async fn open_recording_overlay(
    state: State<'_, AppState>,
    app: AppHandle,
) -> std::result::Result<(), String> {
    let is_recording = state
        .recording
        .inner
        .lock()
        .map(|g| g.is_some())
        .unwrap_or(false);

    // 悬浮窗已存在:录制中带前;未录制则关闭(开关的「关」)
    if let Some(w) = app.get_webview_window(RECORDING_OVERLAY_LABEL) {
        if is_recording {
            let _ = w.show();
            let _ = w.set_focus();
        } else {
            close_overlay(&app);
        }
        return Ok(());
    }

    // ffmpeg 预检要在开窗之前:不可用直接报错(主窗口 hook 弹 toast 引导),避免「开了又关」的闪烁
    if !state.recording.ffmpeg_available() {
        return Err(
            "未检测到 ffmpeg,无法录屏。请先安装 ffmpeg,或在「系统配置」中设置 ffmpeg 路径。"
                .to_string(),
        );
    }
    open_overlay(&app).map_err(|e| format!("打开录屏悬浮窗失败: {e}"))
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
        Ok(guard) => guard
            .as_ref()
            .map(|s| s.status())
            .unwrap_or_else(RecordingStatus::idle),
        Err(_) => RecordingStatus::idle(),
    }
}

/// 切换录屏悬浮窗尺寸:小控制条 ↔ 预览确认(大窗),并重摆到顶部居中。
/// 窗口缩放走后端:悬浮窗按最小授权没有任何窗口控制类 capability,前端自己改不了尺寸。
#[tauri::command]
pub fn set_recording_overlay_preview(app: AppHandle, preview: bool) {
    if let Some(w) = app.get_webview_window(RECORDING_OVERLAY_LABEL) {
        let (width, height) = if preview {
            (PREVIEW_W, PREVIEW_H)
        } else {
            (OVERLAY_W, OVERLAY_H)
        };
        let _ = w.set_size(tauri::LogicalSize::new(width, height));
        position_overlay(&w, width);
    }
}

/// 语音滤镜链(正式录制与音频测试共用,保证试听到的就是成品听感):
/// 高通滤掉低频轰隆/电流声 → afftdn FFT 降噪 → dynaudnorm 动态归一(轻声自动抬升)→
/// 限幅防爆音削顶 → 统一采样率
const AUDIO_FILTER_CHAIN: &str =
    "highpass=f=100,afftdn=nr=12:nf=-28,dynaudnorm=f=150:g=15,alimiter=limit=0.95,aresample=44100";

/// 解析实际可用的 ffmpeg 程序路径(配置为空则用系统 PATH 的 ffmpeg)。
fn resolve_ffmpeg_program(state: &State<'_, AppState>) -> std::result::Result<String, String> {
    let cfg = lock_config(state).map_err(|e| e.to_string())?;
    Ok(cfg
        .media
        .ffmpeg_path
        .as_deref()
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .unwrap_or("ffmpeg")
        .to_string())
}

/// 音频设备信息(回传前端 camelCase 对齐 TS AudioDeviceInfo)。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioDeviceInfo {
    /// dshow 设备名(开始录制 / 音频测试按它回选)。
    pub name: String,
    /// 是否评分最高的推荐设备(前端标注)。
    pub recommended: bool,
}

/// 列出可用音频输入设备(设置面板的音频选择器用;推荐设备标 recommended)。
#[tauri::command]
pub async fn list_audio_devices(
    state: State<'_, AppState>,
) -> std::result::Result<Vec<AudioDeviceInfo>, String> {
    let program = resolve_ffmpeg_program(&state)?;
    // 枚举要起一次 ffmpeg 子进程,阻塞调用放 blocking 线程
    let devs = tauri::async_runtime::spawn_blocking(move || dshow_audio_devices(&program))
        .await
        .map_err(|e| format!("枚举音频设备异常: {e}"))?;
    let mut marked = false;
    Ok(devs
        .into_iter()
        .map(|(name, score)| {
            let recommended = score > 0 && !marked;
            marked |= recommended;
            AudioDeviceInfo { name, recommended }
        })
        .collect())
}

/// 音频测试:用选中的设备录 3 秒(与正式录制同一条滤镜链,试听即成品听感),
/// 返回 m4a 的 base64 data URL 供前端直接回放,不落盘。录制进行中拒绝(设备被占)。
#[tauri::command]
pub async fn test_recording_audio(
    state: State<'_, AppState>,
    device: Option<String>,
) -> std::result::Result<String, String> {
    let busy = state
        .recording
        .inner
        .lock()
        .map(|g| g.is_some())
        .unwrap_or(false);
    if busy {
        return Err("录制进行中,无法测试音频".to_string());
    }
    let program = resolve_ffmpeg_program(&state)?;
    // 设备:用户指定 > 默认缓存 > 现场枚举并回填
    let mic = match device.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(named) => named.to_string(),
        None => match state.recording.probed_mic() {
            Some(Some(cached)) => cached,
            _ => {
                let p = program.clone();
                let mic = tauri::async_runtime::spawn_blocking(move || default_microphone(&p))
                    .await
                    .ok()
                    .flatten();
                state.recording.set_default_mic(mic.clone());
                mic.ok_or("未找到可用麦克风设备")?
            }
        },
    };
    tauri::async_runtime::spawn_blocking(move || record_audio_sample(&program, &mic))
        .await
        .map_err(|e| format!("音频测试异常: {e}"))?
}

/// 录 3 秒麦克风样本到临时 m4a,读成 base64 data URL 后删文件。
#[cfg(windows)]
fn record_audio_sample(program: &str, mic: &str) -> std::result::Result<String, String> {
    use base64::Engine;
    let tmp = std::env::temp_dir().join(format!("veltrix-mic-test-{}.m4a", std::process::id()));
    let mut cmd = std::process::Command::new(program);
    crate::media::hide_console_window(&mut cmd);
    let status = cmd
        .arg("-y")
        .args(["-f", "dshow", "-audio_buffer_size", "80"])
        .args(["-i", &format!("audio={mic}")])
        .args(["-t", "3", "-af", AUDIO_FILTER_CHAIN])
        .args(["-c:a", "aac", "-b:a", "96k"])
        .arg(&tmp)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    let ok = status.map(|s| s.success()).unwrap_or(false)
        && std::fs::metadata(&tmp).map(|m| m.len() > 1000).unwrap_or(false);
    if !ok {
        let _ = std::fs::remove_file(&tmp);
        return Err("录音失败:设备打不开或被占用".to_string());
    }
    let bytes = std::fs::read(&tmp).map_err(|e| format!("读取测试音频失败: {e}"))?;
    let _ = std::fs::remove_file(&tmp);
    Ok(format!(
        "data:audio/mp4;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(bytes)
    ))
}

#[cfg(not(windows))]
fn record_audio_sample(_program: &str, _mic: &str) -> std::result::Result<String, String> {
    Err("当前平台暂不支持音频测试".to_string())
}

/// 展开 / 收起设置面板(小条正下方):窗口加高容纳面板;悬浮窗无窗口控制权限,缩放走后端。
/// 预览态窗口本身够大,前端在预览态内联渲染面板,不调本命令。
#[tauri::command]
pub fn set_recording_overlay_panel(app: AppHandle, open: bool) {
    if let Some(w) = app.get_webview_window(RECORDING_OVERLAY_LABEL) {
        let height = if open { OVERLAY_H + PANEL_H } else { OVERLAY_H };
        let _ = w.set_size(tauri::LogicalSize::new(OVERLAY_W, height));
        position_overlay(&w, OVERLAY_W);
    }
}

/// 每块屏一张预览缩略图(悬浮条平铺选屏用)。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScreenPreview {
    /// 与 list_screens 一致的下标。
    pub index: u32,
    /// PNG 的 base64 data URL。
    pub data_url: String,
}

/// 截取所有显示器的预览缩略图(悬浮条平铺选屏用,不落盘)。
/// 某屏截取失败则跳过该屏,不让整体失败;全失败才报错。
#[tauri::command]
pub async fn recording_preview_all(app: AppHandle) -> std::result::Result<Vec<ScreenPreview>, String> {
    // 目标屏幕沿用录制同款枚举(Tauri),与 xcap 显示器按物理坐标配对,避免两套枚举顺序不一致选错屏
    let screens: Vec<(u32, i32, i32)> = enumerate_screens(&app)
        .into_iter()
        .map(|s| (s.index, s.x, s.y))
        .collect();
    // 截屏是阻塞调用,放 blocking 线程
    tokio::task::spawn_blocking(move || capture_all_previews(&screens))
        .await
        .map_err(|e| format!("截屏任务异常: {e}"))?
}

/// 逐屏截图 + 缩略编码;配对失败的屏跳过。
fn capture_all_previews(
    screens: &[(u32, i32, i32)],
) -> std::result::Result<Vec<ScreenPreview>, String> {
    let monitors = xcap::Monitor::all().map_err(|e| format!("枚举显示器失败: {e}"))?;
    let mut out = Vec::new();
    for (index, x, y) in screens {
        let Some(m) = monitors.iter().find(|m| {
            m.x().unwrap_or(i32::MIN) == *x && m.y().unwrap_or(i32::MIN) == *y
        }) else {
            continue;
        };
        let Ok(img) = m.capture_image() else { continue };
        out.push(ScreenPreview {
            index: *index,
            data_url: encode_preview_png(&img),
        });
    }
    if out.is_empty() {
        return Err("所有显示器截图均失败".to_string());
    }
    Ok(out)
}

/// 缩略图编码:限宽等比缩小(控制 base64 载荷),PNG → data URL。
fn encode_preview_png(image: &image::RgbaImage) -> String {
    use base64::Engine;
    use image::ImageEncoder;
    let (w, h) = (image.width(), image.height());
    let resized;
    let img = if w > PREVIEW_MAX_W {
        let nh = (h as f64 * PREVIEW_MAX_W as f64 / w as f64)
            .round()
            .max(1.0) as u32;
        resized = image::imageops::resize(image, PREVIEW_MAX_W, nh, image::imageops::FilterType::Triangle);
        &resized
    } else {
        image
    };
    let mut png: Vec<u8> = Vec::new();
    if image::codecs::png::PngEncoder::new(&mut png)
        .write_image(
            img.as_raw(),
            img.width(),
            img.height(),
            image::ExtendedColorType::Rgba8,
        )
        .is_err()
    {
        return String::new(); // 编码失败给空串,前端按「此屏无预览」占位
    }
    let b64 = base64::engine::general_purpose::STANDARD.encode(&png);
    format!("data:image/png;base64,{b64}")
}

/// 单屏采集区域(物理像素;主屏左侧/上方的屏幕 x/y 为负,与 gdigrab 虚拟桌面坐标系一致)。
#[derive(Clone, Copy)]
struct ScreenRegion {
    x: i32,
    y: i32,
    width: u32,
    height: u32,
}

/// 一次录制的参数打包(spawn / build 共用,遵守函数参数 ≤4 的约定)。
struct RecordSpec {
    /// 输出 MP4 绝对路径。
    output: std::path::PathBuf,
    /// Some = 采该麦克风;None = 纯视频。
    mic: Option<String>,
    /// Some = 只录该显示器区域;None = 整虚拟桌面(全部屏幕拼一帧)。
    region: Option<ScreenRegion>,
    /// 前端选择的屏幕下标(回传展示用;None = 全部屏幕)。
    screen_index: Option<u32>,
    /// 视频编码器(硬编优先,软编兜底);分段间必须一致才能 -c copy 拼接。
    encoder: VideoEncoder,
    /// 抓屏方式:true = ddagrab(DDA,省 CPU);false = gdigrab(GDI 兜底)。
    ddagrab: bool,
}

/// 起 ffmpeg 录屏进程(stdin 接管用于优雅停止,stderr 接管供诊断)。
fn spawn_ffmpeg(program: &str, spec: &RecordSpec) -> std::result::Result<Child, String> {
    let mut cmd = build_ffmpeg_command(program, spec);
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    cmd.spawn()
        .map_err(|e| format!("启动 ffmpeg 录屏失败: {e}"))
}

/// 视频编码器选择:硬编(nvenc / qsv / amf)把 4K 编码从 CPU 挪到 GPU,慢机录高分辨率不卡的关键;
/// 探测顺序即优先级(N 卡 → Intel 核显 → A 卡),皆不可用回退 libx264 ultrafast(软编兜底)。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum VideoEncoder {
    Nvenc,
    Qsv,
    Amf,
    Software,
}

impl VideoEncoder {
    /// 编码参数(不含 -pix_fmt;硬编用质量档而非固定码率,屏幕内容观感更稳)
    fn encode_args(self) -> &'static [&'static str] {
        match self {
            // p3 预设 + vbr cq:质量接近 x264 medium,GPU 完成,CPU 几乎零开销
            VideoEncoder::Nvenc => &["-c:v", "h264_nvenc", "-preset", "p3", "-rc", "vbr", "-cq", "26"],
            // 关 look_ahead 省 CPU(前瞻分析在 CPU 侧跑)
            VideoEncoder::Qsv => &["-c:v", "h264_qsv", "-b:v", "8M", "-look_ahead", "0"],
            VideoEncoder::Amf => &["-c:v", "h264_amf", "-quality", "speed", "-b:v", "8M"],
            VideoEncoder::Software => &["-c:v", "libx264", "-preset", "ultrafast"],
        }
    }

    /// 像素格式:qsv 只认 nv12,其余用 yuv420p(播放器兼容性最好)
    fn pix_fmt(self) -> &'static str {
        match self {
            VideoEncoder::Qsv => "nv12",
            _ => "yuv420p",
        }
    }

    fn label(self) -> &'static str {
        match self {
            VideoEncoder::Nvenc => "h264_nvenc",
            VideoEncoder::Qsv => "h264_qsv",
            VideoEncoder::Amf => "h264_amf",
            VideoEncoder::Software => "libx264",
        }
    }
}

/// 实测初始化一个编码器:编 3 帧 64×64 空源,能跑通才算可用。
/// 只看 -encoders 清单不可靠——编译进 ffmpeg 不代表机器上有对应 GPU / 驱动。
fn test_video_encoder(program: &str, enc: VideoEncoder) -> bool {
    let mut cmd = std::process::Command::new(program);
    crate::media::hide_console_window(&mut cmd);
    cmd.args(["-hide_banner", "-loglevel", "error"])
        .args(["-f", "lavfi", "-i", "nullsrc=size=64x64:duration=0.2:rate=15"])
        .args(enc.encode_args())
        .args(["-pix_fmt", enc.pix_fmt()])
        .args(["-frames:v", "3", "-f", "null", "-"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    cmd.status().map(|s| s.success()).unwrap_or(false)
}

/// 探测最优可用编码器(nvenc → qsv → amf → libx264 兜底)。
fn probe_video_encoder(program: &str) -> VideoEncoder {
    for enc in [VideoEncoder::Nvenc, VideoEncoder::Qsv, VideoEncoder::Amf] {
        if test_video_encoder(program, enc) {
            return enc;
        }
    }
    VideoEncoder::Software
}

/// 探测 ddagrab(DDA 桌面复制抓屏)是否可用:编译级探测即可(DDA 在 Win8+ 系统上都可用)。
/// ddagrab 走 GPU 侧复制,比 gdigrab 的 GDI BitBlt 省 CPU,4K 屏差距明显。
#[cfg(windows)]
fn probe_ddagrab(program: &str) -> bool {
    let mut cmd = std::process::Command::new(program);
    crate::media::hide_console_window(&mut cmd);
    let output = cmd
        .args(["-hide_banner", "-demuxers"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output();
    match output {
        Ok(o) => String::from_utf8_lossy(&o.stdout).contains(" ddagrab"),
        Err(_) => false,
    }
}

#[cfg(not(windows))]
fn probe_ddagrab(_program: &str) -> bool {
    false
}

/// 解析 ffmpeg dshow 设备清单文本,返回 (设备名, 评分) 按评分降序。
/// 评分:带麦克风特征 +10;虚拟声卡(远控软件装的,采到的是无声流)-100。
#[cfg(windows)]
fn parse_dshow_audio_devices(text: &str) -> Vec<(String, i32)> {
    // 设备清单打在 stderr,行形如:"麦克风阵列 (Realtek(R) Audio)" (audio)
    let mut devs: Vec<(String, i32)> = Vec::new();
    for line in text.lines() {
        if !line.contains("(audio)") {
            continue;
        }
        let Some(name) = line.split('"').nth(1) else { continue };
        let lower = name.to_lowercase();
        let mut score = 0;
        if name.contains("麦克风") || lower.contains("microphone") || lower.contains("mic") {
            score += 10;
        }
        if name.contains("虚拟") || lower.contains("virtual") {
            score -= 100;
        }
        devs.push((name.to_string(), score));
    }
    devs.sort_by(|a, b| b.1.cmp(&a.1));
    devs
}

/// 起一次 ffmpeg 子进程枚举 dshow 音频设备(数百毫秒)。
#[cfg(windows)]
fn dshow_audio_devices(ffmpeg: &str) -> Vec<(String, i32)> {
    let mut cmd = std::process::Command::new(ffmpeg);
    crate::media::hide_console_window(&mut cmd);
    let output = cmd
        .args([
            "-hide_banner",
            "-list_devices",
            "true",
            "-f",
            "dshow",
            "-i",
            "dummy",
        ])
        .output();
    match output {
        Ok(o) => parse_dshow_audio_devices(&String::from_utf8_lossy(&o.stderr)),
        Err(_) => Vec::new(),
    }
}

/// 挑「最像真实麦克风」的默认设备:评分最高且为正;找不到返回 None,调用方降级纯视频。
/// 结果被 RecordingState 缓存(启动预热 + 设备失效后刷新),不随每次开始录制重复枚举。
#[cfg(windows)]
fn default_microphone(ffmpeg: &str) -> Option<String> {
    dshow_audio_devices(ffmpeg)
        .into_iter()
        .find(|(_, score)| *score > 0)
        .map(|(name, _)| name)
}

#[cfg(not(windows))]
fn dshow_audio_devices(_ffmpeg: &str) -> Vec<(String, i32)> {
    Vec::new()
}

#[cfg(not(windows))]
fn default_microphone(_ffmpeg: &str) -> Option<String> {
    None
}

/// 按平台拼 ffmpeg 录屏命令(全屏/单屏区域 + 编码器;mic 为 Some 时附带麦克风音轨)。
/// Windows 视频优先 ddagrab(DDA,省 CPU)否则 gdigrab(单屏加 offset/video_size 圈区域)、
/// 音频用 dshow(带降噪/动态放大滤镜链);编码优先硬编(nvenc/qsv/amf),兜底 libx264 ultrafast。
/// macOS 用 avfoundation(需『屏幕录制』权限,设备索引因机器而异,未实机验证);Linux 用 x11grab。
/// 非 Windows 平台暂不支持单屏区域与音频采集。
fn build_ffmpeg_command(program: &str, spec: &RecordSpec) -> std::process::Command {
    let mut cmd = std::process::Command::new(program);
    crate::media::hide_console_window(&mut cmd);
    cmd.arg("-y"); // 覆盖同名输出,避免交互确认卡住

    // 视频输入(全屏或指定单屏区域)
    #[cfg(windows)]
    {
        // thread_queue_size:每个输入的抓取线程队列默认只有 8 帧,慢机上处理跟不上就阻塞掉帧
        // (ffmpeg 打 "Thread message queue blocking"),加大到 512 帧缓冲吸收抖动
        cmd.args(["-thread_queue_size", "512"]);
        // ddagrab 与 gdigrab 参数面一致(offset/video_size 圈单屏区域;draw_mouse 显式开)
        cmd.args(["-f", if spec.ddagrab { "ddagrab" } else { "gdigrab" }]);
        cmd.args(["-framerate", FRAMERATE, "-draw_mouse", "1"]);
        if let Some(r) = &spec.region {
            // 区域采集:offset 定位到目标显示器左上角(可为负),video_size 圈定该屏。
            // 奇数尺寸由后面的 scale 滤镜裁偶,yuv420p 才能编码
            cmd.args([
                "-offset_x",
                &r.x.to_string(),
                "-offset_y",
                &r.y.to_string(),
                "-video_size",
                &format!("{}x{}", r.width, r.height),
            ]);
        }
        cmd.args(["-i", "desktop"]);
        if let Some(mic) = &spec.mic {
            // dshow 音频输入:buffer 加大到 80ms,默认缓冲偏小易断续/爆音;
            // 同样加大线程队列,慢机上音频打开慢不至于堵住视频输入线程
            cmd.args(["-thread_queue_size", "512"]);
            cmd.args([
                "-f",
                "dshow",
                "-audio_buffer_size",
                "80",
                "-i",
                &format!("audio={mic}"),
            ]);
        }
    }
    #[cfg(target_os = "macos")]
    {
        let _ = &spec.region; // macOS 暂不支持单屏选择
        let _ = spec.ddagrab;
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
        let _ = &spec.region; // Linux 暂不支持单屏选择
        let _ = spec.ddagrab;
        cmd.args(["-f", "x11grab", "-framerate", FRAMERATE, "-i", ":0.0"]);
    }

    // 视频滤镜:仅把奇数尺寸裁偶(yuv420p 要求);不加任何水印
    cmd.args(["-vf", "scale=trunc(iw/2)*2:trunc(ih/2)*2"]);
    // 编码:硬编(探测选定)把 4K 编码从 CPU 挪到 GPU;软编兜底用 ultrafast 降 CPU
    cmd.args(spec.encoder.encode_args());
    cmd.args(["-pix_fmt", spec.encoder.pix_fmt()]);
    if spec.mic.is_some() {
        // 语音滤镜链与音频测试共用同一常量(见 AUDIO_FILTER_CHAIN 注释),试听即成品听感
        cmd.args(["-af", AUDIO_FILTER_CHAIN]);
        cmd.args(["-c:a", "aac", "-b:a", "128k"]);
    }
    // moov 留在文件尾(不实时 faststart):实时重排会在停止收尾时整文件重读重写,
    // 录制越久「停止」越慢;改由停止成功后 remux_faststart 在后台重排到文件头
    cmd.arg(&spec.output);
    cmd
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

/// 分段文件是否有效:缺失 / 过小说明该段录制中途已失败(如 ffmpeg 崩溃),不应入列拼接
fn valid_segment(path: &std::path::Path) -> bool {
    std::fs::metadata(path)
        .map(|m| m.len() >= 4096)
        .unwrap_or(false)
}

/// 优雅收尾一个 ffmpeg 分段进程:向 stdin 写 `q` 让其写完 MP4 moov;超时未退则强杀。
/// 停止与暂停共用——关键是不能直接 kill,直接 kill 会留下缺 moov 的废文件(时长 0:00)。
fn finalize_child(mut child: Child) {
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(b"q");
        let _ = stdin.flush();
        drop(stdin); // 关闭管道,促使 ffmpeg 退出
    }
    let deadline = Instant::now() + STOP_GRACE;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break, // 正常退出
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    break;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(_) => {
                let _ = child.kill();
                break;
            }
        }
    }
}

/// 拼接分段(暂停 / 继续切出的多段):concat 协议 + `-c copy` 只拼封装不重编码,很快。
/// 各段录制参数完全相同(同编码 / 同帧率 / 同分辨率 / 同音频链),可直接流拷贝。
/// 先写 {stem}.merged.mp4,成功后删掉各分段再换名为最终文件;失败保留各分段并返回 false。
async fn concat_segments(
    program: &str,
    segments: &[std::path::PathBuf],
    final_path: &std::path::Path,
) -> bool {
    let (Some(dir), Some(stem)) = (
        final_path.parent(),
        final_path.file_stem().and_then(|s| s.to_str()),
    ) else {
        return false;
    };
    let list_path = dir.join(format!("{stem}.concat.txt"));
    let merged = dir.join(format!("{stem}.merged.mp4"));
    // concat 列表:单引号包裹绝对路径,路径内单引号按 ffmpeg 规则转义
    let list = segments
        .iter()
        .map(|p| format!("file '{}'", p.to_string_lossy().replace('\'', "'\\''")))
        .collect::<Vec<_>>()
        .join("\n");
    if std::fs::write(&list_path, list).is_err() {
        return false;
    }
    let program = program.to_string();
    let merged2 = merged.clone();
    let ok = tauri::async_runtime::spawn_blocking(move || {
        let mut cmd = std::process::Command::new(&program);
        crate::media::hide_console_window(&mut cmd);
        cmd.arg("-y")
            .args(["-f", "concat", "-safe", "0"])
            .arg("-i")
            .arg(&list_path)
            .args(["-c", "copy"])
            .arg(&merged2)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        cmd.status().map(|s| s.success()).unwrap_or(false)
    })
    .await
    .unwrap_or(false);
    // 列表文件只是中间产物,成败都清理
    let _ = std::fs::remove_file(dir.join(format!("{stem}.concat.txt")));
    if !ok || !valid_segment(&merged) {
        tracing::warn!("分段拼接失败,保留各分段文件: {}", final_path.display());
        let _ = std::fs::remove_file(&merged);
        return false;
    }
    // 拼接成功:清理分段(其中一段可能就是最终文件名,先删再换名,Windows 不能覆盖 rename)
    for seg in segments {
        let _ = std::fs::remove_file(seg);
    }
    if let Err(e) = std::fs::rename(&merged, final_path) {
        tracing::warn!("分段拼接产物换名失败: {e}");
        return false;
    }
    true
}

/// 停止后后台 faststart 重排:moov 搬到文件头(<video> 经 asset 协议才能读时长 / 拖动)。
/// 重排走同目录临时文件,成功后换名替换原文件;失败保留原文件(moov 在尾部,多数播放器仍可播)。
async fn remux_faststart(program: &str, path: &std::path::Path) {
    let (Some(dir), Some(stem)) = (path.parent(), path.file_stem().and_then(|s| s.to_str()))
    else {
        return;
    };
    let tmp = dir.join(format!("{stem}.faststart-tmp.mp4"));
    let program = program.to_string();
    let src = path.to_path_buf();
    let tmp2 = tmp.clone();
    // 整文件重读重写,阻塞 I/O 放 blocking 线程;-c copy 只换封装不重编码
    let ok = tauri::async_runtime::spawn_blocking(move || {
        let mut cmd = std::process::Command::new(&program);
        crate::media::hide_console_window(&mut cmd);
        cmd.arg("-y")
            .arg("-i")
            .arg(&src)
            .args(["-c", "copy", "-movflags", "+faststart"])
            .arg(&tmp2)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        cmd.status().map(|s| s.success()).unwrap_or(false)
    })
    .await
    .unwrap_or(false);
    let tmp_ok = ok && std::fs::metadata(&tmp).map(|m| m.len() >= 4096).unwrap_or(false);
    if !tmp_ok {
        tracing::warn!("faststart 重排失败,保留原始文件: {}", path.display());
        let _ = std::fs::remove_file(&tmp);
        return;
    }
    // Windows 的 rename 不能覆盖已存在的目标:先把原件换名为备份,重排版就位后再删备份
    let bak = dir.join(format!("{stem}.faststart-bak"));
    if std::fs::rename(path, &bak).is_err() {
        let _ = std::fs::remove_file(&tmp);
        return;
    }
    if std::fs::rename(&tmp, path).is_ok() {
        let _ = std::fs::remove_file(&bak);
    } else {
        // 换名失败回滚原件,丢弃重排版
        let _ = std::fs::rename(&bak, path);
        let _ = std::fs::remove_file(&tmp);
    }
}

/// 后台预热录屏探测缓存(lib.rs 启动时调用;麦克风设备失效重试路径也会调它刷新):
/// ① 默认麦克风枚举;② 编码器实测初始化探测(nvenc/qsv/amf,兜底 libx264);③ ddagrab 抓屏探测。
/// 这些都要起 ffmpeg 子进程(合计近秒级),提前在启动空闲期做掉,首次「开始录制」直接读缓存。
pub async fn warm_recording_probes(app: AppHandle) {
    let (available, program) = {
        let state = app.state::<AppState>();
        let program = lock_config(&state)
            .ok()
            .and_then(|cfg| cfg.media.ffmpeg_path.clone());
        (state.recording.ffmpeg_available(), program)
    };
    if !available {
        return;
    }
    let program = program
        .as_deref()
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .unwrap_or("ffmpeg")
        .to_string();
    // 探测全是阻塞子进程调用,放 blocking 线程一次做完
    let prog = program.clone();
    let (mic, encoder, ddagrab) = tauri::async_runtime::spawn_blocking(move || {
        (
            default_microphone(&prog),
            probe_video_encoder(&prog),
            probe_ddagrab(&prog),
        )
    })
    .await
    .unwrap_or((None, VideoEncoder::Software, false));
    tracing::info!(
        "录屏预热:麦克风={} 编码器={} 抓屏={}",
        mic.as_deref().unwrap_or("(无可用设备,将降级纯视频)"),
        encoder.label(),
        if ddagrab { "ddagrab" } else { "gdigrab" },
    );
    let state = app.state::<AppState>();
    state.recording.set_default_mic(mic);
    state.recording.set_video_probe(encoder, ddagrab);
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
    position_overlay(&overlay, OVERLAY_W);
    // 把悬浮条排除出屏幕捕获,使其不被录进视频(Windows)
    #[cfg(windows)]
    exclude_overlay_from_capture(&overlay);
    Ok(())
}

/// 把悬浮窗摆到主显示器顶部居中(高 DPI 下用缩放因子换算物理像素;按给定逻辑宽度算居中)。
fn position_overlay(overlay: &tauri::WebviewWindow, logical_w: f64) {
    let scale = overlay.scale_factor().unwrap_or(1.0);
    let w = logical_w * scale;
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
