//! 采集任务 CRUD 命令。任务归属用户(owner)采用当前登录用户。
//!
//! keywords 在数据库以 JSON 字符串存储,前后端按 Vec<String> 序列化;
//! trigger/status/sortMode/timeRange 等枚举以字符串透传,值校验前端约束。

use crate::commands::{current_user, AppState};
use chrono::Utc;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, EntityTrait, FromQueryResult, IntoActiveModel, PaginatorTrait,
    QueryFilter, QueryOrder, QuerySelect, Set,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tauri::State;
use veltrix_core::db::entity::{collect_log, comment, content, task, task_run};
use veltrix_core::error::{CrawlerError, Result};

/// 任务下单个关键词的采集统计(内容数 / 实际入库评论数),供任务列表按关键词分行展示。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KeywordStat {
    pub keyword: String,
    pub content_count: i64,
    pub comment_count: i64,
}

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
    /// 评论采集开关
    pub collect_comments: bool,
    /// 评论发布时间范围:3d / 7d / 14d / any
    pub comment_time_range: String,
    /// 单视频一级评论上限,0 表示不限
    pub comment_limit: i32,
    /// 评论意图分析开关(本阶段仅透传)
    pub analyze_comment_intent: bool,
    pub status: String,
    pub progress: i32,
    /// 素材下载总数(downloading_media 阶段统计,0 表示无素材)
    pub media_total: i32,
    /// 素材已处理数(成功 + 失败均计)
    pub media_done: i32,
    /// 评论采集阶段待采视频总数(collecting_comments 阶段统计)
    pub comment_video_total: i32,
    /// 评论采集阶段已采视频数
    pub comment_video_done: i32,
    pub content_count: i64,
    pub comment_count: i64,
    pub started_at: Option<i64>,
    pub finished_at: Option<i64>,
    pub error_message: Option<String>,
    /// 是否已归档
    pub archived: bool,
    /// 采集完成后自动同步内容到发起者 Obsidian vault
    pub auto_sync_obsidian: bool,
    pub owner: String,
    pub created_at: i64,
    pub updated_at: i64,
    /// 各关键词「本次采集」的统计(内容 / 评论数);仅 list_tasks 填充,事件推送时为空数组
    pub keyword_stats: Vec<KeywordStat>,
    /// 累计采集总量(库里该任务去重后的全部内容 / 评论数);仅 list_tasks 填充,事件推送时为 0
    pub total_contents: i64,
    pub total_comments: i64,
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
            collect_comments: m.collect_comments,
            comment_time_range: m.comment_time_range,
            comment_limit: m.comment_limit,
            analyze_comment_intent: m.analyze_comment_intent,
            status: m.status,
            progress: m.progress,
            media_total: m.media_total,
            media_done: m.media_done,
            comment_video_total: m.comment_video_total,
            comment_video_done: m.comment_video_done,
            content_count: m.content_count,
            comment_count: m.comment_count,
            started_at: m.started_at,
            finished_at: m.finished_at,
            error_message: m.error_message,
            archived: m.archived,
            auto_sync_obsidian: m.auto_sync_obsidian,
            owner: m.owner,
            created_at: m.created_at,
            updated_at: m.updated_at,
            keyword_stats: Vec::new(),
            total_contents: 0,
            total_comments: 0,
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
    /// 评论采集开关(前端可能不传,默认关闭)
    #[serde(default)]
    pub collect_comments: bool,
    /// 评论发布时间范围:3d / 7d / 14d / any(空 / 默认视为 any 不过滤)
    #[serde(default)]
    pub comment_time_range: String,
    /// 单视频一级评论上限,0 表示不限
    #[serde(default)]
    pub comment_limit: i32,
    /// 评论意图分析开关(本阶段仅透传入库)
    #[serde(default)]
    pub analyze_comment_intent: bool,
    /// 采集完成后自动同步内容到发起者(owner)的 Obsidian vault
    #[serde(default)]
    pub auto_sync_obsidian: bool,
}

fn owner_of(state: &AppState) -> Result<String> {
    current_user(state)
        .map(|u| u.name)
        .ok_or_else(|| CrawlerError::Config("未登录".into()))
}

/// 单次最多返回 N 行,防止前端 IPC 被几万行数据噎住。
/// 数据量超出时应改走分页接口(暂留 TODO)。
const LIST_HARD_CAP: u64 = 1000;

/// 采集日志单任务返回上限(日志比内容多,放宽);超出只回最近 N 条。
const LOG_HARD_CAP: u64 = 2000;

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

    // 采集明细只统计「最后一次运行(started_at)之后」采到的:重采即从 0 起算(历史数据不删)。
    // 从没运行过(started_at 为 None)的任务不参与统计,采集明细自然为 0。
    let task_started: HashMap<String, i64> = rows
        .iter()
        .filter_map(|m| m.started_at.map(|s| (m.id.clone(), s)))
        .collect();
    let (stats, totals) = keyword_stats_for_tasks(&state.db, &task_started).await;

    Ok(rows
        .into_iter()
        .map(|m| {
            let mut view: TaskView = m.into();
            let by_keyword = stats.get(&view.id);
            // 采集明细:按任务自身关键词顺序生成「本次采集」统计行,缺记录的关键词计 0
            view.keyword_stats = view
                .keywords
                .iter()
                .map(|kw| {
                    let counts = by_keyword.and_then(|m| m.get(kw)).copied();
                    KeywordStat {
                        keyword: kw.clone(),
                        content_count: counts.map(|c| c.0).unwrap_or(0),
                        comment_count: counts.map(|c| c.1).unwrap_or(0),
                    }
                })
                .collect();
            // 采集结果:累计总量(库里该任务去重后的全部内容 / 评论数)
            let (total_c, total_m) = totals.get(&view.id).copied().unwrap_or((0, 0));
            view.total_contents = total_c;
            view.total_comments = total_m;
            view
        })
        .collect())
}

