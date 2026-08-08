//! 作者画像补采:对指定作者逐个打开主页、拦截画像接口,刷新 authors 表画像字段。
//! 从采集流水线拆出——独立命令 enrich_authors + 其私有辅助,自成一类。

use super::{
    account_collect_lock, account_lock_key, current_user, lock_config, random_comment_video_interval,
    AppState,
};
use crate::adapter::{FetchContext, PlatformAdapter};
use crate::model::{Author, TaskKind};
use crate::webview::pool::{CollectBridge, ProfileCollectRequest};
use chrono::Utc;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, IntoActiveModel, QueryFilter,
    QueryOrder, Set,
};
use serde::Serialize;
use std::sync::Arc;
use tauri::{AppHandle, State};
use veltrix_core::config::PlatformConfig;
use veltrix_core::error::{CrawlerError, Result};

// ===================== 作者画像补采 =====================

/// 作者画像补采的结果汇总(前端 toast 展示)。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EnrichSummary {
    /// 请求补采的作者数。
    pub requested: usize,
    /// 成功刷新画像的作者数。
    pub updated: usize,
    /// 跳过数(平台不支持 / 无账号 / 缺 token / 无权限等,非错误)。
    pub skipped: usize,
    /// 失败数(导航 / 拦截 / 解析 / 落库失败)。
    pub failed: usize,
    /// 跳过 / 失败的简要原因(逐条,供前端提示)。
    pub messages: Vec<String>,
}

/// 取某作者最近一条内容里留存的 author_xsec_token(小红书主页导航鉴权用)。
/// 小红书内容 extra 存了 `author_xsec_token`;无内容 / 无 token 返回 Ok(None);
/// 查询失败返回 Err——DB 错误与「无记录」区分开,避免误报「缺 xsec_token」。
async fn latest_author_xsec_token(
    db: &DatabaseConnection,
    owner: &str,
    platform: &str,
    uid: &str,
) -> Result<Option<String>> {
    use veltrix_core::db::entity::content as content_entity;
    let row = content_entity::Entity::find()
        .filter(content_entity::Column::Owner.eq(owner))
        .filter(content_entity::Column::Platform.eq(platform))
        .filter(content_entity::Column::AuthorUid.eq(uid))
        .order_by_desc(content_entity::Column::CollectedAt)
        .one(db)
        .await
        .map_err(|e| CrawlerError::Config(format!("查询作者 xsec_token 失败: {e}")))?;
    let Some(row) = row else {
        return Ok(None);
    };
    let token = serde_json::from_str::<serde_json::Value>(&row.extra)
        .ok()
        .and_then(|extra| {
            extra
                .get("author_xsec_token")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(str::to_string)
        });
    Ok(token)
}

/// 把补采解析出的画像合并进已有作者档案:只覆盖「解析到的有效字段」
/// (字符串非空、数值 >0),缺失 / 空串 / 0 值字段保留原值(避免空响应清掉已有数据);
/// is_monitored / first_collected_at 始终保留。
async fn apply_profile_to_author(
    db: &DatabaseConnection,
    existing: &veltrix_core::db::entity::author::Model,
    parsed: &Author,
    now: i64,
) -> Result<()> {
    let extra_str = |key: &str| {
        parsed
            .extra
            .get(key)
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    let extra_i64 = |key: &str| {
        parsed
            .extra
            .get(key)
            .and_then(|v| v.as_i64())
            .filter(|&v| v > 0)
    };

    let mut am = existing.clone().into_active_model();
    if !parsed.nickname.is_empty() {
        am.nickname = Set(parsed.nickname.clone());
    }
    // 字符串字段非空才覆盖、数值字段 >0 才覆盖:空串 / 0 视为「没采到」,保留原值
    if let Some(avatar) = parsed.avatar.as_deref().filter(|s| !s.is_empty()) {
        am.avatar = Set(Some(avatar.to_string()));
    }
    if let Some(signature) = parsed.signature.as_deref().filter(|s| !s.is_empty()) {
        am.signature = Set(Some(signature.to_string()));
    }
    if let Some(follower) = parsed.follower_count.filter(|&v| v > 0) {
        am.follower_count = Set(Some(follower));
    }
    if let Some(following) = parsed.following_count.filter(|&v| v > 0) {
        am.following_count = Set(Some(following));
    }
    if let Some(pid) = extra_str("unique_id") {
        am.platform_id = Set(Some(pid));
    }
    if let Some(fav) = extra_i64("total_favorited") {
        am.total_favorited = Set(Some(fav));
    }
    if let Some(loc) = extra_str("ip_location") {
        am.location = Set(Some(loc));
    }
    am.last_collected_at = Set(now);
    am.update(db)
        .await
        .map_err(|e| CrawlerError::Config(format!("刷新作者画像失败: {e}")))?;
    Ok(())
}

/// 单作者画像补采的调用环境(遵守「参数 ≤ 4」集中成结构体)。
/// 手动补采命令与采集流水线的自动补采共用:前置校验(平台启用/适配器支持/账号可用)
/// 与账号互斥锁均由调用方负责,此处只描述"用哪个窗口、以谁的身份补"。
pub(super) struct EnrichAuthorArgs<'a> {
    pub app: &'a AppHandle,
    pub db: &'a DatabaseConnection,
    pub bridge: &'a CollectBridge,
    pub cfg: &'a PlatformConfig,
    pub adapter: Arc<dyn PlatformAdapter>,
    pub account_id: &'a str,
    /// Some 时补采日志经任务事件推给前端面板(采集流水线内);手动补采传 None。
    pub task_id: Option<&'a str>,
}

