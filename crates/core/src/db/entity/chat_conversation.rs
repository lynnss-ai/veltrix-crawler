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
    /// 场景类型:chat / coding / rpa …(默认 chat)。决定走哪个 Agent 与页面布局。
    pub agent_type: String,
    /// 滚动摘要:本会话早期(已滚出 live 窗口)消息压缩后的「前情提要」,发送时作 system 注入。
    #[sea_orm(column_type = "Text")]
    pub summary: String,
    /// 已折叠进 `summary` 的最大消息 id;id 大于此值的消息为 live 原文(不重复进摘要)。
    pub summarized_upto_id: i64,
    pub created_at: i64,
    pub updated_at: i64,
    /// 是否归档:归档会话从「最近对话」与对话页隐藏,仅在对话记录页可见 / 可恢复。默认 false。
    pub archived: bool,
    /// 编程 Agent 的结构化任务清单(JSON 数组 `[{"title","done"}]`):
    /// Plan 模式调研后产出、Act 模式按序执行并勾选;空串表示尚无计划。
    #[sea_orm(column_type = "Text")]
    pub plan_todos: String,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
