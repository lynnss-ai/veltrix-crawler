//! 作者画像补采:对指定作者逐个打开主页、拦截画像接口,刷新 authors 表画像字段。
//! 从采集流水线拆出——独立命令 enrich_authors + 其私有辅助,自成一类。

use super::{
    account_collect_lock, current_user, lock_config, random_comment_video_interval,
    AppState,
};
use crate::adapter::FetchContext;
use crate::cookie::AccountStatus;
use crate::model::{Author, TaskKind};
use crate::webview::pool::{CollectBridge, ProfileCollectRequest};
use chrono::Utc;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, IntoActiveModel, QueryFilter,
    QueryOrder, Set,
};
use serde::Serialize;
use tauri::{AppHandle, State};
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
/// 小红书内容 extra 存了 `author_xsec_token`;无内容 / 无 token 返回 None。
async fn latest_author_xsec_token(
    db: &DatabaseConnection,
    owner: &str,
    platform: &str,
    uid: &str,
) -> Option<String> {
    use veltrix_core::db::entity::content as content_entity;
    let row = content_entity::Entity::find()
        .filter(content_entity::Column::Owner.eq(owner))
        .filter(content_entity::Column::Platform.eq(platform))
        .filter(content_entity::Column::AuthorUid.eq(uid))
        .order_by_desc(content_entity::Column::CollectedAt)
        .one(db)
        .await
        .ok()
        .flatten()?;
    let extra: serde_json::Value = serde_json::from_str(&row.extra).ok()?;
    extra
        .get("author_xsec_token")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// 把补采解析出的画像合并进已有作者档案:只覆盖「解析到的非空字段」,
/// 缺失字段保留原值(避免空响应清掉已有数据);is_monitored / first_collected_at 始终保留。
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
    let extra_i64 = |key: &str| parsed.extra.get(key).and_then(|v| v.as_i64());

    let mut am = existing.clone().into_active_model();
    if !parsed.nickname.is_empty() {
        am.nickname = Set(parsed.nickname.clone());
    }
    if parsed.avatar.is_some() {
        am.avatar = Set(parsed.avatar.clone());
    }
    if parsed.signature.is_some() {
        am.signature = Set(parsed.signature.clone());
    }
    if parsed.follower_count.is_some() {
        am.follower_count = Set(parsed.follower_count);
    }
    if parsed.following_count.is_some() {
        am.following_count = Set(parsed.following_count);
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

/// 作者画像补采:对指定作者逐个打开主页、拦截画像接口、刷新 authors 表画像字段。
/// 仅 `supports(UserProfile)` 的平台(小红书 / 快手 / B站 / YouTube)有效,其余跳过。
/// 串行限速逐个处理(复用账号采集互斥锁,不抢占正在跑的采集),返回汇总供前端提示。
/// self scope 仅能补采自己 owner 的作者。
#[tauri::command]
pub async fn enrich_authors(
    state: State<'_, AppState>,
    app: AppHandle,
    ids: Vec<String>,
) -> Result<EnrichSummary> {
    use veltrix_core::db::entity::author as author_entity;
    let me = current_user(&state).ok_or_else(|| CrawlerError::Config("未登录".into()))?;

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
    // 查不到的 id(含被 scope 过滤的)计为跳过
    if authors.len() < ids.len() {
        summary.skipped += ids.len() - authors.len();
    }

    let bridge = CollectBridge::new(
        state.webviews.clone(),
        state.intercept_channel.clone(),
        state.rpa_channel.clone(),
        state.collect_control.clone(),
    );

    let mut processed = 0usize;
    for a in &authors {
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
        // 该平台可用账号
        let account_id = match state.cookies.list(&a.platform).await {
            Ok(list) => list
                .into_iter()
                .find(|x| matches!(x.status, AccountStatus::Active))
                .map(|x| x.id),
            Err(_) => None,
        };
        let Some(account_id) = account_id else {
            summary.skipped += 1;
            summary
                .messages
                .push(format!("{} · 平台 {} 无可用账号", a.nickname, a.platform));
            continue;
        };
        // 小红书主页导航需 xsec_token:取该作者最近一条内容留存的 author_xsec_token
        let xsec_token = if a.platform == "xhs" {
            match latest_author_xsec_token(&state.db, &a.owner, &a.platform, &a.uid).await {
                Some(t) => t,
                None => {
                    summary.skipped += 1;
                    summary
                        .messages
                        .push(format!("{} · 缺 xsec_token(需先采集其内容)", a.nickname));
                    continue;
                }
            }
        } else {
            String::new()
        };

        // 串行限速:首个不等,之后每个之间随机间隔降频
        if processed > 0 {
            tokio::time::sleep(random_comment_video_interval()).await;
        }
        processed += 1;

        // 账号采集互斥:与正常采集共用锁,避免抢占同账号窗口(锁不跨外层 await 持有问题——
        // 本就是要在补采期间独占该账号窗口)
        let account_lock =
            account_collect_lock(&state.collect_locks, &format!("{}-{}", a.platform, account_id));
        let _guard = account_lock.lock().await;

        let responses = match bridge
            .collect_profile(
                &app,
                ProfileCollectRequest {
                    account_id: &account_id,
                    uid: &a.uid,
                    nickname: &a.nickname,
                    xsec_token: &xsec_token,
                    platform_cfg: &cfg,
                    task_id: None,
                    adapter: adapter.clone(),
                },
            )
            .await
        {
            Ok(r) => r,
            Err(e) => {
                summary.failed += 1;
                summary.messages.push(format!("{} · 补采失败:{e}", a.nickname));
                continue;
            }
        };
        if responses.is_empty() {
            summary.failed += 1;
            summary
                .messages
                .push(format!("{} · 未拦到画像接口(未登录 / 风控?)", a.nickname));
            continue;
        }
        // 解析:ctx.keyword 传 uid,适配器据此把画像归属到该作者
        let ctx = FetchContext {
            keyword: a.uid.clone(),
            responses,
        };
        let parsed = match adapter.parse(&TaskKind::UserProfile, &ctx).await {
            Ok(out) => out.authors.into_iter().next(),
            Err(e) => {
                summary.failed += 1;
                summary.messages.push(format!("{} · 解析失败:{e}", a.nickname));
                continue;
            }
        };
        let Some(parsed) = parsed else {
            summary.failed += 1;
            summary
                .messages
                .push(format!("{} · 画像接口无有效数据", a.nickname));
            continue;
        };
        let now = Utc::now().timestamp();
        match apply_profile_to_author(&state.db, a, &parsed, now).await {
            Ok(()) => summary.updated += 1,
            Err(e) => {
                summary.failed += 1;
                summary.messages.push(format!("{} · {e}", a.nickname));
            }
        }
    }

    Ok(summary)
}