/// 单作者补采结论:调用方据此计数与提示。
pub(super) enum EnrichOutcome {
    /// 画像已刷新落库。
    Updated,
    /// 前置条件不满足跳过(如小红书缺 xsec_token),携带原因。
    Skipped(String),
    /// 导航 / 拦截 / 解析 / 落库失败,携带原因。
    Failed(String),
}

/// 单个作者的画像补采:导航主页 → 拦截画像接口 → 解析 → 合并进作者档案。
/// 搜索响应的 author 对象不带粉丝/关注/获赞/属地,这些字段只能靠主页画像接口补齐。
pub(super) async fn enrich_author_profile(
    args: &EnrichAuthorArgs<'_>,
    author: &veltrix_core::db::entity::author::Model,
) -> EnrichOutcome {
    // 小红书主页导航需 xsec_token:取该作者最近一条内容留存的 author_xsec_token
    let xsec_token = if author.platform == "xhs" {
        match latest_author_xsec_token(args.db, &author.owner, &author.platform, &author.uid).await
        {
            Ok(Some(t)) => t,
            Ok(None) => return EnrichOutcome::Skipped("缺 xsec_token(需先采集其内容)".into()),
            Err(e) => return EnrichOutcome::Failed(e.to_string()),
        }
    } else {
        String::new()
    };

    let responses = match args
        .bridge
        .collect_profile(
            args.app,
            ProfileCollectRequest {
                account_id: args.account_id,
                uid: &author.uid,
                nickname: &author.nickname,
                xsec_token: &xsec_token,
                platform_cfg: args.cfg,
                task_id: args.task_id,
                adapter: args.adapter.clone(),
            },
        )
        .await
    {
        Ok(r) => r,
        Err(e) => return EnrichOutcome::Failed(format!("补采失败:{e}")),
    };
    if responses.is_empty() {
        return EnrichOutcome::Failed("未拦到画像接口(未登录 / 风控?)".into());
    }
    // 解析:ctx.keyword 传 uid,适配器据此把画像归属到该作者
    let ctx = FetchContext {
        keyword: author.uid.clone(),
        responses,
    };
    let parsed = match args.adapter.parse(&TaskKind::UserProfile, &ctx).await {
        Ok(out) => out.authors.into_iter().next(),
        Err(e) => return EnrichOutcome::Failed(format!("解析失败:{e}")),
    };
    let Some(parsed) = parsed else {
        return EnrichOutcome::Failed("画像接口无有效数据".into());
    };
    let now = Utc::now().timestamp();
    match apply_profile_to_author(args.db, author, &parsed, now).await {
        Ok(()) => EnrichOutcome::Updated,
        Err(e) => EnrichOutcome::Failed(e.to_string()),
    }
}

