//! 任务执行历史实体。每次运行 `run_task` 记一条:记录起止时间、终态、本次新增量。
//!
//! 「采集日志」按时间范围关联到运行:list_run_logs 用 (started_at, finished_at) 过滤 collect_logs。
//! 同账号采集串行(account_collect_lock),两次运行时间不重叠,故时间范围切分准确。

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "task_runs")]
pub struct Model {
    /// 主键:运行 id(`{task_id}-run-{started_ts}`,同任务两次运行起始秒不同,唯一)
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: String,
    /// 所属任务(tasks.id 的逻辑外键)
    pub task_id: String,
    /// 数据归属:继承任务 owner
    pub owner: String,
    /// 本次运行开始时间(Unix 秒);与该次采集内容的 collected_at 起点一致
    pub started_at: i64,
    /// 本次运行结束时间(Unix 秒);运行中为 None
    pub finished_at: Option<i64>,
    /// 终态:running / completed / failed / cancelled
    pub status: String,
    /// 本次新增内容数(collected_at >= started_at,即排除重复采到的已有内容)
    pub content_delta: i64,
    /// 本次新增评论数
    pub comment_delta: i64,
    /// 失败原因;None 表示无
    pub error_message: Option<String>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
