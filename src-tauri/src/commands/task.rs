//! 采集任务 CRUD 命令。任务归属用户(owner)采用当前登录用户。
//!
//! keywords 在数据库以 JSON 字符串存储,前后端按 Vec<String> 序列化;
//! trigger/status/sortMode/timeRange 等枚举以字符串透传,值校验前端约束。

use crate::commands::{current_user, AppState};
use chrono::Utc;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, EntityTrait, IntoActiveModel, QueryFilter, QueryOrder,
    QuerySelect, Set,
};
use serde::{Deserialize, Serialize};
use tauri::State;
use veltrix_core::db::entity::{content, task};
use veltrix_core::error::{CrawlerError, Result};

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskView {
    pub id: String,
    pub name: String,
    pub industry: String,
    pub platform: String,
    pub keywords: Vec<String>,
    /// once-now / daily / watching
    pub trigger: String,
    pub scheduled_at: Option<String>,
    pub watch_interval_min: Option<i32>,
    pub sort_mode: String,
    pub time_range: String,
    pub per_keyword_limit: i32,
    pub min_likes: i32,
    pub ai_extract: bool,
    pub status: String,
    pub progress: i32,
    pub content_count: i64,
    pub comment_count: i64,
    pub started_at: Option<i64>,
    pub finished_at: Option<i64>,
    pub error_message: Option<String>,
    pub owner: String,
    pub created_at: i64,
    pub updated_at: i64,
}

