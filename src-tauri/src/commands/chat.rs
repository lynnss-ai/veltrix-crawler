//! AI 对话命令(对话工作区):会话 CRUD + 发送消息(调大模型)+ 语音转写。
//!
//! 会话归属用户(owner);发消息时按会话绑定的 provider/model 调 OpenAI 兼容 chat。
//! 语音输入复用系统设置的「语音转写」厂商(目前 MiMo ASR),把前端录音转文字回填输入框。

use crate::commands::{current_user, lock_config, AppState};
use chrono::Utc;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, EntityTrait, IntoActiveModel, QueryFilter, QueryOrder,
    QuerySelect, Set,
};
use serde::Serialize;
use serde_json::{json, Value};
use tauri::State;
use veltrix_core::db::entity::{chat_conversation as conv, chat_message as msg};
use veltrix_core::error::{CrawlerError, Result};

/// 单会话最多回放消息数(防超长会话噎住 IPC)。
const MESSAGE_HARD_CAP: u64 = 500;
/// 送入模型的历史消息上限(控制上下文长度与成本,取最近 N 条)。
const CONTEXT_MAX_MESSAGES: usize = 30;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversationView {
    pub id: String,
    pub title: String,
    pub provider_id: String,
    pub model: String,
    pub created_at: i64,
    pub updated_at: i64,
}

