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

// ===== 长会话上下文策略:live 原文窗口 + 滚动摘要 =====
// 发送给模型的 = [用户全局记忆] + [会话滚动摘要] + [live 原文消息] + [本次提问]。
// live 原文 = id 大于 summarized_upto_id 的消息;更早的被压缩进会话 summary,聊多久都不丢前文。
// 阈值常量与摘要折叠 / 注入实现已上移到 conversation_summary 模块,与 coding 共用。
use crate::agent::core::shared::{MessageAttachmentView, MessageView};
use crate::agent::core::summary::{
    maintain_conversation_summary, summary_system_message, LIVE_HARD_CAP, MAX_SUMMARY_CHARS,
};

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversationView {
    pub id: String,
    pub title: String,
    pub provider_id: String,
    pub model: String,
    /// 场景类型:chat / coding / rpa(决定前端页面布局)
    pub agent_type: String,
    pub created_at: i64,
    pub updated_at: i64,
    /// 是否归档(归档会话不在「最近对话」与对话页展示,仅对话记录页可见)
    pub archived: bool,
    /// 编程 Agent 的分步计划(JSON 数组 `[{title,done}]` 字符串;空串=无计划)。前端解析渲染 todo 进度。
    pub plan_todos: String,
}

impl From<conv::Model> for ConversationView {
    fn from(m: conv::Model) -> Self {
        Self {
            id: m.id,
            title: m.title,
            provider_id: m.provider_id,
            model: m.model,
            agent_type: m.agent_type,
            created_at: m.created_at,
            updated_at: m.updated_at,
            archived: m.archived,
            plan_todos: m.plan_todos,
        }
    }
}

