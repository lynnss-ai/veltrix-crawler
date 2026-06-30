//! 🗂️ 本机助手 Agent 命令:send_local_message(基于通用 ReAct 运行器)。
//!
//! 工具集来自 `agent::local`(聚合 fs/system/shell 的全部工具),系统提示词为本机助手版。
//! 危险工具(is_dangerous)会暂停等待用户确认;确认回执复用 computer 的 `resolve_agent_confirm` 命令
//! (共享同一 `state.agent_confirm` 通道,confirm_id 全局唯一,前端按 conversationId 过滤事件)。

use crate::agent::core::react::{ReactConfig, ReactHooks};
use crate::agent::core::shared::AgentConfirmChannel;
use crate::agent::core::shared::{
    begin_agent_turn, confirm_dangerous_tool, finalize_conversation_meta, insert_final_assistant,
    live_windowed_messages, load_agent_guidelines, MessageView, MAX_ITERS,
};
use crate::agent::core::summary as conv_summary;
use crate::agent::core::{ChatMsg, ProviderKind, ProviderRef, ToolResult};
use crate::agent::local::tools as local;
use crate::commands::{current_user, AppState};
use async_trait::async_trait;
use serde_json::Value;
use std::sync::Arc;
use tauri::{AppHandle, State};
use veltrix_core::error::{CrawlerError, Result};

/// 本机助手 Agent 钩子:只处理危险操作确认(无截图 / 视觉注入,故不实现 on_after_tool)。
struct LocalHooks {
    confirm_channel: Arc<AgentConfirmChannel>,
    app: AppHandle,
    conversation_id: String,
}

#[async_trait]
impl ReactHooks for LocalHooks {
    async fn on_before_tool(&mut self, name: &str, args: &Value) -> Option<ToolResult> {
        if !local::is_dangerous(name) {
            return None; // 非危险工具,正常执行
        }
        // 危险工具:走共享确认闸门(emit agent-confirm → 等前端回执 / 超时拒绝)
        confirm_dangerous_tool(&self.confirm_channel, &self.app, &self.conversation_id, name, args)
            .await
    }
}

/// 发送一条用户消息,驱动本机助手 Agent 的 ReAct 循环;逐步落库 + 推 `agent-step` 进度,
/// 返回最终 assistant 消息(前端在 resolve 后重载消息以渲染完整工具往返)。
#[tauri::command]
pub async fn send_local_message(
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

    // 聚合本机操作工具(fs/system/shell)
    let registry = local::build_registry();

    // 构建上下文:系统提示词 + 滚动摘要 + live 原文窗口
    let mut messages: Vec<ChatMsg> = vec![ChatMsg::System(local::SYSTEM_PROMPT.to_string())];
    if let Some(g) = load_agent_guidelines(&state.config_dir, "local").await {
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

    let config = ReactConfig {
        max_iters: MAX_ITERS,
        temperature: 0.2, // 低温:文件 / 终端操作要精准、确定的工具调用
        enable_streaming: true,
        context_window_size: 80,
        enable_parallel_tools: true,
        max_retries: 2,
        auto_fix_on_tool_error: true,
    };

    let mut hooks = LocalHooks {
        confirm_channel: state.agent_confirm.clone(),
        app: app.clone(),
        conversation_id: conversation_id.clone(),
    };

    let result = crate::agent::core::react::react_run(
        &state.db,
        &app,
        &conversation_id,
        &provider_ref,
        config,
        &mut hooks,
        &registry,
        &mut messages,
    )
    .await?;

    // 记录 token 用量(source=local 供账单按场景拆分)
    let _ = veltrix_core::db::entity::model_usage_record::Model::record(
        &state.db,
        &conversation.model,
        &provider.id,
        result.usage.prompt,
        result.usage.completion,
        "local",
        &me.name,
    )
    .await;

    // 落库最终 assistant 消息
    let final_msg = insert_final_assistant(
        &state.db,
        &conversation_id,
        result.final_text,
        result.final_reasoning,
    )
    .await?;

    // 更新会话时间;首轮用用户首句起标题
    finalize_conversation_meta(&state.db, conversation, had_messages, &text).await;

    Ok(MessageView::from(final_msg))
}

/// 把一段本机操作任务作为子任务在指定会话(通常是编排器会话)下跑完,返回最终文本。供编排器委派工具调用。
/// 仅 system+task、不带历史、不落最终消息 / 不收尾;危险确认走共享 confirm 通道(同会话 id)。
#[allow(clippy::too_many_arguments)]
pub async fn run_local_subtask(
    db: &sea_orm::DatabaseConnection,
    app: &AppHandle,
    agent_confirm: &Arc<AgentConfirmChannel>,
    config_dir: &std::path::Path,
    conversation_id: &str,
    owner: &str,
    provider_ref: &ProviderRef,
    provider_id: &str,
    task: &str,
) -> Result<String> {
    let registry = local::build_registry();
    let mut messages: Vec<ChatMsg> = vec![ChatMsg::System(local::SYSTEM_PROMPT.to_string())];
    if let Some(g) = load_agent_guidelines(config_dir, "local").await {
        messages.push(ChatMsg::System(format!("【附加规范(用户自定义,务必遵守)】\n{g}")));
    }
    messages.push(ChatMsg::User(task.to_string()));
    let config = ReactConfig {
        max_iters: MAX_ITERS,
        temperature: 0.2,
        enable_streaming: true,
        context_window_size: 80,
        enable_parallel_tools: true,
        max_retries: 2,
        auto_fix_on_tool_error: true,
    };
    let mut hooks = LocalHooks {
        confirm_channel: agent_confirm.clone(),
        app: app.clone(),
        conversation_id: conversation_id.to_string(),
    };
    let result = crate::agent::core::react::react_run(
        db, app, conversation_id, provider_ref, config, &mut hooks, &registry, &mut messages,
    )
    .await?;
    let _ = veltrix_core::db::entity::model_usage_record::Model::record(
        db,
        &provider_ref.model,
        provider_id,
        result.usage.prompt,
        result.usage.completion,
        "local",
        owner,
    )
    .await;
    Ok(result.final_text)
}
