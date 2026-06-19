//! 🖥️ 电脑操作 Agent 命令:send_computer_message(ReAct 循环 + 工具往返落库 + 进度事件 + 截图回灌)。
//!
//! 骨架与 `agent::rpa::commands::send_browser_message` 同构(逐步落库 chat_messages、emit `agent-step`、
//! MAX_ITERS 防失控、capture_screen 截图作 UserWithImages 回灌视觉模型),区别在工具集来自
//! `agent::computer::tools`(聚合 desktop/fs/system/ocr/uia/net/shell 的全部工具)、系统提示词为电脑操作版。
//! 危险工具(is_dangerous)目前先 emit 提示,完整「暂停确认」握手待接入。

use crate::agent::computer::tools as computer;
use crate::agent::core::shared::{
    row_to_chat_msg, tool_calls_to_json, truncate_title, MessageView, MAX_ITERS,
};
use crate::agent::core::summary as conv_summary;
use crate::agent::core::{
    provider_for, ChatMsg, LlmOptions, LlmRequest, ProviderKind, ProviderRef,
};
use crate::commands::{current_user, AppState};
use chrono::Utc;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, EntityTrait, IntoActiveModel, QueryFilter, QueryOrder,
    QuerySelect, Set,
};
use serde_json::json;
use tauri::{AppHandle, Emitter, State};
use veltrix_core::db::entity::{
    chat_conversation as conv, chat_message as msg, provider as provider_entity,
};
use veltrix_core::error::{CrawlerError, Result};

