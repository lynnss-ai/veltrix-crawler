//! 前端可调用的 Tauri IPC 命令。
//!
//! 阶段0 提供平台管理;阶段1 追加账号管理与签名回调;后续追加用户/系统配置 CRUD(admin)。

pub mod admin;
pub mod billing;
pub mod cloud;
pub mod collect;
pub mod creation;
pub mod dashboard;
pub mod task;
// 再导出采集执行引擎的全部命令与类型,保持 commands::X 路径不变(lib.rs invoke_handler 依赖)。
pub use collect::*;

use veltrix_core::config::{AppConfig, PlatformConfig};
use crate::cookie::{Account, AccountStatus, CookiePool};
use veltrix_core::error::{CrawlerError, Result};
use crate::webview::pool::WebviewPool;
use crate::webview::{CollectControl, InterceptChannel, RpaChannel};
use chrono::Utc;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DatabaseBackend, DatabaseConnection,
    EntityTrait, IntoActiveModel, QueryFilter, Set, Statement,
};
use serde::{Deserialize, Serialize};
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
    /// 全局采集并发闸:限制同时占用 WebView 窗口的采集任务数。调度器同点拉起多个 daily/watching
    /// 任务时,超出名额的在此排队,避免一次性弹出过多窗口耗尽 CPU / 内存 / 带宽并加剧风控。
    pub collect_semaphore: Arc<tokio::sync::Semaphore>,
    /// account_id → 登录窗口内自检回传的最近登录态结论("in" / "out")。
    /// 登录窗口关闭时据此决定终态:最近为 "out"(仍明确未登录)→ invalid;其余 → 乐观 active。
    pub login_verdicts: Arc<Mutex<std::collections::HashMap<String, String>>>,
    /// 编程 Agent 的常驻开发服务器状态(预览-开发服务器模式)。
    pub dev_server: Arc<Mutex<crate::agent::coding::commands::DevServer>>,
    /// 沙盒容器「就绪结论」缓存:避免每个编程动作都重跑一串 docker 探测(慢且放大挂死面)。
    pub sandbox_ready: Arc<Mutex<crate::agent::coding::commands::SandboxReady>>,
    /// 应用句柄:供后台 / 非命令上下文(如 resolve_exec 回退本机时)向前端推送事件(弹窗等)。
    pub app_handle: AppHandle,
    /// 自主续航编程 Agent 的「请求停止」会话集合(stop_coding_agent 写入,续航循环每步消费)。
    pub agent_cancel: Arc<Mutex<std::collections::HashSet<String>>>,
    /// 对话 Agent 流式输出的取消标志(stop_chat_agent 写入,流式循环检查)。
    pub chat_cancel_flags: Arc<Mutex<std::collections::HashMap<String, Arc<std::sync::atomic::AtomicBool>>>>,
    /// Agent 危险操作「暂停 — 等用户确认」通道(ReAct 循环命中危险工具时等待,前端
    /// `resolve_agent_confirm` 回执)。
    pub agent_confirm: Arc<crate::agent::core::shared::AgentConfirmChannel>,
    /// 电脑操作 Agent 的屏幕录制状态(同一时刻仅一个录制会话)。
    pub recording: crate::agent::computer::recorder::RecordingState,
}

/// 全局同时进行的采集任务数上限(占用 WebView 窗口的阶段)。取 3:兼顾吞吐与资源占用,
/// 不同账号 / 平台仍可并行,但不会因调度同点拉起一堆任务而一次性弹出过多窗口。
pub const MAX_CONCURRENT_COLLECT: usize = 3;

/// 取某「平台-账号」的采集互斥锁(惰性创建)。外层 std Mutex 仅做表查找,绝不跨 await 持有。
#[allow(clippy::type_complexity)]
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

pub(crate) fn lock_config(state: &AppState) -> Result<std::sync::MutexGuard<'_, AppConfig>> {
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

/// 读取某 agent(coding/computer/rpa)的用户自定义附加规范文本(无则空串)。供设置页回填。
#[tauri::command]
pub async fn get_agent_guidelines(state: State<'_, AppState>, kind: String) -> Result<String> {
    use crate::agent::core::shared::{is_valid_guidelines_kind, load_agent_guidelines};
    if !is_valid_guidelines_kind(&kind) {
        return Err(CrawlerError::Config("无效的 agent 类型".into()));
    }
    Ok(load_agent_guidelines(&state.config_dir, &kind)
        .await
        .unwrap_or_default())
}

