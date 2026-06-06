//! 状态快照采集。MVP 阶段返回占位结构,真接采集引擎时从 webview pool/cookie pool 抓取实时数据。

use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct StatusSnapshot {
    pub tasks: Vec<serde_json::Value>,
    pub accounts: Vec<serde_json::Value>,
    pub today_collected: u64,
    pub risk_count_24h: u64,
    pub recent_activities: Vec<String>,
    pub last_error: Option<String>,
}

impl StatusSnapshot {
    /// 占位实现:返回空快照。真接 AppState 后,这里读 cookies/webviews/tasks 等
    pub fn snapshot() -> Self {
        Self::default()
    }
}
