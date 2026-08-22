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
    /// 全局沙盒管理器(id → 本地进程沙盒台账;coding 会话沙盒是当前使用方):
    /// 首个编程动作惰性创建,「停止沙盒」/ 退出应用时统一 terminate。
    pub sandbox: Arc<crate::sandbox::SandboxManager>,
    /// 每会话取消令牌(stop_chat_agent / stop_coding_agent 触发 cancel;流式读循环 select! 即时中断)。
    pub cancel_tokens: crate::agent::core::shared::CancelTokenMap,
    /// 令牌每回合新建、收尾由守卫摘除(原 agent_cancel / chat_cancel_flags 双轨合一)。
    /// 每会话发送互斥:同会话「发送消息」排队串行(不拒绝,后到先等),防并发回合交错。
    pub chat_send_locks: Arc<Mutex<std::collections::HashMap<String, Arc<tokio::sync::Mutex<()>>>>>,
    /// Agent 危险操作「暂停 — 等用户确认」通道(ReAct 循环命中危险工具时等待,前端
    /// `resolve_agent_confirm` 回执)。
    pub agent_confirm: Arc<crate::agent::core::shared::AgentConfirmChannel>,
    /// 电脑操作 Agent 的屏幕录制状态(同一时刻仅一个录制会话)。
    pub recording: crate::agent::computer::recorder::RecordingState,
}

/// 全局同时进行的采集任务数上限(占用 WebView 窗口的阶段)。取 3:兼顾吞吐与资源占用,
/// 不同账号 / 平台仍可并行,但不会因调度同点拉起一堆任务而一次性弹出过多窗口。
pub const MAX_CONCURRENT_COLLECT: usize = 3;

/// 账号采集锁的 key 口径(平台-账号):全工程统一从这生成,防手写 format! 漂移
pub(crate) fn account_lock_key(platform: &str, account_id: &str) -> String {
    format!("{platform}-{account_id}")
}

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

/// 生成局域网远程访问用的数据库连接串(系统设置「数据库」复制入口)。
/// 连接串含密码属敏感操作:先用当前登录用户密码做 argon2 二次校验。
/// 仅当前生效后端为 PostgreSQL 时可用;主机部分替换为本机局域网 IP
/// (UDP 路由探测,只选出站网卡不实际发包),账号/密码/端口/库名/参数原样保留。
#[tauri::command]
pub async fn get_remote_database_url(
    state: State<'_, AppState>,
    password: String,
) -> Result<String> {
    let user = current_user(&state).ok_or_else(|| CrawlerError::Auth("未登录".into()))?;
    admin::verify_user_password(&state.db, &user.name, &password).await?;

    let url = {
        let cfg = lock_config(&state)?;
        veltrix_core::db::resolve_url(&state.config_dir, &cfg.database)?
    };
    if !url.starts_with("postgres://") && !url.starts_with("postgresql://") {
        return Err(CrawlerError::Config(
            "当前使用的是本地 SQLite,远程连接串仅在 PostgreSQL 下可用".into(),
        ));
    }
    let lan_ip = lan_ipv4()?;
    replace_url_host(&url, &lan_ip)
}

/// 本机局域网 IPv4:向公网地址发起 UDP「连接」(只选路由不实际发包),取出站网卡地址。
fn lan_ipv4() -> Result<String> {
    let sock = std::net::UdpSocket::bind("0.0.0.0:0")
        .map_err(|e| CrawlerError::Config(format!("获取本机局域网 IP 失败: {e}")))?;
    sock.connect("8.8.8.8:80")
        .map_err(|e| CrawlerError::Config(format!("获取本机局域网 IP 失败: {e}")))?;
    let ip = sock
        .local_addr()
        .map_err(|e| CrawlerError::Config(format!("获取本机局域网 IP 失败: {e}")))?
        .ip();
    if !ip.is_ipv4() {
        return Err(CrawlerError::Config("未找到可用的局域网 IPv4 地址".into()));
    }
    Ok(ip.to_string())
}