/// 发送一条用户消息,驱动电脑操作 Agent 的 ReAct 循环;逐步落库 + 推 `agent-step` 进度,
/// 返回最终 assistant 消息(前端在 resolve 后重载消息以渲染完整工具往返)。
#[tauri::command]
pub async fn send_computer_message(
    state: State<'_, AppState>,
    app: AppHandle,
    conversation_id: String,
    content: String,
) -> Result<MessageView> {
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

    // 是否首轮(决定是否用首句起标题)
    let had_messages = msg::Entity::find()
        .filter(msg::Column::ConversationId.eq(&conversation_id))
        .one(&state.db)
        .await
        .ok()
        .flatten()
        .is_some();

    let now = Utc::now().timestamp();
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

    // 聚合全部电脑操作工具(desktop/fs/system/ocr/uia/net/shell + capture_screen)
    let registry = computer::build_registry(app.clone());
    let tool_defs = registry.defs();

    // live 原文窗口 + 滚动摘要(与 coding/rpa 一致)
    let mut rows = msg::Entity::find()
        .filter(msg::Column::ConversationId.eq(&conversation_id))
        .filter(msg::Column::Id.gt(conversation.summarized_upto_id))
        .order_by_desc(msg::Column::Id)
        .limit(conv_summary::LIVE_HARD_CAP)
        .all(&state.db)
        .await
        .map_err(|e| CrawlerError::Config(format!("读取历史失败: {e}")))?;
    rows.reverse();
    // 窗口须从第一条 user 开始,否则可能以 tool / assistant(tool_calls)开头致 OpenAI 报 400
    let windowed: &[msg::Model] = match rows.iter().position(|m| m.role == "user") {
        Some(start) => &rows[start..],
        None => &[],
    };

    let mut messages: Vec<ChatMsg> = vec![ChatMsg::System(computer::SYSTEM_PROMPT.to_string())];
    if let Some(sys) = conv_summary::summary_system_message(&conversation.summary) {
        if let Some(summary_text) = sys.get("content").and_then(|v| v.as_str()) {
            messages.push(ChatMsg::System(summary_text.to_string()));
        }
    }
    messages.extend(windowed.iter().filter_map(row_to_chat_msg));

    let provider_ref = ProviderRef {
        kind: ProviderKind::from_code(&provider.code),
        api_url: provider.api_url.clone(),
        api_key: provider.api_key.clone(),
        model: conversation.model.clone(),
    };
    let llm = provider_for(provider_ref.kind);
    let options = LlmOptions::default();

    let emit = |label: String| {
        let _ = app.emit(
            "agent-step",
            json!({ "conversationId": &conversation_id, "label": label }),
        );
    };

    // ReAct 循环
    let mut final_text = String::new();
    for iter in 0..MAX_ITERS {
        emit(format!("思考中…(第 {} 步)", iter + 1));
        let resp = llm
            .chat(LlmRequest {
                provider: &provider_ref,
                messages: &messages,
                tools: &tool_defs,
                options: &options,
            })
            .await?;

        // 无工具调用 → 模型收尾
        if resp.tool_calls.is_empty() {
            final_text = resp.content.unwrap_or_default();
            break;
        }

        // 落库 assistant(带 tool_calls)
        let assistant_text = resp.content.clone().unwrap_or_default();
        let tc_json = tool_calls_to_json(&resp.tool_calls);
        msg::ActiveModel {
            conversation_id: Set(conversation_id.clone()),
            role: Set("assistant".to_string()),
            content: Set(assistant_text.clone()),
            tool_calls: Set(Some(tc_json)),
            created_at: Set(Utc::now().timestamp()),
            ..Default::default()
        }
        .insert(&state.db)
        .await
        .map_err(|e| CrawlerError::Config(format!("保存回复失败: {e}")))?;
        messages.push(ChatMsg::Assistant {
            text: resp.content.clone(),
            tool_calls: resp.tool_calls.clone(),
        });

        // 逐个执行工具。capture_screen 截图作 UserWithImages 回灌视觉模型;危险工具先 emit 提示。
        let mut pending_images: Vec<String> = Vec::new();
        for call in &resp.tool_calls {
            if computer::is_dangerous(&call.name) {
                emit(format!("⚠ 危险操作 {}", call.name));
            } else {
                emit(format!("🔧 {}", call.name));
            }
            let result = registry.run(&call.name, call.arguments.clone()).await;
            let flag = if result.is_error { "✗" } else { "✓" };
            emit(format!("{flag} {}", call.name));

            // capture_screen 成功时 content 是图片 data URL:tool 消息只落库简短文本(不存超长 base64)
            let is_screenshot = call.name == "capture_screen" && !result.is_error;
            let tool_text = if is_screenshot {
                "已截屏,屏幕画面见随后的图片。".to_string()
            } else {
                result.content.clone()
            };
            msg::ActiveModel {
                conversation_id: Set(conversation_id.clone()),
                role: Set("tool".to_string()),
                content: Set(tool_text.clone()),
                tool_call_id: Set(Some(call.id.clone())),
                tool_name: Set(Some(call.name.clone())),
                created_at: Set(Utc::now().timestamp()),
                ..Default::default()
            }
            .insert(&state.db)
            .await
            .map_err(|e| CrawlerError::Config(format!("保存工具结果失败: {e}")))?;
            messages.push(ChatMsg::Tool {
                tool_call_id: call.id.clone(),
                content: tool_text,
            });
            if is_screenshot {
                pending_images.push(result.content);
            }
        }
        // 所有 tool 消息后注入截图(符合 OpenAI assistant.tool_calls→tool 顺序约束),让下一轮模型看到屏幕
        if !pending_images.is_empty() {
            messages.push(ChatMsg::UserWithImages {
                text: "以下是刚才截取的屏幕画面,请据此判断后继续。".to_string(),
                images: pending_images,
            });
        }

        if iter == MAX_ITERS - 1 {
            final_text = format!("(已达最大步数 {MAX_ITERS},已停止。可继续追问以推进。)");
        }
    }

    // 落库最终 assistant 消息
    let final_msg = msg::ActiveModel {
        conversation_id: Set(conversation_id.clone()),
        role: Set("assistant".to_string()),
        content: Set(final_text),
        created_at: Set(Utc::now().timestamp()),
        ..Default::default()
    }
    .insert(&state.db)
    .await
    .map_err(|e| CrawlerError::Config(format!("保存回复失败: {e}")))?;
    emit("完成".to_string());

    // 更新会话时间;首轮用用户首句起标题
    let mut am = conversation.into_active_model();
    am.updated_at = Set(Utc::now().timestamp());
    if !had_messages {
        am.title = Set(truncate_title(&text));
    }
    let _ = am.update(&state.db).await;

    Ok(MessageView::from(final_msg))
}
