//! AI 对话长期记忆:跨会话、按用户归属的记忆条目。
//!
//! - 注入:发消息前把启用的记忆拼成一条 system 消息(见 [`memory_system_message`]),
//!   让 AI 跨会话记住用户的稳定事实 / 偏好。
//! - 提取:每轮回复落库后异步调 LLM 从本轮对话抽取「值得长期记住的事实」入库
//!   (见 [`extract_and_store_memories`]),去重后写入,不阻塞回复返回。
//! - 管理:list/add/update/delete/clear 命令供设置页手动维护;全局开关控制注入与提取。

use crate::commands::{current_user, AppState};
use crate::llm::{chat::chat_completion, chat::ChatRequest, http};
use chrono::Utc;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, IntoActiveModel, QueryFilter,
    QueryOrder, QuerySelect, Set,
};
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::HashSet;
use tauri::State;
use veltrix_core::db::entity::chat_memory as mem;
use veltrix_core::error::{CrawlerError, Result};

/// 每轮按语义相似度注入的记忆条数上限(置顶项额外恒注入,不占此额度)。
const TOP_K_INJECT: usize = 12;
/// 注入上下文的记忆总字符数上限(避免撑爆上下文,作最终兜底)。
const MAX_MEMORY_INJECT_CHARS: usize = 4000;
/// 每个用户记忆条数硬上限:达到后不再自动新增(用户可在设置页清理后继续)。
const MEMORY_HARD_CAP: usize = 200;
/// 单条记忆最大字符数(过长截断)。
const MAX_MEMORY_ITEM_CHARS: usize = 500;
/// 全局开关在 app_secrets 里的 key;值为 "0" 视为关闭,其余(含未设置)视为开启。
const MEMORY_ENABLED_KEY: &str = "chat_memory_enabled";
/// embedding(语义检索)配置在 app_secrets 的 key:base url / 模型 / 密钥三者齐全才启用检索。
const EMBED_API_URL_KEY: &str = "embedding_api_url";
const EMBED_MODEL_KEY: &str = "embedding_model";
const EMBED_API_KEY_KEY: &str = "embedding_api_key";

/// 自动记忆提取的系统指令:要求只输出 JSON 字符串数组。
const EXTRACT_PROMPT: &str = "你是对话记忆提取器。请从下面这轮对话中,提取关于「用户」值得长期记住的稳定信息,用于未来所有对话的个性化。\n\
规则:\n\
- 只提取长期有效的信息:身份 / 职业 / 长期偏好 / 习惯 / 目标 / 重要背景 / 期望的称呼或回答风格等。\n\
- 不要提取一次性的、临时的、仅与本轮上下文相关的内容(如本次的具体问题、临时数据)。\n\
- 不确定是否值得长期记住,就不要提取。\n\
- 每条一句话,简洁、自包含(脱离本轮对话也能看懂)。\n\
- 严格输出 JSON 字符串数组,例如 [\"用户是前端工程师\",\"用户偏好用简体中文回答\"];没有可记的就输出 []。\n\
- 只输出 JSON,不要任何解释,不要代码块标记。";

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryView {
    pub id: i64,
    pub content: String,
    pub source: String,
    pub enabled: bool,
    /// 置顶:恒注入,不参与相似度淘汰。
    pub pinned: bool,
    pub created_at: i64,
    pub updated_at: i64,
}

impl From<mem::Model> for MemoryView {
    fn from(m: mem::Model) -> Self {
        Self {
            id: m.id,
            content: m.content,
            source: m.source,
            enabled: m.enabled,
            pinned: m.pinned,
            created_at: m.created_at,
            updated_at: m.updated_at,
        }
    }
}

// ===================== 命令:记忆 CRUD =====================

