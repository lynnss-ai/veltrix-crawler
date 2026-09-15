//! 采集任务 CRUD 命令。任务归属用户(owner)采用当前登录用户。
//!
//! keywords 在数据库以 JSON 字符串存储,前后端按 Vec<String> 序列化;
//! trigger/status/sortMode/timeRange 等枚举以字符串透传,值校验前端约束。

use crate::commands::{current_user, AppState};
use chrono::Utc;
use sea_orm::sea_query::{Expr, ExprTrait};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, Condition, ConnectionTrait, EntityTrait, IntoActiveModel,
    PaginatorTrait, QueryFilter, QueryOrder, QuerySelect, Set, Statement,
};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use tauri::State;
use veltrix_core::db::entity::{account, collect_log, comment, content, task, task_run};
use veltrix_core::error::{CrawlerError, Result};

/// 单任务关键词数量上限:关键词越多单轮采集越久、同账号风控压力越集中,
/// 前后端同口径限制(run_task 对存量超限任务同样拦截)。
pub const MAX_TASK_KEYWORDS: usize = 10;

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
    /// 指定采集账号(accounts.id);None = 按「最久未用」自动轮换
    pub account_id: Option<String>,
    pub keywords: Vec<String>,
    /// 定向采集目标链接(视频链接 / 主页链接);空数组 = 关键词搜索任务
    pub target_urls: Vec<String>,
    /// once-now / daily / watching
    pub trigger: String,
    pub scheduled_at: Option<String>,
    pub watch_interval_min: Option<i32>,
    pub sort_mode: String,
    pub time_range: String,
    pub per_keyword_limit: i32,
    pub min_likes: i32,
    /// 音频提取开关(视频转 mp3 留存)
    pub audio_extract: bool,
    /// 保留视频文件开关(视频落盘供自动发布;与音频提取独立)
    pub keep_video: bool,
    /// AI 文案提取开关(依赖音频提取)
    pub ai_extract: bool,
    /// 封面文字识别开关(采集后对封面图做 OCR)
    pub cover_ocr: bool,
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
    /// 平台专属额外筛选(抖音:视频时长 / 搜索范围 / 内容形式),对象 {维度id: 选中文案};{} = 全不限
    pub extra_filters: serde_json::Value,
    /// 失败自动重试次数上限(0=不自动重试)
    pub max_retries: i32,
    /// 当前失败序列已自动重试的次数
    pub retry_count: i32,
    /// 下次自动重试时间(unix 秒);None=未排期
    pub next_retry_at: Option<i64>,
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
        // 定向采集目标链接;老数据无此列值时迁移已回填 '[]',解析失败同样回退空数组
        let target_urls: Vec<String> = serde_json::from_str(&m.target_urls).unwrap_or_default();
        Self {
            id: m.id,
            name: m.name,
            industry: m.industry,
            platform: m.platform,
            account_id: m.account_id,
            keywords,
            target_urls,
            trigger: m.trigger_type,
            scheduled_at: m.scheduled_at,
            watch_interval_min: m.watch_interval_min,
            sort_mode: m.sort_mode,
            time_range: m.time_range,
            per_keyword_limit: m.per_keyword_limit,
            min_likes: m.min_likes,
            audio_extract: m.audio_extract,
            keep_video: m.keep_video,
            ai_extract: m.ai_extract,
            cover_ocr: m.cover_ocr,
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
            extra_filters: serde_json::from_str(&m.extra_filters)
                .unwrap_or_else(|_| serde_json::json!({})),
            max_retries: m.max_retries,
            retry_count: m.retry_count,
            next_retry_at: m.next_retry_at,
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
    /// 指定采集账号(accounts.id);缺省/空 = 按「最久未用」自动轮换
    #[serde(default)]
    pub account_id: Option<String>,
    pub keywords: Vec<String>,
    /// 定向采集目标链接(前端可能不传,默认空数组 = 关键词搜索任务)
    #[serde(default)]
    pub target_urls: Vec<String>,
    pub trigger: String,
    pub scheduled_at: Option<String>,
    pub watch_interval_min: Option<i32>,
    pub sort_mode: String,
    pub time_range: String,
    pub per_keyword_limit: i32,
    pub min_likes: i32,
    /// 音频提取开关(前端可能不传,默认关闭)
    #[serde(default)]
    pub audio_extract: bool,
    /// 保留视频文件开关(前端可能不传,默认关闭)
    #[serde(default)]
    pub keep_video: bool,
    pub ai_extract: bool,
    /// 封面文字识别开关(前端可能不传,默认关闭)
    #[serde(default)]
    pub cover_ocr: bool,
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
    /// 平台专属额外筛选(对象 {维度id: 选中文案});缺省 / 非对象归一化为空 {}
    #[serde(default)]
    pub extra_filters: serde_json::Value,
    /// 失败自动重试次数上限(0=不自动重试;缺省视为 0)
    #[serde(default)]
    pub max_retries: i32,
}

/// 单次最多返回 N 行,防止前端 IPC 被几万行数据噎住。
/// 全量库/评论库已改走分页接口(list_contents_page / list_comments_page),
/// 此上限仍约束任务列表 / 详情评论等未分页命令。
const LIST_HARD_CAP: u64 = 10000;

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
    //
    // 仅对活跃任务(query 时前端正在轮询进度的)查询 per-keyword 统计;
    // 已完成/失败任务的 keyword_stats 是静态数据,跳过查询直接给空。
    // 累计总量(total_contents/total_comments)直接用任务行的 content_count/comment_count,
    // 不再扫描全量 content+comment 表聚合(此前每次轮询都无条件 all(db) 全扫,库到十万行级后
    // 每次 IPC 搬运数十 MB 数据)。
    let active_ids: HashSet<String> = rows
        .iter()
        .filter(|m| {
            matches!(
                m.status.as_str(),
                "running"
                    | "collecting_comments"
                    | "analyzing_comments"
                    | "downloading_media"
                    | "paused"
            )
        })
        .map(|m| m.id.clone())
        .collect();
    let task_started: HashMap<String, i64> = rows
        .iter()
        .filter(|m| active_ids.contains(&m.id))
        .filter_map(|m| m.started_at.map(|s| (m.id.clone(), s)))
        .collect();
    let stats = keyword_stats_for_tasks(&state.db, &task_started).await;

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
            // 累计总量直接用任务行字段(采集时增量维护),不再全表扫描 content/comment 聚合
            view.total_contents = view.content_count;
            view.total_comments = view.comment_count;
            view
        })
        .collect())
}

/// 仅对活跃任务聚合 per-keyword 统计。content 表自带 keyword 直接 GROUP BY 计数;
/// comment 表无 keyword,内连接 contents 归到关键词。
/// 仅统计 collected_at >= started_at 的行(collected_at 是实体插入时的 Unix 秒,与任务 started_at 同源)。
/// 聚合下沉到数据库 GROUP BY:此前全量载入 content/comment 行再在 Rust 侧聚合,
/// 活跃任务累积到十万行级后每次轮询搬运数十 MB 数据(IPC 与内存双高)。
/// 查询失败按空处理,不阻断任务列表。
async fn keyword_stats_for_tasks(
    db: &sea_orm::DatabaseConnection,
    task_started: &HashMap<String, i64>,
) -> HashMap<String, HashMap<String, (i64, i64)>> {
    let mut result: HashMap<String, HashMap<String, (i64, i64)>> = HashMap::new();

    for (task_id, started) in task_started {
        let mut by_keyword: HashMap<String, (i64, i64)> = HashMap::new();

        // 内容:按 keyword 分组计数
        let content_rows: Vec<(String, i64)> = content::Entity::find()
            .select_only()
            .column(content::Column::Keyword)
            .column_as(content::Column::Id.count(), "cnt")
            .filter(content::Column::TaskId.eq(task_id.clone()))
            .filter(content::Column::CollectedAt.gte(*started))
            .group_by(content::Column::Keyword)
            .into_tuple()
            .all(db)
            .await
            .unwrap_or_default();
        for (keyword, cnt) in content_rows {
            by_keyword.entry(keyword).or_insert((0, 0)).0 = cnt;
        }

        // 评论:内连接 contents 按 (task_id, platform, content_id) 归到关键词。
        // 内容主键是 {task_id}-{platform}-{content_id},同任务内 (platform, content_id) 唯一,不会翻倍;
        // 内连接等价旧实现「content 映射里找不到的评论不计数」。占位符由 raw_statement 按后端改写(参数化防注入)。
        let sql = "SELECT ct.keyword AS kw, COUNT(*) AS cnt FROM comments cm \
                   JOIN contents ct ON ct.task_id = cm.task_id AND ct.platform = cm.platform \
                   AND ct.content_id = cm.content_id \
                   WHERE cm.task_id = ? AND cm.collected_at >= ? \
                   GROUP BY ct.keyword";
        let comment_rows = db
            .query_all(raw_statement(
                db.get_database_backend(),
                sql.to_owned(),
                vec![task_id.clone().into(), (*started).into()],
            ))
            .await
            .unwrap_or_default();
        for row in comment_rows {
            let keyword: String = row.try_get("", "kw").unwrap_or_default();
            let cnt: i64 = row.try_get("", "cnt").unwrap_or(0);
            if !keyword.is_empty() {
                by_keyword.entry(keyword).or_insert((0, 0)).1 = cnt;
            }
        }

        result.insert(task_id.clone(), by_keyword);
    }

    result
}