/// 聚合给定任务集的「关键词 → (内容数, 评论数)」。content 表自带 keyword 直接计数;
/// comment 表无 keyword,先取 content 的 (content_id → keyword) 映射,再把每条内容的评论数归入对应关键词。
/// 返回 task_id → (keyword → (content_count, comment_count))。查询失败按空处理,不阻断任务列表。
async fn keyword_stats_for_tasks(
    db: &sea_orm::DatabaseConnection,
    task_started: &HashMap<String, i64>,
) -> (
    HashMap<String, HashMap<String, (i64, i64)>>,
    HashMap<String, (i64, i64)>,
) {
    // result:「本次采集」per-keyword(collected_at >= started_at);totals:累计总量(全部行,去重累计)
    let mut result: HashMap<String, HashMap<String, (i64, i64)>> = HashMap::new();
    let mut totals: HashMap<String, (i64, i64)> = HashMap::new();
    if task_started.is_empty() {
        return (result, totals);
    }
    let task_ids: Vec<String> = task_started.keys().cloned().collect();

    #[derive(FromQueryResult)]
    struct ContentRow {
        task_id: String,
        content_id: String,
        keyword: String,
        collected_at: i64,
    }
    #[derive(FromQueryResult)]
    struct CommentRow {
        task_id: String,
        content_id: String,
        collected_at: i64,
    }

    // content:逐条取 (task_id, content_id, keyword, collected_at)。content→keyword 映射全建(供评论归类),
    // 但仅 collected_at >= 本次 started_at(即最后一次采到)的才计入内容数。
    let content_rows = content::Entity::find()
        .select_only()
        .column(content::Column::TaskId)
        .column(content::Column::ContentId)
        .column(content::Column::Keyword)
        .column(content::Column::CollectedAt)
        .filter(content::Column::TaskId.is_in(task_ids.clone()))
        .into_model::<ContentRow>()
        .all(db)
        .await
        .unwrap_or_default();
    let mut content_keyword: HashMap<(String, String), String> = HashMap::new();
    for r in content_rows {
        let started = task_started.get(&r.task_id).copied().unwrap_or(i64::MAX);
        totals.entry(r.task_id.clone()).or_insert((0, 0)).0 += 1;
        content_keyword.insert((r.task_id.clone(), r.content_id.clone()), r.keyword.clone());
        if r.collected_at >= started {
            result
                .entry(r.task_id)
                .or_default()
                .entry(r.keyword)
                .or_insert((0, 0))
                .0 += 1;
        }
    }

    // comment:逐条取 (task_id, content_id, collected_at);同样只数最后一次采到的,经映射归到对应关键词
    let comment_rows = comment::Entity::find()
        .select_only()
        .column(comment::Column::TaskId)
        .column(comment::Column::ContentId)
        .column(comment::Column::CollectedAt)
        .filter(comment::Column::TaskId.is_in(task_ids.clone()))
        .into_model::<CommentRow>()
        .all(db)
        .await
        .unwrap_or_default();
    for r in comment_rows {
        let started = task_started.get(&r.task_id).copied().unwrap_or(i64::MAX);
        totals.entry(r.task_id.clone()).or_insert((0, 0)).1 += 1;
        if r.collected_at < started {
            continue;
        }
        if let Some(keyword) = content_keyword.get(&(r.task_id.clone(), r.content_id.clone())) {
            result
                .entry(r.task_id.clone())
                .or_default()
                .entry(keyword.clone())
                .or_insert((0, 0))
                .1 += 1;
        }
    }

    (result, totals)
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
            am.collect_comments = Set(input.collect_comments);
            am.comment_time_range = Set(input.comment_time_range);
            am.comment_limit = Set(input.comment_limit);
            am.analyze_comment_intent = Set(input.analyze_comment_intent);
            am.auto_sync_obsidian = Set(input.auto_sync_obsidian);
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
                collect_comments: Set(input.collect_comments),
                comment_time_range: Set(input.comment_time_range),
                comment_limit: Set(input.comment_limit),
                analyze_comment_intent: Set(input.analyze_comment_intent),
                auto_sync_obsidian: Set(input.auto_sync_obsidian),
                archived: Set(false),
                status: Set("pending".into()),
                progress: Set(0),
                media_total: Set(0),
                media_done: Set(0),
                comment_video_total: Set(0),
                comment_video_done: Set(0),
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
    pub archived: Option<bool>,
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
    if let Some(v) = patch.archived {
        am.archived = Set(v);
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
    /// 封面本地绝对路径(下载成功后回写),前端本地优先显示
    pub cover_path: Option<String>,
    /// 作者头像本地绝对路径
    pub avatar_path: Option<String>,
    /// 视频转出音频本地绝对路径(详情页播放用);None=非视频/未提取/旧数据未记录
    pub audio_path: Option<String>,
    /// 视频语音转写文本(转写成功后回写),前端展示
    pub transcript: Option<String>,
    /// 转写失败原因(供前端区分未转写与失败)
    pub transcript_error: Option<String>,
    /// 细粒度处理状态:视频下载 / 图文图片进度 / 评论采集 / 意向分析
    pub video_downloaded: Option<bool>,
    pub image_total: Option<i32>,
    pub image_done: Option<i32>,
    pub comment_collected: Option<bool>,
    pub intent_analyzed: Option<bool>,
    /// 当前登录用户是否已把该内容同步到自己的 Obsidian(list_contents 按当前用户回填)
    pub synced_by_me: bool,
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
            cover_path: m.cover_path,
            avatar_path: m.avatar_path,
            audio_path: m.audio_path,
            transcript: m.transcript,
            transcript_error: m.transcript_error,
            video_downloaded: m.video_downloaded,
            image_total: m.image_total,
            image_done: m.image_done,
            comment_collected: m.comment_collected,
            intent_analyzed: m.intent_analyzed,
            synced_by_me: false, // 由 list_contents 按当前用户回填
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

    // 当前用户已同步到 Obsidian 的内容集合(供前端「已同步」标记)
    let synced: std::collections::HashSet<String> = {
        use veltrix_core::db::entity::content_synced_user as csu;
        let content_ids: std::collections::HashSet<String> =
            rows.iter().map(|r| r.id.clone()).collect();
        csu::Entity::find()
            .filter(csu::Column::SyncedUser.eq(me.name.clone()))
            .filter(csu::Column::ContentId.is_in(content_ids))
            .all(&state.db)
            .await
            .unwrap_or_default()
            .into_iter()
            .map(|r| r.content_id)
            .collect()
    };

    Ok(rows
        .into_iter()
        .map(|m| {
            let industry = industry_map.get(&m.task_id).cloned().unwrap_or_default();
            let is_synced = synced.contains(&m.id);
            let mut view: ContentView = m.into();
            view.industry = industry;
            view.synced_by_me = is_synced;
            view
        })
        .collect())
}

/// 内容详情里的作者扩展信息(从 author_json 解析)+ 该作者在库中的聚合统计。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthorDetail {
    /// 作者 UID(抖音为 sec_uid)
    pub uid: String,
    pub nickname: String,
    pub avatar: Option<String>,
    /// 本地头像绝对路径(下载成功后),前端本地优先显示
    pub avatar_path: Option<String>,
    /// 平台号(抖音号 unique_id 等)
    pub platform_id: Option<String>,
    /// 平台短 ID(extra.uid)
    pub short_id: Option<String>,
    /// 简介 / 个性签名
    pub signature: Option<String>,
    pub follower_count: Option<i64>,
    pub following_count: Option<i64>,
    /// 作者获赞总数(部分平台返回,缺失为 None)
    pub total_favorited: Option<i64>,
    /// IP 属地(部分平台返回,缺失为 None)
    pub location: Option<String>,
    /// 该作者在库中已采视频数(同 owner+platform+author_uid)
    pub video_count: i64,
    /// 该作者在库中已采评论数
    pub comment_count: i64,
    /// 该作者内容的首次采集 / 最近发布 / 最近采集时间(Unix 秒)
    pub first_collected_at: Option<i64>,
    pub last_published_at: Option<i64>,
    pub last_collected_at: Option<i64>,
    /// 是否被持续监控(当前无作者级监控,恒 false,占位供前端展示)
    pub is_monitored: bool,
}

/// 全量库「内容详情」:内容本体 + 作者扩展信息与聚合统计。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContentDetailView {
    pub content: ContentView,
    pub author: AuthorDetail,
}

/// 取单条内容的完整详情(作者扩展信息 + 作者维度聚合)。self scope 仅能看自己 owner 的内容。
#[tauri::command]
pub async fn get_content_detail(
    state: State<'_, AppState>,
    id: String,
) -> Result<ContentDetailView> {
    let me = current_user(&state).ok_or_else(|| CrawlerError::Config("未登录".into()))?;
    let row = content::Entity::find_by_id(id)
        .one(&state.db)
        .await
        .map_err(|e| CrawlerError::Config(format!("查询内容失败: {e}")))?
        .ok_or_else(|| CrawlerError::Config("内容不存在".into()))?;
    if me.scope == "self" && row.owner != me.name {
        return Err(CrawlerError::Config("无权查看该内容".into()));
    }

    // 关联任务取行业
    let industry = task::Entity::find_by_id(row.task_id.clone())
        .one(&state.db)
        .await
        .ok()
        .flatten()
        .map(|t| t.industry)
        .unwrap_or_default();

    // 当前用户是否已同步到 Obsidian
    let synced_by_me = {
        use veltrix_core::db::entity::content_synced_user as csu;
        csu::Entity::find_by_id((row.id.clone(), me.name.clone()))
            .one(&state.db)
            .await
            .ok()
            .flatten()
            .is_some()
    };

    // 解析作者 JSON 的扩展字段(顶层 + extra 子对象)
    let av = serde_json::from_str::<serde_json::Value>(&row.author_json).ok();
    let top_str = |key: &str| {
        av.as_ref()
            .and_then(|v| v.get(key))
            .and_then(|x| x.as_str())
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    let top_i64 = |key: &str| av.as_ref().and_then(|v| v.get(key)).and_then(|x| x.as_i64());
    let extra_str = |key: &str| {
        av.as_ref()
            .and_then(|v| v.get("extra"))
            .and_then(|e| e.get(key))
            .and_then(|x| x.as_str())
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    let extra_i64 = |key: &str| {
        av.as_ref()
            .and_then(|v| v.get("extra"))
            .and_then(|e| e.get(key))
            .and_then(|x| x.as_i64())
    };

    // 同 owner + platform + author_uid 的全部内容,聚合作者维度统计
    let author_contents = content::Entity::find()
        .filter(content::Column::Owner.eq(row.owner.clone()))
        .filter(content::Column::Platform.eq(row.platform.clone()))
        .filter(content::Column::AuthorUid.eq(row.author_uid.clone()))
        .all(&state.db)
        .await
        .unwrap_or_default();
    let video_count = author_contents.iter().filter(|c| c.kind == "video").count() as i64;
    let content_ids: Vec<String> = author_contents.iter().map(|c| c.content_id.clone()).collect();
    let first_collected_at = author_contents.iter().map(|c| c.collected_at).min();
    let last_published_at = author_contents.iter().filter_map(|c| c.published_at).max();
    let last_collected_at = author_contents.iter().map(|c| c.collected_at).max();

    // 该作者内容下已采评论数
    let comment_count = if content_ids.is_empty() {
        0
    } else {
        comment::Entity::find()
            .filter(comment::Column::Owner.eq(row.owner.clone()))
            .filter(comment::Column::Platform.eq(row.platform.clone()))
            .filter(comment::Column::ContentId.is_in(content_ids))
            .count(&state.db)
            .await
            .unwrap_or(0) as i64
    };

    // 优先读作者表(最新画像 + 监控状态);旧数据未入表则回退 author_json 快照
    let author_row = {
        use veltrix_core::db::entity::author as author_entity;
        let aid = format!("{}-{}-{}", row.owner, row.platform, row.author_uid);
        author_entity::Entity::find_by_id(aid)
            .one(&state.db)
            .await
            .ok()
            .flatten()
    };
    let ar = author_row.as_ref();
    let author = AuthorDetail {
        uid: row.author_uid.clone(),
        nickname: ar
            .map(|a| a.nickname.clone())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| row.author_nickname.clone()),
        avatar: ar.and_then(|a| a.avatar.clone()).or_else(|| top_str("avatar")),
        avatar_path: row.avatar_path.clone(),
        platform_id: ar
            .and_then(|a| a.platform_id.clone())
            .or_else(|| extra_str("unique_id")),
        short_id: ar.and_then(|a| a.short_id.clone()).or_else(|| extra_str("uid")),
        signature: ar
            .and_then(|a| a.signature.clone())
            .or_else(|| top_str("signature")),
        follower_count: ar
            .and_then(|a| a.follower_count)
            .or_else(|| top_i64("follower_count")),
        following_count: ar
            .and_then(|a| a.following_count)
            .or_else(|| top_i64("following_count")),
        total_favorited: ar
            .and_then(|a| a.total_favorited)
            .or_else(|| extra_i64("total_favorited")),
        location: ar
            .and_then(|a| a.location.clone())
            .or_else(|| extra_str("ip_location")),
        video_count,
        comment_count,
        first_collected_at,
        last_published_at,
        last_collected_at,
        is_monitored: ar.map(|a| a.is_monitored).unwrap_or(false),
    };

    let mut content_view: ContentView = row.into();
    content_view.industry = industry;
    content_view.synced_by_me = synced_by_me;

    Ok(ContentDetailView {
        content: content_view,
        author,
    })
}

/// 作者库视图(authors 表 + 已采内容数聚合)。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthorView {
    pub id: String,
    pub owner: String,
    pub platform: String,
    pub uid: String,
    pub nickname: String,
    pub avatar: Option<String>,
    /// 平台号(抖音号等)
    pub platform_id: Option<String>,
    pub signature: Option<String>,
    pub follower_count: Option<i64>,
    pub following_count: Option<i64>,
    pub total_favorited: Option<i64>,
    pub location: Option<String>,
    pub is_monitored: bool,
    pub first_collected_at: i64,
    pub last_collected_at: i64,
    /// 该作者在库中的已采内容数(contents 按 owner+platform+uid 聚合)
    pub content_count: i64,
    /// 该作者内容覆盖的行业(经 contents → task.industry 去重聚合;作者可跨多个行业)
    pub industries: Vec<String>,
}

