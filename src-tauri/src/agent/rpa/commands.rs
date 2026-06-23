//! 浏览器 Agent 命令:send_browser_message(ReAct 循环 + 工具往返落库 + 进度事件)。
//!
//! 骨架照搬 `agent::coding::commands::send_coding_message`:逐步落库 chat_messages、emit `agent-step`、
//! MAX_ITERS 防失控。区别在于工具集来自 `agent::rpa::tools`(navigate/click/type/read_page/wait_for/get_network),
//! 系统提示词为浏览器 Agent 版,动作作用于**内嵌主窗口右栏的 "agent" 子 webview**(不绑采集登录态)。
//!
//! 动作回读走 WebView2 ExecuteScript(`webview::script_eval`),不再依赖页面 invoke 回传;
//! 子 webview 装全量原生拦截,命中 json 响应写 sink + 推 `agent-network` 事件给前端面板。
//! 本模块另提供 webview 的定位/显隐命令(set_agent_webview_bounds / show / hide)与拦截读取
//! (get_agent_network)。复用 coding 的消息行 ↔ ChatMsg 转换 / 标题截断 / 滚动摘要。

use crate::agent::core::shared::{
    begin_agent_turn, finalize_conversation_meta, insert_assistant_tool_calls,
    insert_final_assistant, insert_tool_result, live_windowed_messages, load_agent_guidelines,
    MessageView, MAX_ITERS,
};
use crate::agent::core::summary as conv_summary;
use crate::agent::core::{
    provider_for, ChatMsg, LlmOptions, LlmRequest, ProviderKind, ProviderRef,
};
use crate::agent::rpa::tools as browser;
use crate::commands::{current_user, AppState};
use serde_json::json;
use tauri::{AppHandle, Emitter, State};
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

/// 发送一条用户消息,驱动浏览器 Agent 的 ReAct 循环;过程逐步落库 + 推 `agent-step` 进度事件,
/// 返回最终的 assistant 消息(前端在 resolve 后重载消息以渲染完整工具往返)。
///
/// 与 send_coding_message 的差异:工具集为 navigate/click/type;无 Plan/Act 模式;
/// 无 run_command 自动续修(浏览器动作只发出、不回读成败)。
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

    // 工具注册表:用 conversation_id 作为子 webview 隔离 key(内嵌主窗口右栏的 "agent" webview)。
    // 动作回读走 ExecuteScript(script_eval),不再依赖页面 invoke 回传通道。
    let registry = browser::build_registry(app.clone(), state.webviews.clone(), conversation_id.clone());
    let tool_defs = registry.defs();

    // 系统提示词 + 滚动摘要 + live 原文窗口(窗口构建统一走 core::shared)
    let mut messages: Vec<ChatMsg> = vec![ChatMsg::System(browser::SYSTEM_PROMPT.to_string())];
    // 用户可编辑的附加规范(<config_dir>/agent-guidelines/rpa.md):有则注入
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
    let llm = provider_for(provider_ref.kind);
    // 低温:浏览器 Agent 要的是精准、确定的选择器与动作,而非发散
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

    // ReAct 循环。浏览器动作只发出、不回读成败,故无 coding 那套 run_command 自动续修。
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

        // 逐个执行工具,结果落库 + 回灌。capture_screen 特殊:把截图作为图片消息注入(让视觉模型看)
        let mut pending_images: Vec<String> = Vec::new();
        for call in &resp.tool_calls {
            emit(format!("🔧 {}", call.name));
            let result = registry.run(&call.name, call.arguments.clone()).await;
            let flag = if result.is_error { "✗" } else { "✓" };
            emit(format!("{flag} {}", call.name));
            // capture_screen 成功时 content 是图片 data URL:tool 消息只落库 / 回灌简短文本
            //(不存超长 base64),图片改用随后注入的 UserWithImages 喂给视觉模型。
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
        // 所有 tool 消息之后统一注入截图(放在 tool 序列末尾,符合 OpenAI「assistant.tool_calls→tool」
        // 顺序约束),让下一轮模型「看到」屏幕画面。图片只在本次内存上下文、不落库(截图是即时上下文)。
        if !pending_images.is_empty() {
            messages.push(ChatMsg::UserWithImages {
                text: "以下是刚才截取的屏幕画面,请据此判断后继续。".to_string(),
                images: pending_images,
            });
        }

        // 达上限:强制收尾
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

    // 滚动摘要维护:异步进行不阻塞返回。复用 coding 的强化提示无意义(非编程),用通用维护。
    spawn_browser_summary_maintenance(&state.db, &conversation_id, provider_ref);

    Ok(final_msg.into())
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
