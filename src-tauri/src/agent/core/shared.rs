//! 跨智能体复用的共享件:聊天消息视图(落库行 ↔ 前端视图)+ ReAct 持久化/标题工具。
//! 原先散落在 chat / coding 命令里、被另一智能体借用;上移到 core 后,chat / coding / rpa
//! 三者都只依赖本模块,彼此不再直接耦合(加 / 删某个智能体不牵连其它)。

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use chrono::Utc;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, IntoActiveModel, QueryFilter,
    QueryOrder, QuerySelect, Set,
};
use serde::Serialize;
use serde_json::{json, Value};
use tokio::sync::oneshot;
use veltrix_core::db::entity::{
    chat_conversation as conv, chat_message as msg, provider as provider_entity,
};
use veltrix_core::error::{CrawlerError, Result};

use super::llm::{ChatMsg, LlmResponse, ToolCall};
use super::summary::LIVE_HARD_CAP;

/// ReAct 最大步数(防失控循环)。chat / coding / rpa 命令共用,统一一处定义。
pub const MAX_ITERS: usize = 25;

/// 危险操作「暂停 — 等用户确认」通道(场景无关)。
///
/// 形态同 webview::RpaChannel:为每次确认分配 confirm_id,用 oneshot 等前端回执。
/// ReAct 循环命中危险工具时 `open` 取得 id + 接收端,emit 事件让前端弹确认框;
/// 前端经 `resolve_agent_confirm` 命令 `complete(id, approved)`。等待方超时后 `cancel` 清理,
/// 迟到的 `complete` 因条目已移除被安全忽略。
#[derive(Default)]
pub struct AgentConfirmChannel {
    seq: AtomicU64,
    /// confirm_id -> 回执发送端(true=放行 / false=拒绝)。
    pending: Mutex<HashMap<u64, oneshot::Sender<bool>>>,
}

impl AgentConfirmChannel {
    pub fn new() -> Self {
        Self::default()
    }

    /// 开启一次确认,返回 confirm_id 与回执接收端。
    pub fn open(&self) -> (u64, oneshot::Receiver<bool>) {
        let id = self.seq.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        if let Ok(mut pending) = self.pending.lock() {
            pending.insert(id, tx);
        }
        (id, rx)
    }

    /// 前端回执:未登记或已超时清理的 id 安全忽略。
    pub fn complete(&self, id: u64, approved: bool) {
        if let Ok(mut pending) = self.pending.lock() {
            if let Some(tx) = pending.remove(&id) {
                let _ = tx.send(approved);
            }
        }
    }

    /// 等待方超时后清理条目,避免发送端在表里永久残留。
    pub fn cancel(&self, id: u64) {
        if let Ok(mut pending) = self.pending.lock() {
            pending.remove(&id);
        }
    }
}

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
    /// assistant 思考过程(模型推理内容);仅推理型模型非空,供前端折叠展示
    pub reasoning: Option<String>,
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
            reasoning: m.reasoning,
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

// ===================== ReAct 回合公共件(coding / rpa / computer 共用) =====================
// 三个 ReAct 智能体的「前奏 / live 窗口 / 工具往返落库 / 收尾」原本逐字复制三份;
// 抽到此处统一,各命令只保留自身差异的循环体(plan 续航 / 危险确认 / 截图回灌等)。

/// ReAct 回合前奏:校验会话归属 + 厂商 api_key,判定是否首轮,并落库本轮 user 消息。
/// 返回 (会话, 厂商, 是否首轮)。登录校验 / 文本非空由调用方在此之前完成。
pub async fn begin_agent_turn(
    db: &DatabaseConnection,
    owner: &str,
    conversation_id: &str,
    text: &str,
) -> Result<(conv::Model, provider_entity::Model, bool)> {
    let conversation = conv::Entity::find_by_id(conversation_id.to_string())
        .one(db)
        .await
        .map_err(|e| CrawlerError::Config(format!("查询会话失败: {e}")))?
        .ok_or_else(|| CrawlerError::Config("会话不存在".into()))?;
    if conversation.owner != owner {
        return Err(CrawlerError::Config("无权操作该会话".into()));
    }
    let provider = provider_entity::Entity::find_by_id(conversation.provider_id.clone())
        .one(db)
        .await
        .map_err(|e| CrawlerError::Config(format!("查询厂商失败: {e}")))?
        .ok_or_else(|| CrawlerError::Config("会话绑定的模型厂商不存在,请新建会话".into()))?;
    if provider.api_key.trim().is_empty() {
        return Err(CrawlerError::Config(
            "该模型厂商未配置 API Key,请到系统配置补全".into(),
        ));
    }
    let had_messages = msg::Entity::find()
        .filter(msg::Column::ConversationId.eq(conversation_id))
        .one(db)
        .await
        .ok()
        .flatten()
        .is_some();
    msg::ActiveModel {
        conversation_id: Set(conversation_id.to_string()),
        role: Set("user".to_string()),
        content: Set(text.to_string()),
        created_at: Set(Utc::now().timestamp()),
        ..Default::default()
    }
    .insert(db)
    .await
    .map_err(|e| CrawlerError::Config(format!("保存消息失败: {e}")))?;
    Ok((conversation, provider, had_messages))
}

