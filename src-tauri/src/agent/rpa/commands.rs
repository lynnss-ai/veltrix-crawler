//! 浏览器 Agent 命令:send_browser_message(基于通用 ReAct 运行器)。
//!
//! 工具集来自 `agent::rpa`(navigate/click/type/read_page/wait_for/get_network),
//! 系统提示词为浏览器 Agent 版,动作作用于**内嵌主窗口右栏的 "agent" 子 webview**。
//!
//! 本模块另提供 webview 的定位/显隐命令(set_agent_webview_bounds / show / hide)与拦截读取
//! (get_agent_network)。

use crate::agent::core::react::{ReactConfig, ReactHooks, ToolPostAction};
use crate::agent::core::shared::{
    begin_agent_turn, finalize_conversation_meta, insert_final_assistant, live_windowed_messages,
    load_agent_guidelines, MessageView, MAX_ITERS,
};
use crate::agent::core::summary as conv_summary;
use crate::agent::core::{ChatMsg, ProviderKind, ProviderRef, ToolResult};
use crate::agent::rpa::tools as browser;
use crate::commands::{current_user, AppState};
use serde_json::Value;
use tauri::{AppHandle, State};
use veltrix_core::error::{CrawlerError, Result};

/// get_agent_network 返回时单条响应体的截断长度(前端面板展示用)。
const NET_VIEW_BODY_CAP: usize = 4000;

/// 内嵌 Agent webview 拦截到的一条接口响应(前端拦截面板用)。字段需与 TS 端逐字对应。
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NetworkEntryView {
    pub url: String,
    pub body: String,
}

/// RPA Agent 钩子:处理 capture_screen 截图回灌。
struct RpaHooks;

