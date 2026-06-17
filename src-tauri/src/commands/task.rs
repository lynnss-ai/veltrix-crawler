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
    /// 是否被拉黑:命中黑名单的作者在采集时被排除、不抓
    pub is_blacklisted: bool,
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
            is_blacklisted: m.is_blacklisted,
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

/// 作者库的黑名单开关(按作者 id 直改)。加入黑名单后,后续采集命中该作者的内容会被排除、不抓。
#[tauri::command]
pub async fn set_author_blacklisted_by_id(
    state: State<'_, AppState>,
    id: String,
    blacklisted: bool,
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
    am.is_blacklisted = Set(blacklisted);
    am.update(&state.db)
        .await
        .map_err(|e| CrawlerError::Config(format!("更新黑名单状态失败: {e}")))?;
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
            is_blacklisted: Set(false),
            first_collected_at: Set(row.collected_at),
            last_collected_at: Set(row.collected_at),
        };
        am.insert(&state.db)
            .await
            .map_err(|e| CrawlerError::Config(format!("创建作者档案失败: {e}")))?;
    }
    Ok(())
}

/// 一次性回填:历史内容 topics 为空但正文含 #话题 的,从正文(标题 + desc)补提取话题写回 topics。
/// 只补话题、不改正文(剥离正文有误删风险,故保守保留)。幂等:仅处理 topics 为空的行,可安全重跑。
pub async fn backfill_empty_topics(db: &sea_orm::DatabaseConnection) {
    use sea_orm::Condition;
    let empty = Condition::any()
        .add(content::Column::Topics.eq("[]"))
        .add(content::Column::Topics.eq(""))
        .add(content::Column::Topics.is_null());
    let rows = match content::Entity::find().filter(empty).all(db).await {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!("回填话题:读 content 失败: {e}");
            return;
        }
    };
    let mut fixed = 0u64;
    for row in rows {
        // 抖音无独立标题,正文在 desc;其他平台标题/正文都可能含话题,一并提取
        let mut text = String::new();
        if let Some(title) = &row.title {
            text.push_str(title);
            text.push(' ');
        }
        if let Some(desc) = &row.desc {
            text.push_str(desc);
        }
        let topics = crate::adapter::extract_hashtags(&text);
        if topics.is_empty() {
            continue;
        }
        let Ok(json) = serde_json::to_string(&topics) else {
            continue;
        };
        let mut am: content::ActiveModel = row.into();
        am.topics = Set(json);
        if let Err(e) = am.update(db).await {
            tracing::warn!("回填话题:更新失败: {e}");
            continue;
        }
        fixed += 1;
    }
    if fixed > 0 {
        tracing::info!("回填话题:从正文补提取 {fixed} 条内容的话题");
    }
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
            is_blacklisted: Set(false),
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
    /// 采集该内容时命中的关键词(从所属内容关联取;内容已删则为空)
    pub keyword: String,
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
            keyword: String::new(), // 由 list_comments 关联所属内容后填充
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
                // 抖音/快手无独立标题(正文在 desc),title 缺失时回退 desc 截断,避免「所属内容标题」为空
                view.content_title = c
                    .title
                    .clone()
                    .filter(|s| !s.trim().is_empty())
                    .or_else(|| {
                        c.desc.as_deref().filter(|s| !s.trim().is_empty()).map(|d| {
                            let head: String = d.chars().take(60).collect();
                            if d.chars().count() > 60 {
                                format!("{head}…")
                            } else {
                                head
                            }
                        })
                    });
                view.keyword = c.keyword.clone();
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

