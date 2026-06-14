//! 前端可调用的 Tauri IPC 命令。
//!
//! 阶段0 提供平台管理;阶段1 追加账号管理与签名回调;后续追加用户/系统配置 CRUD(admin)。

pub mod admin;
pub mod chat;
pub mod cloud;
pub mod task;

use veltrix_core::config::{AppConfig, PlatformConfig};
use crate::cookie::{Account, AccountStatus, CookiePool};
use veltrix_core::error::{CrawlerError, Result};
use crate::adapter::{FetchContext, FetchOutput};
use crate::model::{Author, Comment, Content, ContentKind, TaskKind};
use crate::webview::pool::{
    CollectBridge, CollectRequest, CommentCollectRequest, ProfileCollectRequest, WebviewPool,
};
use crate::webview::{
    emit_collect_entry, emit_collect_log, CollectControl, CollectEntry, InterceptChannel,
    RpaChannel, RpaOutcome,
};
use chrono::Utc;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DatabaseBackend, DatabaseConnection,
    EntityTrait, IntoActiveModel, QueryFilter, QueryOrder, Set, Statement,
};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, State};

/// 后端会话内的「当前登录用户」。桌面端走 IPC、无 JWT,
/// 故用进程内内存态替代鉴权上下文:name=用户名(业务数据 owner),scope="all"/"self"。
#[derive(Clone)]
pub struct CurrentUser {
    pub name: String,
    pub scope: String,
}

/// 应用级共享状态。所有跨命令、跨任务共享的句柄聚合在此。
pub struct AppState {
    pub config: Mutex<AppConfig>,
    pub config_dir: PathBuf,
    pub registry: crate::adapter::AdapterRegistry,
    /// 全局数据库连接(运行时二选一 SQLite / PostgreSQL),供账号池等持久化复用。
    pub db: DatabaseConnection,
    pub cookies: Arc<CookiePool>,
    pub webviews: Arc<WebviewPool>,
    pub intercept_channel: Arc<InterceptChannel>,
    /// 拟人 RPA 运行结果回传通道(`rpa_done` 命令写入,采集端等待)。
    pub rpa_channel: Arc<RpaChannel>,
    /// 采集中断控制(`stop_collect` 命令写入,采集循环读取以优雅停止)。
    pub collect_control: Arc<CollectControl>,
    /// 当前登录用户会话态;登录前为 None。
    /// 用 std::sync::Mutex,临界区内绝不跨 .await 持锁(取值即克隆后立刻释放)。
    pub current_user: Mutex<Option<CurrentUser>>,
    /// 云端连接客户端:配对、WS 长连接、远程指令执行
    pub cloud: Arc<crate::cloud::CloudClient>,
    /// (平台-账号) → 采集互斥锁:同账号对应同一个 WebView 窗口,两个采集并发驱动
    /// 同一窗口会互踩(导航 / 滚动 / 会话注入互相覆盖)。同账号串行,不同账号 / 平台并行。
    /// 惰性建锁、任务结束不移除(账号数有限,常驻无碍)。
    pub collect_locks: Arc<Mutex<std::collections::HashMap<String, Arc<tokio::sync::Mutex<()>>>>>,
    /// account_id → 登录窗口内自检回传的最近登录态结论("in" / "out")。
    /// 登录窗口关闭时据此决定终态:最近为 "out"(仍明确未登录)→ invalid;其余 → 乐观 active。
    pub login_verdicts: Arc<Mutex<std::collections::HashMap<String, String>>>,
}

/// 取某「平台-账号」的采集互斥锁(惰性创建)。外层 std Mutex 仅做表查找,绝不跨 await 持有。
fn account_collect_lock(
    locks: &Arc<Mutex<std::collections::HashMap<String, Arc<tokio::sync::Mutex<()>>>>>,
    key: &str,
) -> Arc<tokio::sync::Mutex<()>> {
    let mut map = locks.lock().unwrap_or_else(|e| e.into_inner());
    map.entry(key.to_string()).or_default().clone()
}

/// 读取当前登录用户:克隆出 Option 后立即释放锁,杜绝跨 await 持锁。
pub(crate) fn current_user(state: &AppState) -> Option<CurrentUser> {
    state
        .current_user
        .lock()
        .ok()
        .and_then(|guard| guard.clone())
}

fn lock_config(state: &AppState) -> Result<std::sync::MutexGuard<'_, AppConfig>> {
    state
        .config
        .lock()
        .map_err(|_| CrawlerError::Config("配置状态锁异常".into()))
}

// ===================== 会话:当前登录用户 =====================

/// 设置后端当前登录用户(登录成功 / 启动恢复登录态时由前端调用)。
#[tauri::command]
pub fn set_current_user(
    state: State<'_, AppState>,
    username: String,
    data_scope: String,
) -> Result<()> {
    let mut guard = state
        .current_user
        .lock()
        .map_err(|_| CrawlerError::Config("当前用户状态锁异常".into()))?;
    *guard = Some(CurrentUser {
        name: username,
        scope: data_scope,
    });
    Ok(())
}

/// 清除后端当前登录用户(退出登录时调用)。
#[tauri::command]
pub fn clear_current_user(state: State<'_, AppState>) -> Result<()> {
    let mut guard = state
        .current_user
        .lock()
        .map_err(|_| CrawlerError::Config("当前用户状态锁异常".into()))?;
    *guard = None;
    Ok(())
}

// ===================== 平台管理 =====================

#[tauri::command]
pub fn get_app_config(state: State<'_, AppState>) -> Result<AppConfig> {
    Ok(lock_config(&state)?.clone())
}

/// 查询数据库当前占用大小(字节)。SQLite 取页数×页大小,PostgreSQL 用 pg_database_size。
#[tauri::command]
pub async fn get_database_size(state: State<'_, AppState>) -> Result<i64> {
    let db = &state.db;
    let backend = db.get_database_backend();
    let sql = match backend {
        DatabaseBackend::Sqlite => {
            "SELECT (SELECT page_count FROM pragma_page_count()) * \
             (SELECT page_size FROM pragma_page_size()) AS size"
        }
        DatabaseBackend::Postgres => "SELECT pg_database_size(current_database()) AS size",
        DatabaseBackend::MySql => {
            return Err(CrawlerError::Config("不支持的数据库后端".into()))
        }
    };
    let row = db
        .query_one(Statement::from_string(backend, sql.to_owned()))
        .await
        .map_err(|e| CrawlerError::Config(format!("查询数据库大小失败: {e}")))?;
    let size = row
        .and_then(|r| r.try_get::<i64>("", "size").ok())
        .unwrap_or(0);
    Ok(size)
}

/// 测试数据库连接串能否连通(不影响当前连接)。
#[tauri::command]
pub async fn test_database_connection(url: String) -> Result<()> {
    veltrix_core::db::test_connection(&url).await
}

/// 应用默认数据目录(存储路径留空时使用)。
#[tauri::command]
pub fn get_data_dir(state: State<'_, AppState>) -> Result<String> {
    Ok(state.config_dir.display().to_string())
}

/// 当前生效的素材存储根目录(绝对路径)。output_dir 为空 / 相对时拼应用数据目录补全,
/// 供系统设置「存储路径」展示完整路径(而非裸 "media")。
#[tauri::command]
pub fn get_media_root(state: State<'_, AppState>) -> Result<String> {
    let cfg = lock_config(&state)?;
    Ok(crate::media::media_root(&state.config_dir, &cfg.media)
        .display()
        .to_string())
}

/// 获取当前生效的 SQLite 数据库文件路径;非 SQLite(如 PG)返回 None。
#[tauri::command]
pub fn get_database_path(state: State<'_, AppState>) -> Result<Option<String>> {
    let cfg = lock_config(&state)?;
    let url = veltrix_core::db::resolve_url(&state.config_dir, &cfg.database)?;
    Ok(veltrix_core::db::sqlite_file_path(&url))
}

/// 保存数据库配置(连接串与连接池上限)。写入配置文件,重启应用后重连生效。
#[tauri::command]
pub fn set_database_config(
    state: State<'_, AppState>,
    url: String,
    max_connections: u32,
) -> Result<()> {
    let mut cfg = lock_config(&state)?;
    cfg.database.url = url;
    cfg.database.max_connections = max_connections;
    cfg.save(&state.config_dir)
}

/// 保存采集素材的存储根目录(系统设置「存储路径」)。
/// 写入 `config.media.output_dir` 并持久化;空串表示回退应用默认数据目录。
#[tauri::command]
pub fn set_storage_path(state: State<'_, AppState>, path: String) -> Result<()> {
    let mut cfg = lock_config(&state)?;
    cfg.media.output_dir = path;
    cfg.save(&state.config_dir)
}

/// 保存评论意向分析配置(系统设置「意向分析」)。只存对 providers/prompts 的 id 引用 +
/// 模型名 + 批大小;api_key 仍存数据库,不落配置文件。写入后重启或下次任务运行生效。
#[tauri::command]
pub fn set_intent_config(
    state: State<'_, AppState>,
    provider_id: String,
    model: String,
    prompt_id: String,
    batch_size: i32,
) -> Result<()> {
    let mut cfg = lock_config(&state)?;
    cfg.intent.provider_id = provider_id;
    cfg.intent.model = model;
    cfg.intent.prompt_id = prompt_id;
    cfg.intent.batch_size = batch_size;
    cfg.save(&state.config_dir)
}

/// 保存语音转写配置(系统设置「语音转写」)。只存厂商 id 引用 + 模型名;
/// api_key 仍存数据库,不落配置文件。目前仅支持 ASR 的厂商(小米 MiMo)可用。
#[tauri::command]
pub fn set_transcription_config(
    state: State<'_, AppState>,
    provider_id: String,
    model: String,
) -> Result<()> {
    let mut cfg = lock_config(&state)?;
    cfg.transcription.provider_id = provider_id;
    cfg.transcription.model = model;
    cfg.save(&state.config_dir)
}

/// 列出各厂商能力(chat / asr),供前端「语音转写」配置按 ASR 能力过滤厂商下拉。
#[tauri::command]
pub fn list_provider_capabilities() -> Vec<crate::llm::ProviderCapability> {
    crate::llm::all_capabilities()
}

/// 将文本写入指定路径(供前端导出/下载,配合 dialog.save 选定路径)。
/// 安全约束:
/// - 必须是绝对路径(防相对路径绕到工作目录)
/// - 必须以「应用数据目录」为前缀(防写到任意系统位置)
/// - 不允许 `..` 越界
#[tauri::command]
pub fn save_text_file(
    state: State<'_, AppState>,
    path: String,
    content: String,
) -> Result<()> {
    let target = PathBuf::from(&path);
    if !target.is_absolute() {
        return Err(CrawlerError::Config("路径必须是绝对路径".into()));
    }
    if target
        .components()
        .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return Err(CrawlerError::Config("路径包含非法的 .. 段".into()));
    }
    // 规范化前缀(必须在 app 数据目录之下)
    let allowed_root = state.config_dir.canonicalize().unwrap_or_else(|_| {
        state.config_dir.clone()
    });
    let target_parent = target.parent().ok_or_else(|| {
        CrawlerError::Config("路径缺少父目录".into())
    })?;
    let parent_canon = target_parent.canonicalize().unwrap_or_else(|_| {
        target_parent.to_path_buf()
    });
    if !parent_canon.starts_with(&allowed_root) {
        return Err(CrawlerError::Config(format!(
            "拒绝写入应用数据目录之外的路径: {}",
            target.display()
        )));
    }
    std::fs::write(&target, content)
        .map_err(|e| CrawlerError::Config(format!("保存文件失败: {e}")))
}

/// 清空业务数据(系统配置「危险操作」)。不可恢复:
/// 1. 用当前登录用户名 + 传入密码做 argon2 二次校验,未登录或密码错直接拒绝;
/// 2. 按逻辑外键依赖顺序删空 comments → contents → tasks(无物理级联,手动顺序);
/// 3. clear_media 为 true 时,递归清空媒体素材根目录下所有文件(保留目录本身);
///    为 false 时只清库,已下载的素材文件原样保留。
///
/// 平台 / 账号 / 用户 / 客户 / 行业 / 厂商 / 提示词等配置类数据一律保留。
#[tauri::command]
pub async fn clear_business_data(
    state: State<'_, AppState>,
    password: String,
    clear_media: bool,
) -> Result<()> {
    use veltrix_core::db::entity::{
        collect_log as collect_log_entity, comment as comment_entity, content as content_entity,
        task as task_entity,
    };

    // 必须已登录:以会话用户名校验密码,杜绝无身份直接清库
    let user =
        current_user(&state).ok_or_else(|| CrawlerError::Auth("未登录,禁止清空数据".into()))?;
    admin::verify_user_password(&state.db, &user.name, &password).await?;

    // 先取媒体根目录(临界区内拿配置即释放锁,不跨 await 持锁)
    let media_root = {
        let cfg = lock_config(&state)?;
        crate::media::media_root(&state.config_dir, &cfg.media)
    };

    let db = &state.db;
    // 先删子表(日志 / 评论 / 内容)再删父表(任务),与逻辑外键依赖方向一致
    collect_log_entity::Entity::delete_many()
        .exec(db)
        .await
        .map_err(|e| CrawlerError::Config(format!("清空采集日志失败: {e}")))?;
    comment_entity::Entity::delete_many()
        .exec(db)
        .await
        .map_err(|e| CrawlerError::Config(format!("清空评论失败: {e}")))?;
    content_entity::Entity::delete_many()
        .exec(db)
        .await
        .map_err(|e| CrawlerError::Config(format!("清空内容失败: {e}")))?;
    task_entity::Entity::delete_many()
        .exec(db)
        .await
        .map_err(|e| CrawlerError::Config(format!("清空任务失败: {e}")))?;

    if clear_media {
        clear_dir_contents(&media_root)?;
    }
    Ok(())
}

