//! 发布账号池(SeaORM 版)。
//!
//! 职责:发布账号的 CRUD、登录态(active / invalid)维护;分组维度直接复用
//! CRM 客户(customers 表,运营 > 客户管理维护),发布侧不再单独维护分类。
//! 与 `cookie::CookiePool` 同构(持有全局 DatabaseConnection),但**不做轮换 / 占用**——
//! 发布是「写」操作,账号由用户显式指定,不参与采集侧的最久未用分摊逻辑。
//!
//! ⚠️ 安全注意:Cookie 明文存 DB(与采集账号池同一风险口径),禁止打印日志。

use chrono::Utc;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, IntoActiveModel,
    PaginatorTrait, QueryFilter, QueryOrder, QuerySelect, Set,
};
use uuid::Uuid;
use veltrix_core::db::entity::customer::Entity as CustomerEntity;
use veltrix_core::db::entity::publish_account::{self, Entity as PublishAccountEntity};
use veltrix_core::error::{CrawlerError, Result};

/// 业务编码字符集:去掉易混淆的 0/O/1/I/L,与前端 generateCode(ACC-XXXX)同口径。
const CODE_CHARS: &[u8] = b"ABCDEFGHJKMNPQRSTUVWXYZ23456789";

/// 生成发布账号业务编码(PAC-XXXXXX)。随机源复用 uuid(避免引入 rand 依赖)。
fn gen_code() -> String {
    let bytes = Uuid::new_v4().into_bytes();
    let suffix: String = bytes[..6]
        .iter()
        .map(|b| CODE_CHARS[(*b as usize) % CODE_CHARS.len()] as char)
        .collect();
    format!("PAC-{suffix}")
}

/// 新建发布账号的入参(集中成结构体以遵守「参数 ≤ 4」)。
pub struct NewAccount {
    pub platform: String,
    pub category_id: String,
    pub label: String,
    pub owner: String,
}

/// 发布账号池。持有全局数据库连接(`DatabaseConnection` 内部为 Arc,克隆共享)。
pub struct PublishAccounts {
    db: DatabaseConnection,
}

impl PublishAccounts {
    pub fn new(db: DatabaseConnection) -> Self {
        Self { db }
    }

    // ===================== 客户(分组维度) =====================

    /// 校验客户存在:账号必须归属真实客户(CRM customers 表;空串/失效 id 一律拒绝)。
    async fn require_customer(&self, category_id: &str) -> Result<()> {
        let exists = CustomerEntity::find_by_id(category_id)
            .count(&self.db)
            .await
            .map_err(|e| CrawlerError::Config(format!("校验客户失败: {e}")))?
            > 0;
        if !exists {
            return Err(CrawlerError::Config("请先选择有效的客户".into()));
        }
        Ok(())
    }

    // ===================== 账号 =====================

    /// 列出账号:Some(category_id) 按客户过滤,None = 全部。
    pub async fn list_accounts(
        &self,
        category_id: Option<&str>,
    ) -> Result<Vec<publish_account::Model>> {
        // 单用户账号量级有限,与采集账号池同口径设硬上限防意外全表扫
        const HARD_CAP: u64 = 1000;
        let mut query = PublishAccountEntity::find();
        if let Some(cid) = category_id {
            query = query.filter(publish_account::Column::CategoryId.eq(cid));
        }
        query
            .order_by_asc(publish_account::Column::CreatedAt)
            .limit(HARD_CAP)
            .all(&self.db)
            .await
            .map_err(|e| CrawlerError::Account(format!("查询发布账号失败: {e}")))
    }

    /// 按 id 取单个账号(登录回写 / 开窗前查 platform 用)。不存在返回 None。
    pub async fn get(&self, account_id: &str) -> Result<Option<publish_account::Model>> {
        PublishAccountEntity::find_by_id(account_id)
            .one(&self.db)
            .await
            .map_err(|e| CrawlerError::Account(format!("查询发布账号失败: {e}")))
    }

