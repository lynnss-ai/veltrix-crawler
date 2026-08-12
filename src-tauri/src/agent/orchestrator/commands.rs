//! 编排器命令:send_orchestrator_message(默认对话)。
//! 基于通用 ReAct;模型支持 tools 时挂 4 个委派工具,否则空注册表 = 纯对话降级。
//! 子智能体在本会话 conversation_id 下串行运行,落库与事件都归本会话(内联可见)。

use sea_orm::EntityTrait;
use tauri::{AppHandle, State};
use veltrix_core::db::entity::provider as provider_entity;
use veltrix_core::error::{CrawlerError, Result};

use crate::agent::coding::commands::CodingExecCtx;
use crate::agent::core::react::{IterDecision, ReactConfig, ReactHooks};
use crate::agent::core::shared::{
    begin_agent_turn, finalize_conversation_meta, insert_final_assistant, live_windowed_messages,
    load_agent_guidelines, MessageView, MAX_ITERS,
};
use crate::agent::core::summary as conv_summary;
use crate::agent::core::{ChatMsg, ProviderKind, ProviderRef};
use crate::commands::{current_user, AppState};

/// 编排器钩子:迭代间检查取消令牌(双保险;步内取消由 LLM 流式读循环 select! 即时响应)。
struct OrchestratorHooks {
    cancel_token: tokio_util::sync::CancellationToken,
}

impl ReactHooks for OrchestratorHooks {
    fn on_iter_end(&mut self, _iter: usize) -> IterDecision {
        if self.cancel_token.is_cancelled() {
            IterDecision::Finish("(已停止)".to_string())
        } else {
            IterDecision::Continue
        }
    }
}

/// 当前模型是否具备 tools 能力(决定是否挂委派工具)。
fn model_supports_tools(provider_models: &str, model: &str) -> bool {
    crate::llm::provider::parse_models(provider_models)
        .into_iter()
        .find(|s| s.name == model)
        .map(|s| s.capabilities.iter().any(|c| c == "tools"))
        .unwrap_or(false)
}

/// 在所有已配置厂商里找一个「带工具调用能力」的可用模型(有 api_key + capabilities 含 tools),
/// 用作自动 fallback:会话所选模型不支持工具时,改用它来委派(否则编排器无法真正动手)。
async fn find_tools_provider(
    db: &sea_orm::DatabaseConnection,
) -> Option<(provider_entity::Model, String)> {
    let providers = provider_entity::Entity::find().all(db).await.ok()?;
    for p in providers {
        if p.api_key.trim().is_empty() {
            continue;
        }
        for spec in crate::llm::provider::parse_models(&p.models) {
            if spec.capabilities.iter().any(|c| c == "tools") {
                return Some((p, spec.name));
            }
        }
    }
    None
}