/// 作者库:列出采集到的作者档案,按最近采集倒序。dataScope=self 仅看自己。
#[tauri::command]
pub async fn list_authors(state: State<'_, AppState>) -> Result<Vec<AuthorView>> {
    use veltrix_core::db::entity::author as author_entity;
    let me = current_user(&state).ok_or_else(|| CrawlerError::Config("未登录".into()))?;
    let mut q = author_entity::Entity::find()
        .order_by_desc(author_entity::Column::LastCollectedAt);
    if me.scope == "self" {
        q = q.filter(author_entity::Column::Owner.eq(me.name.clone()));
    }
    let rows = q
        .limit(LIST_HARD_CAP)
        .all(&state.db)
        .await
        .map_err(|e| CrawlerError::Config(format!("查询作者失败: {e}")))?;

    // 已采内容数:contents 按 (owner, platform, author_uid) 一次分组计数,避免逐作者查库(N+1)
    let mut cq = content::Entity::find();
    if me.scope == "self" {
        cq = cq.filter(content::Column::Owner.eq(me.name.clone()));
    }
    let counts: Vec<(String, String, String, i64)> = cq
        .select_only()
        .column(content::Column::Owner)
        .column(content::Column::Platform)
        .column(content::Column::AuthorUid)
        .column_as(content::Column::Id.count(), "count")
        .group_by(content::Column::Owner)
        .group_by(content::Column::Platform)
        .group_by(content::Column::AuthorUid)
        .into_tuple()
        .all(&state.db)
        .await
        .map_err(|e| CrawlerError::Config(format!("统计作者内容数失败: {e}")))?;
    // 聚合键与 authors.id 的构成规则一致:{owner}-{platform}-{uid}
    let count_map: std::collections::HashMap<String, i64> = counts
        .into_iter()
        .map(|(owner, platform, uid, count)| (format!("{owner}-{platform}-{uid}"), count))
        .collect();

    // 行业聚合:作者 → 其内容所属任务的行业去重集合(contents 与 tasks 各查一次,无 N+1)
    let mut tq = content::Entity::find();
    if me.scope == "self" {
        tq = tq.filter(content::Column::Owner.eq(me.name.clone()));
    }
    let author_tasks: Vec<(String, String, String, String)> = tq
        .select_only()
        .column(content::Column::Owner)
        .column(content::Column::Platform)
        .column(content::Column::AuthorUid)
        .column(content::Column::TaskId)
        .group_by(content::Column::Owner)
        .group_by(content::Column::Platform)
        .group_by(content::Column::AuthorUid)
        .group_by(content::Column::TaskId)
        .into_tuple()
        .all(&state.db)
        .await
        .map_err(|e| CrawlerError::Config(format!("聚合作者任务失败: {e}")))?;
    let task_ids: std::collections::HashSet<String> =
        author_tasks.iter().map(|(_, _, _, tid)| tid.clone()).collect();
    let industry_map: std::collections::HashMap<String, String> = task::Entity::find()
        .filter(task::Column::Id.is_in(task_ids))
        .all(&state.db)
        .await
        .map_err(|e| CrawlerError::Config(format!("查询任务行业失败: {e}")))?
        .into_iter()
        .map(|t| (t.id, t.industry))
        .collect();
    // BTreeSet 保证行业列表输出顺序稳定
    let mut author_industries: std::collections::HashMap<
        String,
        std::collections::BTreeSet<String>,
    > = std::collections::HashMap::new();
    for (owner, platform, uid, tid) in author_tasks {
        if let Some(industry) = industry_map.get(&tid) {
            if !industry.is_empty() {
                author_industries
                    .entry(format!("{owner}-{platform}-{uid}"))
                    .or_default()
                    .insert(industry.clone());
            }
        }
    }

    Ok(rows
        .into_iter()
        .map(|m| AuthorView {
            content_count: count_map.get(&m.id).copied().unwrap_or(0),
            industries: author_industries
                .get(&m.id)
                .map(|s| s.iter().cloned().collect())
                .unwrap_or_default(),
            id: m.id,
            owner: m.owner,
            platform: m.platform,
            uid: m.uid,
            nickname: m.nickname,
            avatar: m.avatar,
            platform_id: m.platform_id,
            signature: m.signature,
            follower_count: m.follower_count,
            following_count: m.following_count,
            total_favorited: m.total_favorited,
            location: m.location,
            is_monitored: m.is_monitored,
            first_collected_at: m.first_collected_at,
            last_collected_at: m.last_collected_at,
        })
        .collect())
}

/// 作者库的监控开关(按作者 id 直改;与内容详情按 content_id 的入口并存)。
#[tauri::command]
pub async fn set_author_monitored_by_id(
    state: State<'_, AppState>,
    id: String,
    monitored: bool,
) -> Result<()> {
    use veltrix_core::db::entity::author as author_entity;
    let me = current_user(&state).ok_or_else(|| CrawlerError::Config("未登录".into()))?;
    let model = author_entity::Entity::find_by_id(id)
        .one(&state.db)
        .await
        .map_err(|e| CrawlerError::Config(format!("查询作者失败: {e}")))?
        .ok_or_else(|| CrawlerError::Config("作者不存在".into()))?;
    if me.scope == "self" && model.owner != me.name {
        return Err(CrawlerError::Config("无权操作该作者".into()));
    }
    let mut am = model.into_active_model();
    am.is_monitored = Set(monitored);
    am.update(&state.db)
        .await
        .map_err(|e| CrawlerError::Config(format!("更新监控状态失败: {e}")))?;
    Ok(())
}