// 消息视图(MessageView / MessageAttachmentView)已上移到 `crate::agent::core::shared`(见顶部 use)。

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
    // 场景类型(chat / coding / rpa);前端不传时默认 chat
    agent_type: Option<String>,
) -> Result<ConversationView> {
    let me = current_user(&state).ok_or_else(|| CrawlerError::Config("未登录".into()))?;
    let now = Utc::now().timestamp();
    let am = conv::ActiveModel {
        id: Set(id),
        owner: Set(me.name),
        title: Set("新对话".to_string()),
        provider_id: Set(provider_id),
        model: Set(model),
        agent_type: Set(agent_type.unwrap_or_else(|| "chat".to_string())),
        summary: Set(String::new()),
        summarized_upto_id: Set(0),
        created_at: Set(now),
        updated_at: Set(now),
        archived: Set(false),
        plan_todos: Set(String::new()),
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

/// 归档 / 取消归档会话。归档后从「最近对话」与对话页隐藏,仍可在对话记录页查看与恢复。
/// 不改 updated_at,保持原有时间排序。
#[tauri::command]
pub async fn archive_conversation(
    state: State<'_, AppState>,
    id: String,
    archived: bool,
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
    am.archived = Set(archived);
    am.update(&state.db)
        .await
        .map_err(|e| CrawlerError::Config(format!("归档失败: {e}")))?;
    Ok(())
}

/// 切换会话绑定的模型厂商 + 模型。后续发送会按新模型调用(send 时实时读会话 model)。
#[tauri::command]
pub async fn update_conversation_model(
    state: State<'_, AppState>,
    id: String,
    provider_id: String,
    model: String,
) -> Result<ConversationView> {
    let me = current_user(&state).ok_or_else(|| CrawlerError::Config("未登录".into()))?;
    if provider_id.trim().is_empty() || model.trim().is_empty() {
        return Err(CrawlerError::Config("模型厂商或模型不能为空".into()));
    }
    let conversation = conv::Entity::find_by_id(id)
        .one(&state.db)
        .await
        .map_err(|e| CrawlerError::Config(format!("查询会话失败: {e}")))?
        .ok_or_else(|| CrawlerError::Config("会话不存在".into()))?;
    if conversation.owner != me.name {
        return Err(CrawlerError::Config("无权操作该会话".into()));
    }
    let mut am = conversation.into_active_model();
    am.provider_id = Set(provider_id);
    am.model = Set(model);
    am.updated_at = Set(Utc::now().timestamp());
    let updated = am
        .update(&state.db)
        .await
        .map_err(|e| CrawlerError::Config(format!("切换模型失败: {e}")))?;
    Ok(updated.into())
}

/// 读取某会话的滚动摘要(供对话页「本会话记忆」查看)。归属校验。
#[tauri::command]
pub async fn get_conversation_summary(
    state: State<'_, AppState>,
    conversation_id: String,
) -> Result<String> {
    let me = current_user(&state).ok_or_else(|| CrawlerError::Config("未登录".into()))?;
    let conversation = conv::Entity::find_by_id(conversation_id)
        .one(&state.db)
        .await
        .map_err(|e| CrawlerError::Config(format!("查询会话失败: {e}")))?
        .ok_or_else(|| CrawlerError::Config("会话不存在".into()))?;
    if conversation.owner != me.name {
        return Err(CrawlerError::Config("无权查看该会话".into()));
    }
    Ok(conversation.summary)
}

/// 手动更新某会话的滚动摘要(「本会话记忆」编辑后保存)。归属校验,截断到上限。
/// 只改摘要文本,不动 summarized_upto_id(后续维护会在此基础上继续合并)。
#[tauri::command]
pub async fn update_conversation_summary(
    state: State<'_, AppState>,
    conversation_id: String,
    summary: String,
) -> Result<()> {
    let me = current_user(&state).ok_or_else(|| CrawlerError::Config("未登录".into()))?;
    let conversation = conv::Entity::find_by_id(conversation_id)
        .one(&state.db)
        .await
        .map_err(|e| CrawlerError::Config(format!("查询会话失败: {e}")))?
        .ok_or_else(|| CrawlerError::Config("会话不存在".into()))?;
    if conversation.owner != me.name {
        return Err(CrawlerError::Config("无权操作该会话".into()));
    }
    let trimmed: String = summary.trim().chars().take(MAX_SUMMARY_CHARS).collect();
    let mut am = conversation.into_active_model();
    am.summary = Set(trimmed);
    am.update(&state.db)
        .await
        .map_err(|e| CrawlerError::Config(format!("保存会话记忆失败: {e}")))?;
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
    // 顺手清理该会话落盘的图片附件目录(best-effort,失败忽略,不阻断删除)
    let att_dir = state
        .config_dir
        .join("chat-attachments")
        .join(safe_dir_name(&id));
    let _ = tokio::fs::remove_dir_all(&att_dir).await;
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

    // 取 live 原文(id 大于已折叠进摘要的边界,含刚落库的 user 消息),更早的由会话摘要承载
    let mut history = msg::Entity::find()
        .filter(msg::Column::ConversationId.eq(&conversation_id))
        .filter(msg::Column::Id.gt(conversation.summarized_upto_id))
        .order_by_desc(msg::Column::Id)
        .limit(LIVE_HARD_CAP)
        .all(&state.db)
        .await
        .map_err(|e| CrawlerError::Config(format!("读取历史失败: {e}")))?;
    history.reverse();
    let mut arr: Vec<Value> = Vec::with_capacity(history.len() + 2);
    // 跨会话长期记忆:按本轮提问语义检索 top-K,作为 system 消息注入到最前面(关闭 / 无记忆时为 None)
    if let Some(sys) =
        crate::agent::chat::memory::memory_system_message(&state.db, &me.name, &text).await
    {
        arr.push(sys);
    }
    // 本会话滚动摘要:早期消息压缩后的前情提要,注入在原文之前
    if let Some(sys) = summary_system_message(&conversation.summary) {
        arr.push(sys);
    }
    // 历史里带图片的 user 消息重建为多模态,模型后续轮次仍"看得到"图(纯文本消息原样透传)
    for m in &history {
        arr.push(json!({ "role": m.role, "content": history_content(m).await }));
    }
    let messages = json!(arr);

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

    // 会话模型作为杂活角色化解析的回退档
    let fallback_ref = session_provider_ref(&provider, &conversation.model);

    // 自动记忆提取:从本轮对话抽取值得长期记住的事实,异步落库,不阻塞回复返回
    spawn_memory_extraction(&state.db, &me.name, fallback_ref.clone(), &text, &reply);

    // 滚动摘要维护:live 过长时把较旧消息折叠进会话摘要,异步进行,不阻塞回复返回
    spawn_summary_maintenance(&state.db, &conversation_id, fallback_ref.clone());

    // 更新会话时间;首轮用用户首句作标题(截断)
    // 首轮:用 AI 概括对话生成标题(摘要角色的便宜模型;失败回退用户首句截断)
    let new_title = if had_messages {
        None
    } else {
        let title_ref =
            crate::commands::resolve_role_provider(&state.db, crate::llm::AgentRole::Summary, fallback_ref)
                .await;
        Some(
            generate_title(
                &title_ref.api_url,
                &title_ref.api_key,
                &title_ref.model,
                &text,
                &reply,
            )
            .await
            .unwrap_or_else(|| truncate_title(&text)),
        )
    };
    let mut am = conversation.into_active_model();
    am.updated_at = Set(Utc::now().timestamp());
    if let Some(t) = new_title {
        am.title = Set(t);
    }
    let _ = am.update(&state.db).await;

    Ok(assistant.into())
}

/// 一个上传附件。data 为 base64(无 data url 前缀)。
/// 既作发送入参(前端→后端),也作「资产素材→附件」命令的出参(后端→前端)。
#[derive(serde::Deserialize, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ChatAttachment {
    pub name: String,
    pub mime: String,
    pub data: String,
}

/// 单条消息最多附件数(与前端限制一致)。
const MAX_ATTACHMENTS: usize = 12;
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

    // 先读 live 原文(本条 user 消息尚未插入):id 大于已折叠进摘要的边界,更早的由会话摘要承载
    let mut prior = msg::Entity::find()
        .filter(msg::Column::ConversationId.eq(&conversation_id))
        .filter(msg::Column::Id.gt(conversation.summarized_upto_id))
        .order_by_desc(msg::Column::Id)
        .limit(LIVE_HARD_CAP)
        .all(&state.db)
        .await
        .map_err(|e| CrawlerError::Config(format!("读取历史失败: {e}")))?;
    prior.reverse();
    // 是否首轮:用整会话存在性查询,不能用 prior(它已被 summarized_upto_id 过滤,长会话折叠后
    // prior 可能近空,会把非首轮误判为首轮、错误地用首句重起标题)。与非流式 send_chat_message 一致。
    let had_messages = msg::Entity::find()
        .filter(msg::Column::ConversationId.eq(&conversation_id))
        .one(&state.db)
        .await
        .ok()
        .flatten()
        .is_some();

    // 附件落盘并产出元数据 JSON:图片写入本地附件目录(供历史渲染 + 多轮重建),content 只存正文。
    // 旧口径把 [附件:名] 塞进 content,导致历史只见文字占位、图片丢失;现改为图片单列存储。
    let attachments_json =
        persist_attachments(&state.config_dir, &conversation_id, &attachments).await;
    msg::ActiveModel {
        conversation_id: Set(conversation_id.clone()),
        role: Set("user".to_string()),
        content: Set(text.clone()),
        attachments: Set(attachments_json),
        created_at: Set(now),
        ..Default::default()
    }
    .insert(&state.db)
    .await
    .map_err(|e| CrawlerError::Config(format!("保存消息失败: {e}")))?;

    // 当前 user 消息:无附件用纯文本;有附件用多模态 content 数组
    let current_content = build_user_content(&text, &attachments);
    let mut arr: Vec<Value> = Vec::with_capacity(prior.len() + 3);
    // 跨会话长期记忆:按本轮提问语义检索 top-K,作为 system 消息注入到最前面(关闭 / 无记忆时为 None)
    if let Some(sys) =
        crate::agent::chat::memory::memory_system_message(&state.db, &me.name, &text).await
    {
        arr.push(sys);
    }
    // 本会话滚动摘要:早期消息压缩后的前情提要,注入在原文之前
    if let Some(sys) = summary_system_message(&conversation.summary) {
        arr.push(sys);
    }
    // 历史里带图片的 user 消息重建为多模态(与非流式一致),让模型后续轮次仍"看得到"图
    for m in &prior {
        arr.push(json!({ "role": m.role, "content": history_content(m).await }));
    }
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
        move |kind, delta| {
            // kind 区分正文(content)与思考过程(reasoning),前端分别渲染
            let _ = app_emit.emit(
                "chat-stream",
                json!({ "conversationId": cid_emit, "kind": kind, "delta": delta }),
            );
        },
    )
    .await?;
    let reply_text = reply.content;
    let reply_reasoning = reply.reasoning;

    let assistant = msg::ActiveModel {
        conversation_id: Set(conversation_id.clone()),
        role: Set("assistant".to_string()),
        content: Set(reply_text.clone()),
        reasoning: Set(reply_reasoning),
        created_at: Set(Utc::now().timestamp()),
        ..Default::default()
    }
    .insert(&state.db)
    .await
    .map_err(|e| CrawlerError::Config(format!("保存回复失败: {e}")))?;

    // 会话模型作为杂活角色化解析的回退档
    let fallback_ref = session_provider_ref(&provider, &conversation.model);

    // 自动记忆提取:从本轮对话抽取值得长期记住的事实,异步落库,不阻塞回复返回
    spawn_memory_extraction(&state.db, &me.name, fallback_ref.clone(), &text, &reply_text);

    // 滚动摘要维护:live 过长时把较旧消息折叠进会话摘要,异步进行,不阻塞回复返回
    spawn_summary_maintenance(&state.db, &conversation_id, fallback_ref.clone());

    // 首轮:用 AI 概括对话生成标题(摘要角色的便宜模型;失败回退用户首句截断)
    let new_title = if had_messages {
        None
    } else {
        let title_ref =
            crate::commands::resolve_role_provider(&state.db, crate::llm::AgentRole::Summary, fallback_ref)
                .await;
        Some(
            generate_title(
                &title_ref.api_url,
                &title_ref.api_key,
                &title_ref.model,
                &text,
                &reply_text,
            )
            .await
            .unwrap_or_else(|| truncate_title(&text)),
        )
    };
    let mut am = conversation.into_active_model();
    am.updated_at = Set(Utc::now().timestamp());
    if let Some(t) = new_title {
        am.title = Set(t);
    }
    let _ = am.update(&state.db).await;

    Ok(assistant.into())
}