/// 列出当前用户的记忆,按最近更新倒序。
#[tauri::command]
pub async fn list_chat_memories(state: State<'_, AppState>) -> Result<Vec<MemoryView>> {
    let me = current_user(&state).ok_or_else(|| CrawlerError::Config("未登录".into()))?;
    let rows = mem::Entity::find()
        .filter(mem::Column::Owner.eq(me.name))
        .order_by_desc(mem::Column::UpdatedAt)
        .limit(MEMORY_HARD_CAP as u64)
        .all(&state.db)
        .await
        .map_err(|e| CrawlerError::Config(format!("查询记忆失败: {e}")))?;
    Ok(rows.into_iter().map(Into::into).collect())
}

/// 手动新增一条记忆(source=manual,默认启用)。
#[tauri::command]
pub async fn add_chat_memory(state: State<'_, AppState>, content: String) -> Result<MemoryView> {
    let me = current_user(&state).ok_or_else(|| CrawlerError::Config("未登录".into()))?;
    let text: String = content.trim().chars().take(MAX_MEMORY_ITEM_CHARS).collect();
    if text.is_empty() {
        return Err(CrawlerError::Config("记忆内容为空".into()));
    }
    let now = Utc::now().timestamp();
    let saved = mem::ActiveModel {
        owner: Set(me.name),
        content: Set(text),
        source: Set("manual".to_string()),
        enabled: Set(true),
        pinned: Set(false),
        created_at: Set(now),
        updated_at: Set(now),
        ..Default::default()
    }
    .insert(&state.db)
    .await
    .map_err(|e| CrawlerError::Config(format!("保存记忆失败: {e}")))?;
    // 异步补算向量,不阻塞返回(未配置 embedding 时静默跳过)
    spawn_backfill(&state.db, vec![saved.id]);
    Ok(saved.into())
}

/// 更新一条记忆的内容与启用状态(归属校验)。
#[tauri::command]
pub async fn update_chat_memory(
    state: State<'_, AppState>,
    id: i64,
    content: String,
    enabled: bool,
) -> Result<()> {
    let me = current_user(&state).ok_or_else(|| CrawlerError::Config("未登录".into()))?;
    let row = find_owned(&state.db, id, &me.name).await?;
    let text: String = content.trim().chars().take(MAX_MEMORY_ITEM_CHARS).collect();
    if text.is_empty() {
        return Err(CrawlerError::Config("记忆内容为空".into()));
    }
    let mut am = row.into_active_model();
    am.content = Set(text);
    am.enabled = Set(enabled);
    // 内容可能已改,旧向量失效:清空后异步重算
    am.embedding = Set(None);
    am.embed_model = Set(None);
    am.updated_at = Set(Utc::now().timestamp());
    am.update(&state.db)
        .await
        .map_err(|e| CrawlerError::Config(format!("更新记忆失败: {e}")))?;
    spawn_backfill(&state.db, vec![id]);
    Ok(())
}

/// 置顶 / 取消置顶一条记忆(归属校验)。置顶项每轮恒注入,不参与相似度淘汰。
#[tauri::command]
pub async fn set_chat_memory_pinned(
    state: State<'_, AppState>,
    id: i64,
    pinned: bool,
) -> Result<()> {
    let me = current_user(&state).ok_or_else(|| CrawlerError::Config("未登录".into()))?;
    let row = find_owned(&state.db, id, &me.name).await?;
    let mut am = row.into_active_model();
    am.pinned = Set(pinned);
    am.updated_at = Set(Utc::now().timestamp());
    am.update(&state.db)
        .await
        .map_err(|e| CrawlerError::Config(format!("更新置顶失败: {e}")))?;
    Ok(())
}

/// 删除一条记忆(归属校验)。
#[tauri::command]
pub async fn delete_chat_memory(state: State<'_, AppState>, id: i64) -> Result<()> {
    let me = current_user(&state).ok_or_else(|| CrawlerError::Config("未登录".into()))?;
    // 先校验归属再删,避免删到别人的记忆
    find_owned(&state.db, id, &me.name).await?;
    mem::Entity::delete_by_id(id)
        .exec(&state.db)
        .await
        .map_err(|e| CrawlerError::Config(format!("删除记忆失败: {e}")))?;
    Ok(())
}