/// 把 postgres 连接串的主机部分替换为指定 IP,其余部分(账号/密码/端口/库名/参数)原样保留。
fn replace_url_host(url: &str, new_host: &str) -> Result<String> {
    let scheme_end = url
        .find("://")
        .ok_or_else(|| CrawlerError::Config("连接串缺少协议头".into()))?;
    let after_scheme = &url[scheme_end + 3..];
    // authority 结束于第一个 '/'
    let path_start = after_scheme.find('/').unwrap_or(after_scheme.len());
    let authority = &after_scheme[..path_start];
    let rest = &after_scheme[path_start..];
    // host 起点:last '@' 之后(无 @ 则整个 authority 即 host)
    let host_start = authority.rfind('@').map(|i| i + 1).unwrap_or(0);
    let userinfo = &authority[..host_start];
    let hostport = &authority[host_start..];
    // 端口:host 后第一个 ':'(本机 PG 均为 IPv4/主机名,不展开 IPv6)
    let port = match hostport.find(':') {
        Some(i) => &hostport[i..],
        None => "",
    };
    Ok(format!(
        "{}{}{}{}{}",
        &url[..scheme_end + 3],
        userinfo,
        new_host,
        port,
        rest
    ))
}

// ===================== SQLite → PostgreSQL 一键迁移 =====================

/// 单表迁移结果(read=源读取行数,written=实际写入行数;幂等重跑时 written 可为 0)。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TableMigrationView {
    pub table: String,
    pub read: i64,
    pub written: i64,
    /// 目标库不存在该表(未搬)
    pub skipped: bool,
}

/// 迁移进度事件(db-migrate-progress)负载:前端对话框据此渲染进度条。
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct MigrateProgressEvent {
    table: String,
    /// 当前第几张表(1 起)
    table_index: usize,
    table_total: usize,
    /// 当前表总行数(reading 阶段为 0,读完回填)
    table_rows: i64,
    /// 当前表已写入行数
    table_written: i64,
    /// reading / writing / done
    phase: &'static str,
}

