//! 多账号 Cookie 池(SeaORM 版)。
//!
//! 职责:持久化每个平台的多个账号 Cookie,按「最久未用优先」轮换以分摊风控压力。
//!
//! 存储经 SeaORM 落到全局数据库连接(运行时二选一 SQLite / PostgreSQL),
//! 账号操作非热路径,直接走连接池即可。
//!
//! ⚠️ 安全注意:Cookie 明文存储在 DB 中(SQLite 文件无加密),任何人拿到 veltrix.db
//! 即可获取所有已登录账号的会话凭据。若有安全需求,建议后续加 OS keychain 加密。

// 账号轮换/风控降级方法待调度引擎接入,暂保留
#![allow(dead_code)]

use veltrix_core::db::entity::account::{self, Entity as AccountEntity};
use veltrix_core::error::{CrawlerError, Result};
use chrono::Utc;
use sea_orm::sea_query::OnConflict;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, QueryOrder,
    QuerySelect, Set,
};

/// 账号状态。失效 / 停用需人工处理;历史版本曾有「冷却」状态,启动时已统一归并为 active。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountStatus {
    /// 可用。
    Active,
    /// 登录态失效(Cookie 过期),需重新登录。
    Invalid,
    /// 人工停用,保留记录但不参与轮换。
    Disabled,
}

impl AccountStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            AccountStatus::Active => "active",
            AccountStatus::Invalid => "invalid",
            AccountStatus::Disabled => "disabled",
        }
    }

    fn from_str(s: &str) -> Self {
        match s {
            "invalid" => AccountStatus::Invalid,
            "disabled" => AccountStatus::Disabled,
            // 含历史遗留的 "cooldown":一律按可用处理(冷却机制已下线)
            _ => AccountStatus::Active,
        }
    }
}

/// 一个账号记录。`cookie` 为完整 Cookie 串,由 WebView 登录后回写。
#[derive(Debug, Clone)]
pub struct Account {
    pub id: String,
    pub platform: String,
    /// 人工备注 / 登录后回填的昵称,便于在前端识别。
    pub label: String,
    pub cookie: String,
    pub status: AccountStatus,
    /// 累计风控次数(只读展示;风控冷却机制已下线,不再据此变更状态)。
    pub risk_count: i64,
    /// 最后一次被取用的 Unix 秒,轮换据此选「最久未用」。
    pub last_used_at: i64,
    pub created_at: i64,
    /// 业务编码(如 ACC-XXXX),由账号管理写入,采集回写时不覆盖。
    pub code: String,
    /// 人工备注,由账号管理维护。
    pub remark: String,
    /// 归属用户(创建者),用于按用户隔离数据。
    pub owner: String,
}

impl From<account::Model> for Account {
    fn from(m: account::Model) -> Self {
        Account {
            id: m.id,
            platform: m.platform,
            label: m.label,
            cookie: m.cookie,
            status: AccountStatus::from_str(&m.status),
            risk_count: m.risk_count,
            last_used_at: m.last_used_at,
            created_at: m.created_at,
            code: m.code,
            remark: m.remark,
            owner: m.owner,
        }
    }
}

/// Cookie 池。持有全局数据库连接(`DatabaseConnection` 内部为 Arc,克隆共享)。
pub struct CookiePool {
    db: DatabaseConnection,
}

impl CookiePool {
    pub fn new(db: DatabaseConnection) -> Self {
        Self { db }
    }

    /// 新增 / 覆盖一个账号(登录成功或前端导入 Cookie 时调用)。
    /// ON CONFLICT 保留 created_at,仅更新其余字段。
    pub async fn upsert(&self, account: &Account) -> Result<()> {
        let model = account::ActiveModel {
            id: Set(account.id.clone()),
            platform: Set(account.platform.clone()),
            label: Set(account.label.clone()),
            cookie: Set(account.cookie.clone()),
            status: Set(account.status.as_str().to_string()),
            risk_count: Set(account.risk_count),
            // 列保留(历史 schema),冷却机制下线后恒写 0
            cooldown_until: Set(0),
            last_used_at: Set(account.last_used_at),
            created_at: Set(account.created_at),
            // 新建时写入 code/remark/owner;采集登录回写走 on_conflict,不更新这三列(见下)
            code: Set(account.code.clone()),
            remark: Set(account.remark.clone()),
            owner: Set(account.owner.clone()),
        };
        AccountEntity::insert(model)
            .on_conflict(
                // 采集登录回写时,这三个字段(code/remark/owner)会是占位空值,
                // 故 on_conflict 刻意不更新它们,避免覆盖账号管理已维护的内容。
                OnConflict::column(account::Column::Id)
                    .update_columns([
                        account::Column::Platform,
                        account::Column::Label,
                        account::Column::Cookie,
                        account::Column::Status,
                        account::Column::RiskCount,
                        account::Column::CooldownUntil,
                        account::Column::LastUsedAt,
                    ])
                    .to_owned(),
            )
            .exec(&self.db)
            .await
            .map_err(|e| CrawlerError::Account(format!("写入账号失败: {e}")))?;
        Ok(())
    }