#[tauri::command]
pub async fn upsert_task(state: State<'_, AppState>, input: TaskInput) -> Result<()> {
    let db = &state.db;
    let now = Utc::now().timestamp();
    let me = current_user(&state).ok_or_else(|| CrawlerError::Config("未登录".into()))?;
    let owner = me.name.clone();
    // 关键词数量上限(定向任务 keywords 只有占位词,天然不触发);前端表单同口径拦截
    if input.keywords.len() > MAX_TASK_KEYWORDS {
        return Err(CrawlerError::Config(format!(
            "关键词最多 {MAX_TASK_KEYWORDS} 个,当前 {} 个",
            input.keywords.len()
        )));
    }
    let keywords_json = serde_json::to_string(&input.keywords)
        .map_err(|e| CrawlerError::Config(format!("序列化关键词失败: {e}")))?;
    let target_urls_json = serde_json::to_string(&input.target_urls)
        .map_err(|e| CrawlerError::Config(format!("序列化定向目标失败: {e}")))?;

    let existing = task::Entity::find_by_id(input.id.clone())
        .one(db)
        .await
        .map_err(|e| CrawlerError::Config(format!("查询任务失败: {e}")))?;
    // 额外筛选:仅接受对象,其余(Null / 数组等)归一化为空 {} 落库
    let extra_filters_json = if input.extra_filters.is_object() {
        input.extra_filters.to_string()
    } else {
        "{}".to_string()
    };
    // AI 文案提取依赖音频提取:开文案提取时强制带上音频提取(防御前端绕过联动)
    let audio_extract = input.audio_extract || input.ai_extract;
    // 自动重试上限夹在 0~10,避免误填超大值把调度器拖进无限重试循环
    let max_retries = input.max_retries.clamp(0, 10);
    // 指定采集账号:空串归一化为 None(自动轮换);指定时校验存在且与任务平台匹配,
    // 状态是否在运行时再查(账号可能在运行前重新登录恢复可用)
    let account_id = input
        .account_id
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    if let Some(aid) = &account_id {
        let acc = account::Entity::find_by_id(aid.clone())
            .one(db)
            .await
            .map_err(|e| CrawlerError::Config(format!("查询账号失败: {e}")))?
            .ok_or_else(|| CrawlerError::Config("指定账号不存在(可能已被删除)".into()))?;
        if acc.platform != input.platform {
            return Err(CrawlerError::Config(format!(
                "指定账号属于平台 {},与任务平台 {} 不匹配",
                acc.platform, input.platform
            )));
        }
    }
    match existing {
        Some(model) => {
            // self scope 用户只能改自己的任务(与 set_author_monitored_by_id 检查口径一致)
            if me.scope == "self" && model.owner != me.name {
                return Err(CrawlerError::Config("无权修改该任务".into()));
            }
            let mut am = model.into_active_model();
            am.name = Set(input.name);
            am.industry = Set(input.industry);
            am.platform = Set(input.platform);
            am.account_id = Set(account_id);
            am.keywords = Set(keywords_json);
            am.target_urls = Set(target_urls_json);
            am.trigger_type = Set(input.trigger);
            am.scheduled_at = Set(input.scheduled_at);
            am.watch_interval_min = Set(input.watch_interval_min);
            am.sort_mode = Set(input.sort_mode);
            am.time_range = Set(input.time_range);
            am.per_keyword_limit = Set(input.per_keyword_limit);
            am.min_likes = Set(input.min_likes);
            am.audio_extract = Set(audio_extract);
            am.keep_video = Set(input.keep_video);
            am.ai_extract = Set(input.ai_extract);
            am.cover_ocr = Set(input.cover_ocr);
            am.collect_comments = Set(input.collect_comments);
            am.comment_time_range = Set(input.comment_time_range);
            am.comment_limit = Set(input.comment_limit);
            am.analyze_comment_intent = Set(input.analyze_comment_intent);
            am.auto_sync_obsidian = Set(input.auto_sync_obsidian);
            am.extra_filters = Set(extra_filters_json);
            am.max_retries = Set(max_retries);
            // 关闭自动重试时顺带清掉已排期的重试(避免改配置后调度器仍拉起)
            if max_retries == 0 {
                am.next_retry_at = Set(None);
            }
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
                account_id: Set(account_id),
                keywords: Set(keywords_json),
                target_urls: Set(target_urls_json),
                trigger_type: Set(input.trigger),
                scheduled_at: Set(input.scheduled_at),
                watch_interval_min: Set(input.watch_interval_min),
                sort_mode: Set(input.sort_mode),
                time_range: Set(input.time_range),
                per_keyword_limit: Set(input.per_keyword_limit),
                min_likes: Set(input.min_likes),
                audio_extract: Set(audio_extract),
                keep_video: Set(input.keep_video),
                ai_extract: Set(input.ai_extract),
                cover_ocr: Set(input.cover_ocr),
                collect_comments: Set(input.collect_comments),
                comment_time_range: Set(input.comment_time_range),
                comment_limit: Set(input.comment_limit),
                analyze_comment_intent: Set(input.analyze_comment_intent),
                auto_sync_obsidian: Set(input.auto_sync_obsidian),
                extra_filters: Set(extra_filters_json),
                max_retries: Set(max_retries),
                retry_count: Set(0),
                next_retry_at: Set(None),
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
pub async fn update_task_status(state: State<'_, AppState>, patch: TaskStatusPatch) -> Result<()> {
    // 白名单校验:仅允许合法状态,防止前端 bug / 恶意调用写入不存在状态导致调度器误判
    const VALID_STATUSES: &[&str] = &[
        "pending",
        "running",
        "paused",
        "collecting_comments",
        "analyzing_comments",
        "downloading_media",
        "completed",
        "failed",
        "cancelled",
    ];
    if !VALID_STATUSES.contains(&patch.status.as_str()) {
        return Err(CrawlerError::Config(format!(
            "非法的任务状态: {}",
            patch.status
        )));
    }
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
/// 删除任务:仅删除任务行,contents/comments/logs 成为孤儿数据。
/// 全量库按 task_id 穿透仍能看见内容但行业关联为空。
/// 需要完整清理可先调 remove_contents 再删任务,或在 DB 层直接 DELETE CASCADE。
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
    /// 所属行业:content 表无此列,fill_content_views 关联 task.industry 填入
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
    /// 与图片 URL 按下标对应,详情接口回填已下载的文件。
    pub image_paths: Vec<Option<String>>,
    /// 小红书原文导航参数,不暴露整份 extra。
    pub xsec_token: Option<String>,
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
    /// 视频语音转写文本(转写成功后回写),前端展示;
    /// 空串=已转写但未识别到语音(空文案标记),None=未转写/转写失败
    pub transcript: Option<String>,
    /// 转写失败原因(供前端区分未转写与失败)
    pub transcript_error: Option<String>,
    /// 封面 OCR 识别文本;空串=已识别但无文字,None=未识别/识别失败
    pub cover_ocr_text: Option<String>,
    /// 封面 OCR 失败原因(供前端区分未识别与失败)
    pub cover_ocr_error: Option<String>,
    /// 细粒度处理状态:视频下载 / 图文图片进度 / 评论采集 / 意向分析
    pub video_downloaded: Option<bool>,
    pub image_total: Option<i32>,
    pub image_done: Option<i32>,
    pub comment_collected: Option<bool>,
    pub intent_analyzed: Option<bool>,
    /// 当前登录用户是否已把该内容同步到自己的 Obsidian(fill_content_views 按当前用户回填)
    pub synced_by_me: bool,
}

impl From<content::Model> for ContentView {
    fn from(m: content::Model) -> Self {
        // image_urls / topics 反序列化失败回退空数组,避免一条脏数据拖死整表
        let image_urls: Vec<String> = serde_json::from_str(&m.image_urls).unwrap_or_default();
        let topics: Vec<String> = serde_json::from_str(&m.topics).unwrap_or_default();
        let xsec_token = serde_json::from_str::<serde_json::Value>(&m.extra)
            .ok()
            .and_then(|v| {
                v.get("xsec_token")
                    .and_then(|t| t.as_str())
                    .filter(|t| !t.trim().is_empty())
                    .map(str::to_string)
            });
        // 头像在完整作者 JSON 里(实体只单列了 uid/nickname),按需解析出来
        let author_avatar = serde_json::from_str::<serde_json::Value>(&m.author_json)
            .ok()
            .and_then(|v| v.get("avatar").and_then(|a| a.as_str()).map(str::to_string));
        Self {
            id: m.id,
            task_id: m.task_id,
            platform: m.platform,
            industry: String::new(), // 由 fill_content_views 关联 task 后填充
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
            image_paths: m
                .image_paths
                .as_deref()
                .and_then(|text| serde_json::from_str(text).ok())
                .unwrap_or_default(),
            xsec_token,
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
            cover_ocr_text: m.cover_ocr_text,
            cover_ocr_error: m.cover_ocr_error,
            video_downloaded: m.video_downloaded,
            image_total: m.image_total,
            image_done: m.image_done,
            comment_collected: m.comment_collected,
            intent_analyzed: m.intent_analyzed,
            synced_by_me: false, // 由 fill_content_views 按当前用户回填
        }
    }
}

/// 列表专用瘦身视图(list_contents_page 专用):剔除 transcript / cover_ocr_text 全文、
/// image_urls 全量数组、image_paths 等大块字段——瀑布流每批 48 条、表格单页可达 1000 条,
/// 整文过 IPC 是大库列表卡顿的主因。文案与封面文字只保留三态 + 摘要;
/// 全文仍由详情(get_content_detail)/ 导出(list_contents_full)等接口提供。
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ContentListView {
    pub id: String,
    pub task_id: String,
    pub platform: String,
    /// 所属行业:content 表无此列,fill_content_list_views 关联 task.industry 填入
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
    /// 作者头像 URL(Rust 侧从 author_json 解析,author_json 本身不外发)
    pub author_avatar: Option<String>,
    pub like_count: Option<i64>,
    pub comment_count: Option<i64>,
    pub collect_count: Option<i64>,
    pub share_count: Option<i64>,
    pub play_count: Option<i64>,
    pub published_at: Option<i64>,
    pub video_url: Option<String>,
    pub cover_url: Option<String>,
    /// 图集首图 URL(封面缺失时的回退图源;替代旧 image_urls[0] 用法)
    pub first_image_url: Option<String>,
    /// 图集图片张数(瀑布流「N 图」角标等;替代旧 image_urls.length 用法)
    pub image_count: i32,
    /// 小红书原文导航参数(Rust 侧从 extra 解析,不暴露整份 extra)
    pub xsec_token: Option<String>,
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
    /// 封面本地路径(相对 media_root;旧数据可能为绝对路径),前端本地优先显示
    pub cover_path: Option<String>,
    /// 作者头像本地路径
    pub avatar_path: Option<String>,
    /// 视频转出音频本地路径;None=非视频/未提取/旧数据未记录
    pub audio_path: Option<String>,
    /// 转写三态:none=未转写/转写失败,empty=已转写但未识别到语音(空文案),has=有文案;
    /// 列表不再传 transcript 全文,徽章逻辑改读此字段,语义与转写三态约定一一对应
    pub transcript_state: String,
    /// 文案摘要:仅 has 时有值,超出 LIST_PREVIEW_LEN 字符截断并补 …
    pub transcript_preview: Option<String>,
    /// 转写失败原因(供前端区分未转写与失败)
    pub transcript_error: Option<String>,
    /// 封面 OCR 三态(口径同 transcript_state)
    pub cover_ocr_state: String,
    /// 封面文字摘要:仅 has 时有值,截断口径同 transcript_preview
    pub cover_ocr_preview: Option<String>,
    /// 封面 OCR 失败原因
    pub cover_ocr_error: Option<String>,
    /// 细粒度处理状态:视频下载 / 图文图片进度 / 评论采集 / 意向分析
    pub video_downloaded: Option<bool>,
    pub image_total: Option<i32>,
    pub image_done: Option<i32>,
    pub comment_collected: Option<bool>,
    pub intent_analyzed: Option<bool>,
    /// 当前登录用户是否已把该内容同步到自己的 Obsidian(fill_content_list_views 回填)
    pub synced_by_me: bool,
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
    let top_i64 = |key: &str| {
        av.as_ref()
            .and_then(|v| v.get(key))
            .and_then(|x| x.as_i64())
    };
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

    // 同 owner + platform + author_uid 的内容统计:视频数走 COUNT,时间/评论关联只取
    // content_id + 两个时间列——不把整行(含转写全文 transcript / author_json)拉回内存,
    // 作者作品多时详情打开明显卡(曾整行全取再内存聚合)。
    let author_scoped = || {
        content::Entity::find()
            .filter(content::Column::Owner.eq(row.owner.clone()))
            .filter(content::Column::Platform.eq(row.platform.clone()))
            .filter(content::Column::AuthorUid.eq(row.author_uid.clone()))
    };
    let video_count = author_scoped()
        .filter(content::Column::Kind.eq("video"))
        .count(&state.db)
        .await
        .unwrap_or(0) as i64;
    let light_rows: Vec<(String, i64, Option<i64>)> = author_scoped()
        .select_only()
        .column(content::Column::ContentId)
        .column(content::Column::CollectedAt)
        .column(content::Column::PublishedAt)
        .into_tuple()
        .all(&state.db)
        .await
        .unwrap_or_default();
    let content_ids: Vec<String> = light_rows.iter().map(|r| r.0.clone()).collect();
    let first_collected_at = light_rows.iter().map(|r| r.1).min();
    let last_published_at = light_rows.iter().filter_map(|r| r.2).max();
    let last_collected_at = light_rows.iter().map(|r| r.1).max();

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
        avatar: ar
            .and_then(|a| a.avatar.clone())
            .or_else(|| top_str("avatar")),
        avatar_path: row.avatar_path.clone(),
        platform_id: ar
            .and_then(|a| a.platform_id.clone())
            .or_else(|| extra_str("unique_id")),
        short_id: ar
            .and_then(|a| a.short_id.clone())
            .or_else(|| extra_str("uid")),
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

    // 下载器将图集与封面放在同目录。用实际封面定位,兼容旧数据及跨日下载。
    content_view
        .image_paths
        .resize(content_view.image_urls.len(), None);
    // 库存 cover_path 可能是相对 media_root 的相对路径(新口径),先 resolve 成绝对路径做磁盘判断;
    // 写回 view 的条目再经 to_media_rel 转回相对路径,保持视图输出口径一致
    let root = crate::media::media_root(
        &state.config_dir,
        &crate::commands::lock_config(&state)?.media.clone(),
    );
    let cover_abs = content_view
        .cover_path
        .as_deref()
        .map(|p| crate::media::resolve_media_path(&root, p));
    if let Some(cover) = cover_abs.as_deref() {
        if let (Some(dir), Some(prefix)) = (
            cover.parent(),
            cover
                .file_stem()
                .and_then(|s| s.to_str())
                .and_then(|s| s.strip_suffix("_cover")),
        ) {
            for index in 0..content_view.image_urls.len() {
                if content_view.image_paths[index].is_some() {
                    continue;
                }
                let path = dir.join(format!("{prefix}_img{index}.jpg"));
                let exists = tokio::fs::metadata(&path)
                    .await
                    .is_ok_and(|meta| meta.is_file() && meta.len() > 0);
                content_view.image_paths[index] =
                    exists.then(|| crate::media::to_media_rel(&root, &path));
            }
        }
    }

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
    /// 作者头像本地路径(相对 media_root;按 media 模块落盘约定探测文件存在性,未下载过为 None)
    pub avatar_path: Option<String>,
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
    let mut q = author_entity::Entity::find().order_by_desc(author_entity::Column::LastCollectedAt);
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
    let task_ids: std::collections::HashSet<String> = author_tasks
        .iter()
        .map(|(_, _, _, tid)| tid.clone())
        .collect();
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

    // 头像本地路径:与 media 模块头像落盘约定一致({platform}/avatar/{uid}.jpg),
    // 文件存在才回填,前端本地优先、缺失回退平台 CDN
    let media_root = state
        .config
        .lock()
        .map(|cfg| crate::media::media_root(&state.config_dir, &cfg.media))
        .ok();

    Ok(rows
        .into_iter()
        .map(|m| {
            let avatar_rel = format!(
                "{}/{}/{}.jpg",
                m.platform,
                crate::media::avatar_dir_name(),
                crate::media::sanitize_filename(&m.uid)
            );
            let avatar_path = media_root
                .as_ref()
                .filter(|root| root.join(&avatar_rel).is_file())
                .map(|_| avatar_rel);
            AuthorView {
                avatar_path,
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
            }
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
        let top_i64 = |key: &str| {
            av.as_ref()
                .and_then(|v| v.get(key))
                .and_then(|x| x.as_i64())
        };
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
        let top_i64 = |key: &str| {
            av.as_ref()
                .and_then(|v| v.get(key))
                .and_then(|x| x.as_i64())
        };
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

/// 删除一条采集内容(全量库 / 内容库的「删除」操作)。仅删库记录,媒体文件不动;
/// 级联删除该内容的评论,避免评论库留下无关联的孤儿数据。
#[tauri::command]
pub async fn remove_content(state: State<'_, AppState>, id: String) -> Result<()> {
    let me = current_user(&state).ok_or_else(|| CrawlerError::Config("未登录".into()))?;
    // 删除前取回行:权限校验 + 级联删评论需要 (task_id, platform, content_id) 关联键
    let Some(row) = content::Entity::find_by_id(&id)
        .one(&state.db)
        .await
        .map_err(|e| CrawlerError::Config(format!("查询内容失败: {e}")))?
    else {
        return Ok(()); // 不存在则幂等
    };
    if me.scope == "self" && row.owner != me.name {
        return Err(CrawlerError::Config("无权删除该内容".into()));
    }
    content::Entity::delete_by_id(&id)
        .exec(&state.db)
        .await
        .map_err(|e| CrawlerError::Config(format!("删除内容失败: {e}")))?;
    cascade_delete_comments(&state.db, &[(&row.task_id, &row.platform, &row.content_id)]).await?;
    Ok(())
}

/// 批量删除采集内容(全量库多选删除)。仅删库记录,媒体文件不动;级联删除这些内容的
/// 评论。dataScope=self 的用户只能删自己 owner 的内容(越权 id 静默跳过)。返回实际删除条数。
#[tauri::command]
pub async fn remove_contents(state: State<'_, AppState>, ids: Vec<String>) -> Result<u64> {
    if ids.is_empty() {
        return Ok(0);
    }
    let me = current_user(&state).ok_or_else(|| CrawlerError::Config("未登录".into()))?;
    // 先按同一权限口径取回待删行的关联键,供级联删评论
    let mut find = content::Entity::find().filter(content::Column::Id.is_in(ids.clone()));
    if me.scope == "self" {
        find = find.filter(content::Column::Owner.eq(me.name.clone()));
    }
    let rows = find
        .all(&state.db)
        .await
        .map_err(|e| CrawlerError::Config(format!("查询待删内容失败: {e}")))?;
    let mut q = content::Entity::delete_many().filter(content::Column::Id.is_in(ids));
    if me.scope == "self" {
        q = q.filter(content::Column::Owner.eq(me.name.clone()));
    }
    let res = q
        .exec(&state.db)
        .await
        .map_err(|e| CrawlerError::Config(format!("批量删除内容失败: {e}")))?;
    let keys: Vec<(&str, &str, &str)> = rows
        .iter()
        .map(|r| {
            (
                r.task_id.as_str(),
                r.platform.as_str(),
                r.content_id.as_str(),
            )
        })
        .collect();
    cascade_delete_comments(&state.db, &keys).await?;
    Ok(res.rows_affected)
}

/// 级联删除一批内容的评论:按 (task_id, platform, content_id) 三元组精确匹配,
/// 与评论落库 / fill_comment_views 的关联口径一致。
async fn cascade_delete_comments(
    db: &sea_orm::DatabaseConnection,
    keys: &[(&str, &str, &str)],
) -> Result<()> {
    if keys.is_empty() {
        return Ok(());
    }
    let mut cond = Condition::any();
    for (task_id, platform, content_id) in keys {
        cond = cond.add(
            Condition::all()
                .add(comment::Column::TaskId.eq(*task_id))
                .add(comment::Column::Platform.eq(*platform))
                .add(comment::Column::ContentId.eq(*content_id)),
        );
    }
    comment::Entity::delete_many()
        .filter(cond)
        .exec(db)
        .await
        .map_err(|e| CrawlerError::Config(format!("级联删除评论失败: {e}")))?;
    Ok(())
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
    /// 所属行业:comment 表无此列,fill_comment_views 关联 task.industry 填入
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
    /// 所属内容信息(fill_comment_views 关联 contents 填;内容已删则为 None)
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
            industry: String::new(), // 由 fill_comment_views 关联 task 后填充
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
            keyword: String::new(), // 由 fill_comment_views 关联所属内容后填充
        }
    }
}

// ===================== 分页列表查询(全量库 / 评论库)=====================
//
// list_contents / list_comments 此前一次返回最多 LIST_HARD_CAP(10000)行完整对象,
// 前端再全量过滤/排序:大库下 IPC 传输数百 MB、每键过滤卡顿。分页接口把筛选与排序
// 下沉 SQL、limit/offset + total 回传,前端三处消费方(内容库 / 评论库 / 内容选择弹窗)逐步迁移。

/// 全量库内容分页查询参数。ids 模式(批量视图)忽略 limit/offset,按 id 集直接取回(上限 10000)。
#[derive(Deserialize, Clone, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct ContentListQuery {
    /// 任务穿透:按任务过滤(旧任务内容不被分页截断)
    pub task_id: Option<String>,
    /// 任务穿透关键词精确匹配(contents.keyword 全等)
    pub keyword: Option<String>,
    /// 任务穿透单次运行窗口(collected_at 闭区间)
    pub run_start: Option<i64>,
    pub run_end: Option<i64>,
    /// 搜索:title / keyword / desc 子串(大小写不敏感)
    pub search: Option<String>,
    pub platform: Option<String>,
    /// 形态多选(video/image/article/unknown);空=不限
    pub kinds: Vec<String>,
    /// 行业筛选(经 tasks 逻辑外键)
    pub industry: Option<String>,
    /// 采集时间闭区间(Unix 秒)
    pub created_from: Option<i64>,
    pub created_to: Option<i64>,
    /// 发布时间闭区间(Unix 秒)
    pub published_from: Option<i64>,
    pub published_to: Option<i64>,
    /// 图源:image/cover 均附加「有封面」条件;形态限制由 kinds 表达
    pub image_source: Option<String>,
    /// 仅展示已转写文案的视频(内容库视频 tab 口径:转写未出的视频无浏览价值)
    pub require_transcript: Option<bool>,
    /// 内容库额外纳入小红书图文。图文正文来自笔记详情,不依赖语音转写。
    pub include_xhs_images: Option<bool>,
    /// 排序字段白名单:collectedAt / publishedAt / mediaStatus;None=collectedAt
    pub sort_by: Option<String>,
    /// asc / desc;None=desc
    pub sort_dir: Option<String>,
    pub limit: u64,
    pub offset: u64,
    /// 批量视图:按 id 集直接取回(不分页)
    pub ids: Option<Vec<String>>,
}

/// 评论库分页查询参数。
#[derive(Deserialize, Clone, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct CommentListQuery {
    pub task_id: Option<String>,
    /// 搜索:text / author_nickname 子串(大小写不敏感)
    pub search: Option<String>,
    pub platform: Option<String>,
    /// 评论所属内容形态(EXISTS contents 相关子查询)
    pub kinds: Vec<String>,
    /// 行业筛选(经 tasks 逻辑外键)
    pub industry: Option<String>,
    /// 意向多选;unanalyzed = intent_level IS NULL
    pub intent_levels: Vec<String>,
    /// 评论发表时间闭区间(Unix 秒)
    pub created_from: Option<i64>,
    pub created_to: Option<i64>,
    /// 排序字段白名单:collectedAt / createdAt / likeCount / intent;None=collectedAt
    pub sort_by: Option<String>,
    /// asc / desc;None=desc
    pub sort_dir: Option<String>,
    pub limit: u64,
    pub offset: u64,
}

/// 分页列表返回包:条目 + 同筛选口径的总数(前端 hasMore / 页码推导用)。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContentListResult {
    pub items: Vec<ContentListView>,
    pub total: i64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CommentListResult {
    pub items: Vec<CommentView>,
    pub total: i64,
}

/// 全量库「待转写 / 待提取评论 / 待采集音频」计数(与前端 needsTranscript / needsComments 逐条口径一致)。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContentLibraryStats {
    pub untranscribed: i64,
    pub pending_comment: i64,
    /// 音频采集失败/缺失的视频数(素材失败且无音频文件),对应「采集音频」批量按钮
    pub missing_audio: i64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IndustryCount {
    pub industry: String,
    pub count: i64,
}

/// 侧栏行业角标聚合结果:各行业计数 + 「全部」总数。
/// total 忽略行业筛选(含无行业内容),前端「全部行业」角标用它——
/// 此前用列表 total(含行业过滤),选中某行业后「全部」角标会变成该行业的数量。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IndustryCounts {
    pub total: i64,
    pub industries: Vec<IndustryCount>,
}

/// 批量处理种类(对应前端「提取文案 / 提取评论 / 采集音频」按钮)
#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BatchKind {
    Transcript,
    Comments,
    Audio,
}

/// WHERE 片段 + 占位符参数(统一用 ? 书写,执行前由 raw_statement 按后端改写)
struct FilterParts {
    conds: String,
    values: Vec<sea_orm::Value>,
}

/// 构造手写 SQL 的 Statement。SeaORM 对原始 SQL 不做占位符转换,而 sqlx 的
/// PostgreSQL 驱动只认 `$N`(不认 `?`),故 PG 下把 `?` 顺次改写为 `$1..$N`;
/// SQLite/MySQL 原样保留。约定:手写 SQL 中 `?` 只作占位符,含 `?` 的文本一律走绑定参数。
fn raw_statement(
    backend: sea_orm::DatabaseBackend,
    sql: String,
    values: Vec<sea_orm::Value>,
) -> Statement {
    let sql = match backend {
        sea_orm::DatabaseBackend::Postgres => {
            let mut out = String::with_capacity(sql.len() + 8);
            let mut idx = 0usize;
            for ch in sql.chars() {
                if ch == '?' {
                    idx += 1;
                    out.push('$');
                    out.push_str(&idx.to_string());
                } else {
                    out.push(ch);
                }
            }
            out
        }
        _ => sql,
    };
    Statement::from_sql_and_values(backend, sql, values)
}

/// 追加一条 AND 条件(值为占位符参数)
fn and_cond(conds: &mut String, values: &mut Vec<sea_orm::Value>, frag: &str, v: sea_orm::Value) {
    conds.push_str(" AND ");
    conds.push_str(frag);
    values.push(v);
}

/// LIKE 通配符转义:前端搜索是无通配子串匹配(JS includes),转义 % _ \ 后语义等价
fn escape_like(q: &str) -> String {
    let mut escaped = String::with_capacity(q.len() + 16);
    for ch in q.chars() {
        if ch == '\\' || ch == '%' || ch == '_' {
            escaped.push('\\');
        }
        escaped.push(ch);
    }
    escaped
}

/// 内容列表通用过滤(分页查询 / 计数 / 统计 / 批量取 id 共用同一口径)
fn content_filter(query: &ContentListQuery, self_only: bool, owner: &str) -> FilterParts {
    let mut conds = String::new();
    let mut values: Vec<sea_orm::Value> = Vec::new();
    if self_only {
        and_cond(
            &mut conds,
            &mut values,
            "contents.owner = ?",
            owner.to_string().into(),
        );
    }
    if let Some(tid) = query.task_id.as_deref().filter(|t| !t.is_empty()) {
        and_cond(
            &mut conds,
            &mut values,
            "contents.task_id = ?",
            tid.to_string().into(),
        );
    }
    if let Some(kw) = query.keyword.as_deref().filter(|k| !k.is_empty()) {
        and_cond(
            &mut conds,
            &mut values,
            "contents.keyword = ?",
            kw.to_string().into(),
        );
    }
    if let Some(start) = query.run_start {
        and_cond(
            &mut conds,
            &mut values,
            "contents.collected_at >= ?",
            start.into(),
        );
    }
    if let Some(end) = query.run_end {
        and_cond(
            &mut conds,
            &mut values,
            "contents.collected_at <= ?",
            end.into(),
        );
    }
    if let Some(q) = query
        .search
        .as_deref()
        .map(str::trim)
        .filter(|q| !q.is_empty())
    {
        let pattern = format!("%{}%", escape_like(q));
        // 三个命中列共用同一模式(前端 title / keyword / desc 任一 includes 即命中)
        and_cond(
            &mut conds,
            &mut values,
            "(lower(contents.title) LIKE lower(?) ESCAPE '\\' OR lower(contents.keyword) LIKE lower(?) ESCAPE '\\' OR lower(contents.desc) LIKE lower(?) ESCAPE '\\')",
            pattern.clone().into(),
        );
        values.push(pattern.clone().into());
        values.push(pattern.into());
    }
    if let Some(p) = query.platform.as_deref().filter(|p| !p.is_empty()) {
        and_cond(
            &mut conds,
            &mut values,
            "contents.platform = ?",
            p.to_string().into(),
        );
    }
    if !query.kinds.is_empty() {
        let placeholders = vec!["?"; query.kinds.len()].join(", ");
        if query.include_xhs_images.unwrap_or(false) {
            conds.push_str(&format!(
                " AND (contents.kind IN ({placeholders}) OR (contents.platform = 'xhs' AND contents.kind = 'image'))"
            ));
        } else {
            conds.push_str(&format!(" AND contents.kind IN ({placeholders})"));
        }
        for k in &query.kinds {
            values.push(k.clone().into());
        }
    }
    if let Some(ind) = query
        .industry
        .as_deref()
        .filter(|i| !i.is_empty() && *i != "__all")
    {
        and_cond(
            &mut conds,
            &mut values,
            "contents.task_id IN (SELECT id FROM tasks WHERE industry = ?)",
            ind.to_string().into(),
        );
    }
    if let Some(from) = query.created_from {
        and_cond(
            &mut conds,
            &mut values,
            "contents.collected_at >= ?",
            from.into(),
        );
    }
    if let Some(to) = query.created_to {
        and_cond(
            &mut conds,
            &mut values,
            "contents.collected_at <= ?",
            to.into(),
        );
    }
    if let Some(from) = query.published_from {
        and_cond(
            &mut conds,
            &mut values,
            "contents.published_at >= ?",
            from.into(),
        );
    }
    if let Some(to) = query.published_to {
        and_cond(
            &mut conds,
            &mut values,
            "contents.published_at <= ?",
            to.into(),
        );
    }
    match query.image_source.as_deref() {
        // "image":本地封面路径非空(图片素材定位口径,内容选择弹窗用——远程 URL 无法定位本地素材)
        Some("image") => {
            conds.push_str(" AND contents.cover_path IS NOT NULL AND contents.cover_path <> ''");
        }
        // "cover":本地或远程封面任一存在(内容库封面图源,远程封面也展示)
        Some("cover") => {
            conds.push_str(
                " AND ((contents.cover_path IS NOT NULL AND contents.cover_path <> '') OR (contents.cover_url IS NOT NULL AND contents.cover_url <> ''))",
            );
        }
        _ => {}
    }
    if query.require_transcript.unwrap_or(false) {
        if query.include_xhs_images.unwrap_or(false) {
            // 视频仍要求转写完成;小红书图文正文直接来自详情,无需经过音频转写。
            conds.push_str(
                " AND ((contents.platform = 'xhs' AND contents.kind = 'image') OR \
                 (contents.kind = 'video' AND contents.transcript IS NOT NULL AND trim(contents.transcript) <> ''))",
            );
        } else {
            conds.push_str(
                " AND contents.transcript IS NOT NULL AND trim(contents.transcript) <> ''",
            );
        }
    }
    if let Some(ids) = query.ids.as_deref() {
        if ids.is_empty() {
            // 空 ids 集:批量为空,恒 false(避免 IN () 语法错误)
            conds.push_str(" AND 1 = 0");
        } else {
            let placeholders = vec!["?"; ids.len().min(10000)].join(", ");
            conds.push_str(&format!(" AND contents.id IN ({placeholders})"));
            for id in ids.iter().take(10000) {
                values.push(id.clone().into());
            }
        }
    }
    FilterParts { conds, values }
}

#[cfg(test)]
mod content_filter_tests {
    use super::{content_filter, ContentListQuery};

    #[test]
    fn content_library_includes_xhs_images_without_weakening_video_transcript_filter() {
        let query = ContentListQuery {
            kinds: vec!["video".to_string()],
            require_transcript: Some(true),
            include_xhs_images: Some(true),
            ..Default::default()
        };
        let filter = content_filter(&query, false, "");

        assert!(filter.conds.contains(
            "contents.kind IN (?) OR (contents.platform = 'xhs' AND contents.kind = 'image')"
        ));
        assert!(filter.conds.contains(
            "(contents.platform = 'xhs' AND contents.kind = 'image') OR (contents.kind = 'video' AND contents.transcript IS NOT NULL"
        ));
        assert_eq!(filter.values.len(), 1);
    }
}

/// 评论列表通用过滤(分页查询 / 计数 / 行业角标共用同一口径)
fn comment_filter(query: &CommentListQuery, self_only: bool, owner: &str) -> FilterParts {
    let mut conds = String::new();
    let mut values: Vec<sea_orm::Value> = Vec::new();
    if self_only {
        and_cond(
            &mut conds,
            &mut values,
            "comments.owner = ?",
            owner.to_string().into(),
        );
    }
    if let Some(tid) = query.task_id.as_deref().filter(|t| !t.is_empty()) {
        and_cond(
            &mut conds,
            &mut values,
            "comments.task_id = ?",
            tid.to_string().into(),
        );
    }
    if let Some(q) = query
        .search
        .as_deref()
        .map(str::trim)
        .filter(|q| !q.is_empty())
    {
        let pattern = format!("%{}%", escape_like(q));
        and_cond(
            &mut conds,
            &mut values,
            "(lower(comments.text) LIKE lower(?) ESCAPE '\\' OR lower(comments.author_nickname) LIKE lower(?) ESCAPE '\\')",
            pattern.clone().into(),
        );
        values.push(pattern.into());
    }
    if let Some(p) = query.platform.as_deref().filter(|p| !p.is_empty()) {
        and_cond(
            &mut conds,
            &mut values,
            "comments.platform = ?",
            p.to_string().into(),
        );
    }
    if !query.kinds.is_empty() {
        // 所属内容形态:相关 EXISTS(评论与内容按任务级三列关联,无物理外键)
        let placeholders = vec!["?"; query.kinds.len()].join(", ");
        conds.push_str(&format!(
            " AND EXISTS (SELECT 1 FROM contents ct WHERE ct.task_id = comments.task_id \
             AND ct.platform = comments.platform AND ct.content_id = comments.content_id \
             AND ct.kind IN ({placeholders}))"
        ));
        for k in &query.kinds {
            values.push(k.clone().into());
        }
    }
    if let Some(ind) = query
        .industry
        .as_deref()
        .filter(|i| !i.is_empty() && *i != "__all")
    {
        and_cond(
            &mut conds,
            &mut values,
            "comments.task_id IN (SELECT id FROM tasks WHERE industry = ?)",
            ind.to_string().into(),
        );
    }
    if !query.intent_levels.is_empty() {
        let analyzed: Vec<&String> = query
            .intent_levels
            .iter()
            .filter(|l| l.as_str() != "unanalyzed")
            .collect();
        let mut parts: Vec<String> = Vec::new();
        if !analyzed.is_empty() {
            let placeholders = vec!["?"; analyzed.len()].join(", ");
            parts.push(format!("comments.intent_level IN ({placeholders})"));
            for l in &analyzed {
                values.push((*l).clone().into());
            }
        }
        if analyzed.len() != query.intent_levels.len() {
            parts.push("comments.intent_level IS NULL".to_string());
        }
        conds.push_str(&format!(" AND ({})", parts.join(" OR ")));
    }
    if let Some(from) = query.created_from {
        and_cond(
            &mut conds,
            &mut values,
            "comments.created_at >= ?",
            from.into(),
        );
    }
    if let Some(to) = query.created_to {
        and_cond(
            &mut conds,
            &mut values,
            "comments.created_at <= ?",
            to.into(),
        );
    }
    FilterParts { conds, values }
}

/// 内容列表排序:白名单字段 + 方向;末尾恒定 id ASC 保证 offset 分页稳定(同值行不漂移)
fn content_order(query: &ContentListQuery) -> String {
    let col = match query.sort_by.as_deref() {
        Some("publishedAt") => "COALESCE(published_at, 0)",
        Some("mediaStatus") => "COALESCE(media_status, '')",
        _ => "collected_at",
    };
    let dir = match query.sort_dir.as_deref() {
        Some("asc") => "ASC",
        _ => "DESC",
    };
    format!("{col} {dir}, id ASC")
}

/// 评论列表排序:默认 collected_at DESC + 意向高→低 tiebreak(与前端 sorted 口径一致);
/// 意向等级 CASE 对所有排序列统一追加,列值相同时保持展示顺序稳定
fn comment_order(query: &CommentListQuery) -> String {
    let col = match query.sort_by.as_deref() {
        Some("createdAt") => "COALESCE(created_at, 0)",
        Some("likeCount") => "COALESCE(like_count, 0)",
        Some("intent") => {
            "CASE intent_level WHEN 'high' THEN 5 WHEN 'medium' THEN 4 WHEN 'low' THEN 3 WHEN 'none' THEN 2 ELSE 1 END"
        }
        _ => "collected_at",
    };
    let dir = match query.sort_dir.as_deref() {
        Some("asc") => "ASC",
        _ => "DESC",
    };
    format!(
        "{col} {dir}, CASE intent_level WHEN 'high' THEN 5 WHEN 'medium' THEN 4 WHEN 'low' THEN 3 WHEN 'none' THEN 2 ELSE 1 END DESC, id ASC"
    )
}

/// 列表摘要截断长度(字符):覆盖表格「文案片段」与瀑布流文字卡的展示量,全文看详情。
/// 前端事件/重试回写摘要时用同一阈值(ContentLibraryPage 的 toPreview)。
const LIST_PREVIEW_LEN: usize = 100;

/// 列表瘦身查询的 SELECT:剔 transcript / cover_ocr_text / image_paths 等整文列,
/// 三态与摘要在 SQL 内算好(substr/length 在 SQLite 与 PG 均按字符计),整文不出库也不过 IPC;
/// author_json / image_urls / extra 仍需读出供 Rust 派生小字段(头像/首图/xsec_token),但不外发。
fn content_list_select_sql(placeholders: &str) -> String {
    format!(
        "SELECT id, task_id, platform, content_id, keyword, kind, title, desc, \
         author_uid, author_nickname, author_json, \
         like_count, comment_count, collect_count, share_count, play_count, published_at, \
         video_url, cover_url, image_urls, extra, duration, topics, owner, collected_at, \
         media_status, audio_extracted, media_error, cover_path, avatar_path, audio_path, \
         transcript_error, cover_ocr_error, \
         video_downloaded, image_total, image_done, comment_collected, intent_analyzed, \
         CASE WHEN transcript IS NULL THEN 'none' WHEN transcript = '' THEN 'empty' ELSE 'has' END AS transcript_state, \
         CASE WHEN transcript IS NULL OR transcript = '' THEN NULL \
              WHEN length(transcript) > {n} THEN substr(transcript, 1, {n}) || '…' \
              ELSE transcript END AS transcript_preview, \
         CASE WHEN cover_ocr_text IS NULL THEN 'none' WHEN cover_ocr_text = '' THEN 'empty' ELSE 'has' END AS cover_ocr_state, \
         CASE WHEN cover_ocr_text IS NULL OR cover_ocr_text = '' THEN NULL \
              WHEN length(cover_ocr_text) > {n} THEN substr(cover_ocr_text, 1, {n}) || '…' \
              ELSE cover_ocr_text END AS cover_ocr_preview \
         FROM contents WHERE id IN ({placeholders})",
        n = LIST_PREVIEW_LEN,
    )
}

/// 单行手工映射:列与 content_list_select_sql 的清单一一对应;
/// author_json / image_urls / extra / topics 的 JSON 脏数据回退默认,避免一条脏数据拖死整页。
fn map_content_list_row(r: &sea_orm::QueryResult) -> Result<ContentListView> {
    let col_err = |col: &'static str| {
        move |e: sea_orm::DbErr| CrawlerError::Config(format!("读取内容列表列 {col} 失败: {e}"))
    };
    let author_json: String = r
        .try_get("", "author_json")
        .map_err(col_err("author_json"))?;
    let image_urls_raw: String = r.try_get("", "image_urls").map_err(col_err("image_urls"))?;
    let extra: String = r.try_get("", "extra").map_err(col_err("extra"))?;
    let topics_raw: String = r.try_get("", "topics").map_err(col_err("topics"))?;
    // 头像在完整作者 JSON 里(实体只单列了 uid/nickname),按需解析出来
    let author_avatar = serde_json::from_str::<serde_json::Value>(&author_json)
        .ok()
        .and_then(|v| v.get("avatar").and_then(|a| a.as_str()).map(str::to_string));
    // 图集只取首图(封面回退图源)与张数(「N 图」角标),整串不外发
    let image_urls: Vec<String> = serde_json::from_str(&image_urls_raw).unwrap_or_default();
    let first_image_url = image_urls.first().filter(|u| !u.is_empty()).cloned();
    let image_count = image_urls.len() as i32;
    let xsec_token = serde_json::from_str::<serde_json::Value>(&extra)
        .ok()
        .and_then(|v| {
            v.get("xsec_token")
                .and_then(|t| t.as_str())
                .filter(|t| !t.trim().is_empty())
                .map(str::to_string)
        });
    let topics: Vec<String> = serde_json::from_str(&topics_raw).unwrap_or_default();
    Ok(ContentListView {
        id: r.try_get("", "id").map_err(col_err("id"))?,
        task_id: r.try_get("", "task_id").map_err(col_err("task_id"))?,
        platform: r.try_get("", "platform").map_err(col_err("platform"))?,
        industry: String::new(), // 由 fill_content_list_views 关联 task 后填充
        content_id: r.try_get("", "content_id").map_err(col_err("content_id"))?,
        keyword: r.try_get("", "keyword").map_err(col_err("keyword"))?,
        kind: r.try_get("", "kind").map_err(col_err("kind"))?,
        title: r.try_get("", "title").map_err(col_err("title"))?,
        desc: r.try_get("", "desc").map_err(col_err("desc"))?,
        author_uid: r.try_get("", "author_uid").map_err(col_err("author_uid"))?,
        author_nickname: r
            .try_get("", "author_nickname")
            .map_err(col_err("author_nickname"))?,
        author_avatar,
        like_count: r.try_get("", "like_count").map_err(col_err("like_count"))?,
        comment_count: r
            .try_get("", "comment_count")
            .map_err(col_err("comment_count"))?,
        collect_count: r
            .try_get("", "collect_count")
            .map_err(col_err("collect_count"))?,
        share_count: r
            .try_get("", "share_count")
            .map_err(col_err("share_count"))?,
        play_count: r.try_get("", "play_count").map_err(col_err("play_count"))?,
        published_at: r
            .try_get("", "published_at")
            .map_err(col_err("published_at"))?,
        video_url: r.try_get("", "video_url").map_err(col_err("video_url"))?,
        cover_url: r.try_get("", "cover_url").map_err(col_err("cover_url"))?,
        first_image_url,
        image_count,
        xsec_token,
        duration: r.try_get("", "duration").map_err(col_err("duration"))?,
        topics,
        owner: r.try_get("", "owner").map_err(col_err("owner"))?,
        collected_at: r
            .try_get("", "collected_at")
            .map_err(col_err("collected_at"))?,
        media_status: r
            .try_get("", "media_status")
            .map_err(col_err("media_status"))?,
        audio_extracted: r
            .try_get("", "audio_extracted")
            .map_err(col_err("audio_extracted"))?,
        media_error: r
            .try_get("", "media_error")
            .map_err(col_err("media_error"))?,
        cover_path: r.try_get("", "cover_path").map_err(col_err("cover_path"))?,
        avatar_path: r
            .try_get("", "avatar_path")
            .map_err(col_err("avatar_path"))?,
        audio_path: r.try_get("", "audio_path").map_err(col_err("audio_path"))?,
        transcript_state: r
            .try_get("", "transcript_state")
            .map_err(col_err("transcript_state"))?,
        transcript_preview: r
            .try_get("", "transcript_preview")
            .map_err(col_err("transcript_preview"))?,
        transcript_error: r
            .try_get("", "transcript_error")
            .map_err(col_err("transcript_error"))?,
        cover_ocr_state: r
            .try_get("", "cover_ocr_state")
            .map_err(col_err("cover_ocr_state"))?,
        cover_ocr_preview: r
            .try_get("", "cover_ocr_preview")
            .map_err(col_err("cover_ocr_preview"))?,
        cover_ocr_error: r
            .try_get("", "cover_ocr_error")
            .map_err(col_err("cover_ocr_error"))?,
        video_downloaded: r
            .try_get("", "video_downloaded")
            .map_err(col_err("video_downloaded"))?,
        image_total: r
            .try_get("", "image_total")
            .map_err(col_err("image_total"))?,
        image_done: r.try_get("", "image_done").map_err(col_err("image_done"))?,
        comment_collected: r
            .try_get("", "comment_collected")
            .map_err(col_err("comment_collected"))?,
        intent_analyzed: r
            .try_get("", "intent_analyzed")
            .map_err(col_err("intent_analyzed"))?,
        synced_by_me: false, // 由 fill_content_list_views 按当前用户回填
    })
}