/// 取会话「live 原文窗口」消息:id 大于已折叠边界、最新 LIVE_HARD_CAP 条、翻回升序、从首条 user 起
///(否则可能以 tool / assistant(tool_calls) 开头致 OpenAI 报 400)。返回转好的 ChatMsg 序列,
/// 不含 system / 摘要(调用方按各自系统提示词前置)。
pub async fn live_windowed_messages(
    db: &DatabaseConnection,
    conversation: &conv::Model,
) -> Result<Vec<ChatMsg>> {
    let mut rows = msg::Entity::find()
        .filter(msg::Column::ConversationId.eq(&conversation.id))
        .filter(msg::Column::Id.gt(conversation.summarized_upto_id))
        .order_by_desc(msg::Column::Id)
        .limit(LIVE_HARD_CAP)
        .all(db)
        .await
        .map_err(|e| CrawlerError::Config(format!("读取历史失败: {e}")))?;
    rows.reverse();
    let windowed: &[msg::Model] = match rows.iter().position(|m| m.role == "user") {
        Some(start) => &rows[start..],
        None => &[],
    };
    Ok(windowed.iter().filter_map(row_to_chat_msg).collect())
}

/// 落库一条带 tool_calls 的 assistant 消息(ReAct 中间步)。仅落库,不动内存 messages。
pub async fn insert_assistant_tool_calls(
    db: &DatabaseConnection,
    conversation_id: &str,
    resp: &LlmResponse,
) -> Result<()> {
    msg::ActiveModel {
        conversation_id: Set(conversation_id.to_string()),
        role: Set("assistant".to_string()),
        content: Set(resp.content.clone().unwrap_or_default()),
        tool_calls: Set(Some(tool_calls_to_json(&resp.tool_calls))),
        // 每步推理过程一并落库:历史里也能看到 ReAct 每一步的思考(非推理模型为空)
        reasoning: Set(resp.reasoning.clone()),
        created_at: Set(Utc::now().timestamp()),
        ..Default::default()
    }
    .insert(db)
    .await
    .map_err(|e| CrawlerError::Config(format!("保存回复失败: {e}")))?;
    Ok(())
}

/// 落库一条 tool 结果消息(content 由调用方决定:截图等可只存简短文本,不存超长 base64)。
pub async fn insert_tool_result(
    db: &DatabaseConnection,
    conversation_id: &str,
    call: &ToolCall,
    content: &str,
) -> Result<()> {
    msg::ActiveModel {
        conversation_id: Set(conversation_id.to_string()),
        role: Set("tool".to_string()),
        content: Set(content.to_string()),
        tool_call_id: Set(Some(call.id.clone())),
        tool_name: Set(Some(call.name.clone())),
        created_at: Set(Utc::now().timestamp()),
        ..Default::default()
    }
    .insert(db)
    .await
    .map_err(|e| CrawlerError::Config(format!("保存工具结果失败: {e}")))?;
    Ok(())
}

/// 落库最终 assistant 消息并返回该行(供命令转 MessageView 返回前端)。
/// reasoning:模型本轮收尾的思考过程(无工具调用直接收尾时附带;其它收尾路径传 None)。
pub async fn insert_final_assistant(
    db: &DatabaseConnection,
    conversation_id: &str,
    text: String,
    reasoning: Option<String>,
) -> Result<msg::Model> {
    msg::ActiveModel {
        conversation_id: Set(conversation_id.to_string()),
        role: Set("assistant".to_string()),
        content: Set(text),
        reasoning: Set(reasoning),
        created_at: Set(Utc::now().timestamp()),
        ..Default::default()
    }
    .insert(db)
    .await
    .map_err(|e| CrawlerError::Config(format!("保存回复失败: {e}")))
}

/// 收尾:更新会话 updated_at;首轮(此前无消息)用用户首句起标题。失败仅忽略。
pub async fn finalize_conversation_meta(
    db: &DatabaseConnection,
    conversation: conv::Model,
    had_messages: bool,
    first_text: &str,
) {
    let mut am = conversation.into_active_model();
    am.updated_at = Set(Utc::now().timestamp());
    if !had_messages {
        am.title = Set(truncate_title(first_text));
    }
    let _ = am.update(db).await;
}

/// 合法的 agent 规范种类(防路径穿越):coding / computer / rpa。
pub fn is_valid_guidelines_kind(kind: &str) -> bool {
    matches!(kind, "coding" | "computer" | "rpa")
}

/// 某 agent 规范文件路径:<config_dir>/agent-guidelines/<kind>.md。
pub fn agent_guidelines_path(config_dir: &std::path::Path, kind: &str) -> std::path::PathBuf {
    config_dir
        .join("agent-guidelines")
        .join(format!("{kind}.md"))
}

/// 读取某 agent 的「用户可编辑规范」并注入为附加 system 消息(无文件/空内容返回 None)。
/// 让质量/风格规范可由用户随时改文件或经命令调整,不必改代码、不必塞进硬编码提示词。
pub async fn load_agent_guidelines(config_dir: &std::path::Path, kind: &str) -> Option<String> {
    if !is_valid_guidelines_kind(kind) {
        return None;
    }
    let text = tokio::fs::read_to_string(agent_guidelines_path(config_dir, kind))
        .await
        .ok()?;
    let trimmed = text.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}