impl From<conv::Model> for ConversationView {
    fn from(m: conv::Model) -> Self {
        Self {
            id: m.id,
            title: m.title,
            provider_id: m.provider_id,
            model: m.model,
            created_at: m.created_at,
            updated_at: m.updated_at,
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageView {
    pub id: i64,
    pub conversation_id: String,
    pub role: String,
    pub content: String,
    pub created_at: i64,
}

impl From<msg::Model> for MessageView {
    fn from(m: msg::Model) -> Self {
        Self {
            id: m.id,
            conversation_id: m.conversation_id,
            role: m.role,
            content: m.content,
            created_at: m.created_at,
        }
    }
}

/// 列出当前用户的会话,按最近更新倒序。
#[tauri::command]
pub async fn list_conversations(state: State<'_, AppState>) -> Result<Vec<ConversationView>> {
    let me = current_user(&state).ok_or_else(|| CrawlerError::Config("未登录".into()))?;
    let rows = conv::Entity::find()
        .filter(conv::Column::Owner.eq(me.name))
        .order_by_desc(conv::Column::UpdatedAt)
        .limit(MESSAGE_HARD_CAP)
        .all(&state.db)
        .await
        .map_err(|e| CrawlerError::Config(format!("查询会话失败: {e}")))?;
    Ok(rows.into_iter().map(Into::into).collect())
}

/// 新建会话(空会话,标题占位「新对话」)。绑定所选 provider + model。
#[tauri::command]
pub async fn create_conversation(
    state: State<'_, AppState>,
    id: String,
    provider_id: String,
    model: String,
) -> Result<ConversationView> {
    let me = current_user(&state).ok_or_else(|| CrawlerError::Config("未登录".into()))?;
    let now = Utc::now().timestamp();
    let am = conv::ActiveModel {
        id: Set(id),
        owner: Set(me.name),
        title: Set("新对话".to_string()),
        provider_id: Set(provider_id),
        model: Set(model),
        created_at: Set(now),
        updated_at: Set(now),
    };
    let model = am
        .insert(&state.db)
        .await
        .map_err(|e| CrawlerError::Config(format!("创建会话失败: {e}")))?;
    Ok(model.into())
}

/// 重命名会话。
#[tauri::command]
pub async fn rename_conversation(
    state: State<'_, AppState>,
    id: String,
    title: String,
) -> Result<()> {
    let me = current_user(&state).ok_or_else(|| CrawlerError::Config("未登录".into()))?;
    let model = conv::Entity::find_by_id(id)
        .one(&state.db)
        .await
        .map_err(|e| CrawlerError::Config(format!("查询会话失败: {e}")))?
        .ok_or_else(|| CrawlerError::Config("会话不存在".into()))?;
    if model.owner != me.name {
        return Err(CrawlerError::Config("无权操作该会话".into()));
    }
    let mut am = model.into_active_model();
    am.title = Set(title);
    am.updated_at = Set(Utc::now().timestamp());
    am.update(&state.db)
        .await
        .map_err(|e| CrawlerError::Config(format!("重命名失败: {e}")))?;
    Ok(())
}

/// 删除会话及其全部消息。
#[tauri::command]
pub async fn delete_conversation(state: State<'_, AppState>, id: String) -> Result<()> {
    let me = current_user(&state).ok_or_else(|| CrawlerError::Config("未登录".into()))?;
    let model = conv::Entity::find_by_id(id.clone())
        .one(&state.db)
        .await
        .map_err(|e| CrawlerError::Config(format!("查询会话失败: {e}")))?
        .ok_or_else(|| CrawlerError::Config("会话不存在".into()))?;
    if model.owner != me.name {
        return Err(CrawlerError::Config("无权操作该会话".into()));
    }
    // 先删消息再删会话(逻辑外键,手动顺序)
    msg::Entity::delete_many()
        .filter(msg::Column::ConversationId.eq(&id))
        .exec(&state.db)
        .await
        .map_err(|e| CrawlerError::Config(format!("删除消息失败: {e}")))?;
    conv::Entity::delete_by_id(id)
        .exec(&state.db)
        .await
        .map_err(|e| CrawlerError::Config(format!("删除会话失败: {e}")))?;
    Ok(())
}

/// 列出某会话的消息(时间正序)。
#[tauri::command]
pub async fn list_chat_messages(
    state: State<'_, AppState>,
    conversation_id: String,
) -> Result<Vec<MessageView>> {
    let me = current_user(&state).ok_or_else(|| CrawlerError::Config("未登录".into()))?;
    // 归属校验
    let owner_ok = conv::Entity::find_by_id(conversation_id.clone())
        .one(&state.db)
        .await
        .ok()
        .flatten()
        .map(|c| c.owner == me.name)
        .unwrap_or(false);
    if !owner_ok {
        return Err(CrawlerError::Config("无权查看该会话".into()));
    }
    let rows = msg::Entity::find()
        .filter(msg::Column::ConversationId.eq(conversation_id))
        .order_by_asc(msg::Column::Id)
        .limit(MESSAGE_HARD_CAP)
        .all(&state.db)
        .await
        .map_err(|e| CrawlerError::Config(format!("查询消息失败: {e}")))?;
    Ok(rows.into_iter().map(Into::into).collect())
}

/// 发送一条用户消息并取大模型回复。
/// 落库 user 消息 → 取最近历史拼 messages → 调 chat → 落库 assistant 消息 → 返回 assistant 消息。
/// 首轮对话(此前无消息)用首条内容生成会话标题。
#[tauri::command]
pub async fn send_chat_message(
    state: State<'_, AppState>,
    conversation_id: String,
    content: String,
) -> Result<MessageView> {
    use veltrix_core::db::entity::provider as provider_entity;

    let me = current_user(&state).ok_or_else(|| CrawlerError::Config("未登录".into()))?;
    let text = content.trim().to_string();
    if text.is_empty() {
        return Err(CrawlerError::Config("消息内容为空".into()));
    }

    let conversation = conv::Entity::find_by_id(conversation_id.clone())
        .one(&state.db)
        .await
        .map_err(|e| CrawlerError::Config(format!("查询会话失败: {e}")))?
        .ok_or_else(|| CrawlerError::Config("会话不存在".into()))?;
    if conversation.owner != me.name {
        return Err(CrawlerError::Config("无权操作该会话".into()));
    }

    // 取会话绑定的厂商(api_url/api_key)
    let provider = provider_entity::Entity::find_by_id(conversation.provider_id.clone())
        .one(&state.db)
        .await
        .map_err(|e| CrawlerError::Config(format!("查询厂商失败: {e}")))?
        .ok_or_else(|| CrawlerError::Config("会话绑定的模型厂商不存在,请新建会话".into()))?;
    if provider.api_key.trim().is_empty() {
        return Err(CrawlerError::Config(
            "该模型厂商未配置 API Key,请到系统配置补全".into(),
        ));
    }

    let now = Utc::now().timestamp();

    // 是否首轮(决定要不要自动起标题)
    let had_messages = msg::Entity::find()
        .filter(msg::Column::ConversationId.eq(&conversation_id))
        .one(&state.db)
        .await
        .ok()
        .flatten()
        .is_some();

    // 落库 user 消息
    msg::ActiveModel {
        conversation_id: Set(conversation_id.clone()),
        role: Set("user".to_string()),
        content: Set(text.clone()),
        created_at: Set(now),
        ..Default::default()
    }
    .insert(&state.db)
    .await
    .map_err(|e| CrawlerError::Config(format!("保存消息失败: {e}")))?;

    // 取最近 N 条历史(含刚落库的 user 消息)拼成 OpenAI messages
    let mut history = msg::Entity::find()
        .filter(msg::Column::ConversationId.eq(&conversation_id))
        .order_by_desc(msg::Column::Id)
        .limit(CONTEXT_MAX_MESSAGES as u64)
        .all(&state.db)
        .await
        .map_err(|e| CrawlerError::Config(format!("读取历史失败: {e}")))?;
    history.reverse();
    let messages = json!(history
        .iter()
        .map(|m| json!({ "role": m.role, "content": m.content }))
        .collect::<Vec<_>>());

    // 调大模型(失败时回滚刚插入的 user 消息会更干净,但保留也无碍——用户可重发)
    let reply = crate::llm::chat::chat_completion(crate::llm::chat::ChatRequest {
        api_url: &provider.api_url,
        api_key: &provider.api_key,
        model: &conversation.model,
        messages,
        extra_body: None,
        timeout_secs: crate::llm::http::CHAT_TIMEOUT_SECS,
        retry_server_errors: true,
    })
    .await?;

    // 落库 assistant 消息
    let assistant = msg::ActiveModel {
        conversation_id: Set(conversation_id.clone()),
        role: Set("assistant".to_string()),
        content: Set(reply.clone()),
        created_at: Set(Utc::now().timestamp()),
        ..Default::default()
    }
    .insert(&state.db)
    .await
    .map_err(|e| CrawlerError::Config(format!("保存回复失败: {e}")))?;

    // 更新会话时间;首轮用用户首句作标题(截断)
    let mut am = conversation.into_active_model();
    am.updated_at = Set(Utc::now().timestamp());
    if !had_messages {
        am.title = Set(truncate_title(&text));
    }
    let _ = am.update(&state.db).await;

    Ok(assistant.into())
}

/// 一个上传附件。data 为 base64(无 data url 前缀)。
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatAttachment {
    pub name: String,
    pub mime: String,
    pub data: String,
}

/// 单条消息最多附件数(与前端限制一致)。
const MAX_ATTACHMENTS: usize = 10;
/// 文本类附件并入上下文的最大字符数(避免撑爆上下文)。
const MAX_ATTACH_TEXT_CHARS: usize = 20000;

/// 流式发送:与 send_chat_message 同流程,但用 SSE 流式调模型,支持图片/文本附件(多模态)。
/// 每段增量经 `chat-stream` 事件 emit 给前端(打字机效果),最终落库并返回完整 assistant 消息。
/// 事件 payload:{ conversationId, delta }。
#[tauri::command]
pub async fn send_chat_message_stream(
    state: State<'_, AppState>,
    app: tauri::AppHandle,
    conversation_id: String,
    content: String,
    attachments: Vec<ChatAttachment>,
) -> Result<MessageView> {
    use tauri::Emitter;
    use veltrix_core::db::entity::provider as provider_entity;

    let me = current_user(&state).ok_or_else(|| CrawlerError::Config("未登录".into()))?;
    let text = content.trim().to_string();
    if text.is_empty() && attachments.is_empty() {
        return Err(CrawlerError::Config("消息内容为空".into()));
    }
    if attachments.len() > MAX_ATTACHMENTS {
        return Err(CrawlerError::Config(format!(
            "附件最多 {MAX_ATTACHMENTS} 个"
        )));
    }

    let conversation = conv::Entity::find_by_id(conversation_id.clone())
        .one(&state.db)
        .await
        .map_err(|e| CrawlerError::Config(format!("查询会话失败: {e}")))?
        .ok_or_else(|| CrawlerError::Config("会话不存在".into()))?;
    if conversation.owner != me.name {
        return Err(CrawlerError::Config("无权操作该会话".into()));
    }
    let provider = provider_entity::Entity::find_by_id(conversation.provider_id.clone())
        .one(&state.db)
        .await
        .map_err(|e| CrawlerError::Config(format!("查询厂商失败: {e}")))?
        .ok_or_else(|| CrawlerError::Config("会话绑定的模型厂商不存在,请新建会话".into()))?;
    if provider.api_key.trim().is_empty() {
        return Err(CrawlerError::Config(
            "该模型厂商未配置 API Key,请到系统配置补全".into(),
        ));
    }

    let now = Utc::now().timestamp();

    // 先读历史(本条 user 消息尚未插入),供拼上下文;限最近 N 条
    let mut prior = msg::Entity::find()
        .filter(msg::Column::ConversationId.eq(&conversation_id))
        .order_by_desc(msg::Column::Id)
        .limit(CONTEXT_MAX_MESSAGES as u64)
        .all(&state.db)
        .await
        .map_err(|e| CrawlerError::Config(format!("读取历史失败: {e}")))?;
    prior.reverse();
    let had_messages = !prior.is_empty();

    // DB 存储用文本:正文 + 附件名提示(图片不持久化,历史里以名称展示)
    let stored = if attachments.is_empty() {
        text.clone()
    } else {
        let notes: Vec<String> = attachments
            .iter()
            .map(|a| format!("[附件: {}]", a.name))
            .collect();
        if text.is_empty() {
            notes.join("\n")
        } else {
            format!("{text}\n{}", notes.join("\n"))
        }
    };
    msg::ActiveModel {
        conversation_id: Set(conversation_id.clone()),
        role: Set("user".to_string()),
        content: Set(stored),
        created_at: Set(now),
        ..Default::default()
    }
    .insert(&state.db)
    .await
    .map_err(|e| CrawlerError::Config(format!("保存消息失败: {e}")))?;

    // 当前 user 消息:无附件用纯文本;有附件用多模态 content 数组
    let current_content = build_user_content(&text, &attachments);
    let mut arr: Vec<Value> = prior
        .iter()
        .map(|m| json!({ "role": m.role, "content": m.content }))
        .collect();
    arr.push(json!({ "role": "user", "content": current_content }));
    let messages = json!(arr);

    // 流式调模型:每段增量 emit chat-stream 事件
    let app_emit = app.clone();
    let cid_emit = conversation_id.clone();
    let reply = crate::llm::chat::chat_completion_stream(
        crate::llm::chat::ChatRequest {
            api_url: &provider.api_url,
            api_key: &provider.api_key,
            model: &conversation.model,
            messages,
            extra_body: None,
            timeout_secs: crate::llm::http::CHAT_TIMEOUT_SECS,
            retry_server_errors: false,
        },
        move |delta| {
            let _ = app_emit.emit(
                "chat-stream",
                json!({ "conversationId": cid_emit, "delta": delta }),
            );
        },
    )
    .await?;

    let assistant = msg::ActiveModel {
        conversation_id: Set(conversation_id.clone()),
        role: Set("assistant".to_string()),
        content: Set(reply.clone()),
        created_at: Set(Utc::now().timestamp()),
        ..Default::default()
    }
    .insert(&state.db)
    .await
    .map_err(|e| CrawlerError::Config(format!("保存回复失败: {e}")))?;

    let mut am = conversation.into_active_model();
    am.updated_at = Set(Utc::now().timestamp());
    if !had_messages {
        am.title = Set(truncate_title(&text));
    }
    let _ = am.update(&state.db).await;

    Ok(assistant.into())
}

/// 构造当前 user 消息的 content:无附件返回纯文本字符串;有附件返回多模态数组。
/// 图片 → image_url(data url);文本类 → 解码内容内联(截断);其余 → 文件名提示。
fn build_user_content(text: &str, attachments: &[ChatAttachment]) -> Value {
    use base64::Engine;
    if attachments.is_empty() {
        return json!(text);
    }
    let mut parts: Vec<Value> = Vec::new();
    if !text.is_empty() {
        parts.push(json!({ "type": "text", "text": text }));
    }
    for a in attachments {
        if a.mime.starts_with("image/") {
            parts.push(json!({
                "type": "image_url",
                "image_url": { "url": format!("data:{};base64,{}", a.mime, a.data) }
            }));
        } else if a.mime.starts_with("text/") || is_text_name(&a.name) {
            // 文本类附件:解码并内联(截断),让模型读到内容
            let decoded = base64::engine::general_purpose::STANDARD
                .decode(a.data.as_bytes())
                .ok()
                .and_then(|b| String::from_utf8(b).ok());
            match decoded {
                Some(t) => {
                    let body: String = t.chars().take(MAX_ATTACH_TEXT_CHARS).collect();
                    parts.push(json!({
                        "type": "text",
                        "text": format!("附件「{}」内容:\n{}", a.name, body)
                    }));
                }
                None => parts.push(json!({
                    "type": "text",
                    "text": format!("[附件: {}(无法解析为文本)]", a.name)
                })),
            }
        } else {
            parts.push(json!({
                "type": "text",
                "text": format!("[附件: {}(类型 {},未解析)]", a.name, a.mime)
            }));
        }
    }
    json!(parts)
}

/// 按扩展名粗判是否文本类文件(mime 缺失时兜底)。
fn is_text_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    [
        ".txt", ".md", ".markdown", ".csv", ".json", ".log", ".yml", ".yaml", ".xml", ".html",
        ".css", ".js", ".ts", ".tsx", ".jsx", ".py", ".rs", ".go", ".java", ".c", ".cpp", ".h",
        ".sh", ".sql", ".toml", ".ini",
    ]
    .iter()
    .any(|ext| lower.ends_with(ext))
}

/// 用首条用户消息生成标题:取前 24 个字符,去换行。
fn truncate_title(text: &str) -> String {
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

/// 语音输入:把前端录制的音频(base64)转写为文字回填输入框。
/// 复用系统设置的「语音转写」厂商配置(目前 MiMo ASR)。
#[tauri::command]
pub async fn transcribe_chat_audio(
    state: State<'_, AppState>,
    audio_base64: String,
    format: String,
) -> Result<String> {
    use base64::Engine;
    use veltrix_core::db::entity::provider as provider_entity;

    let transcription_cfg = { lock_config(&state)?.transcription.clone() };
    if transcription_cfg.provider_id.trim().is_empty() || transcription_cfg.model.trim().is_empty() {
        return Err(CrawlerError::Config(
            "未配置语音转写,请到系统配置 → 语音转写选择厂商与模型".into(),
        ));
    }
    let provider = provider_entity::Entity::find_by_id(transcription_cfg.provider_id.clone())
        .one(&state.db)
        .await
        .map_err(|e| CrawlerError::Config(format!("查询厂商失败: {e}")))?
        .ok_or_else(|| CrawlerError::Config("语音转写厂商不存在".into()))?;

    // 解码音频写入临时文件(转写实现按文件路径读取)
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(audio_base64.as_bytes())
        .map_err(|e| CrawlerError::Config(format!("音频解码失败: {e}")))?;
    let ext = if format.trim().is_empty() {
        "webm".to_string()
    } else {
        format.trim().to_ascii_lowercase()
    };
    let tmp = std::env::temp_dir().join(format!(
        "veltrix-voice-{}.{ext}",
        Utc::now().timestamp_millis()
    ));
    tokio::fs::write(&tmp, &bytes)
        .await
        .map_err(|e| CrawlerError::Config(format!("写临时音频失败: {e}")))?;

    let result = crate::llm::transcribe(crate::llm::TranscribeRequest {
        provider_code: &provider.code,
        api_url: &provider.api_url,
        api_key: &provider.api_key,
        model: &transcription_cfg.model,
        audio_path: &tmp,
    })
    .await;
    // 转写完删临时文件(失败忽略)
    let _ = tokio::fs::remove_file(&tmp).await;
    result
}
