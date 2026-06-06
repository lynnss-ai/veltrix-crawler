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
    create_table(db, &schema, entity::task::Entity, "tasks").await?;
    create_table(db, &schema, entity::content::Entity, "contents").await?;
    create_table(db, &schema, entity::comment::Entity, "comments").await?;

    // 兼容旧版 accounts 表:仅在列不存在时 ALTER,避免每次启动都触发(SQLite 不可逆操作)
    let backend = db.get_database_backend();
    for (col, ddl) in [
        ("code", "ALTER TABLE accounts ADD COLUMN code TEXT NOT NULL DEFAULT ''"),
        ("remark", "ALTER TABLE accounts ADD COLUMN remark TEXT NOT NULL DEFAULT ''"),
        ("owner", "ALTER TABLE accounts ADD COLUMN owner TEXT NOT NULL DEFAULT ''"),
    ] {
        if !column_exists(db, "accounts", col).await {
            if let Err(e) = db
                .execute(Statement::from_string(backend, ddl.to_owned()))
                .await
            {
                tracing::warn!("ALTER accounts.{col} 失败(忽略): {e}");
            }
        }
    }

    // 兼容已建的 contents 表:补 keyword 列(全量库按采集关键词筛选)
    if !column_exists(db, "contents", "keyword").await {
        if let Err(e) = db
            .execute(Statement::from_string(
                backend,
                "ALTER TABLE contents ADD COLUMN keyword TEXT NOT NULL DEFAULT ''".to_owned(),
            ))
            .await
        {
            tracing::warn!("ALTER contents.keyword 失败(忽略): {e}");
        }
    }

    // 兼容已建的 contents 表:补 cover_url 列(封面下载与展示)
    if !column_exists(db, "contents", "cover_url").await {
        if let Err(e) = db
            .execute(Statement::from_string(
                backend,
                "ALTER TABLE contents ADD COLUMN cover_url TEXT".to_owned(),
            ))
            .await
        {
            tracing::warn!("ALTER contents.cover_url 失败(忽略): {e}");
        }
    }

    // 兼容已建的 contents 表:补 duration(视频时长秒)与 topics(话题 JSON)列
    if !column_exists(db, "contents", "duration").await {
        if let Err(e) = db
            .execute(Statement::from_string(
                backend,
                "ALTER TABLE contents ADD COLUMN duration BIGINT".to_owned(),
            ))
            .await
        {
            tracing::warn!("ALTER contents.duration 失败(忽略): {e}");
        }
    }
    if !column_exists(db, "contents", "topics").await {
        if let Err(e) = db
            .execute(Statement::from_string(
                backend,
                "ALTER TABLE contents ADD COLUMN topics TEXT NOT NULL DEFAULT '[]'".to_owned(),
            ))
            .await
        {
            tracing::warn!("ALTER contents.topics 失败(忽略): {e}");
        }
    }

    // 兼容已建的 contents 表:补素材状态列(下载/音频提取结果与失败重试)。
    // 均可空,旧行 None 表示「未跑过下载」,前端按未知态展示。
    for (col, ddl) in [
        ("media_status", "ALTER TABLE contents ADD COLUMN media_status TEXT"),
        ("audio_extracted", "ALTER TABLE contents ADD COLUMN audio_extracted BOOLEAN"),
        ("media_error", "ALTER TABLE contents ADD COLUMN media_error TEXT"),
    ] {
        if !column_exists(db, "contents", col).await {
            if let Err(e) = db
                .execute(Statement::from_string(backend, ddl.to_owned()))
                .await
            {
                tracing::warn!("ALTER contents.{col} 失败(忽略): {e}");
            }
        }
    }

    // 二级索引:覆盖高频查询字段。CREATE INDEX IF NOT EXISTS 跨 SQLite/PG 通用,重启无副作用。
    for ddl in [
        "CREATE INDEX IF NOT EXISTS idx_accounts_platform_last_used ON accounts(platform, last_used_at)",
        "CREATE INDEX IF NOT EXISTS idx_accounts_owner ON accounts(owner)",
        "CREATE INDEX IF NOT EXISTS idx_tasks_owner_updated ON tasks(owner, updated_at)",
        "CREATE INDEX IF NOT EXISTS idx_tasks_status ON tasks(status)",
        "CREATE INDEX IF NOT EXISTS idx_customers_owner ON customers(owner)",
        "CREATE INDEX IF NOT EXISTS idx_keywords_industry ON keywords(industry_id)",
        "CREATE INDEX IF NOT EXISTS idx_platform_apis_platform ON platform_apis(platform_id)",
        "CREATE UNIQUE INDEX IF NOT EXISTS idx_users_username ON users(username)",
        "CREATE INDEX IF NOT EXISTS idx_contents_task ON contents(task_id)",
        "CREATE INDEX IF NOT EXISTS idx_comments_task ON comments(task_id)",
        "CREATE INDEX IF NOT EXISTS idx_comments_content ON comments(content_id)",
    ] {
        if let Err(e) = db
            .execute(Statement::from_string(backend, ddl.to_owned()))
            .await
        {
            tracing::warn!("创建索引失败(忽略): {ddl} → {e}");
        }
    }

    Ok(())
}

/// 检查指定表的指定列是否存在。SQLite 用 `PRAGMA table_info`,PG 走 `information_schema`。
/// 任何查询错误一律返回 false(让上层照常尝试 ALTER,失败也被忽略)。
async fn column_exists(db: &DatabaseConnection, table: &str, column: &str) -> bool {
    use sea_orm::DatabaseBackend;
    let backend = db.get_database_backend();
    let stmt = match backend {
        DatabaseBackend::Sqlite => Statement::from_string(
            backend,
            format!("PRAGMA table_info({table})"),
        ),
        DatabaseBackend::Postgres => Statement::from_sql_and_values(
            backend,
            "SELECT column_name FROM information_schema.columns WHERE table_name = $1 AND column_name = $2 LIMIT 1",
            [table.into(), column.into()],
        ),
        // MySQL 暂不支持
        _ => return false,
    };
    let rows = match db.query_all(stmt).await {
        Ok(r) => r,
        Err(_) => return false,
    };
    if matches!(backend, DatabaseBackend::Postgres) {
        return !rows.is_empty();
    }
    // SQLite: 遍历找 name 列
    for row in rows {
        let name: String = row.try_get("", "name").unwrap_or_default();
        if name == column {
            return true;
        }
    }
    false
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
