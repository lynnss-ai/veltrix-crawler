//! 内容创作 - 提示词管理的 CRUD 命令(两级:分类目录 → 分镜镜头提示词)。
//!
//! ID 由前端生成(crypto.randomUUID)并随请求传入,后端按 `id` 是否已存在区分新增 / 更新。
//! 数据按 owner 归属:list 命令在 dataScope=="self" 时只返回当前用户自己的;逻辑外键,无物理 FK。

use crate::commands::AppState;
use veltrix_core::db::entity::{prompt_category, shot_prompt};
use veltrix_core::error::{CrawlerError, Result};
use chrono::Utc;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, EntityTrait, IntoActiveModel, QueryFilter, QueryOrder,
    QuerySelect, Set,
};
use serde::{Deserialize, Serialize};
use tauri::State;

/// 单次 list 接口最多返回 N 行,防 IPC 噎住;数据量超出应改分页接口。
const LIST_HARD_CAP: u64 = 1000;

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
pub async fn list_prompt_categories(
    state: State<'_, AppState>,
) -> Result<Vec<PromptCategoryView>> {
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
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClipSegment {
    pub start: f64,
    pub end: f64,
}

/// 转场参数:kind = dissolve(叠化)/ fade(淡黑);duration_secs 转场时长(秒)。
/// 为空 = 不加转场,保持 -c copy 快路径。
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TransitionInput {
    pub kind: String,
    pub duration_secs: f64,
}

/// 剪辑导出:视频轨片段逐段 `-ss -t -c copy` 剪切(不重编码,快;切口对齐关键帧,
/// 可能有百毫秒级偏差),多段用 concat 协议 `-c copy` 拼接;有音频轨叠加段时改走
/// filter_complex 单遍重编码(视频轨 concat + 音频轨 amix 混音,libx264 veryfast)。
/// 产物落 <app_data>/exports/clip-<时间戳>.mp4。字幕轨暂不参与导出(前端占位)。
#[tauri::command]
pub async fn creation_export_video(
    state: State<'_, AppState>,
    app: tauri::AppHandle,
    input_path: String,
    segments: Vec<ClipSegment>,
    audio_segments: Vec<ClipSegment>,
    transition: Option<TransitionInput>,
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
    let dir = app
        .path()
        .app_data_dir()
        .map_err(|e| CrawlerError::Config(format!("定位数据目录失败: {e}")))?
        .join("exports");
    // 剪切 + 拼接是重 I/O 子进程调用,放 blocking 线程
    tauri::async_runtime::spawn_blocking(move || {
        export_video_clips(&program, &dir, &input_path, &segments, &audio_segments, transition.as_ref())
    })
    .await
    .map_err(|e| CrawlerError::Config(format!("剪辑导出异常: {e}")))?
}

/// 导出执行体(blocking 线程):校验 → 逐段剪切 → 拼接 → 清理中间文件。
fn export_video_clips(
    program: &str,
    dir: &std::path::Path,
    input_path: &str,
    segments: &[ClipSegment],
    audio_segments: &[ClipSegment],
    transition: Option<&TransitionInput>,
) -> Result<String> {
    let input = std::path::Path::new(input_path);
    if !input.is_file() {
        return Err(CrawlerError::Config(format!("源视频不存在: {input_path}")));
    }
    // 过滤无效片段(起点为负 / 时长过短),限 64 段防滥用;全部无效直接报错
    let segs: Vec<(f64, f64)> = segments
        .iter()
        .map(|s| (s.start.max(0.0), s.end))
        .filter(|(a, b)| b - a > 0.05)
        .take(64)
        .collect();
    if segs.is_empty() {
        return Err(CrawlerError::Config("没有有效的剪辑片段".into()));
    }
    let overlay: Vec<(f64, f64)> = audio_segments
        .iter()
        .map(|s| (s.start.max(0.0), s.end))
        .filter(|(a, b)| b - a > 0.05)
        .take(64)
        .collect();
    std::fs::create_dir_all(dir)
        .map_err(|e| CrawlerError::Config(format!("创建导出目录失败: {e}")))?;
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
    let stem = format!("clip-{stamp}");
    let final_path = dir.join(format!("{stem}.mp4"));

    // 带转场(多段时):单遍 filter_complex(xfade + acrossfade);与音频轨叠加互斥
    if let Some(tr) = transition {
        if segs.len() >= 2 {
            if !overlay.is_empty() {
                return Err(CrawlerError::Config(
                    "转场暂不能与音频轨叠加同时使用,请先移除音频轨片段".into(),
                ));
            }
            if segs.len() > 24 {
                return Err(CrawlerError::Config(
                    "片段超过 24 段暂不支持加转场,请先精简片段".into(),
                ));
            }
            return export_with_transitions(program, input, &final_path, &segs, tr);
        }
    }

    // 有音频轨叠加段:单遍 filter_complex 重编码(视频轨 concat + 音频轨混音)
    if !overlay.is_empty() {
        return export_with_audio_mix(program, input, &final_path, &segs, &overlay);
    }

    // 逐段剪切:-ss 放输入前快速定位,时长用 -t(避免 -ss/-to 混用的时间轴歧义);-c copy 不重编码
    let mut parts: Vec<std::path::PathBuf> = Vec::new();
    for (i, (start, end)) in segs.iter().enumerate() {
        let part = dir.join(format!("{stem}.part{}.mp4", i + 1));
        let mut cmd = std::process::Command::new(program);
        crate::media::hide_console_window(&mut cmd);
        cmd.arg("-y")
            .args(["-ss", &format!("{start:.3}")])
            .args(["-t", &format!("{:.3}", end - start)])
            .arg("-i")
            .arg(input)
            .args(["-c", "copy"])
            .arg(&part);
        let out = crate::media::run_ffmpeg_local(&mut cmd)
            .map_err(|e| CrawlerError::Config(format!("片段 {} 剪切失败: {e}", i + 1)))?;
        let part_ok = out.status.success()
            && std::fs::metadata(&part).map(|m| m.len() >= 1024).unwrap_or(false);
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
            return Err(CrawlerError::Config(format!("片段 {} 剪切失败: {last}", i + 1)));
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
        .args(["-c", "copy"])
        .arg(&merged);
    let out = crate::media::run_ffmpeg_local(&mut cmd)
        .map_err(|e| CrawlerError::Config(format!("片段拼接异常: {e}")))?;
    let _ = std::fs::remove_file(&list_path);
    let ok = out.status.success()
        && std::fs::metadata(&merged).map(|m| m.len() >= 4096).unwrap_or(false);
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

/// 带音频轨叠加的导出:视频轨片段 concat(重编码)+ 各音频轨片段 concat 后与视频轨
/// 原始音频 amix 混音。重编码用 libx264 veryfast/crf21 + aac,单遍完成。
fn export_with_audio_mix(
    program: &str,
    input: &std::path::Path,
    final_path: &std::path::Path,
    video_segs: &[(f64, f64)],
    audio_segs: &[(f64, f64)],
) -> Result<String> {
    // 视频轨:逐段 trim + concat
    let mut graph = String::new();
    for (i, (a, b)) in video_segs.iter().enumerate() {
        graph.push_str(&format!(
            "[0:v]trim=start={a:.3}:end={b:.3},setpts=PTS-STARTPTS[v{i}];"
        ));
        graph.push_str(&format!(
            "[0:a]atrim=start={a:.3}:end={b:.3},asetpts=PTS-STARTPTS[ab{i}];"
        ));
    }
    let vlabels: String = (0..video_segs.len()).map(|i| format!("[v{i}]")).collect();
    let ablabels: String = (0..video_segs.len()).map(|i| format!("[ab{i}]")).collect();
    let nv = video_segs.len();
    graph.push_str(&format!("{vlabels}concat=n={nv}:v=1:a=0[vout];"));
    graph.push_str(&format!("{ablabels}concat=n={nv}:v=0:a=1[abase];"));
    // 音频轨叠加段:atrim + concat(单段时直接用该段)
    for (i, (a, b)) in audio_segs.iter().enumerate() {
        graph.push_str(&format!(
            "[0:a]atrim=start={a:.3}:end={b:.3},asetpts=PTS-STARTPTS[ao{i}];"
        ));
    }
    if audio_segs.len() == 1 {
        graph.push_str("[ao0]acopy[aover];");
    } else {
        let aolabels: String = (0..audio_segs.len()).map(|i| format!("[ao{i}]")).collect();
        graph.push_str(&format!(
            "{aolabels}concat=n={}:v=0:a=1[aover];",
            audio_segs.len()
        ));
    }
    graph.push_str("[abase][aover]amix=inputs=2:duration=first:dropout_transition=0[aout]");

    let mut cmd = std::process::Command::new(program);
    crate::media::hide_console_window(&mut cmd);
    cmd.arg("-y")
        .arg("-i")
        .arg(input)
        .args(["-filter_complex", &graph])
        .args(["-map", "[vout]", "-map", "[aout]"])
        .args(["-c:v", "libx264", "-preset", "veryfast", "-crf", "21"])
        .args(["-c:a", "aac", "-b:a", "160k"])
        .args(["-movflags", "+faststart"])
        .arg(final_path);
    let out = crate::media::run_ffmpeg_local(&mut cmd)
        .map_err(|e| CrawlerError::Config(format!("混音导出异常: {e}")))?;
    let ok = out.status.success()
        && std::fs::metadata(final_path).map(|m| m.len() >= 4096).unwrap_or(false);
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
}

/// 剪辑历史:扫描 <app_data>/exports 下的成片,按时间倒序,最多 100 条。
/// 拼接 / 剪切的中间产物(异常残留时)不列入。
#[tauri::command]
pub async fn creation_list_exports(app: tauri::AppHandle) -> Result<Vec<ExportItem>> {
    use tauri::Manager;
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
            out.push(ExportItem {
                name,
                path: entry.path().to_string_lossy().to_string(),
                size: meta.len(),
                created_at,
            });
        }
        out.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        out.truncate(100);
        Ok(out)
    })
    .await
    .map_err(|e| CrawlerError::Config(format!("读取剪辑历史异常: {e}")))?
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

/// 执行探测:`ffmpeg -i` 不带输出文件,退出码非 0 属正常,只看 stderr。
fn probe_video_info(program: &str, input_path: &str) -> Result<VideoInfo> {
    let input = std::path::Path::new(input_path);
    if !input.is_file() {
        return Err(CrawlerError::Config(format!("源视频不存在: {input_path}")));
    }
    let mut cmd = std::process::Command::new(program);
    crate::media::hide_console_window(&mut cmd);
    cmd.arg("-i").arg(input);
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

/// 胶片条抽帧间隔下限(秒)与张数上限:短素材至少 2s 一帧,长素材按上限反推间隔。
const THUMB_INTERVAL_SECS: f64 = 2.0;
const THUMB_MAX: usize = 60;

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
    let mtime = std::fs::metadata(input)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let meta = dir.join("meta.txt");
    if let Ok(saved) = std::fs::read_to_string(&meta) {
        if saved.trim() == mtime.to_string() {
            let cached = list_thumb_files(&dir);
            if !cached.is_empty() {
                return Ok(cached
                    .into_iter()
                    .map(|f| crate::media::to_media_rel(root, &f))
                    .collect());
            }
        }
    }

    // 间隔:短素材 2s 一帧,长素材按 THUMB_MAX 张上限反推
    let interval = match probe_video_info(program, input_path) {
        Ok(info) if info.duration_secs > 0.0 => {
            (info.duration_secs / THUMB_MAX as f64).max(THUMB_INTERVAL_SECS)
        }
        _ => THUMB_INTERVAL_SECS,
    };
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir)
        .map_err(|e| CrawlerError::Config(format!("创建缩略图目录失败: {e}")))?;
    let mut cmd = std::process::Command::new(program);
    crate::media::hide_console_window(&mut cmd);
    cmd.arg("-y")
        .arg("-i")
        .arg(input)
        .args([
            "-vf",
            &format!(
                "fps={:.6},scale=160:90:force_original_aspect_ratio=increase,crop=160:90",
                1.0 / interval
            ),
        ])
        .args(["-q:v", "5"])
        .arg(dir.join("t%03d.jpg"));
    let out = crate::media::run_ffmpeg_local(&mut cmd)
        .map_err(|e| CrawlerError::Config(format!("抽帧执行失败: {e}")))?;
    if !out.status.success() {
        let detail = String::from_utf8_lossy(&out.stderr);
        let last = detail
            .lines()
            .rev()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("未知错误")
            .trim()
            .to_string();
        return Err(CrawlerError::Config(format!("抽帧失败: {last}")));
    }
    let _ = std::fs::write(&meta, mtime.to_string());
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
/// 视频链 xfade(kind: dissolve→fade 叠化 / fade→fadeblack 淡黑),音频链 acrossfade。
/// 重编码 libx264 veryfast + aac;源无音频流时只出视频(-an)。
/// xfade 第 k 次转场 offset = 累计流长 - 转场时长(每次转场重叠 d 秒)。
fn export_with_transitions(
    program: &str,
    input: &std::path::Path,
    final_path: &std::path::Path,
    segs: &[(f64, f64)],
    transition: &TransitionInput,
) -> Result<String> {
    let d = transition.duration_secs.clamp(0.1, 2.0);
    let xfade = match transition.kind.as_str() {
        "fade" => "fadeblack",
        _ => "fade",
    };
    if segs.iter().any(|(a, b)| b - a <= d) {
        return Err(CrawlerError::Config(format!(
            "存在短于转场时长({d:.1}s)的片段,无法加转场"
        )));
    }
    let has_audio = probe_video_info(program, &input.to_string_lossy())
        .map(|i| !i.audio_codec.is_empty())
        .unwrap_or(true);

    let mut graph = String::new();
    for (i, (a, b)) in segs.iter().enumerate() {
        graph.push_str(&format!(
            "[0:v]trim=start={a:.3}:end={b:.3},setpts=PTS-STARTPTS[v{i}];"
        ));
        if has_audio {
            graph.push_str(&format!(
                "[0:a]atrim=start={a:.3}:end={b:.3},asetpts=PTS-STARTPTS[a{i}];"
            ));
        }
    }
    let mut offset = segs[0].1 - segs[0].0 - d;
    let mut prev = "v0".to_string();
    for (k, seg) in segs.iter().enumerate().skip(1) {
        let last = k == segs.len() - 1;
        let out_label = if last { "vx".to_string() } else { format!("x{k}") };
        graph.push_str(&format!(
            "[{prev}][v{k}]xfade=transition={xfade}:duration={d:.3}:offset={offset:.3}[{out_label}];"
        ));
        prev = out_label;
        offset += (seg.1 - seg.0) - d;
    }
    if has_audio {
        let mut prev_a = "a0".to_string();
        for k in 1..segs.len() {
            let last = k == segs.len() - 1;
            let out_label = if last { "ax".to_string() } else { format!("y{k}") };
            graph.push_str(&format!("[{prev_a}][a{k}]acrossfade=d={d:.3}[{out_label}];"));
            prev_a = out_label;
        }
    }
    graph.pop(); // 末尾多余分号

    let mut cmd = std::process::Command::new(program);
    crate::media::hide_console_window(&mut cmd);
    cmd.arg("-y")
        .arg("-i")
        .arg(input)
        .args(["-filter_complex", &graph])
        .args(["-map", "[vx]"])
        .args(["-c:v", "libx264", "-preset", "veryfast", "-crf", "20"]);
    if has_audio {
        cmd.args(["-map", "[ax]"])
            .args(["-c:a", "aac", "-b:a", "192k"]);
    } else {
        cmd.arg("-an");
    }
    cmd.arg(final_path);
    let out = crate::media::run_ffmpeg_local(&mut cmd)
        .map_err(|e| CrawlerError::Config(format!("转场导出执行失败: {e}")))?;
    if !out.status.success() {
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
            .args(["-f", "lavfi", "-i", "testsrc=duration=6:size=320x240:rate=30"])
            .args(["-f", "lavfi", "-i", "sine=frequency=440:duration=6"])
            .args(["-c:v", "libx264", "-pix_fmt", "yuv420p", "-c:a", "aac", "-shortest"])
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

        // 元信息探测
        let info = probe_video_info(&program, &path).expect("probe 应成功");
        assert!((info.duration_secs - 6.0).abs() < 0.6, "时长: {}", info.duration_secs);
        assert_eq!((info.width, info.height), (320, 240));
        assert!((29.0..=31.0).contains(&info.fps), "帧率: {}", info.fps);
        assert!(!info.video_codec.is_empty());

        // 胶片条抽帧(2s/帧,6s 样片应得 2~4 张;二次调用走缓存路径)
        let root = dir.join("media");
        let thumbs = extract_video_thumbs(&program, &root, &path).expect("抽帧应成功");
        assert!((2..=THUMB_MAX).contains(&thumbs.len()), "张数: {}", thumbs.len());
        for rel in &thumbs {
            assert!(root.join(rel).is_file(), "缩略图缺失: {rel}");
        }
        let cached = extract_video_thumbs(&program, &root, &path).expect("缓存复用应成功");
        assert_eq!(thumbs, cached);

        // 导出:流拷贝快路径(单段)
        let out_dir = dir.join("exports");
        let segs = vec![ClipSegment { start: 1.0, end: 3.0 }];
        let out = export_video_clips(&program, &out_dir, &path, &segs, &[], None)
            .expect("流拷贝导出应成功");
        assert!(std::path::Path::new(&out).is_file());

        // 导出:两段 + 叠化转场(xfade 单遍重编码)
        let segs2 = vec![
            ClipSegment { start: 0.0, end: 2.5 },
            ClipSegment { start: 3.0, end: 6.0 },
        ];
        let tr = TransitionInput { kind: "dissolve".into(), duration_secs: 0.5 };
        let out2 = export_video_clips(&program, &out_dir, &path, &segs2, &[], Some(&tr))
            .expect("转场导出应成功");
        let len2 = std::fs::metadata(&out2).unwrap().len();
        assert!(len2 > 10_000, "转场产物异常小: {len2}");
        // 转场产物时长 ≈ 2.5 + 3.0 - 0.5 = 5.0s
        let info2 = probe_video_info(&program, &out2).expect("转场产物应可读");
        assert!((info2.duration_secs - 5.0).abs() < 0.6, "转场产物时长: {}", info2.duration_secs);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
