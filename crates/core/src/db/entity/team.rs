//! 团队表 SeaORM 实体(创作 - 团队管理)。
//!
//! member_ids 以 JSON 字符串存成员用户 id 数组(关联 users.id,逻辑外键,同 customer.tags 口径);
//! 用户被删除后成员 id 悬空,前端按现有用户列表过滤显示。

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "teams")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: String,
    /// 团队编码(如 TEAM-XXXX),系统生成,全表唯一。
    pub code: String,
    /// 团队名称。
    pub name: String,
    /// 成员用户 id 数组,JSON 字符串存储(如 ["u1","u2"])。
    #[sea_orm(column_type = "Text")]
    pub member_ids: String,
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