/// 设置作者监控开关(内容详情里的「监控状态」)。作者不在表中(旧数据)则按 content 快照回填一行再置。
#[tauri::command]
pub async fn set_author_monitored(
    state: State<'_, AppState>,
    content_id: String,
    monitored: bool,
) -> Result<()> {
    use veltrix_core::db::entity::author as author_entity;
    let me = current_user(&state).ok_or_else(|| CrawlerError::Config("未登录".into()))?;
    let row = content::Entity::find_by_id(content_id)
        .one(&state.db)
        .await
        .map_err(|e| CrawlerError::Config(format!("查询内容失败: {e}")))?
        .ok_or_else(|| CrawlerError::Config("内容不存在".into()))?;
    if me.scope == "self" && row.owner != me.name {
        return Err(CrawlerError::Config("无权操作该作者".into()));
    }
    let aid = format!("{}-{}-{}", row.owner, row.platform, row.author_uid);
    let existing = author_entity::Entity::find_by_id(aid.clone())
        .one(&state.db)
        .await
        .ok()
        .flatten();
    if let Some(m) = existing {
        let mut am = m.into_active_model();
        am.is_monitored = Set(monitored);
        am.update(&state.db)
            .await
            .map_err(|e| CrawlerError::Config(format!("更新监控状态失败: {e}")))?;
    } else {
        // 旧数据未回填作者表:用 content 快照建一行
        let av = serde_json::from_str::<serde_json::Value>(&row.author_json).ok();
        let top_str = |key: &str| {
            av.as_ref()
                .and_then(|v| v.get(key))
                .and_then(|x| x.as_str())
                .filter(|s| !s.is_empty())
                .map(str::to_string)
        };
        let top_i64 = |key: &str| av.as_ref().and_then(|v| v.get(key)).and_then(|x| x.as_i64());
        let extra_str = |key: &str| {
            av.as_ref()
                .and_then(|v| v.get("extra"))
                .and_then(|e| e.get(key))
                .and_then(|x| x.as_str())
                .filter(|s| !s.is_empty())
                .map(str::to_string)
        };
        let extra_i64 = |key: &str| {
            av.as_ref()
                .and_then(|v| v.get("extra"))
                .and_then(|e| e.get(key))
                .and_then(|x| x.as_i64())
        };
        let am = author_entity::ActiveModel {
            id: Set(aid),
            owner: Set(row.owner.clone()),
            platform: Set(row.platform.clone()),
            uid: Set(row.author_uid.clone()),
            nickname: Set(row.author_nickname.clone()),
            avatar: Set(top_str("avatar")),
            platform_id: Set(extra_str("unique_id")),
            short_id: Set(extra_str("uid")),
            signature: Set(top_str("signature")),
            follower_count: Set(top_i64("follower_count")),
            following_count: Set(top_i64("following_count")),
            total_favorited: Set(extra_i64("total_favorited")),
            location: Set(extra_str("ip_location")),
            is_monitored: Set(monitored),
            first_collected_at: Set(row.collected_at),
            last_collected_at: Set(row.collected_at),
        };
        am.insert(&state.db)
            .await
            .map_err(|e| CrawlerError::Config(format!("创建作者档案失败: {e}")))?;
    }
    Ok(())
}

/// 一次性迁移:authors 表为空时,从 content 存量回填作者档案。
/// 按 owner+platform+uid 去重,升序扫取最新画像 + 最早采集时间。幂等:已有作者数据则跳过。
pub async fn migrate_authors_from_contents(db: &sea_orm::DatabaseConnection) {
    use veltrix_core::db::entity::author as author_entity;
    if author_entity::Entity::find().count(db).await.unwrap_or(0) > 0 {
        return;
    }
    let rows = match content::Entity::find()
        .order_by_asc(content::Column::CollectedAt)
        .all(db)
        .await
    {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!("迁移作者:读 content 失败: {e}");
            return;
        }
    };
    let mut first_seen: std::collections::HashMap<String, i64> = std::collections::HashMap::new();
    let mut map: std::collections::HashMap<String, author_entity::ActiveModel> =
        std::collections::HashMap::new();
    for c in &rows {
        if c.author_uid.is_empty() {
            continue;
        }
        let aid = format!("{}-{}-{}", c.owner, c.platform, c.author_uid);
        let av = serde_json::from_str::<serde_json::Value>(&c.author_json).ok();
        let top_str = |key: &str| {
            av.as_ref()
                .and_then(|v| v.get(key))
                .and_then(|x| x.as_str())
                .filter(|s| !s.is_empty())
                .map(str::to_string)
        };
        let top_i64 = |key: &str| av.as_ref().and_then(|v| v.get(key)).and_then(|x| x.as_i64());
        let extra_str = |key: &str| {
            av.as_ref()
                .and_then(|v| v.get("extra"))
                .and_then(|e| e.get(key))
                .and_then(|x| x.as_str())
                .filter(|s| !s.is_empty())
                .map(str::to_string)
        };
        let extra_i64 = |key: &str| {
            av.as_ref()
                .and_then(|v| v.get("extra"))
                .and_then(|e| e.get(key))
                .and_then(|x| x.as_i64())
        };
        // 升序扫:首次出现即最早采集时间
        let first = *first_seen.entry(aid.clone()).or_insert(c.collected_at);
        let am = author_entity::ActiveModel {
            id: Set(aid.clone()),
            owner: Set(c.owner.clone()),
            platform: Set(c.platform.clone()),
            uid: Set(c.author_uid.clone()),
            nickname: Set(c.author_nickname.clone()),
            avatar: Set(top_str("avatar")),
            platform_id: Set(extra_str("unique_id")),
            short_id: Set(extra_str("uid")),
            signature: Set(top_str("signature")),
            follower_count: Set(top_i64("follower_count")),
            following_count: Set(top_i64("following_count")),
            total_favorited: Set(extra_i64("total_favorited")),
            location: Set(extra_str("ip_location")),
            is_monitored: Set(false),
            first_collected_at: Set(first),
            last_collected_at: Set(c.collected_at),
        };
        // 升序覆盖 → 最新画像;first_collected 由 first_seen 锁定最早
        map.insert(aid, am);
    }
    if map.is_empty() {
        return;
    }
    let authors: Vec<author_entity::ActiveModel> = map.into_values().collect();
    let total = authors.len();
    for chunk in authors.chunks(500) {
        if let Err(e) = author_entity::Entity::insert_many(chunk.to_vec())
            .exec(db)
            .await
        {
            tracing::warn!("迁移作者档案失败: {e}");
        }
    }
    tracing::info!("已从存量内容回填 {total} 位作者到 authors 表");
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

/// 批量删除采集内容(全量库多选删除)。仅删库记录,媒体文件不动;
/// dataScope=self 的用户只能删自己 owner 的内容(越权 id 静默跳过)。返回实际删除条数。
#[tauri::command]
pub async fn remove_contents(state: State<'_, AppState>, ids: Vec<String>) -> Result<u64> {
    if ids.is_empty() {
        return Ok(0);
    }
    let me = current_user(&state).ok_or_else(|| CrawlerError::Config("未登录".into()))?;
    let mut q = content::Entity::delete_many().filter(content::Column::Id.is_in(ids));
    if me.scope == "self" {
        q = q.filter(content::Column::Owner.eq(me.name.clone()));
    }
    let res = q
        .exec(&state.db)
        .await
        .map_err(|e| CrawlerError::Config(format!("批量删除内容失败: {e}")))?;
    Ok(res.rows_affected)
}

/// 评论库视图。author_avatar 从完整作者 JSON 解析(实体只单列了 uid/nickname)。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CommentView {
    pub id: String,
    pub task_id: String,
    pub platform: String,
    pub content_id: String,
    pub comment_id: String,
    /// 父评论 ID;一级评论为空
    pub parent_id: Option<String>,
    pub author_uid: String,
    pub author_nickname: String,
    pub author_avatar: Option<String>,
    /// 作者平台号(抖音号 unique_id 等);从 author_json.extra.unique_id 提取
    pub author_unique_id: Option<String>,
    /// 所属行业:comment 表无此列,list_comments 关联 task.industry 填入
    pub industry: String,
    pub text: String,
    pub like_count: Option<i64>,
    pub reply_count: Option<i64>,
    pub created_at: Option<i64>,
    pub owner: String,
    pub collected_at: i64,
    /// AI 意向等级:high / medium / low / none;None=未分析
    pub intent_level: Option<String>,
    /// AI 意向理由;None=未分析
    pub intent_reason: Option<String>,
    /// 所属内容信息(list_comments 关联 contents 填;内容已删则为 None)
    pub content_title: Option<String>,
    pub content_kind: Option<String>,
    pub content_cover_url: Option<String>,
    pub content_cover_path: Option<String>,
    /// 内容作者(视频/图文创作者,区别于评论者 author_*)
    pub content_author_nickname: Option<String>,
    pub content_author_avatar: Option<String>,
}

