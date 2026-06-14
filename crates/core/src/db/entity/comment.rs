//! 采集评论落库实体。
//!
//! 结构同 content,与任务(task_id)绑定;`parent_id` 为空表示一级评论,非空指向其一级评论。
//! author / extra 等复合字段序列化为 JSON 字符串存 TEXT,跨 SQLite/PG 通用。

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "comments")]
pub struct Model {
    /// 主键:`{task_id}-{platform}-{comment_id}`,同任务内对同一评论去重
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: String,
    /// 所属任务(tasks.id 的逻辑外键)
    pub task_id: String,
    /// 平台 id(platforms.id 的弱关联)
    pub platform: String,
    /// 所属内容的平台内 ID(contents.content_id 的弱关联)
    pub content_id: String,
    /// 平台内评论唯一 ID
    pub comment_id: String,
    /// 父评论 ID;一级评论为空,楼中楼回复指向其一级评论
    pub parent_id: Option<String>,
    /// 评论作者平台内 UID
    pub author_uid: String,
    pub author_nickname: String,
    /// 完整作者信息 JSON
    pub author_json: String,
    pub text: String,
    pub like_count: Option<i64>,
    pub reply_count: Option<i64>,
    /// 评论发表时间(Unix 秒)
    pub created_at: Option<i64>,
    /// 数据归属:继承任务 owner(users.username 弱关联)
    pub owner: String,
    /// 采集时间(Unix 秒)
    pub collected_at: i64,
    /// AI 意向分析等级:high / medium / low / none;None=尚未分析
    pub intent_level: Option<String>,
    /// AI 意向分析理由;None=尚未分析
    pub intent_reason: Option<String>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