/// 把当前 SQLite 库的全量数据复制到指定 PostgreSQL 连接串(系统设置「数据库」一键迁移)。
///
/// 语义(与 scripts/migrate-sqlite-to-pg.py 一致):
/// - 目标库先用实体 DDL 建表(init_schema),再逐表 INSERT ... ON CONFLICT DO NOTHING,
///   幂等可重复执行,源库全程只读;
/// - SQLite 的 0/1 布尔列按目标库 information_schema 的 boolean 列转换(PG 布尔不收整数);
/// - 迁移后重置目标库自增序列(setval 到 max(id)),防止后续插入撞主键;
/// - 逐表/逐批 emit `db-migrate-progress` 事件,前端渲染进度条。
#[tauri::command]
pub async fn migrate_sqlite_to_pg(
    state: State<'_, AppState>,
    app: AppHandle,
    target_url: String,
) -> Result<Vec<TableMigrationView>> {
    use sea_orm::{TransactionTrait, Value};
    use tauri::Emitter;

    let emit = |p: MigrateProgressEvent| {
        let _ = app.emit("db-migrate-progress", p);
    };

    let source = state.db.clone();
    if source.get_database_backend() != DatabaseBackend::Sqlite {
        return Err(CrawlerError::Config(
            "当前已是 PostgreSQL,无需迁移(该功能用于 SQLite → PG)".into(),
        ));
    }
    if !target_url.starts_with("postgres://") && !target_url.starts_with("postgresql://") {
        return Err(CrawlerError::Config(
            "目标连接串必须以 postgres:// 开头".into(),
        ));
    }

    // 目标库不存在(SQLSTATE 3D000)时自动建库:连同服务器的 postgres 维护库 CREATE DATABASE;
    // 其余连接失败(拒连/认证失败等)直接报错,不做任何尝试
    let target = match sea_orm::Database::connect(&target_url).await {
        Ok(db) => db,
        Err(e) => {
            let msg = e.to_string();
            if !(msg.contains("3D000") || msg.contains("does not exist")) {
                return Err(CrawlerError::Config(format!(
                    "连接目标 PostgreSQL 失败: {e}"
                )));
            }
            create_pg_database(&target_url).await.map_err(|ce| {
                CrawlerError::Config(format!(
                    "目标数据库不存在,自动创建失败: {ce}(可手动 CREATE DATABASE 后重试)"
                ))
            })?;
            sea_orm::Database::connect(&target_url)
                .await
                .map_err(|e2| CrawlerError::Config(format!("建库后重连失败: {e2}")))?
        }
    };
    // 目标建表(已存在则跳过);目标连不上/无权限在此直接报错,不会动源库
    veltrix_core::db::init_schema(&target).await?;

    // 目标库的布尔列集合:SQLite 存 0/1,插 PG 前转 bool
    let bool_cols: std::collections::HashSet<(String, String)> = target
        .query_all(Statement::from_string(
            DatabaseBackend::Postgres,
            "SELECT table_name, column_name FROM information_schema.columns \
             WHERE table_schema='public' AND data_type='boolean'".to_owned(),
        ))
        .await
        .map_err(|e| CrawlerError::Config(format!("读取目标库结构失败: {e}")))?
        .iter()
        .filter_map(|r| {
            Some((
                r.try_get::<String>("", "table_name").ok()?,
                r.try_get::<String>("", "column_name").ok()?,
            ))
        })
        .collect();

    // 源库表清单(sqlite_ 前缀为内部表,不搬)
    let tables: Vec<String> = source
        .query_all(Statement::from_string(
            DatabaseBackend::Sqlite,
            "SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name".to_owned(),
        ))
        .await
        .map_err(|e| CrawlerError::Config(format!("读取源库表清单失败: {e}")))?
        .iter()
        .filter_map(|r| r.try_get::<String>("", "name").ok())
        .collect();

    const BATCH: usize = 200;
    let mut report = Vec::new();
    let table_total = tables.len();
    for (idx, table) in tables.into_iter().enumerate() {
        let table_index = idx + 1;
        emit(MigrateProgressEvent {
            table: table.clone(),
            table_index,
            table_total,
            table_rows: 0,
            table_written: 0,
            phase: "reading",
        });
        // 源列(声明类型) + 目标列交集:只搬两边都有的列
        let pragma = source
            .query_all(Statement::from_string(
                DatabaseBackend::Sqlite,
                format!("PRAGMA table_info(\"{table}\")"),
            ))
            .await
            .map_err(|e| CrawlerError::Config(format!("读取源表结构失败({table}): {e}")))?;
        let src_cols: Vec<(String, String)> = pragma
            .iter()
            .filter_map(|r| {
                Some((
                    r.try_get::<String>("", "name").ok()?,
                    r.try_get::<String>("", "type").ok()?.to_uppercase(),
                ))
            })
            .collect();
        let tgt_cols: std::collections::HashSet<String> = target
            .query_all(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "SELECT column_name FROM information_schema.columns \
                 WHERE table_schema='public' AND table_name=$1",
                [table.clone().into()],
            ))
            .await
            .map_err(|e| CrawlerError::Config(format!("读取目标表结构失败({table}): {e}")))?
            .iter()
            .filter_map(|r| r.try_get::<String>("", "column_name").ok())
            .collect();
        if tgt_cols.is_empty() {
            tracing::warn!("目标库不存在表 {table},跳过");
            emit(MigrateProgressEvent {
                table: table.clone(),
                table_index,
                table_total,
                table_rows: 0,
                table_written: 0,
                phase: "done",
            });
            report.push(TableMigrationView { table, read: 0, written: 0, skipped: true });
            continue;
        }
        let cols: Vec<(String, String)> = src_cols
            .into_iter()
            .filter(|(name, _)| tgt_cols.contains(name))
            .collect();
        let col_list = cols
            .iter()
            .map(|(n, _)| format!("\"{n}\""))
            .collect::<Vec<_>>()
            .join(", ");

        let rows = source
            .query_all(Statement::from_string(
                DatabaseBackend::Sqlite,
                format!("SELECT {col_list} FROM \"{table}\""),
            ))
            .await
            .map_err(|e| CrawlerError::Config(format!("读取源表失败({table}): {e}")))?;
        let table_rows = rows.len() as i64;
        emit(MigrateProgressEvent {
            table: table.clone(),
            table_index,
            table_total,
            table_rows,
            table_written: 0,
            phase: "writing",
        });

        // 逐表事务:每批一条多行 INSERT,ON CONFLICT DO NOTHING 保证幂等
        let txn = target
            .begin()
            .await
            .map_err(|e| CrawlerError::Config(format!("开启目标事务失败({table}): {e}")))?;
        let mut written: i64 = 0;
        for (batch_i, chunk) in rows.chunks(BATCH).enumerate() {
            let mut values: Vec<Value> = Vec::with_capacity(chunk.len() * cols.len());
            let mut placeholders = String::new();
            for (row_i, row) in chunk.iter().enumerate() {
                if row_i > 0 {
                    placeholders.push_str(", ");
                }
                placeholders.push('(');
                for (col_i, (name, decl)) in cols.iter().enumerate() {
                    if col_i > 0 {
                        placeholders.push_str(", ");
                    }
                    placeholders.push_str(&format!("${}", row_i * cols.len() + col_i + 1));
                    let is_bool_target = bool_cols.contains(&(table.clone(), name.clone()));
                    // 按 SQLite 声明类型取裸值;目标是布尔列时把 0/1 转 bool
                    let v: Value = if is_bool_target {
                        Value::from(row.try_get::<Option<i64>>("", name).ok().flatten().map(|n| n != 0))
                    } else if decl.contains("INT") || decl.contains("BOOL") {
                        Value::from(row.try_get::<Option<i64>>("", name).ok().flatten())
                    } else if decl.contains("REAL") || decl.contains("FLOA") || decl.contains("DOUB") {
                        Value::from(row.try_get::<Option<f64>>("", name).ok().flatten())
                    } else if decl.contains("BLOB") {
                        Value::from(row.try_get::<Option<Vec<u8>>>("", name).ok().flatten())
                    } else {
                        Value::from(row.try_get::<Option<String>>("", name).ok().flatten())
                    };
                    values.push(v);
                }
                placeholders.push(')');
            }
            let sql = format!(
                "INSERT INTO \"{table}\" ({col_list}) VALUES {placeholders} ON CONFLICT DO NOTHING"
            );
            let res = txn
                .execute(Statement::from_sql_and_values(
                    DatabaseBackend::Postgres,
                    sql,
                    values,
                ))
                .await
                .map_err(|e| CrawlerError::Config(format!("写入目标表失败({table}): {e}")))?;
            written += res.rows_affected() as i64;
            // 批次进度按「已处理行数」推进(幂等重跑 rows_affected=0 时进度条也照常走)
            emit(MigrateProgressEvent {
                table: table.clone(),
                table_index,
                table_total,
                table_rows,
                table_written: (batch_i * BATCH + chunk.len()) as i64,
                phase: "writing",
            });
        }
        txn.commit()
            .await
            .map_err(|e| CrawlerError::Config(format!("提交目标事务失败({table}): {e}")))?;
        tracing::info!(table = %table, read = rows.len(), written, "迁移表完成");
        emit(MigrateProgressEvent {
            table: table.clone(),
            table_index,
            table_total,
            table_rows,
            table_written: table_rows,
            phase: "done",
        });
        report.push(TableMigrationView {
            read: rows.len() as i64,
            written,
            table,
            skipped: false,
        });
    }

    // 自增序列重置:带显式 id 迁入后,序列仍指向旧值会撞主键
    let serial_cols = target
        .query_all(Statement::from_string(
            DatabaseBackend::Postgres,
            "SELECT table_name, column_name FROM information_schema.columns \
             WHERE table_schema='public' AND column_default LIKE 'nextval%'".to_owned(),
        ))
        .await
        .map_err(|e| CrawlerError::Config(format!("读取目标序列失败: {e}")))?;
    for r in &serial_cols {
        let (Ok(t), Ok(c)) = (
            r.try_get::<String>("", "table_name"),
            r.try_get::<String>("", "column_name"),
        ) else {
            continue;
        };
        let _ = target
            .execute(Statement::from_string(
                DatabaseBackend::Postgres,
                format!(
                    "SELECT setval(pg_get_serial_sequence('\"{t}\"','{c}'), \
                     COALESCE((SELECT MAX(\"{c}\") FROM \"{t}\"), 1))"
                ),
            ))
            .await;
    }

    Ok(report)
}