/// 列表瘦身查询:按 id 集取回裁剪后的行(IN 查询不保证顺序,由 reorder_content_list_rows 还原)
async fn query_content_list_rows(
    db: &sea_orm::DatabaseConnection,
    backend: sea_orm::DatabaseBackend,
    ids: &[String],
) -> Result<Vec<ContentListView>> {
    if ids.is_empty() {
        return Ok(vec![]);
    }
    let placeholders = ids.iter().map(|_| "?").collect::<Vec<_>>().join(", ");
    let values: Vec<sea_orm::Value> = ids.iter().cloned().map(Into::into).collect();
    let rows = db
        .query_all(raw_statement(
            backend,
            content_list_select_sql(&placeholders),
            values,
        ))
        .await
        .map_err(|e| CrawlerError::Config(format!("查询内容失败: {e}")))?;
    rows.iter().map(map_content_list_row).collect()
}

/// 按 id 顺序重排列表行(同 reorder_content_rows 的原因:raw SQL 只保证 id 列表的顺序/分页)
fn reorder_content_list_rows(ids: &[String], rows: Vec<ContentListView>) -> Vec<ContentListView> {
    let by_id: HashMap<String, ContentListView> =
        rows.into_iter().map(|m| (m.id.clone(), m)).collect();
    ids.iter().filter_map(|id| by_id.get(id).cloned()).collect()
}

