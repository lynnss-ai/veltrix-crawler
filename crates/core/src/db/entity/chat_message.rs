//! AI 对话消息实体。每条消息属于一个会话(chat_conversations.id 逻辑外键)。

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "chat_messages")]
pub struct Model {
    /// 自增主键(消息量大,用整型自增)
    #[sea_orm(primary_key)]
    pub id: i64,
    /// 所属会话(chat_conversations.id)
    pub conversation_id: String,
    /// 角色:user / assistant
    pub role: String,
    #[sea_orm(column_type = "Text")]
    pub content: String,
    pub created_at: i64,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