impl From<comment::Model> for CommentView {
    fn from(m: comment::Model) -> Self {
        let author_val = serde_json::from_str::<serde_json::Value>(&m.author_json).ok();
        let author_avatar = author_val
            .as_ref()
            .and_then(|v| v.get("avatar").and_then(|a| a.as_str()).map(str::to_string));
        // 抖音号等平台号在 author.extra.unique_id
        let author_unique_id = author_val
            .as_ref()
            .and_then(|v| v.get("extra"))
            .and_then(|e| e.get("unique_id"))
            .and_then(|u| u.as_str())
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        Self {
            id: m.id,
            task_id: m.task_id,
            platform: m.platform,
            content_id: m.content_id,
            comment_id: m.comment_id,
            parent_id: m.parent_id,
            author_uid: m.author_uid,
            author_nickname: m.author_nickname,
            author_avatar,
            author_unique_id,
            industry: String::new(), // 由 list_comments 关联 task 后填充
            text: m.text,
            like_count: m.like_count,
            reply_count: m.reply_count,
            created_at: m.created_at,
            owner: m.owner,
            collected_at: m.collected_at,
            intent_level: m.intent_level,
            intent_reason: m.intent_reason,
            content_title: None,
            content_kind: None,
            content_cover_url: None,
            content_cover_path: None,
            content_author_nickname: None,
            content_author_avatar: None,
        }
    }
}

/// 评论库:列出采集落库的评论,按采集时间倒序。task_id 非空时按任务过滤;dataScope=self 仅看自己。
#[tauri::command]
pub async fn list_comments(
    state: State<'_, AppState>,
    task_id: Option<String>,
) -> Result<Vec<CommentView>> {
    let me = current_user(&state).ok_or_else(|| CrawlerError::Config("未登录".into()))?;
    let mut q = comment::Entity::find().order_by_desc(comment::Column::CollectedAt);
    if me.scope == "self" {
        q = q.filter(comment::Column::Owner.eq(me.name.clone()));
    }
    if let Some(tid) = task_id {
        if !tid.is_empty() {
            q = q.filter(comment::Column::TaskId.eq(tid));
        }
    }
    let rows = q
        .limit(LIST_HARD_CAP)
        .all(&state.db)
        .await
        .map_err(|e| CrawlerError::Config(format!("查询评论失败: {e}")))?;
    // 关联任务取行业(逻辑外键,照 list_contents)
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
    // 关联 contents 取所属内容信息(标题/封面/形态 + 内容作者),按 content.id 精确匹配
    let content_keys: std::collections::HashSet<String> = rows
        .iter()
        .map(|r| format!("{}-{}-{}", r.task_id, r.platform, r.content_id))
        .collect();
    let content_map: std::collections::HashMap<String, content::Model> = content::Entity::find()
        .filter(content::Column::Id.is_in(content_keys))
        .all(&state.db)
        .await
        .map_err(|e| CrawlerError::Config(format!("查询所属内容失败: {e}")))?
        .into_iter()
        .map(|c| (c.id.clone(), c))
        .collect();
    Ok(rows
        .into_iter()
        .map(|m| {
            let industry = industry_map.get(&m.task_id).cloned().unwrap_or_default();
            let cid = format!("{}-{}-{}", m.task_id, m.platform, m.content_id);
            let content = content_map.get(&cid);
            let mut view: CommentView = m.into();
            view.industry = industry;
            if let Some(c) = content {
                view.content_title = c.title.clone();
                view.content_kind = Some(c.kind.clone());
                view.content_cover_url = c.cover_url.clone();
                view.content_cover_path = c.cover_path.clone();
                view.content_author_nickname = Some(c.author_nickname.clone());
                view.content_author_avatar =
                    serde_json::from_str::<serde_json::Value>(&c.author_json)
                        .ok()
                        .and_then(|v| {
                            v.get("avatar").and_then(|a| a.as_str()).map(str::to_string)
                        });
            }
            view
        })
        .collect())
}

/// 采集日志视图(任务详情页加载历史)。entry 从 entry_json 解析回对象,与实时 collect-log 事件结构一致。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CollectLogView {
    pub task_id: String,
    pub ts: i64,
    pub level: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entry: Option<serde_json::Value>,
}

impl From<collect_log::Model> for CollectLogView {
    fn from(m: collect_log::Model) -> Self {
        let entry = m
            .entry_json
            .as_deref()
            .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok());
        Self {
            task_id: m.task_id,
            ts: m.ts,
            level: m.level,
            message: m.message,
            entry,
        }
    }
}

/// 加载某任务的历史采集日志,按时间正序返回。超过上限只回最近 N 条(取最大 id 再反转为正序)。
#[tauri::command]
pub async fn list_collect_logs(
    state: State<'_, AppState>,
    task_id: String,
) -> Result<Vec<CollectLogView>> {
    let mut rows = collect_log::Entity::find()
        .filter(collect_log::Column::TaskId.eq(task_id))
        .order_by_desc(collect_log::Column::Id)
        .limit(LOG_HARD_CAP)
        .all(&state.db)
        .await
        .map_err(|e| CrawlerError::Config(format!("查询采集日志失败: {e}")))?;
    // 取的是最近 N 条(id 倒序),反转回时间正序供前端顺序展示
    rows.reverse();
    Ok(rows.into_iter().map(Into::into).collect())
}

/// 任务执行历史视图(每次运行一条)。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskRunView {
    pub id: String,
    pub task_id: String,
    pub started_at: i64,
    pub finished_at: Option<i64>,
    pub status: String,
    pub content_delta: i64,
    pub comment_delta: i64,
    pub error_message: Option<String>,
}

impl From<task_run::Model> for TaskRunView {
    fn from(m: task_run::Model) -> Self {
        Self {
            id: m.id,
            task_id: m.task_id,
            started_at: m.started_at,
            finished_at: m.finished_at,
            status: m.status,
            content_delta: m.content_delta,
            comment_delta: m.comment_delta,
            error_message: m.error_message,
        }
    }
}

/// 任务执行历史:某任务的全部运行记录,最近的在前。self scope 仅看自己。
#[tauri::command]
pub async fn list_task_runs(
    state: State<'_, AppState>,
    task_id: String,
) -> Result<Vec<TaskRunView>> {
    let me = current_user(&state).ok_or_else(|| CrawlerError::Config("未登录".into()))?;
    let mut q = task_run::Entity::find()
        .filter(task_run::Column::TaskId.eq(task_id))
        .order_by_desc(task_run::Column::StartedAt);
    if me.scope == "self" {
        q = q.filter(task_run::Column::Owner.eq(me.name.clone()));
    }
    let rows = q
        .limit(LIST_HARD_CAP)
        .all(&state.db)
        .await
        .map_err(|e| CrawlerError::Config(format!("查询执行历史失败: {e}")))?;
    Ok(rows.into_iter().map(Into::into).collect())
}

/// 某次运行的采集日志:按该运行的 [started_at, finished_at] 时间范围从 collect_logs 切分;
/// 运行中(finished_at 为 None)取 started_at 至今全部。按时间正序返回。
#[tauri::command]
pub async fn list_run_logs(
    state: State<'_, AppState>,
    run_id: String,
) -> Result<Vec<CollectLogView>> {
    let run = task_run::Entity::find_by_id(run_id)
        .one(&state.db)
        .await
        .map_err(|e| CrawlerError::Config(format!("查询执行记录失败: {e}")))?
        .ok_or_else(|| CrawlerError::Config("执行记录不存在".into()))?;
    let mut q = collect_log::Entity::find()
        .filter(collect_log::Column::TaskId.eq(run.task_id))
        .filter(collect_log::Column::Ts.gte(run.started_at));
    if let Some(end) = run.finished_at {
        q = q.filter(collect_log::Column::Ts.lte(end));
    }
    let rows = q
        .order_by_asc(collect_log::Column::Id)
        .limit(LOG_HARD_CAP)
        .all(&state.db)
        .await
        .map_err(|e| CrawlerError::Config(format!("查询运行日志失败: {e}")))?;
    Ok(rows.into_iter().map(Into::into).collect())
}

/// 数据概览(首页):全量库 / 评论库 / 意向客资 计数(含平台细分)+ 可选区间的多平台采集趋势。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DashboardOverview {
    pub content_total: i64,
    pub comment_total: i64,
    pub intent_total: i64,
    /// 全量库平台细分
    pub content_by_platform: Vec<PlatformCount>,
    /// 全量库内容形态:视频数
    pub content_video: i64,
    /// 全量库内容形态:图文数
    pub content_image: i64,
    /// 评论库平台细分
    pub comment_by_platform: Vec<PlatformCount>,
    /// 评论库内容形态:视频内容下的评论数
    pub comment_video: i64,
    /// 评论库内容形态:图文内容下的评论数
    pub comment_image: i64,
    /// 意向客资平台细分
    pub intent_by_platform: Vec<PlatformCount>,
    /// 意向客资内容形态:视频内容下的高意向评论数
    pub intent_video: i64,
    /// 意向客资内容形态:图文内容下的高意向评论数
    pub intent_image: i64,
    /// 趋势日期轴(MM-DD)
    pub trend_dates: Vec<String>,
    /// 每平台一条采集量序列(counts 与 trend_dates 一一对应,内容+评论合计)
    pub trend_series: Vec<PlatformSeries>,
    /// 意向分布(高/中/低/无)
    pub intent_distribution: IntentDistribution,
    /// 今日采集 + 环比昨日
    pub today: TodayStat,
    /// 任务状态概况
    pub task_status: TaskStatusStat,
    /// 热门内容 Top N(按点赞)
    pub hot_contents: Vec<HotContent>,
    /// 素材下载概况
    pub media_stats: MediaStat,
    /// 热门关键词 Top N(按采集量)
    pub top_keywords: Vec<KeywordCount>,
}

