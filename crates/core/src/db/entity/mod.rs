//! SeaORM 实体集合。新增表 = 加一个实体模块 + 在 `db::init_schema` 建表。

pub mod account;
pub mod author;
pub mod chat_conversation;
pub mod chat_message;
pub mod collect_log;
pub mod comment;
pub mod content;
pub mod content_synced_user;
pub mod customer;
pub mod industry;
pub mod keyword;
pub mod prompt;
pub mod provider;
pub mod task;
pub mod task_run;
pub mod user;
