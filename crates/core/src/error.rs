//! 全局统一错误类型。
//!
//! 为什么单独定义:采集链路跨越 HTTP、签名桥接、解析、持久化、上报多个层级,
//! 统一错误类型便于在调度引擎里按错误种类决定「重试 / 换账号 / 丢弃」策略。

// 部分错误分类与判定方法待调度引擎接入,暂保留
#![allow(dead_code)]

use thiserror::Error;

/// 采集平台无关的统一错误。平台特有的错误码通过 `Platform` 变体携带上下文。
#[derive(Debug, Error)]
pub enum CrawlerError {
    #[error("配置错误: {0}")]
    Config(String),

    /// 鉴权失败(用户名/密码错误、账号禁用等),消息直接面向用户、无前缀。
    #[error("{0}")]
    Auth(String),

    /// 未注册的平台 ID。可配置架构下,任务可能引用尚未注册适配器的平台。
    #[error("未知平台: {0}")]
    UnknownPlatform(String),

    #[error("HTTP 请求失败: {0}")]
    Http(#[from] reqwest::Error),

    /// 签名桥接失败(WebView 未就绪 / 注入脚本异常 / 超时)。
    #[error("签名生成失败: {0}")]
    Sign(String),

    /// 目标平台返回了风控响应(验证码 / 频控 / 登录态失效),调度层据此降级换号。
    #[error("触发风控: 平台={platform} 详情={detail}")]
    RiskControl { platform: String, detail: String },

    #[error("响应解析失败: {0}")]
    Parse(String),

    #[error("账号不可用: {0}")]
    Account(String),

    #[error("数据上报失败: {0}")]
    Report(String),

    #[error("数据库错误: {0}")]
    Storage(#[from] sea_orm::DbErr),

    #[error("IO 错误: {0}")]
    Io(#[from] std::io::Error),

    #[error("JSON 错误: {0}")]
    Json(#[from] serde_json::Error),
}

/// 全局 Result 别名,业务代码统一返回此类型并用 `?` 传播。
pub type Result<T> = std::result::Result<T, CrawlerError>;

impl CrawlerError {
    /// 该错误是否值得重试。调度引擎用它区分「临时故障」与「永久失败」。
    pub fn is_retryable(&self) -> bool {
        match self {
            CrawlerError::Http(e) => e.is_timeout() || e.is_connect(),
            CrawlerError::Sign(_) => true,
            CrawlerError::RiskControl { .. } => true,
            CrawlerError::Report(_) => true,
            _ => false,
        }
    }

    /// 该错误是否意味着当前账号应被降级 / 轮换。
    pub fn should_rotate_account(&self) -> bool {
        matches!(
            self,
            CrawlerError::RiskControl { .. } | CrawlerError::Account(_)
        )
    }
}

/// 让错误能直接跨 Tauri IPC 边界返回给前端。
impl serde::Serialize for CrawlerError {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}
