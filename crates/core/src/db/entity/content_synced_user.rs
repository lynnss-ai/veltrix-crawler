//! 内容-用户同步追踪:记录「某用户已把某条内容同步到其 Obsidian vault」。
//!
//! 复合主键 (content_id, synced_user) 幂等:同一用户对同一内容只记最新一次同步。
//! content_id 为 contents.id(复合 `{task_id}-{platform}-{content_id}`)的弱关联。

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "content_synced_users")]
pub struct Model {
    /// 内容主键(contents.id);与 synced_user 组成复合主键
    #[sea_orm(primary_key, auto_increment = false)]
    pub content_id: String,
    /// 同步该内容的用户名(users.username 弱关联)
    #[sea_orm(primary_key, auto_increment = false)]
    pub synced_user: String,
    /// 最近一次同步时间(Unix 秒)
    pub synced_at: i64,
    /// 同步目标 vault 根路径(便于排查/未来多 vault)
    #[sea_orm(column_type = "Text")]
    pub vault_path: String,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
