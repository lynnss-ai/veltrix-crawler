//! 发布账号表 SeaORM 实体(发布服务)。
//!
//! 与采集账号池(accounts 表)相互独立:采集账号只管「读」,发布账号只管「写」,
//! 风控与频控策略不同,刻意分表避免互相污染。字段刻意用基础标量类型(String / i64),
//! 保证 SQLite 与 PostgreSQL 两后端 DDL 通用。

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "publish_accounts")]
pub struct Model {
    /// 账号唯一 ID,业务侧生成,不自增。
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: String,
    /// 平台 id(douyin / xhs 等,platforms.id 的弱关联)。
    pub platform: String,
    /// 所属客户(customers.id 的逻辑外键,禁物理 FK):发布账号按 CRM 客户分组,不再单独维护分类表。
    pub category_id: String,
    /// 备注名(用户起的展示名,与平台昵称解耦,昵称变了不影响识别)。
    pub label: String,
    /// 平台侧昵称,导入后可能未回填,默认 ''。
    pub nickname: String,
    /// 平台侧头像地址,默认 ''。
    pub avatar: String,
    /// 平台侧 uid(发布接口需要;导入 Cookie 时未必能解析出,默认 '')。
    pub uid: String,
    /// 完整 Cookie 串,可能较长,用 Text 列;按账号隔离使用,禁止打印日志。
    #[sea_orm(column_type = "Text")]
    pub cookie: String,
    /// 状态字符串:active / invalid(失效) / limited(被限流) / disabled(手动停用)。
    pub status: String,
    /// 最近一次失败 / 受限原因(风控提示、接口报错等),供前端展示与人工处置;Text 列。
    #[sea_orm(column_type = "Text")]
    pub fail_reason: String,
    /// 业务编码(如 PAC-XXXX),系统生成,对齐 accounts 表的 code。
    pub code: String,
    /// 最近一次登录 / Cookie 校验成功时间(unix 秒),用于判断登录态新鲜度。
    pub last_login_at: i64,
    /// 最近一次发布成功时间(unix 秒),0 = 从未发布。
    pub last_publish_at: i64,
    /// 当日已发布条数,频控用;与 today_date 配合。
    pub today_published: i64,
    /// 当日日期(YYYY-MM-DD):发布时若与当前日期不一致,先把 today_published 清零再计数,
    /// 用「字段比对」替代定时任务实现跨天滚动清零。
    pub today_date: String,
    /// 归属用户(创建者),用于按用户隔离数据。
    pub owner: String,
    pub created_at: i64,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
