//! SeaORM 实体集合。新增表 = 加一个实体模块 + 在 `db::init_schema` 建表。

pub mod account;
pub mod customer;
pub mod industry;
pub mod keyword;
pub mod platform_api;
pub mod prompt;
pub mod provider;
pub mod user;