/// 意向分布。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IntentDistribution {
    pub high: i64,
    pub medium: i64,
    pub low: i64,
    pub none: i64,
}

/// 今日采集统计(含环比昨日的增量)。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TodayStat {
    pub contents: i64,
    pub comments: i64,
    pub contents_delta: i64,
    pub comments_delta: i64,
    /// 今日各平台采集细分(仅今日有采集的平台)
    pub by_platform: Vec<TodayPlatform>,
}

/// 今日单平台采集量。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TodayPlatform {
    pub platform: String,
    pub contents: i64,
    pub comments: i64,
}

/// 任务状态概况。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskStatusStat {
    pub running: i64,
    pub pending: i64,
    pub completed_today: i64,
    pub failed: i64,
}

/// 热门内容条目。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HotContent {
    pub title: String,
    pub platform: String,
    pub author: String,
    pub like_count: i64,
    pub comment_count: i64,
}

/// 素材下载概况。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaStat {
    pub success: i64,
    pub pending: i64,
    pub failed: i64,
}

/// 关键词计数。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KeywordCount {
    pub keyword: String,
    pub count: i64,
}

/// 平台计数(卡片细分用)。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlatformCount {
    pub platform: String,
    pub count: i64,
}

/// 单平台趋势序列(counts 与 trend_dates 一一对应)。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlatformSeries {
    pub platform: String,
    /// 合计(内容 + 评论),趋势线用
    pub counts: Vec<i64>,
    /// 内容数(悬停明细)
    pub contents: Vec<i64>,
    /// 评论数(悬停明细)
    pub comments: Vec<i64>,
}

/// 趋势默认天数(未指定区间时)。
const TREND_DAYS: i64 = 14;
/// 趋势最大跨度(防止区间过大撑爆)。
const TREND_MAX_DAYS: usize = 90;

#[tauri::command]
pub async fn dashboard_overview(
    state: State<'_, AppState>,
    start: Option<i64>,
    end: Option<i64>,
) -> Result<DashboardOverview> {
    let me = current_user(&state).ok_or_else(|| CrawlerError::Config("未登录".into()))?;
    let self_only = me.scope == "self";
    let db = &state.db;
    let owner = me.name.clone();

    // 所有启用平台 id(没数据的平台也要列出来并补 0,趋势线 / 卡片细分都覆盖全平台)
    let platform_ids: Vec<String> = {
        let cfg = state
            .config
            .lock()
            .map_err(|_| CrawlerError::Config("配置状态锁异常".into()))?;
        cfg.platforms
            .values()
            .filter(|p| p.enabled)
            .map(|p| p.id.clone())
            .collect()
    };

    // 各项查询互不依赖,并行执行(try_join! 任一失败即整体失败);大库下显著缩短首页等待。
    let (
        content_pc,
        comment_pc,
        intent_pc,
        content_kinds,
        comment_kinds,
        intent_kinds,
        trend,
        intent_dist,
        today,
        task_status,
        hot_contents,
        media_stats,
        top_keywords,
    ) = tokio::try_join!(
        content_platform_counts(db, self_only, &owner),
        comment_platform_counts(db, self_only, &owner, false),
        comment_platform_counts(db, self_only, &owner, true),
        content_kind_counts(db, self_only, &owner),
        comment_kind_counts(db, self_only, &owner, false),
        comment_kind_counts(db, self_only, &owner, true),
        dashboard_trend(db, self_only, &owner, start, end, &platform_ids),
        intent_distribution(db, self_only, &owner),
        today_stat(db, self_only, &owner),
        task_status_stat(db, self_only, &owner),
        hot_contents(db, self_only, &owner),
        media_stat(db, self_only, &owner),
        top_topics(db, self_only, &owner),
    )?;

    // 三项计数:平台细分(补全所有平台,没数据补 0;总数由实际值求和)
    let content_by_platform = fill_platform_counts(content_pc, &platform_ids);
    let comment_by_platform = fill_platform_counts(comment_pc, &platform_ids);
    let intent_by_platform = fill_platform_counts(intent_pc, &platform_ids);
    let sum = |v: &[PlatformCount]| v.iter().map(|p| p.count).sum::<i64>();
    let content_total = sum(&content_by_platform);
    let comment_total = sum(&comment_by_platform);
    let intent_total = sum(&intent_by_platform);
    // 内容形态(视频 / 图文):全量库按内容,评论库 / 意向客资按评论所属内容
    let (content_video, content_image) = content_kinds;
    let (comment_video, comment_image) = comment_kinds;
    let (intent_video, intent_image) = intent_kinds;
    let (trend_dates, trend_series) = trend;
    let intent_distribution = intent_dist;

    Ok(DashboardOverview {
        content_total,
        comment_total,
        intent_total,
        content_by_platform,
        content_video,
        content_image,
        comment_by_platform,
        comment_video,
        comment_image,
        intent_by_platform,
        intent_video,
        intent_image,
        trend_dates,
        trend_series,
        intent_distribution,
        today,
        task_status,
        hot_contents,
        media_stats,
        top_keywords,
    })
}

/// 补全平台细分:platform_ids 中未出现的平台补 0,并按 platform 排序稳定输出。
fn fill_platform_counts(
    mut counts: Vec<PlatformCount>,
    platform_ids: &[String],
) -> Vec<PlatformCount> {
    let existing: std::collections::HashSet<String> =
        counts.iter().map(|c| c.platform.clone()).collect();
    for id in platform_ids {
        if !existing.contains(id) {
            counts.push(PlatformCount {
                platform: id.clone(),
                count: 0,
            });
        }
    }
    counts.sort_by(|a, b| a.platform.cmp(&b.platform));
    counts
}

/// 按平台统计内容数(group by platform)。
/// 全量库内容形态统计:返回 (视频数, 图文数)。
async fn content_kind_counts(
    db: &sea_orm::DatabaseConnection,
    self_only: bool,
    owner: &str,
) -> Result<(i64, i64)> {
    let mut q = content::Entity::find();
    if self_only {
        q = q.filter(content::Column::Owner.eq(owner.to_string()));
    }
    let rows: Vec<(String, i64)> = q
        .select_only()
        .column(content::Column::Kind)
        .column_as(content::Column::Id.count(), "count")
        .group_by(content::Column::Kind)
        .into_tuple()
        .all(db)
        .await
        .map_err(|e| CrawlerError::Config(format!("统计内容形态失败: {e}")))?;
    let mut video = 0i64;
    let mut image = 0i64;
    for (kind, count) in rows {
        match kind.as_str() {
            "video" => video = count,
            "image" => image = count,
            _ => {}
        }
    }
    Ok((video, image))
}

/// 评论所属内容的形态统计:返回 (视频内容评论数, 图文内容评论数)。
/// high_only=true 时只统计高意向评论(用于意向客资)。
async fn comment_kind_counts(
    db: &sea_orm::DatabaseConnection,
    self_only: bool,
    owner: &str,
    high_only: bool,
) -> Result<(i64, i64)> {
    // content 的 (platform, content_id) → kind 映射(kind 客观,不按 owner 过滤)
    let content_rows: Vec<(String, String, String)> = content::Entity::find()
        .select_only()
        .column(content::Column::Platform)
        .column(content::Column::ContentId)
        .column(content::Column::Kind)
        .into_tuple()
        .all(db)
        .await
        .map_err(|e| CrawlerError::Config(format!("查询内容形态失败: {e}")))?;
    let mut kind_map: std::collections::HashMap<(String, String), String> =
        std::collections::HashMap::new();
    for (platform, content_id, kind) in content_rows {
        kind_map.entry((platform, content_id)).or_insert(kind);
    }

    // 评论按所属内容形态归类
    let mut cq = comment::Entity::find();
    if high_only {
        cq = cq.filter(comment::Column::IntentLevel.eq("high"));
    }
    if self_only {
        cq = cq.filter(comment::Column::Owner.eq(owner.to_string()));
    }
    let comment_rows: Vec<(String, String)> = cq
        .select_only()
        .column(comment::Column::Platform)
        .column(comment::Column::ContentId)
        .into_tuple()
        .all(db)
        .await
        .map_err(|e| CrawlerError::Config(format!("查询评论形态失败: {e}")))?;
    let mut video = 0i64;
    let mut image = 0i64;
    for (platform, content_id) in comment_rows {
        match kind_map.get(&(platform, content_id)).map(String::as_str) {
            Some("video") => video += 1,
            Some("image") => image += 1,
            _ => {}
        }
    }
    Ok((video, image))
}

