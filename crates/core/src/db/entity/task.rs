//! 采集任务表 SeaORM 实体。
//!
//! 字段映射 src/pages/CollectPage.tsx 的 TaskItem,字段命名采用 snake_case;
//! 复合字段(keywords)序列化为 JSON 字符串存 TEXT,跨 SQLite/PG 通用。

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "tasks")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: String,
    pub name: String,
    /// 行业名称(冗余存名称而非 industry_id,前端直接读 industries 自由扩展)
    pub industry: String,
    /// 平台 id(platforms.id 的弱关联,逻辑外键)
    pub platform: String,
    /// 关键词 JSON 数组,例如 `["a","b"]`
    pub keywords: String,
    /// 触发类型:once-now / daily / watching
    pub trigger_type: String,
    /// 每日定时执行时分,格式 HH:mm(仅 trigger_type=daily)
    pub scheduled_at: Option<String>,
    /// 持续监听轮询分钟数(仅 trigger_type=watching)
    pub watch_interval_min: Option<i32>,
    /// 排序方式:synthetic / hottest / latest
    pub sort_mode: String,
    /// 发布时间范围:any / 1d / 1w / 6m
    pub time_range: String,
    /// 每个关键词最多返回条数
    pub per_keyword_limit: i32,
    /// 最低点赞数(<该值丢弃)
    pub min_likes: i32,
    /// 是否启用 AI 文案提取
    pub ai_extract: bool,
    /// 是否采集评论(开启后内容采集完进入评论采集阶段)
    pub collect_comments: bool,
    /// 评论发布时间范围过滤:3d / 7d / 14d / any(不限)
    pub comment_time_range: String,
    /// 单视频一级评论采集上限,0 表示不限
    pub comment_limit: i32,
    /// 是否对评论做 AI 意图分析(本阶段仅透传入库,分析逻辑后续接入)
    pub analyze_comment_intent: bool,
    /// 运行状态:pending / running / collecting_comments(评论采集中) / downloading_media(素材下载中) / completed / failed / cancelled
    pub status: String,
    /// 进度 0-100
    pub progress: i32,
    /// 素材下载总数(进入 downloading_media 时确定 = 去重后待下载内容数,0 表示无素材)
    pub media_total: i32,
    /// 素材已处理数(成功 + 失败均计入),= media_total 时任务转 completed
    pub media_done: i32,
    /// 评论采集阶段:待采视频总数(进入 collecting_comments 时确定 = 去重后内容数)
    pub comment_video_total: i32,
    /// 评论采集阶段:已采视频数,= comment_video_total 时转入素材下载 / 完成
    pub comment_video_done: i32,
    /// 已采集内容数
    pub content_count: i64,
    /// 已采集评论数
    pub comment_count: i64,
    /// 首次启动时间(unix 秒)
    pub started_at: Option<i64>,
    /// 结束时间(unix 秒,归档后填)
    pub finished_at: Option<i64>,
    /// 失败原因(仅 status=failed)
    pub error_message: Option<String>,
    /// 数据归属:任务所属用户名(users.username 的弱关联)
    pub owner: String,
    /// 是否已归档(手动归档后移入归档 tab;终止/失败不自动归档)
    pub archived: bool,
    /// 采集完成后是否自动同步内容到发起者(owner)的 Obsidian vault
    pub auto_sync_obsidian: bool,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
