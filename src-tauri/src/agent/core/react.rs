//! 通用 ReAct 循环运行器:封装 coding/rpa/computer 共用的骨架,
//! 各 Agent 通过 ReactConfig + ReactHooks 注入差异逻辑。
//!
//! 设计目标:
//! - 消除三份 ReAct 循环的重复代码
//! - 各 Agent 只需实现差异部分(钩子)
//! - 保持灵活性,不丢失任何现有功能

use async_trait::async_trait;
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter};

use super::llm::{ChatMsg, LlmOptions, LlmRequest, ProviderRef, TokenUsage, ToolResult};
use super::shared;
use veltrix_core::error::Result;

/// ReAct 循环配置(各 Agent 填自己的值)。
pub struct ReactConfig {
    /// 最大迭代步数。
    pub max_iters: usize,
    /// 模型温度(低温=精准,高温=发散)。
    pub temperature: f32,
    /// 是否启用流式输出(打字机效果)。
    pub enable_streaming: bool,
    /// 上下文窗口大小(消息条数)。0 表示使用默认值。
    pub context_window_size: u64,
    /// 是否启用工具并行执行。
    pub enable_parallel_tools: bool,
    /// LLM 调用失败时的最大重试次数。
    pub max_retries: u32,
    /// 工具执行失败时是否自动注入修复提示。
    pub auto_fix_on_tool_error: bool,
}

impl Default for ReactConfig {
    fn default() -> Self {
        Self {
            max_iters: 25,
            temperature: 0.2,
            enable_streaming: true,
            context_window_size: 0,
            enable_parallel_tools: true,
            max_retries: 2,
            auto_fix_on_tool_error: true,
        }
    }
}

/// 工具执行后的后处理决策。
pub enum ToolPostAction {
    /// 继续正常流程。
    Continue,
    /// 注入一条带图片的 user 消息(截图回灌给视觉模型):正文 + 图片 data URL 列表。
    /// 走 ChatMsg::UserWithImages,图片作为真正的 image part 发送,而非把 base64 当文本塞进上下文。
    InjectUserImages { text: String, images: Vec<String> },
}

/// 模型无工具调用时的判定决策。
pub enum FinishDecision {
    /// 正常收尾,用给定文本作为最终回复。
    Finish(String),
    /// 注入续写提示继续循环(用于自动修复、计划续航等)。
    ContinueWithPrompt(String),
}

/// 每轮迭代结束时(工具全部执行完后)的判定决策。
pub enum IterDecision {
    /// 继续下一轮。
    Continue,
    /// 注入一条 user 提示后继续(如「改了代码先验证再收尾」的硬闸门)。
    InjectAndContinue(String),
    /// 强制收尾,用给定文本作为最终回复(如模型声明完成 / 用户手动停止 / 达上限)。
    Finish(String),
}

/// 工具结果落库 / 入上下文的文本:截图回灌为图片时只存简短占位(避免把多 MB base64
/// 既当 tool 文本、又当图片重复塞进上下文 / 数据库),其余情况用工具原始结果(统一截断,
/// 防一次「读大文件 / 抓大页面」把后续每轮上下文撑爆)。
fn post_action_tool_text(post: &ToolPostAction, result: &ToolResult) -> String {
    match post {
        ToolPostAction::InjectUserImages { .. } => {
            "[截图已作为图片提供给视觉模型,见下条消息]".to_string()
        }
        _ => shared::truncate_tool_result(&result.content),
    }
}

/// 死循环检测阈值:连续相同调用 / 连续工具错误达到该次数即判循环。
const LOOP_DETECT_THRESHOLD: usize = 3;

/// ReAct 死循环检测:连续 ≥ LOOP_DETECT_THRESHOLD 次「相同 (工具名, 规范化参数)」
/// 或连续 ≥ LOOP_DETECT_THRESHOLD 次工具错误,判为陷入循环。纯逻辑抽出,便于单测。
pub struct LoopDetector {
    last_sig: Option<String>,
    same_streak: usize,
    error_streak: usize,
}

impl Default for LoopDetector {
    fn default() -> Self {
        Self::new()
    }
}

impl LoopDetector {
    pub fn new() -> Self {
        Self {
            last_sig: None,
            same_streak: 0,
            error_streak: 0,
        }
    }

