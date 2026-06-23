//! 🖥️ 电脑操作 Agent 命令:send_computer_message(ReAct 循环 + 工具往返落库 + 进度事件 + 截图回灌)。
//!
//! 骨架与 `agent::rpa::commands::send_browser_message` 同构(逐步落库 chat_messages、emit `agent-step`、
//! MAX_ITERS 防失控、capture_screen 截图作 UserWithImages 回灌视觉模型),区别在工具集来自
//! `agent::computer::tools`(聚合 desktop/fs/system/ocr/uia/net/shell 的全部工具)、系统提示词为电脑操作版。
//! 危险工具(is_dangerous)目前先 emit 提示,完整「暂停确认」握手待接入。

use crate::agent::computer::tools as computer;
use crate::agent::core::shared::{
    begin_agent_turn, finalize_conversation_meta, insert_assistant_tool_calls,
    insert_final_assistant, insert_tool_result, live_windowed_messages, load_agent_guidelines,
    MessageView, MAX_ITERS,
};
use crate::agent::core::summary as conv_summary;
use crate::agent::core::{
    provider_for, ChatMsg, LlmOptions, LlmRequest, ProviderKind, ProviderRef,
};
use crate::commands::{current_user, AppState};
use serde_json::json;
use tauri::{AppHandle, Emitter, State};
use veltrix_core::error::{CrawlerError, Result};

/// 危险操作等待用户确认的超时(秒):超时按「拒绝」处理,避免 ReAct 永久挂起。
const CONFIRM_TIMEOUT_SECS: u64 = 180;

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
    // 前奏(归属 / api_key 校验 + 首轮判定 + 落库 user 消息)统一走 core::shared
    let (conversation, provider, had_messages) =
        begin_agent_turn(&state.db, &me.name, &conversation_id, &text).await?;

    // 聚合全部电脑操作工具(desktop/fs/system/ocr/uia/net/shell + capture_screen)
    let registry = computer::build_registry(app.clone());
    let tool_defs = registry.defs();
    // 危险操作确认通道(命中危险工具时暂停等前端回执)
    let confirm_channel = state.agent_confirm.clone();

    // 系统提示词 + 滚动摘要 + live 原文窗口(窗口构建统一走 core::shared)
    let mut messages: Vec<ChatMsg> = vec![ChatMsg::System(computer::SYSTEM_PROMPT.to_string())];
    // 用户可编辑的附加规范(<config_dir>/agent-guidelines/computer.md):有则注入
    if let Some(g) = load_agent_guidelines(&state.config_dir, "computer").await {
        messages.push(ChatMsg::System(format!("【附加规范(用户自定义,务必遵守)】\n{g}")));
    }
    if let Some(sys) = conv_summary::summary_system_message(&conversation.summary) {
        if let Some(summary_text) = sys.get("content").and_then(|v| v.as_str()) {
            messages.push(ChatMsg::System(summary_text.to_string()));
        }
    }
    messages.extend(live_windowed_messages(&state.db, &conversation).await?);

    let provider_ref = ProviderRef {
        kind: ProviderKind::from_code(&provider.code),
        api_url: provider.api_url.clone(),
        api_key: provider.api_key.clone(),
        model: conversation.model.clone(),
    };
    let llm = provider_for(provider_ref.kind);
    // 低温:电脑操作 Agent 要的是精准、确定的工具调用,而非发散
    let options = LlmOptions {
        temperature: Some(0.2),
        ..LlmOptions::default()
    };

    let emit = |label: String| {
        let _ = app.emit(
            "agent-step",
            json!({ "conversationId": &conversation_id, "label": label }),
        );
    };

    // ReAct 循环
    let mut final_text = String::new();
    // 模型「无工具调用、直接收尾」那步的思考过程(达上限收尾路径无对应推理,保持 None)
    let mut final_reasoning: Option<String> = None;
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
            final_reasoning = resp.reasoning.clone();
            final_text = resp.content.unwrap_or_default();
            break;
        }

        // 落库 assistant(带 tool_calls)+ 推入内存上下文
        insert_assistant_tool_calls(&state.db, &conversation_id, &resp).await?;
        messages.push(ChatMsg::Assistant {
            text: resp.content.clone(),
            tool_calls: resp.tool_calls.clone(),
        });

        // 逐个执行工具。capture_screen 截图作 UserWithImages 回灌视觉模型;危险工具先暂停等用户确认。
        let mut pending_images: Vec<String> = Vec::new();
        for call in &resp.tool_calls {
            // 危险工具:暂停,emit agent-confirm 让前端弹确认框,等回执;拒绝 / 超时则不执行,
            // 把「已拒绝」作为工具结果回灌模型,让它改用更安全的方式而非中断整个循环。
            let result = if computer::is_dangerous(&call.name) {
                emit(format!("⚠ 危险操作 {},待确认", call.name));
                let (confirm_id, rx) = confirm_channel.open();
                let _ = app.emit(
                    "agent-confirm",
                    json!({
                        "conversationId": &conversation_id,
                        "confirmId": confirm_id,
                        "tool": &call.name,
                        "args": &call.arguments,
                    }),
                );
                let approved = match tokio::time::timeout(
                    std::time::Duration::from_secs(CONFIRM_TIMEOUT_SECS),
                    rx,
                )
                .await
                {
                    Ok(Ok(v)) => v,
                    // 超时或发送端被 drop:按拒绝处理,并清理通道条目
                    _ => {
                        confirm_channel.cancel(confirm_id);
                        false
                    }
                };
                if approved {
                    emit(format!("🔧 {}", call.name));
                    registry.run(&call.name, call.arguments.clone()).await
                } else {
                    emit(format!("✗ 已拒绝 {}", call.name));
                    crate::agent::core::ToolResult::err(format!(
                        "用户拒绝执行危险操作「{}」。请勿重试该操作,改用更安全的方式,或先向用户说明原因再继续。",
                        call.name
                    ))
                }
            } else {
                emit(format!("🔧 {}", call.name));
                registry.run(&call.name, call.arguments.clone()).await
            };
            let flag = if result.is_error { "✗" } else { "✓" };
            emit(format!("{flag} {}", call.name));

            // capture_screen 成功时 content 是图片 data URL:tool 消息只落库简短文本(不存超长 base64)
            let is_screenshot = call.name == "capture_screen" && !result.is_error;
            let tool_text = if is_screenshot {
                "已截屏,屏幕画面见随后的图片。".to_string()
            } else {
                result.content.clone()
            };
            insert_tool_result(&state.db, &conversation_id, call, &tool_text).await?;
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
    let final_msg =
        insert_final_assistant(&state.db, &conversation_id, final_text, final_reasoning).await?;
    emit("完成".to_string());

    // 更新会话时间;首轮用用户首句起标题(统一走 core::shared)
    finalize_conversation_meta(&state.db, conversation, had_messages, &text).await;

    Ok(MessageView::from(final_msg))
}

/// 前端对危险操作确认框的回执:approved=true 放行执行,false 拒绝。
/// 由 `send_computer_message` 的 ReAct 循环等待端接收;未登记 / 已超时的 confirm_id 安全忽略。
#[tauri::command]
pub async fn resolve_agent_confirm(
    state: State<'_, AppState>,
    confirm_id: u64,
    approved: bool,
) -> Result<()> {
    state.agent_confirm.complete(confirm_id, approved);
    Ok(())
}
