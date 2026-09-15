//! 内容创作 - 提示词管理的 CRUD 命令(两级:分类目录 → 分镜镜头提示词)。
//!
//! ID 由前端生成(crypto.randomUUID)并随请求传入,后端按 `id` 是否已存在区分新增 / 更新。
//! 数据按 owner 归属:list 命令在 dataScope=="self" 时只返回当前用户自己的;逻辑外键,无物理 FK。

use crate::commands::AppState;
use chrono::Utc;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, EntityTrait, IntoActiveModel, QueryFilter, QueryOrder,
    QuerySelect, Set,
};
use serde::{Deserialize, Serialize};
use tauri::{Emitter, State};
use veltrix_core::db::entity::{prompt_category, shot_prompt};
use veltrix_core::error::{CrawlerError, Result};

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read};
use std::process::{Output, Stdio};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex, OnceLock,
};
use std::time::{Duration, Instant};

/// 单次 list 接口最多返回 N 行,防 IPC 噎住;数据量超出应改分页接口。
const LIST_HARD_CAP: u64 = 1000;

static EXPORT_JOBS: OnceLock<Mutex<HashMap<String, Arc<AtomicBool>>>> = OnceLock::new();
static EXPORT_JOB_STATUS: OnceLock<Mutex<HashMap<String, ExportProgressEvent>>> = OnceLock::new();
static EXPORT_QUEUE_SLOT: OnceLock<Arc<tokio::sync::Semaphore>> = OnceLock::new();

fn export_jobs() -> &'static Mutex<HashMap<String, Arc<AtomicBool>>> {
    EXPORT_JOBS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn export_job_status() -> &'static Mutex<HashMap<String, ExportProgressEvent>> {
    EXPORT_JOB_STATUS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn export_queue_slot() -> Arc<tokio::sync::Semaphore> {
    EXPORT_QUEUE_SLOT
        .get_or_init(|| Arc::new(tokio::sync::Semaphore::new(1)))
        .clone()
}

