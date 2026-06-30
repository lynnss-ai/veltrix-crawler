//! 长会话上下文策略的共享实现:live 原文窗口 + 滚动摘要。
//!
//! 由 `chat`(对话工作区)与 `coding`(编程 Agent)共用:发给模型的上下文 = system 提示 +
//! [会话滚动摘要] + [live 原文消息];live 原文 = id 大于 `summarized_upto_id` 的消息,更早的
//! 被压缩进会话 `summary`,聊多久 / 编多久都不硬截断丢早期上下文。
//!
//! 本模块只承载与场景无关的阈值常量、摘要 system 注入、折叠维护与 LLM 合并调用;
//! `spawn` 封装与角色化解析(resolve_role_provider)仍由各场景命令各自持有(便于带场景化提示)。

use sea_orm::{
    ActiveModelTrait, ColumnTrait, EntityTrait, IntoActiveModel, QueryFilter, QueryOrder,
    QuerySelect, Set,
};
use serde_json::{json, Value};
use veltrix_core::db::entity::{chat_conversation as conv, chat_message as msg};

/// 单次发送携带的 live 原文消息硬上限(安全兜底:摘要维护异步滞后时也不让上下文无限膨胀)。
pub const LIVE_HARD_CAP: u64 = 80;
/// live 原文消息数软阈值:超过则在回复后触发一次压缩(把较旧的折叠进摘要)。
pub const LIVE_MAX_MESSAGES: usize = 48;
/// live 原文总字符数软阈值:超过同样触发压缩(应对单条很长的消息)。
pub const LIVE_MAX_CHARS: usize = 24000;
/// 压缩后保留为 live 原文的最近消息数(其余折叠进摘要)。
pub const RECENT_KEEP_MESSAGES: usize = 24;
/// 单次压缩折叠进摘要的旧消息总字符上限(分批折叠,控制单次摘要调用的输入规模)。
pub const FOLD_MAX_CHARS: usize = 12000;
/// 会话摘要最大字符数(注入与维护时的上限)。
pub const MAX_SUMMARY_CHARS: usize = 2000;

/// 会话滚动摘要 → system 消息;摘要为空返回 None。供 chat / coding 注入前情提要复用。
pub fn summary_system_message(summary: &str) -> Option<Value> {
    let s = summary.trim();
    if s.is_empty() {
        return None;
    }
    Some(json!({
        "role": "system",
        "content": format!(
            "【本会话前情提要】(更早的对话已压缩,供你延续上下文,不必主动复述):\n{s}"
        )
    }))
}

