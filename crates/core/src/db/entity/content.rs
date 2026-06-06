//! 采集内容落库实体。
//!
//! run_task 后台采集 → 平台适配器解析为统一模型(src-tauri 的 model::Content)→ 落库到此表。
//! 与任务(task_id)绑定,同任务重采前按 task_id 清旧再插新,保证为最近一次采集快照。
//! author / image_urls / extra 等复合字段序列化为 JSON 字符串存 TEXT,跨 SQLite/PG 通用。

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "contents")]
pub struct Model {
    /// 主键:`{task_id}-{platform}-{content_id}`,同任务内对同一内容去重(重采覆盖)
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: String,
    /// 所属任务(tasks.id 的逻辑外键)
    pub task_id: String,
    /// 平台 id(platforms.id 的弱关联)
    pub platform: String,
    /// 平台内内容唯一 ID(抖音 aweme_id / 小红书 note_id / 快手 photo_id)
    pub content_id: String,
    /// 采集时命中的关键词(由 run_task 当前 keyword 填),用于全量库按词筛选
    pub keyword: String,
    /// 内容形态:video / image / article / unknown
    pub kind: String,
    pub title: Option<String>,
    pub desc: Option<String>,
    /// 作者平台内 UID
    pub author_uid: String,
    pub author_nickname: String,
    /// 完整作者信息 JSON(头像/签名/粉丝数等,避免频繁加列)
    pub author_json: String,
    pub like_count: Option<i64>,
    pub comment_count: Option<i64>,
    pub collect_count: Option<i64>,
    pub share_count: Option<i64>,
    pub play_count: Option<i64>,
    /// 发布时间(Unix 秒)
    pub published_at: Option<i64>,
    /// 无水印视频直链(视频且解析成功时)
    pub video_url: Option<String>,
    /// 封面图地址:视频封面 / 图文首图
    pub cover_url: Option<String>,
    /// 图片地址 JSON 数组,例如 `["url1","url2"]`
    pub image_urls: String,
    /// 视频时长(秒);图文为 None
    pub duration: Option<i64>,
    /// 话题标签 JSON 数组(# 开头),例如 `["#话题a","#话题b"]`
    pub topics: String,
    /// 平台特有字段原始 JSON
    pub extra: String,
    /// 数据归属:继承任务 owner(users.username 弱关联)
    pub owner: String,
    /// 采集时间(Unix 秒)
    pub collected_at: i64,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
