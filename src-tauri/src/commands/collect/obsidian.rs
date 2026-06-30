//! Obsidian 同步:每用户 vault 路径配置 + 内容/任务同步为 Markdown。
//! 从采集流水线拆出——只读库 + 调 `crate::obsidian::sync_one` 写盘,自成一类。

use crate::commands::{current_user, AppState};
use chrono::Utc;
use std::path::Path;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, IntoActiveModel, QueryFilter,
    Set,
};
use tauri::State;
use veltrix_core::error::{CrawlerError, Result};

/// 保存当前用户的 Obsidian vault 根路径(每用户各自配置)。
#[tauri::command]
pub async fn set_obsidian_vault(state: State<'_, AppState>, vault_path: String) -> Result<()> {
    use veltrix_core::db::entity::user as user_entity;
    let me = current_user(&state).ok_or_else(|| CrawlerError::Config("未登录".into()))?;
    // 外部输入校验:vault 是后续文件写入的根,相对路径 /「..」/ 不存在的目录
    // 都可能把 Markdown 与素材写到意外位置。空值放行(表示清除配置)。
    let trimmed = vault_path.trim().to_string();
    if !trimmed.is_empty() {
        let p = Path::new(&trimmed);
        if !p.is_absolute() {
            return Err(CrawlerError::Config("vault 路径必须是绝对路径".into()));
        }
        if p.components().any(|c| matches!(c, std::path::Component::ParentDir)) {
            return Err(CrawlerError::Config("vault 路径不允许包含「..」".into()));
        }
        if !p.is_dir() {
            return Err(CrawlerError::Config("vault 路径不存在或不是目录".into()));
        }
    }
    let model = user_entity::Entity::find()
        .filter(user_entity::Column::Username.eq(&me.name))
        .one(&state.db)
        .await
        .map_err(|e| CrawlerError::Config(format!("查询用户失败: {e}")))?
        .ok_or_else(|| CrawlerError::Config("用户不存在".into()))?;
    let mut am = model.into_active_model();
    am.obsidian_vault_path = Set(trimmed);
    am.updated_at = Set(Utc::now().timestamp());
    am.update(&state.db)
        .await
        .map_err(|e| CrawlerError::Config(format!("保存 vault 失败: {e}")))?;
    Ok(())
}

/// 读取当前用户的 Obsidian vault 根路径(未配置返回空串)。
#[tauri::command]
pub async fn get_obsidian_vault(state: State<'_, AppState>) -> Result<String> {
    use veltrix_core::db::entity::user as user_entity;
    let me = current_user(&state).ok_or_else(|| CrawlerError::Config("未登录".into()))?;
    let path = user_entity::Entity::find()
        .filter(user_entity::Column::Username.eq(&me.name))
        .one(&state.db)
        .await
        .ok()
        .flatten()
        .map(|u| u.obsidian_vault_path)
        .unwrap_or_default();
    Ok(path)
}