/// 列表视图回填:行业(逻辑外键)+ 当前用户 Obsidian 已同步标记(与 fill_content_views 同口径)
async fn fill_content_list_views(
    db: &sea_orm::DatabaseConnection,
    me_name: &str,
    rows: Vec<ContentListView>,
) -> Result<Vec<ContentListView>> {
    let task_ids: HashSet<String> = rows.iter().map(|r| r.task_id.clone()).collect();
    let industry_map: HashMap<String, String> = task::Entity::find()
        .filter(task::Column::Id.is_in(task_ids))
        .all(db)
        .await
        .map_err(|e| CrawlerError::Config(format!("查询任务行业失败: {e}")))?
        .into_iter()
        .map(|t| (t.id, t.industry))
        .collect();
    let synced: HashSet<String> = {
        use veltrix_core::db::entity::content_synced_user as csu;
        let content_ids: HashSet<String> = rows.iter().map(|r| r.id.clone()).collect();
        csu::Entity::find()
            .filter(csu::Column::SyncedUser.eq(me_name.to_string()))
            .filter(csu::Column::ContentId.is_in(content_ids))
            .all(db)
            .await
            .unwrap_or_default()
            .into_iter()
            .map(|r| r.content_id)
            .collect()
    };
    Ok(rows
        .into_iter()
        .map(|mut view| {
            view.industry = industry_map.get(&view.task_id).cloned().unwrap_or_default();
            view.synced_by_me = synced.contains(&view.id);
            view
        })
        .collect())
}