    /// 取一个可用账号并占用(更新 last_used_at,实现轮换公平性)。
    ///
    /// 选取规则:平台匹配 + 状态 active → 取 last_used_at 最小者。
    ///
    /// 并发安全:候选选出后用 `last_used_at = old_last_used_at` 做乐观 CAS,
    /// 更新影响 0 行表示其他线程已抢走,重试到 MAX_RETRIES 上限。
    pub async fn acquire(&self, platform: &str) -> Result<Account> {
        const MAX_RETRIES: usize = 5;
        let now = Utc::now().timestamp();
        for _ in 0..MAX_RETRIES {
            let model = AccountEntity::find()
                .filter(account::Column::Platform.eq(platform))
                .filter(account::Column::Status.eq(AccountStatus::Active.as_str()))
                .order_by_asc(account::Column::LastUsedAt)
                .one(&self.db)
                .await
                .map_err(|e| CrawlerError::Account(format!("查询账号失败: {e}")))?
                .ok_or_else(|| CrawlerError::Account(format!("平台 {platform} 无可用账号")))?;

            let snapshot_last_used = model.last_used_at;
            let snapshot: Account = model.clone().into();

            // 乐观 CAS:只在 last_used_at 仍是候选时刻的值时更新
            let res = AccountEntity::update_many()
                .col_expr(account::Column::LastUsedAt, sea_orm::sea_query::Expr::value(now))
                .filter(account::Column::Id.eq(model.id.clone()))
                .filter(account::Column::LastUsedAt.eq(snapshot_last_used))
                .exec(&self.db)
                .await
                .map_err(|e| CrawlerError::Account(format!("占用账号失败: {e}")))?;
            if res.rows_affected == 1 {
                return Ok(snapshot);
            }
            // 0 行 = 已被其他线程抢走,重新挑下一个
        }
        Err(CrawlerError::Account(
            "并发争用激烈,获取账号失败,请稍后重试".into(),
        ))
    }

    /// 占用任务指定的账号(区别于 acquire 的自动轮换):校验存在、平台匹配、状态可用,
    /// 占用方式与 acquire 相同(CAS 更新 last_used_at,参与后续轮换公平性)。
    pub async fn acquire_by_id(&self, account_id: &str, platform: &str) -> Result<Account> {
        const MAX_RETRIES: usize = 5;
        let now = Utc::now().timestamp();
        for _ in 0..MAX_RETRIES {
            let model = AccountEntity::find_by_id(account_id)
                .one(&self.db)
                .await
                .map_err(|e| CrawlerError::Account(format!("查询账号失败: {e}")))?
                .ok_or_else(|| {
                    CrawlerError::Account(format!("指定账号不存在(可能已被删除): {account_id}"))
                })?;
            if model.platform != platform {
                return Err(CrawlerError::Account(format!(
                    "指定账号属于平台 {},与任务平台 {platform} 不匹配",
                    model.platform
                )));
            }
            if model.status != AccountStatus::Active.as_str() {
                return Err(CrawlerError::Account(format!(
                    "指定账号「{}」当前不可用(状态: {}),请在账号管理处理后重试,或改用自动轮换",
                    model.label, model.status
                )));
            }

            let snapshot_last_used = model.last_used_at;
            let snapshot: Account = model.clone().into();

            // 乐观 CAS:只在 last_used_at 仍是候选时刻的值时更新
            let res = AccountEntity::update_many()
                .col_expr(account::Column::LastUsedAt, sea_orm::sea_query::Expr::value(now))
                .filter(account::Column::Id.eq(model.id.clone()))
                .filter(account::Column::LastUsedAt.eq(snapshot_last_used))
                .exec(&self.db)
                .await
                .map_err(|e| CrawlerError::Account(format!("占用账号失败: {e}")))?;
            if res.rows_affected == 1 {
                return Ok(snapshot);
            }
            // 0 行 = 已被其他线程抢走,重新读取再试
        }
        Err(CrawlerError::Account(
            "并发争用激烈,获取账号失败,请稍后重试".into(),
        ))
    }