async fn content_platform_counts(
    db: &sea_orm::DatabaseConnection,
    self_only: bool,
    owner: &str,
) -> Result<Vec<PlatformCount>> {
    let mut q = content::Entity::find();
    if self_only {
        q = q.filter(content::Column::Owner.eq(owner.to_string()));
    }
    let rows: Vec<(String, i64)> = q
        .select_only()
        .column(content::Column::Platform)
        .column_as(content::Column::Id.count(), "count")
        .group_by(content::Column::Platform)
        .into_tuple()
        .all(db)
        .await
        .map_err(|e| CrawlerError::Config(format!("统计内容失败: {e}")))?;
    Ok(rows
        .into_iter()
        .map(|(platform, count)| PlatformCount { platform, count })
        .collect())
}

/// 按平台统计评论数;high_only=true 时只统计高意向评论(意向客资)。
async fn comment_platform_counts(
    db: &sea_orm::DatabaseConnection,
    self_only: bool,
    owner: &str,
    high_only: bool,
) -> Result<Vec<PlatformCount>> {
    let mut q = comment::Entity::find();
    if high_only {
        q = q.filter(comment::Column::IntentLevel.eq("high"));
    }
    if self_only {
        q = q.filter(comment::Column::Owner.eq(owner.to_string()));
    }
    let rows: Vec<(String, i64)> = q
        .select_only()
        .column(comment::Column::Platform)
        .column_as(comment::Column::Id.count(), "count")
        .group_by(comment::Column::Platform)
        .into_tuple()
        .all(db)
        .await
        .map_err(|e| CrawlerError::Config(format!("统计评论失败: {e}")))?;
    Ok(rows
        .into_iter()
        .map(|(platform, count)| PlatformCount { platform, count })
        .collect())
}

/// 区间内每平台每天的采集量(内容 + 评论合计)。未指定区间默认近 TREND_DAYS 天。
/// 返回 (日期轴, 每平台序列)。日期按 UTC 日历日归并。
async fn dashboard_trend(
    db: &sea_orm::DatabaseConnection,
    self_only: bool,
    owner: &str,
    start: Option<i64>,
    end: Option<i64>,
    platform_ids: &[String],
) -> Result<(Vec<String>, Vec<PlatformSeries>)> {
    let end_ts = end.unwrap_or_else(|| Utc::now().timestamp());
    let start_ts = start.unwrap_or_else(|| {
        let today = Utc::now().date_naive();
        (today - chrono::Duration::days(TREND_DAYS - 1))
            .and_hms_opt(0, 0, 0)
            .map(|dt| dt.and_utc().timestamp())
            .unwrap_or(0)
    });

    // 日期轴(UTC 日历日,MM-DD)
    let start_date = chrono::DateTime::from_timestamp(start_ts, 0)
        .map(|dt| dt.date_naive())
        .unwrap_or_else(|| Utc::now().date_naive());
    let end_date = chrono::DateTime::from_timestamp(end_ts, 0)
        .map(|dt| dt.date_naive())
        .unwrap_or_else(|| Utc::now().date_naive());
    let mut dates: Vec<String> = Vec::new();
    let mut date_idx: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    let mut d = start_date;
    while d <= end_date && dates.len() < TREND_MAX_DAYS {
        let s = d.format("%m-%d").to_string();
        date_idx.insert(s.clone(), dates.len());
        dates.push(s);
        d += chrono::Duration::days(1);
    }

    let day_of = |ts: i64| -> Option<String> {
        chrono::DateTime::from_timestamp(ts, 0).map(|dt| dt.format("%m-%d").to_string())
    };

    // 取 (platform, collected_at):内容 + 评论
    let mut cq = content::Entity::find()
        .filter(content::Column::CollectedAt.gte(start_ts))
        .filter(content::Column::CollectedAt.lte(end_ts));
    if self_only {
        cq = cq.filter(content::Column::Owner.eq(owner.to_string()));
    }
    let content_rows: Vec<(String, i64)> = cq
        .select_only()
        .column(content::Column::Platform)
        .column(content::Column::CollectedAt)
        .into_tuple()
        .all(db)
        .await
        .map_err(|e| CrawlerError::Config(format!("查询采集趋势失败: {e}")))?;

    let mut mq = comment::Entity::find()
        .filter(comment::Column::CollectedAt.gte(start_ts))
        .filter(comment::Column::CollectedAt.lte(end_ts));
    if self_only {
        mq = mq.filter(comment::Column::Owner.eq(owner.to_string()));
    }
    let comment_rows: Vec<(String, i64)> = mq
        .select_only()
        .column(comment::Column::Platform)
        .column(comment::Column::CollectedAt)
        .into_tuple()
        .all(db)
        .await
        .map_err(|e| CrawlerError::Config(format!("查询评论趋势失败: {e}")))?;

    // platform → 每天 count(内容 + 评论合计);先把所有平台补 0 序列,确保趋势覆盖全平台
    let dlen = dates.len();
    // 各平台:内容 / 评论分开按天累加(趋势线画合计,悬停显示明细)
    let mut content_map: std::collections::HashMap<String, Vec<i64>> = platform_ids
        .iter()
        .map(|p| (p.clone(), vec![0i64; dlen]))
        .collect();
    let mut comment_map: std::collections::HashMap<String, Vec<i64>> = platform_ids
        .iter()
        .map(|p| (p.clone(), vec![0i64; dlen]))
        .collect();
    for (platform, ts) in content_rows {
        if let Some(di) = day_of(ts).and_then(|s| date_idx.get(&s).copied()) {
            content_map
                .entry(platform)
                .or_insert_with(|| vec![0i64; dlen])[di] += 1;
        }
    }
    for (platform, ts) in comment_rows {
        if let Some(di) = day_of(ts).and_then(|s| date_idx.get(&s).copied()) {
            comment_map
                .entry(platform)
                .or_insert_with(|| vec![0i64; dlen])[di] += 1;
        }
    }

    // 合并平台(内容 / 评论并集),按平台名稳定排序
    let mut platforms: std::collections::BTreeSet<String> =
        content_map.keys().cloned().collect();
    platforms.extend(comment_map.keys().cloned());
    let trend_series: Vec<PlatformSeries> = platforms
        .into_iter()
        .map(|platform| {
            let contents = content_map
                .get(&platform)
                .cloned()
                .unwrap_or_else(|| vec![0i64; dlen]);
            let comments = comment_map
                .get(&platform)
                .cloned()
                .unwrap_or_else(|| vec![0i64; dlen]);
            let counts: Vec<i64> = contents
                .iter()
                .zip(comments.iter())
                .map(|(a, b)| a + b)
                .collect();
            PlatformSeries {
                platform,
                counts,
                contents,
                comments,
            }
        })
        .collect();

    Ok((dates, trend_series))
}

/// 意向分布:评论按 intent_level 分组(null / 未知归入 none)。
async fn intent_distribution(
    db: &sea_orm::DatabaseConnection,
    self_only: bool,
    owner: &str,
) -> Result<IntentDistribution> {
    let mut q = comment::Entity::find();
    if self_only {
        q = q.filter(comment::Column::Owner.eq(owner.to_string()));
    }
    let rows: Vec<(Option<String>, i64)> = q
        .select_only()
        .column(comment::Column::IntentLevel)
        .column_as(comment::Column::Id.count(), "count")
        .group_by(comment::Column::IntentLevel)
        .into_tuple()
        .all(db)
        .await
        .map_err(|e| CrawlerError::Config(format!("统计意向分布失败: {e}")))?;
    let mut dist = IntentDistribution {
        high: 0,
        medium: 0,
        low: 0,
        none: 0,
    };
    for (level, count) in rows {
        match level.as_deref() {
            Some("high") => dist.high = count,
            Some("medium") => dist.medium = count,
            Some("low") => dist.low = count,
            _ => dist.none += count,
        }
    }
    Ok(dist)
}

/// 统计某时间区间 [from, to) 的内容数(to=None 表示到现在)。
async fn count_content_range(
    db: &sea_orm::DatabaseConnection,
    self_only: bool,
    owner: &str,
    from: i64,
    to: Option<i64>,
) -> Result<i64> {
    let mut q = content::Entity::find().filter(content::Column::CollectedAt.gte(from));
    if let Some(t) = to {
        q = q.filter(content::Column::CollectedAt.lt(t));
    }
    if self_only {
        q = q.filter(content::Column::Owner.eq(owner.to_string()));
    }
    Ok(q
        .count(db)
        .await
        .map_err(|e| CrawlerError::Config(format!("统计内容失败: {e}")))? as i64)
}

