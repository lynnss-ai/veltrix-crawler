//! 🖥️ 电脑操作 Agent 命令:send_computer_message(基于通用 ReAct 运行器)。
//!
//! 工具集来自 `agent::computer`(聚合 desktop/fs/system/ocr/uia/net/shell 的全部工具),
//! 系统提示词为电脑操作版。危险工具(is_dangerous)会暂停等待用户确认。

use crate::agent::computer::tools as computer;
use crate::agent::core::react::{ReactConfig, ReactHooks, ToolPostAction};
use crate::agent::core::shared::{
    begin_agent_turn, confirm_dangerous_tool, finalize_conversation_meta, insert_final_assistant,
    live_windowed_messages, load_agent_guidelines, MessageView, MAX_ITERS,
};
use crate::agent::core::summary as conv_summary;
use crate::agent::core::shared::AgentConfirmChannel;
use crate::agent::core::{ChatMsg, ProviderKind, ProviderRef, ToolResult};
use crate::commands::{current_user, AppState};
use async_trait::async_trait;
use serde_json::Value;
use std::sync::Arc;
use tauri::{AppHandle, State};
use veltrix_core::error::{CrawlerError, Result};

/// Computer Agent 钩子:处理危险操作确认和 capture_screen 截图回灌。
struct ComputerHooks {
    confirm_channel: Arc<AgentConfirmChannel>,
    app: AppHandle,
    conversation_id: String,
}

#[async_trait]
impl ReactHooks for ComputerHooks {
    async fn on_before_tool(&mut self, name: &str, args: &Value) -> Option<ToolResult> {
        if !computer::is_dangerous(name) {
            return None; // 非危险工具,正常执行
        }
        // 危险工具:走共享确认闸门(emit agent-confirm → 等前端回执 / 超时拒绝)
        confirm_dangerous_tool(&self.confirm_channel, &self.app, &self.conversation_id, name, args)
            .await
    }

    fn on_after_tool(&mut self, name: &str, _args: &Value, result: &ToolResult) -> ToolPostAction {
        // capture_screen 成功时 content 是图片 data URL:改用 UserWithImages 作为真正的图片
        // part 喂给视觉模型(tool 消息只落简短占位,由 react_run 统一处理)。
        if name == "capture_screen" && !result.is_error {
            ToolPostAction::InjectUserImages {
                text: "已截屏,以下是当前屏幕画面。".to_string(),
                images: vec![result.content.clone()],
            }
        } else {
            ToolPostAction::Continue
        }
    }
}

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

    // 构建上下文:系统提示词 + 滚动摘要 + live 原文窗口
    let mut messages: Vec<ChatMsg> = vec![ChatMsg::System(computer::SYSTEM_PROMPT.to_string())];
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

    let config = ReactConfig {
        max_iters: MAX_ITERS,
        temperature: 0.2, // 低温:电脑操作 Agent 要精准、确定的工具调用
        enable_streaming: true, // 启用流式输出
        context_window_size: 80, // 默认上下文窗口
        enable_parallel_tools: true, // 启用工具并行执行
        max_retries: 2, // LLM 调用失败时重试 2 次
        auto_fix_on_tool_error: true, // 工具失败时自动修复
    };

    let mut hooks = ComputerHooks {
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

    // 记录 token 用量(多步 ReAct 累计;source=computer 供账单按场景拆分)
    let _ = veltrix_core::db::entity::model_usage_record::Model::record(
        &state.db,
        &conversation.model,
        &provider.id,
        result.usage.prompt,
        result.usage.completion,
        "computer",
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

/// 把一段电脑操作任务作为子任务在指定会话(通常是编排器会话)下跑完,返回最终文本。供编排器委派工具调用。
/// 仅 system+task、不带历史、不落最终消息 / 不收尾;危险确认走共享 confirm 通道(同会话 id)。
#[allow(clippy::too_many_arguments)]
pub async fn run_computer_subtask(
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
    let registry = computer::build_registry(app.clone());
    let mut messages: Vec<ChatMsg> = vec![ChatMsg::System(computer::SYSTEM_PROMPT.to_string())];
    if let Some(g) = load_agent_guidelines(config_dir, "computer").await {
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
    let mut hooks = ComputerHooks {
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
        "computer",
        owner,
    )
    .await;
    Ok(result.final_text)
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
