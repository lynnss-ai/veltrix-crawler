//! 模型厂商表(系统配置 - 模型厂商)。
//! models 列存结构化模型列表的 JSON(`[{"name","capabilities":[...]}]`);
//! 兼容旧库的多行文本(每行一个模型名),解析/序列化见 `src-tauri` 的 `llm::provider`。

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "providers")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: String,
    /// 业务编码(如 PRV-XXXX),系统生成。
    pub code: String,
    pub name: String,
    #[sea_orm(column_type = "Text")]
    pub api_url: String,
    #[sea_orm(column_type = "Text")]
    pub api_key: String,
    /// 可用模型:结构化列表 JSON(名称 + 能力);兼容旧多行文本(降级为仅对话能力)。
    #[sea_orm(column_type = "Text")]
    pub models: String,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