/// 内容视图回填:行业(逻辑外键)+ 当前用户 Obsidian 已同步标记(照旧 list_contents 口径)
async fn fill_content_views(
    db: &sea_orm::DatabaseConnection,
    me_name: &str,
    rows: Vec<content::Model>,
) -> Result<Vec<ContentView>> {
    let task_ids: HashSet<String> = rows.iter().map(|r| r.task_id.clone()).collect();
    let industry_map: HashMap<String, String> = task::Entity::find()
        .filter(task::Column::Id.is_in(task_ids))
        .all(db)
        .await
        .map_err(|e| CrawlerError::Config(format!("查询任务行业失败: {e}")))?
        .into_iter()
        .map(|t| (t.id, t.industry))
        .collect();
    let synced: HashSet<String> = {
        use veltrix_core::db::entity::content_synced_user as csu;
        let content_ids: HashSet<String> = rows.iter().map(|r| r.id.clone()).collect();
        csu::Entity::find()
            .filter(csu::Column::SyncedUser.eq(me_name.to_string()))
            .filter(csu::Column::ContentId.is_in(content_ids))
            .all(db)
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

/// 评论视图回填:行业 + 所属内容信息(标题/封面/形态/作者/关键词,照旧 list_comments 口径)
async fn fill_comment_views(
    db: &sea_orm::DatabaseConnection,
    rows: Vec<comment::Model>,
) -> Result<Vec<CommentView>> {
    let task_ids: HashSet<String> = rows.iter().map(|r| r.task_id.clone()).collect();
    let industry_map: HashMap<String, String> = task::Entity::find()
        .filter(task::Column::Id.is_in(task_ids))
        .all(db)
        .await
        .map_err(|e| CrawlerError::Config(format!("查询任务行业失败: {e}")))?
        .into_iter()
        .map(|t| (t.id, t.industry))
        .collect();
    // 关联 contents 取所属内容信息(标题/封面/形态 + 内容作者),按 content.id 精确匹配
    let content_keys: HashSet<String> = rows
        .iter()
        .map(|r| format!("{}-{}-{}", r.task_id, r.platform, r.content_id))
        .collect();
    let content_map: HashMap<String, content::Model> = content::Entity::find()
        .filter(content::Column::Id.is_in(content_keys))
        .all(db)
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
                view.content_title =
                    c.title
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
                        .and_then(|v| v.get("avatar").and_then(|a| a.as_str()).map(str::to_string));
            }
            view
        })
        .collect())
}

