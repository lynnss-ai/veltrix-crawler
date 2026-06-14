//! 作者表:平台创作者去重档案。
//!
//! 采集时按 (owner, platform, uid) upsert,刷新最新画像(粉丝/获赞/属地等)。
//! 主键 `{owner}-{platform}-{uid}` 兼顾数据归属与作者去重。
//! content/comment 仍各存作者快照(采集那一刻),本表提供「一个作者一行、可更新、可监控」的聚合视角。

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "authors")]
pub struct Model {
    /// 主键:`{owner}-{platform}-{uid}`
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: String,
    /// 数据归属(users.username 弱关联)
    pub owner: String,
    pub platform: String,
    /// 作者 UID(抖音为 sec_uid)
    pub uid: String,
    pub nickname: String,
    pub avatar: Option<String>,
    /// 平台号(抖音号 unique_id 等)
    pub platform_id: Option<String>,
    /// 平台短 ID(extra.uid)
    pub short_id: Option<String>,
    #[sea_orm(column_type = "Text")]
    pub signature: Option<String>,
    pub follower_count: Option<i64>,
    pub following_count: Option<i64>,
    /// 作者获赞总数(部分平台返回,缺失为 None)
    pub total_favorited: Option<i64>,
    /// IP 属地(部分平台返回,缺失为 None)
    pub location: Option<String>,
    /// 是否被持续监控(作者级监控开关)
    pub is_monitored: bool,
    /// 首次采集 / 最近采集时间(Unix 秒)
    pub first_collected_at: i64,
    pub last_collected_at: i64,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
