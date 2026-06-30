//! AI 对话长期记忆实体。跨会话、按用户(owner)归属的记忆条目。
//! 来源:`auto`(LLM 每轮自动从对话中提取)/ `manual`(用户在设置页手动维护)。
//! 发消息前把启用的记忆拼成 system 消息注入上下文,让 AI 跨会话记住用户。
//! 支持记忆层级化:global(全局)/project(项目)/conversation(会话)。

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "chat_memories")]
pub struct Model {
    /// 自增主键(记忆条数可能较多,用整型自增)
    #[sea_orm(primary_key)]
    pub id: i64,
    /// 数据归属:用户名
    pub owner: String,
    /// 记忆作用域:global(全局)/project(项目)/conversation(会话)
    pub scope: String,
    /// 作用域 ID:project 时为项目 ID,conversation 时为会话 ID,global 时为空
    pub scope_id: String,
    /// 记忆内容(一条自包含的事实 / 偏好)
    #[sea_orm(column_type = "Text")]
    pub content: String,
    /// 来源:`auto`(自动提取)/ `manual`(手动添加)
    pub source: String,
    /// 是否启用:关闭后不注入上下文,但保留可恢复
    pub enabled: bool,
    /// 内容向量(JSON float 数组字符串);None=尚未生成。按当前问题语义检索 top-K 注入(RAG)用。
    #[sea_orm(column_type = "Text", nullable)]
    pub embedding: Option<String>,
    /// 生成该向量所用的 embedding 模型;换模型后据此判定旧向量失效、需重算。
    pub embed_model: Option<String>,
    /// 置顶:每轮对话恒注入,不参与相似度淘汰(给「称呼/职业」等永远要带的事实)。
    pub pinned: bool,
    /// 分类:identity(身份)/preference(偏好)/project(项目)/relationship(人际)/habit(习惯)/other(其它)。
    pub mem_type: String,
    /// 重要度 1-5:越高越优先注入、淘汰时越靠后。
    pub importance: i32,
    /// 置信度 1-5:模型对该记忆的确定程度;低置信优先被淘汰。
    pub confidence: i32,
    /// 命中次数:每次被注入 +1,衡量记忆实际有用程度。
    pub hit_count: i64,
    /// 最后命中时间(Unix 秒):时间衰减用,久未命中的逐渐降权。
    pub last_hit_at: i64,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