#[derive(Clone)]
struct ExportRuntime {
    job_id: String,
    cancelled: Arc<AtomicBool>,
    app: tauri::AppHandle,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportProgressEvent {
    job_id: String,
    percent: f64,
    stage: String,
    status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    output_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

fn publish_export_event(app: &tauri::AppHandle, event: ExportProgressEvent) {
    // 最终状态也保留到用户主动收起，避免切回首页期间完成/失败后丢失结果。
    if let Ok(mut jobs) = export_job_status().lock() {
        jobs.insert(event.job_id.clone(), event.clone());
    }
    let _ = app.emit("creation-export-progress", event);
}

impl ExportRuntime {
    fn report(&self, percent: f64, stage: impl Into<String>) {
        publish_export_event(
            &self.app,
            ExportProgressEvent {
                job_id: self.job_id.clone(),
                percent: percent.clamp(0.0, 100.0),
                stage: stage.into(),
                status: "running".into(),
                output_path: None,
                error: None,
            },
        );
    }
}

// ===================== 提示词分类目录 =====================

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptCategoryView {
    pub id: String,
    pub owner: String,
    pub name: String,
    pub remark: String,
    pub created_at: i64,
    pub updated_at: i64,
}

impl From<prompt_category::Model> for PromptCategoryView {
    fn from(m: prompt_category::Model) -> Self {
        Self {
            id: m.id,
            owner: m.owner,
            name: m.name,
            remark: m.remark,
            created_at: m.created_at,
            updated_at: m.updated_at,
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptCategoryInput {
    pub id: String,
    pub name: String,
    pub remark: String,
}

#[tauri::command]
pub async fn list_prompt_categories(state: State<'_, AppState>) -> Result<Vec<PromptCategoryView>> {
    // 先取出当前用户(克隆后释放锁),再异步查询,避免跨 await 持锁
    let user = super::current_user(&state);
    let mut query =
        prompt_category::Entity::find().order_by_asc(prompt_category::Column::CreatedAt);
    // scope=="self" 只返回自己的;"all" 或未登录返回全部
    if let Some(u) = &user {
        if u.scope == "self" {
            query = query.filter(prompt_category::Column::Owner.eq(u.name.clone()));
        }
    }
    let rows = query
        .limit(LIST_HARD_CAP)
        .all(&state.db)
        .await
        .map_err(|e| CrawlerError::Config(format!("查询提示词分类失败: {e}")))?;
    Ok(rows.into_iter().map(Into::into).collect())
}

#[tauri::command]
pub async fn upsert_prompt_category(
    state: State<'_, AppState>,
    category: PromptCategoryInput,
) -> Result<()> {
    let db = &state.db;
    let now = Utc::now().timestamp();
    let existing = prompt_category::Entity::find_by_id(category.id.clone())
        .one(db)
        .await
        .map_err(|e| CrawlerError::Config(format!("查询提示词分类失败: {e}")))?;
    match existing {
        Some(model) => {
            // 编辑:owner 不随编辑变更,保留原值
            let mut am = model.into_active_model();
            am.name = Set(category.name);
            am.remark = Set(category.remark);
            am.updated_at = Set(now);
            am.update(db)
                .await
                .map_err(|e| CrawlerError::Config(format!("更新提示词分类失败: {e}")))?;
        }
        None => {
            // 新建归属由后端会话决定:有当前用户则记其用户名,无则回退空串(兼容调试)
            let owner = super::current_user(&state)
                .map(|u| u.name)
                .unwrap_or_default();
            let am = prompt_category::ActiveModel {
                id: Set(category.id),
                owner: Set(owner),
                name: Set(category.name),
                remark: Set(category.remark),
                created_at: Set(now),
                updated_at: Set(now),
            };
            am.insert(db)
                .await
                .map_err(|e| CrawlerError::Config(format!("创建提示词分类失败: {e}")))?;
        }
    }
    Ok(())
}

#[tauri::command]
pub async fn remove_prompt_category(state: State<'_, AppState>, id: String) -> Result<()> {
    let db = &state.db;
    // 逻辑外键无物理级联,删除分类时手动级联删除其下提示词
    shot_prompt::Entity::delete_many()
        .filter(shot_prompt::Column::CategoryId.eq(id.clone()))
        .exec(db)
        .await
        .map_err(|e| CrawlerError::Config(format!("删除分类下提示词失败: {e}")))?;
    prompt_category::Entity::delete_by_id(id)
        .exec(db)
        .await
        .map_err(|e| CrawlerError::Config(format!("删除提示词分类失败: {e}")))?;
    Ok(())
}

// ===================== 分镜镜头提示词 =====================

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ShotPromptView {
    pub id: String,
    pub owner: String,
    pub category_id: String,
    pub name: String,
    pub content: String,
    pub remark: String,
    pub created_at: i64,
    pub updated_at: i64,
}

impl From<shot_prompt::Model> for ShotPromptView {
    fn from(m: shot_prompt::Model) -> Self {
        Self {
            id: m.id,
            owner: m.owner,
            category_id: m.category_id,
            name: m.name,
            content: m.content,
            remark: m.remark,
            created_at: m.created_at,
            updated_at: m.updated_at,
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShotPromptInput {
    pub id: String,
    pub category_id: String,
    pub name: String,
    pub content: String,
    pub remark: String,
}

#[tauri::command]
pub async fn list_shot_prompts(
    state: State<'_, AppState>,
    category_id: String,
) -> Result<Vec<ShotPromptView>> {
    let user = super::current_user(&state);
    let mut query = shot_prompt::Entity::find()
        .filter(shot_prompt::Column::CategoryId.eq(category_id))
        .order_by_asc(shot_prompt::Column::CreatedAt);
    if let Some(u) = &user {
        if u.scope == "self" {
            query = query.filter(shot_prompt::Column::Owner.eq(u.name.clone()));
        }
    }
    let rows = query
        .limit(LIST_HARD_CAP)
        .all(&state.db)
        .await
        .map_err(|e| CrawlerError::Config(format!("查询提示词失败: {e}")))?;
    Ok(rows.into_iter().map(Into::into).collect())
}

#[tauri::command]
pub async fn upsert_shot_prompt(state: State<'_, AppState>, prompt: ShotPromptInput) -> Result<()> {
    let db = &state.db;
    let now = Utc::now().timestamp();
    let existing = shot_prompt::Entity::find_by_id(prompt.id.clone())
        .one(db)
        .await
        .map_err(|e| CrawlerError::Config(format!("查询提示词失败: {e}")))?;
    match existing {
        Some(model) => {
            let mut am = model.into_active_model();
            am.name = Set(prompt.name);
            am.content = Set(prompt.content);
            am.remark = Set(prompt.remark);
            am.updated_at = Set(now);
            am.update(db)
                .await
                .map_err(|e| CrawlerError::Config(format!("更新提示词失败: {e}")))?;
        }
        None => {
            let owner = super::current_user(&state)
                .map(|u| u.name)
                .unwrap_or_default();
            let am = shot_prompt::ActiveModel {
                id: Set(prompt.id),
                owner: Set(owner),
                category_id: Set(prompt.category_id),
                name: Set(prompt.name),
                content: Set(prompt.content),
                remark: Set(prompt.remark),
                created_at: Set(now),
                updated_at: Set(now),
            };
            am.insert(db)
                .await
                .map_err(|e| CrawlerError::Config(format!("创建提示词失败: {e}")))?;
        }
    }
    Ok(())
}

#[tauri::command]
pub async fn remove_shot_prompt(state: State<'_, AppState>, id: String) -> Result<()> {
    shot_prompt::Entity::delete_by_id(id)
        .exec(&state.db)
        .await
        .map_err(|e| CrawlerError::Config(format!("删除提示词失败: {e}")))?;
    Ok(())
}

// ===================== 视频剪辑导出 =====================

/// 一个剪辑片段(秒;camelCase 对齐 TS ClipSegment)。
/// position = 时间轴(序列)位置,音轨混音按它做 adelay 定位;缺省(旧前端)回退按顺序拼接。
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClipSegment {
    pub start: f64,
    pub end: f64,
    #[serde(default)]
    pub position: Option<f64>,
    /// 片段自己的视频或音频素材路径;为空时使用工程主视频。
    #[serde(default)]
    pub input_path: Option<String>,
    /// 视频片段自己的裁剪与画面变换;音频片段忽略。
    #[serde(default)]
    pub transform: Option<VideoTransformInput>,
    /// 仅关闭本片段原声;解决多视频轨中部分轨道分离音频后的重复播放。
    #[serde(default)]
    pub mute_original: bool,
    /// 视频合成层级;数值越大越靠上。旧工程缺省为 0。
    #[serde(default)]
    pub layer: i32,
    /// 相对片段起点的缩放/位置关键帧，按时间线线性插值。
    #[serde(default)]
    pub keyframes: Vec<VideoKeyframeInput>,
    /// 音频片段的混音参数；视频片段暂不使用。
    #[serde(default = "default_audio_volume")]
    pub volume: f64,
    #[serde(default)]
    pub pan: f64,
    #[serde(default)]
    pub fade_in: f64,
    #[serde(default)]
    pub fade_out: f64,
    #[serde(default = "default_clip_speed")]
    pub speed: f64,
    #[serde(default)]
    pub transition_in: Option<TransitionInput>,
}

fn default_audio_volume() -> f64 {
    1.0
}

fn default_clip_speed() -> f64 {
    1.0
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VideoKeyframeInput {
    #[serde(rename = "id")]
    pub _id: String,
    pub offset: f64,
    pub scale: f64,
    pub position_x: f64,
    pub position_y: f64,
    #[serde(default)]
    pub easing: Option<String>,
}

/// 画面变换值使用前端友好的单位:裁剪/位置为百分比,scale 为倍率。
#[derive(Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VideoTransformInput {
    #[serde(default)]
    pub rotation: Option<i32>,
    #[serde(default)]
    pub scale: Option<f64>,
    #[serde(default)]
    pub position_x: Option<f64>,
    #[serde(default)]
    pub position_y: Option<f64>,
    #[serde(default)]
    pub crop_top: Option<f64>,
    #[serde(default)]
    pub crop_right: Option<f64>,
    #[serde(default)]
    pub crop_bottom: Option<f64>,
    #[serde(default)]
    pub crop_left: Option<f64>,
    #[serde(default)]
    pub opacity: Option<f64>,
    #[serde(default)]
    pub brightness: Option<f64>,
    #[serde(default)]
    pub contrast: Option<f64>,
    #[serde(default)]
    pub saturation: Option<f64>,
    #[serde(default)]
    pub temperature: Option<f64>,
    #[serde(default)]
    pub hue: Option<f64>,
    #[serde(default)]
    pub filter: Option<String>,
}

#[derive(Clone)]
struct TimelineVideoSegment {
    start: f64,
    end: f64,
    position: f64,
    path: String,
    transform: Option<VideoTransformInput>,
    muted: bool,
    layer: i32,
    keyframes: Vec<VideoKeyframeInput>,
    speed: f64,
    transition_in: Option<TransitionInput>,
}

#[derive(Clone)]
struct TimelineAudioSegment {
    start: f64,
    end: f64,
    position: Option<f64>,
    path: String,
    volume: f64,
    pan: f64,
    fade_in: f64,
    fade_out: f64,
    speed: f64,
}

fn atempo_suffix(speed: f64) -> String {
    let mut remaining = speed.clamp(0.25, 4.0);
    let mut filters = Vec::new();
    while remaining > 2.0 + 0.001 {
        filters.push("atempo=2.0".to_string());
        remaining /= 2.0;
    }
    while remaining < 0.5 - 0.001 {
        filters.push("atempo=0.5".to_string());
        remaining /= 0.5;
    }
    if (remaining - 1.0).abs() > 0.001 {
        filters.push(format!("atempo={remaining:.6}"));
    }
    if filters.is_empty() {
        String::new()
    } else {
        format!(",{}", filters.join(","))
    }
}

/// 把片段级音量、声像、淡入淡出收敛成一条可复用的 FFmpeg 音频链。
fn audio_segment_filters(segment: &TimelineAudioSegment, delay_ms: f64) -> String {
    let duration = (segment.end - segment.start).max(0.0) / segment.speed.clamp(0.25, 4.0);
    let fade_in = segment.fade_in.clamp(0.0, duration);
    let fade_out = segment.fade_out.clamp(0.0, duration);
    let fade_out_start = (duration - fade_out).max(0.0);
    let pan = segment.pan.clamp(-1.0, 1.0);
    let left = if pan > 0.0 { 1.0 - pan } else { 1.0 };
    let right = if pan < 0.0 { 1.0 + pan } else { 1.0 };
    let mut filters = format!(
        "atrim=start={:.3}:end={:.3},asetpts=PTS-STARTPTS{},volume={:.6},pan=stereo|c0={left:.6}*c0|c1={right:.6}*c1",
        segment.start,
        segment.end,
        atempo_suffix(segment.speed),
        segment.volume.clamp(0.0, 2.0),
    );
    if fade_in > 0.001 {
        filters.push_str(&format!(",afade=t=in:st=0:d={fade_in:.3}"));
    }
    if fade_out > 0.001 {
        filters.push_str(&format!(
            ",afade=t=out:st={fade_out_start:.3}:d={fade_out:.3}"
        ));
    }
    filters.push_str(&format!(",adelay={delay_ms:.0}:all=1"));
    filters
}

/// 转场参数:kind = dissolve(叠化)/ fade(淡黑);duration_secs 转场时长(秒)。
/// 为空 = 不加转场,保持 -c copy 快路径。
#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TransitionInput {
    pub kind: String,
    pub duration_secs: f64,
}

fn effective_transition<'a>(
    own: Option<&'a TransitionInput>,
    global: Option<&'a TransitionInput>,
) -> Option<&'a TransitionInput> {
    own.or(global)
        .filter(|value| value.kind != "none" && value.duration_secs > 0.001)
}

/// 导出缩放目标(宽高均为偶数;由前端按源分辨率与目标档位换算,不做放大)。
#[derive(Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScaleInput {
    pub width: u32,
    pub height: u32,
}

/// 时间轴文字覆盖层;start/end 使用成片序列时间,样式字段均有安全默认值。
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TextOverlay {
    pub start: f64,
    pub end: f64,
    pub text: String,
    #[serde(default)]
    pub font_size: Option<u32>,
    #[serde(default)]
    pub color: Option<String>,
    #[serde(default)]
    pub position: Option<String>,
    #[serde(default)]
    pub x: Option<f64>,
    #[serde(default)]
    pub y: Option<f64>,
    #[serde(default)]
    pub rotation: Option<f64>,
}

/// 取消正在运行的剪辑导出。任务不存在时保持幂等,便于窗口卸载或重复点击取消。
#[tauri::command]
pub fn creation_cancel_export(job_id: String) -> Result<()> {
    if let Ok(jobs) = export_jobs().lock() {
        if let Some(cancelled) = jobs.get(&job_id) {
            cancelled.store(true, Ordering::Release);
        }
    }
    Ok(())
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreationExportRequest {
    input_path: String,
    segments: Vec<ClipSegment>,
    #[serde(default)]
    audio_segments: Vec<ClipSegment>,
    transition: Option<TransitionInput>,
    scale: Option<ScaleInput>,
    quality: Option<String>,
    mute_original: Option<bool>,
    #[serde(default)]
    text_overlays: Vec<TextOverlay>,
    job_id: String,
}

/// 后台导出入口：请求立即入队并返回，单并发执行避免多个编码任务抢满 CPU/GPU。
#[tauri::command]
pub async fn creation_start_export(
    state: State<'_, AppState>,
    app: tauri::AppHandle,
    input: CreationExportRequest,
) -> Result<()> {
    use tauri::Manager;
    let program = ffmpeg_program(&state)?;
    let dir = app
        .path()
        .app_data_dir()
        .map_err(|e| CrawlerError::Config(format!("定位数据目录失败: {e}")))?
        .join("exports");
    let job_id = input.job_id.trim().to_string();
    if job_id.is_empty() || job_id.len() > 128 {
        return Err(CrawlerError::Config("导出任务标识无效".into()));
    }
    let cancelled = Arc::new(AtomicBool::new(false));
    {
        let mut jobs = export_jobs()
            .lock()
            .map_err(|_| CrawlerError::Config("导出任务状态不可用".into()))?;
        if jobs.contains_key(&job_id) {
            return Err(CrawlerError::Config("导出任务已存在".into()));
        }
        jobs.insert(job_id.clone(), cancelled.clone());
    }
    publish_export_event(
        &app,
        ExportProgressEvent {
            job_id: job_id.clone(),
            percent: 0.0,
            stage: "等待前一个任务完成".into(),
            status: "queued".into(),
            output_path: None,
            error: None,
        },
    );
    let crf = match input.quality.as_deref() {
        Some("high") => 18,
        Some("low") => 27,
        _ => 21,
    };
    tauri::async_runtime::spawn(async move {
        let permit = export_queue_slot().acquire_owned().await;
        let runtime = ExportRuntime {
            job_id: job_id.clone(),
            cancelled: cancelled.clone(),
            app: app.clone(),
        };
        let result = if permit.is_err() {
            Err(CrawlerError::Config("导出队列不可用".into()))
        } else if cancelled.load(Ordering::Acquire) {
            Err(CrawlerError::Config("导出已取消".into()))
        } else {
            runtime.report(1.0, "正在准备素材");
            tauri::async_runtime::spawn_blocking(move || {
                export_video_clips(
                    &program,
                    &dir,
                    &input.input_path,
                    &input.segments,
                    &input.audio_segments,
                    input.transition.as_ref(),
                    input
                        .scale
                        .map(|value| (value.width, value.height))
                        .as_ref(),
                    crf,
                    input.mute_original.unwrap_or(false),
                    &input.text_overlays,
                    Some(&runtime),
                )
            })
            .await
            .map_err(|error| CrawlerError::Config(format!("剪辑导出异常: {error}")))
            .and_then(|result| result)
        };
        if let Ok(mut jobs) = export_jobs().lock() {
            jobs.remove(&job_id);
        }
        match result {
            Ok(path) => publish_export_event(
                &app,
                ExportProgressEvent {
                    job_id,
                    percent: 100.0,
                    stage: "导出完成".into(),
                    status: "completed".into(),
                    output_path: Some(path),
                    error: None,
                },
            ),
            Err(error) => {
                let message = error.to_string();
                let cancelled = message.contains("导出已取消");
                publish_export_event(
                    &app,
                    ExportProgressEvent {
                        job_id,
                        percent: 0.0,
                        stage: if cancelled {
                            "已取消"
                        } else {
                            "导出失败"
                        }
                        .into(),
                        status: if cancelled { "cancelled" } else { "failed" }.into(),
                        output_path: None,
                        error: (!cancelled).then_some(message),
                    },
                );
            }
        }
    });
    Ok(())
}

/// 页面重新进入时恢复仍在排队或执行中的后台任务。
#[tauri::command]
pub fn creation_list_active_exports() -> Vec<ExportProgressEvent> {
    export_job_status()
        .lock()
        .map(|jobs| jobs.values().cloned().collect())
        .unwrap_or_default()
}

/// 用户从任务坞收起已结束任务；运行中的任务不能被误删，只能走取消。
#[tauri::command]
pub fn creation_dismiss_export_job(job_id: String) -> Result<()> {
    if let Ok(mut jobs) = export_job_status().lock() {
        if jobs
            .get(&job_id)
            .is_some_and(|job| matches!(job.status.as_str(), "completed" | "failed" | "cancelled"))
        {
            jobs.remove(&job_id);
        }
    }
    Ok(())
}

/// 运行导出 FFmpeg 并读取 `-progress pipe:1` 的媒体时间。百分比来自已编码时间,
/// 不用定时器猜测；轮询子进程的同时响应取消,避免长视频只能强制关闭应用。
fn run_export_ffmpeg(
    cmd: &mut std::process::Command,
    runtime: Option<&ExportRuntime>,
    media_duration: f64,
    base_percent: f64,
    percent_span: f64,
    stage: &str,
) -> Result<Output> {
    let Some(runtime) = runtime else {
        return crate::media::run_ffmpeg_local(cmd)
            .map_err(|e| CrawlerError::Config(format!("FFmpeg 执行失败: {e}")));
    };
    if runtime.cancelled.load(Ordering::Acquire) {
        return Err(CrawlerError::Config("导出已取消".into()));
    }
    runtime.report(base_percent, stage);
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd
        .spawn()
        .map_err(|e| CrawlerError::Config(format!("启动 FFmpeg 失败: {e}")))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| CrawlerError::Config("读取 FFmpeg 进度失败".into()))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| CrawlerError::Config("读取 FFmpeg 日志失败".into()))?;
    let (tx, rx) = std::sync::mpsc::channel::<f64>();
    let stdout_reader = std::thread::spawn(move || {
        let mut raw = Vec::new();
        for line in BufReader::new(stdout).lines().map_while(|line| line.ok()) {
            raw.extend_from_slice(line.as_bytes());
            raw.push(b'\n');
            if let Some(value) = line.strip_prefix("out_time=") {
                let _ = tx.send(parse_ffmpeg_duration(value));
            }
        }
        raw
    });
    let stderr_reader = std::thread::spawn(move || {
        let mut raw = Vec::new();
        let _ = BufReader::new(stderr).read_to_end(&mut raw);
        raw
    });
    // 剪辑导出允许长时间编码；六小时仅用于处理 FFmpeg 卡死,不是视频时长限制。
    let deadline = Instant::now() + Duration::from_secs(6 * 60 * 60);
    let status = loop {
        while let Ok(seconds) = rx.try_recv() {
            let ratio = if media_duration > 0.001 {
                (seconds / media_duration).clamp(0.0, 1.0)
            } else {
                0.0
            };
            runtime.report(base_percent + percent_span * ratio, stage);
        }
        if runtime.cancelled.load(Ordering::Acquire) {
            let _ = child.kill();
            let _ = child.wait();
            let _ = stdout_reader.join();
            let _ = stderr_reader.join();
            runtime.report(base_percent, "已取消");
            return Err(CrawlerError::Config("导出已取消".into()));
        }
        if let Some(status) = child
            .try_wait()
            .map_err(|e| CrawlerError::Config(format!("等待 FFmpeg 失败: {e}")))?
        {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            let _ = stdout_reader.join();
            let _ = stderr_reader.join();
            return Err(CrawlerError::Config("导出超时,已终止 FFmpeg".into()));
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let stdout = stdout_reader.join().unwrap_or_default();
    let stderr = stderr_reader.join().unwrap_or_default();
    runtime.report(base_percent + percent_span, stage);
    Ok(Output {
        status,
        stdout,
        stderr,
    })
}

/// 剪辑导出:视频轨片段逐段 `-ss -t -c copy` 剪切(不重编码,快;切口对齐关键帧,
/// 可能有百毫秒级偏差),多段用 concat 协议 `-c copy` 拼接;有音频轨叠加段或指定
/// 分辨率缩放时改走 filter_complex 单遍重编码(视频轨 concat + 音频轨 amix 混音,
/// libx264 veryfast)。文字层通过 drawtext 烧录进画面。产物落 <app_data>/exports/clip-<时间戳>.mp4。
#[tauri::command]
pub async fn creation_export_video(
    state: State<'_, AppState>,
    app: tauri::AppHandle,
    input_path: String,
    segments: Vec<ClipSegment>,
    audio_segments: Vec<ClipSegment>,
    transition: Option<TransitionInput>,
    scale: Option<ScaleInput>,
    quality: Option<String>,
    mute_original: Option<bool>,
    text_overlays: Vec<TextOverlay>,
    job_id: String,
) -> Result<String> {
    use tauri::Manager;
    let program = {
        let cfg = super::lock_config(&state).map_err(|e| CrawlerError::Config(e.to_string()))?;
        cfg.media
            .ffmpeg_path
            .as_deref()
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .unwrap_or("ffmpeg")
            .to_string()
    };
    // 画质档位 → CRF(high 体积大质量高 / low 相反);medium 与既有默认一致
    let crf = match quality.as_deref() {
        Some("high") => 18,
        Some("low") => 27,
        _ => 21,
    };
    let dir = app
        .path()
        .app_data_dir()
        .map_err(|e| CrawlerError::Config(format!("定位数据目录失败: {e}")))?
        .join("exports");
    let job_id = job_id.trim().to_string();
    if job_id.is_empty() || job_id.len() > 128 {
        return Err(CrawlerError::Config("导出任务标识无效".into()));
    }
    let cancelled = Arc::new(AtomicBool::new(false));
    export_jobs()
        .lock()
        .map_err(|_| CrawlerError::Config("导出任务状态不可用".into()))?
        .insert(job_id.clone(), cancelled.clone());
    let runtime = ExportRuntime {
        job_id: job_id.clone(),
        cancelled,
        app: app.clone(),
    };
    runtime.report(1.0, "正在准备素材");
    // 剪切 + 拼接是重 I/O 子进程调用,放 blocking 线程
    let task_result = tauri::async_runtime::spawn_blocking(move || {
        export_video_clips(
            &program,
            &dir,
            &input_path,
            &segments,
            &audio_segments,
            transition.as_ref(),
            scale.map(|s| (s.width, s.height)).as_ref(),
            crf,
            mute_original.unwrap_or(false),
            &text_overlays,
            Some(&runtime),
        )
    })
    .await;
    if let Ok(mut jobs) = export_jobs().lock() {
        jobs.remove(&job_id);
    }
    let result = task_result.map_err(|e| CrawlerError::Config(format!("剪辑导出异常: {e}")))?;
    if let Ok(path) = &result {
        publish_export_event(
            &app,
            ExportProgressEvent {
                job_id,
                percent: 100.0,
                stage: "导出完成".into(),
                status: "completed".into(),
                output_path: Some(path.clone()),
                error: None,
            },
        );
    }
    result
}

/// 导出执行体(blocking 线程):校验 → 逐段剪切 → 拼接 → 清理中间文件。
/// scale = 目标分辨率(需重编码,分流拷贝快路径);crf 作用于所有重编码路径;
/// mute_original = 丢弃视频轨原声(流拷贝路径用 -an 剥离,重编码路径不再混入原声)。
fn export_video_clips(
    program: &str,
    dir: &std::path::Path,
    input_path: &str,
    segments: &[ClipSegment],
    audio_segments: &[ClipSegment],
    transition: Option<&TransitionInput>,
    scale: Option<&(u32, u32)>,
    crf: u32,
    mute_original: bool,
    text_overlays: &[TextOverlay],
    runtime: Option<&ExportRuntime>,
) -> Result<String> {
    let input = std::path::Path::new(input_path);
    if !input.is_file() {
        return Err(CrawlerError::Config(format!("源视频不存在: {input_path}")));
    }
    // 超限必须显式报错,不能静默截断——时间轴与成片少片段比导出失败更危险。
    if segments.len() > 64 || audio_segments.len() > 64 {
        return Err(CrawlerError::Config(
            "单次导出最多支持 64 个视频片段和 64 个音频片段".into(),
        ));
    }
    // 过滤无效片段(起点为负 / 时长过短);position 缺省时按旧版顺序紧凑排列。
    let mut fallback_position = 0.0;
    let timeline_segs: Vec<TimelineVideoSegment> = segments
        .iter()
        .filter_map(|s| {
            let start = s.start.max(0.0);
            if s.end - start <= 0.05 {
                return None;
            }
            let path = s
                .input_path
                .as_deref()
                .map(str::trim)
                .filter(|p| !p.is_empty())
                .unwrap_or(input_path)
                .to_owned();
            let position = s.position.unwrap_or(fallback_position).max(0.0);
            let speed = s.speed.clamp(0.25, 4.0);
            fallback_position = position + (s.end - start) / speed;
            Some(TimelineVideoSegment {
                start,
                end: s.end,
                position,
                path,
                transform: s.transform.clone(),
                muted: s.mute_original,
                layer: s.layer.max(0),
                keyframes: s.keyframes.clone(),
                speed,
                transition_in: s.transition_in.clone(),
            })
        })
        .collect();
    if timeline_segs.is_empty() {
        return Err(CrawlerError::Config("没有有效的剪辑片段".into()));
    }
    let segs: Vec<(
        f64,
        f64,
        String,
        Option<VideoTransformInput>,
        bool,
        f64,
        Option<TransitionInput>,
    )> = timeline_segs
        .iter()
        .map(|s| {
            (
                s.start,
                s.end,
                s.path.clone(),
                s.transform.clone(),
                s.muted,
                s.speed,
                s.transition_in.clone(),
            )
        })
        .collect();
    // 音频叠加段:(start, end, position)——position 驱动混音的 adelay 定位(真多轨混音)
    for (_, _, path, _, _, _, _) in &segs {
        if !std::path::Path::new(path).is_file() {
            return Err(CrawlerError::Config(format!("视频素材不存在: {path}")));
        }
    }
    let overlay: Vec<TimelineAudioSegment> = audio_segments
        .iter()
        .map(|s| {
            let path = s
                .input_path
                .as_deref()
                .map(str::trim)
                .filter(|p| !p.is_empty())
                .unwrap_or(input_path)
                .to_owned();
            let duration = (s.end - s.start.max(0.0)).max(0.0);
            TimelineAudioSegment {
                start: s.start.max(0.0),
                end: s.end,
                position: s.position,
                path,
                volume: s.volume.clamp(0.0, 2.0),
                pan: s.pan.clamp(-1.0, 1.0),
                fade_in: s.fade_in.clamp(0.0, duration),
                fade_out: s.fade_out.clamp(0.0, duration),
                speed: s.speed.clamp(0.25, 4.0),
            }
        })
        .filter(|segment| segment.end - segment.start > 0.05)
        .collect();
    for segment in &overlay {
        if !std::path::Path::new(&segment.path).is_file() {
            return Err(CrawlerError::Config(format!(
                "音频素材不存在: {}",
                segment.path
            )));
        }
    }
    std::fs::create_dir_all(dir)
        .map_err(|e| CrawlerError::Config(format!("创建导出目录失败: {e}")))?;
    // 毫秒后缀避免同一秒内连续导出互相覆盖。
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S-%3f");
    let stem = format!("clip-{stamp}");
    let final_path = dir.join(format!("{stem}.mp4"));
    let multi_source = segs
        .first()
        .is_some_and(|first| segs.iter().any(|seg| seg.2 != first.2));
    let has_transforms = segs.iter().any(|seg| transform_is_active(seg.3.as_ref()));
    let has_opacity = timeline_segs.iter().any(|seg| {
        (seg.transform
            .as_ref()
            .and_then(|value| value.opacity)
            .unwrap_or(1.0)
            - 1.0)
            .abs()
            > 0.001
    });
    let has_timeline_gaps_or_overlaps = {
        let mut ordered = timeline_segs.clone();
        ordered.sort_by(|a, b| a.position.total_cmp(&b.position));
        let mut cursor = 0.0;
        let mut changed = false;
        for seg in &ordered {
            if (seg.position - cursor).abs() > 0.01 {
                changed = true;
                break;
            }
            cursor += (seg.end - seg.start) / seg.speed;
        }
        changed
    };
    let has_keyframes = timeline_segs.iter().any(|seg| !seg.keyframes.is_empty());
    let timeline_composite = has_opacity || has_timeline_gaps_or_overlaps || has_keyframes;
    let has_segment_mutes = !mute_original && segs.iter().any(|seg| seg.4);
    let has_speed_changes = segs.iter().any(|seg| (seg.5 - 1.0).abs() > 0.001);
    let has_transitions = segs
        .iter()
        .enumerate()
        .skip(1)
        .any(|(_, seg)| effective_transition(seg.6.as_ref(), transition).is_some());
    // 多源视频即使没有主动降分辨率,也要统一到主视频画布后才能安全 concat/xfade。
    let fallback_scale =
        if (multi_source || has_transforms || timeline_composite) && scale.is_none() {
            probe_video_info(program, input_path)
                .ok()
                .map(|info| (info.width, info.height))
        } else {
            None
        };
    let render_scale = scale.or(fallback_scale.as_ref());
    if has_transforms && render_scale.is_none() {
        return Err(CrawlerError::Config(
            "无法读取主视频尺寸,不能应用画面变换".into(),
        ));
    }

    if timeline_composite {
        if has_transitions {
            return Err(CrawlerError::Config(
                "画中画、关键帧或空白时间轴暂不能同时使用片段转场".into(),
            ));
        }
        return export_with_video_layers(
            program,
            &final_path,
            &timeline_segs,
            &overlay,
            render_scale.ok_or_else(|| CrawlerError::Config("无法确定合成画布尺寸".into()))?,
            crf,
            mute_original,
            text_overlays,
            runtime,
        );
    }

    // 带转场(多段时):单遍 filter_complex 完成画面转场、原声渐变与音轨叠加。
    if has_transitions && segs.len() >= 2 {
        if segs.len() > 24 {
            return Err(CrawlerError::Config(
                "片段超过 24 段暂不支持加转场,请先精简片段".into(),
            ));
        }
        return export_with_transitions(
            program,
            &final_path,
            &segs,
            &overlay,
            transition,
            render_scale,
            crf,
            mute_original,
            text_overlays,
            runtime,
        );
    }

    // 有音频轨叠加段,或指定了缩放:单遍 filter_complex 重编码
    // (视频轨 concat + 音频轨混音;缩放只在重编码路径有效,流拷贝无法改分辨率)
    if !overlay.is_empty()
        || scale.is_some()
        || multi_source
        || has_transforms
        || has_segment_mutes
        || has_speed_changes
        || !text_overlays.is_empty()
    {
        return export_with_audio_mix(
            program,
            &final_path,
            &segs,
            &overlay,
            render_scale,
            crf,
            mute_original,
            text_overlays,
            runtime,
        );
    }

    // 逐段剪切:-ss 放输入前快速定位,时长用 -t(避免 -ss/-to 混用的时间轴歧义);-c copy 不重编码
    let mut parts: Vec<std::path::PathBuf> = Vec::new();
    for (i, (start, end, source_path, _, _, _, _)) in segs.iter().enumerate() {
        let part = dir.join(format!("{stem}.part{}.mp4", i + 1));
        let mut cmd = std::process::Command::new(program);
        crate::media::hide_console_window(&mut cmd);
        cmd.arg("-y")
            .args(["-ss", &format!("{start:.3}")])
            .args(["-t", &format!("{:.3}", end - start)])
            .arg("-i")
            .arg(source_path)
            .args(["-c", "copy"]);
        // 关闭原声:流拷贝下用 -an 剥离音频,不重编码仍是快路径
        if mute_original {
            cmd.arg("-an");
        }
        if runtime.is_some() {
            cmd.args(["-progress", "pipe:1", "-nostats"]);
        }
        cmd.arg(&part);
        let span = if segs.len() == 1 {
            93.0
        } else {
            78.0 / segs.len() as f64
        };
        let out = match run_export_ffmpeg(
            &mut cmd,
            runtime,
            end - start,
            5.0 + span * i as f64,
            span,
            &format!("正在剪切片段 {}/{}", i + 1, segs.len()),
        ) {
            Ok(out) => out,
            Err(error) => {
                let _ = std::fs::remove_file(&part);
                for path in &parts {
                    let _ = std::fs::remove_file(path);
                }
                return Err(error);
            }
        };
        let part_ok = out.status.success()
            && std::fs::metadata(&part)
                .map(|m| m.len() >= 1024)
                .unwrap_or(false);
        if !part_ok {
            let _ = std::fs::remove_file(&part);
            let detail = String::from_utf8_lossy(&out.stderr);
            let last = detail
                .lines()
                .rev()
                .find(|l| !l.trim().is_empty())
                .unwrap_or("未知错误")
                .trim()
                .to_string();
            return Err(CrawlerError::Config(format!(
                "片段 {} 剪切失败: {last}",
                i + 1
            )));
        }
        parts.push(part);
    }

    // 单段:直接换名为成品
    if parts.len() == 1 {
        std::fs::rename(&parts[0], &final_path)
            .map_err(|e| CrawlerError::Config(format!("导出换名失败: {e}")))?;
        return Ok(final_path.to_string_lossy().to_string());
    }

    // 多段:concat 协议 -c copy 拼接(参数相同,流拷贝即可),成功则清理分段
    let list_path = dir.join(format!("{stem}.concat.txt"));
    let list = parts
        .iter()
        .map(|p| format!("file '{}'", p.to_string_lossy().replace('\'', "'\\''")))
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(&list_path, &list)
        .map_err(|e| CrawlerError::Config(format!("写拼接清单失败: {e}")))?;
    let merged = dir.join(format!("{stem}.merged.mp4"));
    let mut cmd = std::process::Command::new(program);
    crate::media::hide_console_window(&mut cmd);
    cmd.arg("-y")
        .args(["-f", "concat", "-safe", "0"])
        .arg("-i")
        .arg(&list_path)
        .args(["-c", "copy"]);
    if runtime.is_some() {
        cmd.args(["-progress", "pipe:1", "-nostats"]);
    }
    cmd.arg(&merged);
    let total_duration = segs
        .iter()
        .map(|(start, end, _, _, _, _, _)| end - start)
        .sum();
    let out = match run_export_ffmpeg(
        &mut cmd,
        runtime,
        total_duration,
        85.0,
        13.0,
        "正在合并片段",
    ) {
        Ok(out) => out,
        Err(error) => {
            let _ = std::fs::remove_file(&list_path);
            let _ = std::fs::remove_file(&merged);
            for path in &parts {
                let _ = std::fs::remove_file(path);
            }
            return Err(error);
        }
    };
    let _ = std::fs::remove_file(&list_path);
    let ok = out.status.success()
        && std::fs::metadata(&merged)
            .map(|m| m.len() >= 4096)
            .unwrap_or(false);
    if !ok {
        let _ = std::fs::remove_file(&merged);
        for p in &parts {
            let _ = std::fs::remove_file(p);
        }
        return Err(CrawlerError::Config("片段拼接失败".into()));
    }
    for p in &parts {
        let _ = std::fs::remove_file(p);
    }
    std::fs::rename(&merged, &final_path)
        .map_err(|e| CrawlerError::Config(format!("导出换名失败: {e}")))?;
    Ok(final_path.to_string_lossy().to_string())
}

fn transform_is_active(transform: Option<&VideoTransformInput>) -> bool {
    transform.is_some_and(|t| {
        t.rotation.unwrap_or(0).rem_euclid(360) != 0
            || (t.scale.unwrap_or(1.0) - 1.0).abs() > 0.001
            || t.position_x.unwrap_or(0.0).abs() > 0.001
            || t.position_y.unwrap_or(0.0).abs() > 0.001
            || t.crop_top.unwrap_or(0.0) > 0.001
            || t.crop_right.unwrap_or(0.0) > 0.001
            || t.crop_bottom.unwrap_or(0.0) > 0.001
            || t.crop_left.unwrap_or(0.0) > 0.001
            || (t.opacity.unwrap_or(1.0) - 1.0).abs() > 0.001
            || t.brightness.unwrap_or(0.0).abs() > 0.001
            || (t.contrast.unwrap_or(1.0) - 1.0).abs() > 0.001
            || (t.saturation.unwrap_or(1.0) - 1.0).abs() > 0.001
            || t.temperature.unwrap_or(0.0).abs() > 0.001
            || t.hue.unwrap_or(0.0).abs() > 0.001
            || t.filter.as_deref().is_some_and(|value| value != "none")
    })
}

/// 预设与手动调节合并为 FFmpeg 色彩滤镜，预览端使用同一组语义参数。
fn color_filter_suffix(transform: &VideoTransformInput) -> String {
    let preset = transform.filter.as_deref().unwrap_or("none");
    let (preset_brightness, preset_contrast, preset_saturation, preset_temperature, mono) =
        match preset {
            "vivid" => (0.02, 1.08, 1.35, 0.02, false),
            "cinema" => (-0.02, 1.14, 0.88, -0.04, false),
            "warm" => (0.01, 1.03, 1.08, 0.12, false),
            "cool" => (0.0, 1.04, 1.03, -0.12, false),
            "mono" => (0.0, 1.08, 0.0, 0.0, true),
            _ => (0.0, 1.0, 1.0, 0.0, false),
        };
    let brightness = (preset_brightness + transform.brightness.unwrap_or(0.0)).clamp(-1.0, 1.0);
    let contrast = (preset_contrast * transform.contrast.unwrap_or(1.0)).clamp(0.0, 3.0);
    let saturation = if mono {
        0.0
    } else {
        (preset_saturation * transform.saturation.unwrap_or(1.0)).clamp(0.0, 3.0)
    };
    let temperature = (preset_temperature + transform.temperature.unwrap_or(0.0)).clamp(-1.0, 1.0);
    let hue = transform.hue.unwrap_or(0.0).clamp(-180.0, 180.0);
    let mut suffix = String::new();
    if brightness.abs() > 0.001
        || (contrast - 1.0).abs() > 0.001
        || (saturation - 1.0).abs() > 0.001
    {
        suffix.push_str(&format!(
            ",eq=brightness={brightness:.4}:contrast={contrast:.4}:saturation={saturation:.4}"
        ));
    }
    if hue.abs() > 0.001 {
        suffix.push_str(&format!(",hue=h={hue:.3}"));
    }
    if temperature.abs() > 0.001 {
        suffix.push_str(&format!(
            ",colorbalance=rs={:.4}:bs={:.4}",
            temperature * 0.45,
            temperature * -0.45
        ));
    }
    suffix
}

/// 为支持逐帧求值的 FFmpeg 参数生成分段线性表达式；t 使用成片绝对时间。
fn keyframe_value_expr(
    keyframes: &[VideoKeyframeInput],
    base: f64,
    position: f64,
    duration: f64,
    value: fn(&VideoKeyframeInput) -> f64,
) -> String {
    let mut points: Vec<(f64, f64, &str)> = keyframes
        .iter()
        .map(|frame| {
            (
                frame.offset.clamp(0.0, duration),
                value(frame),
                frame.easing.as_deref().unwrap_or("linear"),
            )
        })
        .collect();
    points.sort_by(|a, b| a.0.total_cmp(&b.0));
    points.dedup_by(|a, b| (a.0 - b.0).abs() < 0.001);
    if points.first().is_none_or(|point| point.0 > 0.001) {
        points.insert(0, (0.0, base, "linear"));
    }
    if points.is_empty() {
        return format!("{base:.6}");
    }
    let mut expression = format!("{:.6}", points.last().expect("关键帧非空").1);
    for window in points.windows(2).rev() {
        let (start, from, _) = window[0];
        let (end, to, easing) = window[1];
        let start_time = position + start;
        let end_time = position + end;
        let delta = (end - start).max(0.001);
        let progress = format!("(t-{start_time:.6})/{delta:.6}");
        let eased = match easing {
            "easeIn" => format!("({progress})*({progress})"),
            "easeOut" => format!("1-(1-({progress}))*(1-({progress}))"),
            "easeInOut" => format!("({progress})*({progress})*(3-2*({progress}))"),
            _ => progress,
        };
        expression = format!(
            "if(lt(t\\,{end_time:.6})\\,{from:.6}+({to:.6}-{from:.6})*({eased})\\,{expression})"
        );
    }
    expression
}

/// 先按百分比裁掉源画面,再旋转并等比适配输出画布;缩放大于 1 时用 crop 平移取景,
/// 小于 1 时用 pad 留黑边。这样每个片段最终尺寸恒定,可继续 concat/xfade。
fn video_transform_suffix(transform: Option<&VideoTransformInput>, canvas: (u32, u32)) -> String {
    let t = transform.cloned().unwrap_or_default();
    let mut top = t.crop_top.unwrap_or(0.0).clamp(0.0, 89.0) / 100.0;
    let mut right = t.crop_right.unwrap_or(0.0).clamp(0.0, 89.0) / 100.0;
    let mut bottom = t.crop_bottom.unwrap_or(0.0).clamp(0.0, 89.0) / 100.0;
    let mut left = t.crop_left.unwrap_or(0.0).clamp(0.0, 89.0) / 100.0;
    if top + bottom > 0.9 {
        let ratio = 0.9 / (top + bottom);
        top *= ratio;
        bottom *= ratio;
    }
    if left + right > 0.9 {
        let ratio = 0.9 / (left + right);
        left *= ratio;
        right *= ratio;
    }
    let mut suffix = String::new();
    if top + right + bottom + left > 0.0001 {
        suffix.push_str(&format!(
            ",crop=iw*{:.6}:ih*{:.6}:iw*{left:.6}:ih*{top:.6}",
            1.0 - left - right,
            1.0 - top - bottom,
        ));
    }
    suffix.push_str(&color_filter_suffix(&t));
    match t.rotation.unwrap_or(0).rem_euclid(360) {
        90 => suffix.push_str(",transpose=clock"),
        180 => suffix.push_str(",hflip,vflip"),
        270 => suffix.push_str(",transpose=cclock"),
        _ => {}
    }
    let (w, h) = canvas;
    let zoom = t.scale.unwrap_or(1.0).clamp(0.25, 3.0);
    let px = t.position_x.unwrap_or(0.0).clamp(-100.0, 100.0);
    let py = t.position_y.unwrap_or(0.0).clamp(-100.0, 100.0);
    suffix.push_str(&format!(
        ",scale=w='max(2\\,trunc(iw*min({w}/iw\\,{h}/ih)*{zoom:.6}/2)*2)':h='max(2\\,trunc(ih*min({w}/iw\\,{h}/ih)*{zoom:.6}/2)*2)'"
    ));
    suffix.push_str(&format!(
        ",crop=w='min(iw\\,{w})':h='min(ih\\,{h})':x='max(0\\,(iw-{w})/2-{px:.6}*{w}/200)':y='max(0\\,(ih-{h})/2-{py:.6}*{h}/200)'"
    ));
    suffix.push_str(&format!(
        ",pad={w}:{h}:x='max(0\\,({w}-iw)/2+{px:.6}*{w}/200)':y='max(0\\,({h}-ih)/2+{py:.6}*{h}/200)':color=black,setsar=1"
    ));
    suffix
}

/// 多视频轨合成:黑色画布为底,各片段按 position 延时并按 layer 依次 overlay。
/// 与顺序 concat 分开实现,避免画中画片段的透明区域被旧版黑色 pad 覆盖底层画面。
fn export_with_video_layers(
    program: &str,
    final_path: &std::path::Path,
    video_segs: &[TimelineVideoSegment],
    audio_segs: &[TimelineAudioSegment],
    canvas: &(u32, u32),
    crf: u32,
    mute_original: bool,
    text_overlays: &[TextOverlay],
    runtime: Option<&ExportRuntime>,
) -> Result<String> {
    let (width, height) = *canvas;
    let duration = video_segs
        .iter()
        .map(|seg| seg.position + (seg.end - seg.start) / seg.speed)
        .chain(
            audio_segs
                .iter()
                .map(|seg| seg.position.unwrap_or(0.0) + (seg.end - seg.start) / seg.speed),
        )
        .fold(0.0_f64, f64::max)
        .max(0.1);
    let mut ordered: Vec<(usize, &TimelineVideoSegment)> = video_segs.iter().enumerate().collect();
    ordered.sort_by(|(ia, a), (ib, b)| {
        a.layer
            .cmp(&b.layer)
            .then_with(|| a.position.total_cmp(&b.position))
            .then_with(|| ia.cmp(ib))
    });

    let mut graph =
        format!("color=c=black:s={width}x{height}:r=30:d={duration:.3},format=rgba[canvas0];");
    let mut audio_labels = Vec::new();
    for (order, (input_index, seg)) in ordered.iter().enumerate() {
        let transform = seg.transform.clone().unwrap_or_default();
        let mut top = transform.crop_top.unwrap_or(0.0).clamp(0.0, 89.0) / 100.0;
        let mut right = transform.crop_right.unwrap_or(0.0).clamp(0.0, 89.0) / 100.0;
        let mut bottom = transform.crop_bottom.unwrap_or(0.0).clamp(0.0, 89.0) / 100.0;
        let mut left = transform.crop_left.unwrap_or(0.0).clamp(0.0, 89.0) / 100.0;
        if top + bottom > 0.9 {
            let ratio = 0.9 / (top + bottom);
            top *= ratio;
            bottom *= ratio;
        }
        if left + right > 0.9 {
            let ratio = 0.9 / (left + right);
            left *= ratio;
            right *= ratio;
        }
        let mut visual = format!(
            "[{input_index}:v]trim=start={:.3}:end={:.3},setpts=(PTS-STARTPTS)/{:.6}+{:.3}/TB",
            seg.start, seg.end, seg.speed, seg.position
        );
        if top + right + bottom + left > 0.0001 {
            visual.push_str(&format!(
                ",crop=iw*{:.6}:ih*{:.6}:iw*{left:.6}:ih*{top:.6}",
                1.0 - left - right,
                1.0 - top - bottom,
            ));
        }
        match transform.rotation.unwrap_or(0).rem_euclid(360) {
            90 => visual.push_str(",transpose=clock"),
            180 => visual.push_str(",hflip,vflip"),
            270 => visual.push_str(",transpose=cclock"),
            _ => {}
        }
        visual.push_str(&color_filter_suffix(&transform));
        let clip_duration = (seg.end - seg.start) / seg.speed;
        let zoom = keyframe_value_expr(
            &seg.keyframes,
            transform.scale.unwrap_or(1.0).clamp(0.1, 3.0),
            seg.position,
            clip_duration,
            |frame| frame.scale.clamp(0.1, 3.0),
        );
        let opacity = transform.opacity.unwrap_or(1.0).clamp(0.0, 1.0);
        visual.push_str(&format!(
            ",scale=w='max(2\\,trunc(iw*min({width}/iw\\,{height}/ih)*({zoom})/2)*2)':h='max(2\\,trunc(ih*min({width}/iw\\,{height}/ih)*({zoom})/2)*2)':eval=frame,format=rgba,colorchannelmixer=aa={opacity:.6}[layer{order}];"
        ));
        graph.push_str(&visual);
        let x = keyframe_value_expr(
            &seg.keyframes,
            transform.position_x.unwrap_or(0.0).clamp(-100.0, 100.0),
            seg.position,
            clip_duration,
            |frame| frame.position_x.clamp(-100.0, 100.0),
        );
        let y = keyframe_value_expr(
            &seg.keyframes,
            transform.position_y.unwrap_or(0.0).clamp(-100.0, 100.0),
            seg.position,
            clip_duration,
            |frame| frame.position_y.clamp(-100.0, 100.0),
        );
        graph.push_str(&format!(
            "[canvas{order}][layer{order}]overlay=x='(W-w)/2+({x})*W/200':y='(H-h)/2+({y})*H/200':eof_action=pass:repeatlast=0:shortest=0:enable='between(t,{:.3},{:.3})'[canvas{}];",
            seg.position,
            seg.position + clip_duration,
            order + 1,
        ));

        if !mute_original
            && !seg.muted
            && probe_video_info(program, &seg.path)
                .map(|info| !info.audio_codec.is_empty())
                .unwrap_or(false)
        {
            let delay = seg.position * 1000.0;
            graph.push_str(&format!(
                "[{input_index}:a]atrim=start={:.3}:end={:.3},asetpts=PTS-STARTPTS{},adelay={delay:.0}:all=1[va{order}];",
                seg.start, seg.end, atempo_suffix(seg.speed),
            ));
            audio_labels.push(format!("[va{order}]"));
        }
    }

    let video_input_count = video_segs.len();
    for (i, segment) in audio_segs.iter().enumerate() {
        let input_index = video_input_count + i;
        let delay = segment.position.unwrap_or(0.0).max(0.0) * 1000.0;
        let filters = audio_segment_filters(segment, delay);
        graph.push_str(&format!("[{input_index}:a]{filters}[xa{i}];"));
        audio_labels.push(format!("[xa{i}]"));
    }
    if !audio_labels.is_empty() {
        if audio_labels.len() == 1 {
            graph.push_str(&format!("{}anull[aout];", audio_labels[0]));
        } else {
            graph.push_str(&format!(
                "{}amix=inputs={}:duration=longest:dropout_transition=0:normalize=0[aout];",
                audio_labels.concat(),
                audio_labels.len(),
            ));
        }
    }
    let base_label = format!("canvas{}", ordered.len());
    let text_files = append_text_filters(&mut graph, &base_label, text_overlays, final_path)?;
    if graph.ends_with(';') {
        graph.pop();
    }

    let mut cmd = std::process::Command::new(program);
    crate::media::hide_console_window(&mut cmd);
    cmd.arg("-y");
    for seg in video_segs {
        cmd.arg("-i").arg(&seg.path);
    }
    for segment in audio_segs {
        cmd.arg("-i").arg(&segment.path);
    }
    cmd.args(["-filter_complex", &graph])
        .args(["-map", "[vout]"])
        .args([
            "-c:v",
            "libx264",
            "-preset",
            "veryfast",
            "-crf",
            &crf.to_string(),
            "-pix_fmt",
            "yuv420p",
        ]);
    if audio_labels.is_empty() {
        cmd.arg("-an");
    } else {
        cmd.args(["-map", "[aout]", "-c:a", "aac", "-b:a", "160k"]);
    }
    cmd.args(["-t", &format!("{duration:.3}"), "-movflags", "+faststart"]);
    if runtime.is_some() {
        cmd.args(["-progress", "pipe:1", "-nostats"]);
    }
    cmd.arg(final_path);
    let out = match run_export_ffmpeg(&mut cmd, runtime, duration, 5.0, 93.0, "正在合成画面与音频")
    {
        Ok(out) => out,
        Err(error) => {
            let _ = std::fs::remove_file(final_path);
            for path in &text_files {
                let _ = std::fs::remove_file(path);
            }
            return Err(error);
        }
    };
    for path in text_files {
        let _ = std::fs::remove_file(path);
    }
    let ok = out.status.success()
        && std::fs::metadata(final_path)
            .map(|meta| meta.len() >= 4096)
            .unwrap_or(false);
    if !ok {
        let _ = std::fs::remove_file(final_path);
        let detail = String::from_utf8_lossy(&out.stderr);
        let last = detail
            .lines()
            .rev()
            .find(|line| !line.trim().is_empty())
            .unwrap_or("未知错误")
            .trim();
        return Err(CrawlerError::Config(format!("画中画导出失败: {last}")));
    }
    Ok(final_path.to_string_lossy().to_string())
}

/// 重编码导出:视频轨片段 concat(可选 scale 缩放/逐片段画面变换)+ 音频轨叠加段与视频轨原声 amix 混音。
/// mute_original = 关闭原声:不再混入 [abase],仅剩叠加段;叠加段也没有时出无声视频。
/// libx264 veryfast + aac,单遍完成;crf 由画质档位决定。
fn export_with_audio_mix(
    program: &str,
    final_path: &std::path::Path,
    video_segs: &[(
        f64,
        f64,
        String,
        Option<VideoTransformInput>,
        bool,
        f64,
        Option<TransitionInput>,
    )],
    audio_segs: &[TimelineAudioSegment],
    scale: Option<&(u32, u32)>,
    crf: u32,
    mute_original: bool,
    text_overlays: &[TextOverlay],
    runtime: Option<&ExportRuntime>,
) -> Result<String> {
    let base_on = !mute_original && video_segs.iter().any(|seg| !seg.4);
    // 多来源视频统一画布和像素格式,避免尺寸或宽高比不同导致 concat 失败。
    let has_transforms = video_segs
        .iter()
        .any(|seg| transform_is_active(seg.3.as_ref()));
    let scale_suffix = scale
        .map(|(w, h)| {
            format!(
                ",scale={w}:{h}:force_original_aspect_ratio=decrease,pad={w}:{h}:(ow-iw)/2:(oh-ih)/2,setsar=1"
            )
        })
        .unwrap_or_else(|| ",setsar=1".to_string());
    let mut graph = String::new();
    for (i, (a, b, path, transform, clip_muted, speed, _)) in video_segs.iter().enumerate() {
        let visual_suffix = if has_transforms {
            video_transform_suffix(
                transform.as_ref(),
                *scale.expect("画面变换已在入口校验画布尺寸"),
            )
        } else {
            scale_suffix.clone()
        };
        graph.push_str(&format!(
            "[{i}:v]trim=start={a:.3}:end={b:.3},setpts=(PTS-STARTPTS)/{speed:.6}{visual_suffix},format=yuv420p[v{i}];"
        ));
        if base_on {
            let has_audio = !clip_muted
                && probe_video_info(program, path)
                    .map(|info| !info.audio_codec.is_empty())
                    .unwrap_or(false);
            if has_audio {
                graph.push_str(&format!(
                    "[{i}:a]atrim=start={a:.3}:end={b:.3},asetpts=PTS-STARTPTS{}[ab{i}];",
                    atempo_suffix(*speed),
                ));
            } else {
                graph.push_str(&format!(
                    "anullsrc=r=48000:cl=stereo,atrim=duration={:.3},asetpts=PTS-STARTPTS[ab{i}];",
                    (b - a) / speed
                ));
            }
        }
    }
    let vlabels: String = (0..video_segs.len()).map(|i| format!("[v{i}]")).collect();
    let nv = video_segs.len();
    graph.push_str(&format!("{vlabels}concat=n={nv}:v=1:a=0[vbase];"));
    if audio_segs.is_empty() && base_on {
        let ablabels: String = (0..nv).map(|i| format!("[ab{i}]")).collect();
        graph.push_str(&format!("{ablabels}concat=n={nv}:v=0:a=1[aout]"));
    } else if !audio_segs.is_empty() {
        // 音频轨叠加段:atrim + adelay(按序列 position 定位)+ amix 真多轨混音——
        // 重叠片段(如多条音轨同段各配各的)正确叠混,而非串接;normalize=0 保持原响度。
        // position 缺省(旧前端)时按顺序回退为串接定位(与前版行为一致)。
        let mut cursor = 0f64;
        let mut mix_labels = String::new();
        if base_on {
            let ablabels: String = (0..nv).map(|i| format!("[ab{i}]")).collect();
            graph.push_str(&format!("{ablabels}concat=n={nv}:v=0:a=1[abase];"));
            mix_labels.push_str("[abase]");
        }
        for (i, segment) in audio_segs.iter().enumerate() {
            let delay_ms = segment.position.unwrap_or(cursor).max(0.0) * 1000.0;
            cursor += (segment.end - segment.start) / segment.speed;
            let input_index = nv + i;
            let filters = audio_segment_filters(segment, delay_ms);
            graph.push_str(&format!("[{input_index}:a]{filters}[ao{i}];"));
            mix_labels.push_str(&format!("[ao{i}]"));
        }
        let n = audio_segs.len() + usize::from(base_on);
        if n == 1 {
            // 仅一路音轨:无需混音,直通
            graph.push_str(&format!("{mix_labels}anull[aout]"));
        } else {
            let duration_mode = if base_on { "first" } else { "longest" };
            graph.push_str(&format!(
                "{mix_labels}amix=inputs={n}:duration={duration_mode}:dropout_transition=0:normalize=0[aout]"
            ));
        }
    }

    if !graph.ends_with(';') {
        graph.push(';');
    }
    let text_files = append_text_filters(&mut graph, "vbase", text_overlays, final_path)?;
    // 分支拼接可能留下末尾分号,ffmpeg 滤镜图不允许空链
    if graph.ends_with(';') {
        graph.pop();
    }

    let mut cmd = std::process::Command::new(program);
    crate::media::hide_console_window(&mut cmd);
    cmd.arg("-y");
    for (_, _, path, _, _, _, _) in video_segs {
        cmd.arg("-i").arg(path);
    }
    for segment in audio_segs {
        cmd.arg("-i").arg(&segment.path);
    }
    cmd.args(["-filter_complex", &graph])
        .args(["-map", "[vout]"])
        .args([
            "-c:v",
            "libx264",
            "-preset",
            "veryfast",
            "-crf",
            &crf.to_string(),
        ]);
    if base_on || !audio_segs.is_empty() {
        cmd.args(["-map", "[aout]"])
            .args(["-c:a", "aac", "-b:a", "160k"]);
    } else {
        cmd.arg("-an");
    }
    cmd.args(["-movflags", "+faststart"]);
    if runtime.is_some() {
        cmd.args(["-progress", "pipe:1", "-nostats"]);
    }
    cmd.arg(final_path);
    let video_duration: f64 = video_segs
        .iter()
        .map(|(start, end, _, _, _, speed, _)| (end - start) / speed)
        .sum();
    let audio_duration = audio_segs
        .iter()
        .map(|segment| {
            segment.position.unwrap_or(0.0) + (segment.end - segment.start) / segment.speed
        })
        .fold(0.0, f64::max);
    let render_duration = video_duration.max(audio_duration);
    let out = match run_export_ffmpeg(
        &mut cmd,
        runtime,
        render_duration,
        5.0,
        93.0,
        "正在渲染画面与音频",
    ) {
        Ok(out) => out,
        Err(error) => {
            let _ = std::fs::remove_file(final_path);
            for path in &text_files {
                let _ = std::fs::remove_file(path);
            }
            return Err(error);
        }
    };
    for path in text_files {
        let _ = std::fs::remove_file(path);
    }
    let ok = out.status.success()
        && std::fs::metadata(final_path)
            .map(|m| m.len() >= 4096)
            .unwrap_or(false);
    if !ok {
        let _ = std::fs::remove_file(final_path);
        let detail = String::from_utf8_lossy(&out.stderr);
        let last = detail
            .lines()
            .rev()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("未知错误")
            .trim()
            .to_string();
        return Err(CrawlerError::Config(format!(
            "混音导出失败(源视频需含音频流): {last}"
        )));
    }
    Ok(final_path.to_string_lossy().to_string())
}

/// 把文字层串到视频输出后方。文字正文走 UTF-8 textfile,避免用户输入中的冒号、引号、
/// 百分号等被 FFmpeg 滤镜语法再次解释；Windows 优先使用微软雅黑保证中文可见。
fn append_text_filters(
    graph: &mut String,
    base_label: &str,
    overlays: &[TextOverlay],
    final_path: &std::path::Path,
) -> Result<Vec<std::path::PathBuf>> {
    if overlays.is_empty() {
        graph.push_str(&format!("[{base_label}]null[vout];"));
        return Ok(Vec::new());
    }
    let parent = final_path
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."));
    let stem = final_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("clip");
    let font = if std::path::Path::new(r"C:\Windows\Fonts\msyh.ttc").is_file() {
        Some(r"C:\Windows\Fonts\msyh.ttc")
    } else if std::path::Path::new(r"C:\Windows\Fonts\arial.ttf").is_file() {
        Some(r"C:\Windows\Fonts\arial.ttf")
    } else {
        None
    };
    let escape_path = |path: &str| {
        path.replace('\\', "/")
            .replace(':', "\\:")
            .replace('\'', "\\'")
    };
    let mut files = Vec::new();
    let mut previous = base_label.to_string();
    for (index, overlay) in overlays.iter().enumerate() {
        let text = overlay.text.trim();
        if text.is_empty() || overlay.end - overlay.start <= 0.05 {
            continue;
        }
        let path = parent.join(format!(".{stem}.text{}.txt", index + 1));
        std::fs::write(&path, text)
            .map_err(|e| CrawlerError::Config(format!("写入字幕临时文件失败: {e}")))?;
        let out = format!("vtext{index}");
        let size = overlay.font_size.unwrap_or(42).clamp(12, 160);
        let color = overlay
            .color
            .as_deref()
            .filter(|value| {
                value.len() == 7
                    && value.starts_with('#')
                    && value[1..].chars().all(|c| c.is_ascii_hexdigit())
            })
            .unwrap_or("#ffffff")
            .trim_start_matches('#');
        let x_percent = overlay.x.unwrap_or(50.0).clamp(0.0, 100.0);
        let y_percent = overlay
            .y
            .unwrap_or_else(|| match overlay.position.as_deref() {
                Some("top") => 10.0,
                Some("center") => 50.0,
                _ => 90.0,
            })
            .clamp(0.0, 100.0);
        let rotation = overlay.rotation.unwrap_or(0.0).rem_euclid(360.0);
        let font_arg = font
            .map(|value| format!(":fontfile='{}'", escape_path(value)))
            .unwrap_or_default();
        let escaped_text_path = escape_path(&path.to_string_lossy());
        if rotation.abs() <= 0.01 {
            graph.push_str(&format!(
                "[{previous}]drawtext=textfile='{escaped_text_path}'{font_arg}:expansion=none:fontsize={size}:fontcolor=0x{color}:borderw=2:bordercolor=black@0.7:x=w*{x_percent:.4}/100-text_w/2:y=h*{y_percent:.4}/100-text_h/2:enable='between(t,{:.3},{:.3})'[{out}];",
                overlay.start.max(0.0),
                overlay.end.max(0.0),
            ));
        } else {
            // drawtext 本身不支持旋转：从当前帧复制一张全透明画布，把文字放在画布中心
            // 后旋转整张透明层，再平移到百分比坐标叠回原画面。
            graph.push_str(&format!(
                "[{previous}]split=2[{out}base][{out}canvas];[{out}canvas]format=rgba,colorchannelmixer=aa=0,drawtext=textfile='{escaped_text_path}'{font_arg}:expansion=none:fontsize={size}:fontcolor=0x{color}:borderw=2:bordercolor=black@0.7:x=(w-text_w)/2:y=(h-text_h)/2[{out}text];[{out}text]rotate={rotation:.4}*PI/180:c=none:ow=iw:oh=ih[{out}rot];[{out}base][{out}rot]overlay=x='main_w*{x_percent:.4}/100-overlay_w/2':y='main_h*{y_percent:.4}/100-overlay_h/2':enable='between(t,{:.3},{:.3})'[{out}];",
                overlay.start.max(0.0),
                overlay.end.max(0.0),
            ));
        }
        previous = out;
        files.push(path);
    }
    if previous == base_label {
        graph.push_str(&format!("[{base_label}]null[vout];"));
    } else {
        graph.push_str(&format!("[{previous}]null[vout];"));
    }
    Ok(files)
}

// ===================== 剪辑历史 =====================

/// 剪辑历史条目(扫描导出目录,不落库:文件即记录;camelCase 对齐 TS ExportItem)。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportItem {
    /// 文件名(clip-<时间戳>.mp4)。
    pub name: String,
    /// 绝对路径(载入 / 打开文件夹用)。
    pub path: String,
    /// 文件大小(字节)。
    pub size: u64,
    /// 导出时间(Unix 秒,取文件 mtime)。
    pub created_at: i64,
    /// 成片时长与画面尺寸；旧文件探测失败时为 0,前端据此隐藏。
    pub duration_secs: f64,
    pub width: u32,
    pub height: u32,
}

/// 剪辑历史:扫描 <app_data>/exports 下的成片,按时间倒序,最多 100 条。
/// 拼接 / 剪切的中间产物(异常残留时)不列入。
#[tauri::command]
pub async fn creation_list_exports(
    state: State<'_, AppState>,
    app: tauri::AppHandle,
) -> Result<Vec<ExportItem>> {
    use tauri::Manager;
    let program = ffmpeg_program(&state)?;
    let dir = app
        .path()
        .app_data_dir()
        .map_err(|e| CrawlerError::Config(format!("定位数据目录失败: {e}")))?
        .join("exports");
    tauri::async_runtime::spawn_blocking(move || {
        let mut out: Vec<ExportItem> = Vec::new();
        let Ok(rd) = std::fs::read_dir(&dir) else {
            return Ok(out);
        };
        for entry in rd.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            // 只要成片:clip-<时间戳>.mp4;中间产物(.partN / .merged)跳过
            if !name.starts_with("clip-")
                || !name.ends_with(".mp4")
                || name.contains(".part")
                || name.contains(".merged")
            {
                continue;
            }
            let Ok(meta) = entry.metadata() else { continue };
            let created_at = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            let path = entry.path();
            let info = probe_video_info(&program, &path.to_string_lossy()).ok();
            out.push(ExportItem {
                name,
                path: path.to_string_lossy().to_string(),
                size: meta.len(),
                created_at,
                duration_secs: info
                    .as_ref()
                    .map(|value| value.duration_secs)
                    .unwrap_or(0.0),
                width: info.as_ref().map(|value| value.width).unwrap_or(0),
                height: info.as_ref().map(|value| value.height).unwrap_or(0),
            });
        }
        out.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        out.truncate(100);
        Ok(out)
    })
    .await
    .map_err(|e| CrawlerError::Config(format!("读取剪辑历史异常: {e}")))?
}

/// 删除一条已完成的导出记录。只接受 exports 根目录下标准成片文件名，禁止路径穿越。
#[tauri::command]
pub async fn creation_delete_export(app: tauri::AppHandle, name: String) -> Result<()> {
    use tauri::Manager;
    let valid_name = std::path::Path::new(&name)
        .file_name()
        .is_some_and(|value| value == std::ffi::OsStr::new(&name))
        && name.starts_with("clip-")
        && name.ends_with(".mp4")
        && !name.contains(".part")
        && !name.contains(".merged");
    if !valid_name {
        return Err(CrawlerError::Config("导出文件名无效".into()));
    }
    let path = app
        .path()
        .app_data_dir()
        .map_err(|e| CrawlerError::Config(format!("定位数据目录失败: {e}")))?
        .join("exports")
        .join(name);
    tauri::async_runtime::spawn_blocking(move || {
        if !path.exists() {
            return Ok(());
        }
        std::fs::remove_file(&path)
            .map_err(|e| CrawlerError::Config(format!("删除导出文件失败: {e}")))
    })
    .await
    .map_err(|e| CrawlerError::Config(format!("删除导出记录异常: {e}")))?
}

// ===================== 剪辑媒体信息 / 时间轴缩略图 =====================

/// 视频元信息。桌面端只捆绑 ffmpeg.exe(无 ffprobe),统一跑 `ffmpeg -i` 解析 stderr。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VideoInfo {
    pub duration_secs: f64,
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    pub video_codec: String,
    pub audio_codec: String,
    pub bitrate_kbps: u32,
}

/// 解析配置里的 ffmpeg 程序路径(空配置回退 PATH 里的 ffmpeg)。
fn ffmpeg_program(state: &State<'_, AppState>) -> Result<String> {
    let cfg = super::lock_config(state).map_err(|e| CrawlerError::Config(e.to_string()))?;
    Ok(cfg
        .media
        .ffmpeg_path
        .as_deref()
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .unwrap_or("ffmpeg")
        .to_string())
}

/// 剪辑页视频元信息:帧率 / 码率 / 编码等 <video> 元素拿不到的字段,供属性面板展示。
#[tauri::command]
pub async fn creation_video_info(
    state: State<'_, AppState>,
    input_path: String,
) -> Result<VideoInfo> {
    let program = ffmpeg_program(&state)?;
    tauri::async_runtime::spawn_blocking(move || probe_video_info(&program, &input_path))
        .await
        .map_err(|e| CrawlerError::Config(format!("读取视频信息异常: {e}")))?
}

#[derive(Serialize, Deserialize)]
struct WaveformCache {
    stamp: String,
    peaks: Vec<f32>,
}

/// 流式生成时间轴音频峰值。FFmpeg 的 PCM 输出边读边聚合，前端无需把整个视频
/// fetch 到内存；5GB 级素材的内存占用也只与峰值桶数量相关。
#[tauri::command]
pub async fn creation_audio_peaks(
    state: State<'_, AppState>,
    input_path: String,
    buckets: u32,
) -> Result<Vec<f32>> {
    let program = ffmpeg_program(&state)?;
    let root = {
        let cfg = super::lock_config(&state).map_err(|e| CrawlerError::Config(e.to_string()))?;
        crate::media::media_root(&state.config_dir, &cfg.media)
    };
    tauri::async_runtime::spawn_blocking(move || {
        extract_audio_peaks(
            &program,
            &root,
            &input_path,
            buckets.clamp(1024, 65_536) as usize,
        )
    })
    .await
    .map_err(|e| CrawlerError::Config(format!("生成音频波形异常: {e}")))?
}

fn extract_audio_peaks(
    program: &str,
    root: &std::path::Path,
    input_path: &str,
    buckets: usize,
) -> Result<Vec<f32>> {
    use std::hash::{Hash, Hasher};

    let input = std::path::Path::new(input_path);
    if !input.is_file() {
        return Err(CrawlerError::Config(format!("源视频不存在: {input_path}")));
    }
    let source_meta = std::fs::metadata(input)
        .map_err(|e| CrawlerError::Config(format!("读取波形源文件失败: {e}")))?;
    let mtime = source_meta
        .modified()
        .ok()
        .and_then(|value| value.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|value| value.as_secs())
        .unwrap_or(0);
    let stamp = format!("v1:{}:{mtime}:{buckets}", source_meta.len());
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    input_path.hash(&mut hasher);
    let dir = root.join("editor").join("waveforms");
    let cache_path = dir.join(format!("{:016x}-{buckets}.json", hasher.finish()));
    if let Ok(raw) = std::fs::read(&cache_path) {
        if let Ok(cached) = serde_json::from_slice::<WaveformCache>(&raw) {
            if cached.stamp == stamp && cached.peaks.len() == buckets {
                return Ok(cached.peaks);
            }
        }
    }

    let info = probe_video_info(program, input_path)?;
    let sample_rate = 8_000_f64;
    let total_samples = (info.duration_secs * sample_rate).ceil().max(1.0) as u64;
    let mut cmd = std::process::Command::new(program);
    crate::media::hide_console_window(&mut cmd);
    cmd.args(["-hide_banner", "-loglevel", "error", "-nostdin", "-i"])
        .arg(input)
        .args([
            "-map",
            "0:a:0",
            "-vn",
            "-ac",
            "1",
            "-ar",
            "8000",
            "-acodec",
            "pcm_f32le",
            "-f",
            "f32le",
            "pipe:1",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = cmd
        .spawn()
        .map_err(|e| CrawlerError::Config(format!("启动音频波形解码失败: {e}")))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| CrawlerError::Config("读取音频波形输出失败".into()))?;
    let mut reader = BufReader::with_capacity(64 * 1024, stdout);
    let mut peaks = vec![0_f32; buckets];
    let mut sample = [0_u8; 4];
    let mut sample_index = 0_u64;
    loop {
        match reader.read_exact(&mut sample) {
            Ok(()) => {
                let value = f32::from_le_bytes(sample).abs();
                if value.is_finite() {
                    let bucket = ((sample_index.saturating_mul(buckets as u64)) / total_samples)
                        .min(buckets.saturating_sub(1) as u64)
                        as usize;
                    peaks[bucket] = peaks[bucket].max(value);
                }
                sample_index = sample_index.saturating_add(1);
            }
            Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(error) => {
                let _ = child.kill();
                return Err(CrawlerError::Config(format!("读取音频波形失败: {error}")));
            }
        }
    }
    let status = child
        .wait()
        .map_err(|e| CrawlerError::Config(format!("等待音频波形解码失败: {e}")))?;
    if !status.success() || sample_index == 0 {
        return Err(CrawlerError::Config("视频没有可解码的音频轨道".into()));
    }

    // 98 分位归一化避免单次爆音压扁整段人声的显示高度。
    let mut sorted: Vec<f32> = peaks.iter().copied().filter(|value| *value > 0.0).collect();
    sorted.sort_by(|a, b| a.total_cmp(b));
    if let Some(ceiling) = sorted.get((sorted.len() as f64 * 0.98).floor() as usize) {
        if *ceiling > 0.0 {
            for peak in &mut peaks {
                *peak = (*peak / *ceiling).min(1.0);
            }
        }
    }

    std::fs::create_dir_all(&dir)
        .map_err(|e| CrawlerError::Config(format!("创建波形缓存目录失败: {e}")))?;
    let cache = WaveformCache {
        stamp,
        peaks: peaks.clone(),
    };
    if let Ok(raw) = serde_json::to_vec(&cache) {
        let temp = cache_path.with_extension("building.json");
        if std::fs::write(&temp, raw).is_ok() {
            let _ = std::fs::remove_file(&cache_path);
            let _ = std::fs::rename(temp, cache_path);
        }
    }
    Ok(peaks)
}

/// 为剪辑预览生成短 GOP 代理文件。时间轴与导出始终保留原始路径,代理只负责播放器解码,
/// 避免手机录屏等长 GOP 素材在拖动播放头后需要从很远的关键帧开始解码。
#[tauri::command]
pub async fn creation_video_proxy(
    state: State<'_, AppState>,
    input_path: String,
) -> Result<String> {
    let program = ffmpeg_program(&state)?;
    let root = {
        let cfg = super::lock_config(&state).map_err(|e| CrawlerError::Config(e.to_string()))?;
        crate::media::media_root(&state.config_dir, &cfg.media)
    };
    tauri::async_runtime::spawn_blocking(move || create_video_proxy(&program, &root, &input_path))
        .await
        .map_err(|e| CrawlerError::Config(format!("生成剪辑代理异常: {e}")))?
}

fn create_video_proxy(program: &str, root: &std::path::Path, input_path: &str) -> Result<String> {
    use std::hash::{Hash, Hasher};

    let input = std::path::Path::new(input_path);
    if !input.is_file() {
        return Err(CrawlerError::Config(format!("源视频不存在: {input_path}")));
    }
    let source_meta = std::fs::metadata(input)
        .map_err(|e| CrawlerError::Config(format!("读取源视频信息失败: {e}")))?;
    let mtime = source_meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let stamp = format!("{}:{mtime}", source_meta.len());
    let mut h = std::collections::hash_map::DefaultHasher::new();
    input_path.hash(&mut h);
    let dir = root.join("editor").join("proxies");
    let stem = format!("{:016x}", h.finish());
    let output = dir.join(format!("{stem}.mp4"));
    let meta = dir.join(format!("{stem}.meta"));
    if output.is_file() && std::fs::read_to_string(&meta).is_ok_and(|saved| saved.trim() == stamp) {
        return Ok(crate::media::to_media_rel(root, &output));
    }

    std::fs::create_dir_all(&dir)
        .map_err(|e| CrawlerError::Config(format!("创建代理目录失败: {e}")))?;
    let temp = dir.join(format!("{stem}.building.mp4"));
    let _ = std::fs::remove_file(&temp);
    let mut cmd = std::process::Command::new(program);
    crate::media::hide_console_window(&mut cmd);
    cmd.arg("-y")
        .arg("-i")
        .arg(input)
        .args(["-map", "0:v:0", "-map", "0:a:0?"])
        // 720p/30fps 足够剪辑预览;每 12 帧一个关键帧,任意落点最多只需前解码约 0.4 秒。
        .args([
            "-vf",
            "scale='min(1280,iw)':'min(720,ih)':force_original_aspect_ratio=decrease:force_divisible_by=2,fps=30",
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-crf",
            "27",
            "-g",
            "12",
            "-keyint_min",
            "12",
            "-sc_threshold",
            "0",
            "-pix_fmt",
            "yuv420p",
            "-c:a",
            "aac",
            "-b:a",
            "96k",
            "-movflags",
            "+faststart",
        ])
        .arg(&temp);
    let out = crate::media::run_ffmpeg_local(&mut cmd)
        .map_err(|e| CrawlerError::Config(format!("代理转码执行失败: {e}")))?;
    if !out.status.success() {
        let _ = std::fs::remove_file(&temp);
        let detail = String::from_utf8_lossy(&out.stderr);
        let last = detail
            .lines()
            .rev()
            .find(|line| !line.trim().is_empty())
            .unwrap_or("未知错误")
            .trim();
        return Err(CrawlerError::Config(format!("生成剪辑代理失败: {last}")));
    }
    // 同目录临时文件完成后再替换,播放器永远不会读到半个 mp4。
    let _ = std::fs::remove_file(&output);
    std::fs::rename(&temp, &output)
        .map_err(|e| CrawlerError::Config(format!("写入剪辑代理失败: {e}")))?;
    let _ = std::fs::write(&meta, stamp);
    Ok(crate::media::to_media_rel(root, &output))
}

/// 执行探测：按需调用打包内置 FFmpeg。相比在主进程直连 libav DLL，单次多几十毫秒，
/// 但电脑冷启动无需预加载/安全扫描两百多 MB 动态库，整体打开速度明显更稳定。
fn probe_video_info(program: &str, input_path: &str) -> Result<VideoInfo> {
    let input = std::path::Path::new(input_path);
    if !input.is_file() {
        return Err(CrawlerError::Config(format!("源视频不存在: {input_path}")));
    }
    let mut cmd = std::process::Command::new(program);
    crate::media::hide_console_window(&mut cmd);
    cmd.args(["-hide_banner", "-nostdin", "-i"]).arg(input);
    let out = crate::media::run_ffmpeg_local(&mut cmd)
        .map_err(|e| CrawlerError::Config(format!("读取视频信息失败: {e}")))?;
    let stderr = String::from_utf8_lossy(&out.stderr);
    parse_video_info(&stderr)
        .ok_or_else(|| CrawlerError::Parse(format!("解析视频信息失败: {input_path}")))
}

/// 解析 `ffmpeg -i` stderr:Duration 行取时长 / 码率,Video 流取编码 / 分辨率 / 帧率,Audio 流取编码。
fn parse_video_info(stderr: &str) -> Option<VideoInfo> {
    let mut info = VideoInfo {
        duration_secs: 0.0,
        width: 0,
        height: 0,
        fps: 0.0,
        video_codec: String::new(),
        audio_codec: String::new(),
        bitrate_kbps: 0,
    };
    let mut saw_video = false;
    for line in stderr.lines() {
        let l = line.trim();
        if let Some(rest) = l.strip_prefix("Duration:") {
            let mut parts = rest.split(',');
            if let Some(dur) = parts.next() {
                info.duration_secs = parse_ffmpeg_duration(dur.trim());
            }
            for p in parts {
                if let Some(b) = p.trim().strip_prefix("bitrate:") {
                    info.bitrate_kbps = b.replace("kb/s", "").trim().parse().unwrap_or(0);
                }
            }
            continue;
        }
        if !l.starts_with("Stream") {
            continue;
        }
        if let Some((_, v)) = l.split_once(" Video: ") {
            saw_video = true;
            info.video_codec = v.split([',', ' ']).next().unwrap_or("").to_string();
            for tok in v.split([',', ' ']) {
                if info.width == 0 {
                    if let Some((w, h)) = parse_resolution(tok) {
                        info.width = w;
                        info.height = h;
                    }
                }
            }
            if let Some(idx) = v.find(" fps") {
                let before = v[..idx].split([',', ' ']).next_back().unwrap_or("");
                info.fps = before.parse().unwrap_or(0.0);
            }
        } else if let Some((_, a)) = l.split_once(" Audio: ") {
            if info.audio_codec.is_empty() {
                info.audio_codec = a.split([',', ' ']).next().unwrap_or("").to_string();
            }
        }
    }
    (saw_video && info.duration_secs > 0.0).then_some(info)
}

/// hh:mm:ss.cc → 秒
fn parse_ffmpeg_duration(s: &str) -> f64 {
    let mut it = s.split(':');
    let (h, m, sec) = match (it.next(), it.next(), it.next()) {
        (Some(h), Some(m), Some(s)) => (h, m, s),
        _ => return 0.0,
    };
    h.parse::<f64>().unwrap_or(0.0) * 3600.0
        + m.parse::<f64>().unwrap_or(0.0) * 60.0
        + sec.parse::<f64>().unwrap_or(0.0)
}

/// 形如 2560x1600 的分辨率 token(过滤 SAR/DAR 里的非分辨率值:宽高都 ≥16 才算)
fn parse_resolution(tok: &str) -> Option<(u32, u32)> {
    let (w, h) = tok.split_once('x')?;
    let w: u32 = w.parse().ok()?;
    let h: u32 = h.parse().ok()?;
    (w >= 16 && h >= 16).then_some((w, h))
}

/// 胶片条只需提供时间定位参照。限制到 20 帧并使用输入侧快速 seek，避免为了缩略图
/// 从头顺序解码整段大视频；时间轴放大时由前端按最近帧重复铺设。
const THUMB_INTERVAL_SECS: f64 = 5.0;
const THUMB_MAX: usize = 20;

/// 时间轴胶片条缩略图:落 `{media_root}/editor/thumbs/{源路径hash}/t%03d.jpg`,
/// 返回 media_root 相对路径数组(前端走 8788 文件服务渲染)。
/// 缓存:meta.txt 记录源文件 mtime,一致直接复用;源变了清空重抽。
#[tauri::command]
pub async fn creation_video_thumbs(
    state: State<'_, AppState>,
    input_path: String,
) -> Result<Vec<String>> {
    let program = ffmpeg_program(&state)?;
    let root = {
        let cfg = super::lock_config(&state).map_err(|e| CrawlerError::Config(e.to_string()))?;
        crate::media::media_root(&state.config_dir, &cfg.media)
    };
    tauri::async_runtime::spawn_blocking(move || extract_video_thumbs(&program, &root, &input_path))
        .await
        .map_err(|e| CrawlerError::Config(format!("生成缩略图异常: {e}")))?
}

fn extract_video_thumbs(
    program: &str,
    root: &std::path::Path,
    input_path: &str,
) -> Result<Vec<String>> {
    use std::hash::{Hash, Hasher};
    let input = std::path::Path::new(input_path);
    if !input.is_file() {
        return Err(CrawlerError::Config(format!("源视频不存在: {input_path}")));
    }
    let mut h = std::collections::hash_map::DefaultHasher::new();
    input_path.hash(&mut h);
    let dir = root
        .join("editor")
        .join("thumbs")
        .join(format!("{:016x}", h.finish()));
    let source_meta = std::fs::metadata(input)
        .map_err(|e| CrawlerError::Config(format!("读取缩略图源文件失败: {e}")))?;
    let mtime = source_meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let stamp = format!("fast-v2:{}:{mtime}", source_meta.len());
    let meta = dir.join("meta.txt");
    if let Ok(saved) = std::fs::read_to_string(&meta) {
        if saved.trim() == stamp {
            let cached = list_thumb_files(&dir);
            if !cached.is_empty() {
                return Ok(cached
                    .into_iter()
                    .map(|f| crate::media::to_media_rel(root, &f))
                    .collect());
            }
        }
    }

    let info = probe_video_info(program, input_path)?;
    let frame_count =
        ((info.duration_secs / THUMB_INTERVAL_SECS).ceil() as usize).clamp(1, THUMB_MAX);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir)
        .map_err(|e| CrawlerError::Config(format!("创建缩略图目录失败: {e}")))?;
    for index in 0..frame_count {
        let time = if frame_count <= 1 {
            0.0
        } else {
            (info.duration_secs - 0.05).max(0.0) * index as f64 / (frame_count - 1) as f64
        };
        let mut cmd = std::process::Command::new(program);
        crate::media::hide_console_window(&mut cmd);
        cmd.arg("-y")
            // -ss 放在 -i 前走关键帧快速定位；单帧任务不再顺序解码整段视频。
            .args(["-ss", &format!("{time:.3}")])
            .arg("-i")
            .arg(input)
            .args(["-frames:v", "1", "-an", "-sn", "-dn"])
            .args([
                "-vf",
                "scale=160:90:force_original_aspect_ratio=increase,crop=160:90",
                "-q:v",
                "5",
            ])
            .arg(dir.join(format!("t{:03}.jpg", index + 1)));
        // 个别时间点损坏时保留其他帧，只有全部失败才回报错误。
        let _ = crate::media::run_ffmpeg_local(&mut cmd);
    }
    let _ = std::fs::write(&meta, stamp);
    let files = list_thumb_files(&dir);
    if files.is_empty() {
        return Err(CrawlerError::Parse("抽帧未产出任何图片".into()));
    }
    Ok(files
        .into_iter()
        .map(|f| crate::media::to_media_rel(root, &f))
        .collect())
}

fn list_thumb_files(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut v: Vec<std::path::PathBuf> = std::fs::read_dir(dir)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|x| x == "jpg"))
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

/// 转场导出(blocking 线程):单遍 filter_complex —— 每段 trim/setpts 后,
/// 视频链 xfade(kind: dissolve→fade 叠化 / fade→fadeblack 淡黑),原声链 acrossfade,
/// 分离音轨按时间轴位置 adelay 后与原声混音。重编码 libx264 veryfast + aac;
/// 源无音频流且没有音轨时只出视频(-an)。
/// xfade 第 k 次转场 offset = 累计流长 - 转场时长(每次转场重叠 d 秒)。
fn export_with_transitions(
    program: &str,
    final_path: &std::path::Path,
    segs: &[(
        f64,
        f64,
        String,
        Option<VideoTransformInput>,
        bool,
        f64,
        Option<TransitionInput>,
    )],
    audio_segs: &[TimelineAudioSegment],
    global_transition: Option<&TransitionInput>,
    scale: Option<&(u32, u32)>,
    crf: u32,
    mute_original: bool,
    text_overlays: &[TextOverlay],
    runtime: Option<&ExportRuntime>,
) -> Result<String> {
    let transition_plan: Vec<Option<(String, f64)>> = segs
        .iter()
        .enumerate()
        .map(|(index, seg)| {
            if index == 0 {
                None
            } else {
                effective_transition(seg.6.as_ref(), global_transition)
                    .map(|value| (value.kind.clone(), value.duration_secs.clamp(0.1, 2.0)))
            }
        })
        .collect();
    for index in 1..segs.len() {
        let Some((_, duration)) = &transition_plan[index] else {
            continue;
        };
        let previous_duration = (segs[index - 1].1 - segs[index - 1].0) / segs[index - 1].5;
        let current_duration = (segs[index].1 - segs[index].0) / segs[index].5;
        if previous_duration <= *duration || current_duration <= *duration {
            return Err(CrawlerError::Config(format!(
                "第 {} 个连接点的转场时长 {:.2}s 超过相邻片段可用时长",
                index, duration
            )));
        }
    }
    let base_on = !mute_original && segs.iter().any(|seg| !seg.4);
    let has_transforms = segs.iter().any(|seg| transform_is_active(seg.3.as_ref()));
    let scale_suffix = scale
        .map(|(w, h)| {
            format!(
                ",scale={w}:{h}:force_original_aspect_ratio=decrease,pad={w}:{h}:(ow-iw)/2:(oh-ih)/2,setsar=1"
            )
        })
        .unwrap_or_else(|| ",setsar=1".to_string());

    let mut graph = String::new();
    for (i, (a, b, path, transform, clip_muted, speed, _)) in segs.iter().enumerate() {
        let visual_suffix = if has_transforms {
            video_transform_suffix(
                transform.as_ref(),
                *scale.expect("画面变换已在入口校验画布尺寸"),
            )
        } else {
            scale_suffix.clone()
        };
        graph.push_str(&format!(
            "[{i}:v]trim=start={a:.3}:end={b:.3},setpts=(PTS-STARTPTS)/{speed:.6}{visual_suffix},format=yuv420p[v{i}];"
        ));
        if base_on {
            let has_audio = !clip_muted
                && probe_video_info(program, path)
                    .map(|info| !info.audio_codec.is_empty())
                    .unwrap_or(false);
            if has_audio {
                graph.push_str(&format!(
                    "[{i}:a]atrim=start={a:.3}:end={b:.3},asetpts=PTS-STARTPTS{}[a{i}];",
                    atempo_suffix(*speed),
                ));
            } else {
                graph.push_str(&format!(
                    "anullsrc=r=48000:cl=stereo,atrim=duration={:.3},asetpts=PTS-STARTPTS[a{i}];",
                    (b - a) / speed
                ));
            }
        }
    }
    let mut render_duration = (segs[0].1 - segs[0].0) / segs[0].5;
    let mut prev = "v0".to_string();
    for (k, seg) in segs.iter().enumerate().skip(1) {
        let last = k == segs.len() - 1;
        let out_label = if last {
            "vbase".to_string()
        } else {
            format!("x{k}")
        };
        let segment_duration = (seg.1 - seg.0) / seg.5;
        if let Some((kind, duration)) = &transition_plan[k] {
            let xfade = if kind == "fade" { "fadeblack" } else { "fade" };
            let offset = render_duration - duration;
            graph.push_str(&format!(
                "[{prev}][v{k}]xfade=transition={xfade}:duration={duration:.3}:offset={offset:.3}[{out_label}];"
            ));
            render_duration += segment_duration - duration;
        } else {
            graph.push_str(&format!("[{prev}][v{k}]concat=n=2:v=1:a=0[{out_label}];"));
            render_duration += segment_duration;
        }
        prev = out_label;
    }
    if base_on {
        let mut prev_a = "a0".to_string();
        for k in 1..segs.len() {
            let last = k == segs.len() - 1;
            let out_label = if last {
                "abase".to_string()
            } else {
                format!("y{k}")
            };
            if let Some((_, duration)) = &transition_plan[k] {
                graph.push_str(&format!(
                    "[{prev_a}][a{k}]acrossfade=d={duration:.3}[{out_label}];"
                ));
            } else {
                graph.push_str(&format!("[{prev_a}][a{k}]concat=n=2:v=0:a=1[{out_label}];"));
            }
            prev_a = out_label;
        }
    }

    // 分离/叠加音轨使用源时间裁切,再按序列位置延迟；与转场后的原声共同混音。
    let mut mix_labels = String::new();
    if base_on {
        mix_labels.push_str("[abase]");
    }
    let mut source_boundary = 0.0;
    let mut transition_cuts = Vec::new();
    for index in 1..segs.len() {
        source_boundary += (segs[index - 1].1 - segs[index - 1].0) / segs[index - 1].5;
        if let Some((_, duration)) = &transition_plan[index] {
            transition_cuts.push((source_boundary, *duration));
        }
    }
    for (i, segment) in audio_segs.iter().enumerate() {
        let original_position = segment.position.unwrap_or(0.0).max(0.0);
        let adjustment = transition_cuts
            .iter()
            .filter(|(boundary, _)| original_position >= *boundary - 0.001)
            .map(|(_, duration)| duration)
            .sum::<f64>();
        let delay_ms = (original_position - adjustment).max(0.0) * 1000.0;
        let input_index = segs.len() + i;
        let filters = audio_segment_filters(segment, delay_ms);
        graph.push_str(&format!("[{input_index}:a]{filters}[ao{i}];"));
        mix_labels.push_str(&format!("[ao{i}]"));
    }
    let audio_inputs = audio_segs.len() + usize::from(base_on);
    if audio_inputs == 1 {
        graph.push_str(&format!("{mix_labels}anull[aout]"));
    } else if audio_inputs > 1 {
        let duration_mode = if base_on { "first" } else { "longest" };
        graph.push_str(&format!(
            "{mix_labels}amix=inputs={audio_inputs}:duration={duration_mode}:dropout_transition=0:normalize=0[aout]"
        ));
    } else if graph.ends_with(';') {
        graph.pop();
    }
    if !graph.ends_with(';') {
        graph.push(';');
    }
    // 转场会压缩成片时间轴；字幕的起止点也必须使用同一映射，否则首个转场后会逐渐错位。
    let adjusted_text_overlays = text_overlays
        .iter()
        .map(|overlay| {
            let adjust = |time: f64| {
                let removed = transition_cuts
                    .iter()
                    .filter(|(boundary, _)| time >= *boundary - 0.001)
                    .map(|(_, duration)| duration)
                    .sum::<f64>();
                (time - removed).max(0.0)
            };
            let start = adjust(overlay.start);
            TextOverlay {
                start,
                end: adjust(overlay.end).max(start + 0.01),
                text: overlay.text.clone(),
                font_size: overlay.font_size,
                color: overlay.color.clone(),
                position: overlay.position.clone(),
                x: overlay.x,
                y: overlay.y,
                rotation: overlay.rotation,
            }
        })
        .collect::<Vec<_>>();
    let text_files = append_text_filters(&mut graph, "vbase", &adjusted_text_overlays, final_path)?;
    if graph.ends_with(';') {
        graph.pop();
    }

    let mut cmd = std::process::Command::new(program);
    crate::media::hide_console_window(&mut cmd);
    cmd.arg("-y");
    for (_, _, path, _, _, _, _) in segs {
        cmd.arg("-i").arg(path);
    }
    for segment in audio_segs {
        cmd.arg("-i").arg(&segment.path);
    }
    cmd.args(["-filter_complex", &graph])
        .args(["-map", "[vout]"])
        .args([
            "-c:v",
            "libx264",
            "-preset",
            "veryfast",
            "-crf",
            &crf.to_string(),
        ]);
    if audio_inputs > 0 {
        cmd.args(["-map", "[aout]"])
            .args(["-c:a", "aac", "-b:a", "192k"]);
    } else {
        cmd.arg("-an");
    }
    if runtime.is_some() {
        cmd.args(["-progress", "pipe:1", "-nostats"]);
    }
    cmd.arg(final_path);
    let out = match run_export_ffmpeg(
        &mut cmd,
        runtime,
        render_duration,
        5.0,
        93.0,
        "正在渲染转场",
    ) {
        Ok(out) => out,
        Err(error) => {
            let _ = std::fs::remove_file(final_path);
            for path in &text_files {
                let _ = std::fs::remove_file(path);
            }
            return Err(error);
        }
    };
    for path in text_files {
        let _ = std::fs::remove_file(path);
    }
    if !out.status.success() {
        let _ = std::fs::remove_file(final_path);
        let detail = String::from_utf8_lossy(&out.stderr);
        let last = detail
            .lines()
            .rev()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("未知错误")
            .trim()
            .to_string();
        return Err(CrawlerError::Config(format!("转场导出失败: {last}")));
    }
    Ok(final_path.to_string_lossy().to_string())
}

// ===================== 集成测试(真实 ffmpeg 管线) =====================
// 用捆绑的 ffmpeg 现场生成测试视频,端到端跑 探测 / 抽帧 / 导出(流拷贝与转场)。
// ffmpeg 不可用或样片生成失败时跳过(不判失败),保证任何机器上 cargo test 不炸。
#[cfg(test)]
mod tests {
    use super::*;

