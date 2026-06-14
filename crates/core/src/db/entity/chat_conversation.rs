//! AI 对话会话实体(对话工作区)。每个会话绑定一个模型厂商 + 模型,归属用户。

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "chat_conversations")]
pub struct Model {
    /// 会话 id(前端生成 UUID)
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: String,
    /// 数据归属:创建者用户名
    pub owner: String,
    /// 会话标题(首条消息后自动生成,可手动改)
    pub title: String,
    /// 所用模型厂商 id(providers.id 逻辑引用)
    pub provider_id: String,
    /// 所用模型名
    pub model: String,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