/// 清空当前用户的全部记忆。
#[tauri::command]
pub async fn clear_chat_memories(state: State<'_, AppState>) -> Result<()> {
    let me = current_user(&state).ok_or_else(|| CrawlerError::Config("未登录".into()))?;
    mem::Entity::delete_many()
        .filter(mem::Column::Owner.eq(me.name))
        .exec(&state.db)
        .await
        .map_err(|e| CrawlerError::Config(format!("清空记忆失败: {e}")))?;
    Ok(())
}

/// 读取全局记忆开关(默认开启)。
#[tauri::command]
pub async fn get_chat_memory_enabled(state: State<'_, AppState>) -> Result<bool> {
    Ok(memory_enabled(&state.db).await)
}

/// 设置全局记忆开关:关闭后既不注入也不自动提取。
#[tauri::command]
pub async fn set_chat_memory_enabled(state: State<'_, AppState>, enabled: bool) -> Result<()> {
    crate::commands::set_secret(&state.db, MEMORY_ENABLED_KEY, if enabled { "1" } else { "0" }).await
}

// ===================== 命令:embedding(语义检索)配置 =====================

/// embedding 配置回填用视图;api_key 只回传「是否已配置」,不回明文。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EmbeddingConfigView {
    pub api_url: String,
    pub model: String,
    pub has_api_key: bool,
}

/// 读取 embedding 配置(供记忆中心回填)。
#[tauri::command]
pub async fn get_embedding_config(state: State<'_, AppState>) -> Result<EmbeddingConfigView> {
    Ok(EmbeddingConfigView {
        api_url: crate::commands::get_secret(&state.db, EMBED_API_URL_KEY).await,
        model: crate::commands::get_secret(&state.db, EMBED_MODEL_KEY).await,
        has_api_key: !crate::commands::get_secret(&state.db, EMBED_API_KEY_KEY)
            .await
            .trim()
            .is_empty(),
    })
}

/// 保存 embedding 配置;api_key 留空表示不修改已存密钥。配齐后即对历史记忆按需补算向量。
#[tauri::command]
pub async fn set_embedding_config(
    state: State<'_, AppState>,
    api_url: String,
    model: String,
    api_key: String,
) -> Result<()> {
    crate::commands::set_secret(&state.db, EMBED_API_URL_KEY, api_url.trim()).await?;
    crate::commands::set_secret(&state.db, EMBED_MODEL_KEY, model.trim()).await?;
    if !api_key.trim().is_empty() {
        crate::commands::set_secret(&state.db, EMBED_API_KEY_KEY, api_key.trim()).await?;
    }
    Ok(())
}

/// 取 embedding 配置三元组;任一为空视为未配置(返回 None,调用方回退到非检索注入)。
async fn embedding_config(db: &DatabaseConnection) -> Option<(String, String, String)> {
    let api_url = crate::commands::get_secret(db, EMBED_API_URL_KEY).await;
    let model = crate::commands::get_secret(db, EMBED_MODEL_KEY).await;
    let api_key = crate::commands::get_secret(db, EMBED_API_KEY_KEY).await;
    if api_url.trim().is_empty() || model.trim().is_empty() || api_key.trim().is_empty() {
        return None;
    }
    Some((api_url, model, api_key))
}

// ===================== 辅助:注入与提取(供 chat 命令调用) =====================

/// 取当前用户的启用记忆,**按当前问题语义检索 top-K** 拼成一条 system 消息(JSON)。
/// 无记忆 / 关闭时返回 None。`query` 为本轮用户提问,用于语义匹配。
///
/// 检索策略:置顶记忆恒注入;其余按与 `query` 的余弦相似度取 top-K。
/// 未配置 embedding / 查询为空 / 查询向量化失败时,回退到「最近更新优先」(旧行为,零破坏)。
pub async fn memory_system_message(
    db: &DatabaseConnection,
    owner: &str,
    query: &str,
) -> Option<Value> {
    if !memory_enabled(db).await {
        return None;
    }
    // 一次性载入该用户全部启用记忆(≤ MEMORY_HARD_CAP=200,内存暴力 cosine 足够快)
    let rows = mem::Entity::find()
        .filter(mem::Column::Owner.eq(owner))
        .filter(mem::Column::Enabled.eq(true))
        .order_by_desc(mem::Column::UpdatedAt)
        .limit(MEMORY_HARD_CAP as u64)
        .all(db)
        .await
        .ok()?;
    if rows.is_empty() {
        return None;
    }
    let selected = select_memories(db, &rows, query).await;
    build_memory_message(&selected)
}