/// 递归删除目录下全部条目但保留目录本身;目录不存在视为已清空(无素材可删)。
/// 安全护栏:拒绝对盘符根 / 无父级的路径动手,避免存储路径误配成根目录时连带清空系统盘。
fn clear_dir_contents(dir: &Path) -> Result<()> {
    if !dir.exists() {
        return Ok(());
    }
    if dir.parent().is_none() {
        return Err(CrawlerError::Config(format!(
            "拒绝清空疑似根目录: {}",
            dir.display()
        )));
    }
    for entry in std::fs::read_dir(dir)
        .map_err(|e| CrawlerError::Config(format!("读取素材目录失败: {e}")))?
    {
        let entry =
            entry.map_err(|e| CrawlerError::Config(format!("遍历素材目录失败: {e}")))?;
        let path = entry.path();
        let removed = if path.is_dir() {
            std::fs::remove_dir_all(&path)
        } else {
            std::fs::remove_file(&path)
        };
        removed.map_err(|e| {
            CrawlerError::Config(format!("删除素材 {} 失败: {e}", path.display()))
        })?;
    }
    Ok(())
}

#[tauri::command]
pub fn list_platforms(state: State<'_, AppState>) -> Result<Vec<PlatformConfig>> {
    Ok(lock_config(&state)?.platforms.values().cloned().collect())
}

#[tauri::command]
pub fn upsert_platform(state: State<'_, AppState>, platform: PlatformConfig) -> Result<()> {
    let mut cfg = lock_config(&state)?;
    cfg.upsert_platform(platform);
    cfg.save(&state.config_dir)
}

#[tauri::command]
pub fn remove_platform(state: State<'_, AppState>, id: String) -> Result<bool> {
    let mut cfg = lock_config(&state)?;
    let removed = cfg.remove_platform(&id);
    if removed {
        cfg.save(&state.config_dir)?;
    }
    Ok(removed)
}

#[tauri::command]
pub fn registered_adapters(state: State<'_, AppState>) -> Vec<String> {
    state.registry.registered_ids()
}

// ===================== 账号管理 =====================

/// 前端传入的账号载荷。把 status 用字符串约定,避免与 Rust 枚举强耦合。
#[derive(Debug, Deserialize)]
pub struct AccountInput {
    pub id: String,
    pub platform: String,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub cookie: String,
    #[serde(default)]
    pub code: String,
    #[serde(default)]
    pub remark: String,
    #[serde(default)]
    pub owner: String,
}

/// 账号对外视图,展平 status 字符串便于前端表格展示。
#[derive(Debug, Serialize)]
pub struct AccountView {
    pub id: String,
    pub platform: String,
    pub label: String,
    pub cookie: String,
    pub status: String,
    pub risk_count: i64,
    pub cooldown_until: i64,
    pub last_used_at: i64,
    pub created_at: i64,
    pub code: String,
    pub remark: String,
    pub owner: String,
}

impl From<Account> for AccountView {
    fn from(a: Account) -> Self {
        let status = match a.status {
            AccountStatus::Active => "active",
            AccountStatus::Cooldown => "cooldown",
            AccountStatus::Invalid => "invalid",
            AccountStatus::Disabled => "disabled",
        };
        Self {
            id: a.id,
            platform: a.platform,
            label: a.label,
            cookie: a.cookie,
            status: status.into(),
            risk_count: a.risk_count,
            cooldown_until: a.cooldown_until,
            last_used_at: a.last_used_at,
            created_at: a.created_at,
            code: a.code,
            remark: a.remark,
            owner: a.owner,
        }
    }
}

#[tauri::command]
pub async fn list_accounts(
    state: State<'_, AppState>,
    platform: String,
) -> Result<Vec<AccountView>> {
    // 先取出当前用户(克隆后释放锁),再做异步查询,避免跨 await 持锁
    let user = current_user(&state);
    let accounts = state.cookies.list(&platform).await?;
    // scope=="self" 只返回自己创建的;"all" 或未登录返回全部
    let views = accounts
        .into_iter()
        .filter(|a| match &user {
            Some(u) if u.scope == "self" => a.owner == u.name,
            _ => true,
        })
        .map(Into::into)
        .collect();
    Ok(views)
}

/// 保存 / 更新一个账号(账号管理界面)。
///
/// 刻意不走 `cookie.upsert`:采集登录回写用的那条 upsert 路径在 on_conflict 时
/// 不更新 code/remark/owner(避免被采集占位空值覆盖)。但账号管理需要能更新备注等字段,
/// 故这里直接对 account 实体做 find_by_id 判断 insert/update,更新时保留 cookie 与风控状态。
#[tauri::command]
pub async fn upsert_account(state: State<'_, AppState>, account: AccountInput) -> Result<()> {
    use veltrix_core::db::entity::account as account_entity;

    let db = &state.db;
    let now = Utc::now().timestamp();
    // 编码须全表唯一(排除自身),避免重复编码
    let dup = account_entity::Entity::find()
        .filter(account_entity::Column::Code.eq(account.code.clone()))
        .filter(account_entity::Column::Id.ne(account.id.clone()))
        .one(db)
        .await
        .map_err(|e| CrawlerError::Account(format!("查询账号失败: {e}")))?;
    if dup.is_some() {
        return Err(CrawlerError::Config(format!("编码已存在: {}", account.code)));
    }
    let existing = account_entity::Entity::find_by_id(account.id.clone())
        .one(db)
        .await
        .map_err(|e| CrawlerError::Account(format!("查询账号失败: {e}")))?;
    match existing {
        Some(model) => {
            // 编辑:仅更新账号管理可维护的字段,cookie / 风控状态 / 创建时间保持不变。
            // owner(归属)不随编辑变更,保留原值。
            let mut am = model.into_active_model();
            am.platform = Set(account.platform);
            am.label = Set(account.label);
            am.code = Set(account.code);
            am.remark = Set(account.remark);
            am.update(db)
                .await
                .map_err(|e| CrawlerError::Account(format!("更新账号失败: {e}")))?;
        }
        None => {
            // 新建归属由后端会话决定:有当前用户则记其用户名,无则回退前端传值(兼容)
            let owner = current_user(&state)
                .map(|u| u.name)
                .unwrap_or(account.owner);
            let am = account_entity::ActiveModel {
                id: Set(account.id),
                platform: Set(account.platform),
                label: Set(account.label),
                cookie: Set(account.cookie),
                // 新建账号默认未登录,显示「去登录」;扫码登录后(窗口关闭)转为 active
                status: Set(AccountStatus::Invalid.as_str().to_string()),
                risk_count: Set(0),
                cooldown_until: Set(0),
                last_used_at: Set(0),
                created_at: Set(now),
                code: Set(account.code),
                remark: Set(account.remark),
                owner: Set(owner),
            };
            am.insert(db)
                .await
                .map_err(|e| CrawlerError::Account(format!("创建账号失败: {e}")))?;
        }
    }
    Ok(())
}

#[tauri::command]
pub async fn remove_account(
    state: State<'_, AppState>,
    platform: String,
    account_id: String,
) -> Result<bool> {
    // 顺带关闭对应 WebView,避免句柄泄漏
    let _ = state.webviews.drop_window(&platform, &account_id);
    state.cookies.remove(&account_id).await
}

/// 清空某账号的登录状态:关窗 + 删除该账号 WebView 数据目录(登录凭据),并置 invalid。
/// 下次点「登录」从干净状态重新扫码。账号记录与备注保留,只清登录态。
#[tauri::command]
pub async fn clear_account_login(
    state: State<'_, AppState>,
    app: AppHandle,
    platform: String,
    account_id: String,
) -> Result<()> {
    state
        .webviews
        .clear_login_data(&app, &platform, &account_id)
        .await?;
    state.cookies.mark_invalid(&account_id).await?;
    // 清掉登录检测残留结论,避免影响下次登录判定
    if let Ok(mut map) = state.login_verdicts.lock() {
        map.remove(&account_id);
    }
    Ok(())
}

/// 登录窗口关闭、账号转 active 后推送给前端的事件名,payload 为平台 id。
/// 前端账号页 listen 后刷新对应平台账号列表,免用户手动点刷新。
const ACCOUNT_LOGIN_UPDATED_EVENT: &str = "account-login-updated";

/// 登录窗口内自检脚本回传登录态结论。`status`: "in"(已登录)/ "out"(明确未登录)。
/// 检测到已登录立即把账号置 active 并通知前端(列表实时变绿);记录最近结论供关窗时定终态。
#[tauri::command]
pub async fn login_status_report(
    state: State<'_, AppState>,
    app: AppHandle,
    account_id: String,
    status: String,
) -> Result<()> {
    // 记录最近结论(关窗 Destroyed 时读取)
    if let Ok(mut map) = state.login_verdicts.lock() {
        map.insert(account_id.clone(), status.clone());
    }
    // 检测到已登录:实时置 active,前端即时变绿,不必等关窗
    if status == "in" {
        if let Err(e) = state.cookies.mark_active(&account_id).await {
            tracing::warn!(account_id, "登录检测置 active 失败: {e}");
            return Ok(());
        }
        if let Ok(Some(acc)) = state.cookies.get(&account_id).await {
            use tauri::Emitter;
            let _ = app.emit(ACCOUNT_LOGIN_UPDATED_EVENT, &acc.platform);
        }
    }
    Ok(())
}

/// 打开某账号的可见登录窗口,用户在其中扫码 / 输入完成登录。
/// 登录态写入该账号独立的 WebView 数据目录,采集时复用同窗口即带登录态。
#[tauri::command]
pub fn open_login_window(
    state: State<'_, AppState>,
    app: AppHandle,
    platform: String,
    account_id: String,
    account_label: String,
) -> Result<()> {
    // 取出平台配置(clone 出来,不持锁进异步)。在异步线程里 build WebView,
    // 避免在主线程同步创建窗口 + 加载首页时阻塞事件循环,导致窗口卡死 / 关不掉。
    let pcfg = lock_config(&state)?.platform(&platform)?.clone();
    let webviews = state.webviews.clone();
    let cookies = state.cookies.clone();
    let login_verdicts = state.login_verdicts.clone();
    // 每次打开登录窗口清掉旧结论,避免上次会话的判定残留影响本次关窗终态
    if let Ok(mut map) = login_verdicts.lock() {
        map.remove(&account_id);
    }
    tauri::async_runtime::spawn(async move {
        match webviews.open_login(&app, &platform, &account_id, &account_label, &pcfg) {
            Ok(window) => {
                // 关窗时定终态:窗口内自检最近结论为 "out"(仍明确处于未登录)→ 置 invalid;
                // 其余(检测到 "in"、不确定、或未配置检测)→ 沿用乐观行为置 active,不误伤。
                let acc = account_id.clone();
                let app_for_event = app.clone();
                let platform_for_event = platform.clone();
                window.on_window_event(move |event| {
                    if matches!(event, tauri::WindowEvent::Destroyed) {
                        let cookies = cookies.clone();
                        let acc = acc.clone();
                        let app = app_for_event.clone();
                        let platform = platform_for_event.clone();
                        let verdicts = login_verdicts.clone();
                        tauri::async_runtime::spawn(async move {
                            let last = verdicts
                                .lock()
                                .ok()
                                .and_then(|mut m| m.remove(&acc));
                            let result = if last.as_deref() == Some("out") {
                                cookies.mark_invalid(&acc).await
                            } else {
                                cookies.mark_active(&acc).await
                            };
                            if let Err(e) = result {
                                tracing::warn!("登录后回写账号状态失败: {e}");
                                return;
                            }
                            // 状态已更新,通知前端刷新该平台账号列表(免手动刷新)
                            use tauri::Emitter;
                            let _ = app.emit(ACCOUNT_LOGIN_UPDATED_EVENT, &platform);
                        });
                    }
                });
            }
            Err(e) => tracing::error!(platform, account_id, "打开账号窗口失败: {e}"),
        }
    });
    Ok(())
}

// ===================== 采集:拦截回传与启动 =====================

/// 页面内拦截 hook 调用本命令回传一条命中的接口响应。
/// 字段命名与注入脚本中的 invoke 一致(camelCase: sessionId/url/body)。
#[tauri::command]
pub fn intercept_push(state: State<'_, AppState>, session_id: u64, url: String, body: String) {
    state.intercept_channel.push(session_id, url, body);
}

/// HUD「结束」按钮回传:请求停止指定会话的采集。
/// 采集循环每轮检查该标志,命中即优雅停止(保留已采内容,作为正常完成)。
#[tauri::command]
pub fn stop_collect(state: State<'_, AppState>, session_id: u64) {
    state.collect_control.request_stop(session_id);
}

/// 拟人 RPA 执行器跑完(或某步失败)时回传结果。
/// 字段与注入脚本一致(camelCase: runId/ok/failedStep/message)。
#[tauri::command]
pub fn rpa_done(
    state: State<'_, AppState>,
    run_id: u64,
    ok: bool,
    failed_step: i64,
    message: String,
) {
    state.rpa_channel.complete(
        run_id,
        RpaOutcome {
            ok,
            failed_step,
            message,
        },
    );
}

/// 一次采集的结果。`urls` 暴露命中的接口便于联调核对 `intercept_patterns`。
#[derive(Debug, Serialize)]
pub struct CollectResult {
    /// 拦截到的接口响应数量。
    pub intercepted: usize,
    /// 命中的接口 URL 列表。
    pub urls: Vec<String>,
    pub contents: Vec<Content>,
    pub comments: Vec<Comment>,
}