/// 把录制好的视频以附件方式作为一条 user 消息加入对话(引用本地路径,不重新上传 base64)。
/// 录屏停止后前端按当前会话调用;返回新消息视图供前端追加渲染(内联播放器)。
#[tauri::command]
pub async fn attach_recording_message(
    state: State<'_, AppState>,
    conversation_id: String,
    path: String,
) -> Result<MessageView> {
    let me = current_user(&state).ok_or_else(|| CrawlerError::Config("未登录".into()))?;
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return Err(CrawlerError::Config("录制文件路径为空".into()));
    }
    let conversation = conv::Entity::find_by_id(conversation_id.clone())
        .one(&state.db)
        .await
        .map_err(|e| CrawlerError::Config(format!("查询会话失败: {e}")))?
        .ok_or_else(|| CrawlerError::Config("会话不存在".into()))?;
    if conversation.owner != me.name {
        return Err(CrawlerError::Config("无权操作该会话".into()));
    }
    let name = std::path::Path::new(trimmed)
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "recording.mp4".to_string());
    // 附件 JSON 与 persist_attachments 同口径:[{name,mime,path}](path 为本地绝对路径,走 asset 协议播放)
    let attachments_json =
        json!([{ "name": name, "mime": "video/mp4", "path": trimmed }]).to_string();
    let now = Utc::now().timestamp();
    let saved = msg::ActiveModel {
        conversation_id: Set(conversation_id.clone()),
        role: Set("user".to_string()),
        content: Set(String::new()),
        attachments: Set(Some(attachments_json)),
        created_at: Set(now),
        ..Default::default()
    }
    .insert(&state.db)
    .await
    .map_err(|e| CrawlerError::Config(format!("保存录制消息失败: {e}")))?;
    // 抬升会话 updated_at,让其在侧栏置顶
    let mut am = conversation.into_active_model();
    am.updated_at = Set(now);
    let _ = am.update(&state.db).await;
    Ok(saved.into())
}

