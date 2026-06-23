//! 用户 / 模型厂商 / 提示词的 CRUD 命令(系统管理 + 系统配置)。
//!
//! ID 由前端生成(crypto.randomUUID)并随请求传入,后端按 `id` 是否已存在区分新增 / 更新。
//! 用户密码用 argon2 哈希存储,接口一律不回传哈希。逻辑外键,无物理 FK。

use crate::commands::AppState;
use veltrix_core::db::entity::{
    customer, industry, keyword, prompt, provider, user,
};
use veltrix_core::error::{CrawlerError, Result};
use argon2::password_hash::rand_core::OsRng;
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;
use chrono::Utc;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, IntoActiveModel,
    PaginatorTrait, QueryFilter, QueryOrder, QuerySelect, Set,
};

/// 单次 list 接口最多返回 N 行,防 IPC 噎住;数据量超出应改分页接口
const LIST_HARD_CAP: u64 = 1000;
use serde::{Deserialize, Serialize};
use tauri::State;

/// 用 argon2 哈希密码(随机盐)。
fn hash_password(pw: &str) -> Result<String> {
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(pw.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|e| CrawlerError::Config(format!("密码哈希失败: {e}")))
}

/// 校验某用户名的明文密码(argon2)。用户不存在 / 已删 / 密码不符一律返回 Auth 错误,
/// 不区分原因以免被探测用户是否存在。供危险操作(如清空数据)前的二次身份确认复用。
pub(crate) async fn verify_user_password(
    db: &DatabaseConnection,
    username: &str,
    password: &str,
) -> Result<()> {
    let model = user::Entity::find()
        .filter(user::Column::Username.eq(username))
        .filter(user::Column::DeletedAt.eq(0))
        .one(db)
        .await
        .map_err(|e| CrawlerError::Config(format!("查询用户失败: {e}")))?
        .ok_or_else(|| CrawlerError::Auth("密码错误".into()))?;
    let parsed = PasswordHash::new(&model.password_hash)
        .map_err(|_| CrawlerError::Auth("密码错误".into()))?;
    Argon2::default()
        .verify_password(password.as_bytes(), &parsed)
        .map_err(|_| CrawlerError::Auth("密码错误".into()))?;
    Ok(())
}

// ===================== 用户 =====================

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UserView {
    pub id: String,
    pub username: String,
    pub email: String,
    pub nickname: String,
    pub avatar: String,
    pub remark: String,
    pub status: String,
    pub data_scope: String,
    /// 是否初始化创建的超级管理员(最早创建的用户);前端据此禁止禁用 / 改数据级别
    pub is_super_admin: bool,
    pub created_at: i64,
    pub updated_at: i64,
}

