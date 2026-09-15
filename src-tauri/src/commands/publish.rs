//! 发布服务命令:发布账号 CRUD(按客户分组)与登录窗口通道。
//!
//! 与采集账号管理(commands/mod.rs 账号管理一节)平行但独立:发布账号只管「写」,
//! 不参与采集轮换;分组维度直接复用 CRM 客户(customers 表,运营 > 客户管理维护),
//! 发布侧不提供分类的新建 / 删除;登录复用同一套 WebView 登录窗口 + 页内自检上报通道,
//! 仅窗口 label(`veltrix-pub-`)与上报前缀(`pub:`)不同。

use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QueryOrder, QuerySelect};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State};
use veltrix_core::db::entity::{customer, publish_account};
use veltrix_core::error::{CrawlerError, Result};

use super::{current_user, lock_config, AppState, CurrentUser};
use crate::publish::{NewAccount, PublishAccounts, ACCOUNT_UPDATED_EVENT};

/// 数据归属过滤:scope=="self" 只看自己创建的;"all" 或未登录看全部。
/// 口径与 list_accounts(采集账号)一致。
fn visible_to(owner: &str, user: &Option<CurrentUser>) -> bool {
    match user {
        Some(u) if u.scope == "self" => owner == u.name,
        _ => true,
    }
}

/// 当前会话用户名(新建记录的 owner);未登录回退空串(兼容无会话场景)。
fn current_owner(state: &AppState) -> String {
    current_user(state).map(|u| u.name).unwrap_or_default()
}

/// 发布账号的分组(客户)对外视图:直接复用 CRM 客户,展示名称 + 编码。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PublishCustomerView {
    pub id: String,
    pub name: String,
    /// 客户编码(如 CUS-XXXX)。
    pub code: String,
    /// 该客户下的发布账号数(按当前用户可见口径统计),前端分组展示用。
    pub account_count: i64,
    pub created_at: i64,
}

/// 发布账号对外视图。cookie 刻意不下发(前端无展示需求,减少凭据暴露面)。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PublishAccountView {
    pub id: String,
    pub platform: String,
    pub category_id: String,
    pub label: String,
    pub nickname: String,
    pub avatar: String,
    pub uid: String,
    pub status: String,
    pub fail_reason: String,
    pub code: String,
    pub last_login_at: i64,
    pub last_publish_at: i64,
    pub today_published: i64,
    pub created_at: i64,
}

impl From<publish_account::Model> for PublishAccountView {
    fn from(m: publish_account::Model) -> Self {
        Self {
            id: m.id,
            platform: m.platform,
            category_id: m.category_id,
            label: m.label,
            nickname: m.nickname,
            avatar: m.avatar,
            uid: m.uid,
            status: m.status,
            fail_reason: m.fail_reason,
            code: m.code,
            last_login_at: m.last_login_at,
            last_publish_at: m.last_publish_at,
            today_published: m.today_published,
            created_at: m.created_at,
        }
    }
}

// ===================== 平台清单 =====================

/// 发布平台对外视图(发布侧独立清单 = 创作者中心,非采集平台表)。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PublishPlatformView {
    pub id: String,
    pub name: String,
    pub login_url: String,
    pub enabled: bool,
}

/// 列出启用中的发布平台,供「新增发布账号」的平台下拉用(顺序即配置顺序)。
#[tauri::command]
pub async fn list_publish_platforms(
    state: State<'_, AppState>,
) -> Result<Vec<PublishPlatformView>> {
    let cfg = lock_config(&state)?;
    Ok(cfg
        .publish_platforms
        .iter()
        .filter(|p| p.enabled)
        .map(|p| PublishPlatformView {
            id: p.id.clone(),
            name: p.name.clone(),
            login_url: p.login_url.clone(),
            enabled: p.enabled,
        })
        .collect())
}

// ===================== 客户(分组维度,只读) =====================

