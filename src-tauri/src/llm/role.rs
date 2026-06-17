//! Agent 角色:在「选模型」这一层加维度,让杂活(分类 / 摘要 / 套用)可走便宜模型,
//! 主任务仍走会话绑定的模型。只描述角色枚举与字符串编解码,不碰 LlmProvider / ChatMsg 下游契约。
//!
//! 角色 → 模型的映射存 app_secrets KV:key 形如 `role_model_classify`,值为 `providerId::model`
//! (与前端编码一致);空值表示回退到会话绑定的模型。映射解析在 `commands::resolve_role_provider`。

/// 大模型调用的角色场景。Chat 为主任务(默认会话模型);其余为可单独配廉价档的杂活。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentRole {
    /// 主对话 / 主循环:始终用会话绑定模型,不参与角色化降档(保留枚举以统一表达)。
    Chat,
    /// 意图分类(coding / chat 判定)等只回一个词的轻量判别。
    Classify,
    /// 摘要 / 标题 / 记忆提取等后台压缩类杂活。
    Summary,
    /// 套用 / 应用改动(编程 Agent 的 apply 场景预留)。
    Apply,
}

impl AgentRole {
    /// 角色稳定字符串标识;同时作为 app_secrets key 的后缀(`role_model_<as_str>`)。
    pub fn as_str(self) -> &'static str {
        match self {
            AgentRole::Chat => "chat",
            AgentRole::Classify => "classify",
            AgentRole::Summary => "summary",
            AgentRole::Apply => "apply",
        }
    }

    /// 由字符串解析角色;未知值返回 None(调用方据此回退,而非报错)。
    /// 与 `as_str` 对称构成角色编解码 API,供按字符串选择角色的入口复用(暂未全用上,先留地基)。
    #[allow(dead_code)]
    pub fn from_str(value: &str) -> Option<Self> {
        match value {
            "chat" => Some(AgentRole::Chat),
            "classify" => Some(AgentRole::Classify),
            "summary" => Some(AgentRole::Summary),
            "apply" => Some(AgentRole::Apply),
            _ => None,
        }
    }

    /// 该角色在 app_secrets 中的映射 key(`role_model_<role>`)。
    pub fn secret_key(self) -> String {
        format!("role_model_{}", self.as_str())
    }
}