/// 保存某 agent 的用户自定义附加规范(写入 <config_dir>/agent-guidelines/<kind>.md;空串=清空)。
/// 下一轮该 agent 对话即注入生效,无需重启。
#[tauri::command]
pub async fn set_agent_guidelines(
    state: State<'_, AppState>,
    kind: String,
    text: String,
) -> Result<()> {
    use crate::agent::core::shared::{agent_guidelines_path, is_valid_guidelines_kind};
    if !is_valid_guidelines_kind(&kind) {
        return Err(CrawlerError::Config("无效的 agent 类型".into()));
    }
    let path = agent_guidelines_path(&state.config_dir, &kind);
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|e| CrawlerError::Config(format!("创建规范目录失败: {e}")))?;
    }
    tokio::fs::write(&path, text)
        .await
        .map_err(|e| CrawlerError::Config(format!("保存规范失败: {e}")))?;
    Ok(())
}

/// 保存评论意向分析配置(系统设置「意向分析」)。只存对 providers/prompts 的 id 引用 +
/// 模型名 + 批大小;api_key 仍存数据库,不落配置文件。写入后重启或下次任务运行生效。
#[tauri::command]
pub async fn set_intent_config(
    state: State<'_, AppState>,
    api_url: String,
    model: String,
    intent_prompt: String,
    batch_size: i32,
    api_key: String,
) -> Result<()> {
    {
        let mut cfg = lock_config(&state)?;
        cfg.intent.api_url = api_url;
        cfg.intent.model = model;
        cfg.intent.intent_prompt = intent_prompt;
        cfg.intent.batch_size = batch_size;
        cfg.save(&state.config_dir)?;
    }
    // api_key 留空表示不修改已存的密钥
    if !api_key.trim().is_empty() {
        set_secret(&state.db, "intent_api_key", &api_key).await?;
    }
    Ok(())
}

/// 保存语音转写配置(系统设置「语音转写」)。只存厂商 id 引用 + 模型名;
/// api_key 仍存数据库,不落配置文件。目前仅支持 ASR 的厂商(小米 MiMo)可用。
#[tauri::command]
pub async fn set_transcription_config(
    state: State<'_, AppState>,
    api_url: String,
    model: String,
    api_key: String,
) -> Result<()> {
    {
        let mut cfg = lock_config(&state)?;
        cfg.transcription.api_url = api_url;
        cfg.transcription.model = model;
        cfg.save(&state.config_dir)?;
    }
    if !api_key.trim().is_empty() {
        set_secret(&state.db, "transcription_api_key", &api_key).await?;
    }
    Ok(())
}

// 密钥读写(api_key 存数据库 app_secrets,不落配置文件)
pub(crate) async fn set_secret(db: &sea_orm::DatabaseConnection, key: &str, value: &str) -> Result<()> {
    use sea_orm::sea_query::OnConflict;
    use sea_orm::Set;
    use veltrix_core::db::entity::app_secret;
    app_secret::Entity::insert(app_secret::ActiveModel {
        key: Set(key.to_owned()),
        value: Set(value.to_owned()),
    })
    .on_conflict(
        OnConflict::column(app_secret::Column::Key)
            .update_column(app_secret::Column::Value)
            .to_owned(),
    )
    .exec(db)
    .await
    .map_err(|e| CrawlerError::Config(format!("保存密钥失败: {e}")))?;
    Ok(())
}

pub(crate) async fn get_secret(db: &sea_orm::DatabaseConnection, key: &str) -> String {
    use veltrix_core::db::entity::app_secret;
    app_secret::Entity::find_by_id(key.to_owned())
        .one(db)
        .await
        .ok()
        .flatten()
        .map(|m| m.value)
        .unwrap_or_default()
}

// ===================== 角色模型(Provider 角色化) =====================

use crate::agent::core::{ProviderKind, ProviderRef};
use crate::llm::AgentRole;

