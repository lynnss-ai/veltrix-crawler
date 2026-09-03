//! 发布服务:独立发布账号池。
//!
//! 与采集账号池(`cookie` 模块)刻意分表分服务:采集账号只管「读」、按最久未用轮换;
//! 发布账号只管「写」,按客户(CRM customers 表)分组、不做轮换,风控与频控策略后续独立演进。
//! 登录复用与采集相同的 WebView 通道(登录窗口 + 页内自检上报),但窗口 label 用
//! `veltrix-pub-` 前缀,数据目录与采集账号互不串。

pub mod account;

pub use account::{NewAccount, PublishAccounts};

/// 登录自检上报的发布账号 id 前缀:`pub:{account_id}`。
/// 注入脚本无法改 invoke 命令名,故借 account_id 前缀区分发布账号,
/// `login_status_report` 据此把上报路由到发布账号池而非采集账号池。
pub const LOGIN_REPORT_PREFIX: &str = "pub:";

/// 发布账号登录态变化(置 active / invalid、Cookie 回写)后推给前端的事件名,
/// payload 为平台 id(对齐采集侧的 account-login-updated),前端 listen 后刷新列表。
pub const ACCOUNT_UPDATED_EVENT: &str = "publish-account-updated";
