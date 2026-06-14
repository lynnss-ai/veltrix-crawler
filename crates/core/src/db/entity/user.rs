//! 用户表 SeaORM 实体(系统管理 - 用户管理)。
//!
//! 仅用基础标量类型,保证 SQLite / PostgreSQL 两后端 DDL 通用。
//! 密码仅存哈希,不落明文;data_scope 控制业务数据可见范围(all / self)。

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "users")]
pub struct Model {
    /// 用户唯一 ID,业务侧生成,不自增。
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: String,
    pub username: String,
    /// 密码哈希(如 argon2 / bcrypt),禁止存明文。
    pub password_hash: String,
    pub email: String,
    pub nickname: String,
    /// 头像 URL 或 base64 data URL,可能较长,用 Text 列。
    #[sea_orm(column_type = "Text")]
    pub avatar: String,
    pub remark: String,
    /// 状态:enabled / disabled。
    pub status: String,
    /// 数据级别:all(全部数据) / self(仅自己)。
    pub data_scope: String,
    /// 该用户的 Obsidian vault 根路径(每用户各自配置);空=未配置,不能同步
    #[sea_orm(column_type = "Text")]
    pub obsidian_vault_path: String,
    pub created_at: i64,
    pub updated_at: i64,
    /// 软删除标记,0 表示未删除。
    pub deleted_at: i64,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