/// 把本轮对话的记忆提取放到后台 spawn 执行,避免阻塞回复返回。
/// 入参均 clone 进任务,绕开生命周期约束(provider/conversation 随后会被消费)。
/// 记忆提取属杂活,优先走 Summary 角色单独配置的便宜模型;未配置则回退会话模型(fallback)。
fn spawn_memory_extraction(
    db: &sea_orm::DatabaseConnection,
    owner: &str,
    fallback: crate::agent::core::ProviderRef,
    user_text: &str,
    assistant_text: &str,
) {
    let db = db.clone();
    let owner = owner.to_string();
    let user_text = user_text.to_string();
    let assistant_text = assistant_text.to_string();
    tauri::async_runtime::spawn(async move {
        let p = crate::commands::resolve_role_provider(
            &db,
            crate::llm::AgentRole::Summary,
            fallback,
        )
        .await;
        crate::agent::chat::memory::extract_and_store_memories(
            &db,
            &owner,
            &p.api_url,
            &p.api_key,
            &p.model,
            &user_text,
            &assistant_text,
        )
        .await;
    });
}

/// 用会话绑定的厂商 + 模型构造一个 ProviderRef,作为角色化解析的回退档(杂活复用)。
fn session_provider_ref(
    provider: &veltrix_core::db::entity::provider::Model,
    model: &str,
) -> crate::agent::core::ProviderRef {
    crate::agent::core::ProviderRef {
        kind: crate::agent::core::ProviderKind::from_code(&provider.code),
        api_url: provider.api_url.clone(),
        api_key: provider.api_key.clone(),
        model: model.to_string(),
    }
}