/// 统计某时间区间 [from, to) 的评论数(to=None 表示到现在)。
async fn count_comment_range(
    db: &sea_orm::DatabaseConnection,
    self_only: bool,
    owner: &str,
    from: i64,
    to: Option<i64>,
) -> Result<i64> {
    let mut q = comment::Entity::find().filter(comment::Column::CollectedAt.gte(from));
    if let Some(t) = to {
        q = q.filter(comment::Column::CollectedAt.lt(t));
    }
    if self_only {
        q = q.filter(comment::Column::Owner.eq(owner.to_string()));
    }
    Ok(q
        .count(db)
        .await
        .map_err(|e| CrawlerError::Config(format!("统计评论失败: {e}")))? as i64)
}

/// 今日采集 + 环比昨日。
async fn today_stat(
    db: &sea_orm::DatabaseConnection,
    self_only: bool,
    owner: &str,
) -> Result<TodayStat> {
    let today0 = Utc::now()
        .date_naive()
        .and_hms_opt(0, 0, 0)
        .map(|dt| dt.and_utc().timestamp())
        .unwrap_or(0);
    let yest0 = today0 - 86_400;
    let contents = count_content_range(db, self_only, owner, today0, None).await?;
    let comments = count_comment_range(db, self_only, owner, today0, None).await?;
    let yest_contents = count_content_range(db, self_only, owner, yest0, Some(today0)).await?;
    let yest_comments = count_comment_range(db, self_only, owner, yest0, Some(today0)).await?;
    let by_platform = today_by_platform(db, self_only, owner, today0).await?;
    Ok(TodayStat {
        contents,
        comments,
        contents_delta: contents - yest_contents,
        comments_delta: comments - yest_comments,
        by_platform,
    })
}

/// 今日各平台采集细分(content / comment 按 platform 分组合并,仅保留今日有采集的平台)。
async fn today_by_platform(
    db: &sea_orm::DatabaseConnection,
    self_only: bool,
    owner: &str,
    today0: i64,
) -> Result<Vec<TodayPlatform>> {
    let mut cq = content::Entity::find().filter(content::Column::CollectedAt.gte(today0));
    if self_only {
        cq = cq.filter(content::Column::Owner.eq(owner.to_string()));
    }
    let content_rows: Vec<(String, i64)> = cq
        .select_only()
        .column(content::Column::Platform)
        .column_as(content::Column::Id.count(), "count")
        .group_by(content::Column::Platform)
        .into_tuple()
        .all(db)
        .await
        .map_err(|e| CrawlerError::Config(format!("统计今日内容失败: {e}")))?;

    let mut mq = comment::Entity::find().filter(comment::Column::CollectedAt.gte(today0));
    if self_only {
        mq = mq.filter(comment::Column::Owner.eq(owner.to_string()));
    }
    let comment_rows: Vec<(String, i64)> = mq
        .select_only()
        .column(comment::Column::Platform)
        .column_as(comment::Column::Id.count(), "count")
        .group_by(comment::Column::Platform)
        .into_tuple()
        .all(db)
        .await
        .map_err(|e| CrawlerError::Config(format!("统计今日评论失败: {e}")))?;

    // 按平台合并内容数 / 评论数(BTreeMap 保证平台顺序稳定)
    let mut map: std::collections::BTreeMap<String, (i64, i64)> =
        std::collections::BTreeMap::new();
    for (platform, count) in content_rows {
        map.entry(platform).or_default().0 = count;
    }
    for (platform, count) in comment_rows {
        map.entry(platform).or_default().1 = count;
    }
    Ok(map
        .into_iter()
        .map(|(platform, (contents, comments))| TodayPlatform {
            platform,
            contents,
            comments,
        })
        .collect())
}

/// 任务状态概况。
async fn task_status_stat(
    db: &sea_orm::DatabaseConnection,
    self_only: bool,
    owner: &str,
) -> Result<TaskStatusStat> {
    let mut q = task::Entity::find();
    if self_only {
        q = q.filter(task::Column::Owner.eq(owner.to_string()));
    }
    let rows: Vec<(String, i64)> = q
        .select_only()
        .column(task::Column::Status)
        .column_as(task::Column::Id.count(), "count")
        .group_by(task::Column::Status)
        .into_tuple()
        .all(db)
        .await
        .map_err(|e| CrawlerError::Config(format!("统计任务状态失败: {e}")))?;
    let mut stat = TaskStatusStat {
        running: 0,
        pending: 0,
        completed_today: 0,
        failed: 0,
    };
    for (status, count) in rows {
        match status.as_str() {
            "running" | "collecting_comments" | "analyzing_comments" | "downloading_media" => {
                stat.running += count
            }
            "pending" => stat.pending = count,
            "failed" => stat.failed = count,
            _ => {}
        }
    }
    // 今日完成:completed 且 finished_at 落在今天
    let today0 = Utc::now()
        .date_naive()
        .and_hms_opt(0, 0, 0)
        .map(|dt| dt.and_utc().timestamp())
        .unwrap_or(0);
    let mut cq = task::Entity::find()
        .filter(task::Column::Status.eq("completed"))
        .filter(task::Column::FinishedAt.gte(today0));
    if self_only {
        cq = cq.filter(task::Column::Owner.eq(owner.to_string()));
    }
    stat.completed_today = cq
        .count(db)
        .await
        .map_err(|e| CrawlerError::Config(format!("统计今日完成失败: {e}")))? as i64;
    Ok(stat)
}

/// 热门内容 Top 8(按点赞数降序)。
async fn hot_contents(
    db: &sea_orm::DatabaseConnection,
    self_only: bool,
    owner: &str,
) -> Result<Vec<HotContent>> {
    let mut q = content::Entity::find();
    if self_only {
        q = q.filter(content::Column::Owner.eq(owner.to_string()));
    }
    let rows = q
        .order_by_desc(content::Column::LikeCount)
        .limit(50)
        .all(db)
        .await
        .map_err(|e| CrawlerError::Config(format!("查询热门内容失败: {e}")))?;
    Ok(rows
        .into_iter()
        .map(|c| HotContent {
            title: c
                .title
                .filter(|s| !s.trim().is_empty())
                .or(c.desc)
                .unwrap_or_default(),
            platform: c.platform,
            author: c.author_nickname,
            like_count: c.like_count.unwrap_or(0),
            comment_count: c.comment_count.unwrap_or(0),
        })
        .collect())
}

/// 素材下载概况(success / failed / 其余归 pending)。
async fn media_stat(
    db: &sea_orm::DatabaseConnection,
    self_only: bool,
    owner: &str,
) -> Result<MediaStat> {
    let mut q = content::Entity::find();
    if self_only {
        q = q.filter(content::Column::Owner.eq(owner.to_string()));
    }
    let rows: Vec<(Option<String>, i64)> = q
        .select_only()
        .column(content::Column::MediaStatus)
        .column_as(content::Column::Id.count(), "count")
        .group_by(content::Column::MediaStatus)
        .into_tuple()
        .all(db)
        .await
        .map_err(|e| CrawlerError::Config(format!("统计素材状态失败: {e}")))?;
    let mut stat = MediaStat {
        success: 0,
        pending: 0,
        failed: 0,
    };
    for (s, count) in rows {
        match s.as_deref() {
            Some("success") => stat.success = count,
            Some("failed") => stat.failed = count,
            _ => stat.pending += count,
        }
    }
    Ok(stat)
}

/// 热门话题 Top 50:从内容的 topics(# 话题标签 JSON 数组)逐条展开后聚合计数。
async fn top_topics(
    db: &sea_orm::DatabaseConnection,
    self_only: bool,
    owner: &str,
) -> Result<Vec<KeywordCount>> {
    let mut q = content::Entity::find();
    if self_only {
        q = q.filter(content::Column::Owner.eq(owner.to_string()));
    }
    let rows: Vec<String> = q
        .select_only()
        .column(content::Column::Topics)
        .into_tuple()
        .all(db)
        .await
        .map_err(|e| CrawlerError::Config(format!("统计热门话题失败: {e}")))?;
    // topics 是 JSON 数组字符串(如 ["#话题a","#话题b"]),展开后按出现次数计数
    let mut counter: std::collections::HashMap<String, i64> =
        std::collections::HashMap::new();
    for topics_json in rows {
        let Ok(topics) = serde_json::from_str::<Vec<String>>(&topics_json) else {
            continue;
        };
        for raw in topics {
            let topic = raw.trim().trim_start_matches('#').trim();
            if topic.is_empty() {
                continue;
            }
            *counter.entry(topic.to_string()).or_insert(0) += 1;
        }
    }
    let mut list: Vec<(String, i64)> = counter.into_iter().collect();
    list.sort_by(|a, b| b.1.cmp(&a.1));
    list.truncate(50);
    Ok(list
        .into_iter()
        .map(|(keyword, count)| KeywordCount { keyword, count })
        .collect())
}
