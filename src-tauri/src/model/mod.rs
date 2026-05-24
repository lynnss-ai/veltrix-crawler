//! 跨平台统一数据模型。
//!
//! 设计取舍:抖音/小红书/快手及后续平台字段差异很大,这里只抽取「共性字段」做强类型,
//! 平台特有字段统一塞进 `extra`(原始 JSON 子集),既保证上报结构稳定,
//! 又不会因为新增平台而频繁改动核心结构体。

// 采集任务/条目模型待调度引擎接入,暂保留
#![allow(dead_code)]

use serde::{Deserialize, Serialize};

/// 平台标识。刻意用字符串而非枚举:新增平台只需加配置 + 注册适配器,无需改本类型。
pub type PlatformId = String;

/// 内容形态。`Unknown` 兜底,避免新平台出现未知类型时反序列化失败。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ContentKind {
    Video,
    Image,
    Article,
    Unknown,
}

impl Default for ContentKind {
    fn default() -> Self {
        ContentKind::Unknown
    }
}

/// 作者 / 博主。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Author {
    pub platform: PlatformId,
    /// 平台内用户唯一 ID(抖音 sec_uid、小红书 user_id 等)。
    pub uid: String,
    pub nickname: String,
    pub avatar: Option<String>,
    pub signature: Option<String>,
    pub follower_count: Option<i64>,
    pub following_count: Option<i64>,
    /// 平台特有字段原样保留(如抖音 unique_id、小红书红薯号)。
    #[serde(default)]
    pub extra: serde_json::Value,
}

/// 互动统计。各平台命名不同,统一归一化到这里。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Stats {
    pub like_count: Option<i64>,
    pub comment_count: Option<i64>,
    pub collect_count: Option<i64>,
    pub share_count: Option<i64>,
    pub play_count: Option<i64>,
}

/// 一条内容(视频 / 图文 / 笔记)。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Content {
    pub platform: PlatformId,
    /// 平台内内容唯一 ID(抖音 aweme_id、小红书 note_id、快手 photo_id)。
    pub content_id: String,
    pub kind: ContentKind,
    pub title: Option<String>,
    pub desc: Option<String>,
    pub author: Author,
    pub stats: Stats,
    /// 发布时间(Unix 秒)。平台多为秒级时间戳,统一存秒。
    pub published_at: Option<i64>,
    /// 无水印视频地址(若为视频且解析成功),供阶段5「视频转音频」使用。
    pub video_url: Option<String>,
    pub image_urls: Vec<String>,
    /// 采集时间(Unix 秒)。
    pub collected_at: i64,
    #[serde(default)]
    pub extra: serde_json::Value,
}

/// 一条评论(含二级回复)。`parent_id` 为空表示一级评论。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Comment {
    pub platform: PlatformId,
    pub content_id: String,
    pub comment_id: String,
    /// 父评论 ID;一级评论为 None,楼中楼回复指向其一级评论。
    pub parent_id: Option<String>,
    pub author: Author,
    pub text: String,
    pub like_count: Option<i64>,
    pub reply_count: Option<i64>,
    pub created_at: Option<i64>,
    pub collected_at: i64,
    #[serde(default)]
    pub extra: serde_json::Value,
}

/// 采集任务类型。对应阶段2 各适配器需实现的能力。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TaskKind {
    /// 内容详情。
    ContentDetail,
    /// 评论与二级回复。
    Comments,
    /// 用户主页信息。
    UserProfile,
    /// 用户作品列表(分页)。
    UserPosts,
    /// 关键词搜索。
    Search,
    /// 榜单 / 热榜监控。
    Rank,
}

/// 采集结果的统一载体,供上报模块消费。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CollectItem {
    Content(Content),
    Comment(Comment),
    Author(Author),
}

/// 一个具体的采集任务定义。`target` 语义随 `kind` 变化:
/// 详情/评论=内容ID,用户=用户ID,搜索=关键词,榜单=榜单标识。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CollectTask {
    pub id: String,
    pub platform: PlatformId,
    pub kind: TaskKind,
    pub target: String,
    /// 任务级覆盖参数(分页上限、是否抓二级回复、是否转音频等),由适配器解释。
    #[serde(default)]
    pub params: serde_json::Value,
}