/// 把会话摘要维护放到后台 spawn 执行,避免阻塞回复返回。
/// 摘要属杂活,优先走 Summary 角色单独配置的便宜模型;未配置则回退会话模型(fallback)。
fn spawn_summary_maintenance(
    db: &sea_orm::DatabaseConnection,
    conversation_id: &str,
    fallback: crate::agent::core::ProviderRef,
) {
    let db = db.clone();
    let conversation_id = conversation_id.to_string();
    tauri::async_runtime::spawn(async move {
        let p = crate::commands::resolve_role_provider(
            &db,
            crate::llm::AgentRole::Summary,
            fallback,
        )
        .await;
        // chat 通用对话:无场景化额外保留要求,extra_hint 传空串
        maintain_conversation_summary(&db, &conversation_id, &p.api_url, &p.api_key, &p.model, "")
            .await;
    });
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

/// 把本条用户消息的附件落盘并产出元数据 JSON(供历史渲染 + 多轮重建)。
/// 图片:解码 base64 写入 `{config_dir}/chat-attachments/{会话id}/{毫秒}-{序号}.{ext}`,记绝对 path;
/// 非图片:不落盘(其内容已在本轮内联给模型),仅记 name/mime 供历史以文件 chip 展示。
/// 无附件返回 None(attachments 列存 NULL)。
async fn persist_attachments(
    config_dir: &std::path::Path,
    conversation_id: &str,
    attachments: &[ChatAttachment],
) -> Option<String> {
    if attachments.is_empty() {
        return None;
    }
    let dir = config_dir
        .join("chat-attachments")
        .join(safe_dir_name(conversation_id));
    let stamp = Utc::now().timestamp_millis();
    let mut metas: Vec<Value> = Vec::with_capacity(attachments.len());
    for (idx, a) in attachments.iter().enumerate() {
        let path = if a.mime.starts_with("image/") {
            // 图片落盘;失败降级为无 path(历史仍展示文件名,不阻断发送)
            match write_image_file(&dir, stamp, idx, a).await {
                Ok(p) => p,
                Err(e) => {
                    tracing::warn!("聊天图片落盘失败({}): {e}", a.name);
                    String::new()
                }
            }
        } else {
            String::new()
        };
        metas.push(json!({ "name": a.name, "mime": a.mime, "path": path }));
    }
    serde_json::to_string(&metas).ok()
}

/// 解码图片 base64 写入附件目录,返回绝对路径字符串。
async fn write_image_file(
    dir: &std::path::Path,
    stamp: i64,
    idx: usize,
    a: &ChatAttachment,
) -> Result<String> {
    use base64::Engine;
    tokio::fs::create_dir_all(dir)
        .await
        .map_err(|e| CrawlerError::Config(format!("创建附件目录失败: {e}")))?;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(a.data.as_bytes())
        .map_err(|e| CrawlerError::Config(format!("图片解码失败: {e}")))?;
    let full = dir.join(format!("{stamp}-{idx}.{}", image_ext(&a.mime, &a.name)));
    tokio::fs::write(&full, &bytes)
        .await
        .map_err(|e| CrawlerError::Config(format!("写图片失败: {e}")))?;
    Ok(full.to_string_lossy().into_owned())
}

/// 图片落盘扩展名:优先按 mime,回退文件名扩展名,再回退 jpg。
fn image_ext(mime: &str, name: &str) -> String {
    match mime {
        "image/png" => return "png".to_string(),
        "image/jpeg" => return "jpg".to_string(),
        "image/webp" => return "webp".to_string(),
        "image/gif" => return "gif".to_string(),
        _ => {}
    }
    std::path::Path::new(name)
        .extension()
        .and_then(|s| s.to_str())
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_else(|| "jpg".to_string())
}

/// 会话 id 作目录名的安全化:非字母数字 / - / _ 一律替换为 _(会话 id 通常为 UUID 已安全,防御性处理)。
fn safe_dir_name(id: &str) -> String {
    id.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// 构造历史消息发给模型的 content:带图片的 user 消息重建为多模态数组(正文 + 各图 image_url,
/// 图片从落盘 path 读回 base64);否则原样返回纯文本 content。读图失败降级为文字占位,不中断。
async fn history_content(m: &msg::Model) -> Value {
    use base64::Engine;
    let metas: Vec<MessageAttachmentView> = m
        .attachments
        .as_deref()
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or_default();
    let images: Vec<&MessageAttachmentView> = metas
        .iter()
        .filter(|a| a.mime.starts_with("image/") && !a.path.is_empty())
        .collect();
    if m.role != "user" || images.is_empty() {
        return json!(m.content);
    }
    let mut parts: Vec<Value> = Vec::new();
    if !m.content.is_empty() {
        parts.push(json!({ "type": "text", "text": m.content }));
    }
    for a in images {
        match tokio::fs::read(&a.path).await {
            Ok(bytes) => {
                let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
                parts.push(json!({
                    "type": "image_url",
                    "image_url": { "url": format!("data:{};base64,{}", a.mime, b64) }
                }));
            }
            Err(e) => {
                tracing::warn!("重建历史图片失败({}): {e}", a.path);
                parts.push(json!({
                    "type": "text",
                    "text": format!("[图片: {}(本地已丢失)]", a.name)
                }));
            }
        }
    }
    json!(parts)
}

/// 用 AI 概括一轮对话生成简短标题(≤16 字,无标点/引号)。失败返回 None 由上层回退。
async fn generate_title(
    api_url: &str,
    api_key: &str,
    model: &str,
    user_text: &str,
    assistant_text: &str,
) -> Option<String> {
    if api_url.trim().is_empty() || api_key.trim().is_empty() {
        return None;
    }
    let user: String = user_text.chars().take(500).collect();
    let assistant: String = assistant_text.chars().take(500).collect();
    let prompt = format!(
        "请用不超过 16 个字的简短标题概括下面这轮对话,只输出标题本身,不要标点、引号或解释。\n\n用户:{user}\n助手:{assistant}"
    );
    let reply = crate::llm::chat::chat_completion(crate::llm::chat::ChatRequest {
        api_url,
        api_key,
        model,
        messages: json!([{ "role": "user", "content": prompt }]),
        extra_body: None,
        timeout_secs: crate::llm::http::CHAT_TIMEOUT_SECS,
        retry_server_errors: false,
    })
    .await
    .ok()?;
    // 清洗:去引号/书名号/换行,截断到 20 字
    let cleaned: String = reply
        .trim()
        .trim_matches(|c| "\"'「」《》【】 \t".contains(c))
        .replace(['\n', '\r'], " ");
    let title: String = cleaned.trim().chars().take(20).collect();
    if title.is_empty() {
        None
    } else {
        Some(title)
    }
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

    let transcription_cfg = { lock_config(&state)?.transcription.clone() };
    // 地址/模型已有 MiMo 默认值,真正必填的是 API Key
    let api_key = crate::commands::get_secret(&state.db, "transcription_api_key").await;
    if api_key.trim().is_empty() {
        return Err(CrawlerError::Config(
            "未配置语音转写 API Key,请到系统设置 → 语音转写填写 API Key".into(),
        ));
    }

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
        provider_code: "mimo",
        api_url: &transcription_cfg.api_url,
        api_key: &api_key,
        model: &transcription_cfg.model,
        audio_path: &tmp,
    })
    .await;
    // 转写完删临时文件(失败忽略)
    let _ = tokio::fs::remove_file(&tmp).await;
    result
}

