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
    /// assistant 要求调用的工具(JSON 数组 [{id,name,arguments}]);纯文本回复为 None
    #[sea_orm(column_type = "Text", nullable)]
    pub tool_calls: Option<String>,
    /// role=tool 时:对应的工具调用 id(关联上一条 assistant 的某次 tool_call)
    pub tool_call_id: Option<String>,
    /// role=tool 时:工具名(便于前端展示)
    pub tool_name: Option<String>,
    pub created_at: i64,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