/// 滚动摘要维护:当 live 原文超过软阈值(条数或字符)时,把较旧的一批消息折叠进会话摘要,
/// 并推进 `summarized_upto_id`。摘要调用失败则不推进边界,旧消息仍留在 live,下轮重试(不丢)。
///
/// `extra_summary_hint` 为场景化的额外保留要求(coding 传「文件清单 / 命令结果 / 待办」等),
/// 空串表示通用对话摘要。重新 `find_by_id` 读最新摘要与边界,避免用过期快照重复折叠。
///
/// 最大的坑:折叠边界绝不能把某条 tool 结果与产生它的 assistant(tool_calls)拆到两侧,
/// 否则下轮 live 窗口以孤立 tool 开头 → OpenAI 400。`new_upto` 只落在「一组 assistant(tool_calls)
/// + 其全部对应 tool 结果完整结束、且下一条是 user」的安全边界处。
pub async fn maintain_conversation_summary(
    db: &sea_orm::DatabaseConnection,
    conversation_id: &str,
    api_url: &str,
    api_key: &str,
    model: &str,
    extra_summary_hint: &str,
) {
    if api_url.trim().is_empty() || api_key.trim().is_empty() {
        return;
    }
    // 重新读会话拿最新摘要与边界(避免用过期快照导致重复折叠)
    let conversation = match conv::Entity::find_by_id(conversation_id.to_string())
        .one(db)
        .await
    {
        Ok(Some(c)) => c,
        Ok(None) => return,
        Err(e) => {
            tracing::warn!("摘要维护读取会话失败: {e}");
            return;
        }
    };
    // live 原文:id 大于已折叠边界,正序(取最旧的若干候选,上限 LIVE_HARD_CAP)
    let live = match msg::Entity::find()
        .filter(msg::Column::ConversationId.eq(conversation_id))
        .filter(msg::Column::Id.gt(conversation.summarized_upto_id))
        .order_by_asc(msg::Column::Id)
        .limit(LIVE_HARD_CAP)
        .all(db)
        .await
    {
        Ok(rows) => rows,
        Err(e) => {
            tracing::warn!("摘要维护读取历史失败: {e}");
            return;
        }
    };
    // 不足保底保留量,无需压缩
    if live.len() <= RECENT_KEEP_MESSAGES {
        return;
    }
    let total_chars: usize = live.iter().map(|m| m.content.chars().count()).sum();
    // 条数与字符任一超过软阈值才折叠
    if live.len() <= LIVE_MAX_MESSAGES && total_chars <= LIVE_MAX_CHARS {
        return;
    }

    // 从最旧开始累计要折叠的消息:不超过(总数 - 保底保留)且受单次字符预算约束(至少折 1 条)
    let max_fold = live.len() - RECENT_KEEP_MESSAGES;
    let mut fold_end = 0usize;
    let mut acc_chars = 0usize;
    for (i, m) in live.iter().enumerate() {
        if i >= max_fold {
            break;
        }
        let c = m.content.chars().count();
        if fold_end > 0 && acc_chars + c > FOLD_MAX_CHARS {
            break;
        }
        acc_chars += c;
        fold_end = i + 1;
    }
    // 收缩折叠边界到「工具往返完整结束」处:fold[fold_end-1] 之后若仍有该轮未折完的 tool 结果,
    // 会让 live 以孤立 tool 开头(其 assistant tool_calls 已折进摘要)→ OpenAI 400。
    // 故把 fold_end 回退到「最后一条不是 assistant(带 tool_calls)、且其后续 tool 结果都在折叠区内」的位置:
    // 即 fold_end 处(留在 live 的第一条)必须是 user 或不依赖更早 assistant 的消息。
    let fold_end = safe_fold_end(&live, fold_end);
    if fold_end == 0 {
        return;
    }
    let fold = &live[..fold_end];
    let new_upto = fold[fold.len() - 1].id;
    let folded_text = fold
        .iter()
        .map(format_message_line)
        .collect::<Vec<_>>()
        .join("\n");

    // 调 LLM 合并摘要;失败则不推进边界,下轮重试
    let Some(new_summary) = summarize_conversation(
        api_url,
        api_key,
        model,
        &conversation.summary,
        &folded_text,
        extra_summary_hint,
    )
    .await
    else {
        return;
    };

    let mut am = conversation.into_active_model();
    am.summary = Set(new_summary);
    am.summarized_upto_id = Set(new_upto);
    if let Err(e) = am.update(db).await {
        tracing::warn!("更新会话摘要失败: {e}");
    }
}