/// 用关键词在指定账号的可见 WebView 内执行一次 RPA 采集。
///
/// 流程:复用登录态窗口 → 导航搜索页 → 拦截接口响应 → 交平台适配器解析为统一模型。
/// 未注册该平台适配器时不报错,仅返回拦截到的原始接口信息,供联调阶段验证拦截链路。
#[tauri::command]
pub async fn start_collect(
    state: State<'_, AppState>,
    app: AppHandle,
    platform: String,
    keyword: String,
    account_id: String,
) -> Result<CollectResult> {
    // 先 clone 出平台配置,避免把配置锁的 guard 跨 await 持有
    let cfg = { lock_config(&state)?.platform(&platform)?.clone() };

    // 联调单采也竞争同账号互斥锁,避免与正在运行的任务并发驱动同一窗口互踩
    let account_lock =
        account_collect_lock(&state.collect_locks, &format!("{}-{account_id}", cfg.id));
    let _collect_guard = account_lock.lock().await;

    let bridge = CollectBridge::new(
        state.webviews.clone(),
        state.intercept_channel.clone(),
        state.rpa_channel.clone(),
        state.collect_control.clone(),
    );
    let responses = bridge
        .collect(
            &app,
            CollectRequest {
                account_id: &account_id,
                keyword: &keyword,
                platform_cfg: &cfg,
                task_id: None,
                // 联调单采:不设目标数量,退回固定轮数盲滚
                target_count: 0,
                adapter: None,
                // 联调单采不增量落库,行为不变
                content_tx: None,
                existing_ids: None,
                sort_mode: "",
                time_range: "",
                // 联调单采不按点赞过滤
                min_likes: 0,
            },
        )
        .await?;

    let intercepted = responses.len();
    let urls = responses.iter().map(|r| r.url.clone()).collect();

    // 有适配器则解析为统一模型;暂未注册时降级为只返回原始拦截信息
    let (contents, comments) = match state.registry.get(&platform) {
        Ok(adapter) => {
            let ctx = FetchContext { keyword, responses };
            let output = adapter.parse(&TaskKind::Search, &ctx).await?;
            (output.contents, output.comments)
        }
        Err(_) => (Vec::new(), Vec::new()),
    };

    Ok(CollectResult {
        intercepted,
        urls,
        contents,
        comments,
    })
}