impl ReactHooks for RpaHooks {
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

/// 发送一条用户消息,驱动浏览器 Agent 的 ReAct 循环;过程逐步落库 + 推 `agent-step` 进度事件,
/// 返回最终的 assistant 消息(前端在 resolve 后重载消息以渲染完整工具往返)。
#[tauri::command]
pub async fn send_browser_message(
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

    // 工具注册表
    let registry = browser::build_registry(app.clone(), state.webviews.clone(), conversation_id.clone());

    // 构建上下文:系统提示词 + 滚动摘要 + live 原文窗口
    let mut messages: Vec<ChatMsg> = vec![ChatMsg::System(browser::SYSTEM_PROMPT.to_string())];
    if let Some(g) = load_agent_guidelines(&state.config_dir, "rpa").await {
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
        temperature: 0.2, // 低温:浏览器 Agent 要精准、确定的选择器与动作
        enable_streaming: true, // 启用流式输出
        context_window_size: 80, // 默认上下文窗口
        enable_parallel_tools: true, // 启用工具并行执行
        max_retries: 2, // LLM 调用失败时重试 2 次
        auto_fix_on_tool_error: true, // 工具失败时自动修复
    };

    let result = crate::agent::core::react::react_run(
        &state.db,
        &app,
        &conversation_id,
        &provider_ref,
        config,
        &mut RpaHooks,
        &registry,
        &mut messages,
    )
    .await?;

    // 记录 token 用量(多步 ReAct 累计;source=rpa 供账单按场景拆分)
    let _ = veltrix_core::db::entity::model_usage_record::Model::record(
        &state.db,
        &conversation.model,
        &provider.id,
        result.usage.prompt,
        result.usage.completion,
        "rpa",
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

    // 滚动摘要维护:异步进行不阻塞返回
    spawn_browser_summary_maintenance(&state.db, &conversation_id, provider_ref);

    Ok(final_msg.into())
}

/// 把一段浏览器任务作为子任务在指定会话(通常是编排器会话)下跑完,返回最终文本。供编排器委派工具调用。
/// 仅 system+task、不带历史、不落最终消息 / 不收尾;webview / 拦截均按传入 conversation_id 隔离。
#[allow(clippy::too_many_arguments)]
pub async fn run_rpa_subtask(
    db: &sea_orm::DatabaseConnection,
    app: &AppHandle,
    pool: &std::sync::Arc<crate::webview::pool::WebviewPool>,
    config_dir: &std::path::Path,
    conversation_id: &str,
    owner: &str,
    provider_ref: &ProviderRef,
    provider_id: &str,
    task: &str,
) -> Result<String> {
    let registry = browser::build_registry(app.clone(), pool.clone(), conversation_id.to_string());
    let mut messages: Vec<ChatMsg> = vec![ChatMsg::System(browser::SYSTEM_PROMPT.to_string())];
    if let Some(g) = load_agent_guidelines(config_dir, "rpa").await {
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
    let result = crate::agent::core::react::react_run(
        db, app, conversation_id, provider_ref, config, &mut RpaHooks, &registry, &mut messages,
    )
    .await?;
    let _ = veltrix_core::db::entity::model_usage_record::Model::record(
        db,
        &provider_ref.model,
        provider_id,
        result.usage.prompt,
        result.usage.completion,
        "rpa",
        owner,
    )
    .await;
    Ok(result.final_text)
}

/// 前端把右栏 DOM 区域(逻辑坐标,相对主窗口客户区)同步给后端,定位内嵌 Agent webview。
/// webview 尚未创建(还没 navigate)则静默忽略——前端在 `agent-webview-ready` 后会重发。
#[tauri::command]
pub fn set_agent_webview_bounds(
    state: State<'_, AppState>,
    conversation_id: String,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
) -> Result<()> {
    state
        .webviews
        .set_agent_bounds(&conversation_id, x, y, width, height)
}

/// 显示某会话的内嵌 Agent webview(进入/返回 RPA 页时)。
#[tauri::command]
pub fn show_agent_webview(state: State<'_, AppState>, conversation_id: String) -> Result<()> {
    state.webviews.show_agent_webview(&conversation_id)
}

/// 隐藏某会话的内嵌 Agent webview(切到其它会话 / 弹模态时)。
#[tauri::command]
pub fn hide_agent_webview(state: State<'_, AppState>, conversation_id: String) -> Result<()> {
    state.webviews.hide_agent_webview(&conversation_id)
}

/// 隐藏全部内嵌 Agent webview(离开 RPA 工作区时,防原生层盖住其它页面)。
#[tauri::command]
pub fn hide_all_agent_webviews(state: State<'_, AppState>) -> Result<()> {
    state.webviews.hide_all_agent_webviews();
    Ok(())
}

/// 读取某会话内嵌 Agent webview 拦截到的接口响应(供右栏面板按需拉取;实时增量另走
/// `agent-network` 事件)。可选 url_contains 子串过滤。返回 (url, body) 列表,body 已截断。
#[tauri::command]
pub fn get_agent_network(
    state: State<'_, AppState>,
    conversation_id: String,
    url_contains: Option<String>,
) -> Result<Vec<NetworkEntryView>> {
    let needle = url_contains.unwrap_or_default().trim().to_lowercase();
    let Some(sink) = state.webviews.agent_sink(&conversation_id) else {
        return Ok(Vec::new());
    };
    let list = sink
        .lock()
        .map_err(|_| CrawlerError::Config("读取拦截缓冲失败(锁异常)".into()))?
        .iter()
        .filter(|r| needle.is_empty() || r.url.to_lowercase().contains(&needle))
        .map(|r| NetworkEntryView {
            url: r.url.clone(),
            body: r.body.chars().take(NET_VIEW_BODY_CAP).collect(),
        })
        .collect();
    Ok(list)
}

/// 把浏览器会话的滚动摘要维护放到后台 spawn 执行,避免阻塞回复返回。
/// 摘要属杂活,优先走 Summary 角色单独配置的便宜模型;未配置则回退会话模型(fallback)。
fn spawn_browser_summary_maintenance(
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
        // 浏览器会话强化提示:让摘要保留对续接操作最有用的状态
        const BROWSER_HINT: &str =
            "当前所在页面 / 网址、已执行的导航 / 点击 / 输入动作、以及待完成的下一步";
        conv_summary::maintain_conversation_summary(
            &db,
            &conversation_id,
            &p.api_url,
            &p.api_key,
            &p.model,
            BROWSER_HINT,
        )
        .await;
    });
}