/// 选出本轮要注入的记忆:置顶恒注入 + 其余按与 query 的语义相似度取 top-K。
/// 缺向量的记忆触发后台补算;未配置 embedding / 查询空 / 向量化失败时回退最近更新优先。
async fn select_memories<'a>(
    db: &DatabaseConnection,
    rows: &'a [mem::Model],
    query: &str,
) -> Vec<&'a mem::Model> {
    let pinned: Vec<&mem::Model> = rows.iter().filter(|m| m.pinned).collect();
    let rest: Vec<&mem::Model> = rows.iter().filter(|m| !m.pinned).collect();
    let q = query.trim();

    // 非置顶记忆本就 ≤ 注入额度:全部注入即可,排序无意义 → 跳过 embedding 网络往返(降每条消息延迟)。
    // 与下方回退分支结果一致(rest 全取),只是省掉了对 embedding 接口的调用。
    if rest.len() <= TOP_K_INJECT {
        let mut out = pinned;
        out.extend(rest);
        return out;
    }

    if let Some((api_url, model, api_key)) = embedding_config(db).await {
        if !q.is_empty() {
            if let Ok(mut qvecs) =
                crate::llm::embedding::embed_texts(&api_url, &api_key, &model, &[q.to_string()])
                    .await
            {
                if let Some(qvec) = qvecs.pop() {
                    // 逐条算相似度;向量缺失或模型不匹配的记 -1(排末尾)并加入补算队列
                    let mut scored: Vec<(f32, &mem::Model)> = Vec::with_capacity(rest.len());
                    let mut missing: Vec<i64> = Vec::new();
                    for m in &rest {
                        let usable = m.embed_model.as_deref() == Some(model.as_str());
                        match (usable, parse_embedding(m)) {
                            (true, Some(v)) if v.len() == qvec.len() => {
                                scored.push((crate::llm::embedding::cosine(&qvec, &v), m));
                            }
                            _ => {
                                scored.push((-1.0, m));
                                missing.push(m.id);
                            }
                        }
                    }
                    if !missing.is_empty() {
                        spawn_backfill(db, missing);
                    }
                    scored.sort_by(|a, b| {
                        b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal)
                    });
                    let mut out = pinned;
                    out.extend(scored.into_iter().take(TOP_K_INJECT).map(|(_, m)| m));
                    return out;
                }
            }
        }
    }

    // 回退:置顶 + 最近更新优先(rows 已按 UpdatedAt 倒序,rest 保持同序)
    let mut out = pinned;
    out.extend(rest.into_iter().take(TOP_K_INJECT));
    out
}

/// 解析记忆里存的向量(JSON float 数组);为空 / 解析失败返回 None。
fn parse_embedding(m: &mem::Model) -> Option<Vec<f32>> {
    let raw = m.embedding.as_deref()?;
    if raw.trim().is_empty() {
        return None;
    }
    let arr: Vec<f32> = serde_json::from_str(raw).ok()?;
    if arr.is_empty() {
        None
    } else {
        Some(arr)
    }
}