    /// 记录一次工具调用结果;返回 true 表示检测到死循环(调用方应引导模型收尾)。
    pub fn record(&mut self, name: &str, args: &Value, is_error: bool) -> bool {
        let sig = format!("{name}\u{1}{}", normalize_args(args));
        if self.last_sig.as_deref() == Some(sig.as_str()) {
            self.same_streak += 1;
        } else {
            self.last_sig = Some(sig);
            self.same_streak = 1;
        }
        if is_error {
            self.error_streak += 1;
        } else {
            self.error_streak = 0;
        }
        self.same_streak >= LOOP_DETECT_THRESHOLD || self.error_streak >= LOOP_DETECT_THRESHOLD
    }
}

/// 规范化工具参数(对象键递归排序后序列化):让「键序不同、内容相同」的 JSON 判同,
/// 否则模型仅调换参数键序就能绕过相同调用检测。
fn normalize_args(v: &Value) -> String {
    fn sorted(v: &Value) -> Value {
        match v {
            Value::Object(m) => {
                let mut keys: Vec<&String> = m.keys().collect();
                keys.sort();
                let mut out = serde_json::Map::new();
                for k in keys {
                    out.insert(k.clone(), sorted(&m[k]));
                }
                Value::Object(out)
            }
            Value::Array(a) => Value::Array(a.iter().map(sorted).collect()),
            _ => v.clone(),
        }
    }
    sorted(v).to_string()
}

/// ReAct 循环钩子(各 Agent 实现自己的差异逻辑)。
///
/// `on_before_tool` 为 async:危险操作确认需要「emit 事件 → 等前端回执」的异步等待,
/// 同步钩子无法 await,故整个 trait 走 async_trait(其余钩子保持同步)。
#[async_trait]
pub trait ReactHooks: Send {
    /// 工具执行前的拦截(可异步:用于危险操作「暂停等用户确认」)。
    /// 返回 None 表示正常执行,Some(result) 表示跳过执行直接用该结果(如用户拒绝)。
    async fn on_before_tool(&mut self, _call_name: &str, _call_args: &Value) -> Option<ToolResult> {
        None
    }

    /// 工具执行后的后处理。可用于追踪状态、注入额外消息等。
    fn on_after_tool(&mut self, _call_name: &str, _call_args: &Value, _result: &ToolResult) -> ToolPostAction {
        ToolPostAction::Continue
    }

    /// 模型无工具调用时的判定。返回 Finish 正常收尾,ContinueWithPrompt 注入提示继续。
    fn on_model_finish(&mut self, content: Option<String>) -> FinishDecision {
        FinishDecision::Finish(content.unwrap_or_default())
    }

    /// 每轮迭代结束时的判定(工具全部执行完后)。
    /// 返回 Continue 继续、InjectAndContinue 注入提示后继续、Finish 强制收尾。
    fn on_iter_end(&mut self, _iter: usize) -> IterDecision {
        IterDecision::Continue
    }

    /// 循环结束后的清理(落库最终消息后调用)。
    fn on_finalize(&mut self) {}
}

/// 通用 ReAct 循环执行结果。
pub struct ReactResult {
    /// 最终回复文本。
    pub final_text: String,
    /// 最终回复的思考过程(仅推理型模型非空)。
    pub final_reasoning: Option<String>,
    /// 本轮 ReAct 全部 LLM 调用累计的 token 用量(供命令层落库计费)。
    pub usage: TokenUsage,
}

