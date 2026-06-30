//! 数据概览(首页 Dashboard)聚合查询命令。
//!
//! 从 `task.rs` 拆出:全量库 / 评论库 / 意向客资计数、平台细分、采集趋势等只读聚合统计,
//! 自成一类、与任务 / 内容 CRUD 解耦,便于单独演进。

use crate::commands::{current_user, AppState};
use chrono::{Local, Utc};
use sea_orm::{
    ColumnTrait, EntityTrait, PaginatorTrait, QueryFilter, QueryOrder, QuerySelect,
};
use serde::Serialize;
use tauri::State;
use veltrix_core::db::entity::{comment, content, task};
use veltrix_core::error::{CrawlerError, Result};

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

/// 给定本地自然日,返回其 0 点的 Unix 时间戳(秒)。
/// DST 切换的模糊时刻 single() 为 None,本机时区(无 DST)恒为单值,兜底 0。
fn local_date_start_ts(date: chrono::NaiveDate) -> i64 {
    date.and_hms_opt(0, 0, 0)
        .and_then(|naive| naive.and_local_timezone(Local).single())
        .map(|dt| dt.timestamp())
        .unwrap_or(0)
}

/// 本地时区「今天 0 点」的 Unix 时间戳(秒)。
/// 「今日 / 按天」统计必须按用户本地自然日:桌面端跑在用户机器上,UTC+8 下若用 UTC 边界,
/// 凌晨时段会把跨日数据整体错位一天(本地已是新一天、UTC 仍停在昨天)。
fn local_today_start_ts() -> i64 {
    local_date_start_ts(Local::now().date_naive())
}

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
        let today = Local::now().date_naive();
        local_date_start_ts(today - chrono::Duration::days(TREND_DAYS - 1))
    });

    // 日期轴(本地日历日,MM-DD):与「今日」边界同口径,凌晨时段不错位
    let start_date = chrono::DateTime::from_timestamp(start_ts, 0)
        .map(|dt| dt.with_timezone(&Local).date_naive())
        .unwrap_or_else(|| Local::now().date_naive());
    let end_date = chrono::DateTime::from_timestamp(end_ts, 0)
        .map(|dt| dt.with_timezone(&Local).date_naive())
        .unwrap_or_else(|| Local::now().date_naive());
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
        chrono::DateTime::from_timestamp(ts, 0)
            .map(|dt| dt.with_timezone(&Local).format("%m-%d").to_string())
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
    let today0 = local_today_start_ts();
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
    // 今日完成:completed 且 finished_at 落在今天(本地自然日)
    let today0 = local_today_start_ts();
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
    list.sort_by_key(|item| std::cmp::Reverse(item.1));
    list.truncate(50);
    Ok(list
        .into_iter()
        .map(|(keyword, count)| KeywordCount { keyword, count })
        .collect())
}
