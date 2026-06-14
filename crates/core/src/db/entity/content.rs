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
    /// 素材下载状态:pending(待处理) / success(成功) / failed(失败)。None=旧数据,未跑过下载
    pub media_status: Option<String>,
    /// 音频是否提取成功:仅「视频 + 开启音频提取」时有意义,其余为 None
    pub audio_extracted: Option<bool>,
    /// 素材失败原因(视频 403 / ffmpeg 转码失败等),供前端展示与失败重试判断
    pub media_error: Option<String>,
    /// 封面本地绝对路径(下载成功后回写):前端本地优先显示,None=未下载/失败回退外链
    pub cover_path: Option<String>,
    /// 作者头像本地绝对路径(下载成功/已存在后回写);None=未下载/失败回退外链
    pub avatar_path: Option<String>,
    /// 视频转出音频(mp3 等)本地绝对路径;仅「视频 + 音频提取成功」时有值,详情页播放用
    pub audio_path: Option<String>,
    /// 视频语音转写文本;仅视频且转写成功时有值,None=未转写/非视频/失败
    pub transcript: Option<String>,
    /// 转写失败原因(供前端区分「未转写」与「转写失败」)
    pub transcript_error: Option<String>,
    /// 视频文件是否下载成功(仅 video + ai_extract);None=非视频/未尝试
    pub video_downloaded: Option<bool>,
    /// 图文图片总数 / 已成功下载数(仅 image);None=非图文
    pub image_total: Option<i32>,
    pub image_done: Option<i32>,
    /// 是否已采集评论(评论采集阶段后回写);None=未到评论阶段/未开评论采集
    pub comment_collected: Option<bool>,
    /// 是否已做意向分析(意向分析后回写);None=未分析
    pub intent_analyzed: Option<bool>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
