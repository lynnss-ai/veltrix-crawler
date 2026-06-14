//! 采集日志落库实体。每条 collect-log 事件持久化一行,供任务详情页加载历史日志。
//! `entry_json` 存富条目(内容/评论)的 JSON;普通日志为 None。

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "collect_logs")]
pub struct Model {
    /// 自增主键(日志量大,用整型自增而非 UUID)
    #[sea_orm(primary_key)]
    pub id: i64,
    /// 所属任务(tasks.id 的逻辑外键)
    pub task_id: String,
    /// 产生时间(Unix 秒)
    pub ts: i64,
    /// 级别:info / warn / error
    pub level: String,
    #[sea_orm(column_type = "Text")]
    pub message: String,
    /// 富条目(内容/评论)JSON;普通日志为 None
    #[sea_orm(column_type = "Text", nullable)]
    pub entry_json: Option<String>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