/// 默认对话:编排器 ReAct 循环。需要动手的任务由模型委派给子智能体工具执行,结果回灌本会话。
#[tauri::command]
pub async fn send_orchestrator_message(
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

    // 同会话发送互斥:并发发送排队串行(不拒绝),持锁到回合结束
    let send_lock = {
        let mut map = state
            .chat_send_locks
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        map.entry(conversation_id.clone()).or_default().clone()
    };
    let _send_guard = send_lock.lock().await;

    // 取消令牌:stop 命令 cancel 后,流式读循环 select! 即时中断;守卫收尾(含错误路径)摘除
    let (cancel_token, _cancel_guard) =
        crate::agent::core::shared::begin_cancel_token(&state.cancel_tokens, &conversation_id);

    let (conversation, provider, had_messages) =
        begin_agent_turn(&state.db, &me.name, &conversation_id, &text).await?;

    // 工具能力 + 自动 fallback:会话所选模型不支持工具调用时,自动改用任意已配置的、
    // 带工具能力的模型来委派(否则编排器只能纯对话、无法真正动手)。
    let conv_has_tools = model_supports_tools(&provider.models, &conversation.model);
    let fallback = if conv_has_tools {
        None
    } else {
        find_tools_provider(&state.db).await
    };
    let used_fallback = fallback.is_some();
    let (eff_provider, eff_model) = match fallback {
        Some((p, m)) => (p, m),
        None => (provider, conversation.model.clone()),
    };
    let has_tools = conv_has_tools || used_fallback;

    let provider_ref = ProviderRef {
        kind: ProviderKind::from_code(&eff_provider.code),
        api_url: eff_provider.api_url.clone(),
        api_key: eff_provider.api_key.clone(),
        model: eff_model.clone(),
    };

    let registry = if has_tools {
        super::tools::build_registry(
            state.db.clone(),
            app.clone(),
            conversation_id.clone(),
            me.name.clone(),
            provider_ref.clone(),
            eff_provider.id.clone(),
            state.config_dir.clone(),
            cancel_token.child_token(),
            CodingExecCtx::from_state(&state),
            state.webviews.clone(),
            state.agent_confirm.clone(),
        )
    } else {
        crate::agent::core::ToolRegistry::new()
    };

    // 系统提示:有工具用编排器提示(列出委派工具);无工具用纯对话提示——否则模型会照着提示词
    // 把工具调用「写成文字」假装执行(实际没委派、没产物)。
    let system_prompt = if has_tools {
        super::SYSTEM_PROMPT
    } else {
        super::PLAIN_PROMPT
    };

    // 上下文:系统提示 + 全局记忆 + 滚动摘要 + 过滤后历史(去掉 tool_calls/tool 行,防跨轮重放 400)
    let mut messages: Vec<ChatMsg> = vec![ChatMsg::System(system_prompt.to_string())];
    if used_fallback {
        messages.push(ChatMsg::System(format!(
            "(系统提示:本会话所选模型不支持工具调用,已临时改用「{eff_model}」执行委派。请在回答末尾用一句话简短告知用户已自动改用该模型。)"
        )));
    }
    if let Some(g) = load_agent_guidelines(&state.config_dir, "orchestrator").await {
        messages.push(ChatMsg::System(format!("【附加规范(用户自定义,务必遵守)】\n{g}")));
    }
    if let Some(v) =
        crate::agent::chat::memory::memory_system_message(&state.db, &me.name, &text, "global", "")
            .await
    {
        if let Some(c) = v.get("content").and_then(|x| x.as_str()) {
            messages.push(ChatMsg::System(c.to_string()));
        }
    }
    if let Some(sys) = conv_summary::summary_system_message(&conversation.summary) {
        if let Some(c) = sys.get("content").and_then(|x| x.as_str()) {
            messages.push(ChatMsg::System(c.to_string()));
        }
    }
    let history = live_windowed_messages(&state.db, &conversation).await?;
    messages.extend(history.into_iter().filter(|m| match m {
        // 丢弃带工具调用的 assistant 与所有 tool 结果行,避免编排器 / 子智能体交错的 tool_calls 破坏重放
        ChatMsg::Assistant { tool_calls, .. } => tool_calls.is_empty(),
        ChatMsg::Tool { .. } => false,
        _ => true,
    }));

    let config = ReactConfig {
        max_iters: MAX_ITERS,
        temperature: 0.3, // 略高:要会对话,不只是工具网格
        enable_streaming: true,
        context_window_size: 80,
        enable_parallel_tools: false, // 委派必须串行(共享会话资源)
        max_retries: 2,
        auto_fix_on_tool_error: false,
    };

    let mut hooks = OrchestratorHooks {
        cancel_token: cancel_token.clone(),
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
        Some(&cancel_token),
    )
    .await?;

    let _ = veltrix_core::db::entity::model_usage_record::Model::record(
        &state.db,
        &eff_model,
        &eff_provider.id,
        result.usage.prompt,
        result.usage.completion,
        "orchestrator",
        &me.name,
    )
    .await;

    let final_text = result.final_text;
    let final_msg = insert_final_assistant(
        &state.db,
        &conversation_id,
        final_text.clone(),
        result.final_reasoning,
    )
    .await?;
    finalize_conversation_meta(&state.db, conversation, had_messages, &text).await;

    // 记忆提取(与 chat 路径对齐:编排器回合同样沉淀用户偏好 / 事实到全局记忆)
    crate::agent::chat::commands::spawn_memory_extraction(
        &state.db,
        &me.name,
        provider_ref.clone(),
        &text,
        &final_text,
    );

    // 后台滚动摘要维护(杂活,优先 Summary 角色便宜模型,未配则回退会话模型)
    spawn_summary_maintenance(&state.db, &conversation_id, provider_ref);

    Ok(final_msg.into())
}

/// 后台维护本会话滚动摘要(不阻塞返回)。
fn spawn_summary_maintenance(
    db: &sea_orm::DatabaseConnection,
    conversation_id: &str,
    fallback: ProviderRef,
) {
    let db = db.clone();
    let conversation_id = conversation_id.to_string();
    tauri::async_runtime::spawn(async move {
        let p =
            crate::commands::resolve_role_provider(&db, crate::llm::AgentRole::Summary, fallback)
                .await;
        const HINT: &str = "对话要点、已委派的子任务及其结果、待办的下一步";
        conv_summary::maintain_conversation_summary(
            &db,
            &conversation_id,
            &p.api_url,
            &p.api_key,
            &p.model,
            HINT,
        )
        .await;
    });
}
