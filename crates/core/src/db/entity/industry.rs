//! 行业表 SeaORM 实体(基础设施 - 行业类别)。
//!
//! 行业用于归类采集关键词;code 为系统生成的业务编码。

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "industries")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: String,
    /// 业务编码(如 IND-XXXX),系统生成。
    pub code: String,
    pub name: String,
    pub remark: String,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