    fn ffmpeg() -> String {
        let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("resources")
            .join("ffmpeg.exe");
        if p.is_file() {
            p.to_string_lossy().to_string()
        } else {
            "ffmpeg".to_string()
        }
    }

    /// 6 秒样片:testsrc 画面 + 440Hz 正弦音(检验视频流与音频流两条解析路径)
    fn sample_video(dir: &std::path::Path) -> Option<std::path::PathBuf> {
        let out = dir.join("sample.mp4");
        let status = std::process::Command::new(ffmpeg())
            .arg("-y")
            .args([
                "-f",
                "lavfi",
                "-i",
                "testsrc=duration=6:size=320x240:rate=30",
            ])
            .args(["-f", "lavfi", "-i", "sine=frequency=440:duration=6"])
            .args([
                "-c:v",
                "libx264",
                "-pix_fmt",
                "yuv420p",
                "-c:a",
                "aac",
                "-shortest",
            ])
            .arg(&out)
            .output()
            .ok()?;
        (status.status.success() && out.is_file()).then_some(out)
    }

    #[test]
    fn parse_video_info_from_stderr() {
        let stderr = r#"Input #0, mov,mp4,m4a,3gp,3g2,mj2, from 'x.mp4':
  Duration: 00:11:13.40, start: 0.000000, bitrate: 1234 kb/s
  Stream #0:0[0x1](und): Video: h264 (High) (avc1 / 0x31637661), yuv420p(progressive), 2560x1600 [SAR 1:1 DAR 16:10], 30 fps, 30 tbr, 15360 tbn (default)
  Stream #0:1[0x2](und): Audio: aac (LC) (mp4a / 0x6134706D), 48000 Hz, stereo, fltp, 128 kb/s (default)
"#;
        let info = parse_video_info(stderr).expect("应解析出元信息");
        assert!((info.duration_secs - 673.4).abs() < 0.01);
        assert_eq!(info.bitrate_kbps, 1234);
        assert_eq!((info.width, info.height), (2560, 1600));
        assert!((info.fps - 30.0).abs() < 0.01);
        assert_eq!(info.video_codec, "h264");
        assert_eq!(info.audio_codec, "aac");
    }

