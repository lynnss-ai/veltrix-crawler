//! 采集去重台账实体。每条已采集内容记一行 (platform, content_id),独立于业务数据:
//! 「清空业务数据」不清空本表。再次采集时据此判重,跳过曾经采过的内容,
//! 避免清空(或删任务)后重采时把旧内容重复入库。

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "collect_records")]
pub struct Model {
    /// 主键 = `ledger_key(platform, content_id)`,保证同平台同内容全局唯一。
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: String,
    /// 平台 id(douyin / xhs ...)
    pub platform: String,
    /// 平台侧内容 id(去重的核心维度)
    pub content_id: String,
    /// 首次采集(登记)时间,Unix 秒
    pub created_at: i64,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}

/// 台账主键:平台 + 内容 id 拼接,作为全局唯一去重键。
/// 用 `::` 分隔,避免平台 id 含连字符时与内容 id 边界混淆。
pub fn ledger_key(platform: &str, content_id: &str) -> String {
    format!("{platform}::{content_id}")
}