/// 启动一个任务的采集:选该平台一个可用账号,后台遍历关键词逐个采集(自动开窗 + 拟人 RPA),
/// 命令立即返回,采集在后台进行,前端轮询 `list_tasks` 看进度。
///
/// 当前为最小闭环:`content_count` 暂记拦截到的接口响应数(非真实内容数),待解析落库后修正。
#[tauri::command]
pub async fn run_task(
    state: State<'_, AppState>,
    app: AppHandle,
    task_id: String,
) -> Result<()> {
    // entity 名与本模块 `mod task` 同名,别名规避冲突
    use veltrix_core::db::entity::task as task_entity;

    let model = task_entity::Entity::find_by_id(task_id.clone())
        .one(&state.db)
        .await
        .map_err(|e| CrawlerError::Config(format!("查询任务失败: {e}")))?
        .ok_or_else(|| CrawlerError::Config(format!("任务不存在: {task_id}")))?;

    // 防重复启动:任务已在进行中(双击「运行」/ 前端状态滞后)时再 spawn 一份采集,
    // 两份会写同一任务的进度与状态互相覆盖,直接拒绝
    if matches!(
        model.status.as_str(),
        "running" | "collecting_comments" | "analyzing_comments" | "downloading_media"
    ) {
        return Err(CrawlerError::Config("任务正在进行中,请勿重复启动".into()));
    }

    let platform = model.platform.clone();
    let owner = model.owner.clone();
    let keywords: Vec<String> = serde_json::from_str(&model.keywords).unwrap_or_default();
    if keywords.is_empty() {
        return Err(CrawlerError::Config("任务未配置关键词".into()));
    }

    // 选该平台一个可用账号;account_id 作为采集窗口的隔离 key(对应独立 WebView2 数据目录)
    let account = state
        .cookies
        .list(&platform)
        .await?
        .into_iter()
        .find(|a| matches!(a.status, AccountStatus::Active))
        .ok_or_else(|| {
            CrawlerError::Config(format!("平台 {platform} 无可用账号,请先在账号管理添加并登录"))
        })?;
    let account_id = account.id;

    // clone 出平台配置,避免把配置锁 guard 跨 await 持有
    let cfg = { lock_config(&state)?.platform(&platform)?.clone() };

    // 媒体下载所需:存储配置与配置目录(用于解析素材根目录)。
    // 在 spawn 前 clone 出来 move 进后台任务,避免跨 await 持有配置锁。
    let media_cfg = { lock_config(&state)?.media.clone() };
    let config_dir = state.config_dir.clone();

    // 意向分析配置(provider/prompt 引用 + 模型 + 批大小);clone 出来 move 进后台任务
    let intent_cfg = { lock_config(&state)?.intent.clone() };
    // 语音转写配置(厂商引用 + 模型);clone 出来 move 进后台任务,采集结束后转写用
    let transcription_cfg = { lock_config(&state)?.transcription.clone() };

    // 每关键词目标数量:作为滚动「按量停止」的依据(<=0 视为不限,退回固定轮数盲滚)
    let per_keyword_limit = model.per_keyword_limit.max(0) as usize;
    // 最低点赞数:采集时过滤,点赞数低于此值的内容不计目标数、不落库(0=不限)
    let min_likes = model.min_likes.max(0);

    // 评论采集参数(model 即将被 into_active_model 消费,先取出 move 进后台任务)
    let collect_comments = model.collect_comments;
    let comment_time_range = model.comment_time_range.clone();
    let comment_limit = model.comment_limit.max(0) as usize;
    let analyze_comment_intent = model.analyze_comment_intent;
    // AI 文案提取:开 → 视频下载并转音频(供转写);关 → 视频不下载、不存储
    let ai_extract = model.ai_extract;
    // 采集完成后是否自动同步到发起者(owner)的 Obsidian vault
    let auto_sync_obsidian = model.auto_sync_obsidian;
    // 排序方式 / 发布时间:采集时在结果页做 RPA 文案点击筛选
    let sort_mode = model.sort_mode.clone();
    let time_range = model.time_range.clone();

    // 先标记 running + started_at,前端立即看到状态翻转
    let now = Utc::now().timestamp();
    let mut am = model.into_active_model();
    am.status = Set("running".to_string());
    am.started_at = Set(Some(now));
    am.progress = Set(0);
    am.updated_at = Set(now);
    let started = am
        .update(&state.db)
        .await
        .map_err(|e| CrawlerError::Config(format!("更新任务状态失败: {e}")))?;
    // 启动瞬间推送一次,前端立即翻转为「运行中」并据此开启轮询兜底
    emit_task_progress(&app, started);

    // 后台采集,不阻塞命令返回。句柄均为 Clone/Arc,可安全 move 进 spawn
    let db = state.db.clone();
    let registry = state.registry.clone();
    let collect_locks = state.collect_locks.clone();
    let bridge = CollectBridge::new(
        state.webviews.clone(),
        state.intercept_channel.clone(),
        state.rpa_channel.clone(),
        state.collect_control.clone(),
    );
    tauri::async_runtime::spawn(async move {
        // 同账号采集互斥:占用 WebView 窗口的阶段(关键词采集 + 评论采集)串行,
        // 其他账号 / 平台的任务不受影响,可真正并行采集
        let account_lock =
            account_collect_lock(&collect_locks, &format!("{}-{}", cfg.id, account_id));
        let collect_guard = account_lock.lock().await;
        // 落库为「判重 upsert」保留历史(不删数据);采集结果=累计总量,采集明细=本次新增,
        // 靠 keyword_stats_for_tasks 按 collected_at >= 本次 started_at 过滤实现(on_conflict 不刷新 collected_at)。

        // 执行历史:本次运行先记一条 task_run(running);采集日志按 [started_at, finished_at]
        // 时间范围归到该次运行(见 list_run_logs)。run_id 用 task_id + 起始秒,串行运行起始秒唯一。
        let run_id = format!("{}-run-{}", task_id, now);
        {
            use veltrix_core::db::entity::task_run as run_entity;
            let am = run_entity::ActiveModel {
                id: Set(run_id.clone()),
                task_id: Set(task_id.clone()),
                owner: Set(owner.clone()),
                started_at: Set(now),
                finished_at: Set(None),
                status: Set("running".to_string()),
                content_delta: Set(0),
                comment_delta: Set(0),
                error_message: Set(None),
            };
            if let Err(e) = am.insert(&db).await {
                tracing::warn!(task_id = %task_id, "创建执行历史失败: {e}");
            }
        }

        // 平台适配器:有则解析落库并计真实数,无则降级为只累计拦截响应数(不落明细)
        let adapter = registry.get(&cfg.id).ok();
        let total = keywords.len();
        emit_collect_log(&app, &task_id, "info", format!("任务开始,共 {total} 个关键词"));
        if adapter.is_none() {
            emit_collect_log(
                &app,
                &task_id,
                "warn",
                format!("平台 {} 未注册适配器,仅统计拦截数,明细不落库", cfg.id),
            );
        }
        let mut seen_contents: HashSet<String> = HashSet::new();
        let mut seen_comments: HashSet<String> = HashSet::new();
        let mut intercepted_total: i64 = 0;
        // 是否出现过关键词采集失败:用于结尾区分终态(零产出且有错 → failed,否则 completed)
        let mut had_error = false;
        // 本次任务解析出的全部内容,采集主流程结束后统一下载素材(去重后避免重复下载)
        let mut contents_for_media: Vec<Content> = Vec::new();

        // 该任务已采内容快照(content_id 集合):智能停止据此「只数新增」(重复不占目标配额),
        // 素材下载也据此跳过已成功下载的旧内容。运行开始时取一次,运行中新增的不进此集合。
        let existing_ids = load_existing_content_ids(&db, &task_id).await;
        // 内容逐条日志的任务内序号(跨关键词连续);consumer 子任务共享,故用原子量
        let content_seq = std::sync::Arc::new(std::sync::atomic::AtomicI64::new(0));

        for (idx, keyword) in keywords.iter().enumerate() {
            let progress = (((idx + 1) as f64 / total as f64) * 100.0) as i32;
            emit_collect_log(
                &app,
                &task_id,
                "info",
                format!("采集第 {}/{} 个关键词:{keyword}", idx + 1, total),
            );

            let (content_count, comment_count) = match &adapter {
                Some(adapter_arc) => {
                    // 边采边入库:collect 滚动循环每轮把新增 Content 经 channel 发出,消费任务用
                    // **独立局部 seen** 即时落库 + 回写进度(只为实时可见)。不碰主 seen,故消费任务
                    // 异常也不会重置跨关键词去重与最终计数(那由下面的「兜底全量解析」维护)。
                    let (tx, mut rx) =
                        tokio::sync::mpsc::unbounded_channel::<Vec<Content>>();
                    let db_c = db.clone();
                    let task_id_c = task_id.clone();
                    let owner_c = owner.clone();
                    let keyword_c = keyword.clone();
                    let app_c = app.clone();
                    let content_seq_c = content_seq.clone();
                    let consumer = tauri::async_runtime::spawn(async move {
                        let mut seen_c_local: HashSet<String> = HashSet::new();
                        let mut seen_m_local: HashSet<String> = HashSet::new();
                        while let Some(batch) = rx.recv().await {
                            if batch.is_empty() {
                                continue;
                            }
                            // 逐条内容富日志:序号 + 头像 + 昵称 + 标题(截断)+ 类型
                            for c in &batch {
                                let seq = content_seq_c
                                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
                                    + 1;
                                let title = log_content_title(c);
                                emit_collect_entry(
                                    &app_c,
                                    &task_id_c,
                                    format!("#{seq} {} {}", c.author.nickname, title),
                                    CollectEntry {
                                        kind: "content".to_string(),
                                        seq,
                                        avatar: c.author.avatar.clone(),
                                        nickname: c.author.nickname.clone(),
                                        title,
                                        content_kind: Some(
                                            content_kind_label(&c.kind).to_string(),
                                        ),
                                    },
                                );
                            }
                            let output = FetchOutput {
                                contents: batch,
                                comments: Vec::new(),
                                authors: Vec::new(),
                            };
                            persist_collected(
                                &db_c,
                                &task_id_c,
                                &owner_c,
                                &keyword_c,
                                output,
                                &mut seen_c_local,
                                &mut seen_m_local,
                            )
                            .await;
                            let c = seen_c_local.len() as i64;
                            write_task_progress(&app_c, &db_c, &task_id_c, progress, c, 0).await;
                            emit_collect_log(
                                &app_c,
                                &task_id_c,
                                "info",
                                format!("「{keyword_c}」已入库 {c} 条内容(累计)"),
                            );
                        }
                    });

                    // 启动采集:tx 透传进 CollectRequest,滚动循环每轮发新增内容
                    let collect_result = bridge
                        .collect(
                            &app,
                            CollectRequest {
                                account_id: &account_id,
                                keyword,
                                platform_cfg: &cfg,
                                task_id: Some(&task_id),
                                target_count: per_keyword_limit,
                                adapter: adapter.clone(),
                                content_tx: Some(tx.clone()),
                                existing_ids: Some(&existing_ids),
                                sort_mode: &sort_mode,
                                time_range: &time_range,
                                min_likes,
                            },
                        )
                        .await;
                    if let Err(e) = &collect_result {
                        had_error = true;
                        tracing::error!(keyword = %keyword, "采集失败: {e}");
                        emit_collect_log(
                            &app,
                            &task_id,
                            "error",
                            format!("「{keyword}」采集失败: {e}"),
                        );
                    }
                    // 关闭发送端让消费任务退出;增量入库到此完成,结果忽略(只为实时显示)
                    drop(tx);
                    let _ = consumer.await;

                    // 兜底:对 collect 返回的「全量响应」(原生拦截 + 页面 hook 合并)再整体解析一次,
                    // 补齐增量通道没覆盖的路径——run_human_rpa / 非 smart 模式不发 channel——以及评论。
                    // 与主 seen 共用去重,persist_collected 是 on_conflict upsert,幂等不会重复落库;
                    // media 以兜底全量内容为准,确保各路径都能下到素材。
                    if let Ok(responses) = collect_result {
                        if !responses.is_empty() {
                            let ctx = FetchContext {
                                keyword: keyword.clone(),
                                responses,
                            };
                            match adapter_arc.parse(&TaskKind::Search, &ctx).await {
                                Ok(mut output) => {
                                    // 兜底解析同样按最低点赞数过滤,与增量通道口径一致(缺失放行)
                                    if min_likes > 0 {
                                        output.contents.retain(|c| {
                                            c.stats
                                                .like_count
                                                .map(|likes| likes >= min_likes as i64)
                                                .unwrap_or(true)
                                        });
                                    }
                                    contents_for_media.extend(output.contents.iter().cloned());
                                    persist_collected(
                                        &db,
                                        &task_id,
                                        &owner,
                                        keyword,
                                        output,
                                        &mut seen_contents,
                                        &mut seen_comments,
                                    )
                                    .await;
                                }
                                Err(e) => {
                                    tracing::warn!(keyword = %keyword, "兜底解析失败: {e}");
                                }
                            }
                        }
                    }

                    let (c, m) = (seen_contents.len() as i64, seen_comments.len() as i64);
                    write_task_progress(&app, &db, &task_id, progress, c, m).await;
                    emit_collect_log(
                        &app,
                        &task_id,
                        "info",
                        format!("「{keyword}」采集结束,累计 {c} 内容 / {m} 评论"),
                    );
                    (c, m)
                }
                None => {
                    // 未注册适配器:collect 不会发 channel(content_tx 传 None),
                    // 退化为原「仅统计拦截响应数」分支,明细不落库
                    let responses = match bridge
                        .collect(
                            &app,
                            CollectRequest {
                                account_id: &account_id,
                                keyword,
                                platform_cfg: &cfg,
                                task_id: Some(&task_id),
                                target_count: per_keyword_limit,
                                adapter: None,
                                content_tx: None,
                                existing_ids: Some(&existing_ids),
                                sort_mode: &sort_mode,
                                time_range: &time_range,
                                min_likes,
                            },
                        )
                        .await
                    {
                        Ok(responses) => responses,
                        Err(e) => {
                            had_error = true;
                            tracing::error!(keyword = %keyword, "采集失败: {e}");
                            emit_collect_log(
                                &app,
                                &task_id,
                                "error",
                                format!("「{keyword}」采集失败: {e}"),
                            );
                            Vec::new()
                        }
                    };
                    intercepted_total += responses.len() as i64;
                    (intercepted_total, 0)
                }
            };

            write_task_progress(&app, &db, &task_id, progress, content_count, comment_count).await;
        }

        let total_contents = seen_contents.len();

        // 评论采集:开启开关且采到内容时,在素材下载之前逐视频采**一级评论**。
        // 按评论发布时间范围过滤、按单视频上限截断;复用 persist_collected 评论 upsert 分支落库。
        if collect_comments && total_contents > 0 {
            if let Some(adapter_arc) = adapter.clone() {
                // 本次采到的去重内容 ID 作为采评论目标(详情页只需 content_id=aweme_id)
                let mut id_seen: HashSet<String> = HashSet::new();
                // content_id → 采集关键词:评论日志复用内容所在关键词 HUD tab(评论不单独成 tab)
                let keyword_map: std::collections::HashMap<String, String> = {
                    use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QuerySelect};
                    use veltrix_core::db::entity::content as ce;
                    ce::Entity::find()
                        .filter(ce::Column::TaskId.eq(&task_id))
                        .select_only()
                        .column(ce::Column::ContentId)
                        .column(ce::Column::Keyword)
                        .into_tuple::<(String, String)>()
                        .all(&db)
                        .await
                        .unwrap_or_default()
                        .into_iter()
                        .collect()
                };
                // (content_id, xsec_token, title, keyword):小红书详情页需 token(存于 content.extra),抖音留空
                let video_ids: Vec<(String, String, String, String)> = contents_for_media
                    .iter()
                    .filter(|c| id_seen.insert(c.content_id.clone()))
                    // 评论数明确为 0 的内容跳过,不导航详情页抓评论(省时 + 省请求);
                    // 未知(None,适配器未解析出评论数)仍尝试,避免误跳过实际有评论的内容。
                    .filter(|c| c.stats.comment_count != Some(0))
                    .map(|c| {
                        let token = c
                            .extra
                            .get("xsec_token")
                            .and_then(|v| v.as_str())
                            .unwrap_or_default()
                            .to_string();
                        let keyword = keyword_map.get(&c.content_id).cloned().unwrap_or_default();
                        (c.content_id.clone(), token, log_content_title(c), keyword)
                    })
                    .collect();
                let cutoff = comment_time_cutoff(&comment_time_range);
                let total_videos = video_ids.len();
                let mut comment_seq: i64 = 0;
                write_task_collecting_comments(&app, &db, &task_id, total_videos as i32).await;
                emit_collect_log(
                    &app,
                    &task_id,
                    "info",
                    format!(
                        "转入评论采集 · 共 {} 个视频 · 单视频上限 {}",
                        video_ids.len(),
                        if comment_limit == 0 {
                            "不限".to_string()
                        } else {
                            comment_limit.to_string()
                        }
                    ),
                );

                for (vidx, (content_id, xsec_token, title, keyword)) in
                    video_ids.iter().enumerate()
                {
                    // 串行限速:首个不等,之后每个之间随机间隔降频(评论用更短间隔 1.5~4s)
                    if vidx > 0 {
                        tokio::time::sleep(random_comment_video_interval()).await;
                    }
                    emit_collect_log(
                        &app,
                        &task_id,
                        "info",
                        format!(
                            "正在采集第 {}/{} 个视频的评论:{title}",
                            vidx + 1,
                            total_videos
                        ),
                    );
                    match bridge
                        .collect_comments(
                            &app,
                            CommentCollectRequest {
                                account_id: &account_id,
                                content_id,
                                title,
                                xsec_token,
                                platform_cfg: &cfg,
                                task_id: Some(&task_id),
                                limit: comment_limit,
                                adapter: adapter_arc.clone(),
                                keyword,
                            },
                        )
                        .await
                    {
                        Ok(responses) if !responses.is_empty() => {
                            let ctx = FetchContext {
                                keyword: content_id.clone(),
                                responses,
                            };
                            match adapter_arc.parse(&TaskKind::Comments, &ctx).await {
                                Ok(mut output) => {
                                    // 时间范围过滤 + 单视频上限精确截断(滚动已大致控量,这里兜底精确)
                                    output.comments =
                                        filter_comments(output.comments, cutoff, comment_limit);
                                    // 逐条评论富日志:序号 + 头像 + 昵称 + 评论内容(截断)
                                    for cm in &output.comments {
                                        comment_seq += 1;
                                        let text = truncate_chars(&cm.text, 60);
                                        emit_collect_entry(
                                            &app,
                                            &task_id,
                                            format!("#{comment_seq} {}:{text}", cm.author.nickname),
                                            CollectEntry {
                                                kind: "comment".to_string(),
                                                seq: comment_seq,
                                                avatar: cm.author.avatar.clone(),
                                                nickname: cm.author.nickname.clone(),
                                                title: text,
                                                content_kind: None,
                                            },
                                        );
                                    }
                                    persist_collected(
                                        &db,
                                        &task_id,
                                        &owner,
                                        content_id,
                                        output,
                                        &mut seen_contents,
                                        &mut seen_comments,
                                    )
                                    .await;
                                }
                                Err(e) => {
                                    tracing::warn!(content_id = %content_id, "评论解析失败: {e}")
                                }
                            }
                        }
                        Ok(_) => {}
                        Err(e) => {
                            tracing::warn!(content_id = %content_id, "评论采集失败: {e}");
                            emit_collect_log(
                                &app,
                                &task_id,
                                "warn",
                                format!("视频「{title}」评论采集失败: {e}"),
                            );
                        }
                    }
                    // 逐视频回写进度(已采视频数 + 累计评论数),调度页据此刷新「评论采集中 done/total」
                    write_task_comment_progress(
                        &app,
                        &db,
                        &task_id,
                        (vidx + 1) as i32,
                        seen_comments.len() as i64,
                    )
                    .await;
                }
                emit_collect_log(
                    &app,
                    &task_id,
                    "info",
                    format!("评论采集完成 · 累计 {} 条评论", seen_comments.len()),
                );
                // 标记本批已采评论的内容(comment_collected),供细粒度状态与补偿判断
                {
                    use sea_orm::sea_query::Expr;
                    use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
                    use veltrix_core::db::entity::content as content_entity;
                    let ids: Vec<String> = video_ids
                        .iter()
                        .map(|(cid, _, _, _)| format!("{task_id}-{}-{}", cfg.id, cid))
                        .collect();
                    if !ids.is_empty() {
                        let _ = content_entity::Entity::update_many()
                            .col_expr(content_entity::Column::CommentCollected, Expr::value(true))
                            .filter(content_entity::Column::Id.is_in(ids))
                            .exec(&db)
                            .await;
                    }
                }
            }
        }

        // WebView 占用阶段(关键词采集 + 评论采集)已结束,释放同账号互斥锁;
        // 后续意向分析(LLM)与素材下载(HTTP)不占窗口,其他任务可立即用该账号开采
        drop(collect_guard);

        // 评论意向分析:开启意向开关 + 评论采集 + 意向配置完整时,对本任务评论分批调 LLM 标注。
        // 放在评论采集之后、素材下载之前(评论刚落库即分析);失败仅告警,不影响任务终态。
        let intent_ready = analyze_comment_intent
            && collect_comments
            && total_contents > 0
            && !intent_cfg.provider_id.is_empty()
            && !intent_cfg.model.is_empty();
        if intent_ready {
            write_task_analyzing(&app, &db, &task_id).await;
            analyze_comments_intent(&app, &db, &task_id, &intent_cfg).await;
            // 意向分析覆盖本任务全部评论:把已采评论的内容标记为已意向分析
            {
                use sea_orm::sea_query::Expr;
                use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
                use veltrix_core::db::entity::content as content_entity;
                let _ = content_entity::Entity::update_many()
                    .col_expr(content_entity::Column::IntentAnalyzed, Expr::value(true))
                    .filter(content_entity::Column::TaskId.eq(&task_id))
                    .filter(content_entity::Column::CommentCollected.eq(true))
                    .exec(&db)
                    .await;
            }
        }

        // 采集主体结束 → 先落终态,让任务调度页即时翻转(不再被后续素材下载拖在「运行中」):
        //   零产出且出现过采集失败 → failed;否则 completed。
        let total_comments = seen_comments.len();
        let to_download = if total_contents == 0 && had_error {
            write_task_failed(&app, &db, &task_id, "采集未获取到任何内容").await;
            emit_collect_log(
                &app,
                &task_id,
                "error",
                "任务失败 · 未采集到内容,请检查账号登录态 / 风控".to_string(),
            );
            Vec::new()
        } else {
            // 采集主体完成,但素材(封面/头像/图文/视频)可能还在下载:
            //   先过滤出真正要下载的内容 = 去重后、且库里未成功下载过的
            //   (重复内容只更新点赞/评论等统计、不重下素材);
            //   有待下载 → 先转 downloading_media,调度页显示「素材下载中 done/total」,
            //              待全部处理完(成功/失败均定)再由下载流程收尾为 completed;
            //   无待下载 → 直接 completed。
            let pending = filter_pending_media(&db, &task_id, contents_for_media).await;
            if pending.is_empty() {
                write_task_done(&app, &db, &task_id).await;
            } else {
                write_task_downloading(&app, &db, &task_id, pending.len() as i32).await;
            }
            emit_collect_log(
                &app,
                &task_id,
                "info",
                format!("采集完成,共 {total_contents} 内容 / {total_comments} 评论 · 转入素材下载"),
            );
            pending
        };

        // 素材下载(封面 / 头像 / 图文 / 视频转音频):逐条回写 media_done 进度,
        // 全部处理完后把任务从 downloading_media 收尾为 completed。to_download 为空时内部直接返回。
        download_media_for_contents(
            &app,
            &db,
            &task_id,
            &config_dir,
            &media_cfg,
            &transcription_cfg,
            ai_extract,
            to_download,
        )
        .await;

        // 采集完成后自动同步到发起者(owner)的 Obsidian vault(失败仅告警,不影响任务)
        if auto_sync_obsidian {
            sync_task_to_obsidian(&db, &task_id, &owner).await;
            emit_collect_log(&app, &task_id, "info", "已自动同步内容到 Obsidian");
        }

        // 收尾执行历史:记终态 + 本次新增量(collected_at >= 本次 started_at = 本次新增,排除重复)
        {
            use sea_orm::{
                ColumnTrait, EntityTrait, IntoActiveModel, PaginatorTrait, QueryFilter,
            };
            use veltrix_core::db::entity::{
                comment as comment_entity, content as content_entity, task as task_entity,
                task_run as run_entity,
            };
            let final_task = task_entity::Entity::find_by_id(task_id.clone())
                .one(&db)
                .await
                .ok()
                .flatten();
            let final_status = final_task
                .as_ref()
                .map(|t| t.status.clone())
                .unwrap_or_else(|| "completed".to_string());
            let final_error = final_task.and_then(|t| t.error_message);
            let content_delta = content_entity::Entity::find()
                .filter(content_entity::Column::TaskId.eq(&task_id))
                .filter(content_entity::Column::CollectedAt.gte(now))
                .count(&db)
                .await
                .unwrap_or(0) as i64;
            let comment_delta = comment_entity::Entity::find()
                .filter(comment_entity::Column::TaskId.eq(&task_id))
                .filter(comment_entity::Column::CollectedAt.gte(now))
                .count(&db)
                .await
                .unwrap_or(0) as i64;
            if let Ok(Some(run)) = run_entity::Entity::find_by_id(run_id.clone()).one(&db).await {
                let mut am = run.into_active_model();
                am.finished_at = Set(Some(Utc::now().timestamp()));
                am.status = Set(final_status);
                am.content_delta = Set(content_delta);
                am.comment_delta = Set(comment_delta);
                am.error_message = Set(final_error);
                if let Err(e) = am.update(&db).await {
                    tracing::warn!(task_id = %task_id, "收尾执行历史失败: {e}");
                }
            }
        }
    });

    Ok(())
}

