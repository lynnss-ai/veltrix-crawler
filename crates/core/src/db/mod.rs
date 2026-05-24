//! 数据库连接与建表。运行时按连接串二选一支持 SQLite / PostgreSQL。
//!
//! 后端由连接串自动识别(`sqlite://...` / `postgres://...`),实体与查询写一次跨后端复用。
//! 连接串优先级:环境变量 `VELTRIX_DATABASE_URL` > 配置文件 > 默认本地 SQLite 文件;
//! PG 含密码时只走环境变量,避免落盘到配置文件(安全规范)。

pub mod entity;

use crate::config::DatabaseConfig;
use crate::error::{CrawlerError, Result};
use sea_orm::{
    ConnectOptions, ConnectionTrait, Database, DatabaseConnection, EntityTrait, Schema, Statement,
};
use std::path::Path;
use std::time::Duration;

/// 默认本地 SQLite 文件名(连接串为空时使用)。
const DEFAULT_LOCAL_DB_FILE: &str = "veltrix.db";
/// 承载敏感连接串(如含密码的 PG)的环境变量名。
const DATABASE_URL_ENV: &str = "VELTRIX_DATABASE_URL";
/// 连接超时秒数。
const CONNECT_TIMEOUT_SECS: u64 = 8;

/// 连接数据库并确保表结构存在。
/// 若按配置/环境变量解析出的连接串无法连接(如用户填了无效连接串),
/// 自动回退到默认本地 SQLite,避免启动崩溃。
pub async fn connect(config_dir: &Path, cfg: &DatabaseConfig) -> Result<DatabaseConnection> {
    let url = resolve_url(config_dir, cfg)?;
    let db = match try_connect(&url, cfg.max_connections).await {
        Ok(db) => db,
        Err(e) => {
            let fallback = default_sqlite_url(config_dir)?;
            if fallback == url {
                // 连默认本地库都失败,无从回退
                return Err(e);
            }
            tracing::warn!("连接数据库失败({e}),已回退默认本地 SQLite: {fallback}");
            try_connect(&fallback, cfg.max_connections).await?
        }
    };
    init_schema(&db).await?;
    Ok(db)
}

/// 测试给定连接串能否连通(不建表、不影响当前连接)。
pub async fn test_connection(url: &str) -> Result<()> {
    if url.trim().is_empty() {
        return Err(CrawlerError::Config("连接串为空".into()));
    }
    let db = try_connect(url, 1).await?;
    db.ping()
        .await
        .map_err(|e| CrawlerError::Config(format!("连接测试失败: {e}")))?;
    Ok(())
}

/// 按给定连接串建立连接(不建表)。
async fn try_connect(url: &str, max_connections: u32) -> Result<DatabaseConnection> {
    let mut opt = ConnectOptions::new(url.to_owned());
    opt.max_connections(max_connections)
        .connect_timeout(Duration::from_secs(CONNECT_TIMEOUT_SECS))
        .sqlx_logging(false);
    Database::connect(opt)
        .await
        .map_err(|e| CrawlerError::Config(format!("连接数据库失败: {e}")))
}

/// 默认本地 SQLite 连接串(应用数据目录下的文件)。
fn default_sqlite_url(config_dir: &Path) -> Result<String> {
    std::fs::create_dir_all(config_dir)?;
    let path = config_dir.join(DEFAULT_LOCAL_DB_FILE);
    // Windows 路径分隔符转 URL 友好的正斜杠;mode=rwc 允许文件不存在时自动创建
    let normalized = path.display().to_string().replace('\\', "/");
    Ok(format!("sqlite://{normalized}?mode=rwc"))
}

/// 从连接串解析 SQLite 文件路径;非 SQLite(如 PG)返回 None。
pub fn sqlite_file_path(url: &str) -> Option<String> {
    if !url.to_ascii_lowercase().starts_with("sqlite:") {
        return None;
    }
    let rest = url
        .trim_start_matches("sqlite://")
        .trim_start_matches("sqlite:");
    let path = rest.split('?').next().unwrap_or(rest);
    if path.is_empty() || path == ":memory:" {
        return None;
    }
    Some(path.to_string())
}

/// 解析连接串:环境变量 > 配置 > 默认本地 SQLite。
pub fn resolve_url(config_dir: &Path, cfg: &DatabaseConfig) -> Result<String> {
    if let Ok(env_url) = std::env::var(DATABASE_URL_ENV) {
        if !env_url.trim().is_empty() {
            return Ok(env_url);
        }
    }
    if !cfg.url.trim().is_empty() {
        return Ok(cfg.url.clone());
    }
    default_sqlite_url(config_dir)
}

/// 建表(若不存在)。用实体生成跨方言 DDL,SQLite / PG 通用。
/// 后续新增表在此追加 `create_table` 调用。
async fn init_schema(db: &DatabaseConnection) -> Result<()> {
    let schema = Schema::new(db.get_database_backend());

    create_table(db, &schema, entity::account::Entity, "accounts").await?;
    create_table(db, &schema, entity::user::Entity, "users").await?;
    create_table(db, &schema, entity::industry::Entity, "industries").await?;
    create_table(db, &schema, entity::keyword::Entity, "keywords").await?;
    create_table(db, &schema, entity::customer::Entity, "customers").await?;
    create_table(db, &schema, entity::platform_api::Entity, "platform_apis").await?;
    create_table(db, &schema, entity::provider::Entity, "providers").await?;
    create_table(db, &schema, entity::prompt::Entity, "prompts").await?;

    // 兼容旧版 accounts 表:补充新增列(列已存在时忽略错误)。
    // ALTER TABLE ADD COLUMN 在 SQLite / PostgreSQL 通用。
    let backend = db.get_database_backend();
    for ddl in [
        "ALTER TABLE accounts ADD COLUMN code TEXT NOT NULL DEFAULT ''",
        "ALTER TABLE accounts ADD COLUMN remark TEXT NOT NULL DEFAULT ''",
        "ALTER TABLE accounts ADD COLUMN owner TEXT NOT NULL DEFAULT ''",
    ] {
        let _ = db
            .execute(Statement::from_string(backend, ddl.to_owned()))
            .await;
    }

    Ok(())
}

/// 用实体建表(若不存在)。
async fn create_table<E>(
    db: &DatabaseConnection,
    schema: &Schema,
    entity: E,
    name: &str,
) -> Result<()>
where
    E: EntityTrait,
{
    let backend = db.get_database_backend();
    let mut stmt = schema.create_table_from_entity(entity);
    stmt.if_not_exists();
    db.execute(backend.build(&stmt))
        .await
        .map_err(|e| CrawlerError::Config(format!("初始化 {name} 表失败: {e}")))?;
    Ok(())
}