/// 把角色解析为具体的厂商引用:杂活可走单独配置的便宜模型,主任务用会话绑定模型。
///
/// 查 `role_model_<role>` secret,命中则按 `providerId::model` 拆出 providerId、查 providers 表组 ProviderRef;
/// 未配置 / 拆分失败 / 厂商查不到 / 厂商无 api_key 一律稳妥回退 `fallback`(绝不报错)——
/// 否则异步摘要、记忆提取等后台杂活会因角色配置缺失而静默失败。
pub(crate) async fn resolve_role_provider(
    db: &sea_orm::DatabaseConnection,
    role: AgentRole,
    fallback: ProviderRef,
) -> ProviderRef {
    use veltrix_core::db::entity::provider as provider_entity;

    // 主对话角色不降档:始终用会话模型(即 fallback),省一次查询
    if role == AgentRole::Chat {
        return fallback;
    }
    let raw = get_secret(db, &role.secret_key()).await;
    let raw = raw.trim();
    if raw.is_empty() {
        return fallback;
    }
    // 前端编码与会话一致:providerId::model(model 自身可能含 ::,故只按首个分隔拆)
    let Some((provider_id, model)) = raw.split_once("::") else {
        return fallback;
    };
    let (provider_id, model) = (provider_id.trim(), model.trim());
    if provider_id.is_empty() || model.is_empty() {
        return fallback;
    }
    let found = provider_entity::Entity::find_by_id(provider_id.to_string())
        .one(db)
        .await
        .ok()
        .flatten();
    let Some(provider) = found else {
        return fallback;
    };
    // 未配置 api_key 的厂商不可用,回退避免调用失败
    if provider.api_key.trim().is_empty() {
        return fallback;
    }
    ProviderRef {
        kind: ProviderKind::from_code(&provider.code),
        api_url: provider.api_url,
        api_key: provider.api_key,
        model: model.to_string(),
    }
}

/// 角色模型配置(前端 KV 编辑用)。值为 `providerId::model` 串或空(空=回退会话模型)。
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RoleModelConfig {
    /// 意图分类角色的模型(便宜档)。
    pub classify_model: String,
    /// 摘要 / 标题 / 记忆提取角色的模型(便宜档)。
    pub summary_model: String,
    /// 套用 / 应用改动角色的模型(编程 Agent 预留)。
    pub apply_model: String,
}

/// 读取角色模型配置(供系统设置「角色模型」小节回填)。
#[tauri::command]
pub async fn get_role_models(state: State<'_, AppState>) -> Result<RoleModelConfig> {
    Ok(RoleModelConfig {
        classify_model: get_secret(&state.db, &AgentRole::Classify.secret_key()).await,
        summary_model: get_secret(&state.db, &AgentRole::Summary.secret_key()).await,
        apply_model: get_secret(&state.db, &AgentRole::Apply.secret_key()).await,
    })
}

/// 保存角色模型配置(空串=清空映射,回退会话模型)。用 set_secret 持久化到 app_secrets。
#[tauri::command]
pub async fn set_role_models(state: State<'_, AppState>, config: RoleModelConfig) -> Result<()> {
    set_secret(&state.db, &AgentRole::Classify.secret_key(), config.classify_model.trim()).await?;
    set_secret(&state.db, &AgentRole::Summary.secret_key(), config.summary_model.trim()).await?;
    set_secret(&state.db, &AgentRole::Apply.secret_key(), config.apply_model.trim()).await?;
    Ok(())
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

/// 导出文件:把 base64 内容写到经系统保存对话框选定的绝对路径(导出 Excel 等)。
/// 与 save_text_file 不同,不限应用数据目录——路径由 OS 保存对话框授权,写用户主动选定位置。
#[tauri::command]
pub fn save_binary_file(path: String, content_base64: String) -> Result<()> {
    use base64::Engine;
    let target = PathBuf::from(&path);
    if !target.is_absolute() {
        return Err(CrawlerError::Config("路径必须是绝对路径".into()));
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(content_base64.as_bytes())
        .map_err(|e| CrawlerError::Config(format!("导出内容解码失败: {e}")))?;
    std::fs::write(&target, bytes)
        .map_err(|e| CrawlerError::Config(format!("写入导出文件失败: {e}")))?;
    Ok(())
}

/// 清空业务数据(系统配置「危险操作」)。不可恢复:
/// 1. 用当前登录用户名 + 传入密码做 argon2 二次校验,未登录或密码错直接拒绝;
/// 2. 按逻辑外键依赖顺序删空 comments → contents → tasks(无物理级联,手动顺序);
/// 3. clear_media 为 true 时,递归清空媒体素材根目录下所有文件(保留目录本身);
///    为 false 时只清库,已下载的素材文件原样保留。
///
/// 平台 / 账号 / 用户 / 客户 / 行业 / 厂商 / 提示词等配置类数据一律保留。
/// 采集去重台账(collect_records)也刻意不清:清空后重采时据它跳过曾采过的内容,避免重复入库。
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