/// 作者画像补采:对指定作者逐个打开主页、拦截画像接口、刷新 authors 表画像字段。
/// 仅 `supports(UserProfile)` 的平台(抖音 / 小红书 / 快手 / B站 / YouTube)有效,其余跳过。
/// 串行限速逐个处理(复用账号采集互斥锁,锁等待 30s 超时跳过;不抢占正在跑的采集),
/// 用户手动关闭采集窗口即终止、剩余作者记跳过,结束后归还补采开过的窗口。返回汇总供前端提示。
/// self scope 仅能补采自己 owner 的作者。
#[tauri::command]
pub async fn enrich_authors(
    state: State<'_, AppState>,
    app: AppHandle,
    ids: Vec<String>,
) -> Result<EnrichSummary> {
    use veltrix_core::db::entity::author as author_entity;
    let me = current_user(&state).ok_or_else(|| CrawlerError::Config("未登录".into()))?;

    // 去重:重复 id 会让 requested 虚高、重复项被误算进 skipped
    let mut ids = ids;
    ids.sort();
    ids.dedup();

    let authors = author_entity::Entity::find()
        .filter(author_entity::Column::Id.is_in(ids.clone()))
        .all(&state.db)
        .await
        .map_err(|e| CrawlerError::Config(format!("查询作者失败: {e}")))?;

    let mut summary = EnrichSummary {
        requested: ids.len(),
        updated: 0,
        skipped: 0,
        failed: 0,
        messages: Vec::new(),
    };
    // 查不到的 id(已删除等)计为跳过,并告知用户具体是哪些出了问题
    if authors.len() < ids.len() {
        let missing = ids.len() - authors.len();
        summary.skipped += missing;
        summary
            .messages
            .push(format!("{missing} 个作者不存在(可能已删除),已跳过"));
    }

    let bridge = CollectBridge::new(
        state.webviews.clone(),
        state.intercept_channel.clone(),
        state.rpa_channel.clone(),
        state.collect_control.clone(),
    );

    let mut processed = 0usize;
    // 记录本批补采实际开过采集窗口的账号(平台, 账号),结束后统一归还——
    // 只关自己开过的,不碰别的账号正在用的窗口
    let mut opened_windows: Vec<(String, String)> = Vec::new();
    for (idx, a) in authors.iter().enumerate() {
        if me.scope == "self" && a.owner != me.name {
            summary.skipped += 1;
            continue;
        }
        // 平台配置(clone 出来,不跨 await 持配置锁)
        let cfg = {
            lock_config(&state)
                .ok()
                .and_then(|c| c.platform(&a.platform).ok().cloned())
        };
        let Some(cfg) = cfg else {
            summary.skipped += 1;
            summary.messages.push(format!("{} · 平台未启用或不存在", a.nickname));
            continue;
        };
        // 适配器须支持画像补采
        let adapter = match state.registry.get(&a.platform) {
            Ok(ad) if ad.supports(&TaskKind::UserProfile) => ad,
            _ => {
                summary.skipped += 1;
                summary
                    .messages
                    .push(format!("{} · {} 不支持画像补采", a.nickname, a.platform));
                continue;
            }
        };
        if cfg.collect.profile_url_template.is_empty() {
            summary.skipped += 1;
            summary.messages.push(format!("{} · 未配置主页地址", a.nickname));
            continue;
        }
        // 该平台可用账号:走 acquire 轮换(「最久未用」优先 + 更新 last_used_at),
        // 不再恒取第一个 active——批量补采压在同一账号上更容易触发风控
        let account_id = match state.cookies.acquire(&a.platform).await {
            Ok(acc) => Some(acc.id),
            Err(_) => None,
        };
        let Some(account_id) = account_id else {
            summary.skipped += 1;
            summary
                .messages
                .push(format!("{} · 平台 {} 无可用账号", a.nickname, a.platform));
            continue;
        };
        // 串行限速:首个不等,之后每个之间随机间隔降频
        if processed > 0 {
            tokio::time::sleep(random_comment_video_interval()).await;
        }
        processed += 1;

        // 账号采集互斥:与正常采集共用锁,避免抢占同账号窗口(锁不跨外层 await 持有问题——
        // 本就是要在补采期间独占该账号窗口)。
        // 锁等待带 30s 超时:采集任务持锁可达数十分钟,无限等会让「画像补采」一直卡死;
        // 超时则跳过该作者继续下一个,不阻塞整批补采
        let account_lock =
            account_collect_lock(&state.collect_locks, &account_lock_key(&a.platform, &account_id));
        let _guard = match tokio::time::timeout(
            std::time::Duration::from_secs(30),
            account_lock.lock(),
        )
        .await
        {
            Ok(guard) => guard,
            Err(_) => {
                summary.skipped += 1;
                summary
                    .messages
                    .push(format!("{} · 账号窗口被采集任务占用,稍后再试", a.nickname));
                continue;
            }
        };

        // 用户手动关闭采集窗口 = 终止补采:不再为后续作者重建窗口
        // (与采集主链路「关窗即终止」语义一致),剩余作者记 Skipped
        if bridge.is_collect_window_closed(&cfg.id, &account_id) {
            let remaining = authors.len() - idx;
            summary.skipped += remaining;
            summary
                .messages
                .push(format!("采集窗口已被手动关闭 · 终止补采(剩余 {remaining} 位作者跳过)"));
            break;
        }
        // 补采持锁期间开的窗口属于自己,登记下来结束后归还
        opened_windows.push((a.platform.clone(), account_id.clone()));

        let enrich_args = EnrichAuthorArgs {
            app: &app,
            db: &state.db,
            bridge: &bridge,
            cfg: &cfg,
            adapter: adapter.clone(),
            account_id: &account_id,
            task_id: None,
        };
        match enrich_author_profile(&enrich_args, a).await {
            EnrichOutcome::Updated => summary.updated += 1,
            EnrichOutcome::Skipped(msg) => {
                summary.skipped += 1;
                summary.messages.push(format!("{} · {msg}", a.nickname));
            }
            EnrichOutcome::Failed(msg) => {
                summary.failed += 1;
                summary.messages.push(format!("{} · {msg}", a.nickname));
            }
        }
    }

    // 归还补采期间打开的采集窗口(正常结束或中断都执行)。
    // 关窗会触发 Destroyed 把「被手动关闭」标记置位,随即重置该标记,
    // 避免自己关窗留下的标记污染下次补采的首轮检查
    for (platform, account_id) in &opened_windows {
        bridge.close_collect_window(platform, account_id);
        bridge.reset_collect_window_closed(platform, account_id);
    }

    Ok(summary)
}
