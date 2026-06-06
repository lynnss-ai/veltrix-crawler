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
    /// 运行状态:pending / running / paused / completed / failed / cancelled
    pub status: String,
    /// 进度 0-100
    pub progress: i32,
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
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