/// 通用 ReAct 循环:循环(LLM → 工具执行 → 落库)。
///
/// 前奏(begin_agent_turn)和收尾(finalize_conversation_meta)由调用方负责,
/// 本函数只封装中间的循环骨架。
/// `cancel`:可选取消令牌——迭代间检查 + 穿透进 LLM 流式读循环(步内 select! 即时中断)。
#[allow(clippy::too_many_arguments)]
pub async fn react_run(
    db: &sea_orm::DatabaseConnection,
    app: &AppHandle,
    conversation_id: &str,
    provider_ref: &ProviderRef,
    config: ReactConfig,
    hooks: &mut dyn ReactHooks,
    registry: &super::llm::ToolRegistry,
    messages: &mut Vec<ChatMsg>,
    cancel: Option<&tokio_util::sync::CancellationToken>,
) -> Result<ReactResult> {
    let llm = super::llm::provider_for(provider_ref.kind);
    let tool_defs = registry.defs();
    let options = LlmOptions {
        temperature: Some(config.temperature),
        ..Default::default()
    };

    // 上下文窗口大小:0 表示不限制
    let window_size = if config.context_window_size > 0 {
        Some(config.context_window_size as usize)
    } else {
        None
    };

    let app_clone = app.clone();
    let cid_clone = conversation_id.to_string();
    let emit = move |label: &str| {
        let _ = app_clone.emit(
            "agent-step",
            json!({ "conversationId": &cid_clone, "label": label }),
        );
    };

    // 委派工具(delegate_to_*)的类型化事件:前端据此开右栏分栏呈现结果。
    // 相比从 agent-step 文案里字符串匹配,类型化信号不靠文案、不被文案调整打断;
    // start/done 两相位都发,done 即「有结果可呈现」的时刻(对齐 Claude 的 artifact 触发点)。
    let app_panel = app.clone();
    let cid_panel = conversation_id.to_string();
    let emit_panel = move |tool_name: &str, phase: &str| {
        if let Some(agent) = tool_name.strip_prefix("delegate_to_") {
            let _ = app_panel.emit(
                "agent-panel",
                json!({ "conversationId": &cid_panel, "agent": agent, "phase": phase }),
            );
        }
    };

    // 流式输出:增量经合批器按时间窗攒批后 emit chat-stream 事件(格式不变,前端无感)
    let mut batcher = shared::DeltaBatcher::new(app.clone(), conversation_id.to_string());

    let mut final_text = String::new();
    let mut final_reasoning: Option<String> = None;
    let mut consecutive_tool_errors = 0;
    // 死循环检测:连续相同调用 / 连续工具错误达阈值即引导收尾并强制停止
    let mut loop_detector = LoopDetector::new();
    // 累计本轮全部 LLM 调用的 token 用量(coding/rpa/computer 多步循环最耗 token,逐步累加供计费)
    let mut usage = TokenUsage::default();

    for iter in 0..config.max_iters {
        // 迭代间取消检查(双保险;步内取消由 LLM 流式读循环的 select! 即时响应)
        if cancel.is_some_and(|t| t.is_cancelled()) {
            final_text = "(已停止)".to_string();
            break;
        }
        emit(&format!("思考中…(第 {} 步)", iter + 1));

        // 上下文窗口管理:截断过长的消息列表(保留 system 消息)
        if let Some(max_len) = window_size {
            truncate_messages(messages, max_len);
        }

        // LLM 调用(带重试);增量闭包借用合批器,调用结束后强制 flush 再进入工具执行
        let resp = {
            let mut on_delta = |delta: String| batcher.push("content", &delta);
            call_llm_with_retry(
                &*llm,
                provider_ref,
                messages,
                &tool_defs,
                &options,
                config.enable_streaming,
                &mut on_delta,
                config.max_retries,
                cancel,
            ).await?
        };
        batcher.flush();

        // 步内取消:流式读循环被令牌中断后在此统一收尾(半截工具调用已被 llm 层丢弃)
        if cancel.is_some_and(|t| t.is_cancelled()) {
            final_text = "(已停止)".to_string();
            break;
        }

        // 累计 token 用量(厂商可能不返回则为 0,saturating 防溢出)
        usage.prompt = usage.prompt.saturating_add(resp.usage.prompt);
        usage.completion = usage.completion.saturating_add(resp.usage.completion);

        // 无工具调用 → 钩子判定
        if resp.tool_calls.is_empty() {
            match hooks.on_model_finish(resp.content.clone()) {
                FinishDecision::Finish(text) => {
                    final_reasoning = resp.reasoning.clone();
                    final_text = text;
                    break;
                }
                FinishDecision::ContinueWithPrompt(prompt) => {
                    // 把模型这轮的话纳入上下文(让它记得刚说过什么),再追加续写提示
                    if let Some(t) = &resp.content {
                        if !t.trim().is_empty() {
                            messages.push(ChatMsg::Assistant {
                                text: Some(t.clone()),
                                tool_calls: vec![],
                            });
                        }
                    }
                    messages.push(ChatMsg::User(prompt));
                    continue;
                }
            }
        }

        // 落库 assistant(带 tool_calls) + 推入上下文
        shared::insert_assistant_tool_calls(db, conversation_id, &resp).await?;
        messages.push(ChatMsg::Assistant {
            text: resp.content.clone(),
            tool_calls: resp.tool_calls.clone(),
        });

        // 执行工具:支持并行执行无依赖的工具。pending_injects 直接收 ChatMsg(支持文本/图片回灌)。
        let mut pending_injects: Vec<ChatMsg> = Vec::new();
        let tool_calls = &resp.tool_calls;
        let mut has_tool_error = false;
        let mut loop_detected = false;

        if tool_calls.len() > 1 && config.enable_parallel_tools {
            // 并行执行多个工具
            let results = execute_tools_parallel(
                tool_calls,
                hooks,
                registry,
                &emit,
            ).await;

            // 处理结果
            for (call, result) in tool_calls.iter().zip(results) {
                if result.is_error {
                    has_tool_error = true;
                }
                if loop_detector.record(&call.name, &call.arguments, result.is_error) {
                    loop_detected = true;
                }
                // 钩子:执行后处理
                let post = hooks.on_after_tool(&call.name, &call.arguments, &result);
                let tool_text = post_action_tool_text(&post, &result);
                match post {
                    ToolPostAction::Continue => {}
                    ToolPostAction::InjectUserImages { text, images } => {
                        pending_injects.push(ChatMsg::UserWithImages { text, images })
                    }
                }

                // 落库工具结果(截图回灌时只落简短占位,base64 图片走随后的 UserWithImages)
                shared::insert_tool_result(db, conversation_id, call, &tool_text).await?;
                messages.push(ChatMsg::Tool {
                    tool_call_id: call.id.clone(),
                    content: tool_text,
                });
            }
        } else {
            // 串行执行工具(单个工具或禁用并行时)
            for call in tool_calls {
                emit(&format!("🔧 {}", call.name));
                emit_panel(&call.name, "start");

                // 钩子:执行前拦截(可异步,如危险操作等用户确认)
                let result = match hooks.on_before_tool(&call.name, &call.arguments).await {
                    Some(r) => r,
                    None => registry.run(&call.name, call.arguments.clone()).await,
                };

                let flag = if result.is_error { "✗" } else { "✓" };
                emit(&format!("{flag} {}", call.name));
                emit_panel(&call.name, "done");

                if result.is_error {
                    has_tool_error = true;
                }
                if loop_detector.record(&call.name, &call.arguments, result.is_error) {
                    loop_detected = true;
                }

                // 钩子:执行后处理
                let post = hooks.on_after_tool(&call.name, &call.arguments, &result);
                let tool_text = post_action_tool_text(&post, &result);
                match post {
                    ToolPostAction::Continue => {}
                    ToolPostAction::InjectUserImages { text, images } => {
                        pending_injects.push(ChatMsg::UserWithImages { text, images })
                    }
                }

                // 落库工具结果(截图回灌时只落简短占位,base64 图片走随后的 UserWithImages)
                shared::insert_tool_result(db, conversation_id, call, &tool_text).await?;
                messages.push(ChatMsg::Tool {
                    tool_call_id: call.id.clone(),
                    content: tool_text,
                });
            }
        }

        // 死循环检测:命中则注入「必须立即收尾」引导并强制停止(防模型无限重复同一调用空转)
        if loop_detected {
            emit("检测到工具调用循环,已强制收尾");
            messages.push(ChatMsg::User(
                "系统检测到工具调用陷入循环(连续重复相同调用或连续失败)。必须立即停止调用工具,直接收尾。"
                    .to_string(),
            ));
            final_text =
                "(检测到工具调用陷入循环,已强制停止。可换个问法或把任务拆小后重试。)".to_string();
            break;
        }

        // 错误恢复:工具执行失败时自动注入修复提示
        if has_tool_error {
            consecutive_tool_errors += 1;
            if config.auto_fix_on_tool_error && consecutive_tool_errors <= 2 {
                emit(&format!("工具执行失败,自动尝试修复…(第 {consecutive_tool_errors} 次)"));
                messages.push(ChatMsg::User(
                    "工具执行失败。请分析错误原因，尝试使用不同的参数或方法重试。如果多次失败，请说明原因并尝试其他方案。".to_string()
                ));
            }
        } else {
            consecutive_tool_errors = 0;
        }

        // 注入钩子产生的额外消息(纯文本 / 截图图片回灌)
        for msg in pending_injects {
            messages.push(msg);
        }

        // 钩子:迭代结束判定(继续 / 注入提示后继续 / 强制收尾)
        match hooks.on_iter_end(iter) {
            IterDecision::Continue => {}
            IterDecision::InjectAndContinue(prompt) => messages.push(ChatMsg::User(prompt)),
            IterDecision::Finish(text) => {
                final_text = text;
                break;
            }
        }

        // 达上限:强制收尾
        if iter == config.max_iters - 1 {
            final_text = format!(
                "(已达最大步数 {},已停止。可继续追问以推进。)",
                config.max_iters
            );
        }
    }

    hooks.on_finalize();

    Ok(ReactResult {
        final_text,
        final_reasoning,
        usage,
    })
}