/// 调度器:扫描到点的 daily / watching 任务并自动启动采集。lib.rs 后台循环每 30s 调一次。
///
/// 规则:
/// - 每日定时(daily):本地时间过了 scheduled_at(HH:mm)且今天还没跑过 → 启动;
/// - 持续监听(watching):上次结束(或启动)时间起算,间隔 watch_interval_min 分钟到点 → 再次启动;
///   从未运行过的监听任务不自动首启(由用户手动启动),手动停止(cancelled)即退出自动监听;
/// - 进行中(running/评论/分析/下载)一律跳过,与 run_task 的防重复启动一致。
pub async fn run_due_scheduled_tasks(app: &tauri::AppHandle) {
    use tauri::Manager;
    use veltrix_core::db::entity::task as task_entity;
    let state = app.state::<AppState>();
    let now = chrono::Local::now();
    let tasks = match task_entity::Entity::find()
        .filter(task_entity::Column::Archived.eq(false))
        .filter(task_entity::Column::TriggerType.is_in(["daily", "watching"]))
        .all(&state.db)
        .await
    {
        Ok(t) => t,
        Err(e) => {
            tracing::warn!("调度器扫描任务失败: {e}");
            return;
        }
    };
    for t in tasks {
        if matches!(
            t.status.as_str(),
            "running" | "collecting_comments" | "analyzing_comments" | "downloading_media"
        ) {
            continue;
        }
        let due = match t.trigger_type.as_str() {
            "daily" => daily_task_due(&t, &now),
            "watching" => t.status != "cancelled" && watching_task_due(&t, now.timestamp()),
            _ => false,
        };
        if !due {
            continue;
        }
        tracing::info!(task_id = %t.id, trigger = %t.trigger_type, "调度器自动启动任务");
        if let Err(e) = run_task(app.state::<AppState>(), app.clone(), t.id.clone()).await {
            tracing::warn!(task_id = %t.id, "调度器启动任务失败: {e}");
        }
    }
}

/// 每日定时是否到点:本地时间已过今日 HH:mm 且本日尚未启动过。
fn daily_task_due(
    t: &veltrix_core::db::entity::task::Model,
    now: &chrono::DateTime<chrono::Local>,
) -> bool {
    let Some(at) = t.scheduled_at.as_deref() else {
        return false;
    };
    let Ok(target_time) = chrono::NaiveTime::parse_from_str(at, "%H:%M") else {
        return false;
    };
    let today_target = now.date_naive().and_time(target_time);
    let chrono::LocalResult::Single(target) = today_target.and_local_timezone(chrono::Local)
    else {
        return false;
    };
    let target_ts = target.timestamp();
    if now.timestamp() < target_ts {
        return false;
    }
    // 今天已经跑过(本次启动时间晚于今日目标点)则不重复
    t.started_at.map(|s| s < target_ts).unwrap_or(true)
}

/// 持续监听是否到点:距上次结束(兜底取启动)已超过监听间隔。从未运行过不自动首启。
fn watching_task_due(t: &veltrix_core::db::entity::task::Model, now_ts: i64) -> bool {
    let Some(interval_min) = t.watch_interval_min else {
        return false;
    };
    if interval_min <= 0 {
        return false;
    }
    match t.finished_at.or(t.started_at) {
        Some(last) => now_ts - last >= interval_min as i64 * 60,
        None => false,
    }
}

/// 回写任务进度与已采内容/评论计数。查询/更新失败仅告警,不中断采集循环。
/// 任务进度/状态变更事件名。前端 listen 后就地刷新对应任务行,免等轮询。
const TASK_PROGRESS_EVENT: &str = "task-progress";

/// 进度/状态变更后向前端推送最新任务视图,前端据此就地更新该行(实时进度)。
/// emit 失败仅忽略(无前端监听时不影响采集);传引用即可满足 Serialize + Clone 约束。
fn emit_task_progress(app: &AppHandle, model: veltrix_core::db::entity::task::Model) {
    use tauri::Emitter;
    let view: crate::commands::task::TaskView = model.into();
    let _ = app.emit(TASK_PROGRESS_EVENT, &view);
}

async fn write_task_progress(
    app: &AppHandle,
    db: &DatabaseConnection,
    task_id: &str,
    progress: i32,
    content_count: i64,
    comment_count: i64,
) {
    use veltrix_core::db::entity::task as task_entity;
    match task_entity::Entity::find_by_id(task_id.to_string()).one(db).await {
        Ok(Some(m)) => {
            let mut am = m.into_active_model();
            am.progress = Set(progress);
            am.content_count = Set(content_count);
            am.comment_count = Set(comment_count);
            am.updated_at = Set(Utc::now().timestamp());
            match am.update(db).await {
                Ok(updated) => emit_task_progress(app, updated),
                Err(e) => tracing::warn!("回写任务进度失败: {e}"),
            }
        }
        Ok(None) => tracing::warn!(task_id, "回写进度时任务已不存在"),
        Err(e) => tracing::warn!("回写进度查询失败: {e}"),
    }
}

/// 语音转写间隔:1~3s 随机。ASR 调用之间串行插入,降低对厂商的请求频率。
/// 复用 pool 的廉价熵源做法(系统时间纳秒),不引额外依赖。
fn random_media_interval() -> std::time::Duration {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as u64)
        .unwrap_or(0);
    // 1000 + [0,2000) → 1000~2999ms
    std::time::Duration::from_millis(1000 + nanos % 2000)
}

/// 评论采集的视频间隔:1.5~3s 随机。串行遍历视频之间插入,降低请求频率。
fn random_comment_video_interval() -> std::time::Duration {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as u64)
        .unwrap_or(0);
    // 1500 + [0,1500) → 1500~2999ms
    std::time::Duration::from_millis(1500 + nanos % 1500)
}

/// 按字符截断(中文友好),超出 max 个字符则截断并加省略号。
fn truncate_chars(text: &str, max: usize) -> String {
    let trimmed = text.trim();
    let chars: Vec<char> = trimmed.chars().collect();
    if chars.len() <= max {
        return trimmed.to_string();
    }
    let mut out: String = chars[..max].iter().collect();
    out.push('…');
    out
}

/// 内容用于日志展示的标题:优先 title,空则用正文 desc,均空给占位;截断到 40 字。
fn log_content_title(content: &Content) -> String {
    let raw = content
        .title
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .or(content.desc.as_deref())
        .filter(|s| !s.trim().is_empty())
        .unwrap_or("(无标题)");
    truncate_chars(raw, 40)
}

/// ContentKind → 字符串标识(日志 entry 用)。
fn content_kind_label(kind: &ContentKind) -> &'static str {
    match kind {
        ContentKind::Video => "video",
        ContentKind::Image => "image",
        ContentKind::Article => "article",
        ContentKind::Unknown => "unknown",
    }
}

/// 采集落库后下载内容素材。并发处理(限 10 路、不再限速),按 content_id 去重避免重复下载;
/// 副产品失败已在 media::process_content 内部吞为告警,主素材成败回写到 contents 表。
async fn download_media_for_contents(
    app: &AppHandle,
    db: &DatabaseConnection,
    task_id: &str,
    config_dir: &PathBuf,
    media_cfg: &veltrix_core::config::MediaConfig,
    transcription_cfg: &veltrix_core::config::TranscriptionConfig,
    ai_extract: bool,
    contents: Vec<Content>,
) {
    if contents.is_empty() {
        return;
    }
    let root = crate::media::media_root(config_dir, media_cfg);
    use futures_util::StreamExt;
    // 收集视频转出的音频(content row id, mp3 路径),供素材下载结束后统一转写
    let mut audios: Vec<(String, String)> = Vec::new();
    // 跨关键词同一内容只下一次(取 owned,move 进并发任务,避免 async 闭包借用的生命周期问题)
    let mut downloaded: HashSet<String> = HashSet::new();
    let targets: Vec<Content> = contents
        .into_iter()
        .filter(|c| downloaded.insert(c.content_id.clone()))
        .collect();
    let total = targets.len();
    emit_collect_log(app, task_id, "info", format!("开始下载素材 · 共 {total} 条"));
    let mut count = 0usize;
    let mut failed = 0usize;
    // 并发下载(限 10 路并发,不再串行限速),边完成边回写结果与进度
    let root_ref = &root;
    let mut stream = futures_util::stream::iter(targets.into_iter().map(|content| async move {
        // 标题在下载前取(content 随后 move 进 process_content);用于 HUD 逐条日志展示
        let title = log_content_title(&content);
        let outcome =
            crate::media::process_content(&content, root_ref, media_cfg, ai_extract).await;
        let id = format!("{task_id}-{}-{}", content.platform, content.content_id);
        (id, title, outcome)
    }))
    .buffer_unordered(10);
    while let Some((id, title, outcome)) = stream.next().await {
        let ok = is_media_ok(&outcome);
        if !ok {
            failed += 1;
        }
        record_media_outcome(db, &id, &outcome).await;
        // 视频转出音频的,记下供采集结束后统一转写(不占采集通道)
        if let Some(audio_path) = &outcome.audio_path {
            audios.push((id.clone(), audio_path.clone()));
        }
        count += 1;
        // 逐条素材下载日志:HUD 面板可见下载过程(成功标题 / 失败原因)
        if ok {
            // 视频转音频成功时额外标注,便于看出转写素材已就绪
            let extra = if outcome.audio_extracted == Some(true) {
                " · 已转音频"
            } else {
                ""
            };
            emit_collect_log(
                app,
                task_id,
                "info",
                format!("素材 {count}/{total} · {title} · 完成{extra}"),
            );
        } else {
            let reason = outcome.error.as_deref().unwrap_or("未知原因");
            emit_collect_log(
                app,
                task_id,
                "warn",
                format!("素材 {count}/{total} · {title} · 失败:{reason}"),
            );
        }
        // 逐条回写进度,调度页据此刷新「素材下载中 done/total」
        write_task_media_done(app, db, task_id, count as i32).await;
    }
    emit_collect_log(
        app,
        task_id,
        "info",
        format!(
            "素材下载完成,共处理 {count} 条内容(失败 {failed} 条),输出目录: {}",
            root.display()
        ),
    );
    // 素材下载完成后统一做语音转写(视频音频→文案),失败仅告警不影响任务终态
    transcribe_for_contents(app, db, task_id, transcription_cfg, audios).await;
    // 素材全部处理完毕,任务从 downloading_media 收尾为 completed
    write_task_done(app, db, task_id).await;
}

/// 采集结束后统一语音转写:把每条视频转出的音频逐条调 ASR 厂商,回写 content.transcript。
/// 串行 + 限速,失败仅告警不中断;未配置/厂商不支持 ASR 则跳过。不占采集通道(主体已结束)。
async fn transcribe_for_contents(
    app: &AppHandle,
    db: &DatabaseConnection,
    task_id: &str,
    transcription_cfg: &veltrix_core::config::TranscriptionConfig,
    audios: Vec<(String, String)>,
) {
    use veltrix_core::db::entity::provider as provider_entity;
    if audios.is_empty() {
        return;
    }
    // 未配置转写厂商 / 模型 → 跳过(降级,不影响任务)
    if transcription_cfg.provider_id.trim().is_empty() || transcription_cfg.model.trim().is_empty() {
        emit_collect_log(app, task_id, "info", "未配置语音转写,跳过");
        return;
    }
    let provider = match provider_entity::Entity::find_by_id(transcription_cfg.provider_id.clone())
        .one(db)
        .await
    {
        Ok(Some(p)) => p,
        _ => {
            emit_collect_log(app, task_id, "warn", "语音转写厂商不存在,跳过");
            return;
        }
    };
    // 厂商不支持 ASR → 跳过(降级)
    if !crate::llm::provider::provider_supports_asr(&provider.code) {
        emit_collect_log(
            app,
            task_id,
            "warn",
            format!("厂商「{}」不支持语音转写,跳过", provider.name),
        );
        return;
    }

    let total = audios.len();
    emit_collect_log(app, task_id, "info", format!("开始语音转写 · 共 {total} 条"));
    let mut done = 0usize;
    for (id, audio_path) in &audios {
        // 串行限速:首条不等,之后每条之间随机间隔降频(ASR 调用较重)
        if done > 0 {
            tokio::time::sleep(random_media_interval()).await;
        }
        let result = crate::llm::transcribe(crate::llm::TranscribeRequest {
            provider_code: &provider.code,
            api_url: &provider.api_url,
            api_key: &provider.api_key,
            model: &transcription_cfg.model,
            audio_path: std::path::Path::new(audio_path),
        })
        .await;
        match result {
            Ok(text) => record_transcript(db, id, Some(text), None).await,
            Err(e) => {
                tracing::warn!(content_id = %id, "语音转写失败: {e}");
                record_transcript(db, id, None, Some(format!("{e}"))).await;
            }
        }
        done += 1;
        emit_collect_log(app, task_id, "info", format!("转写中 {done}/{total}"));
    }
    emit_collect_log(app, task_id, "info", format!("语音转写完成 · {done}/{total}"));
}