/// 把选中的记忆按字符预算拼成 system 消息(置顶在前,优先保留)。
fn build_memory_message(selected: &[&mem::Model]) -> Option<Value> {
    let mut lines = String::new();
    let mut used = 0usize;
    for r in selected {
        let line = r.content.trim();
        if line.is_empty() {
            continue;
        }
        if used + line.chars().count() > MAX_MEMORY_INJECT_CHARS {
            break;
        }
        used += line.chars().count();
        lines.push_str("- ");
        lines.push_str(line);
        lines.push('\n');
    }
    if lines.is_empty() {
        return None;
    }
    let content = format!(
        "以下是与当前问题相关的用户长期记忆(来自历史对话或用户手动设置)。请在回答时自然地结合这些信息,但不要主动复述或提及「记忆」本身,除非用户问起:\n{lines}"
    );
    Some(json!({ "role": "system", "content": content }))
}

/// 把缺向量的记忆放到后台补算,不阻塞调用方;未配置 embedding 时静默跳过。
fn spawn_backfill(db: &DatabaseConnection, ids: Vec<i64>) {
    if ids.is_empty() {
        return;
    }
    let db = db.clone();
    tauri::async_runtime::spawn(async move {
        backfill_embeddings(&db, &ids).await;
    });
}

/// 为指定记忆补算并写回向量(取当前内容重新 embedding)。任何失败仅告警,不影响主流程。
async fn backfill_embeddings(db: &DatabaseConnection, ids: &[i64]) {
    let Some((api_url, model, api_key)) = embedding_config(db).await else {
        return;
    };
    let rows = match mem::Entity::find()
        .filter(mem::Column::Id.is_in(ids.to_vec()))
        .all(db)
        .await
    {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!("补算向量读取记忆失败: {e}");
            return;
        }
    };
    if rows.is_empty() {
        return;
    }
    let texts: Vec<String> = rows.iter().map(|m| m.content.clone()).collect();
    let vecs = match crate::llm::embedding::embed_texts(&api_url, &api_key, &model, &texts).await {
        Ok(v) if v.len() == rows.len() => v,
        Ok(_) => {
            tracing::warn!("补算向量数量不符,跳过");
            return;
        }
        Err(e) => {
            tracing::warn!("补算向量失败: {e}");
            return;
        }
    };
    for (m, v) in rows.into_iter().zip(vecs) {
        let mut am = m.into_active_model();
        am.embedding = Set(Some(serde_json::to_string(&v).unwrap_or_default()));
        am.embed_model = Set(Some(model.clone()));
        if let Err(e) = am.update(db).await {
            tracing::warn!("写回向量失败: {e}");
        }
    }
}

/// 从本轮对话提取记忆并落库(去重)。失败仅告警,不影响主流程——本函数设计为在 spawn 中调用。
pub async fn extract_and_store_memories(
    db: &DatabaseConnection,
    owner: &str,
    api_url: &str,
    api_key: &str,
    model: &str,
    user_text: &str,
    assistant_text: &str,
) {
    if !memory_enabled(db).await {
        return;
    }
    // 纯附件无用户文本时不提取(图片很少携带可长期记住的事实)
    if user_text.trim().is_empty() {
        return;
    }
    if api_url.trim().is_empty() || api_key.trim().is_empty() {
        return;
    }

    let existing = match mem::Entity::find()
        .filter(mem::Column::Owner.eq(owner))
        .all(db)
        .await
    {
        Ok(rows) => rows,
        Err(e) => {
            tracing::warn!("读取记忆失败,跳过本轮提取: {e}");
            return;
        }
    };
    // 达上限不再自动新增,避免无限增长(用户清理后可继续)
    if existing.len() >= MEMORY_HARD_CAP {
        return;
    }

    let Some(extracted) = call_extractor(api_url, api_key, model, user_text, assistant_text).await
    else {
        return;
    };
    if extracted.is_empty() {
        return;
    }

    // 去重:与已有记忆(规范化后精确匹配)及本批内部
    let mut seen: HashSet<String> = existing.iter().map(|m| normalize_key(&m.content)).collect();
    let mut contents: Vec<String> = Vec::new();
    for raw in extracted {
        let content: String = raw.trim().chars().take(MAX_MEMORY_ITEM_CHARS).collect();
        if content.is_empty() {
            continue;
        }
        let key = normalize_key(&content);
        if key.is_empty() || seen.contains(&key) {
            continue;
        }
        seen.insert(key);
        contents.push(content);
    }
    if contents.is_empty() {
        return;
    }

    // 落库即生成向量(best-effort):配了 embedding 就内联存,未配 / 失败则留空,后续按需补算
    let embeds: Option<(String, Vec<Vec<f32>>)> = match embedding_config(db).await {
        Some((url, model, key)) => {
            match crate::llm::embedding::embed_texts(&url, &key, &model, &contents).await {
                Ok(v) if v.len() == contents.len() => Some((model, v)),
                _ => None,
            }
        }
        None => None,
    };

    let now = Utc::now().timestamp();
    let mut to_insert: Vec<mem::ActiveModel> = Vec::with_capacity(contents.len());
    for (i, content) in contents.into_iter().enumerate() {
        let (embedding, embed_model) = match &embeds {
            Some((model, vecs)) => (
                Some(serde_json::to_string(&vecs[i]).unwrap_or_default()),
                Some(model.clone()),
            ),
            None => (None, None),
        };
        to_insert.push(mem::ActiveModel {
            owner: Set(owner.to_string()),
            content: Set(content),
            source: Set("auto".to_string()),
            enabled: Set(true),
            pinned: Set(false),
            embedding: Set(embedding),
            embed_model: Set(embed_model),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        });
    }
    if let Err(e) = mem::Entity::insert_many(to_insert).exec(db).await {
        tracing::warn!("自动记忆落库失败: {e}");
    }
}