    /// 标记账号登录态失效(Cookie 过期),需重新登录。账号不存在时静默。
    pub async fn mark_invalid(&self, account_id: &str) -> Result<()> {
        if let Some(model) = AccountEntity::find_by_id(account_id)
            .one(&self.db)
            .await
            .map_err(|e| CrawlerError::Account(format!("查询账号失败: {e}")))?
        {
            let mut am: account::ActiveModel = model.into();
            am.status = Set(AccountStatus::Invalid.as_str().to_string());
            am.update(&self.db)
                .await
                .map_err(|e| CrawlerError::Account(format!("标记失效失败: {e}")))?;
        }
        Ok(())
    }

    /// 标记账号已登录可用:扫码登录完成后置 active,清零风控计数,
    /// 并把 last_used_at 更新为当前时间(「最近使用」以登录成功为准,而非仅采集占用)。
    pub async fn mark_active(&self, account_id: &str) -> Result<()> {
        if let Some(model) = AccountEntity::find_by_id(account_id)
            .one(&self.db)
            .await
            .map_err(|e| CrawlerError::Account(format!("查询账号失败: {e}")))?
        {
            let now = Utc::now().timestamp();
            let mut am: account::ActiveModel = model.into();
            am.status = Set(AccountStatus::Active.as_str().to_string());
            am.risk_count = Set(0);
            am.cooldown_until = Set(0);
            am.last_used_at = Set(now);
            am.update(&self.db)
                .await
                .map_err(|e| CrawlerError::Account(format!("标记账号可用失败: {e}")))?;
        }
        Ok(())
    }

    /// 正常归还账号:成功采集后重置风控计数,体现账号「健康」。
    pub async fn release_ok(&self, account_id: &str) -> Result<()> {
        if let Some(model) = AccountEntity::find_by_id(account_id)
            .one(&self.db)
            .await
            .map_err(|e| CrawlerError::Account(format!("查询账号失败: {e}")))?
        {
            // 仅对 active 账号重置,避免覆盖失效 / 停用状态
            if model.status == AccountStatus::Active.as_str() {
                let mut am: account::ActiveModel = model.into();
                am.risk_count = Set(0);
                am.update(&self.db)
                    .await
                    .map_err(|e| CrawlerError::Account(format!("重置风控计数失败: {e}")))?;
            }
        }
        Ok(())
    }

    /// 列出某平台全部账号,供前端账号管理界面展示。
    pub async fn list(&self, platform: &str) -> Result<Vec<Account>> {
        // 单平台最多 1000 个账号,超出走分页接口
        const HARD_CAP: u64 = 1000;
        let models = AccountEntity::find()
            .filter(account::Column::Platform.eq(platform))
            .order_by_asc(account::Column::CreatedAt)
            .limit(HARD_CAP)
            .all(&self.db)
            .await
            .map_err(|e| CrawlerError::Account(format!("列出账号失败: {e}")))?;
        Ok(models.into_iter().map(Into::into).collect())
    }

    /// 按 id 取单个账号(登录检测回写后取 platform 用于通知前端)。不存在返回 None。
    pub async fn get(&self, account_id: &str) -> Result<Option<Account>> {
        let model = AccountEntity::find_by_id(account_id)
            .one(&self.db)
            .await
            .map_err(|e| CrawlerError::Account(format!("查询账号失败: {e}")))?;
        Ok(model.map(Into::into))
    }

    /// 删除账号。
    pub async fn remove(&self, account_id: &str) -> Result<bool> {
        let res = AccountEntity::delete_by_id(account_id)
            .exec(&self.db)
            .await
            .map_err(|e| CrawlerError::Account(format!("删除账号失败: {e}")))?;
        Ok(res.rows_affected > 0)
    }
}