/// 把候选折叠长度收缩到「一轮对话 / 工具往返完整结束」的安全边界。
///
/// 最大的坑:折叠区与 live 的切分绝不能把「带 tool_calls 的 assistant」与其对应 tool 结果拆到两侧。
/// 若拆开,下轮 live 窗口会以孤立 tool(或其 assistant 已折进摘要)开头 → OpenAI 400。
///
/// 故约束:留在 live 的第一条(下标 `fold_end`)必须是 `user`——即 `new_upto` 落在「上一组
/// (user 提问 + assistant 回复 / 若干 assistant(tool_calls)+其全部 tool 结果)完整结束、
/// 下一条是 user」的位置。如此整组要么全折进摘要、要么全留在 live,绝不半折。
/// 从候选 `fold_end` 向前回退到最近一个「下一条是 user」的位置;回退到 0 表示本轮无安全边界
/// 可折(留待下轮积累更多消息再折,旧消息暂留 live,不丢)。
fn safe_fold_end(live: &[msg::Model], mut fold_end: usize) -> usize {
    while fold_end > 0 {
        // 留在 live 的首条必须是 user(其前的 assistant/tool 组已整组折进摘要)
        let first_kept_is_user = live
            .get(fold_end)
            .map(|m| m.role == "user")
            .unwrap_or(false);
        if first_kept_is_user {
            return fold_end;
        }
        fold_end -= 1;
    }
    0
}

/// 折叠区某条消息 → 摘要输入文本行;含工具往返也能可读地表达(供摘要 LLM 理解上下文)。
fn format_message_line(m: &msg::Model) -> String {
    match m.role.as_str() {
        "user" => format!("用户:{}", m.content),
        "assistant" => {
            // 带 tool_calls 的 assistant:补一句工具调用提示,让摘要知道执行了哪些动作
            if let Some(tc) = m.tool_calls.as_deref().filter(|s| !s.trim().is_empty()) {
                let names = tool_call_names(tc);
                if m.content.trim().is_empty() {
                    format!("助手(调用工具:{names})")
                } else {
                    format!("助手:{}(调用工具:{names})", m.content)
                }
            } else {
                format!("助手:{}", m.content)
            }
        }
        "tool" => {
            let name = m.tool_name.as_deref().unwrap_or("tool");
            format!("工具[{name}]结果:{}", m.content)
        }
        other => format!("{other}:{}", m.content),
    }
}

/// 从落库的 tool_calls JSON([{id,name,arguments}])提取工具名,逗号拼接(失败回退占位)。
fn tool_call_names(json_str: &str) -> String {
    serde_json::from_str::<Value>(json_str)
        .ok()
        .and_then(|v| v.as_array().cloned())
        .map(|arr| {
            arr.iter()
                .filter_map(|tc| tc.get("name").and_then(|n| n.as_str()).map(str::to_string))
                .collect::<Vec<_>>()
                .join(", ")
        })
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "工具".to_string())
}

/// 把【已有摘要】与【新增对话】合并为更新后的摘要(≤ MAX_SUMMARY_CHARS 字)。失败返回 None。
/// `extra_hint` 非空时(coding 场景)追加额外保留要求。
pub async fn summarize_conversation(
    api_url: &str,
    api_key: &str,
    model: &str,
    prev_summary: &str,
    folded_text: &str,
    extra_hint: &str,
) -> Option<String> {
    let prev = if prev_summary.trim().is_empty() {
        "(无)"
    } else {
        prev_summary
    };
    // 通用保留项 + 场景化补充(编程会话额外保留文件清单 / 命令结果 / 待办)
    let extra = extra_hint.trim();
    let extra_block = if extra.is_empty() {
        String::new()
    } else {
        format!("。此外请额外保留:{extra}")
    };
    let prompt = format!(
        "你在维护一段长对话的「前情摘要」。请把【已有摘要】和【新增对话】合并成一份更新后的摘要,保留对后续对话仍然有用的关键信息:用户的目标 / 事实 / 偏好、已达成的结论、待办或未决事项、重要的上下文与约束{extra_block}。要求:客观第三人称陈述、条理清晰、不逐字复述、不超过 {MAX_SUMMARY_CHARS} 字。只输出摘要正文,不要解释或标题。\n\n【已有摘要】\n{prev}\n\n【新增对话】\n{folded_text}"
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
    .ok()?
    .content;
    let cleaned = reply.trim();
    if cleaned.is_empty() {
        return None;
    }
    Some(cleaned.chars().take(MAX_SUMMARY_CHARS).collect())
}