/// 单条资产引入的图片附件上限(与单条消息附件上限一致)。
const ASSET_IMAGE_CAP: usize = MAX_ATTACHMENTS;

/// 把某条已采集内容(资产)的本地视觉素材读成 base64 聊天附件。
/// 仅用「已下载到本地」的文件。`cover_only=true`(图源=封面)只取封面;否则(图源=图文)
/// 优先图文图片(封面同目录 `{prefix}_img{idx}.jpg`,命名见 media::process_content),
/// 无图片再退回封面。`indices` 给出时仅取这些「本地图片排序后的位置」(逐张挑选用),
/// 与不带 indices 的整相册调用顺序一致,避免部分下载导致的错位。都没有则报错。
#[tauri::command]
pub async fn build_content_attachments(
    state: State<'_, AppState>,
    content_id: String,
    cover_only: Option<bool>,
    indices: Option<Vec<i32>>,
) -> Result<Vec<ChatAttachment>> {
    use veltrix_core::db::entity::content as content_entity;

    let me = current_user(&state).ok_or_else(|| CrawlerError::Config("未登录".into()))?;
    let row = content_entity::Entity::find_by_id(content_id)
        .one(&state.db)
        .await
        .map_err(|e| CrawlerError::Config(format!("查询内容失败: {e}")))?
        .ok_or_else(|| CrawlerError::Config("内容不存在".into()))?;
    // 归属校验:self 用户只能引用自己的资产
    if me.scope == "self" && row.owner != me.name {
        return Err(CrawlerError::Config("无权引用该内容".into()));
    }

    // 封面图源只取封面;否则图文图片优先、无则退回封面
    let mut paths: Vec<String> = Vec::new();
    if !cover_only.unwrap_or(false) {
        paths = local_image_paths(&row);
        // 逐张挑选:仅保留指定位置(基于上面已排序的本地图片列表)
        if let Some(want) = indices.as_ref() {
            let want: std::collections::HashSet<usize> =
                want.iter().filter(|i| **i >= 0).map(|i| *i as usize).collect();
            paths = paths
                .into_iter()
                .enumerate()
                .filter(|(pos, _)| want.contains(pos))
                .map(|(_, p)| p)
                .collect();
        }
    }
    if paths.is_empty() {
        if let Some(cover) = row.cover_path.as_deref().filter(|s| !s.is_empty()) {
            paths.push(cover.to_string());
        }
    }
    if paths.is_empty() {
        return Err(CrawlerError::Config(
            "该内容暂无已下载到本地的图片/封面,请先到全量库下载素材".into(),
        ));
    }

    let mut atts: Vec<ChatAttachment> = Vec::with_capacity(paths.len());
    for p in paths.into_iter().take(ASSET_IMAGE_CAP) {
        match read_file_as_attachment(&p).await {
            Ok(a) => atts.push(a),
            // 单张读失败不阻断其余(文件被删等),记告警继续
            Err(e) => tracing::warn!("读取资产素材失败({p}): {e}"),
        }
    }
    if atts.is_empty() {
        return Err(CrawlerError::Config("素材读取失败".into()));
    }
    Ok(atts)
}