/// 回写单条内容的转写结果(只更新 transcript / transcript_error 两列,不触碰其它字段)。
async fn record_transcript(
    db: &DatabaseConnection,
    id: &str,
    text: Option<String>,
    err: Option<String>,
) {
    use veltrix_core::db::entity::content as content_entity;
    let am = content_entity::ActiveModel {
        id: Set(id.to_string()),
        transcript: Set(text),
        transcript_error: Set(err),
        ..Default::default()
    };
    if let Err(e) = am.update(db).await {
        tracing::warn!(content_id = %id, "回写转写文本失败: {e}");
    }
}

/// 加载任务已采内容的 content_id 集合:智能停止「只数新增」(重复不占目标配额)的依据。
async fn load_existing_content_ids(db: &DatabaseConnection, task_id: &str) -> HashSet<String> {
    use sea_orm::{ColumnTrait, QueryFilter, QuerySelect};
    use veltrix_core::db::entity::content as content_entity;
    content_entity::Entity::find()
        .filter(content_entity::Column::TaskId.eq(task_id))
        .select_only()
        .column(content_entity::Column::ContentId)
        .into_tuple::<String>()
        .all(db)
        .await
        .unwrap_or_else(|e| {
            tracing::warn!("加载已采内容失败: {e}");
            Vec::new()
        })
        .into_iter()
        .collect()
}

/// 过滤出真正需要下载素材的内容:按 content_id 去重,并排除库中已成功下载过的旧内容。
/// 重复内容(media_status=success)只在 persist 阶段更新统计,这里不再重复下载素材。
/// 一次性取「已成功」集合避免逐条查库(N+1)。
async fn filter_pending_media(
    db: &DatabaseConnection,
    task_id: &str,
    contents: Vec<Content>,
) -> Vec<Content> {
    use sea_orm::{ColumnTrait, QueryFilter, QuerySelect};
    use veltrix_core::db::entity::content as content_entity;
    let done: HashSet<String> = content_entity::Entity::find()
        .filter(content_entity::Column::TaskId.eq(task_id))
        .filter(content_entity::Column::MediaStatus.eq("success"))
        .select_only()
        .column(content_entity::Column::ContentId)
        .into_tuple::<String>()
        .all(db)
        .await
        .unwrap_or_default()
        .into_iter()
        .collect();
    let mut seen: HashSet<String> = HashSet::new();
    let mut pending = Vec::new();
    for c in contents {
        // 跨关键词同一内容只下一次
        if !seen.insert(c.content_id.clone()) {
            continue;
        }
        // 已成功下载过 → 媒体不重下(统计已由 persist upsert 更新)
        if done.contains(&c.content_id) {
            continue;
        }
        pending.push(c);
    }
    pending
}

/// 素材是否整体成功:主素材就绪且音频提取未失败(开启提取时)。
fn is_media_ok(outcome: &crate::media::MediaOutcome) -> bool {
    outcome.ok && outcome.audio_extracted != Some(false)
}

/// 把素材处理结果回写到 contents 表(仅更新状态相关三列,不触碰其它字段)。
async fn record_media_outcome(db: &DatabaseConnection, id: &str, outcome: &crate::media::MediaOutcome) {
    use veltrix_core::db::entity::content as content_entity;
    let status = if is_media_ok(outcome) { "success" } else { "failed" };
    let mut am = content_entity::ActiveModel {
        id: Set(id.to_string()),
        media_status: Set(Some(status.to_string())),
        audio_extracted: Set(outcome.audio_extracted),
        media_error: Set(outcome.error.clone()),
        ..Default::default()
    };
    // 下载成功才回写本地路径;失败/未下不覆盖旧值(NotSet),便于重试后保留上次成功路径
    if let Some(p) = &outcome.cover_path {
        am.cover_path = Set(Some(p.clone()));
    }
    if let Some(p) = &outcome.avatar_path {
        am.avatar_path = Set(Some(p.clone()));
    }
    // 音频路径回写:详情页播放音频用(仅视频 + 提取成功时有值)
    if let Some(p) = &outcome.audio_path {
        am.audio_path = Set(Some(p.clone()));
    }
    if let Some(v) = outcome.video_downloaded {
        am.video_downloaded = Set(Some(v));
    }
    if let Some(v) = outcome.image_total {
        am.image_total = Set(Some(v));
    }
    if let Some(v) = outcome.image_done {
        am.image_done = Set(Some(v));
    }
    if let Err(e) = am.update(db).await {
        tracing::warn!(content_id = %id, "回写素材状态失败: {e}");
    }
}

/// contents 实体 → model::Content,供失败重试时重跑素材下载。
/// 只填下载所需字段(链接/形态/作者头像),统计等无关字段走 Default。
fn content_from_model(m: &veltrix_core::db::entity::content::Model) -> Content {
    let kind = match m.kind.as_str() {
        "video" => ContentKind::Video,
        "image" => ContentKind::Image,
        "article" => ContentKind::Article,
        _ => ContentKind::Unknown,
    };
    let image_urls: Vec<String> = serde_json::from_str(&m.image_urls).unwrap_or_default();
    let avatar = serde_json::from_str::<serde_json::Value>(&m.author_json)
        .ok()
        .and_then(|v| v.get("avatar").and_then(|a| a.as_str()).map(str::to_string));
    Content {
        platform: m.platform.clone(),
        content_id: m.content_id.clone(),
        kind,
        title: m.title.clone(),
        desc: m.desc.clone(),
        author: Author {
            platform: m.platform.clone(),
            uid: m.author_uid.clone(),
            nickname: m.author_nickname.clone(),
            avatar,
            ..Default::default()
        },
        video_url: m.video_url.clone(),
        cover_url: m.cover_url.clone(),
        image_urls,
        duration: m.duration,
        ..Default::default()
    }
}

/// 单条内容素材状态视图:retry_content_media 返回最新状态,前端就地刷新该行。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaStatusView {
    pub id: String,
    pub media_status: Option<String>,
    pub audio_extracted: Option<bool>,
    pub media_error: Option<String>,
}

/// 失败重试:对单条内容重跑素材下载并回写状态。
///
/// 注意:平台视频直链多为带时效签名的 CDN 地址(douyinvod 等),过期后重试仍会 403。
/// 此时需重新发起采集刷新链接——本命令只能用库里已有链接重试,无法起死回生。
#[tauri::command]
pub async fn retry_content_media(
    state: State<'_, AppState>,
    id: String,
) -> Result<MediaStatusView> {
    use veltrix_core::db::entity::content as content_entity;
    let me = current_user(&state).ok_or_else(|| CrawlerError::Config("未登录".into()))?;
    let row = content_entity::Entity::find_by_id(id.clone())
        .one(&state.db)
        .await
        .map_err(|e| CrawlerError::Config(format!("查询内容失败: {e}")))?
        .ok_or_else(|| CrawlerError::Config("内容不存在".into()))?;
    // 数据归属:self 用户只能操作自己的内容
    if me.scope == "self" && row.owner != me.name {
        return Err(CrawlerError::Config("无权操作该内容".into()));
    }

    // clone 出媒体配置,避免跨 await 持有配置锁
    let media_cfg = { lock_config(&state)?.media.clone() };
    let root = crate::media::media_root(&state.config_dir, &media_cfg);
    // 重试遵循任务的「AI 文案提取」设置:决定视频是否下载并转音频
    let ai_extract = veltrix_core::db::entity::task::Entity::find_by_id(row.task_id.clone())
        .one(&state.db)
        .await
        .ok()
        .flatten()
        .map(|t| t.ai_extract)
        .unwrap_or(false);
    let content = content_from_model(&row);
    let outcome = crate::media::process_content(&content, &root, &media_cfg, ai_extract).await;
    record_media_outcome(&state.db, &id, &outcome).await;

    let status = if is_media_ok(&outcome) { "success" } else { "failed" };
    Ok(MediaStatusView {
        id,
        media_status: Some(status.to_string()),
        audio_extracted: outcome.audio_extracted,
        media_error: outcome.error,
    })
}

/// 失败任务补偿:对已采内容补做缺失的后处理(意向分析、素材下载+转写),按任务采集参数。
/// 仅 failed 任务;无已采内容时落 failed 并提示用「重新运行」重采。
/// 评论缺失需用「重新运行」(评论采集依赖 WebView,不在补偿范围)。
#[tauri::command]
pub async fn compensate_task(
    state: State<'_, AppState>,
    app: AppHandle,
    id: String,
) -> Result<()> {
    use veltrix_core::db::entity::task as task_entity;
    let me = current_user(&state).ok_or_else(|| CrawlerError::Config("未登录".into()))?;
    let model = task_entity::Entity::find_by_id(id.clone())
        .one(&state.db)
        .await
        .map_err(|e| CrawlerError::Config(format!("查询任务失败: {e}")))?
        .ok_or_else(|| CrawlerError::Config("任务不存在".into()))?;
    if model.status != "failed" {
        return Err(CrawlerError::Config("仅失败任务可补偿".into()));
    }
    if me.scope == "self" && model.owner != me.name {
        return Err(CrawlerError::Config("无权操作该任务".into()));
    }
    let ai_extract = model.ai_extract;
    let analyze_comment_intent = model.analyze_comment_intent;
    let media_cfg = { lock_config(&state)?.media.clone() };
    let transcription_cfg = { lock_config(&state)?.transcription.clone() };
    let intent_cfg = { lock_config(&state)?.intent.clone() };
    let config_dir = state.config_dir.clone();
    let db = state.db.clone();

    // 立即翻转为「素材下载中」,前端开始轮询;后台按采集参数补做
    write_task_downloading(&app, &db, &id, 0).await;

    tauri::async_runtime::spawn(async move {
        use sea_orm::sea_query::Expr;
        use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
        use veltrix_core::db::entity::content as content_entity;

        let rows = match content_entity::Entity::find()
            .filter(content_entity::Column::TaskId.eq(&id))
            .all(&db)
            .await
        {
            Ok(r) => r,
            Err(e) => {
                write_task_failed(&app, &db, &id, &format!("补偿查询内容失败: {e}")).await;
                return;
            }
        };
        if rows.is_empty() {
            write_task_failed(&app, &db, &id, "补偿:本任务无已采内容,请用「重新运行」重新采集")
                .await;
            return;
        }
        emit_collect_log(&app, &id, "info", format!("开始补偿 · 已采内容 {} 条", rows.len()));

        // 意向分析补做(analyze_comments_intent 内部按 intent_level IS NULL 幂等筛选)
        let intent_ready = analyze_comment_intent
            && !intent_cfg.provider_id.trim().is_empty()
            && !intent_cfg.model.trim().is_empty();
        if intent_ready {
            write_task_analyzing(&app, &db, &id).await;
            analyze_comments_intent(&app, &db, &id, &intent_cfg).await;
            let _ = content_entity::Entity::update_many()
                .col_expr(content_entity::Column::IntentAnalyzed, Expr::value(true))
                .filter(content_entity::Column::TaskId.eq(&id))
                .filter(content_entity::Column::CommentCollected.eq(true))
                .exec(&db)
                .await;
        }

        // 素材下载 + 转写补做(仅 ai_extract;filter_pending_media 排除已成功的)
        if ai_extract {
            let contents: Vec<Content> = rows.iter().map(content_from_model).collect();
            let pending = filter_pending_media(&db, &id, contents).await;
            if !pending.is_empty() {
                write_task_downloading(&app, &db, &id, pending.len() as i32).await;
                download_media_for_contents(
                    &app,
                    &db,
                    &id,
                    &config_dir,
                    &media_cfg,
                    &transcription_cfg,
                    ai_extract,
                    pending,
                )
                .await; // 内部末尾会 write_task_done
                emit_collect_log(&app, &id, "info", "补偿完成");
                return;
            }
        }
        // 无素材可补 → 直接收尾为完成
        write_task_done(&app, &db, &id).await;
        emit_collect_log(&app, &id, "info", "补偿完成");
    });
    Ok(())
}

/// ffmpeg 探测结果:供前端在「AI 文案提取」处按是否已安装切换提示——已装则隐藏下载引导。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FfmpegStatus {
    /// 是否检测到可用的 ffmpeg。
    pub available: bool,
    /// 可用时的版本信息首行(形如 "ffmpeg version ...");不可用为 None。
    pub version: Option<String>,
}

/// 检测 ffmpeg 是否可用:用配置的 ffmpeg_path(空则系统 PATH 的 `ffmpeg`)跑一次 `-version`。
/// 探测是阻塞的进程调用,挪到阻塞线程池,避免占用异步运行时工作线程。
#[tauri::command]
pub async fn check_ffmpeg(state: State<'_, AppState>) -> Result<FfmpegStatus> {
    // clone 出路径再 spawn_blocking,避免把配置锁 guard 跨 await 持有
    let ffmpeg_path = { lock_config(&state)?.media.ffmpeg_path.clone() };
    let version = tauri::async_runtime::spawn_blocking(move || {
        crate::media::probe_ffmpeg(ffmpeg_path.as_deref())
    })
    .await
    .map_err(|e| CrawlerError::Config(format!("ffmpeg 探测任务失败: {e}")))?;
    Ok(FfmpegStatus {
        available: version.is_some(),
        version,
    })
}