/// 列出 CRM 客户作为发布账号的分组列表(客户的增删改在运营 > 客户管理,发布侧只读)。
/// 归属过滤口径同 list_customers:scope=="self" 只看自己跟踪的客户;"all" 或未登录看全部。
#[tauri::command]
pub async fn list_publish_customers(
    state: State<'_, AppState>,
) -> Result<Vec<PublishCustomerView>> {
    let user = current_user(&state);
    let mut query = customer::Entity::find().order_by_asc(customer::Column::CreatedAt);
    if let Some(u) = &user {
        if u.scope == "self" {
            query = query.filter(customer::Column::Owner.eq(u.name.clone()));
        }
    }
    let customers = query
        .limit(1000)
        .all(&state.db)
        .await
        .map_err(|e| CrawlerError::Config(format!("查询客户失败: {e}")))?;
    // 账号数按当前用户可见口径统计,避免「看全部」管理员视角的数字串到「仅自己」视图
    let accounts = state.publish.list_accounts(None).await?;
    let views = customers
        .into_iter()
        .map(|c| {
            let count = accounts
                .iter()
                .filter(|a| a.category_id == c.id && visible_to(&a.owner, &user))
                .count() as i64;
            PublishCustomerView {
                id: c.id,
                name: c.name,
                code: c.code,
                account_count: count,
                created_at: c.created_at,
            }
        })
        .collect();
    Ok(views)
}

// ===================== 账号 =====================

#[tauri::command]
pub async fn list_publish_accounts(
    state: State<'_, AppState>,
    category_id: Option<String>,
) -> Result<Vec<PublishAccountView>> {
    let user = current_user(&state);
    let accounts = state.publish.list_accounts(category_id.as_deref()).await?;
    Ok(accounts
        .into_iter()
        .filter(|a| visible_to(&a.owner, &user))
        .map(Into::into)
        .collect())
}

#[tauri::command]
pub async fn create_publish_account(
    state: State<'_, AppState>,
    platform: String,
    category_id: String,
    label: String,
) -> Result<PublishAccountView> {
    // 校验发布平台存在且启用(发布侧独立清单,抖音/小红书/快手 id 与采集侧相同,老数据兼容;
    // 视频号 only 发布侧有),避免建了打不开登录窗的账号
    lock_config(&state)?.publish_platform(&platform)?;
    let input = NewAccount {
        platform,
        category_id,
        label,
        owner: current_owner(&state),
    };
    let model = state.publish.create_account(input).await?;
    Ok(model.into())
}

#[tauri::command]
pub async fn update_publish_account(
    state: State<'_, AppState>,
    id: String,
    category_id: String,
    label: String,
) -> Result<PublishAccountView> {
    let model = state
        .publish
        .update_account(&id, &category_id, &label)
        .await?;
    Ok(model.into())
}

/// 删除发布账号:先关其登录窗口(防 WebView 句柄泄漏),再删记录。口径同 remove_account。
#[tauri::command]
pub async fn delete_publish_account(state: State<'_, AppState>, id: String) -> Result<()> {
    if let Some(acc) = state.publish.get(&id).await? {
        let label = crate::webview::pool::publish_window_label(&acc.platform, &id);
        let _ = state.webviews.drop_window_by_label(&label);
    }
    state.publish.remove(&id).await?;
    Ok(())
}

// ===================== 登录窗口通道 =====================

/// 打开发布账号的登录窗口(扫码 / 输入完成登录)。
/// 窗口 label `veltrix-pub-{platform}-{id}`,数据目录与采集账号隔离;
/// 关窗时定终态:自检从未报 "in" → invalid,报过 → active(Cookie 在上报时已回写)。
#[tauri::command]
pub async fn open_publish_account_login(
    state: State<'_, AppState>,
    app: AppHandle,
    id: String,
) -> Result<()> {
    let acc = state
        .publish
        .get(&id)
        .await?
        .ok_or_else(|| CrawlerError::Account(format!("发布账号不存在: {id}")))?;
    // 登录页取发布平台配置(创作者中心),不再用采集平台的主站登录页
    let pcfg = lock_config(&state)?
        .publish_platform(&acc.platform)?
        .clone();
    let webviews = state.webviews.clone();
    let publish = state.publish.clone();
    let login_verdicts = state.login_verdicts.clone();
    // 每次开窗清掉旧结论,避免上次会话的判定残留影响本次关窗终态(口径同采集登录窗)
    let verdict_key = format!("{}{id}", crate::publish::LOGIN_REPORT_PREFIX);
    if let Ok(mut map) = login_verdicts.lock() {
        map.remove(&verdict_key);
    }
    tauri::async_runtime::spawn(async move {
        match webviews.open_login_publish(&app, &acc.platform, &id, &acc.label, &pcfg) {
            Ok(window) => {
                let app_for_event = app.clone();
                let platform_for_event = acc.platform.clone();
                window.on_window_event(move |event| {
                    if matches!(event, tauri::WindowEvent::Destroyed) {
                        let publish = publish.clone();
                        let verdicts = login_verdicts.clone();
                        let key = verdict_key.clone();
                        let app = app_for_event.clone();
                        let platform = platform_for_event.clone();
                        tauri::async_runtime::spawn(async move {
                            finalize_publish_login(&publish, &verdicts, &key).await;
                            let _ = app.emit(ACCOUNT_UPDATED_EVENT, &platform);
                        });
                    }
                });
            }
            Err(e) => tracing::error!(id, "打开发布账号登录窗口失败: {e}"),
        }
    });
    Ok(())
}

