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
    // DB 错误上抛(区别于「未配置」返回空串):查询失败时前端应看到错误而非空值
    let path = user_entity::Entity::find()
        .filter(user_entity::Column::Username.eq(&me.name))
        .one(&state.db)
        .await
        .map_err(|e| CrawlerError::Config(format!("查询 vault 配置失败: {e}")))?
        .map(|u| u.obsidian_vault_path)
        .unwrap_or_default();
    Ok(path)
}

/// 幂等记录「某用户已同步某条内容」到 content_synced_users(冲突时更新同步时间 / vault 路径)。
/// 这张表是前端「已同步」标记的唯一来源,写失败会产生「磁盘有文件但 UI 显示未同步」。
async fn record_synced(
    db: &DatabaseConnection,
    content_pk: &str,
    user: &str,
    vault: &str,
    now: i64,
) -> std::result::Result<(), sea_orm::DbErr> {
    use sea_orm::sea_query::OnConflict;
    use veltrix_core::db::entity::content_synced_user as csu_entity;
    csu_entity::Entity::insert(csu_entity::ActiveModel {
        content_id: Set(content_pk.to_string()),
        synced_user: Set(user.to_string()),
        synced_at: Set(now),
        vault_path: Set(vault.to_string()),
    })
    .on_conflict(
        OnConflict::columns([csu_entity::Column::ContentId, csu_entity::Column::SyncedUser])
            .update_columns([csu_entity::Column::SyncedAt, csu_entity::Column::VaultPath])
            .to_owned(),
    )
    .exec(db)
    .await?;
    Ok(())
}

/// 把若干内容同步到「当前用户」的 Obsidian vault:渲染 Markdown + 复制封面,并记录同步关系。
/// self scope 仅能同步自己 owner 的内容。返回成功同步的条数。
#[tauri::command]
pub async fn sync_contents_to_obsidian(
    state: State<'_, AppState>,
    ids: Vec<String>,
) -> Result<usize> {
    use veltrix_core::db::entity::{
        comment as comment_entity, content as content_entity, task as task_entity,
        user as user_entity,
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
    // 批量取内容,避免逐条 find_by_id 的 N+1;查不到的 id(含被删的)自然跳过
    let contents = content_entity::Entity::find()
        .filter(content_entity::Column::Id.is_in(ids))
        .all(&state.db)
        .await
        .map_err(|e| CrawlerError::Config(format!("查询内容失败: {e}")))?;
    let mut synced = 0usize;
    // 行业按 task_id 缓存:同任务的内容共享一次 task 查询,避免重复查
    let mut industry_cache: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();
    for content in &contents {
        if me.scope == "self" && content.owner != me.name {
            continue;
        }
        let comments = match comment_entity::Entity::find()
            .filter(comment_entity::Column::TaskId.eq(&content.task_id))
            .filter(comment_entity::Column::ContentId.eq(&content.content_id))
            .all(&state.db)
            .await
        {
            Ok(c) => c,
            // 评论查询失败不能当「无评论」继续:那会生成缺评论块的 Markdown 还标记已同步,
            // 视为本条同步失败跳过
            Err(e) => {
                tracing::warn!(content_id = %content.id, "查询评论失败,跳过同步: {e}");
                continue;
            }
        };
        // 行业取自内容所属任务,用于「行业-日期」归档目录
        let industry = match industry_cache.get(&content.task_id) {
            Some(ind) => ind.clone(),
            None => {
                let ind = task_entity::Entity::find_by_id(&content.task_id)
                    .one(&state.db)
                    .await
                    .ok()
                    .flatten()
                    .map(|t| t.industry)
                    .unwrap_or_default();
                industry_cache.insert(content.task_id.clone(), ind.clone());
                ind
            }
        };
        if let Err(e) = crate::obsidian::sync_one(&vault_path, content, &comments, &industry).await
        {
            tracing::warn!(content_id = %content.id, "同步 Obsidian 失败: {e}");
            continue;
        }
        // 幂等记录「当前用户已同步该条」;记录失败视为本条同步失败(不计入成功数),
        // 否则会出现「磁盘有文件但 UI 显示未同步」
        if let Err(e) = record_synced(&state.db, &content.id, &me.name, &vault, now).await {
            tracing::warn!(content_id = %content.id, "记录 Obsidian 同步关系失败: {e}");
            continue;
        }
        synced += 1;
    }
    Ok(synced)
}

/// 自动同步:把任务全部内容同步到指定用户(owner)的 Obsidian vault,并记录同步关系。
/// 失败仅告警不中断;owner 未配 vault 则直接跳过。返回成功同步的条数(供调用方记日志)。
pub(super) async fn sync_task_to_obsidian(
    db: &DatabaseConnection,
    task_id: &str,
    owner: &str,
) -> usize {
    use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
    use veltrix_core::db::entity::{
        comment as comment_entity, content as content_entity, task as task_entity,
        user as user_entity,
    };
    let vault = match user_entity::Entity::find()
        .filter(user_entity::Column::Username.eq(owner))
        .one(db)
        .await
    {
        Ok(Some(u)) => u.obsidian_vault_path,
        Ok(None) => {
            tracing::warn!(owner, "自动同步 Obsidian 跳过:用户不存在");
            return 0;
        }
        Err(e) => {
            // 此前与「未配置」一样静默返回,DB 故障时自动同步无声消失,无法排查
            tracing::warn!(owner, "自动同步 Obsidian 跳过:查询用户 vault 配置失败: {e}");
            return 0;
        }
    };
    if vault.trim().is_empty() {
        return 0;
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
        Err(e) => {
            tracing::warn!(task_id, "自动同步 Obsidian 跳过:查询任务内容失败: {e}");
            return 0;
        }
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
                let comments = match comment_entity::Entity::find()
                    .filter(comment_entity::Column::TaskId.eq(task_id))
                    .filter(comment_entity::Column::ContentId.eq(&content.content_id))
                    .all(&db)
                    .await
                {
                    Ok(c) => c,
                    // 评论查询失败不能当「无评论」继续:那会生成缺评论块的 Markdown
                    // 还标记已同步,视为本条同步失败跳过
                    Err(e) => {
                        tracing::warn!(content_id = %content.id, "查询评论失败,跳过同步: {e}");
                        return false;
                    }
                };
                if let Err(e) =
                    crate::obsidian::sync_one(&vault_path, &content, &comments, &industry).await
                {
                    tracing::warn!(content_id = %content.id, "自动同步 Obsidian 写盘失败: {e}");
                    return false;
                }
                // 幂等记录失败只告警不中断,但不能再静默吞掉——否则「磁盘有文件但 UI 显示未同步」
                if let Err(e) = record_synced(&db, &content.id, &owner, &vault, now).await {
                    tracing::warn!(content_id = %content.id, "记录 Obsidian 同步关系失败: {e}");
                }
                true
            }
        })
        .buffer_unordered(8)
        .filter(|ok| futures_util::future::ready(*ok))
        .count()
        .await
}
