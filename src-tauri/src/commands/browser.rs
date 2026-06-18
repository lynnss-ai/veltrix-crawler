//! 浏览器 Agent 命令:send_browser_message(ReAct 循环 + 工具往返落库 + 进度事件)。
//!
//! 骨架照搬 `commands::coding::send_coding_message`:逐步落库 chat_messages、emit `agent-step`、
//! MAX_ITERS 防失控。区别在于工具集来自 `agent::browser`(navigate/click/type/read_page/wait_for),
//! 系统提示词为浏览器 Agent 版,且用独立平台名 "agent" 的 per-window 隔离窗口(不绑采集登录态)。
//!
//! 动作回读:工具经 `AgentActionChannel` 按 req_id 等待页面回传结果(`browser_agent_result` 命令),
//! 故动作有真实成败 / 页面信息;`capture_agent_preview` 截取窗口画面供前端右栏实时预览。
//! 复用 coding 的消息行 ↔ ChatMsg 转换 / 标题截断 / 滚动摘要,避免重复实现。

use crate::agent::browser;
use crate::commands::chat::MessageView;
use crate::commands::coding::{row_to_chat_msg, tool_calls_to_json, truncate_title};
use crate::commands::conversation_summary as conv_summary;
use crate::commands::{current_user, AppState};
use crate::llm::agent::{
    provider_for, ChatMsg, LlmOptions, LlmRequest, ProviderKind, ProviderRef,
};
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

// 复用一处定义,避免与 coding 各执一份魔法数:ReAct 最大步数(防失控循环)。
use crate::commands::coding::MAX_ITERS;

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

    // 工具注册表:用 conversation_id 作为 per-window 隔离 key(独立 "agent" 窗口,不绑采集登录态)。
    // 传入动作回读通道,使 navigate/click/read 等动作能按 req_id 等待页面回传结果。
    let registry = browser::build_registry(
        app.clone(),
        state.webviews.clone(),
        state.agent_actions.clone(),
        conversation_id.clone(),
    );
    let tool_defs = registry.defs();

    // live 原文窗口 + 滚动摘要(与 coding / chat 一致):id 大于已折叠边界的为原文,更早的靠摘要承载
    let mut rows = msg::Entity::find()
        .filter(msg::Column::ConversationId.eq(&conversation_id))
        .filter(msg::Column::Id.gt(conversation.summarized_upto_id))
        .order_by_desc(msg::Column::Id)
        .limit(conv_summary::LIVE_HARD_CAP)
        .all(&state.db)
        .await
        .map_err(|e| CrawlerError::Config(format!("读取历史失败: {e}")))?;
    // 取最新 LIVE_HARD_CAP 条后翻回升序(与 chat 一致),保证超额时尾部仍是刚落库的本轮 user
    rows.reverse();
    // 兜底:窗口须从第一条 user 开始,否则可能以 tool / assistant(tool_calls)开头致 OpenAI 报 400
    let windowed: &[msg::Model] = match rows.iter().position(|m| m.role == "user") {
        Some(start) => &rows[start..],
        None => &[],
    };

    let mut messages: Vec<ChatMsg> = vec![ChatMsg::System(browser::SYSTEM_PROMPT.to_string())];
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

    // ReAct 循环。浏览器动作只发出、不回读成败,故无 coding 那套 run_command 自动续修。
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

        // 逐个执行工具,结果落库 + 回灌
        for call in &resp.tool_calls {
            emit(format!("🔧 {}", call.name));
            let result = registry.run(&call.name, call.arguments.clone()).await;
            let flag = if result.is_error { "✗" } else { "✓" };
            emit(format!("{flag} {}", call.name));
            msg::ActiveModel {
                conversation_id: Set(conversation_id.clone()),
                role: Set("tool".to_string()),
                content: Set(result.content.clone()),
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
                content: result.content,
            });
        }

        // 达上限:强制收尾
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

    // 滚动摘要维护:异步进行不阻塞返回。复用 coding 的强化提示无意义(非编程),用通用维护。
    spawn_browser_summary_maintenance(&state.db, &conversation_id, provider_ref);

    Ok(final_msg.into())
}

/// 页面注入脚本经此回传一次浏览器 Agent 动作结果(navigate/click/read 等的回读)。
/// 与注入脚本里的 `browser_agent_result` 调用对应:按 req_id 把结果投递给等待中的动作。
/// data 为页面回读的任意 JSON(url/title/元素清单等);缺省为空对象。
#[tauri::command]
pub fn browser_agent_result(
    state: State<'_, AppState>,
    req_id: u64,
    ok: bool,
    data: Option<serde_json::Value>,
) -> Result<()> {
    state.agent_actions.complete(
        req_id,
        crate::webview::AgentActionOutcome {
            ok,
            data: data.unwrap_or(serde_json::Value::Null),
        },
    );
    Ok(())
}

/// 截取某浏览器 Agent 会话窗口的当前画面,返回 PNG 的 data URL(供右栏预览 <img> 直接用)。
/// 窗口尚未创建(还没 navigate)或平台不支持截图时返回 None,前端显示占位。
#[tauri::command]
pub async fn capture_agent_preview(
    app: AppHandle,
    conversation_id: String,
) -> Result<Option<String>> {
    use base64::Engine;
    use tauri::Manager;
    let label = crate::webview::pool::window_label("agent", &conversation_id);
    let Some(window) = app.get_webview_window(&label) else {
        return Ok(None);
    };
    let Some(png) = crate::webview::screenshot::capture_png(&window).await else {
        return Ok(None);
    };
    let b64 = base64::engine::general_purpose::STANDARD.encode(&png);
    Ok(Some(format!("data:image/png;base64,{b64}")))
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