impl From<task::Model> for TaskView {
    fn from(m: task::Model) -> Self {
        // keywords 反序列化失败回退空数组,避免一条脏数据拖死整表
        let keywords: Vec<String> = serde_json::from_str(&m.keywords).unwrap_or_default();
        Self {
            id: m.id,
            name: m.name,
            industry: m.industry,
            platform: m.platform,
            keywords,
            trigger: m.trigger_type,
            scheduled_at: m.scheduled_at,
            watch_interval_min: m.watch_interval_min,
            sort_mode: m.sort_mode,
            time_range: m.time_range,
            per_keyword_limit: m.per_keyword_limit,
            min_likes: m.min_likes,
            ai_extract: m.ai_extract,
            status: m.status,
            progress: m.progress,
            content_count: m.content_count,
            comment_count: m.comment_count,
            started_at: m.started_at,
            finished_at: m.finished_at,
            error_message: m.error_message,
            owner: m.owner,
            created_at: m.created_at,
            updated_at: m.updated_at,
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskInput {
    pub id: String,
    pub name: String,
    pub industry: String,
    pub platform: String,
    pub keywords: Vec<String>,
    pub trigger: String,
    pub scheduled_at: Option<String>,
    pub watch_interval_min: Option<i32>,
    pub sort_mode: String,
    pub time_range: String,
    pub per_keyword_limit: i32,
    pub min_likes: i32,
    pub ai_extract: bool,
}

fn owner_of(state: &AppState) -> Result<String> {
    current_user(state)
        .map(|u| u.name)
        .ok_or_else(|| CrawlerError::Config("未登录".into()))
}

/// 单次最多返回 N 行,防止前端 IPC 被几万行数据噎住。
/// 数据量超出时应改走分页接口(暂留 TODO)。
const LIST_HARD_CAP: u64 = 1000;

#[tauri::command]
pub async fn list_tasks(state: State<'_, AppState>) -> Result<Vec<TaskView>> {
    // 按 dataScope 过滤;self 仅看自己,all 看全部
    let me = current_user(&state).ok_or_else(|| CrawlerError::Config("未登录".into()))?;
    let mut q = task::Entity::find().order_by_desc(task::Column::UpdatedAt);
    if me.scope == "self" {
        q = q.filter(task::Column::Owner.eq(me.name.clone()));
    }
    let rows = q
        .limit(LIST_HARD_CAP)
        .all(&state.db)
        .await
        .map_err(|e| CrawlerError::Config(format!("查询任务失败: {e}")))?;
    Ok(rows.into_iter().map(Into::into).collect())
}

#[tauri::command]
pub async fn upsert_task(state: State<'_, AppState>, input: TaskInput) -> Result<()> {
    let db = &state.db;
    let now = Utc::now().timestamp();
    let owner = owner_of(&state)?;
    let keywords_json = serde_json::to_string(&input.keywords)
        .map_err(|e| CrawlerError::Config(format!("序列化关键词失败: {e}")))?;

    let existing = task::Entity::find_by_id(input.id.clone())
        .one(db)
        .await
        .map_err(|e| CrawlerError::Config(format!("查询任务失败: {e}")))?;
    match existing {
        Some(model) => {
            let mut am = model.into_active_model();
            am.name = Set(input.name);
            am.industry = Set(input.industry);
            am.platform = Set(input.platform);
            am.keywords = Set(keywords_json);
            am.trigger_type = Set(input.trigger);
            am.scheduled_at = Set(input.scheduled_at);
            am.watch_interval_min = Set(input.watch_interval_min);
            am.sort_mode = Set(input.sort_mode);
            am.time_range = Set(input.time_range);
            am.per_keyword_limit = Set(input.per_keyword_limit);
            am.min_likes = Set(input.min_likes);
            am.ai_extract = Set(input.ai_extract);
            am.updated_at = Set(now);
            am.update(db)
                .await
                .map_err(|e| CrawlerError::Config(format!("更新任务失败: {e}")))?;
        }
        None => {
            let am = task::ActiveModel {
                id: Set(input.id),
                name: Set(input.name),
                industry: Set(input.industry),
                platform: Set(input.platform),
                keywords: Set(keywords_json),
                trigger_type: Set(input.trigger),
                scheduled_at: Set(input.scheduled_at),
                watch_interval_min: Set(input.watch_interval_min),
                sort_mode: Set(input.sort_mode),
                time_range: Set(input.time_range),
                per_keyword_limit: Set(input.per_keyword_limit),
                min_likes: Set(input.min_likes),
                ai_extract: Set(input.ai_extract),
                status: Set("pending".into()),
                progress: Set(0),
                content_count: Set(0),
                comment_count: Set(0),
                started_at: Set(None),
                finished_at: Set(None),
                error_message: Set(None),
                owner: Set(owner),
                created_at: Set(now),
                updated_at: Set(now),
            };
            am.insert(db)
                .await
                .map_err(|e| CrawlerError::Config(format!("创建任务失败: {e}")))?;
        }
    }
    Ok(())
}

/// 单独修改任务运行态(启动/暂停/终止/归档),不动其他字段
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskStatusPatch {
    pub id: String,
    pub status: String,
    pub started_at: Option<i64>,
    pub finished_at: Option<i64>,
}

#[tauri::command]
pub async fn update_task_status(
    state: State<'_, AppState>,
    patch: TaskStatusPatch,
) -> Result<()> {
    let db = &state.db;
    let now = Utc::now().timestamp();
    let model = task::Entity::find_by_id(patch.id.clone())
        .one(db)
        .await
        .map_err(|e| CrawlerError::Config(format!("查询任务失败: {e}")))?
        .ok_or_else(|| CrawlerError::Config(format!("任务不存在: {}", patch.id)))?;
    let mut am = model.into_active_model();
    am.status = Set(patch.status);
    if let Some(v) = patch.started_at {
        am.started_at = Set(Some(v));
    }
    if let Some(v) = patch.finished_at {
        am.finished_at = Set(Some(v));
    }
    am.updated_at = Set(now);
    am.update(db)
        .await
        .map_err(|e| CrawlerError::Config(format!("更新任务状态失败: {e}")))?;
    Ok(())
}

#[tauri::command]
pub async fn remove_task(state: State<'_, AppState>, id: String) -> Result<()> {
    task::Entity::delete_by_id(id)
        .exec(&state.db)
        .await
        .map_err(|e| CrawlerError::Config(format!("删除任务失败: {e}")))?;
    Ok(())
}

/// 全量库内容视图。image_urls 在库里是 JSON 字符串,前端按数组消费。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContentView {
    pub id: String,
    pub task_id: String,
    pub platform: String,
    /// 所属行业:content 表无此列,list_contents 关联 task.industry 填入
    pub industry: String,
    pub content_id: String,
    /// 采集时命中的关键词
    pub keyword: String,
    /// video / image / article / unknown
    pub kind: String,
    pub title: Option<String>,
    pub desc: Option<String>,
    pub author_uid: String,
    pub author_nickname: String,
    /// 作者头像 URL(从 author_json 解析)
    pub author_avatar: Option<String>,
    pub like_count: Option<i64>,
    pub comment_count: Option<i64>,
    pub collect_count: Option<i64>,
    pub share_count: Option<i64>,
    pub play_count: Option<i64>,
    pub published_at: Option<i64>,
    pub video_url: Option<String>,
    pub cover_url: Option<String>,
    pub image_urls: Vec<String>,
    /// 视频时长(秒);图文为 None
    pub duration: Option<i64>,
    /// 话题标签(# 开头)
    pub topics: Vec<String>,
    pub owner: String,
    pub collected_at: i64,
    /// 素材下载状态:pending / success / failed;None=旧数据未跑过下载
    pub media_status: Option<String>,
    /// 音频是否提取成功(仅视频且开启提取时有意义)
    pub audio_extracted: Option<bool>,
    /// 素材失败原因(403 / ffmpeg 失败等)
    pub media_error: Option<String>,
}

impl From<content::Model> for ContentView {
    fn from(m: content::Model) -> Self {
        // image_urls / topics 反序列化失败回退空数组,避免一条脏数据拖死整表
        let image_urls: Vec<String> = serde_json::from_str(&m.image_urls).unwrap_or_default();
        let topics: Vec<String> = serde_json::from_str(&m.topics).unwrap_or_default();
        // 头像在完整作者 JSON 里(实体只单列了 uid/nickname),按需解析出来
        let author_avatar = serde_json::from_str::<serde_json::Value>(&m.author_json)
            .ok()
            .and_then(|v| v.get("avatar").and_then(|a| a.as_str()).map(str::to_string));
        Self {
            id: m.id,
            task_id: m.task_id,
            platform: m.platform,
            industry: String::new(), // 由 list_contents 关联 task 后填充
            content_id: m.content_id,
            keyword: m.keyword,
            kind: m.kind,
            title: m.title,
            desc: m.desc,
            author_uid: m.author_uid,
            author_nickname: m.author_nickname,
            author_avatar,
            like_count: m.like_count,
            comment_count: m.comment_count,
            collect_count: m.collect_count,
            share_count: m.share_count,
            play_count: m.play_count,
            published_at: m.published_at,
            video_url: m.video_url,
            cover_url: m.cover_url,
            image_urls,
            duration: m.duration,
            topics,
            owner: m.owner,
            collected_at: m.collected_at,
            media_status: m.media_status,
            audio_extracted: m.audio_extracted,
            media_error: m.media_error,
        }
    }
}

/// 全量库:列出采集落库的全部内容,按采集时间倒序。dataScope=self 仅看自己。
#[tauri::command]
pub async fn list_contents(state: State<'_, AppState>) -> Result<Vec<ContentView>> {
    let me = current_user(&state).ok_or_else(|| CrawlerError::Config("未登录".into()))?;
    let mut q = content::Entity::find().order_by_desc(content::Column::CollectedAt);
    if me.scope == "self" {
        q = q.filter(content::Column::Owner.eq(me.name.clone()));
    }
    let rows = q
        .limit(LIST_HARD_CAP)
        .all(&state.db)
        .await
        .map_err(|e| CrawlerError::Config(format!("查询内容失败: {e}")))?;

    // 关联任务取行业(逻辑外键,无物理 relation):一次性查回相关 task 建 id→industry 表
    let task_ids: std::collections::HashSet<String> =
        rows.iter().map(|r| r.task_id.clone()).collect();
    let industry_map: std::collections::HashMap<String, String> = task::Entity::find()
        .filter(task::Column::Id.is_in(task_ids))
        .all(&state.db)
        .await
        .map_err(|e| CrawlerError::Config(format!("查询任务行业失败: {e}")))?
        .into_iter()
        .map(|t| (t.id, t.industry))
        .collect();

    Ok(rows
        .into_iter()
        .map(|m| {
            let industry = industry_map.get(&m.task_id).cloned().unwrap_or_default();
            let mut view: ContentView = m.into();
            view.industry = industry;
            view
        })
        .collect())
}

/// 删除一条采集内容(全量库 / 内容库的「删除」操作)。仅删库记录,媒体文件不动。
#[tauri::command]
pub async fn remove_content(state: State<'_, AppState>, id: String) -> Result<()> {
    content::Entity::delete_by_id(id)
        .exec(&state.db)
        .await
        .map_err(|e| CrawlerError::Config(format!("删除内容失败: {e}")))?;
    Ok(())
}