/// 在目标服务器上创建连接串所指的数据库(连默认 postgres 维护库执行 CREATE DATABASE)。
/// 库名做标识符白名单校验,杜绝拼串注入;已存在(42P04)视为成功(重试/并发幂等)。
async fn create_pg_database(target_url: &str) -> Result<()> {
    let after_scheme = target_url
        .splitn(2, "://")
        .nth(1)
        .ok_or_else(|| CrawlerError::Config("连接串缺少协议头".into()))?;
    // 库名 = path 段(去查询串/fragment)
    let path = after_scheme
        .find('/')
        .map(|i| &after_scheme[i + 1..])
        .unwrap_or("");
    let db_name = path.split(['?', '#']).next().unwrap_or("").trim();
    if db_name.is_empty()
        || !db_name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Err(CrawlerError::Config(format!(
            "连接串中的库名无效(仅允许字母/数字/_/-): {db_name}"
        )));
    }
    // 维护库连接串:同账号同主机,库名换成 postgres
    let host_part = &after_scheme[..after_scheme.find('/').unwrap_or(after_scheme.len())];
    let scheme = &target_url[..target_url.len() - after_scheme.len()];
    let maintenance = format!("{scheme}{host_part}/postgres");
    let db = sea_orm::Database::connect(&maintenance)
        .await
        .map_err(|e| CrawlerError::Config(format!("连接 postgres 维护库失败: {e}")))?;
    match db
        .execute(Statement::from_string(
            DatabaseBackend::Postgres,
            format!("CREATE DATABASE \"{db_name}\""),
        ))
        .await
    {
        Ok(_) => Ok(()),
        // 已存在 = 目标达成(并发/重复迁移场景)
        Err(e) if e.to_string().contains("42P04") || e.to_string().contains("already exists") => {
            Ok(())
        }
        Err(e) => Err(CrawlerError::Config(format!("CREATE DATABASE 失败: {e}"))),
    }
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

