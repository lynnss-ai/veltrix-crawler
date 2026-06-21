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
    let db = Database::connect(opt)
        .await
        .map_err(|e| CrawlerError::Config(format!("连接数据库失败: {e}")))?;
    // SQLite 默认单写者、无忙等待:连接池有多条连接(默认 8)且采集期并发写库
    // (增量入库 / 日志 writer / 进度回写 / 媒体回写)会立刻抛 "database is locked"。
    // 开 WAL 让读写并发、设 busy_timeout 让写冲突自动重试,消除并发丢更新。
    // PG 不走此分支(仅对 sqlite 连接串生效)。
    if url.starts_with("sqlite") {
        for pragma in [
            "PRAGMA journal_mode=WAL;",
            "PRAGMA busy_timeout=5000;",
            "PRAGMA synchronous=NORMAL;",
        ] {
            if let Err(e) = db.execute_unprepared(pragma).await {
                tracing::warn!("设置 SQLite PRAGMA 失败({pragma}): {e}");
            }
        }
    }
    Ok(db)
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
    create_table(db, &schema, entity::provider::Entity, "providers").await?;
    create_table(db, &schema, entity::prompt::Entity, "prompts").await?;
    create_table(db, &schema, entity::app_secret::Entity, "app_secrets").await?;
    create_table(db, &schema, entity::task::Entity, "tasks").await?;
    create_table(db, &schema, entity::content::Entity, "contents").await?;
    create_table(db, &schema, entity::comment::Entity, "comments").await?;
    create_table(db, &schema, entity::collect_log::Entity, "collect_logs").await?;
    create_table(db, &schema, entity::task_run::Entity, "task_runs").await?;
    create_table(
        db,
        &schema,
        entity::content_synced_user::Entity,
        "content_synced_users",
    )
    .await?;
    create_table(db, &schema, entity::author::Entity, "authors").await?;
    create_table(
        db,
        &schema,
        entity::chat_conversation::Entity,
        "chat_conversations",
    )
    .await?;
    create_table(db, &schema, entity::chat_message::Entity, "chat_messages").await?;
    create_table(db, &schema, entity::chat_memory::Entity, "chat_memories").await?;

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
        ("cover_path", "ALTER TABLE contents ADD COLUMN cover_path TEXT"),
        ("avatar_path", "ALTER TABLE contents ADD COLUMN avatar_path TEXT"),
        ("audio_path", "ALTER TABLE contents ADD COLUMN audio_path TEXT"),
        ("transcript", "ALTER TABLE contents ADD COLUMN transcript TEXT"),
        ("transcript_error", "ALTER TABLE contents ADD COLUMN transcript_error TEXT"),
        ("video_downloaded", "ALTER TABLE contents ADD COLUMN video_downloaded BOOLEAN"),
        ("image_total", "ALTER TABLE contents ADD COLUMN image_total INTEGER"),
        ("image_done", "ALTER TABLE contents ADD COLUMN image_done INTEGER"),
        ("comment_collected", "ALTER TABLE contents ADD COLUMN comment_collected BOOLEAN"),
        ("intent_analyzed", "ALTER TABLE contents ADD COLUMN intent_analyzed BOOLEAN"),
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

    // 兼容已建的 tasks 表:补素材下载进度列(采集完成 → downloading_media 阶段统计)。
    // NOT NULL DEFAULT 0,旧行回填 0,语义为「无素材待下载」。
    for (col, ddl) in [
        ("media_total", "ALTER TABLE tasks ADD COLUMN media_total INTEGER NOT NULL DEFAULT 0"),
        ("media_done", "ALTER TABLE tasks ADD COLUMN media_done INTEGER NOT NULL DEFAULT 0"),
    ] {
        if !column_exists(db, "tasks", col).await {
            if let Err(e) = db
                .execute(Statement::from_string(backend, ddl.to_owned()))
                .await
            {
                tracing::warn!("ALTER tasks.{col} 失败(忽略): {e}");
            }
        }
    }

    // 兼容已建的 tasks 表:补评论采集列(开关 / 过滤参数 / 评论采集阶段进度)。
    // 布尔与整数 NOT NULL DEFAULT 0,文本默认 'any'(不限),旧行回填默认值语义为「未开评论采集」。
    for (col, ddl) in [
        ("collect_comments", "ALTER TABLE tasks ADD COLUMN collect_comments BOOLEAN NOT NULL DEFAULT 0"),
        ("comment_time_range", "ALTER TABLE tasks ADD COLUMN comment_time_range TEXT NOT NULL DEFAULT 'any'"),
        ("comment_limit", "ALTER TABLE tasks ADD COLUMN comment_limit INTEGER NOT NULL DEFAULT 0"),
        ("analyze_comment_intent", "ALTER TABLE tasks ADD COLUMN analyze_comment_intent BOOLEAN NOT NULL DEFAULT 0"),
        ("comment_video_total", "ALTER TABLE tasks ADD COLUMN comment_video_total INTEGER NOT NULL DEFAULT 0"),
        ("comment_video_done", "ALTER TABLE tasks ADD COLUMN comment_video_done INTEGER NOT NULL DEFAULT 0"),
        ("archived", "ALTER TABLE tasks ADD COLUMN archived BOOLEAN NOT NULL DEFAULT 0"),
        ("auto_sync_obsidian", "ALTER TABLE tasks ADD COLUMN auto_sync_obsidian BOOLEAN NOT NULL DEFAULT 0"),
    ] {
        if !column_exists(db, "tasks", col).await {
            if let Err(e) = db
                .execute(Statement::from_string(backend, ddl.to_owned()))
                .await
            {
                tracing::warn!("ALTER tasks.{col} 失败(忽略): {e}");
            }
        }
    }

    // 兼容已建的 comments 表:补意向分析列(AI 标注,可空,旧行 None=未分析)
    for (col, ddl) in [
        ("intent_level", "ALTER TABLE comments ADD COLUMN intent_level TEXT"),
        ("intent_reason", "ALTER TABLE comments ADD COLUMN intent_reason TEXT"),
    ] {
        if !column_exists(db, "comments", col).await {
            if let Err(e) = db
                .execute(Statement::from_string(backend, ddl.to_owned()))
                .await
            {
                tracing::warn!("ALTER comments.{col} 失败(忽略): {e}");
            }
        }
    }

    // 兼容已建的 users 表:补 Obsidian vault 路径列(每用户各自配置,空=未配置)
    if !column_exists(db, "users", "obsidian_vault_path").await {
        if let Err(e) = db
            .execute(Statement::from_string(
                backend,
                "ALTER TABLE users ADD COLUMN obsidian_vault_path TEXT NOT NULL DEFAULT ''"
                    .to_owned(),
            ))
            .await
        {
            tracing::warn!("ALTER users.obsidian_vault_path 失败(忽略): {e}");
        }
    }

    // 兼容已建的 chat_conversations 表:补滚动摘要列(长会话压缩前情提要)+ 场景类型。
    // summary 默认空串,summarized_upto_id 默认 0(尚未折叠任何消息);agent_type 默认 chat。
    for (col, ddl) in [
        ("summary", "ALTER TABLE chat_conversations ADD COLUMN summary TEXT NOT NULL DEFAULT ''"),
        ("summarized_upto_id", "ALTER TABLE chat_conversations ADD COLUMN summarized_upto_id BIGINT NOT NULL DEFAULT 0"),
        ("agent_type", "ALTER TABLE chat_conversations ADD COLUMN agent_type TEXT NOT NULL DEFAULT 'chat'"),
        ("archived", "ALTER TABLE chat_conversations ADD COLUMN archived BOOLEAN NOT NULL DEFAULT 0"),
        ("plan_todos", "ALTER TABLE chat_conversations ADD COLUMN plan_todos TEXT NOT NULL DEFAULT ''"),
    ] {
        if !column_exists(db, "chat_conversations", col).await {
            if let Err(e) = db
                .execute(Statement::from_string(backend, ddl.to_owned()))
                .await
            {
                tracing::warn!("ALTER chat_conversations.{col} 失败(忽略): {e}");
            }
        }
    }

    // 兼容已建的 chat_messages 表:补工具消息列(Agent 工具往返;均可空,旧行为纯文本)
    for (col, ddl) in [
        ("tool_calls", "ALTER TABLE chat_messages ADD COLUMN tool_calls TEXT"),
        ("tool_call_id", "ALTER TABLE chat_messages ADD COLUMN tool_call_id TEXT"),
        ("tool_name", "ALTER TABLE chat_messages ADD COLUMN tool_name TEXT"),
        ("attachments", "ALTER TABLE chat_messages ADD COLUMN attachments TEXT"),
        ("reasoning", "ALTER TABLE chat_messages ADD COLUMN reasoning TEXT"),
    ] {
        if !column_exists(db, "chat_messages", col).await {
            if let Err(e) = db
                .execute(Statement::from_string(backend, ddl.to_owned()))
                .await
            {
                tracing::warn!("ALTER chat_messages.{col} 失败(忽略): {e}");
            }
        }
    }

    // 兼容已建的 chat_memories 表:补向量检索列(RAG)。embedding/embed_model 可空(未生成时为 NULL),
    // pinned 默认 0;旧库走 ALTER,新库走 entity DDL。
    for (col, ddl) in [
        ("embedding", "ALTER TABLE chat_memories ADD COLUMN embedding TEXT"),
        ("embed_model", "ALTER TABLE chat_memories ADD COLUMN embed_model TEXT"),
        ("pinned", "ALTER TABLE chat_memories ADD COLUMN pinned BOOLEAN NOT NULL DEFAULT 0"),
        // 记忆模块深化:分类 + 重要度/置信度打分 + 命中计数/时间衰减(检索排序与淘汰用)
        ("mem_type", "ALTER TABLE chat_memories ADD COLUMN mem_type TEXT NOT NULL DEFAULT 'other'"),
        ("importance", "ALTER TABLE chat_memories ADD COLUMN importance INTEGER NOT NULL DEFAULT 3"),
        ("confidence", "ALTER TABLE chat_memories ADD COLUMN confidence INTEGER NOT NULL DEFAULT 3"),
        ("hit_count", "ALTER TABLE chat_memories ADD COLUMN hit_count INTEGER NOT NULL DEFAULT 0"),
        ("last_hit_at", "ALTER TABLE chat_memories ADD COLUMN last_hit_at INTEGER NOT NULL DEFAULT 0"),
    ] {
        if !column_exists(db, "chat_memories", col).await {
            if let Err(e) = db
                .execute(Statement::from_string(backend, ddl.to_owned()))
                .await
            {
                tracing::warn!("ALTER chat_memories.{col} 失败(忽略): {e}");
            }
        }
    }

    // 兼容已建的 authors 表:补黑名单开关列(旧行默认 0 = 未拉黑)
    if !column_exists(db, "authors", "is_blacklisted").await {
        if let Err(e) = db
            .execute(Statement::from_string(
                backend,
                "ALTER TABLE authors ADD COLUMN is_blacklisted BOOLEAN NOT NULL DEFAULT 0"
                    .to_owned(),
            ))
            .await
        {
            tracing::warn!("ALTER authors.is_blacklisted 失败(忽略): {e}");
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
        "CREATE UNIQUE INDEX IF NOT EXISTS idx_users_username ON users(username)",
        "CREATE INDEX IF NOT EXISTS idx_contents_task ON contents(task_id)",
        "CREATE INDEX IF NOT EXISTS idx_comments_task ON comments(task_id)",
        "CREATE INDEX IF NOT EXISTS idx_comments_content ON comments(content_id)",
        "CREATE INDEX IF NOT EXISTS idx_collect_logs_task ON collect_logs(task_id, ts)",
        "CREATE INDEX IF NOT EXISTS idx_content_synced_users_user ON content_synced_users(synced_user)",
        "CREATE INDEX IF NOT EXISTS idx_content_synced_users_content ON content_synced_users(content_id)",
        "CREATE INDEX IF NOT EXISTS idx_authors_owner ON authors(owner)",
        "CREATE INDEX IF NOT EXISTS idx_authors_platform_uid ON authors(platform, uid)",
        "CREATE INDEX IF NOT EXISTS idx_authors_monitored ON authors(is_monitored)",
        "CREATE INDEX IF NOT EXISTS idx_authors_blacklisted ON authors(is_blacklisted)",
        // 复合索引:全量库/评论库按 owner 过滤 + collected_at 倒序的列表与趋势查询,
        // 以及内容详情的作者维度聚合(owner+platform+author_uid)、意向客资筛选(intent_level)。
        // 大库(十万行级)下这些查询无索引会退化为全表扫描 + 文件排序。
        "CREATE INDEX IF NOT EXISTS idx_contents_owner_collected ON contents(owner, collected_at)",
        "CREATE INDEX IF NOT EXISTS idx_contents_collected ON contents(collected_at)",
        "CREATE INDEX IF NOT EXISTS idx_contents_owner_platform_author ON contents(owner, platform, author_uid)",
        "CREATE INDEX IF NOT EXISTS idx_comments_owner_collected ON comments(owner, collected_at)",
        "CREATE INDEX IF NOT EXISTS idx_comments_collected ON comments(collected_at)",
        "CREATE INDEX IF NOT EXISTS idx_comments_intent ON comments(intent_level)",
        "CREATE INDEX IF NOT EXISTS idx_chat_conversations_owner ON chat_conversations(owner, updated_at)",
        "CREATE INDEX IF NOT EXISTS idx_chat_messages_conversation ON chat_messages(conversation_id, id)",
        "CREATE INDEX IF NOT EXISTS idx_chat_memories_owner ON chat_memories(owner, updated_at)",
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