/// 保存当前用户的 Obsidian vault 根路径(每用户各自配置)。
#[tauri::command]
pub async fn set_obsidian_vault(state: State<'_, AppState>, vault_path: String) -> Result<()> {
    use veltrix_core::db::entity::user as user_entity;
    let me = current_user(&state).ok_or_else(|| CrawlerError::Config("未登录".into()))?;
    // 外部输入校验:vault 是后续文件写入的根,相对路径 /「..」/ 不存在的目录
    // 都可能把 Markdown 与素材写到意外位置。空值放行(表示清除配置)。
    let trimmed = vault_path.trim().to_string();
    if !trimmed.is_empty() {
        let p = Path::new(&trimmed);
        if !p.is_absolute() {
            return Err(CrawlerError::Config("vault 路径必须是绝对路径".into()));
        }
        if p.components().any(|c| matches!(c, std::path::Component::ParentDir)) {
            return Err(CrawlerError::Config("vault 路径不允许包含「..」".into()));
        }
        if !p.is_dir() {
            return Err(CrawlerError::Config("vault 路径不存在或不是目录".into()));
        }
    }
    let model = user_entity::Entity::find()
        .filter(user_entity::Column::Username.eq(&me.name))
        .one(&state.db)
        .await
        .map_err(|e| CrawlerError::Config(format!("查询用户失败: {e}")))?
        .ok_or_else(|| CrawlerError::Config("用户不存在".into()))?;
    let mut am = model.into_active_model();
    am.obsidian_vault_path = Set(trimmed);
    am.updated_at = Set(Utc::now().timestamp());
    am.update(&state.db)
        .await
        .map_err(|e| CrawlerError::Config(format!("保存 vault 失败: {e}")))?;
    Ok(())
}

/// 读取当前用户的 Obsidian vault 根路径(未配置返回空串)。
#[tauri::command]
pub async fn get_obsidian_vault(state: State<'_, AppState>) -> Result<String> {
    use veltrix_core::db::entity::user as user_entity;
    let me = current_user(&state).ok_or_else(|| CrawlerError::Config("未登录".into()))?;
    let path = user_entity::Entity::find()
        .filter(user_entity::Column::Username.eq(&me.name))
        .one(&state.db)
        .await
        .ok()
        .flatten()
        .map(|u| u.obsidian_vault_path)
        .unwrap_or_default();
    Ok(path)
}

/// 把若干内容同步到「当前用户」的 Obsidian vault:渲染 Markdown + 复制封面,并记录同步关系。
/// self scope 仅能同步自己 owner 的内容。返回成功同步的条数。
#[tauri::command]
pub async fn sync_contents_to_obsidian(
    state: State<'_, AppState>,
    ids: Vec<String>,
) -> Result<usize> {
    use sea_orm::sea_query::OnConflict;
    use veltrix_core::db::entity::{
        comment as comment_entity, content as content_entity,
        content_synced_user as csu_entity, user as user_entity,
    };
    let me = current_user(&state).ok_or_else(|| CrawlerError::Config("未登录".into()))?;
    let vault = user_entity::Entity::find()
        .filter(user_entity::Column::Username.eq(&me.name))
        .one(&state.db)
        .await
        .ok()
        .flatten()
        .map(|u| u.obsidian_vault_path)
        .unwrap_or_default();
    if vault.trim().is_empty() {
        return Err(CrawlerError::Config(
            "请先在「系统设置 → Obsidian」配置 vault 路径".into(),
        ));
    }
    let vault_path = std::path::PathBuf::from(&vault);
    let now = Utc::now().timestamp();
    let mut synced = 0usize;
    for id in &ids {
        let content = match content_entity::Entity::find_by_id(id).one(&state.db).await {
            Ok(Some(c)) => c,
            _ => continue,
        };
        if me.scope == "self" && content.owner != me.name {
            continue;
        }
        let comments = comment_entity::Entity::find()
            .filter(comment_entity::Column::TaskId.eq(&content.task_id))
            .filter(comment_entity::Column::ContentId.eq(&content.content_id))
            .all(&state.db)
            .await
            .unwrap_or_default();
        if let Err(e) = crate::obsidian::sync_one(&vault_path, &content, &comments).await {
            tracing::warn!(content_id = %id, "同步 Obsidian 失败: {e}");
            continue;
        }
        // 幂等记录「当前用户已同步该条」
        let am = csu_entity::ActiveModel {
            content_id: Set(id.clone()),
            synced_user: Set(me.name.clone()),
            synced_at: Set(now),
            vault_path: Set(vault.clone()),
        };
        let _ = csu_entity::Entity::insert(am)
            .on_conflict(
                OnConflict::columns([
                    csu_entity::Column::ContentId,
                    csu_entity::Column::SyncedUser,
                ])
                .update_columns([csu_entity::Column::SyncedAt, csu_entity::Column::VaultPath])
                .to_owned(),
            )
            .exec(&state.db)
            .await;
        synced += 1;
    }
    Ok(synced)
}

/// 自动同步:把任务全部内容同步到指定用户(owner)的 Obsidian vault,并记录同步关系。
/// 失败仅告警不中断;owner 未配 vault 则直接跳过。
async fn sync_task_to_obsidian(db: &DatabaseConnection, task_id: &str, owner: &str) {
    use sea_orm::sea_query::OnConflict;
    use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
    use veltrix_core::db::entity::{
        comment as comment_entity, content as content_entity,
        content_synced_user as csu_entity, user as user_entity,
    };
    let vault = match user_entity::Entity::find()
        .filter(user_entity::Column::Username.eq(owner))
        .one(db)
        .await
    {
        Ok(Some(u)) => u.obsidian_vault_path,
        _ => return,
    };
    if vault.trim().is_empty() {
        return;
    }
    let vault_path = std::path::PathBuf::from(&vault);
    let now = Utc::now().timestamp();
    let rows = match content_entity::Entity::find()
        .filter(content_entity::Column::TaskId.eq(task_id))
        .all(db)
        .await
    {
        Ok(r) => r,
        Err(_) => return,
    };
    for content in &rows {
        let comments = comment_entity::Entity::find()
            .filter(comment_entity::Column::TaskId.eq(task_id))
            .filter(comment_entity::Column::ContentId.eq(&content.content_id))
            .all(db)
            .await
            .unwrap_or_default();
        if crate::obsidian::sync_one(&vault_path, content, &comments)
            .await
            .is_err()
        {
            continue;
        }
        let am = csu_entity::ActiveModel {
            content_id: Set(content.id.clone()),
            synced_user: Set(owner.to_string()),
            synced_at: Set(now),
            vault_path: Set(vault.clone()),
        };
        let _ = csu_entity::Entity::insert(am)
            .on_conflict(
                OnConflict::columns([
                    csu_entity::Column::ContentId,
                    csu_entity::Column::SyncedUser,
                ])
                .update_columns([csu_entity::Column::SyncedAt, csu_entity::Column::VaultPath])
                .to_owned(),
            )
            .exec(db)
            .await;
    }
}

/// 把适配器解析出的内容/评论落库。调用方维护跨关键词去重集合,
/// 避免同任务多关键词命中同一条造成主键冲突。落库失败仅告警,不中断采集。
async fn persist_collected(
    db: &DatabaseConnection,
    task_id: &str,
    owner: &str,
    keyword: &str,
    output: FetchOutput,
    seen_contents: &mut HashSet<String>,
    seen_comments: &mut HashSet<String>,
) {
    use veltrix_core::db::entity::{
        author as author_entity, comment as comment_entity, content as content_entity,
    };

    let contents: Vec<content_entity::ActiveModel> = output
        .contents
        .iter()
        .filter_map(|c| {
            let id = format!("{task_id}-{}-{}", c.platform, c.content_id);
            // insert 返回 false 表示已存在(本任务已落过),跳过
            if !seen_contents.insert(id.clone()) {
                return None;
            }
            Some(content_to_active(id, c, task_id, keyword, owner))
        })
        .collect();
    if !contents.is_empty() {
        // 判重 upsert:主键(task-平台-内容)已存在时刷新会随时间变化的字段(互动数 + 标题/文案),
        // 不重复插入。标题/文案也可能被作者编辑,一并刷新避免漂移。
        let on_conflict = sea_orm::sea_query::OnConflict::column(content_entity::Column::Id)
            .update_columns([
                content_entity::Column::LikeCount,
                content_entity::Column::CommentCount,
                content_entity::Column::CollectCount,
                content_entity::Column::ShareCount,
                content_entity::Column::PlayCount,
                content_entity::Column::Title,
                content_entity::Column::Desc,
                // 不刷新 collected_at:保留首次采集时间,使采集明细「collected_at >= 本次 started_at」
                // 恰好只统计本次新增内容,重复采到的已有内容(首次时间早)被排除。
            ])
            .to_owned();
        if let Err(e) = content_entity::Entity::insert_many(contents)
            .on_conflict(on_conflict)
            .exec(db)
            .await
        {
            tracing::warn!("落库采集内容失败: {e}");
        }
    }

    let comments: Vec<comment_entity::ActiveModel> = output
        .comments
        .iter()
        .filter_map(|c| {
            let id = format!("{task_id}-{}-{}", c.platform, c.comment_id);
            if !seen_comments.insert(id.clone()) {
                return None;
            }
            Some(comment_to_active(id, c, task_id, owner))
        })
        .collect();
    if !comments.is_empty() {
        // 评论同样判重 upsert:已存在时刷新点赞 / 回复数
        let on_conflict = sea_orm::sea_query::OnConflict::column(comment_entity::Column::Id)
            .update_columns([
                comment_entity::Column::LikeCount,
                comment_entity::Column::ReplyCount,
                // 同 content:不刷新 collected_at,采集明细只统计本次新增、排除重复采到的已有评论
            ])
            .to_owned();
        if let Err(e) = comment_entity::Entity::insert_many(comments)
            .on_conflict(on_conflict)
            .exec(db)
            .await
        {
            tracing::warn!("落库采集评论失败: {e}");
        }
    }

    // 作者档案:7 天节流。新作者建档;已有作者距上次采集超过 7 天才刷新画像(粉丝/获赞/签名等),
    // 7 天内不动,避免每次采集都写库。first_collected_at 与 is_monitored 始终保留。
    const AUTHOR_REFRESH_SECS: i64 = 7 * 24 * 3600;
    let now = Utc::now().timestamp();
    let mut seen_authors: HashSet<String> = HashSet::new();
    for c in &output.contents {
        let a = &c.author;
        if a.uid.is_empty() {
            continue;
        }
        let aid = format!("{owner}-{}-{}", a.platform, a.uid);
        // 同批同作者只处理一次
        if !seen_authors.insert(aid.clone()) {
            continue;
        }
        let existing = author_entity::Entity::find_by_id(aid.clone())
            .one(db)
            .await
            .ok()
            .flatten();
        match existing {
            None => {
                if let Err(e) = author_to_active(&aid, a, owner, now).insert(db).await {
                    tracing::warn!("建作者档案失败: {e}");
                }
            }
            Some(m) if now - m.last_collected_at >= AUTHOR_REFRESH_SECS => {
                // 超 7 天:用最新采集刷新画像,保留建档时间与监控开关
                let fresh = author_to_active(&aid, a, owner, now);
                let mut am = m.into_active_model();
                am.nickname = fresh.nickname;
                am.avatar = fresh.avatar;
                am.platform_id = fresh.platform_id;
                am.short_id = fresh.short_id;
                am.signature = fresh.signature;
                am.follower_count = fresh.follower_count;
                am.following_count = fresh.following_count;
                am.total_favorited = fresh.total_favorited;
                am.location = fresh.location;
                am.last_collected_at = Set(now);
                if let Err(e) = am.update(db).await {
                    tracing::warn!("刷新作者档案失败: {e}");
                }
            }
            Some(_) => { /* 7 天内,跳过不更新 */ }
        }
    }
}

/// model::Author → authors 表 ActiveModel。平台号/属地/获赞从 author.extra 取(各适配器按需填)。
fn author_to_active(
    id: &str,
    a: &crate::model::Author,
    owner: &str,
    now: i64,
) -> veltrix_core::db::entity::author::ActiveModel {
    let extra_str = |key: &str| {
        a.extra
            .get(key)
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    let extra_i64 = |key: &str| a.extra.get(key).and_then(|v| v.as_i64());
    veltrix_core::db::entity::author::ActiveModel {
        id: Set(id.to_string()),
        owner: Set(owner.to_string()),
        platform: Set(a.platform.clone()),
        uid: Set(a.uid.clone()),
        nickname: Set(a.nickname.clone()),
        avatar: Set(a.avatar.clone()),
        platform_id: Set(extra_str("unique_id")),
        short_id: Set(extra_str("uid")),
        signature: Set(a.signature.clone()),
        follower_count: Set(a.follower_count),
        following_count: Set(a.following_count),
        total_favorited: Set(extra_i64("total_favorited")),
        location: Set(extra_str("ip_location")),
        is_monitored: Set(false),
        first_collected_at: Set(now),
        last_collected_at: Set(now),
    }
}

/// 把可序列化值转 JSON 文本;失败回退 "null",不让单条脏字段中断整批落库。
fn to_json_text<T: Serialize>(value: &T) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "null".to_string())
}

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