    #[test]
    fn probe_thumbs_and_exports() {
        let dir = std::env::temp_dir().join(format!("veltrix-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let Some(video) = sample_video(&dir) else {
            eprintln!("skip: ffmpeg 样片生成失败");
            let _ = std::fs::remove_dir_all(&dir);
            return;
        };
        let program = ffmpeg();
        let path = video.to_string_lossy().to_string();
        // 用独立路径模拟第二个视频素材,确保滤镜输入不是误用主视频的重复流。
        let video2 = dir.join("sample-2.mp4");
        std::fs::copy(&video, &video2).expect("第二素材复制应成功");
        let path2 = video2.to_string_lossy().to_string();

        // 元信息探测
        let info = probe_video_info(&program, &path).expect("probe 应成功");
        assert!(
            (info.duration_secs - 6.0).abs() < 0.6,
            "时长: {}",
            info.duration_secs
        );
        assert_eq!((info.width, info.height), (320, 240));
        assert!((29.0..=31.0).contains(&info.fps), "帧率: {}", info.fps);
        assert!(!info.video_codec.is_empty());

        // 胶片条抽帧(2s/帧,6s 样片应得 2~4 张;二次调用走缓存路径)
        let root = dir.join("media");
        let thumbs = extract_video_thumbs(&program, &root, &path).expect("抽帧应成功");
        assert!(
            (2..=THUMB_MAX).contains(&thumbs.len()),
            "张数: {}",
            thumbs.len()
        );
        for rel in &thumbs {
            assert!(root.join(rel).is_file(), "缩略图缺失: {rel}");
        }
        let cached = extract_video_thumbs(&program, &root, &path).expect("缓存复用应成功");
        assert_eq!(thumbs, cached);

        // 导出:流拷贝快路径(单段)
        let out_dir = dir.join("exports");
        let segs = vec![ClipSegment {
            start: 1.0,
            end: 3.0,
            position: None,
            input_path: None,
            transform: None,
            mute_original: false,
            layer: 0,
            keyframes: vec![],
            volume: 1.0,
            pan: 0.0,
            fade_in: 0.0,
            fade_out: 0.0,
            speed: 1.0,
            transition_in: None,
        }];
        let out = export_video_clips(
            &program,
            &out_dir,
            &path,
            &segs,
            &[],
            None,
            None,
            21,
            false,
            &[],
            None,
        )
        .expect("流拷贝导出应成功");
        assert!(std::path::Path::new(&out).is_file());

        // 导出:两个独立视频素材 + 叠化转场(xfade 单遍重编码)
        let segs2 = vec![
            ClipSegment {
                start: 0.0,
                end: 2.5,
                position: None,
                input_path: None,
                transform: Some(VideoTransformInput {
                    rotation: Some(90),
                    scale: Some(1.15),
                    position_x: Some(8.0),
                    position_y: Some(-5.0),
                    crop_top: Some(4.0),
                    crop_right: Some(3.0),
                    crop_bottom: Some(2.0),
                    crop_left: Some(5.0),
                    opacity: None,
                    brightness: Some(0.08),
                    contrast: Some(1.1),
                    saturation: Some(1.2),
                    temperature: Some(0.05),
                    hue: Some(4.0),
                    filter: Some("vivid".into()),
                }),
                mute_original: true,
                layer: 0,
                keyframes: vec![],
                volume: 1.0,
                pan: 0.0,
                fade_in: 0.0,
                fade_out: 0.0,
                speed: 1.0,
                transition_in: None,
            },
            ClipSegment {
                start: 3.0,
                end: 6.0,
                position: None,
                input_path: Some(path2.clone()),
                transform: None,
                mute_original: false,
                layer: 0,
                keyframes: vec![],
                volume: 1.0,
                pan: 0.0,
                fade_in: 0.0,
                fade_out: 0.0,
                speed: 1.0,
                transition_in: Some(TransitionInput {
                    kind: "fade".into(),
                    duration_secs: 0.5,
                }),
            },
        ];
        let tr = TransitionInput {
            kind: "dissolve".into(),
            duration_secs: 0.5,
        };
        let out2 = export_video_clips(
            &program,
            &out_dir,
            &path,
            &segs2,
            &[],
            Some(&tr),
            None,
            21,
            false,
            &[],
            None,
        )
        .expect("转场导出应成功");
        let len2 = std::fs::metadata(&out2).unwrap().len();
        assert!(len2 > 10_000, "转场产物异常小: {len2}");
        // 转场产物时长 ≈ 2.5 + 3.0 - 0.5 = 5.0s
        let info2 = probe_video_info(&program, &out2).expect("转场产物应可读");
        assert!(
            (info2.duration_secs - 5.0).abs() < 0.6,
            "转场产物时长: {}",
            info2.duration_secs
        );
        assert_eq!(
            (info2.width, info2.height),
            (320, 240),
            "画面变换后应保持工程画布尺寸"
        );

        // 三段混合连接点：第二段叠化，第三段显式硬切；验证单个片段可覆盖工程默认值。
        let mixed_transition_segs = vec![
            ClipSegment {
                start: 0.0,
                end: 1.5,
                position: None,
                input_path: None,
                transform: None,
                mute_original: false,
                layer: 0,
                keyframes: vec![],
                volume: 1.0,
                pan: 0.0,
                fade_in: 0.0,
                fade_out: 0.0,
                speed: 1.0,
                transition_in: None,
            },
            ClipSegment {
                start: 1.5,
                end: 3.0,
                position: None,
                input_path: Some(path2.clone()),
                transform: None,
                mute_original: false,
                layer: 0,
                keyframes: vec![],
                volume: 1.0,
                pan: 0.0,
                fade_in: 0.0,
                fade_out: 0.0,
                speed: 1.0,
                transition_in: Some(TransitionInput {
                    kind: "dissolve".into(),
                    duration_secs: 0.3,
                }),
            },
            ClipSegment {
                start: 3.0,
                end: 4.5,
                position: None,
                input_path: None,
                transform: None,
                mute_original: false,
                layer: 0,
                keyframes: vec![],
                volume: 1.0,
                pan: 0.0,
                fade_in: 0.0,
                fade_out: 0.0,
                speed: 1.0,
                transition_in: Some(TransitionInput {
                    kind: "none".into(),
                    duration_secs: 0.0,
                }),
            },
        ];
        let mixed_out = export_video_clips(
            &program,
            &out_dir,
            &path,
            &mixed_transition_segs,
            &[],
            Some(&tr),
            None,
            21,
            false,
            &[],
            None,
        )
        .expect("混合连接点转场应可导出");
        let mixed_info = probe_video_info(&program, &mixed_out).expect("混合转场产物应可读");
        assert!(
            (mixed_info.duration_secs - 4.2).abs() < 0.5,
            "混合转场产物时长: {}",
            mixed_info.duration_secs
        );

        // 画中画:第二轨在同一时间段叠加,带缩放、偏移和透明度。
        let pip_segs = vec![
            ClipSegment {
                start: 0.0,
                end: 3.0,
                position: Some(0.0),
                input_path: None,
                transform: None,
                mute_original: false,
                layer: 0,
                keyframes: vec![],
                volume: 1.0,
                pan: 0.0,
                fade_in: 0.0,
                fade_out: 0.0,
                speed: 1.0,
                transition_in: None,
            },
            ClipSegment {
                start: 0.0,
                end: 2.0,
                position: Some(0.5),
                input_path: Some(path2),
                transform: Some(VideoTransformInput {
                    scale: Some(0.4),
                    position_x: Some(55.0),
                    position_y: Some(-45.0),
                    opacity: Some(0.75),
                    ..Default::default()
                }),
                mute_original: true,
                layer: 1,
                keyframes: vec![
                    VideoKeyframeInput {
                        _id: "start".into(),
                        offset: 0.0,
                        scale: 0.35,
                        position_x: 45.0,
                        position_y: -40.0,
                        easing: Some("easeInOut".into()),
                    },
                    VideoKeyframeInput {
                        _id: "end".into(),
                        offset: 1.8,
                        scale: 0.55,
                        position_x: 60.0,
                        position_y: -50.0,
                        easing: Some("easeOut".into()),
                    },
                ],
                volume: 1.0,
                pan: 0.0,
                fade_in: 0.0,
                fade_out: 0.0,
                speed: 1.5,
                transition_in: None,
            },
        ];
        let pip_out = export_video_clips(
            &program,
            &out_dir,
            &path,
            &pip_segs,
            &[],
            None,
            None,
            21,
            false,
            &[],
            None,
        )
        .expect("画中画导出应成功");
        let pip_info = probe_video_info(&program, &pip_out).expect("画中画产物应可读");
        assert_eq!((pip_info.width, pip_info.height), (320, 240));
        assert!((pip_info.duration_secs - 3.0).abs() < 0.5);

        // 转场与分离音轨可同时渲染,音轨按序列位置叠加而不是退化为顺序串接。
        let audio = vec![ClipSegment {
            start: 0.5,
            end: 1.5,
            position: Some(1.0),
            input_path: Some(path.clone()),
            transform: None,
            mute_original: false,
            layer: 0,
            keyframes: vec![],
            volume: 0.8,
            pan: -0.25,
            fade_in: 0.15,
            fade_out: 0.2,
            speed: 1.25,
            transition_in: None,
        }];
        let text = vec![TextOverlay {
            start: 0.5,
            end: 2.0,
            text: "字幕测试".into(),
            font_size: Some(36),
            color: Some("#ffffff".into()),
            position: Some("bottom".into()),
            x: None,
            y: None,
            rotation: None,
        }];
        let out3 = export_video_clips(
            &program,
            &out_dir,
            &path,
            &segs2,
            &audio,
            Some(&tr),
            None,
            21,
            false,
            &text,
            None,
        )
        .expect("转场、音轨与字幕应可同时导出");
        let info3 = probe_video_info(&program, &out3).expect("混合产物应可读");
        assert!(!info3.audio_codec.is_empty(), "混合产物应包含音频流");

        // 恒定变速必须同时改变画面与原声时长；4 秒素材 2× 后约为 2 秒。
        let speed_segs = vec![ClipSegment {
            start: 0.0,
            end: 4.0,
            position: Some(0.0),
            input_path: None,
            transform: None,
            mute_original: false,
            layer: 0,
            keyframes: vec![],
            volume: 1.0,
            pan: 0.0,
            fade_in: 0.0,
            fade_out: 0.0,
            speed: 2.0,
            transition_in: None,
        }];
        let speed_out = export_video_clips(
            &program,
            &out_dir,
            &path,
            &speed_segs,
            &[],
            None,
            None,
            21,
            false,
            &[],
            None,
        )
        .expect("恒定变速应可导出");
        let speed_info = probe_video_info(&program, &speed_out).expect("变速产物应可读");
        assert!(
            (speed_info.duration_secs - 2.0).abs() < 0.4,
            "变速产物时长: {}",
            speed_info.duration_secs
        );
        assert!(!speed_info.audio_codec.is_empty(), "变速后原声应保持同步");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
