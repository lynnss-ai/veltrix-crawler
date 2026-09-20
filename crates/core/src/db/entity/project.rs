//! 项目表 SeaORM 实体(运营 - 客户管理 / 项目信息)。
//!
//! customer_id 为逻辑外键(关联 customers.id),客户删除后悬空,前端按「未关联客户」兜底显示。

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "projects")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: String,
    /// 项目编码(如 PRJ-XXXX),系统生成,全表唯一。
    pub code: String,
    /// 项目名称。
    pub name: String,
    /// 所属客户(customers.id)。
    pub customer_id: String,
    /// 项目开始 / 结束日期(YYYY-MM-DD,空串 = 未设)。
    pub start_date: String,
    pub end_date: String,
    #[sea_orm(column_type = "Text")]
    pub remark: String,
    /// 归属用户(创建人)。
    pub owner: String,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
