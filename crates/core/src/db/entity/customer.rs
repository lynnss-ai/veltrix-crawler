//! 客户表 SeaORM 实体(基础设施 - 客户管理 / CRM)。
//!
//! owner 为归属用户(跟踪人,关联 users.username 或 id);tags 以 JSON 字符串存多标签。

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "customers")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: String,
    /// 客户编号(如 CUS-XXXX),系统生成。
    pub code: String,
    pub name: String,
    pub phone: String,
    pub email: String,
    pub company: String,
    pub position: String,
    pub wechat: String,
    /// 所属行业(名称或行业 code)。
    pub industry: String,
    /// 标签数组,以 JSON 字符串存储(如 ["高意向","KOL"])。
    #[sea_orm(column_type = "Text")]
    pub tags: String,
    pub source: String,
    /// 客户状态:new / following / negotiating / closed / lost / dormant。
    pub status: String,
    /// 归属用户(跟踪人)。
    pub owner: String,
    #[sea_orm(column_type = "Text")]
    pub remark: String,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
