//! 平台接口表 SeaORM 实体(基础设施 - 平台管理下的 API 子管理)。
//!
//! 平台基础信息仍存于配置文件(PlatformConfig);此表登记每个平台下的接口条目。

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "platform_apis")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: String,
    /// 所属平台 ID(配置文件中的平台 id)。
    pub platform_id: String,
    pub name: String,
    #[sea_orm(column_type = "Text")]
    pub url: String,
    pub remark: String,
    pub created_at: i64,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