/// 按 id 顺序重排模型:raw SQL 只取回 id 列表(保证排序/分页),Model 批量查询不保证顺序
fn reorder_content_rows(ids: &[String], models: Vec<content::Model>) -> Vec<content::Model> {
    let by_id: HashMap<String, content::Model> =
        models.into_iter().map(|m| (m.id.clone(), m)).collect();
    ids.iter().filter_map(|id| by_id.get(id).cloned()).collect()
}

fn reorder_comment_rows(ids: &[String], models: Vec<comment::Model>) -> Vec<comment::Model> {
    let by_id: HashMap<String, comment::Model> =
        models.into_iter().map(|m| (m.id.clone(), m)).collect();
    ids.iter().filter_map(|id| by_id.get(id).cloned()).collect()
}

/// 全量库分页列表:筛选与排序下沉 SQL,limit/offset + total 回传。
/// ids 模式(批量视图)忽略分页,按 id 集直接取回(上限 10000),total=返回数。
#[tauri::command]
pub async fn list_contents_page(
    state: State<'_, AppState>,
    query: ContentListQuery,
) -> Result<ContentListResult> {
    let me = current_user(&state).ok_or_else(|| CrawlerError::Config("未登录".into()))?;
    let filter = content_filter(&query, me.scope == "self", &me.name);
    let backend = state.db.get_database_backend();
    let use_ids = query
        .ids
        .as_deref()
        .map(|ids| !ids.is_empty())
        .unwrap_or(false);
    let limit = query.limit.clamp(1, 2000);
    let (page_limit, page_offset) = if use_ids {
        (10000u64, 0u64)
    } else {
        (limit, query.offset)
    };

    let id_sql = format!(
        "SELECT id FROM contents WHERE 1=1 {} ORDER BY {} LIMIT {page_limit} OFFSET {page_offset}",
        filter.conds,
        content_order(&query),
    );
    let id_rows = state
        .db
        .query_all(raw_statement(backend, id_sql, filter.values.clone()))
        .await
        .map_err(|e| CrawlerError::Config(format!("查询内容失败: {e}")))?;
    let ids: Vec<String> = id_rows
        .iter()
        .map(|r| r.try_get("", "id").unwrap_or_default())
        .collect();
    if ids.is_empty() {
        return Ok(ContentListResult {
            items: vec![],
            total: 0,
        });
    }
    let total = if use_ids {
        ids.len() as i64
    } else {
        let count_sql = format!(
            "SELECT COUNT(*) AS cnt FROM contents WHERE 1=1 {}",
            filter.conds
        );
        let count_rows = state
            .db
            .query_all(raw_statement(backend, count_sql, filter.values))
            .await
            .map_err(|e| CrawlerError::Config(format!("统计内容失败: {e}")))?;
        count_rows
            .first()
            .and_then(|r| r.try_get("", "cnt").ok())
            .unwrap_or(0)
    };
    let rows = query_content_list_rows(&state.db, backend, &ids).await?;
    let items =
        fill_content_list_views(&state.db, &me.name, reorder_content_list_rows(&ids, rows)).await?;
    Ok(ContentListResult { items, total })
}

/// 按 id 集取完整内容视图:导出 Excel / 对话插入文案等需要转写全文的场景用;
/// 列表页一律走瘦身的 list_contents_page。按入参 id 顺序返回。
#[tauri::command]
pub async fn list_contents_full(
    state: State<'_, AppState>,
    ids: Vec<String>,
) -> Result<Vec<ContentView>> {
    let me = current_user(&state).ok_or_else(|| CrawlerError::Config("未登录".into()))?;
    // 防御上限:与分页 limit clamp 同级,避免误传超大 id 集拖垮 IPC
    let ids: Vec<String> = ids.into_iter().take(2000).collect();
    if ids.is_empty() {
        return Ok(vec![]);
    }
    let models = content::Entity::find()
        .filter(content::Column::Id.is_in(ids.iter().cloned()))
        .all(&state.db)
        .await
        .map_err(|e| CrawlerError::Config(format!("查询内容失败: {e}")))?;
    // dataScope=self 只放行本人的内容:列表已按 scope 过滤,这里兜底防直接调用越权
    let models: Vec<content::Model> = if me.scope == "self" {
        models.into_iter().filter(|m| m.owner == me.name).collect()
    } else {
        models
    };
    fill_content_views(&state.db, &me.name, reorder_content_rows(&ids, models)).await
}

/// 评论库分页列表(同 list_contents_page 结构)。
#[tauri::command]
pub async fn list_comments_page(
    state: State<'_, AppState>,
    query: CommentListQuery,
) -> Result<CommentListResult> {
    let me = current_user(&state).ok_or_else(|| CrawlerError::Config("未登录".into()))?;
    let filter = comment_filter(&query, me.scope == "self", &me.name);
    let backend = state.db.get_database_backend();
    let limit = query.limit.clamp(1, 2000);

    let id_sql = format!(
        "SELECT id FROM comments WHERE 1=1 {} ORDER BY {} LIMIT {limit} OFFSET {}",
        filter.conds,
        comment_order(&query),
        query.offset,
    );
    let id_rows = state
        .db
        .query_all(raw_statement(backend, id_sql, filter.values.clone()))
        .await
        .map_err(|e| CrawlerError::Config(format!("查询评论失败: {e}")))?;
    let ids: Vec<String> = id_rows
        .iter()
        .map(|r| r.try_get("", "id").unwrap_or_default())
        .collect();
    if ids.is_empty() {
        return Ok(CommentListResult {
            items: vec![],
            total: 0,
        });
    }
    let count_sql = format!(
        "SELECT COUNT(*) AS cnt FROM comments WHERE 1=1 {}",
        filter.conds
    );
    let count_rows = state
        .db
        .query_all(raw_statement(backend, count_sql, filter.values))
        .await
        .map_err(|e| CrawlerError::Config(format!("统计评论失败: {e}")))?;
    let total = count_rows
        .first()
        .and_then(|r| r.try_get("", "cnt").ok())
        .unwrap_or(0);
    let models = comment::Entity::find()
        .filter(comment::Column::Id.is_in(ids.iter().cloned()))
        .all(&state.db)
        .await
        .map_err(|e| CrawlerError::Config(format!("查询评论失败: {e}")))?;
    let items = fill_comment_views(&state.db, reorder_comment_rows(&ids, models)).await?;
    Ok(CommentListResult { items, total })
}

// ===================== 评论库瀑布流(按来源分组) =====================

/// 评论来源分组(评论库瀑布流):来源元信息(标题/封面/作者)由组内评论行的关联字段携带,
/// comment_count 是当前筛选口径下该来源的评论总数(卡片「查看全部 N 条」用),
/// comments 为后端截好的预览(点赞倒序前 N 条),前端不再自行分组/截断。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CommentSourceGroup {
    pub platform: String,
    pub content_id: String,
    pub comment_count: i64,
    pub comments: Vec<CommentView>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CommentSourcePageResult {
    pub items: Vec<CommentSourceGroup>,
    /// 来源总数(distinct platform + content_id),分页推导用
    pub total: i64,
}