impl From<user::Model> for UserView {
    fn from(m: user::Model) -> Self {
        Self {
            id: m.id,
            username: m.username,
            email: m.email,
            nickname: m.nickname,
            avatar: m.avatar,
            remark: m.remark,
            status: m.status,
            data_scope: m.data_scope,
            is_super_admin: false,
            created_at: m.created_at,
            updated_at: m.updated_at,
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserInput {
    pub id: String,
    pub username: String,
    /// 新建必填;编辑留空表示不修改密码。
    #[serde(default)]
    pub password: String,
    pub email: String,
    pub nickname: String,
    pub avatar: String,
    pub remark: String,
    pub status: String,
    pub data_scope: String,
}

#[tauri::command]
pub async fn list_users(state: State<'_, AppState>) -> Result<Vec<UserView>> {
    let rows = user::Entity::find()
        .filter(user::Column::DeletedAt.eq(0))
        .order_by_asc(user::Column::CreatedAt)
        .limit(LIST_HARD_CAP)
        .all(&state.db)
        .await
        .map_err(|e| CrawlerError::Config(format!("查询用户失败: {e}")))?;
    // 已按 created_at 升序,第一条即初始化创建的超级管理员
    let views = rows
        .into_iter()
        .enumerate()
        .map(|(i, m)| {
            let mut v: UserView = m.into();
            v.is_super_admin = i == 0;
            v
        })
        .collect();
    Ok(views)
}

/// 最早创建(未删)的用户 id —— 即初始化时创建的超级管理员
async fn earliest_user_id(db: &sea_orm::DatabaseConnection) -> Option<String> {
    user::Entity::find()
        .filter(user::Column::DeletedAt.eq(0))
        .order_by_asc(user::Column::CreatedAt)
        .one(db)
        .await
        .ok()
        .flatten()
        .map(|m| m.id)
}

#[tauri::command]
pub async fn upsert_user(state: State<'_, AppState>, user: UserInput) -> Result<()> {
    let db = &state.db;
    let now = Utc::now().timestamp();
    let existing = user::Entity::find_by_id(user.id.clone())
        .one(db)
        .await
        .map_err(|e| CrawlerError::Config(format!("查询用户失败: {e}")))?;

    match existing {
        Some(model) => {
            // 初始化超级管理员(最早创建)强制保持启用 + 全部数据,防止前端被绕过禁用 / 降级
            let is_super = earliest_user_id(db).await.as_deref() == Some(model.id.as_str());
            let mut am = model.into_active_model();
            am.username = Set(user.username);
            am.email = Set(user.email);
            am.nickname = Set(user.nickname);
            am.avatar = Set(user.avatar);
            am.remark = Set(user.remark);
            am.status = Set(if is_super {
                "enabled".to_string()
            } else {
                user.status
            });
            am.data_scope = Set(if is_super {
                "all".to_string()
            } else {
                user.data_scope
            });
            am.updated_at = Set(now);
            if !user.password.is_empty() {
                am.password_hash = Set(hash_password(&user.password)?);
            }
            am.update(db)
                .await
                .map_err(|e| CrawlerError::Config(format!("更新用户失败: {e}")))?;
        }
        None => {
            if user.password.is_empty() {
                return Err(CrawlerError::Config("新建用户需设置密码".into()));
            }
            let am = user::ActiveModel {
                id: Set(user.id),
                username: Set(user.username),
                password_hash: Set(hash_password(&user.password)?),
                email: Set(user.email),
                nickname: Set(user.nickname),
                avatar: Set(user.avatar),
                remark: Set(user.remark),
                status: Set(user.status),
                data_scope: Set(user.data_scope),
                // Obsidian vault 由用户在系统设置单独配置,新建时留空
                obsidian_vault_path: Set(String::new()),
                created_at: Set(now),
                updated_at: Set(now),
                deleted_at: Set(0),
            };
            am.insert(db)
                .await
                .map_err(|e| CrawlerError::Config(format!("创建用户失败: {e}")))?;
        }
    }
    Ok(())
}

#[tauri::command]
pub async fn remove_user(state: State<'_, AppState>, id: String) -> Result<()> {
    let db = &state.db;
    let now = Utc::now().timestamp();
    if let Some(model) = user::Entity::find_by_id(id)
        .one(db)
        .await
        .map_err(|e| CrawlerError::Config(format!("查询用户失败: {e}")))?
    {
        let mut am = model.into_active_model();
        am.deleted_at = Set(now); // 软删除
        am.updated_at = Set(now);
        am.update(db)
            .await
            .map_err(|e| CrawlerError::Config(format!("删除用户失败: {e}")))?;
    }
    Ok(())
}

#[tauri::command]
pub async fn reset_user_password(
    state: State<'_, AppState>,
    id: String,
    password: String,
) -> Result<()> {
    let db = &state.db;
    let model = user::Entity::find_by_id(id)
        .one(db)
        .await
        .map_err(|e| CrawlerError::Config(format!("查询用户失败: {e}")))?
        .ok_or_else(|| CrawlerError::Config("用户不存在".into()))?;
    let mut am = model.into_active_model();
    am.password_hash = Set(hash_password(&password)?);
    am.updated_at = Set(Utc::now().timestamp());
    am.update(db)
        .await
        .map_err(|e| CrawlerError::Config(format!("重置密码失败: {e}")))?;
    Ok(())
}

/// 校验本地恢复的登录态:用户仍存在且启用则返回其最新 dataScope,否则返回 None。
/// 清库 / 删用户 / 禁用后,前端 localStorage 里的旧登录态必须据此作废,不能直接放行。
#[tauri::command]
pub async fn verify_session_user(
    state: State<'_, AppState>,
    username: String,
) -> Result<Option<String>> {
    let found = user::Entity::find()
        .filter(user::Column::Username.eq(username))
        .filter(user::Column::DeletedAt.eq(0))
        .one(&state.db)
        .await
        .map_err(|e| CrawlerError::Config(format!("校验用户失败: {e}")))?;
    Ok(found
        .filter(|u| u.status == "enabled")
        .map(|u| u.data_scope))
}

/// 是否已存在任意用户(供前端判断首次启动是否走初始化向导)。
#[tauri::command]
pub async fn has_users(state: State<'_, AppState>) -> Result<bool> {
    let count = user::Entity::find()
        .filter(user::Column::DeletedAt.eq(0))
        .count(&state.db)
        .await
        .map_err(|e| CrawlerError::Config(format!("查询用户失败: {e}")))?;
    Ok(count > 0)
}

/// 登录:按用户名查未删用户,argon2 校验密码,成功返回用户视图(含 dataScope)。
#[tauri::command]
pub async fn login(
    state: State<'_, AppState>,
    username: String,
    password: String,
) -> Result<UserView> {
    let model = user::Entity::find()
        .filter(user::Column::Username.eq(username))
        .filter(user::Column::DeletedAt.eq(0))
        .one(&state.db)
        .await
        .map_err(|e| CrawlerError::Config(format!("查询用户失败: {e}")))?
        .ok_or_else(|| CrawlerError::Auth("用户名或密码错误".into()))?;
    if model.status != "enabled" {
        return Err(CrawlerError::Auth("账号已被禁用".into()));
    }
    let parsed = PasswordHash::new(&model.password_hash)
        .map_err(|_| CrawlerError::Auth("用户名或密码错误".into()))?;
    Argon2::default()
        .verify_password(password.as_bytes(), &parsed)
        .map_err(|_| CrawlerError::Auth("用户名或密码错误".into()))?;
    let mut view: UserView = model.into();
    view.is_super_admin =
        earliest_user_id(&state.db).await.as_deref() == Some(view.id.as_str());
    Ok(view)
}

/// 当前登录用户修改自己的密码:argon2 校验旧密码,通过后写入新密码哈希。
#[tauri::command]
pub async fn change_password(
    state: State<'_, AppState>,
    old_password: String,
    new_password: String,
) -> Result<()> {
    let current = super::current_user(&state)
        .ok_or_else(|| CrawlerError::Auth("未登录,无法修改密码".into()))?;
    if new_password.chars().count() < 6 {
        return Err(CrawlerError::Config("新密码至少 6 位".into()));
    }
    // 复用统一的 argon2 校验:旧密码错误会直接返回 Err
    verify_user_password(&state.db, &current.name, &old_password).await?;
    let model = user::Entity::find()
        .filter(user::Column::Username.eq(current.name.as_str()))
        .filter(user::Column::DeletedAt.eq(0))
        .one(&state.db)
        .await
        .map_err(|e| CrawlerError::Config(format!("查询用户失败: {e}")))?
        .ok_or_else(|| CrawlerError::Auth("用户不存在".into()))?;
    let mut am = model.into_active_model();
    am.password_hash = Set(hash_password(&new_password)?);
    am.updated_at = Set(Utc::now().timestamp());
    am.update(&state.db)
        .await
        .map_err(|e| CrawlerError::Config(format!("修改密码失败: {e}")))?;
    Ok(())
}

/// 当前登录用户修改自己的资料(昵称/邮箱/头像/备注);用户名、数据范围、超管标识不可改。
#[tauri::command]
pub async fn update_profile(
    state: State<'_, AppState>,
    nickname: String,
    email: String,
    avatar: String,
    remark: String,
) -> Result<UserView> {
    let current = super::current_user(&state)
        .ok_or_else(|| CrawlerError::Auth("未登录,无法修改资料".into()))?;
    let model = user::Entity::find()
        .filter(user::Column::Username.eq(current.name.as_str()))
        .filter(user::Column::DeletedAt.eq(0))
        .one(&state.db)
        .await
        .map_err(|e| CrawlerError::Config(format!("查询用户失败: {e}")))?
        .ok_or_else(|| CrawlerError::Auth("用户不存在".into()))?;
    let mut am = model.into_active_model();
    am.nickname = Set(nickname);
    am.email = Set(email);
    am.avatar = Set(avatar);
    am.remark = Set(remark);
    am.updated_at = Set(Utc::now().timestamp());
    let updated = am
        .update(&state.db)
        .await
        .map_err(|e| CrawlerError::Config(format!("修改资料失败: {e}")))?;
    let mut view: UserView = updated.into();
    view.is_super_admin =
        earliest_user_id(&state.db).await.as_deref() == Some(view.id.as_str());
    Ok(view)
}

// ===================== 模型厂商 =====================

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderDto {
    pub id: String,
    pub code: String,
    pub name: String,
    pub api_url: String,
    pub api_key: String,
    /// 结构化模型列表(名称 + 能力集合);列里以 JSON 存,经 parse/serialize 转换。
    pub models: Vec<crate::llm::provider::ModelSpec>,
}

/// api_key 列表展示用打码:保留尾部 4 位,前面打码
fn mask_api_key(key: &str) -> String {
    if key.is_empty() {
        return String::new();
    }
    let tail: String = key.chars().rev().take(4).collect::<String>().chars().rev().collect();
    if tail.chars().count() < key.chars().count() {
        format!("••••••{tail}")
    } else {
        // 4 字符及以下,全部打码
        "••••".to_string()
    }
}

impl From<provider::Model> for ProviderDto {
    fn from(m: provider::Model) -> Self {
        Self {
            id: m.id,
            code: m.code,
            name: m.name,
            api_url: m.api_url,
            api_key: mask_api_key(&m.api_key),
            models: crate::llm::provider::parse_models(&m.models),
        }
    }
}

#[tauri::command]
pub async fn list_providers(state: State<'_, AppState>) -> Result<Vec<ProviderDto>> {
    let rows = provider::Entity::find()
        .order_by_asc(provider::Column::CreatedAt)
        .limit(LIST_HARD_CAP)
        .all(&state.db)
        .await
        .map_err(|e| CrawlerError::Config(format!("查询厂商失败: {e}")))?;
    Ok(rows.into_iter().map(Into::into).collect())
}

#[tauri::command]
pub async fn upsert_provider(state: State<'_, AppState>, provider: ProviderDto) -> Result<()> {
    let db = &state.db;
    let now = Utc::now().timestamp();
    // 编码须全表唯一(排除自身),避免重复编码
    let dup = provider::Entity::find()
        .filter(provider::Column::Code.eq(provider.code.clone()))
        .filter(provider::Column::Id.ne(provider.id.clone()))
        .one(db)
        .await
        .map_err(|e| CrawlerError::Config(format!("查询厂商失败: {e}")))?;
    if dup.is_some() {
        return Err(CrawlerError::Config(format!("编码已存在: {}", provider.code)));
    }
    let existing = provider::Entity::find_by_id(provider.id.clone())
        .one(db)
        .await
        .map_err(|e| CrawlerError::Config(format!("查询厂商失败: {e}")))?;
    match existing {
        Some(model) => {
            // 前端只看到打码后的 api_key;若提交的就是打码占位串,说明用户没改密钥 → 保留原值
            let api_key_to_save = if provider.api_key.starts_with("••")
                || provider.api_key.is_empty()
            {
                model.api_key.clone()
            } else {
                provider.api_key
            };
            let mut am = model.into_active_model();
            am.code = Set(provider.code);
            am.name = Set(provider.name);
            am.api_url = Set(provider.api_url);
            am.api_key = Set(api_key_to_save);
            am.models = Set(crate::llm::provider::serialize_models(&provider.models));
            am.updated_at = Set(now);
            am.update(db)
                .await
                .map_err(|e| CrawlerError::Config(format!("更新厂商失败: {e}")))?;
        }
        None => {
            let am = provider::ActiveModel {
                id: Set(provider.id),
                code: Set(provider.code),
                name: Set(provider.name),
                api_url: Set(provider.api_url),
                api_key: Set(provider.api_key),
                models: Set(crate::llm::provider::serialize_models(&provider.models)),
                created_at: Set(now),
                updated_at: Set(now),
            };
            am.insert(db)
                .await
                .map_err(|e| CrawlerError::Config(format!("创建厂商失败: {e}")))?;
        }
    }
    Ok(())
}

#[tauri::command]
pub async fn remove_provider(state: State<'_, AppState>, id: String) -> Result<()> {
    provider::Entity::delete_by_id(id)
        .exec(&state.db)
        .await
        .map_err(|e| CrawlerError::Config(format!("删除厂商失败: {e}")))?;
    Ok(())
}

// ===================== 提示词 =====================

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptDto {
    pub id: String,
    pub code: String,
    pub name: String,
    pub content: String,
}

impl From<prompt::Model> for PromptDto {
    fn from(m: prompt::Model) -> Self {
        Self {
            id: m.id,
            code: m.code,
            name: m.name,
            content: m.content,
        }
    }
}

#[tauri::command]
pub async fn list_prompts(state: State<'_, AppState>) -> Result<Vec<PromptDto>> {
    let rows = prompt::Entity::find()
        .order_by_asc(prompt::Column::CreatedAt)
        .limit(LIST_HARD_CAP)
        .all(&state.db)
        .await
        .map_err(|e| CrawlerError::Config(format!("查询提示词失败: {e}")))?;
    Ok(rows.into_iter().map(Into::into).collect())
}

#[tauri::command]
pub async fn upsert_prompt(state: State<'_, AppState>, prompt: PromptDto) -> Result<()> {
    let db = &state.db;
    let now = Utc::now().timestamp();
    // 编码须全表唯一(排除自身),避免重复编码
    let dup = prompt::Entity::find()
        .filter(prompt::Column::Code.eq(prompt.code.clone()))
        .filter(prompt::Column::Id.ne(prompt.id.clone()))
        .one(db)
        .await
        .map_err(|e| CrawlerError::Config(format!("查询提示词失败: {e}")))?;
    if dup.is_some() {
        return Err(CrawlerError::Config(format!("编码已存在: {}", prompt.code)));
    }
    let existing = prompt::Entity::find_by_id(prompt.id.clone())
        .one(db)
        .await
        .map_err(|e| CrawlerError::Config(format!("查询提示词失败: {e}")))?;
    match existing {
        Some(model) => {
            let mut am = model.into_active_model();
            am.code = Set(prompt.code);
            am.name = Set(prompt.name);
            am.content = Set(prompt.content);
            am.updated_at = Set(now);
            am.update(db)
                .await
                .map_err(|e| CrawlerError::Config(format!("更新提示词失败: {e}")))?;
        }
        None => {
            let am = prompt::ActiveModel {
                id: Set(prompt.id),
                code: Set(prompt.code),
                name: Set(prompt.name),
                content: Set(prompt.content),
                created_at: Set(now),
                updated_at: Set(now),
            };
            am.insert(db)
                .await
                .map_err(|e| CrawlerError::Config(format!("创建提示词失败: {e}")))?;
        }
    }
    Ok(())
}

#[tauri::command]
pub async fn remove_prompt(state: State<'_, AppState>, id: String) -> Result<()> {
    prompt::Entity::delete_by_id(id)
        .exec(&state.db)
        .await
        .map_err(|e| CrawlerError::Config(format!("删除提示词失败: {e}")))?;
    Ok(())
}

// ===================== 客户管理 =====================

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CustomerView {
    pub id: String,
    pub code: String,
    pub name: String,
    pub phone: String,
    pub email: String,
    pub company: String,
    pub position: String,
    pub wechat: String,
    pub industry: String,
    /// 标签:实体内以 JSON 字符串存储,这里解析回数组供前端直接渲染。
    pub tags: Vec<String>,
    pub source: String,
    pub status: String,
    pub owner: String,
    pub remark: String,
    pub created_at: i64,
    pub updated_at: i64,
}

impl From<customer::Model> for CustomerView {
    fn from(m: customer::Model) -> Self {
        Self {
            id: m.id,
            code: m.code,
            name: m.name,
            phone: m.phone,
            email: m.email,
            company: m.company,
            position: m.position,
            wechat: m.wechat,
            industry: m.industry,
            tags: serde_json::from_str(&m.tags).unwrap_or_default(),
            source: m.source,
            status: m.status,
            owner: m.owner,
            remark: m.remark,
            created_at: m.created_at,
            updated_at: m.updated_at,
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CustomerInput {
    pub id: String,
    pub code: String,
    pub name: String,
    pub phone: String,
    pub email: String,
    pub company: String,
    pub position: String,
    pub wechat: String,
    pub industry: String,
    pub tags: Vec<String>,
    pub source: String,
    pub status: String,
    pub owner: String,
    pub remark: String,
}

#[tauri::command]
pub async fn list_customers(state: State<'_, AppState>) -> Result<Vec<CustomerView>> {
    // 先取出当前用户(克隆后释放锁),再异步查询,避免跨 await 持锁
    let user = super::current_user(&state);
    let mut query = customer::Entity::find().order_by_asc(customer::Column::CreatedAt);
    // scope=="self" 只返回自己跟踪的;"all" 或未登录返回全部
    if let Some(u) = &user {
        if u.scope == "self" {
            query = query.filter(customer::Column::Owner.eq(u.name.clone()));
        }
    }
    let rows = query
        .limit(LIST_HARD_CAP)
        .all(&state.db)
        .await
        .map_err(|e| CrawlerError::Config(format!("查询客户失败: {e}")))?;
    Ok(rows.into_iter().map(Into::into).collect())
}

#[tauri::command]
pub async fn upsert_customer(state: State<'_, AppState>, customer: CustomerInput) -> Result<()> {
    let db = &state.db;
    let now = Utc::now().timestamp();
    // 编码须全表唯一(排除自身),避免重复编码
    let dup = customer::Entity::find()
        .filter(customer::Column::Code.eq(customer.code.clone()))
        .filter(customer::Column::Id.ne(customer.id.clone()))
        .one(db)
        .await
        .map_err(|e| CrawlerError::Config(format!("查询客户失败: {e}")))?;
    if dup.is_some() {
        return Err(CrawlerError::Config(format!("编码已存在: {}", customer.code)));
    }
    // 标签数组序列化为 JSON 字符串落库
    let tags = serde_json::to_string(&customer.tags)
        .map_err(|e| CrawlerError::Config(format!("序列化标签失败: {e}")))?;
    let existing = customer::Entity::find_by_id(customer.id.clone())
        .one(db)
        .await
        .map_err(|e| CrawlerError::Config(format!("查询客户失败: {e}")))?;
    match existing {
        Some(model) => {
            // 编辑:owner(跟踪人)不随编辑变更,保留原值
            let mut am = model.into_active_model();
            am.code = Set(customer.code);
            am.name = Set(customer.name);
            am.phone = Set(customer.phone);
            am.email = Set(customer.email);
            am.company = Set(customer.company);
            am.position = Set(customer.position);
            am.wechat = Set(customer.wechat);
            am.industry = Set(customer.industry);
            am.tags = Set(tags);
            am.source = Set(customer.source);
            am.status = Set(customer.status);
            am.remark = Set(customer.remark);
            am.updated_at = Set(now);
            am.update(db)
                .await
                .map_err(|e| CrawlerError::Config(format!("更新客户失败: {e}")))?;
        }
        None => {
            // 新建归属由后端会话决定:有当前用户则记其用户名,无则回退前端传值(兼容)
            let owner = super::current_user(&state)
                .map(|u| u.name)
                .unwrap_or(customer.owner);
            let am = customer::ActiveModel {
                id: Set(customer.id),
                code: Set(customer.code),
                name: Set(customer.name),
                phone: Set(customer.phone),
                email: Set(customer.email),
                company: Set(customer.company),
                position: Set(customer.position),
                wechat: Set(customer.wechat),
                industry: Set(customer.industry),
                tags: Set(tags),
                source: Set(customer.source),
                status: Set(customer.status),
                owner: Set(owner),
                remark: Set(customer.remark),
                created_at: Set(now),
                updated_at: Set(now),
            };
            am.insert(db)
                .await
                .map_err(|e| CrawlerError::Config(format!("创建客户失败: {e}")))?;
        }
    }
    Ok(())
}

#[tauri::command]
pub async fn remove_customer(state: State<'_, AppState>, id: String) -> Result<()> {
    customer::Entity::delete_by_id(id)
        .exec(&state.db)
        .await
        .map_err(|e| CrawlerError::Config(format!("删除客户失败: {e}")))?;
    Ok(())
}

// ===================== 行业类别 =====================

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IndustryView {
    pub id: String,
    pub code: String,
    pub name: String,
    pub remark: String,
    pub created_at: i64,
    pub updated_at: i64,
}

impl From<industry::Model> for IndustryView {
    fn from(m: industry::Model) -> Self {
        Self {
            id: m.id,
            code: m.code,
            name: m.name,
            remark: m.remark,
            created_at: m.created_at,
            updated_at: m.updated_at,
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IndustryInput {
    pub id: String,
    pub code: String,
    pub name: String,
    pub remark: String,
}

#[tauri::command]
pub async fn list_industries(state: State<'_, AppState>) -> Result<Vec<IndustryView>> {
    let rows = industry::Entity::find()
        .order_by_asc(industry::Column::CreatedAt)
        .limit(LIST_HARD_CAP)
        .all(&state.db)
        .await
        .map_err(|e| CrawlerError::Config(format!("查询行业失败: {e}")))?;
    Ok(rows.into_iter().map(Into::into).collect())
}

#[tauri::command]
pub async fn upsert_industry(state: State<'_, AppState>, industry: IndustryInput) -> Result<()> {
    let db = &state.db;
    let now = Utc::now().timestamp();
    // 编码须全表唯一(排除自身),避免重复编码
    let dup = industry::Entity::find()
        .filter(industry::Column::Code.eq(industry.code.clone()))
        .filter(industry::Column::Id.ne(industry.id.clone()))
        .one(db)
        .await
        .map_err(|e| CrawlerError::Config(format!("查询行业失败: {e}")))?;
    if dup.is_some() {
        return Err(CrawlerError::Config(format!("编码已存在: {}", industry.code)));
    }
    let existing = industry::Entity::find_by_id(industry.id.clone())
        .one(db)
        .await
        .map_err(|e| CrawlerError::Config(format!("查询行业失败: {e}")))?;
    match existing {
        Some(model) => {
            let mut am = model.into_active_model();
            am.code = Set(industry.code);
            am.name = Set(industry.name);
            am.remark = Set(industry.remark);
            am.updated_at = Set(now);
            am.update(db)
                .await
                .map_err(|e| CrawlerError::Config(format!("更新行业失败: {e}")))?;
        }
        None => {
            let am = industry::ActiveModel {
                id: Set(industry.id),
                code: Set(industry.code),
                name: Set(industry.name),
                remark: Set(industry.remark),
                created_at: Set(now),
                updated_at: Set(now),
            };
            am.insert(db)
                .await
                .map_err(|e| CrawlerError::Config(format!("创建行业失败: {e}")))?;
        }
    }
    Ok(())
}

#[tauri::command]
pub async fn remove_industry(state: State<'_, AppState>, id: String) -> Result<()> {
    let db = &state.db;
    // 逻辑外键无物理级联,删除行业时手动级联删除其下关键词
    keyword::Entity::delete_many()
        .filter(keyword::Column::IndustryId.eq(id.clone()))
        .exec(db)
        .await
        .map_err(|e| CrawlerError::Config(format!("删除行业关键词失败: {e}")))?;
    industry::Entity::delete_by_id(id)
        .exec(db)
        .await
        .map_err(|e| CrawlerError::Config(format!("删除行业失败: {e}")))?;
    Ok(())
}

// ===================== 关键词 =====================

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KeywordDto {
    pub id: String,
    pub industry_id: String,
    pub word: String,
}

impl From<keyword::Model> for KeywordDto {
    fn from(m: keyword::Model) -> Self {
        Self {
            id: m.id,
            industry_id: m.industry_id,
            word: m.word,
        }
    }
}

#[tauri::command]
pub async fn list_keywords(
    state: State<'_, AppState>,
    industry_id: String,
) -> Result<Vec<KeywordDto>> {
    let rows = keyword::Entity::find()
        .filter(keyword::Column::IndustryId.eq(industry_id))
        .order_by_asc(keyword::Column::CreatedAt)
        .limit(LIST_HARD_CAP)
        .all(&state.db)
        .await
        .map_err(|e| CrawlerError::Config(format!("查询关键词失败: {e}")))?;
    Ok(rows.into_iter().map(Into::into).collect())
}

/// 批量新增关键词。前端按行输入,这里对非空、去重后的每个词生成 id 插入。
#[tauri::command]
pub async fn create_keywords(
    state: State<'_, AppState>,
    industry_id: String,
    words: Vec<String>,
) -> Result<()> {
    let db = &state.db;
    let now = Utc::now().timestamp();
    // 无 uuid 依赖,用纳秒时间戳 + 行业 id + 序号拼出稳定唯一的关键词 id
    let nanos = Utc::now().timestamp_nanos_opt().unwrap_or(now * 1_000_000_000);
    let mut seen = std::collections::HashSet::new();
    let models: Vec<keyword::ActiveModel> = words
        .into_iter()
        .map(|w| w.trim().to_string())
        .filter(|w| !w.is_empty() && seen.insert(w.clone()))
        .enumerate()
        .map(|(idx, w)| keyword::ActiveModel {
            id: Set(format!("kw-{industry_id}-{nanos}-{idx}")),
            industry_id: Set(industry_id.clone()),
            word: Set(w),
            created_at: Set(now),
        })
        .collect();
    if models.is_empty() {
        return Ok(());
    }
    keyword::Entity::insert_many(models)
        .exec(db)
        .await
        .map_err(|e| CrawlerError::Config(format!("新增关键词失败: {e}")))?;
    Ok(())
}

/// 更新单个关键词(行业页编辑场景);id 不存在时按插入处理。
#[tauri::command]
pub async fn upsert_keyword(state: State<'_, AppState>, keyword: KeywordDto) -> Result<()> {
    let db = &state.db;
    let now = Utc::now().timestamp();
    let existing = keyword::Entity::find_by_id(keyword.id.clone())
        .one(db)
        .await
        .map_err(|e| CrawlerError::Config(format!("查询关键词失败: {e}")))?;
    match existing {
        Some(model) => {
            let mut am = model.into_active_model();
            am.word = Set(keyword.word);
            am.update(db)
                .await
                .map_err(|e| CrawlerError::Config(format!("更新关键词失败: {e}")))?;
        }
        None => {
            let am = keyword::ActiveModel {
                id: Set(keyword.id),
                industry_id: Set(keyword.industry_id),
                word: Set(keyword.word),
                created_at: Set(now),
            };
            am.insert(db)
                .await
                .map_err(|e| CrawlerError::Config(format!("创建关键词失败: {e}")))?;
        }
    }
    Ok(())
}

#[tauri::command]
pub async fn remove_keyword(state: State<'_, AppState>, id: String) -> Result<()> {
    keyword::Entity::delete_by_id(id)
        .exec(&state.db)
        .await
        .map_err(|e| CrawlerError::Config(format!("删除关键词失败: {e}")))?;
    Ok(())
}