/// 关闭发布账号的登录窗口(不删账号、不清登录态)。对齐采集侧 remove_account 的关窗方式。
/// 关窗触发的 Destroyed 监听会按最近自检结论定终态(active / invalid)。
#[tauri::command]
pub async fn close_publish_account_window(state: State<'_, AppState>, id: String) -> Result<()> {
    if let Some(acc) = state.publish.get(&id).await? {
        let label = crate::webview::pool::publish_window_label(&acc.platform, &id);
        state.webviews.drop_window_by_label(&label)?;
    }
    Ok(())
}

/// 发布账号登录窗口关闭后的终态落定(发布开窗的 Destroyed 监听与采集关窗路径共用):
/// 最近结论为 "in" → active;否则(明确 "out" 或从未上报)→ invalid。
/// Cookie 不在这里补:Destroyed 时 WebView 已销毁读不到,Cookie 在 login_status_report
/// 收到 "in" 时就地从存活会话读出回写。
pub(crate) async fn finalize_publish_login(
    publish: &PublishAccounts,
    verdicts: &std::sync::Arc<std::sync::Mutex<std::collections::HashMap<String, String>>>,
    verdict_key: &str,
) {
    let last = verdicts.lock().ok().and_then(|mut m| m.remove(verdict_key));
    let Some(account_id) = verdict_key.strip_prefix(crate::publish::LOGIN_REPORT_PREFIX) else {
        return;
    };
    let result = if last.as_deref() == Some("in") {
        publish.mark_active(account_id).await
    } else {
        publish
            .mark_invalid(account_id, "登录窗口关闭,未完成登录")
            .await
    };
    if let Err(e) = result {
        tracing::warn!(account_id, "发布账号登录终态回写失败: {e}");
    }
}

/// `login_status_report` 的发布账号分支(account_id = `pub:{id}`)。
/// 收到 "in":置 active,并就地从存活登录窗口读会话 Cookie 回写——此时窗口必然存活
/// (脚本刚上报);关窗 Destroyed 时 WebView 已销毁读不到,故 Cookie 在上报时回写而非关窗时。
/// nickname / uid 暂无可靠页面特征可抓,按约定留空,后续发布链路需要时再补。
pub(crate) async fn publish_login_status_report(
    state: &AppState,
    app: &AppHandle,
    account_id: &str,
    status: &str,
) -> Result<()> {
    let Some(id) = account_id.strip_prefix(crate::publish::LOGIN_REPORT_PREFIX) else {
        return Ok(());
    };
    if status != "in" {
        return Ok(());
    }
    if let Err(e) = state.publish.mark_active(id).await {
        tracing::warn!(account_id, "发布账号登录检测置 active 失败: {e}");
        return Ok(());
    }
    let Ok(Some(acc)) = state.publish.get(id).await else {
        return Ok(());
    };
    // Cookie 回写失败不阻断:active 已置,下次上报可再补;Cookie 禁打日志。
    // 读 Cookie 的 URL 用发布平台登录页(创作者中心域)——登录发生在那里,主站域读不到对应 Cookie。
    let login_url = lock_config(state).ok().and_then(|cfg| {
        cfg.publish_platform(&acc.platform)
            .ok()
            .map(|p| p.login_url.clone())
    });
    let label = crate::webview::pool::publish_window_label(&acc.platform, id);
    if let (Some(url), Some(window)) = (login_url, app.get_webview_window(&label)) {
        if let Some(cookie) = crate::webview::cookies::read_cookies(&window, &url).await {
            if let Err(e) = state.publish.set_cookie(id, &cookie).await {
                tracing::warn!(account_id, "发布账号 Cookie 回写失败: {e}");
            }
        }
    }
    // 通知前端刷新发布账号列表(免手动刷新)
    let _ = app.emit(ACCOUNT_UPDATED_EVENT, &acc.platform);
    Ok(())
}
