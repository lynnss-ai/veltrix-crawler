//! 关键词表 SeaORM 实体(基础设施 - 行业类别下的关键词)。
//!
//! 每个关键词归属一个行业(industry_id),用于驱动采集。

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "keywords")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: String,
    /// 所属行业 ID(industries.id)。
    pub industry_id: String,
    pub word: String,
    pub created_at: i64,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
