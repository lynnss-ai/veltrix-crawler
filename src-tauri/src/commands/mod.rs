//! 前端可调用的 Tauri IPC 命令。
//!
//! 阶段0 提供平台管理;阶段1 追加账号管理与签名回调;后续追加用户/系统配置 CRUD(admin)。

pub mod admin;
pub mod cloud;
pub mod task;

use veltrix_core::config::{AppConfig, PlatformConfig};
use crate::cookie::{Account, AccountStatus, CookiePool};
use veltrix_core::error::{CrawlerError, Result};
use crate::adapter::{FetchContext, FetchOutput};
use crate::model::{Comment, Content, ContentKind, TaskKind};
use crate::webview::pool::{CollectBridge, CollectRequest, WebviewPool};
use crate::webview::{emit_collect_log, CollectControl, InterceptChannel, RpaChannel, RpaOutcome};
use chrono::Utc;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DatabaseBackend, DatabaseConnection,
    EntityTrait, IntoActiveModel, QueryFilter, Set, Statement,
};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::PathBuf;
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
                status: Set(AccountStatus::Active.as_str().to_string()),
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

/// 打开某账号的可见登录窗口,用户在其中扫码 / 输入完成登录。
/// 登录态写入该账号独立的 WebView 数据目录,采集时复用同窗口即带登录态。
#[tauri::command]
pub fn open_login_window(
    state: State<'_, AppState>,
    app: AppHandle,
    platform: String,
    account_id: String,
) -> Result<()> {
    let cfg = lock_config(&state)?;
    let pcfg = cfg.platform(&platform)?;
    state.webviews.open_login(&app, &platform, &account_id, pcfg)?;
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

    // 每关键词目标数量:作为滚动「按量停止」的依据(<=0 视为不限,退回固定轮数盲滚)
    let per_keyword_limit = model.per_keyword_limit.max(0) as usize;

    // 先标记 running + started_at,前端立即看到状态翻转
    let now = Utc::now().timestamp();
    let mut am = model.into_active_model();
    am.status = Set("running".to_string());
    am.started_at = Set(Some(now));
    am.progress = Set(0);
    am.updated_at = Set(now);
    am.update(&state.db)
        .await
        .map_err(|e| CrawlerError::Config(format!("更新任务状态失败: {e}")))?;

    // 后台采集,不阻塞命令返回。句柄均为 Clone/Arc,可安全 move 进 spawn
    let db = state.db.clone();
    let registry = state.registry.clone();
    let bridge = CollectBridge::new(
        state.webviews.clone(),
        state.intercept_channel.clone(),
        state.rpa_channel.clone(),
        state.collect_control.clone(),
    );
    tauri::async_runtime::spawn(async move {
        // 不再重采前清空:落库改为「判重 upsert」——已存在的内容只更新赞/评/藏计数,
        // 新内容插入;这样重复采集会刷新热度而非重复插入,也保留历史。

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
        // 本次任务解析出的全部内容,采集主流程结束后统一下载素材(去重后避免重复下载)
        let mut contents_for_media: Vec<Content> = Vec::new();

        for (idx, keyword) in keywords.iter().enumerate() {
            let progress = (((idx + 1) as f64 / total as f64) * 100.0) as i32;

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
                    let consumer = tauri::async_runtime::spawn(async move {
                        let mut seen_c_local: HashSet<String> = HashSet::new();
                        let mut seen_m_local: HashSet<String> = HashSet::new();
                        while let Some(batch) = rx.recv().await {
                            if batch.is_empty() {
                                continue;
                            }
                            let output = FetchOutput {
                                contents: batch,
                                comments: Vec::new(),
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
                            write_task_progress(&db_c, &task_id_c, progress, c, 0).await;
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
                            },
                        )
                        .await;
                    if let Err(e) = &collect_result {
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
                                Ok(output) => {
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
                    write_task_progress(&db, &task_id, progress, c, m).await;
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
                            },
                        )
                        .await
                    {
                        Ok(responses) => responses,
                        Err(e) => {
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

            write_task_progress(&db, &task_id, progress, content_count, comment_count).await;
        }

        // 采集落库后统一下载素材:封面 / 头像 / 图文图片 / 视频转音频。
        // 放主流程末尾顺序下载,避免穿插阻塞下一个关键词的采集节奏;按 content_id 去重防重复下载。
        download_media_for_contents(&app, &task_id, &config_dir, &media_cfg, contents_for_media)
            .await;

        write_task_done(&db, &task_id).await;
        emit_collect_log(
            &app,
            &task_id,
            "info",
            format!(
                "任务完成,共 {} 内容 / {} 评论",
                seen_contents.len(),
                seen_comments.len()
            ),
        );
    });

    Ok(())
}

/// 回写任务进度与已采内容/评论计数。查询/更新失败仅告警,不中断采集循环。
async fn write_task_progress(
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
            if let Err(e) = am.update(db).await {
                tracing::warn!("回写任务进度失败: {e}");
            }
        }
        Ok(None) => tracing::warn!(task_id, "回写进度时任务已不存在"),
        Err(e) => tracing::warn!("回写进度查询失败: {e}"),
    }
}

/// 采集落库后下载内容素材。按 content_id 去重避免重复下载;
/// 单条失败已在 media::process_content 内部吞为告警,这里只负责遍历与日志汇总。
async fn download_media_for_contents(
    app: &AppHandle,
    task_id: &str,
    config_dir: &PathBuf,
    media_cfg: &veltrix_core::config::MediaConfig,
    contents: Vec<Content>,
) {
    if contents.is_empty() {
        return;
    }
    let root = crate::media::media_root(config_dir, media_cfg);
    let mut downloaded: HashSet<String> = HashSet::new();
    let mut count = 0usize;
    for content in &contents {
        // 跨关键词同一内容只下一次
        if !downloaded.insert(content.content_id.clone()) {
            continue;
        }
        crate::media::process_content(content, &root, media_cfg).await;
        count += 1;
    }
    emit_collect_log(
        app,
        task_id,
        "info",
        format!("素材下载完成,共处理 {count} 条内容,输出目录: {}", root.display()),
    );
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
    use veltrix_core::db::entity::{comment as comment_entity, content as content_entity};

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
}

/// 把可序列化值转 JSON 文本;失败回退 "null",不让单条脏字段中断整批落库。
fn to_json_text<T: Serialize>(value: &T) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "null".to_string())
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
    }
}

/// 标记任务完成(status=completed, progress=100, finished_at)。
async fn write_task_done(db: &DatabaseConnection, task_id: &str) {
    use veltrix_core::db::entity::task as task_entity;
    if let Ok(Some(m)) = task_entity::Entity::find_by_id(task_id.to_string()).one(db).await {
        let now = Utc::now().timestamp();
        let mut am = m.into_active_model();
        am.status = Set("completed".to_string());
        am.progress = Set(100);
        am.finished_at = Set(Some(now));
        am.updated_at = Set(now);
        if let Err(e) = am.update(db).await {
            tracing::warn!("标记任务完成失败: {e}");
        }
    }
}