/// 评论库瀑布流:与 list_comments_page 同一筛选口径(comment_filter),按来源(所属内容)
/// 分组返回;来源排序 = 最新采集时间倒序(与表格默认序一致),每组预览评论点赞倒序。
#[tauri::command]
pub async fn list_comment_sources_page(
    state: State<'_, AppState>,
    query: CommentListQuery,
    preview_limit: Option<u64>,
) -> Result<CommentSourcePageResult> {
    let me = current_user(&state).ok_or_else(|| CrawlerError::Config("未登录".into()))?;
    let filter = comment_filter(&query, me.scope == "self", &me.name);
    let backend = state.db.get_database_backend();
    let limit = query.limit.clamp(1, 100);
    let preview = preview_limit.unwrap_or(6).clamp(1, 20);

    // 1) 当前筛选下的来源总数(distinct platform + content_id)
    let count_sql = format!(
        "SELECT COUNT(*) AS cnt FROM (SELECT 1 FROM comments WHERE 1=1 {} GROUP BY platform, content_id)",
        filter.conds
    );
    let count_rows = state
        .db
        .query_all(raw_statement(backend, count_sql, filter.values.clone()))
        .await
        .map_err(|e| CrawlerError::Config(format!("统计评论来源失败: {e}")))?;
    let total = count_rows
        .first()
        .and_then(|r| r.try_get("", "cnt").ok())
        .unwrap_or(0);
    if total == 0 {
        return Ok(CommentSourcePageResult {
            items: vec![],
            total: 0,
        });
    }

    // 2) 来源分页:最新采集倒序;组内评论数一并聚合(卡片「查看全部 N 条」用)
    let group_sql = format!(
        "SELECT platform, content_id, COUNT(*) AS cnt, MAX(collected_at) AS latest          FROM comments WHERE 1=1 {} GROUP BY platform, content_id          ORDER BY latest DESC, platform ASC, content_id ASC LIMIT {limit} OFFSET {}",
        filter.conds, query.offset
    );
    let group_rows = state
        .db
        .query_all(raw_statement(backend, group_sql, filter.values.clone()))
        .await
        .map_err(|e| CrawlerError::Config(format!("查询评论来源失败: {e}")))?;
    struct GroupKey {
        platform: String,
        content_id: String,
        count: i64,
    }
    let groups: Vec<GroupKey> = group_rows
        .iter()
        .map(|r| GroupKey {
            platform: r.try_get("", "platform").unwrap_or_default(),
            content_id: r.try_get("", "content_id").unwrap_or_default(),
            count: r.try_get("", "cnt").unwrap_or(0),
        })
        .collect();
    if groups.is_empty() {
        return Ok(CommentSourcePageResult {
            items: vec![],
            total,
        });
    }

    // 3) 预览评论:窗口函数按点赞倒序每组截前 N 条
    //    (复合键 IN 用 OR 链;页内 ≤100 组,条件长度可接受)
    let mut key_conds = String::new();
    let mut key_values: Vec<sea_orm::Value> = Vec::new();
    for g in &groups {
        key_conds.push_str(" OR (platform = ? AND content_id = ?)");
        key_values.push(g.platform.clone().into());
        key_values.push(g.content_id.clone().into());
    }
    let preview_sql = format!(
        "SELECT id FROM (SELECT id, platform, content_id,          ROW_NUMBER() OVER (PARTITION BY platform, content_id              ORDER BY COALESCE(like_count, 0) DESC, id ASC) AS rn          FROM comments WHERE 1=1 {} AND (1=0 {})) t          WHERE rn <= {preview} ORDER BY platform, content_id, rn",
        filter.conds, key_conds
    );
    let mut preview_values = filter.values.clone();
    preview_values.extend(key_values);
    let id_rows = state
        .db
        .query_all(raw_statement(backend, preview_sql, preview_values))
        .await
        .map_err(|e| CrawlerError::Config(format!("查询来源预览评论失败: {e}")))?;
    let ids: Vec<String> = id_rows
        .iter()
        .map(|r| r.try_get("", "id").unwrap_or_default())
        .collect();
    let models = comment::Entity::find()
        .filter(comment::Column::Id.is_in(ids.iter().cloned()))
        .all(&state.db)
        .await
        .map_err(|e| CrawlerError::Config(format!("查询来源预览评论失败: {e}")))?;
    let views = fill_comment_views(&state.db, reorder_comment_rows(&ids, models)).await?;

    // 4) 组装:保持第 2 步的分页顺序归组;组内即窗口序(点赞倒序)
    let mut by_key: HashMap<(String, String), Vec<CommentView>> = HashMap::new();
    for v in views {
        by_key
            .entry((v.platform.clone(), v.content_id.clone()))
            .or_default()
            .push(v);
    }
    let items = groups
        .into_iter()
        .map(|g| {
            let key = (g.platform.clone(), g.content_id.clone());
            CommentSourceGroup {
                platform: g.platform,
                content_id: g.content_id,
                comment_count: g.count,
                comments: by_key.remove(&key).unwrap_or_default(),
            }
        })
        .collect();
    Ok(CommentSourcePageResult { items, total })
}

/// 全量库「待转写 / 待提取评论 / 待采集音频」计数(与前端 needsTranscript / needsComments 逐条口径一致,
/// 含任务穿透 / 平台 / 行业 / 时间 / 搜索等全部当前筛选)。
/// 待转写口径:transcript IS NULL(未转写/上次失败);空串是「空文案」已转写标记,不计入。
#[tauri::command]
pub async fn content_library_stats(
    state: State<'_, AppState>,
    query: ContentListQuery,
) -> Result<ContentLibraryStats> {
    let me = current_user(&state).ok_or_else(|| CrawlerError::Config("未登录".into()))?;
    let filter = content_filter(&query, me.scope == "self", &me.name);
    let sql = format!(
        "SELECT \
         SUM(CASE WHEN kind = 'video' AND transcript IS NULL \
             AND audio_path IS NOT NULL AND audio_path <> '' THEN 1 ELSE 0 END) AS untranscribed, \
         SUM(CASE WHEN comment_collected IS NOT TRUE \
             AND (comment_count IS NULL OR comment_count != 0) THEN 1 ELSE 0 END) AS pending_comment, \
         SUM(CASE WHEN kind = 'video' \
             AND (audio_path IS NULL OR audio_path = '') THEN 1 ELSE 0 END) AS missing_audio \
         FROM contents WHERE 1=1 {}",
        filter.conds
    );
    let rows = state
        .db
        .query_all(raw_statement(
            state.db.get_database_backend(),
            sql,
            filter.values,
        ))
        .await
        .map_err(|e| CrawlerError::Config(format!("统计内容处理状态失败: {e}")))?;
    let first = rows.first();
    Ok(ContentLibraryStats {
        untranscribed: first
            .and_then(|r| r.try_get("", "untranscribed").ok())
            .unwrap_or(0),
        pending_comment: first
            .and_then(|r| r.try_get("", "pending_comment").ok())
            .unwrap_or(0),
        missing_audio: first
            .and_then(|r| r.try_get("", "missing_audio").ok())
            .unwrap_or(0),
    })
}

/// 批量处理的目标内容 id 快照(与当前筛选口径一致;点击瞬间一次性取回,
/// 之后成功移除不再改变集合——与前端「快照后逐条处理」的旧行为一致)。
#[tauri::command]
pub async fn list_batch_content_ids(
    state: State<'_, AppState>,
    query: ContentListQuery,
    batch: BatchKind,
) -> Result<Vec<String>> {
    let me = current_user(&state).ok_or_else(|| CrawlerError::Config("未登录".into()))?;
    let mut filter = content_filter(&query, me.scope == "self", &me.name);
    match batch {
        BatchKind::Transcript => {
            // transcript IS NULL = 未转写/上次失败;空串是「空文案」已转写标记,不重跑
            filter.conds.push_str(
                " AND kind = 'video' AND transcript IS NULL \
                 AND audio_path IS NOT NULL AND audio_path <> ''",
            );
        }
        BatchKind::Comments => {
            filter.conds.push_str(
                " AND comment_collected IS NOT TRUE \
                 AND (comment_count IS NULL OR comment_count != 0)",
            );
        }
        BatchKind::Audio => {
            // 缺音频的视频:不限素材状态——failed(下载失败)/ pending(历史任务媒体阶段未跑)/
            // success(任务未开音频提取)都可能是缺音频,用户点「采集音频」即显式要求补采
            filter
                .conds
                .push_str(" AND kind = 'video' AND (audio_path IS NULL OR audio_path = '')");
        }
    }
    let sql = format!(
        "SELECT id FROM contents WHERE 1=1 {} ORDER BY collected_at DESC, id ASC LIMIT 10000",
        filter.conds
    );
    let rows = state
        .db
        .query_all(raw_statement(
            state.db.get_database_backend(),
            sql,
            filter.values,
        ))
        .await
        .map_err(|e| CrawlerError::Config(format!("查询批量目标失败: {e}")))?;
    Ok(rows
        .iter()
        .map(|r| r.try_get("", "id").unwrap_or_default())
        .collect())
}

/// 「全部行业」总数查询:复用已构造的筛选(调用方已把 industry 置 None),
/// 不加行业非空条件——无行业内容也计入「全部」。table 仅允许 contents / comments 两个字面量。
async fn count_industry_total(
    state: &State<'_, AppState>,
    table: &str,
    filter: &FilterParts,
) -> Result<i64> {
    let sql = format!(
        "SELECT COUNT(*) AS cnt FROM {table} JOIN tasks t ON t.id = {table}.task_id WHERE 1=1 {}",
        filter.conds
    );
    let row = state
        .db
        .query_one(raw_statement(
            state.db.get_database_backend(),
            sql,
            filter.values.clone(),
        ))
        .await
        .map_err(|e| CrawlerError::Config(format!("统计全部行业总数失败: {e}")))?
        .ok_or_else(|| CrawlerError::Config("统计全部行业总数失败: 无结果行".into()))?;
    Ok(row.try_get("", "cnt").unwrap_or(0))
}

/// 全量库各行业内容数(侧栏角标)。忽略行业筛选自身(否则当前行业计数恒 0),其余筛选同列表口径。
#[tauri::command]
pub async fn content_industry_counts(
    state: State<'_, AppState>,
    query: ContentListQuery,
) -> Result<IndustryCounts> {
    let me = current_user(&state).ok_or_else(|| CrawlerError::Config("未登录".into()))?;
    let mut query = query;
    query.industry = None;
    let filter = content_filter(&query, me.scope == "self", &me.name);
    let sql = format!(
        "SELECT t.industry AS industry, COUNT(*) AS cnt FROM contents \
         JOIN tasks t ON t.id = contents.task_id \
         WHERE 1=1 {} AND t.industry IS NOT NULL AND t.industry <> '' \
         GROUP BY t.industry",
        filter.conds
    );
    let rows = state
        .db
        .query_all(raw_statement(
            state.db.get_database_backend(),
            sql,
            filter.values.clone(),
        ))
        .await
        .map_err(|e| CrawlerError::Config(format!("统计行业内容失败: {e}")))?;
    let industries = rows
        .iter()
        .map(|r| IndustryCount {
            industry: r.try_get("", "industry").unwrap_or_default(),
            count: r.try_get("", "cnt").unwrap_or(0),
        })
        .collect();
    // 「全部行业」总数:与上方同一筛选(已忽略行业),但含无行业内容
    let total = count_industry_total(&state, "contents", &filter).await?;
    Ok(IndustryCounts { total, industries })
}

/// 评论库各行业评论数(侧栏角标,口径同 content_industry_counts)。
#[tauri::command]
pub async fn comment_industry_counts(
    state: State<'_, AppState>,
    query: CommentListQuery,
) -> Result<IndustryCounts> {
    let me = current_user(&state).ok_or_else(|| CrawlerError::Config("未登录".into()))?;
    let mut query = query;
    query.industry = None;
    let filter = comment_filter(&query, me.scope == "self", &me.name);
    let sql = format!(
        "SELECT t.industry AS industry, COUNT(*) AS cnt FROM comments \
         JOIN tasks t ON t.id = comments.task_id \
         WHERE 1=1 {} AND t.industry IS NOT NULL AND t.industry <> '' \
         GROUP BY t.industry",
        filter.conds
    );
    let rows = state
        .db
        .query_all(raw_statement(
            state.db.get_database_backend(),
            sql,
            filter.values.clone(),
        ))
        .await
        .map_err(|e| CrawlerError::Config(format!("统计行业评论失败: {e}")))?;
    let industries = rows
        .iter()
        .map(|r| IndustryCount {
            industry: r.try_get("", "industry").unwrap_or_default(),
            count: r.try_get("", "cnt").unwrap_or(0),
        })
        .collect();
    // 「全部行业」总数:与上方同一筛选(已忽略行业),但含无行业内容
    let total = count_industry_total(&state, "comments", &filter).await?;
    Ok(IndustryCounts { total, industries })
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

/// 单次运行的导出数据视图:该运行时间窗内落库的内容 + 评论。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RunDataView {
    pub contents: Vec<ContentView>,
    pub comments: Vec<CommentView>,
}