/// 截断过长的消息列表，保留 system 消息和最近的消息。
/// 确保不会截断 tool_calls 和对应的 tool 结果。
fn truncate_messages(messages: &mut Vec<ChatMsg>, max_len: usize) {
    if messages.len() <= max_len {
        return;
    }

    // 找到第一个非 system 消息的位置
    let first_non_system = messages
        .iter()
        .position(|m| !matches!(m, ChatMsg::System(_)))
        .unwrap_or(0);

    let system_count = first_non_system;
    let available_slots = max_len.saturating_sub(system_count);

    if available_slots == 0 {
        // 只保留 system 消息
        messages.truncate(system_count);
        return;
    }

    // 保留最后 available_slots 条非 system 消息
    let non_system_messages: Vec<ChatMsg> = messages[system_count..].to_vec();
    let keep_start = non_system_messages.len().saturating_sub(available_slots);

    // 确保不会从 tool_calls 或 tool 结果中间截断
    let safe_start = find_safe_truncation_point(&non_system_messages, keep_start);

    // 重建消息列表
    let mut new_messages: Vec<ChatMsg> = messages[..system_count].to_vec();
    new_messages.extend_from_slice(&non_system_messages[safe_start..]);
    *messages = new_messages;
}

/// 找到安全的截断点，确保不会截断 tool_calls 和对应的 tool 结果。
fn find_safe_truncation_point(messages: &[ChatMsg], desired_start: usize) -> usize {
    if desired_start == 0 {
        return 0;
    }

    // 从 desired_start 向后找，确保不会从 tool_calls 中间截断
    let mut safe_start = desired_start;

    // 检查 desired_start 位置是否是 tool 结果
    if let Some(ChatMsg::Tool { .. }) = messages.get(safe_start) {
        // 如果是 tool 结果，需要找到对应的 assistant(tool_calls)
        for i in (0..safe_start).rev() {
            if let ChatMsg::Assistant { tool_calls, .. } = &messages[i] {
                if !tool_calls.is_empty() {
                    // 找到对应的 assistant，从这条 assistant 开始
                    safe_start = i;
                    break;
                }
            }
        }
    }

    // 确保不会截断到 assistant(tool_calls) 的中间
    if let Some(ChatMsg::Assistant { tool_calls, .. }) = messages.get(safe_start) {
        if !tool_calls.is_empty() {
            // 如果这条 assistant 有 tool_calls，需要确保所有 tool 结果都在
            let tool_call_ids: Vec<String> = tool_calls.iter().map(|tc| tc.id.clone()).collect();
            for msg in messages.iter().skip(safe_start + 1) {
                if let ChatMsg::Tool { tool_call_id, .. } = msg {
                    if tool_call_ids.contains(tool_call_id) {
                        continue;
                    }
                }
                break;
            }
        }
    }

    safe_start
}