/// 把若干内容同步到「当前用户」的 Obsidian vault:渲染 Markdown + 复制封面,并记录同步关系。
/// self scope 仅能同步自己 owner 的内容。返回成功同步的条数。
#[tauri::command]
pub async fn sync_contents_to_obsidian(
    state: State<'_, AppState>,
    ids: Vec<String>,
) -> Result<usize> {
    use sea_orm::sea_query::OnConflict;
    use veltrix_core::db::entity::{
        comment as comment_entity, content as content_entity,
        content_synced_user as csu_entity, task as task_entity, user as user_entity,
    };
    let me = current_user(&state).ok_or_else(|| CrawlerError::Config("未登录".into()))?;
    let vault = user_entity::Entity::find()
        .filter(user_entity::Column::Username.eq(&me.name))
        .one(&state.db)
        .await
        .ok()
        .flatten()
        .map(|u| u.obsidian_vault_path)
        .unwrap_or_default();
    if vault.trim().is_empty() {
        return Err(CrawlerError::Config(
            "请先在「系统设置 → Obsidian」配置 vault 路径".into(),
        ));
    }
    let vault_path = std::path::PathBuf::from(&vault);
    let now = Utc::now().timestamp();
    let mut synced = 0usize;
    for id in &ids {
        let content = match content_entity::Entity::find_by_id(id).one(&state.db).await {
            Ok(Some(c)) => c,
            _ => continue,
        };
        if me.scope == "self" && content.owner != me.name {
            continue;
        }
        let comments = comment_entity::Entity::find()
            .filter(comment_entity::Column::TaskId.eq(&content.task_id))
            .filter(comment_entity::Column::ContentId.eq(&content.content_id))
            .all(&state.db)
            .await
            .unwrap_or_default();
        // 行业取自内容所属任务,用于「行业-日期」归档目录
        let industry = task_entity::Entity::find_by_id(&content.task_id)
            .one(&state.db)
            .await
            .ok()
            .flatten()
            .map(|t| t.industry)
            .unwrap_or_default();
        if let Err(e) = crate::obsidian::sync_one(&vault_path, &content, &comments, &industry).await
        {
            tracing::warn!(content_id = %id, "同步 Obsidian 失败: {e}");
            continue;
        }
        // 幂等记录「当前用户已同步该条」
        let am = csu_entity::ActiveModel {
            content_id: Set(id.clone()),
            synced_user: Set(me.name.clone()),
            synced_at: Set(now),
            vault_path: Set(vault.clone()),
        };
        let _ = csu_entity::Entity::insert(am)
            .on_conflict(
                OnConflict::columns([
                    csu_entity::Column::ContentId,
                    csu_entity::Column::SyncedUser,
                ])
                .update_columns([csu_entity::Column::SyncedAt, csu_entity::Column::VaultPath])
                .to_owned(),
            )
            .exec(&state.db)
            .await;
        synced += 1;
    }
    Ok(synced)
}

/// 自动同步:把任务全部内容同步到指定用户(owner)的 Obsidian vault,并记录同步关系。
/// 失败仅告警不中断;owner 未配 vault 则直接跳过。
pub(super) async fn sync_task_to_obsidian(db: &DatabaseConnection, task_id: &str, owner: &str) {
    use sea_orm::sea_query::OnConflict;
    use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
    use veltrix_core::db::entity::{
        comment as comment_entity, content as content_entity,
        content_synced_user as csu_entity, task as task_entity, user as user_entity,
    };
    let vault = match user_entity::Entity::find()
        .filter(user_entity::Column::Username.eq(owner))
        .one(db)
        .await
    {
        Ok(Some(u)) => u.obsidian_vault_path,
        _ => return,
    };
    if vault.trim().is_empty() {
        return;
    }
    let vault_path = std::path::PathBuf::from(&vault);
    // 整批同属一个任务,行业查一次即可
    let industry = task_entity::Entity::find_by_id(task_id)
        .one(db)
        .await
        .ok()
        .flatten()
        .map(|t| t.industry)
        .unwrap_or_default();
    let now = Utc::now().timestamp();
    let rows = match content_entity::Entity::find()
        .filter(content_entity::Column::TaskId.eq(task_id))
        .all(db)
        .await
    {
        Ok(r) => r,
        Err(_) => return,
    };
    use futures_util::StreamExt;
    let db_clone = db.clone();
    futures_util::stream::iter(rows)
        .map(|content| {
            let db = db_clone.clone();
            let vault_path = vault_path.clone();
            let vault = vault.clone();
            let industry = industry.clone();
            let owner = owner.to_string();
            async move {
                let comments = comment_entity::Entity::find()
                    .filter(comment_entity::Column::TaskId.eq(task_id))
                    .filter(comment_entity::Column::ContentId.eq(&content.content_id))
                    .all(&db)
                    .await
                    .unwrap_or_default();
                if crate::obsidian::sync_one(&vault_path, &content, &comments, &industry)
                    .await
                    .is_err()
                {
                    return;
                }
                let am = csu_entity::ActiveModel {
                    content_id: Set(content.id.clone()),
                    synced_user: Set(owner),
                    synced_at: Set(now),
                    vault_path: Set(vault),
                };
                let _ = csu_entity::Entity::insert(am)
                    .on_conflict(
                        OnConflict::columns([
                            csu_entity::Column::ContentId,
                            csu_entity::Column::SyncedUser,
                        ])
                        .update_columns([
                            csu_entity::Column::SyncedAt,
                            csu_entity::Column::VaultPath,
                        ])
                        .to_owned(),
                    )
                    .exec(&db)
                    .await;
            }
        })
        .buffer_unordered(8)
        .count()
        .await;
}