/// 读本地文件为聊天附件(base64);文件名取路径末段,mime 按扩展名粗判。
async fn read_file_as_attachment(path: &str) -> Result<ChatAttachment> {
    use base64::Engine;
    let bytes = tokio::fs::read(path)
        .await
        .map_err(|e| CrawlerError::Config(format!("读取本地素材失败: {e}")))?;
    let name = std::path::Path::new(path)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "asset".to_string());
    Ok(ChatAttachment {
        name,
        mime: mime_from_ext(path),
        data: base64::engine::general_purpose::STANDARD.encode(&bytes),
    })
}

/// 按扩展名粗判图片 mime;封面/图片均以 .jpg 落地,默认 image/jpeg。
fn mime_from_ext(path: &str) -> String {
    let lower = path.to_ascii_lowercase();
    if lower.ends_with(".png") {
        "image/png"
    } else if lower.ends_with(".webp") {
        "image/webp"
    } else if lower.ends_with(".gif") {
        "image/gif"
    } else {
        "image/jpeg"
    }
    .to_string()
}

/// 推断某内容已下载到本地的图片绝对路径(按 idx 升序)。
/// 图片与封面同目录,故从 cover_path 拆出目录与 `{prefix}_cover.jpg` 前缀,扫同目录 `{prefix}_img*`。
/// 无封面路径则无从定位(下载当天日期未落库),返回空让上层提示去下载。
fn local_image_paths(row: &veltrix_core::db::entity::content::Model) -> Vec<String> {
    let Some(cover) = row.cover_path.as_deref().filter(|s| !s.is_empty()) else {
        return Vec::new();
    };
    let cover_path = std::path::Path::new(cover);
    let (Some(dir), Some(file_name)) = (
        cover_path.parent(),
        cover_path.file_name().and_then(|n| n.to_str()),
    ) else {
        return Vec::new();
    };
    let Some(prefix) = file_name.strip_suffix("_cover.jpg") else {
        return Vec::new();
    };

    let img_prefix = format!("{prefix}_img");
    let read_dir = match std::fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(_) => return Vec::new(),
    };
    let mut files: Vec<(u32, String)> = Vec::new();
    for entry in read_dir.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if let Some(rest) = name.strip_prefix(&img_prefix) {
            // rest 形如 "{idx}.jpg" → 解析 idx 做数值排序(>=10 张也不乱序)
            let idx = rest
                .split('.')
                .next()
                .and_then(|s| s.parse::<u32>().ok())
                .unwrap_or(u32::MAX);
            files.push((idx, entry.path().to_string_lossy().into_owned()));
        }
    }
    files.sort_by_key(|(idx, _)| *idx);
    files.into_iter().map(|(_, p)| p).collect()
}
