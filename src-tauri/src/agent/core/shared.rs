//! 跨智能体复用的共享件:聊天消息视图(落库行 ↔ 前端视图)+ ReAct 持久化/标题工具。
//! 原先散落在 chat / coding 命令里、被另一智能体借用;上移到 core 后,chat / coding / rpa
//! 三者都只依赖本模块,彼此不再直接耦合(加 / 删某个智能体不牵连其它)。

use serde::Serialize;
use serde_json::{json, Value};
use veltrix_core::db::entity::chat_message as msg;

use super::llm::{ChatMsg, ToolCall};

/// ReAct 最大步数(防失控循环)。chat / coding / rpa 命令共用,统一一处定义。
pub const MAX_ITERS: usize = 25;

/// 一条消息附件的展示元数据(前端历史渲染用)。图片带本地绝对 path(走 asset 协议),非图片 path 空。
#[derive(serde::Deserialize, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct MessageAttachmentView {
    pub name: String,
    pub mime: String,
    /// 本地绝对路径(仅图片落盘后非空);前端 convertFileSrc 读取,空则只展示文件名 chip。
    pub path: String,
}

/// 一条消息的对外视图(三类智能体共用:都落 chat_messages 表、都按此返回前端)。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageView {
    pub id: i64,
    pub conversation_id: String,
    pub role: String,
    pub content: String,
    /// assistant 的工具调用(JSON 字符串,前端解析展示);纯文本为 None
    pub tool_calls: Option<String>,
    /// role=tool:对应的工具调用 id
    pub tool_call_id: Option<String>,
    /// role=tool:工具名
    pub tool_name: Option<String>,
    /// user 消息附件(图片缩略图 + 文件 chip);无附件为空数组
    pub attachments: Vec<MessageAttachmentView>,
    pub created_at: i64,
}

impl From<msg::Model> for MessageView {
    fn from(m: msg::Model) -> Self {
        let attachments = m
            .attachments
            .as_deref()
            .and_then(|s| serde_json::from_str::<Vec<MessageAttachmentView>>(s).ok())
            .unwrap_or_default();
        Self {
            id: m.id,
            conversation_id: m.conversation_id,
            role: m.role,
            content: m.content,
            tool_calls: m.tool_calls,
            tool_call_id: m.tool_call_id,
            tool_name: m.tool_name,
            attachments,
            created_at: m.created_at,
        }
    }
}

/// DB 消息行 → 统一 ChatMsg;无法识别的角色跳过。
pub fn row_to_chat_msg(m: &msg::Model) -> Option<ChatMsg> {
    match m.role.as_str() {
        "user" => Some(ChatMsg::User(m.content.clone())),
        "assistant" => {
            let tool_calls = m
                .tool_calls
                .as_deref()
                .map(parse_tool_calls)
                .unwrap_or_default();
            let text = if m.content.is_empty() {
                None
            } else {
                Some(m.content.clone())
            };
            Some(ChatMsg::Assistant { text, tool_calls })
        }
        "tool" => Some(ChatMsg::Tool {
            tool_call_id: m.tool_call_id.clone().unwrap_or_default(),
            content: m.content.clone(),
        }),
        _ => None,
    }
}

/// 解析 DB 里存的 tool_calls JSON([{id,name,arguments}])为 Vec<ToolCall>。
pub fn parse_tool_calls(json_str: &str) -> Vec<ToolCall> {
    serde_json::from_str::<Value>(json_str)
        .ok()
        .and_then(|v| v.as_array().cloned())
        .map(|arr| {
            arr.iter()
                .filter_map(|tc| {
                    Some(ToolCall {
                        id: tc.get("id")?.as_str()?.to_string(),
                        name: tc.get("name")?.as_str()?.to_string(),
                        arguments: tc.get("arguments").cloned().unwrap_or(Value::Null),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Vec<ToolCall> → 落库用 JSON 字符串。
pub fn tool_calls_to_json(calls: &[ToolCall]) -> String {
    let arr: Vec<Value> = calls
        .iter()
        .map(|tc| json!({ "id": tc.id, "name": tc.name, "arguments": tc.arguments }))
        .collect();
    Value::Array(arr).to_string()
}

/// 用首条用户消息生成标题:取前 24 个字符,去换行。
pub fn truncate_title(text: &str) -> String {
    let one_line = text.replace(['\n', '\r'], " ");
    let trimmed = one_line.trim();
    let chars: Vec<char> = trimmed.chars().collect();
    if chars.len() <= 24 {
        trimmed.to_string()
    } else {
        let mut s: String = chars[..24].iter().collect();
        s.push('…');
        s
    }
}