/// model::Content → contents 实体 ActiveModel。复合字段(作者/图片/扩展)序列化为 JSON 文本。
fn content_to_active(
    id: String,
    c: &Content,
    task_id: &str,
    keyword: &str,
    owner: &str,
) -> veltrix_core::db::entity::content::ActiveModel {
    use veltrix_core::db::entity::content as content_entity;
    let kind = match c.kind {
        ContentKind::Video => "video",
        ContentKind::Image => "image",
        ContentKind::Article => "article",
        ContentKind::Unknown => "unknown",
    };
    content_entity::ActiveModel {
        id: Set(id),
        task_id: Set(task_id.to_string()),
        platform: Set(c.platform.clone()),
        content_id: Set(c.content_id.clone()),
        keyword: Set(keyword.to_string()),
        kind: Set(kind.to_string()),
        title: Set(c.title.clone()),
        desc: Set(c.desc.clone()),
        author_uid: Set(c.author.uid.clone()),
        author_nickname: Set(c.author.nickname.clone()),
        author_json: Set(to_json_text(&c.author)),
        like_count: Set(c.stats.like_count),
        comment_count: Set(c.stats.comment_count),
        collect_count: Set(c.stats.collect_count),
        share_count: Set(c.stats.share_count),
        play_count: Set(c.stats.play_count),
        published_at: Set(c.published_at),
        video_url: Set(c.video_url.clone()),
        cover_url: Set(c.cover_url.clone()),
        image_urls: Set(to_json_text(&c.image_urls)),
        duration: Set(c.duration),
        topics: Set(to_json_text(&c.topics)),
        extra: Set(to_json_text(&c.extra)),
        owner: Set(owner.to_string()),
        collected_at: Set(c.collected_at),
        // 初始置「待处理」,素材下载完成后由 record_media_outcome 回写成败
        media_status: Set(Some("pending".to_string())),
        audio_extracted: Set(None),
        media_error: Set(None),
        // 本地素材路径采集时未知,素材下载成功后回写
        cover_path: Set(None),
        avatar_path: Set(None),
        audio_path: Set(None),
        // 转写文本采集时未知,语音转写后回写
        transcript: Set(None),
        transcript_error: Set(None),
        // 细粒度处理状态:媒体下载/评论采集/意向分析后回写
        video_downloaded: Set(None),
        image_total: Set(None),
        image_done: Set(None),
        comment_collected: Set(None),
        intent_analyzed: Set(None),
    }
}

/// model::Comment → comments 实体 ActiveModel。
fn comment_to_active(
    id: String,
    c: &Comment,
    task_id: &str,
    owner: &str,
) -> veltrix_core::db::entity::comment::ActiveModel {
    use veltrix_core::db::entity::comment as comment_entity;
    comment_entity::ActiveModel {
        id: Set(id),
        task_id: Set(task_id.to_string()),
        platform: Set(c.platform.clone()),
        content_id: Set(c.content_id.clone()),
        comment_id: Set(c.comment_id.clone()),
        parent_id: Set(c.parent_id.clone()),
        author_uid: Set(c.author.uid.clone()),
        author_nickname: Set(c.author.nickname.clone()),
        author_json: Set(to_json_text(&c.author)),
        text: Set(c.text.clone()),
        like_count: Set(c.like_count),
        reply_count: Set(c.reply_count),
        created_at: Set(c.created_at),
        owner: Set(owner.to_string()),
        collected_at: Set(c.collected_at),
        // 新采集评论尚未分析,意向字段留空,待意向分析阶段回写
        intent_level: Set(None),
        intent_reason: Set(None),
    }
}

/// 标记任务完成(status=completed, progress=100, finished_at)。
async fn write_task_done(app: &AppHandle, db: &DatabaseConnection, task_id: &str) {
    use veltrix_core::db::entity::task as task_entity;
    if let Ok(Some(m)) = task_entity::Entity::find_by_id(task_id.to_string()).one(db).await {
        let now = Utc::now().timestamp();
        let mut am = m.into_active_model();
        am.status = Set("completed".to_string());
        am.progress = Set(100);
        am.finished_at = Set(Some(now));
        am.updated_at = Set(now);
        match am.update(db).await {
            Ok(updated) => emit_task_progress(app, updated),
            Err(e) => tracing::warn!("标记任务完成失败: {e}"),
        }
    }
}

/// 评论发布时间范围转 Unix 秒下限;`any` / 空 / 未知值返回 None(不按时间过滤)。
fn comment_time_cutoff(range: &str) -> Option<i64> {
    let days: i64 = match range {
        "3d" => 3,
        "7d" => 7,
        "14d" => 14,
        _ => return None,
    };
    Some(Utc::now().timestamp() - days * 24 * 3600)
}

/// 过滤评论:按发布时间下限保留 + 按单视频上限截断。
/// cutoff=None 不按时间过滤;limit=0 不截断;created_at 缺失的评论保留(不因无时间而误删)。
fn filter_comments(mut comments: Vec<Comment>, cutoff: Option<i64>, limit: usize) -> Vec<Comment> {
    if let Some(min_ts) = cutoff {
        comments.retain(|c| c.created_at.map(|t| t >= min_ts).unwrap_or(true));
    }
    if limit > 0 && comments.len() > limit {
        comments.truncate(limit);
    }
    comments
}

/// 标记任务进入评论采集态(status=collecting_comments,记录待采视频总数,清零已采)。
/// 内容采集已结束但评论未采完时调用,调度页据此显示「评论采集中 done/total」;不写 finished_at。
async fn write_task_collecting_comments(
    app: &AppHandle,
    db: &DatabaseConnection,
    task_id: &str,
    video_total: i32,
) {
    use veltrix_core::db::entity::task as task_entity;
    if let Ok(Some(m)) = task_entity::Entity::find_by_id(task_id.to_string()).one(db).await {
        let now = Utc::now().timestamp();
        let mut am = m.into_active_model();
        am.status = Set("collecting_comments".to_string());
        am.comment_video_total = Set(video_total);
        am.comment_video_done = Set(0);
        am.updated_at = Set(now);
        match am.update(db).await {
            Ok(updated) => emit_task_progress(app, updated),
            Err(e) => tracing::warn!("标记任务评论采集态失败: {e}"),
        }
    }
}

/// 回写评论采集进度(已采视频数 comment_video_done + 累计评论数 comment_count)。失败仅告警。
async fn write_task_comment_progress(
    app: &AppHandle,
    db: &DatabaseConnection,
    task_id: &str,
    video_done: i32,
    comment_count: i64,
) {
    use veltrix_core::db::entity::task as task_entity;
    if let Ok(Some(m)) = task_entity::Entity::find_by_id(task_id.to_string()).one(db).await {
        let mut am = m.into_active_model();
        am.comment_video_done = Set(video_done);
        am.comment_count = Set(comment_count);
        am.updated_at = Set(Utc::now().timestamp());
        match am.update(db).await {
            Ok(updated) => emit_task_progress(app, updated),
            Err(e) => tracing::warn!("回写评论采集进度失败: {e}"),
        }
    }
}

/// 标记任务进入意向分析态(status=analyzing_comments)。不写 finished_at。
async fn write_task_analyzing(app: &AppHandle, db: &DatabaseConnection, task_id: &str) {
    use veltrix_core::db::entity::task as task_entity;
    if let Ok(Some(m)) = task_entity::Entity::find_by_id(task_id.to_string()).one(db).await {
        let mut am = m.into_active_model();
        am.status = Set("analyzing_comments".to_string());
        am.updated_at = Set(Utc::now().timestamp());
        match am.update(db).await {
            Ok(updated) => emit_task_progress(app, updated),
            Err(e) => tracing::warn!("标记任务意向分析态失败: {e}"),
        }
    }
}

/// 对任务评论分批做意向分析并写回 comment.intent_*。读 intent 配置引用的厂商 / 提示词,
/// 按 batch_size 分批调 LLM。任一环节失败仅告警,不影响任务终态。
async fn analyze_comments_intent(
    app: &AppHandle,
    db: &DatabaseConnection,
    task_id: &str,
    intent_cfg: &veltrix_core::config::CommentIntentConfig,
) {
    use veltrix_core::db::entity::{
        comment as comment_entity, prompt as prompt_entity, provider as provider_entity,
    };

    // 厂商(api_url / api_key);不存在直接跳过
    let provider = match provider_entity::Entity::find_by_id(intent_cfg.provider_id.clone())
        .one(db)
        .await
    {
        Ok(Some(p)) => p,
        _ => {
            emit_collect_log(app, task_id, "warn", "意向分析厂商不存在,跳过");
            return;
        }
    };
    // 提示词(可选;未配置 / 为空则用内置默认)
    let configured_prompt = match prompt_entity::Entity::find_by_id(intent_cfg.prompt_id.clone())
        .one(db)
        .await
    {
        Ok(Some(p)) => p.content,
        _ => String::new(),
    };
    let system_prompt = if configured_prompt.trim().is_empty() {
        "你是评论意向分析助手,判断每条评论作者的购买 / 咨询 / 合作意向强度。".to_string()
    } else {
        configured_prompt
    };

    // 取本任务尚未分析(intent_level 为空)的评论
    let rows = match comment_entity::Entity::find()
        .filter(comment_entity::Column::TaskId.eq(task_id))
        .filter(comment_entity::Column::IntentLevel.is_null())
        .all(db)
        .await
    {
        Ok(r) => r,
        Err(e) => {
            emit_collect_log(app, task_id, "warn", format!("查询待分析评论失败: {e}"));
            return;
        }
    };
    if rows.is_empty() {
        emit_collect_log(app, task_id, "info", "无待分析评论");
        return;
    }

    let batch_size = if intent_cfg.batch_size > 0 {
        intent_cfg.batch_size as usize
    } else {
        20
    };
    let total = rows.len();
    emit_collect_log(
        app,
        task_id,
        "info",
        format!("开始意向分析 · 共 {total} 条 · 批大小 {batch_size}"),
    );

    let mut analyzed = 0usize;
    for chunk in rows.chunks(batch_size) {
        let batch: Vec<(String, String)> = chunk
            .iter()
            .map(|c| (c.comment_id.clone(), c.text.clone()))
            .collect();
        let verdicts = match crate::llm::analyze_intent(crate::llm::IntentRequest {
            api_url: &provider.api_url,
            api_key: &provider.api_key,
            model: &intent_cfg.model,
            system_prompt: &system_prompt,
            comments: &batch,
        })
        .await
        {
            Ok(v) => v,
            Err(e) => {
                emit_collect_log(app, task_id, "warn", format!("意向分析批次失败: {e}"));
                continue;
            }
        };
        // 按 comment_id 对齐回写(模型可能错序 / 少返回)
        let verdict_map: std::collections::HashMap<String, crate::llm::IntentVerdict> = verdicts
            .into_iter()
            .map(|v| (v.comment_id.clone(), v))
            .collect();
        for c in chunk {
            if let Some(v) = verdict_map.get(&c.comment_id) {
                let am = comment_entity::ActiveModel {
                    id: Set(c.id.clone()),
                    intent_level: Set(Some(v.level.clone())),
                    intent_reason: Set(Some(v.reason.clone())),
                    ..Default::default()
                };
                if let Err(e) = am.update(db).await {
                    tracing::warn!(comment_id = %c.comment_id, "回写意向失败: {e}");
                }
            }
        }
        analyzed += chunk.len();
        emit_collect_log(app, task_id, "info", format!("意向分析进度 {analyzed}/{total}"));
    }
    emit_collect_log(
        app,
        task_id,
        "info",
        format!("意向分析完成 · 已处理 {analyzed} 条"),
    );
}

/// 标记任务进入素材下载态(status=downloading_media, progress=100, 记录素材总数,清零已处理数)。
/// 采集主体已结束但素材未下完时调用,调度页据此显示「素材下载中 done/total」;不写 finished_at。
async fn write_task_downloading(
    app: &AppHandle,
    db: &DatabaseConnection,
    task_id: &str,
    media_total: i32,
) {
    use veltrix_core::db::entity::task as task_entity;
    if let Ok(Some(m)) = task_entity::Entity::find_by_id(task_id.to_string()).one(db).await {
        let now = Utc::now().timestamp();
        let mut am = m.into_active_model();
        am.status = Set("downloading_media".to_string());
        am.progress = Set(100);
        am.media_total = Set(media_total);
        am.media_done = Set(0);
        am.updated_at = Set(now);
        match am.update(db).await {
            Ok(updated) => emit_task_progress(app, updated),
            Err(e) => tracing::warn!("标记任务素材下载态失败: {e}"),
        }
    }
}

/// 回写素材下载进度(仅更新 media_done)。失败仅告警,不中断下载循环。
async fn write_task_media_done(
    app: &AppHandle,
    db: &DatabaseConnection,
    task_id: &str,
    media_done: i32,
) {
    use veltrix_core::db::entity::task as task_entity;
    if let Ok(Some(m)) = task_entity::Entity::find_by_id(task_id.to_string()).one(db).await {
        let mut am = m.into_active_model();
        am.media_done = Set(media_done);
        am.updated_at = Set(Utc::now().timestamp());
        match am.update(db).await {
            Ok(updated) => emit_task_progress(app, updated),
            Err(e) => tracing::warn!("回写素材进度失败: {e}"),
        }
    }
}

/// 标记任务失败(status=failed, finished_at, error_message)。
/// 采集零产出且过程出错时调用,避免失败任务被误标「已完成」。
async fn write_task_failed(app: &AppHandle, db: &DatabaseConnection, task_id: &str, message: &str) {
    use veltrix_core::db::entity::task as task_entity;
    if let Ok(Some(m)) = task_entity::Entity::find_by_id(task_id.to_string()).one(db).await {
        let now = Utc::now().timestamp();
        let mut am = m.into_active_model();
        am.status = Set("failed".to_string());
        am.finished_at = Set(Some(now));
        am.error_message = Set(Some(message.to_string()));
        am.updated_at = Set(now);
        match am.update(db).await {
            Ok(updated) => emit_task_progress(app, updated),
            Err(e) => tracing::warn!("标记任务失败状态失败: {e}"),
        }
    }
}
