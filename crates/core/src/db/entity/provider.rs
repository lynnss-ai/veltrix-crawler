//! 模型厂商表(系统配置 - 模型厂商)。models 以多行文本存储(每行一个模型)。

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
    /// 可用模型,多行文本(每行一个)。
    #[sea_orm(column_type = "Text")]
    pub models: String,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