/// 保存语音转写配置(系统设置「语音转写」)。存厂商 code + API 地址 + 模型名;
/// api_key 仍存数据库,不落配置文件。仅支持 ASR 的厂商(小米 MiMo、智谱 GLM)可用。
#[tauri::command]
pub async fn set_transcription_config(
    state: State<'_, AppState>,
    provider: String,
    api_url: String,
    model: String,
    api_key: String,
    concurrency: u32,
) -> Result<()> {
    {
        let mut cfg = lock_config(&state)?;
        cfg.transcription.provider = provider;
        cfg.transcription.api_url = api_url;
        cfg.transcription.model = model;
        // 并发数最小 1;前端异常传 0 时回退默认,避免 buffer_unordered(0) 空转
        cfg.transcription.concurrency = if concurrency == 0 {
            veltrix_core::config::DEFAULT_ASR_CONCURRENCY
        } else {
            concurrency
        };
        cfg.save(&state.config_dir)?;
    }
    if !api_key.trim().is_empty() {
        set_secret(&state.db, "transcription_api_key", &api_key).await?;
    }
    Ok(())
}

/// 保存海外平台音频拉流的代理(系统设置「网络代理」)。
/// proxy:空 = 自动探测本机代理、"off" = 关闭(直连)、其余按代理 URL 使用。
/// 仅作用于下次任务(采集任务启动时快照 media 配置),不影响正在跑的任务。
#[tauri::command]
pub async fn set_media_proxy(state: State<'_, AppState>, proxy: String) -> Result<()> {
    let mut cfg = lock_config(&state)?;
    cfg.media.proxy = proxy.trim().to_string();
    cfg.save(&state.config_dir)?;
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
/// 2. 按逻辑外键依赖顺序删空 comments → contents → tasks(无物理级联,手动顺序),再清 authors 作者库;
/// 3. clear_media 为 true 时,递归清空媒体素材根目录下所有文件(保留目录本身);
///    为 false 时只清库,已下载的素材文件原样保留。
///
/// 平台 / 账号 / 用户 / 客户 / 行业 / 厂商 / 提示词等配置类数据一律保留。
/// 采集去重台账(collect_records)也保留:重采时曾采过的内容仍会入库,台账只用于
/// 智能停止的「新增计数」(重复内容不占目标配额)。
#[tauri::command]
pub async fn clear_business_data(
    state: State<'_, AppState>,
    password: String,
    clear_media: bool,
) -> Result<()> {
    use veltrix_core::db::entity::{
        author as author_entity, collect_log as collect_log_entity,
        collect_record as collect_record_entity, comment as comment_entity,
        content as content_entity,
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
    // 采集去重台账一并清空:台账驱动的「跳过已采」若保留,清空后重采会一条不入库
    collect_record_entity::Entity::delete_many()
        .exec(db)
        .await
        .map_err(|e| CrawlerError::Config(format!("清空采集去重台账失败: {e}")))?;
    task_entity::Entity::delete_many()
        .exec(db)
        .await
        .map_err(|e| CrawlerError::Config(format!("清空任务失败: {e}")))?;
    // 作者库(作者聚合档案)同属业务数据,一并清空;注意会连带清掉作者级监控 / 拉黑标记
    author_entity::Entity::delete_many()
        .exec(db)
        .await
        .map_err(|e| CrawlerError::Config(format!("清空作者库失败: {e}")))?;

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