/// 并行执行多个工具调用。
/// 返回结果列表，顺序与输入 tool_calls 一致。
async fn execute_tools_parallel(
    tool_calls: &[super::llm::ToolCall],
    hooks: &mut dyn ReactHooks,
    registry: &super::llm::ToolRegistry,
    emit: &(dyn Fn(&str) + Send + Sync),
) -> Vec<ToolResult> {
    // 并行执行需要在 spawn 中运行，但 hooks 不是 Send
    // 改为串行执行（与 enable_parallel_tools=false 时一致）
    let mut results = Vec::new();

    for call in tool_calls {
        emit(&format!("🔧 {}", call.name));

        // 钩子:执行前拦截(可异步,如危险操作等用户确认)
        let result = match hooks.on_before_tool(&call.name, &call.arguments).await {
            Some(r) => r,
            None => registry.run(&call.name, call.arguments.clone()).await,
        };

        let flag = if result.is_error { "✗" } else { "✓" };
        emit(&format!("{flag} {}", call.name));

        results.push(result);
    }

    results
}

/// 带重试的 LLM 调用。
#[allow(clippy::too_many_arguments)]
async fn call_llm_with_retry(
    llm: &dyn super::llm::LlmProvider,
    provider_ref: &ProviderRef,
    messages: &[ChatMsg],
    tool_defs: &[super::llm::ToolDef],
    options: &LlmOptions,
    enable_streaming: bool,
    on_delta: &mut (dyn FnMut(String) + Send),
    max_retries: u32,
    cancel: Option<&tokio_util::sync::CancellationToken>,
) -> Result<super::llm::LlmResponse> {
    let mut last_error = None;

    for attempt in 0..=max_retries {
        let result = if enable_streaming {
            llm.chat_stream(
                LlmRequest {
                    provider: provider_ref,
                    messages,
                    tools: tool_defs,
                    options,
                    cancel,
                },
                on_delta,
            )
            .await
        } else {
            llm.chat(LlmRequest {
                provider: provider_ref,
                messages,
                tools: tool_defs,
                options,
                cancel,
            })
            .await
        };

        match result {
            Ok(resp) => return Ok(resp),
            Err(e) => {
                last_error = Some(e);
                // 已取消:不再退避重试,直接把错误交上层(正常取消走 Ok 分支,这里兜竞态)
                if cancel.is_some_and(|t| t.is_cancelled()) {
                    break;
                }
                if attempt < max_retries {
                    // 等待一段时间后重试
                    tokio::time::sleep(tokio::time::Duration::from_millis(1000 * (attempt + 1) as u64)).await;
                }
            }
        }
    }

    Err(last_error.unwrap_or_else(|| veltrix_core::error::CrawlerError::Config("LLM 调用失败".into())))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 规范化参数键序无关() {
        let a = json!({ "x": 1, "y": [2, { "b": 1, "a": 2 }] });
        let b = json!({ "y": [2, { "a": 2, "b": 1 }], "x": 1 });
        assert_eq!(normalize_args(&a), normalize_args(&b));
    }

    #[test]
    fn 连续相同调用达阈值判循环() {
        let mut d = LoopDetector::new();
        let args = json!({ "path": "a.rs" });
        assert!(!d.record("read_file", &args, false));
        assert!(!d.record("read_file", &args, false));
        assert!(d.record("read_file", &args, false));
    }

    #[test]
    fn 参数不同则重新计数() {
        let mut d = LoopDetector::new();
        assert!(!d.record("read_file", &json!({ "path": "a" }), false));
        assert!(!d.record("read_file", &json!({ "path": "a" }), false));
        // 第三次换了参数:不判循环
        assert!(!d.record("read_file", &json!({ "path": "b" }), false));
        assert!(!d.record("read_file", &json!({ "path": "b" }), false));
    }

    #[test]
    fn 键序不同的相同参数也判循环() {
        let mut d = LoopDetector::new();
        assert!(!d.record("run", &json!({ "a": 1, "b": 2 }), false));
        assert!(!d.record("run", &json!({ "b": 2, "a": 1 }), false));
        assert!(d.record("run", &json!({ "a": 1, "b": 2 }), false));
    }

    #[test]
    fn 连续工具错误达阈值判循环() {
        let mut d = LoopDetector::new();
        assert!(!d.record("a", &json!({}), true));
        assert!(!d.record("b", &json!({}), true)); // 错误累积与调用签名无关
        assert!(d.record("c", &json!({}), true));
    }

    #[test]
    fn 成功调用重置错误连击() {
        let mut d = LoopDetector::new();
        assert!(!d.record("a", &json!({}), true));
        assert!(!d.record("b", &json!({}), true));
        assert!(!d.record("c", &json!({}), false)); // 成功:错误连击清零
        assert!(!d.record("d", &json!({}), true));
        assert!(!d.record("e", &json!({}), true));
    }
}