// ===================== 内部辅助 =====================

/// 按 id 查记忆并校验归属;不存在或非本人一律拒绝。
async fn find_owned(db: &DatabaseConnection, id: i64, owner: &str) -> Result<mem::Model> {
    let row = mem::Entity::find_by_id(id)
        .one(db)
        .await
        .map_err(|e| CrawlerError::Config(format!("查询记忆失败: {e}")))?
        .ok_or_else(|| CrawlerError::Config("记忆不存在".into()))?;
    if row.owner != owner {
        return Err(CrawlerError::Config("无权操作该记忆".into()));
    }
    Ok(row)
}

/// 全局记忆开关:仅显式存 "0" 视为关闭,其余(含未设置)默认开启。
async fn memory_enabled(db: &DatabaseConnection) -> bool {
    crate::commands::get_secret(db, MEMORY_ENABLED_KEY).await != "0"
}

/// 记忆去重用的规范化 key:裁剪 + 小写 + 折叠空白。
fn normalize_key(s: &str) -> String {
    s.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// 调 LLM 提取记忆,解析出字符串数组;任何失败返回 None。
async fn call_extractor(
    api_url: &str,
    api_key: &str,
    model: &str,
    user_text: &str,
    assistant_text: &str,
) -> Option<Vec<String>> {
    let user: String = user_text.chars().take(2000).collect();
    let assistant: String = assistant_text.chars().take(2000).collect();
    let prompt = format!("{EXTRACT_PROMPT}\n\n用户:{user}\n助手:{assistant}");
    let reply = chat_completion(ChatRequest {
        api_url,
        api_key,
        model,
        messages: json!([{ "role": "user", "content": prompt }]),
        extra_body: None,
        timeout_secs: http::CHAT_TIMEOUT_SECS,
        retry_server_errors: false,
    })
    .await
    .ok()?;
    parse_memory_list(&reply)
}

/// 解析模型返回为字符串数组:容错去掉代码块包裹,截取首个 `[` 到末个 `]` 再解析。
fn parse_memory_list(reply: &str) -> Option<Vec<String>> {
    let body = reply
        .trim()
        .trim_start_matches("```json")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim();
    let start = body.find('[')?;
    let end = body.rfind(']')?;
    if end < start {
        return None;
    }
    let arr: Vec<Value> = serde_json::from_str(&body[start..=end]).ok()?;
    Some(
        arr.into_iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect(),
    )
}
