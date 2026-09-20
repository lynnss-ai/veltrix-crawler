//! 内容创作 - 提示词管理的 CRUD 命令(两级:分类目录 → 分镜镜头提示词)。
//!
//! ID 由前端生成(crypto.randomUUID)并随请求传入,后端按 `id` 是否已存在区分新增 / 更新。
//! 数据按 owner 归属:list 命令在 dataScope=="self" 时只返回当前用户自己的;逻辑外键,无物理 FK。

use crate::commands::AppState;
use veltrix_core::db::entity::{prompt_category, shot_prompt};
use veltrix_core::error::{CrawlerError, Result};
use chrono::Utc;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, EntityTrait, IntoActiveModel, QueryFilter, QueryOrder,
    QuerySelect, Set,
};
use serde::{Deserialize, Serialize};
use tauri::State;

/// 单次 list 接口最多返回 N 行,防 IPC 噎住;数据量超出应改分页接口。
const LIST_HARD_CAP: u64 = 1000;

// ===================== 提示词分类目录 =====================

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptCategoryView {
    pub id: String,
    pub owner: String,
    pub name: String,
    pub remark: String,
    pub created_at: i64,
    pub updated_at: i64,
}

impl From<prompt_category::Model> for PromptCategoryView {
    fn from(m: prompt_category::Model) -> Self {
        Self {
            id: m.id,
            owner: m.owner,
            name: m.name,
            remark: m.remark,
            created_at: m.created_at,
            updated_at: m.updated_at,
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptCategoryInput {
    pub id: String,
    pub name: String,
    pub remark: String,
}

#[tauri::command]
pub async fn list_prompt_categories(
    state: State<'_, AppState>,
) -> Result<Vec<PromptCategoryView>> {
    // 先取出当前用户(克隆后释放锁),再异步查询,避免跨 await 持锁
    let user = super::current_user(&state);
    let mut query =
        prompt_category::Entity::find().order_by_asc(prompt_category::Column::CreatedAt);
    // scope=="self" 只返回自己的;"all" 或未登录返回全部
    if let Some(u) = &user {
        if u.scope == "self" {
            query = query.filter(prompt_category::Column::Owner.eq(u.name.clone()));
        }
    }
    let rows = query
        .limit(LIST_HARD_CAP)
        .all(&state.db)
        .await
        .map_err(|e| CrawlerError::Config(format!("查询提示词分类失败: {e}")))?;
    Ok(rows.into_iter().map(Into::into).collect())
}

#[tauri::command]
pub async fn upsert_prompt_category(
    state: State<'_, AppState>,
    category: PromptCategoryInput,
) -> Result<()> {
    let db = &state.db;
    let now = Utc::now().timestamp();
    let existing = prompt_category::Entity::find_by_id(category.id.clone())
        .one(db)
        .await
        .map_err(|e| CrawlerError::Config(format!("查询提示词分类失败: {e}")))?;
    match existing {
        Some(model) => {
            // 编辑:owner 不随编辑变更,保留原值
            let mut am = model.into_active_model();
            am.name = Set(category.name);
            am.remark = Set(category.remark);
            am.updated_at = Set(now);
            am.update(db)
                .await
                .map_err(|e| CrawlerError::Config(format!("更新提示词分类失败: {e}")))?;
        }
        None => {
            // 新建归属由后端会话决定:有当前用户则记其用户名,无则回退空串(兼容调试)
            let owner = super::current_user(&state)
                .map(|u| u.name)
                .unwrap_or_default();
            let am = prompt_category::ActiveModel {
                id: Set(category.id),
                owner: Set(owner),
                name: Set(category.name),
                remark: Set(category.remark),
                created_at: Set(now),
                updated_at: Set(now),
            };
            am.insert(db)
                .await
                .map_err(|e| CrawlerError::Config(format!("创建提示词分类失败: {e}")))?;
        }
    }
    Ok(())
}

#[tauri::command]
pub async fn remove_prompt_category(state: State<'_, AppState>, id: String) -> Result<()> {
    let db = &state.db;
    // 逻辑外键无物理级联,删除分类时手动级联删除其下提示词
    shot_prompt::Entity::delete_many()
        .filter(shot_prompt::Column::CategoryId.eq(id.clone()))
        .exec(db)
        .await
        .map_err(|e| CrawlerError::Config(format!("删除分类下提示词失败: {e}")))?;
    prompt_category::Entity::delete_by_id(id)
        .exec(db)
        .await
        .map_err(|e| CrawlerError::Config(format!("删除提示词分类失败: {e}")))?;
    Ok(())
}

// ===================== 分镜镜头提示词 =====================

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ShotPromptView {
    pub id: String,
    pub owner: String,
    pub category_id: String,
    pub name: String,
    pub content: String,
    pub remark: String,
    pub created_at: i64,
    pub updated_at: i64,
}

impl From<shot_prompt::Model> for ShotPromptView {
    fn from(m: shot_prompt::Model) -> Self {
        Self {
            id: m.id,
            owner: m.owner,
            category_id: m.category_id,
            name: m.name,
            content: m.content,
            remark: m.remark,
            created_at: m.created_at,
            updated_at: m.updated_at,
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShotPromptInput {
    pub id: String,
    pub category_id: String,
    pub name: String,
    pub content: String,
    pub remark: String,
}

#[tauri::command]
pub async fn list_shot_prompts(
    state: State<'_, AppState>,
    category_id: String,
) -> Result<Vec<ShotPromptView>> {
    let user = super::current_user(&state);
    let mut query = shot_prompt::Entity::find()
        .filter(shot_prompt::Column::CategoryId.eq(category_id))
        .order_by_asc(shot_prompt::Column::CreatedAt);
    if let Some(u) = &user {
        if u.scope == "self" {
            query = query.filter(shot_prompt::Column::Owner.eq(u.name.clone()));
        }
    }
    let rows = query
        .limit(LIST_HARD_CAP)
        .all(&state.db)
        .await
        .map_err(|e| CrawlerError::Config(format!("查询提示词失败: {e}")))?;
    Ok(rows.into_iter().map(Into::into).collect())
}

#[tauri::command]
pub async fn upsert_shot_prompt(state: State<'_, AppState>, prompt: ShotPromptInput) -> Result<()> {
    let db = &state.db;
    let now = Utc::now().timestamp();
    let existing = shot_prompt::Entity::find_by_id(prompt.id.clone())
        .one(db)
        .await
        .map_err(|e| CrawlerError::Config(format!("查询提示词失败: {e}")))?;
    match existing {
        Some(model) => {
            let mut am = model.into_active_model();
            am.name = Set(prompt.name);
            am.content = Set(prompt.content);
            am.remark = Set(prompt.remark);
            am.updated_at = Set(now);
            am.update(db)
                .await
                .map_err(|e| CrawlerError::Config(format!("更新提示词失败: {e}")))?;
        }
        None => {
            let owner = super::current_user(&state)
                .map(|u| u.name)
                .unwrap_or_default();
            let am = shot_prompt::ActiveModel {
                id: Set(prompt.id),
                owner: Set(owner),
                category_id: Set(prompt.category_id),
                name: Set(prompt.name),
                content: Set(prompt.content),
                remark: Set(prompt.remark),
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
pub async fn remove_shot_prompt(state: State<'_, AppState>, id: String) -> Result<()> {
    shot_prompt::Entity::delete_by_id(id)
        .exec(&state.db)
        .await
        .map_err(|e| CrawlerError::Config(format!("删除提示词失败: {e}")))?;
    Ok(())
}