/// 某次运行采集到的内容 + 评论(任务详情「执行历史」导出 Excel 用)。
/// 时间窗口径与「查看内容」穿透一致:collected_at ∈ [started_at, finished_at ?? 现在]。
#[tauri::command]
pub async fn list_run_data(state: State<'_, AppState>, run_id: String) -> Result<RunDataView> {
    let me = current_user(&state).ok_or_else(|| CrawlerError::Config("未登录".into()))?;
    let run = task_run::Entity::find_by_id(run_id)
        .one(&state.db)
        .await
        .map_err(|e| CrawlerError::Config(format!("查询执行记录失败: {e}")))?
        .ok_or_else(|| CrawlerError::Config("执行记录不存在".into()))?;
    if me.scope == "self" && run.owner != me.name {
        return Err(CrawlerError::Config("无权查看该执行记录".into()));
    }
    let end = run.finished_at.unwrap_or_else(|| Utc::now().timestamp());

    // 行业(逻辑外键,照 fill_content_views)
    let industry = task::Entity::find_by_id(run.task_id.clone())
        .one(&state.db)
        .await
        .map_err(|e| CrawlerError::Config(format!("查询任务行业失败: {e}")))?
        .map(|t| t.industry)
        .unwrap_or_default();

    collect_task_data_window(
        &state.db,
        &run.task_id,
        industry,
        Some(run.started_at),
        Some(end),
    )
    .await
}

/// 任务全部采集数据(任务调度「更多 → 导出」Excel 用):该任务落库的全部内容 + 评论。
#[tauri::command]
pub async fn list_task_data(state: State<'_, AppState>, task_id: String) -> Result<RunDataView> {
    let me = current_user(&state).ok_or_else(|| CrawlerError::Config("未登录".into()))?;
    let t = task::Entity::find_by_id(task_id)
        .one(&state.db)
        .await
        .map_err(|e| CrawlerError::Config(format!("查询任务失败: {e}")))?
        .ok_or_else(|| CrawlerError::Config("任务不存在".into()))?;
    if me.scope == "self" && t.owner != me.name {
        return Err(CrawlerError::Config("无权查看该任务".into()));
    }
    collect_task_data_window(&state.db, &t.id, t.industry, None, None).await
}

/// 任务级导出数据查询:按任务(+ 可选 collected_at 时间窗)取内容 + 评论并组装视图。
/// start/end 为 None = 任务全量(「更多 → 导出」);Some = 单次运行窗口(执行历史导出)。
async fn collect_task_data_window(
    db: &sea_orm::DatabaseConnection,
    task_id: &str,
    industry: String,
    start: Option<i64>,
    end: Option<i64>,
) -> Result<RunDataView> {
    let mut cq = content::Entity::find().filter(content::Column::TaskId.eq(task_id));
    if let Some(s) = start {
        cq = cq.filter(content::Column::CollectedAt.gte(s));
    }
    if let Some(e) = end {
        cq = cq.filter(content::Column::CollectedAt.lte(e));
    }
    let contents = cq
        .order_by_asc(content::Column::CollectedAt)
        .all(db)
        .await
        .map_err(|e| CrawlerError::Config(format!("查询内容失败: {e}")))?;
    // 评论的「所属内容」关联表(id → Model);内容视图在评论之后统一构建,避免 clone
    let content_map: HashMap<String, content::Model> =
        contents.iter().map(|c| (c.id.clone(), c.clone())).collect();

    let mut mq = comment::Entity::find().filter(comment::Column::TaskId.eq(task_id));
    if let Some(s) = start {
        mq = mq.filter(comment::Column::CollectedAt.gte(s));
    }
    if let Some(e) = end {
        mq = mq.filter(comment::Column::CollectedAt.lte(e));
    }
    let comments = mq
        .order_by_asc(comment::Column::CollectedAt)
        .all(db)
        .await
        .map_err(|e| CrawlerError::Config(format!("查询评论失败: {e}")))?;
    let comment_views: Vec<CommentView> = comments
        .into_iter()
        .map(|m| {
            let cid = format!("{}-{}-{}", m.task_id, m.platform, m.content_id);
            build_comment_view(m, content_map.get(&cid), &industry)
        })
        .collect();

    // 导出用不上「已同步 Obsidian」标记,置 false 省一次 content_synced_users 查询
    let content_views: Vec<ContentView> = contents
        .into_iter()
        .map(|m| {
            let mut view: ContentView = m.into();
            view.industry = industry.clone();
            view.synced_by_me = false;
            view
        })
        .collect();

    Ok(RunDataView {
        contents: content_views,
        comments: comment_views,
    })
}

/// 评论视图组装:关联所属内容(标题回退 desc 截断,与 fill_comment_views 同口径)。
/// content 为 None(内容已删 / 跨窗口)时关联字段留默认。
fn build_comment_view(
    m: comment::Model,
    content: Option<&content::Model>,
    industry: &str,
) -> CommentView {
    let mut view: CommentView = m.into();
    view.industry = industry.to_string();
    if let Some(c) = content {
        // 标题缺失时回退 desc 截断(抖音/快手无独立标题),与 fill_comment_views 同口径
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
        view.content_author_nickname = Some(c.author_nickname.clone());
    }
    view
}

/// 内容详情评论栏分页:每页条数(首屏 + 每次「加载更多」)。
const COMMENT_PAGE_SIZE: u32 = 50;
/// 单页上限:防御异常入参,避免一次拉几千条把详情页拖垮。
const COMMENT_PAGE_MAX: u32 = 200;

/// 内容详情评论栏的分页响应:本页评论 + 总数(标题展示)+ 下一页游标。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CommentPageView {
    pub items: Vec<CommentView>,
    pub total: i64,
    /// 下一页游标;None = 已到底(前端据此隐藏「加载更多」)
    pub next_cursor: Option<String>,
}

/// 解析评论分页游标(格式 `"{点赞数}:{评论行id}"`,用最后一个冒号分隔——
/// 行 id 为 `{task_id}-{platform}-{comment_id}` 不含冒号,防御性解析不受影响)。
fn parse_comment_cursor(cursor: &str) -> Result<(i64, String)> {
    let invalid = || CrawlerError::Config(format!("评论分页游标非法: {cursor}"));
    let (likes, id) = cursor.rsplit_once(':').ok_or_else(invalid)?;
    let likes: i64 = likes.parse().map_err(|_| invalid())?;
    if id.is_empty() {
        return Err(invalid());
    }
    Ok((likes, id.to_string()))
}

/// 单条内容的评论列表(全量库详情右侧评论栏 / 评论库瀑布流来源抽屉):
/// content_id 先按内容行主键精确匹配(详情页场景);未命中则按平台原生
/// content_id(+platform 消歧)兜底——瀑布流分组来自 comments 表,评论可能
/// 没有对应内容行(内容未入库或已删),不能因此报错,退化为纯评论查询。
/// 按点赞数倒序(热评在前)。游标分页:首页不带 cursor;has_more 时返回 nextCursor,前端「加载更多」续拉。
#[tauri::command]
pub async fn list_content_comments(
    state: State<'_, AppState>,
    content_id: String,
    cursor: Option<String>,
    limit: Option<u32>,
    platform: Option<String>,
) -> Result<CommentPageView> {
    let me = current_user(&state).ok_or_else(|| CrawlerError::Config("未登录".into()))?;
    let by_pk = content::Entity::find_by_id(content_id.clone())
        .one(&state.db)
        .await
        .map_err(|e| CrawlerError::Config(format!("查询内容失败: {e}")))?;
    let row = match by_pk {
        Some(r) => Some(r),
        None => {
            let mut cq =
                content::Entity::find().filter(content::Column::ContentId.eq(content_id.clone()));
            if let Some(p) = platform.as_deref().filter(|p| !p.is_empty()) {
                cq = cq.filter(content::Column::Platform.eq(p));
            }
            cq.one(&state.db).await.ok().flatten()
        }
    };
    if me.scope == "self" {
        if let Some(r) = &row {
            if r.owner != me.name {
                return Err(CrawlerError::Config("无权查看该内容".into()));
            }
        }
    }
    let industry = match &row {
        Some(r) => task::Entity::find_by_id(r.task_id.clone())
            .one(&state.db)
            .await
            .ok()
            .flatten()
            .map(|t| t.industry)
            .unwrap_or_default(),
        None => String::new(),
    };
    // 点赞数 NULL 按 0 参与排序与游标比较(排序/过滤口径一致,否则 NULL 行翻页会漏)
    let likes = Expr::col(comment::Column::LikeCount).if_null(0);
    let mut q = comment::Entity::find();
    q = match &row {
        Some(r) => q
            .filter(comment::Column::TaskId.eq(r.task_id.clone()))
            .filter(comment::Column::Platform.eq(r.platform.clone()))
            .filter(comment::Column::ContentId.eq(r.content_id.clone())),
        None => {
            let mut cq = q.filter(comment::Column::ContentId.eq(content_id.clone()));
            if let Some(p) = platform.as_deref().filter(|p| !p.is_empty()) {
                cq = cq.filter(comment::Column::Platform.eq(p));
            }
            // 无内容行可鉴权时退化为按评论 owner 过滤
            if me.scope == "self" {
                cq = cq.filter(comment::Column::Owner.eq(me.name.clone()));
            }
            cq
        }
    };
    let total = q.clone().count(&state.db).await.unwrap_or(0);
    if let Some(cur) = cursor.as_deref().filter(|c| !c.is_empty()) {
        let (last_likes, last_id) = parse_comment_cursor(cur)?;
        // 复合游标条件:点赞数更小,或点赞数相同但行 id 更大(id 升序 tiebreak,不重不漏)
        q = q.filter(
            Condition::any().add(likes.clone().lt(last_likes)).add(
                likes
                    .clone()
                    .eq(last_likes)
                    .and(comment::Column::Id.gt(last_id)),
            ),
        );
    }
    let limit = limit
        .unwrap_or(COMMENT_PAGE_SIZE)
        .clamp(1, COMMENT_PAGE_MAX) as u64;
    // 多取一条判断是否还有下一页,避免前端再发一次空页请求
    let rows = q
        .order_by_desc(likes)
        .order_by_asc(comment::Column::Id)
        .limit(limit + 1)
        .all(&state.db)
        .await
        .map_err(|e| CrawlerError::Config(format!("查询评论失败: {e}")))?;
    let has_more = rows.len() as u64 > limit;
    let items: Vec<CommentView> = rows
        .into_iter()
        .take(limit as usize)
        .map(|m| build_comment_view(m, row.as_ref(), &industry))
        .collect();
    // 游标取本页末行的 (点赞数, 行 id);与排序/过滤同口径(NULL 按 0)
    let next_cursor = has_more
        .then(|| {
            items
                .last()
                .map(|last| format!("{}:{}", last.like_count.unwrap_or(0), last.id))
        })
        .flatten();
    Ok(CommentPageView {
        items,
        total: total as i64,
        next_cursor,
    })
}