    /// 新建发布账号:初始状态 invalid(未登录,显示「去登录」),登录窗口报 "in" 后转 active。
    pub async fn create_account(&self, input: NewAccount) -> Result<publish_account::Model> {
        self.require_customer(&input.category_id).await?;
        let now = Utc::now().timestamp();
        let model = publish_account::ActiveModel {
            id: Set(Uuid::new_v4().to_string()),
            platform: Set(input.platform),
            category_id: Set(input.category_id),
            label: Set(input.label),
            // 平台侧昵称 / 头像 / uid 待登录后回填,建号时一律空串
            nickname: Set(String::new()),
            avatar: Set(String::new()),
            uid: Set(String::new()),
            cookie: Set(String::new()),
            status: Set("invalid".to_string()),
            fail_reason: Set(String::new()),
            code: Set(gen_code()),
            last_login_at: Set(0),
            last_publish_at: Set(0),
            today_published: Set(0),
            today_date: Set(String::new()),
            owner: Set(input.owner),
            created_at: Set(now),
        }
        .insert(&self.db)
        .await
        .map_err(|e| CrawlerError::Account(format!("创建发布账号失败: {e}")))?;
        Ok(model)
    }

    /// 更新账号的归属客户与备注名;cookie / 状态 / 归属等登录与运营字段保持不变。
    pub async fn update_account(
        &self,
        account_id: &str,
        category_id: &str,
        label: &str,
    ) -> Result<publish_account::Model> {
        self.require_customer(category_id).await?;
        let model = PublishAccountEntity::find_by_id(account_id)
            .one(&self.db)
            .await
            .map_err(|e| CrawlerError::Account(format!("查询发布账号失败: {e}")))?
            .ok_or_else(|| CrawlerError::Account(format!("发布账号不存在: {account_id}")))?;
        let mut am = model.into_active_model();
        am.category_id = Set(category_id.to_string());
        am.label = Set(label.to_string());
        am.update(&self.db)
            .await
            .map_err(|e| CrawlerError::Account(format!("更新发布账号失败: {e}")))
    }

    /// 删除账号。调用方负责先关闭其登录窗口(避免 WebView 句柄泄漏)。
    pub async fn remove(&self, account_id: &str) -> Result<bool> {
        let res = PublishAccountEntity::delete_by_id(account_id)
            .exec(&self.db)
            .await
            .map_err(|e| CrawlerError::Account(format!("删除发布账号失败: {e}")))?;
        Ok(res.rows_affected > 0)
    }

    /// 标记登录态可用:登录自检报 "in" 时调用,刷新 last_login_at 并清掉历史失败原因。
    pub async fn mark_active(&self, account_id: &str) -> Result<()> {
        if let Some(model) = self.get(account_id).await? {
            let mut am = model.into_active_model();
            am.status = Set("active".to_string());
            am.fail_reason = Set(String::new());
            am.last_login_at = Set(Utc::now().timestamp());
            am.update(&self.db)
                .await
                .map_err(|e| CrawlerError::Account(format!("标记发布账号可用失败: {e}")))?;
        }
        Ok(())
    }

    /// 标记登录态失效(关窗时从未报 "in",或后续发布链路发现 Cookie 过期)。
    pub async fn mark_invalid(&self, account_id: &str, reason: &str) -> Result<()> {
        if let Some(model) = self.get(account_id).await? {
            let mut am = model.into_active_model();
            am.status = Set("invalid".to_string());
            am.fail_reason = Set(reason.to_string());
            am.update(&self.db)
                .await
                .map_err(|e| CrawlerError::Account(format!("标记发布账号失效失败: {e}")))?;
        }
        Ok(())
    }

    /// 回写会话 Cookie(登录窗口报 "in" 后从存活 WebView 读出)。
    /// 单独成方法:Cookie 读取可能失败(窗口已关 / 超时),失败不应回滚已置的 active 状态。
    pub async fn set_cookie(&self, account_id: &str, cookie: &str) -> Result<()> {
        if let Some(model) = self.get(account_id).await? {
            let mut am = model.into_active_model();
            am.cookie = Set(cookie.to_string());
            am.update(&self.db)
                .await
                .map_err(|e| CrawlerError::Account(format!("回写发布账号 Cookie 失败: {e}")))?;
        }
        Ok(())
    }
}
