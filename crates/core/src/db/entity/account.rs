//! 账号表 SeaORM 实体。字段与领域模型 `cookie::Account` 一一对应。
//!
//! 刻意用基础标量类型(String / i64),保证 SQLite 与 PostgreSQL 两后端 DDL 通用。

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "accounts")]
pub struct Model {
    /// 账号唯一 ID,业务侧生成,不自增。
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: String,
    pub platform: String,
    pub label: String,
    /// 完整 Cookie 串,可能较长,用 Text 列。
    #[sea_orm(column_type = "Text")]
    pub cookie: String,
    /// 状态字符串:active / cooldown / invalid / disabled。
    pub status: String,
    pub risk_count: i64,
    pub cooldown_until: i64,
    pub last_used_at: i64,
    pub created_at: i64,
    /// 业务编码(如 ACC-XXXX),系统生成。
    pub code: String,
    #[sea_orm(column_type = "Text")]
    pub remark: String,
    /// 归属用户(创建者),用于按用户隔离数据。
    pub owner: String,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
