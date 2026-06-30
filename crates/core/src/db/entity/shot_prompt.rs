//! 分镜镜头提示词表 SeaORM 实体(内容创作 - 提示词管理)。
//!
//! 每条提示词归属一个分类目录(category_id);owner 为归属用户。逻辑外键,无物理 FK。

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "shot_prompts")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: String,
    /// 归属用户。
    pub owner: String,
    /// 所属分类目录 ID(prompt_categories.id)。
    pub category_id: String,
    /// 提示词标题。
    pub name: String,
    /// 提示词正文。
    #[sea_orm(column_type = "Text")]
    pub content: String,
    #[sea_orm(column_type = "Text")]
    pub remark: String,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
