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

/// 每轮按综合评分注入的记忆条数上限(置顶项额外恒注入,不占此额度)。
const TOP_K_INJECT: usize = 12;
/// 身份类(identity)恒注入的条数上限:置顶为用户显式要求不设限,但 identity 由 LLM 自动分类、
/// 可能堆积,超出此数的低分 identity 退回按相关度竞争,避免把与当前问题最相关的记忆挤出字符预算。
const ALWAYS_IDENTITY_CAP: usize = 8;
/// 注入上下文的记忆总字符数上限(避免撑爆上下文,作最终兜底)。
const MAX_MEMORY_INJECT_CHARS: usize = 4000;
/// 每个用户记忆条数硬上限:达到后自动新增前先淘汰综合分最低者(不再粗暴拒绝)。
const MEMORY_HARD_CAP: usize = 200;
/// 单条记忆最大字符数(过长截断)。
const MAX_MEMORY_ITEM_CHARS: usize = 500;
/// 智能维护时给 LLM 看的「相关已有记忆」条数(作去重 / 更新判断的依据)。
const MAINTAIN_CONTEXT_N: usize = 15;
/// 注入综合评分权重:语义相似 + 重要度 + 时间衰减 + 命中频次。
const W_SIM: f32 = 1.0;
const W_IMP: f32 = 0.30;
const W_REC: f32 = 0.20;
const W_HIT: f32 = 0.15;
/// 时间衰减半衰期(秒,约 30 天):久未命中的记忆按指数衰减降权。
const RECENCY_HALF_LIFE_SECS: f32 = 30.0 * 86400.0;
/// 已知记忆分类;未知值落库时归一为 other。
const MEM_TYPES: &[&str] = &[
    "identity",
    "preference",
    "project",
    "relationship",
    "habit",
    "other",
];
/// 全局开关在 app_secrets 里的 key;值为 "0" 视为关闭,其余(含未设置)视为开启。
const MEMORY_ENABLED_KEY: &str = "chat_memory_enabled";
/// embedding(语义检索)配置在 app_secrets 的 key:base url / 模型 / 密钥三者齐全才启用检索。
const EMBED_API_URL_KEY: &str = "embedding_api_url";
const EMBED_MODEL_KEY: &str = "embedding_model";
const EMBED_API_KEY_KEY: &str = "embedding_api_key";

/// 记忆维护(提取 + 分类 + 打分 + 去重/更新决策)的系统指令:输出操作对象数组。
/// 关键:给 LLM 看「相关已有记忆」,让它判断每条是新增还是更新已有某条(根治碎片化与矛盾堆积)。
const MAINTAIN_PROMPT: &str = "你是对话记忆管理器。基于本轮对话,维护关于「用户」的长期记忆库,用于未来所有对话的个性化。\n\
规则:\n\
- 只关注长期有效的稳定信息:身份 / 职业 / 长期偏好 / 习惯 / 目标 / 重要背景 / 期望的称呼或回答风格 / 与用户相关的人或项目等。忽略一次性、临时、仅本轮相关的内容。\n\
- 对每条值得记住的信息,决定操作:\n\
  - add:全新信息,下面「已有记忆」里没有。\n\
  - update:本轮信息**更新或修正**了某条已有记忆(如职业变了、偏好改了、称呼变了),给出那条记忆的 id 与新内容。\n\
  - 若已有记忆已完整覆盖、无新增或变化,就不要输出该条(等于跳过)。\n\
- 每条标注:type(identity/preference/project/relationship/habit/other 之一)、importance(1-5,对个性化的重要程度)、confidence(1-5,你对该信息的确定程度)。\n\
- 每条 content 一句话,简洁、自包含(脱离本轮对话也能看懂)。\n\
- 严格输出 JSON 数组,每项形如 {\"op\":\"add\",\"content\":\"用户是前端工程师\",\"type\":\"identity\",\"importance\":4,\"confidence\":5} 或 {\"op\":\"update\",\"id\":12,\"content\":\"用户已转为后端工程师\",\"type\":\"identity\",\"importance\":4,\"confidence\":5}。\n\
- 没有要增改的就输出 []。只输出 JSON,不要任何解释,不要代码块标记。";

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryView {
    pub id: i64,
    pub content: String,
    pub source: String,
    pub enabled: bool,
    /// 置顶:恒注入,不参与相似度淘汰。
    pub pinned: bool,
    /// 作用域:global(全局)/project(项目)/conversation(会话)
    pub scope: String,
    /// 作用域 ID:project 时为项目 ID,conversation 时为会话 ID,global 时为空
    pub scope_id: String,
    /// 分类:identity/preference/project/relationship/habit/other。
    pub mem_type: String,
    /// 重要度 1-5。
    pub importance: i32,
    /// 置信度 1-5。
    pub confidence: i32,
    /// 命中次数(被注入的累计次数)。
    pub hit_count: i64,
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
            scope: m.scope,
            scope_id: m.scope_id,
            mem_type: m.mem_type,
            importance: m.importance,
            confidence: m.confidence,
            hit_count: m.hit_count,
            created_at: m.created_at,
            updated_at: m.updated_at,
        }
    }
}

// ===================== 命令:记忆 CRUD =====================

/// 列出当前用户的记忆,按最近更新倒序。
/// 可选按 scope 和 scope_id 过滤。
#[tauri::command]
pub async fn list_chat_memories(
    state: State<'_, AppState>,
    scope: Option<String>,
    scope_id: Option<String>,
) -> Result<Vec<MemoryView>> {
    let me = current_user(&state).ok_or_else(|| CrawlerError::Config("未登录".into()))?;
    let mut query = mem::Entity::find()
        .filter(mem::Column::Owner.eq(me.name));

    // 按 scope 过滤
    if let Some(s) = scope {
        query = query.filter(mem::Column::Scope.eq(s));
    }
    if let Some(sid) = scope_id {
        query = query.filter(mem::Column::ScopeId.eq(sid));
    }

    let rows = query
        .order_by_desc(mem::Column::UpdatedAt)
        .limit(MEMORY_HARD_CAP as u64)
        .all(&state.db)
        .await
        .map_err(|e| CrawlerError::Config(format!("查询记忆失败: {e}")))?;
    Ok(rows.into_iter().map(Into::into).collect())
}

/// 手动新增一条记忆(source=manual,默认启用、高置信)。mem_type/importance 可选(缺省 other / 3)。
/// scope 默认为 global,scope_id 默认为空。
#[tauri::command]
pub async fn add_chat_memory(
    state: State<'_, AppState>,
    content: String,
    mem_type: Option<String>,
    importance: Option<i32>,
    scope: Option<String>,
    scope_id: Option<String>,
) -> Result<MemoryView> {
    let me = current_user(&state).ok_or_else(|| CrawlerError::Config("未登录".into()))?;
    let text: String = content.trim().chars().take(MAX_MEMORY_ITEM_CHARS).collect();
    if text.is_empty() {
        return Err(CrawlerError::Config("记忆内容为空".into()));
    }
    let now = Utc::now().timestamp();
    let saved = mem::ActiveModel {
        owner: Set(me.name),
        scope: Set(scope.unwrap_or_else(|| "global".to_string())),
        scope_id: Set(scope_id.unwrap_or_default()),
        content: Set(text),
        source: Set("manual".to_string()),
        enabled: Set(true),
        pinned: Set(false),
        mem_type: Set(normalize_type(mem_type.as_deref())),
        importance: Set(clamp_score(importance.unwrap_or(3))),
        confidence: Set(5), // 用户手动录入,视为高置信
        hit_count: Set(0),
        last_hit_at: Set(0),
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

/// 更新一条记忆(归属校验)。mem_type/importance 可选(传则更新,不传保留原值)。
#[tauri::command]
pub async fn update_chat_memory(
    state: State<'_, AppState>,
    id: i64,
    content: String,
    enabled: bool,
    mem_type: Option<String>,
    importance: Option<i32>,
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
    if let Some(t) = mem_type {
        am.mem_type = Set(normalize_type(Some(&t)));
    }
    if let Some(imp) = importance {
        am.importance = Set(clamp_score(imp));
    }
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
/// scope 和 scope_id 用于过滤特定作用域的记忆,同时包含全局记忆。
///
/// 检索策略:置顶记忆恒注入;其余按与 `query` 的余弦相似度取 top-K。
/// 未配置 embedding / 查询为空 / 查询向量化失败时,回退到「最近更新优先」(旧行为,零破坏)。
pub async fn memory_system_message(
    db: &DatabaseConnection,
    owner: &str,
    query: &str,
    scope: &str,
    scope_id: &str,
) -> Option<Value> {
    if !memory_enabled(db).await {
        return None;
    }
    // 一次性载入该用户全部启用记忆(≤ MEMORY_HARD_CAP=200,内存暴力 cosine 足够快)
    // 同时包含全局记忆和特定作用域的记忆
    let rows = mem::Entity::find()
        .filter(mem::Column::Owner.eq(owner))
        .filter(mem::Column::Enabled.eq(true))
        .filter(
            mem::Column::Scope.eq("global")
                .or(mem::Column::Scope.eq(scope).and(mem::Column::ScopeId.eq(scope_id)))
        )
        .order_by_desc(mem::Column::UpdatedAt)
        .limit(MEMORY_HARD_CAP as u64)
        .all(db)
        .await
        .ok()?;
    if rows.is_empty() {
        return None;
    }
    let selected = select_memories(db, &rows, query).await;
    let (message, injected_ids) = build_memory_message(&selected)?;
    // 只对真正进入 prompt 的记忆记命中(被字符预算截断的不算),避免污染综合排序
    spawn_record_hits(db, injected_ids, Utc::now().timestamp());
    Some(message)
}

/// 选出本轮要注入的记忆:置顶 + 身份类恒注入;其余按综合评分(相似度+重要度+时间衰减+命中)取 top-K。
/// 选中的记忆异步累加命中、刷新命中时间(供下次排序与淘汰)。
async fn select_memories<'a>(
    db: &DatabaseConnection,
    rows: &'a [mem::Model],
    query: &str,
) -> Vec<&'a mem::Model> {
    let now = Utc::now().timestamp();
    // 恒注入:置顶(用户显式要求,不设上限)+ 身份类(称呼 / 职业等)。
    // identity 做数量上限:按综合分取头部,其余退回 rest 按相关度竞争,
    // 避免自动分类堆积的 identity 把按相关度选出的记忆挤出字符预算。
    let mut identity_ranked: Vec<&mem::Model> = rows
        .iter()
        .filter(|m| !m.pinned && m.mem_type == "identity")
        .collect();
    identity_ranked.sort_by(|a, b| {
        composite_score(0.0, b, now)
            .partial_cmp(&composite_score(0.0, a, now))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    identity_ranked.truncate(ALWAYS_IDENTITY_CAP);
    let always_ids: std::collections::HashSet<i64> = rows
        .iter()
        .filter(|m| m.pinned)
        .map(|m| m.id)
        .chain(identity_ranked.iter().map(|m| m.id))
        .collect();
    let always: Vec<&mem::Model> =
        rows.iter().filter(|m| always_ids.contains(&m.id)).collect();
    let rest: Vec<&mem::Model> =
        rows.iter().filter(|m| !always_ids.contains(&m.id)).collect();

    // 非恒注入项本就 ≤ 额度:全注入即可,省掉 embedding 往返
    let selected_rest: Vec<&mem::Model> = if rest.len() <= TOP_K_INJECT {
        rest
    } else {
        // query 向量(未配 embedding / query 空 / 失败 → 无相似度项,仅按重要度+时间+命中排序)
        let qvec = compute_query_vec(db, query).await;
        let mut scored: Vec<(f32, &mem::Model)> = Vec::with_capacity(rest.len());
        let mut missing: Vec<i64> = Vec::new();
        for m in &rest {
            let sim = match &qvec {
                Some((qv, model)) => {
                    let usable = m.embed_model.as_deref() == Some(model.as_str());
                    match (usable, parse_embedding(m)) {
                        (true, Some(v)) if v.len() == qv.len() => {
                            crate::llm::embedding::cosine(qv, &v)
                        }
                        _ => {
                            missing.push(m.id);
                            0.0
                        }
                    }
                }
                None => 0.0,
            };
            scored.push((composite_score(sim, m, now), m));
        }
        if !missing.is_empty() {
            spawn_backfill(db, missing);
        }
        scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        scored.into_iter().take(TOP_K_INJECT).map(|(_, m)| m).collect()
    };

    let mut out = always;
    out.extend(selected_rest);
    // 命中记录移到 build_memory_message 之后:那里才按字符预算确定真正进入 prompt 的记忆,
    // 在此按整个候选集记 hit 会把没注入的记忆也算命中,污染后续排序(见 memory_system_message)。
    out
}

/// 综合评分:语义相似 + 重要度 + 时间衰减(久未命中降权)+ 命中频次。各项归一到约 0..1 再加权。
fn composite_score(sim: f32, m: &mem::Model, now: i64) -> f32 {
    let imp = (m.importance.clamp(1, 5) as f32 - 1.0) / 4.0;
    let last = m.last_hit_at.max(m.created_at);
    let age = (now - last).max(0) as f32;
    let recency = 0.5_f32.powf(age / RECENCY_HALF_LIFE_SECS);
    let hit = (m.hit_count as f32 / 10.0).min(1.0);
    sim * W_SIM + imp * W_IMP + recency * W_REC + hit * W_HIT
}

/// query 向量化的最大等待(秒):它在对话关键路径上(模型回复前),embedding 端点慢/不可用时
/// 不能拖住每条对话——超时即放弃相似度,仅按重要度+时间+命中排序(降级不报错)。
const QUERY_EMBED_TIMEOUT_SECS: u64 = 4;

/// 算 query 的 embedding 向量 + 所用模型;未配 / query 空 / 失败 / 超时返回 None。
async fn compute_query_vec(db: &DatabaseConnection, query: &str) -> Option<(Vec<f32>, String)> {
    let q = query.trim();
    if q.is_empty() {
        return None;
    }
    let (api_url, model, api_key) = embedding_config(db).await?;
    // inputs 命名绑定:让切片活到函数末尾,否则 embed_texts 返回的 future 借用的临时数组会被提前释放
    let inputs = [q.to_string()];
    let fut = crate::llm::embedding::embed_texts(&api_url, &api_key, &model, &inputs);
    // 加短超时:超时(外层 Err)或接口失败(内层 Err)都回退到无相似度排序
    let mut vecs = tokio::time::timeout(
        std::time::Duration::from_secs(QUERY_EMBED_TIMEOUT_SECS),
        fut,
    )
    .await
    .ok()?
    .ok()?;
    let vec = vecs.pop()?;
    Some((vec, model))
}

/// 异步累加命中次数、刷新最后命中时间(被注入即视为一次命中)。失败仅告警。
fn spawn_record_hits(db: &DatabaseConnection, ids: Vec<i64>, now: i64) {
    if ids.is_empty() {
        return;
    }
    let db = db.clone();
    tauri::async_runtime::spawn(async move {
        use sea_orm::sea_query::Expr;
        if let Err(e) = mem::Entity::update_many()
            .col_expr(mem::Column::HitCount, Expr::col(mem::Column::HitCount).add(1))
            .col_expr(mem::Column::LastHitAt, Expr::value(now))
            .filter(mem::Column::Id.is_in(ids))
            .exec(&db)
            .await
        {
            tracing::warn!("更新记忆命中失败: {e}");
        }
    });
}

/// 归一化记忆分类:已知类原样,未知 / None → other。
fn normalize_type(t: Option<&str>) -> String {
    match t {
        Some(s) if MEM_TYPES.contains(&s) => s.to_string(),
        _ => "other".to_string(),
    }
}

/// 重要度 / 置信度评分钳到 1-5。
fn clamp_score(v: i32) -> i32 {
    v.clamp(1, 5)
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
/// 返回 (system 消息, 真正进入 prompt 的记忆 id):后者用于只对实际注入项记命中。
fn build_memory_message(selected: &[&mem::Model]) -> Option<(Value, Vec<i64>)> {
    let mut lines = String::new();
    let mut used = 0usize;
    let mut injected: Vec<i64> = Vec::new();
    for r in selected {
        let line = r.content.trim();
        if line.is_empty() {
            continue;
        }
        let len = line.chars().count();
        // 放不下就跳过这条、继续尝试后面更短的;不能 break,否则其后(可能更相关)记忆被整体丢弃
        if used + len > MAX_MEMORY_INJECT_CHARS {
            continue;
        }
        used += len;
        injected.push(r.id);
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
    Some((json!({ "role": "system", "content": content }), injected))
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

/// 智能维护:从本轮对话提取信息,对照「相关已有记忆」判断新增 / 更新 / 跳过,带分类与打分落库。
/// 根治「只增不治」导致的碎片化与矛盾。失败仅告警,设计为在 spawn 中调用,不阻塞回复。
/// scope 和 scope_id 用于记忆层级化,默认为 global。
#[allow(clippy::too_many_arguments)]
pub async fn extract_and_store_memories(
    db: &DatabaseConnection,
    owner: &str,
    api_url: &str,
    api_key: &str,
    model: &str,
    user_text: &str,
    assistant_text: &str,
    scope: &str,
    scope_id: &str,
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
            tracing::warn!("读取记忆失败,跳过本轮维护: {e}");
            return;
        }
    };

    // 检索与本轮最相关的已有记忆,作为 LLM 判断去重 / 更新的依据
    let relevant = retrieve_relevant(db, &existing, user_text).await;
    let Some(ops) =
        call_maintainer(api_url, api_key, model, user_text, assistant_text, &relevant).await
    else {
        return;
    };
    if ops.is_empty() {
        return;
    }

    let owned_ids: HashSet<i64> = existing.iter().map(|m| m.id).collect();
    let now = Utc::now().timestamp();

    // 先处理 update(更新已有,不增条数);id 非法 / 非本人的退化为 add
    let mut pending_adds: Vec<MemOp> = Vec::new();
    // 本轮刚更新的 id:淘汰时要排除——existing 是更新前的快照,这些行的综合分仍是旧低分,
    // 不排除会把 LLM 明确选择保留 / 刷新的记忆当成低分淘汰掉
    let mut updated_ids: HashSet<i64> = HashSet::new();
    for op in ops {
        if op.op == "update" {
            match op.id {
                Some(id) if owned_ids.contains(&id) => {
                    apply_update(db, id, &op, now).await;
                    updated_ids.insert(id);
                }
                _ => pending_adds.push(op),
            }
        } else {
            pending_adds.push(op);
        }
    }
    if pending_adds.is_empty() {
        return;
    }

    // add 去重:与已有内容规范化精确匹配的跳过(LLM 偶尔仍会重复)
    let mut seen: HashSet<String> = existing.iter().map(|m| normalize_key(&m.content)).collect();
    let mut adds: Vec<MemOp> = Vec::new();
    for op in pending_adds {
        let key = normalize_key(&op.content);
        if key.is_empty() || seen.contains(&key) {
            continue;
        }
        seen.insert(key);
        adds.push(op);
    }
    if adds.is_empty() {
        return;
    }

    // 满上限:按综合分淘汰最低的旧记忆腾位(候选排除置顶 / identity / 本轮刚更新)
    let overflow = (existing.len() + adds.len()).saturating_sub(MEMORY_HARD_CAP);
    if overflow > 0 {
        let freed = evict_lowest(db, &existing, overflow, now, &updated_ids).await;
        // 淘汰不足(候选全是置顶 / identity / 刚更新)→ 硬上限优先:截断新增,
        // 否则记忆表会突破 MEMORY_HARD_CAP 无界增长,且恒注入项吃满预算挤掉相关记忆
        if freed < overflow {
            let keep = adds.len().saturating_sub(overflow - freed);
            if keep < adds.len() {
                tracing::warn!(
                    "记忆达上限 {} 且可淘汰项不足,丢弃 {} 条新增",
                    MEMORY_HARD_CAP,
                    adds.len() - keep
                );
                adds.truncate(keep);
            }
            if adds.is_empty() {
                return;
            }
        }
    }

    // 落库即生成向量(best-effort):配了 embedding 就内联存,未配 / 失败则留空,后续按需补算
    let contents: Vec<String> = adds.iter().map(|o| o.content.clone()).collect();
    let embeds: Option<(String, Vec<Vec<f32>>)> = match embedding_config(db).await {
        Some((url, emodel, key)) => {
            match crate::llm::embedding::embed_texts(&url, &key, &emodel, &contents).await {
                Ok(v) if v.len() == contents.len() => Some((emodel, v)),
                _ => None,
            }
        }
        None => None,
    };

    let mut to_insert: Vec<mem::ActiveModel> = Vec::with_capacity(adds.len());
    for (i, op) in adds.into_iter().enumerate() {
        let (embedding, embed_model) = match &embeds {
            Some((emodel, vecs)) => (
                Some(serde_json::to_string(&vecs[i]).unwrap_or_default()),
                Some(emodel.clone()),
            ),
            None => (None, None),
        };
        to_insert.push(mem::ActiveModel {
            owner: Set(owner.to_string()),
            scope: Set(scope.to_string()),
            scope_id: Set(scope_id.to_string()),
            content: Set(op.content),
            source: Set("auto".to_string()),
            enabled: Set(true),
            pinned: Set(false),
            mem_type: Set(op.mem_type),
            importance: Set(op.importance),
            confidence: Set(op.confidence),
            hit_count: Set(0),
            last_hit_at: Set(0),
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

/// 一条记忆维护操作(LLM 输出解析得到)。
struct MemOp {
    op: String,
    id: Option<i64>,
    content: String,
    mem_type: String,
    importance: i32,
    confidence: i32,
}

/// 更新已有记忆:内容 / 分类 / 打分覆盖,清空旧向量后异步重算。失败仅告警。
async fn apply_update(db: &DatabaseConnection, id: i64, op: &MemOp, now: i64) {
    let Ok(Some(row)) = mem::Entity::find_by_id(id).one(db).await else {
        return;
    };
    let mut am = row.into_active_model();
    am.content = Set(op.content.clone());
    am.mem_type = Set(op.mem_type.clone());
    am.importance = Set(op.importance);
    am.confidence = Set(op.confidence);
    am.embedding = Set(None);
    am.embed_model = Set(None);
    am.updated_at = Set(now);
    if let Err(e) = am.update(db).await {
        tracing::warn!("更新记忆失败: {e}");
        return;
    }
    spawn_backfill(db, vec![id]);
}

/// 淘汰综合分最低的若干条记忆(跳过置顶 / 身份类 / 本轮刚更新),给新记忆腾位。返回实际淘汰条数。
async fn evict_lowest(
    db: &DatabaseConnection,
    existing: &[mem::Model],
    count: usize,
    now: i64,
    exclude: &HashSet<i64>,
) -> usize {
    let mut cands: Vec<&mem::Model> = existing
        .iter()
        .filter(|m| !m.pinned && m.mem_type != "identity" && !exclude.contains(&m.id))
        .collect();
    // 无 query 语境,sim 记 0,仅按重要度 + 时间 + 命中排序;升序取最低 count 个
    cands.sort_by(|a, b| {
        composite_score(0.0, a, now)
            .partial_cmp(&composite_score(0.0, b, now))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let ids: Vec<i64> = cands.into_iter().take(count).map(|m| m.id).collect();
    if ids.is_empty() {
        return 0;
    }
    let n = ids.len();
    if let Err(e) = mem::Entity::delete_many()
        .filter(mem::Column::Id.is_in(ids))
        .exec(db)
        .await
    {
        tracing::warn!("淘汰旧记忆失败: {e}");
        return 0;
    }
    n
}

/// 检索与本轮最相关的已有记忆(给 LLM 作去重 / 更新依据)。
/// 配了 embedding → 按 query 余弦 top-N;否则取最近更新的 N 条。条数 ≤ N 时全给。
async fn retrieve_relevant<'a>(
    db: &DatabaseConnection,
    existing: &'a [mem::Model],
    query: &str,
) -> Vec<&'a mem::Model> {
    if existing.len() <= MAINTAIN_CONTEXT_N {
        return existing.iter().collect();
    }
    if let Some((qv, model)) = compute_query_vec(db, query).await {
        let mut scored: Vec<(f32, &mem::Model)> = existing
            .iter()
            .map(|m| {
                let sim = if m.embed_model.as_deref() == Some(model.as_str()) {
                    match parse_embedding(m) {
                        Some(v) if v.len() == qv.len() => crate::llm::embedding::cosine(&qv, &v),
                        _ => -1.0,
                    }
                } else {
                    -1.0
                };
                (sim, m)
            })
            .collect();
        scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        scored.into_iter().take(MAINTAIN_CONTEXT_N).map(|(_, m)| m).collect()
    } else {
        let mut sorted: Vec<&mem::Model> = existing.iter().collect();
        sorted.sort_by_key(|m| std::cmp::Reverse(m.updated_at));
        sorted.into_iter().take(MAINTAIN_CONTEXT_N).collect()
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

/// 调 LLM 做记忆维护:喂本轮对话 + 相关已有记忆,解析出操作数组;任何失败返回 None。
async fn call_maintainer(
    api_url: &str,
    api_key: &str,
    model: &str,
    user_text: &str,
    assistant_text: &str,
    relevant: &[&mem::Model],
) -> Option<Vec<MemOp>> {
    let user: String = user_text.chars().take(2000).collect();
    let assistant: String = assistant_text.chars().take(2000).collect();
    let existing_block = if relevant.is_empty() {
        "(无)".to_string()
    } else {
        relevant
            .iter()
            .map(|m| format!("[{}] {}", m.id, m.content))
            .collect::<Vec<_>>()
            .join("\n")
    };
    let prompt = format!(
        "{MAINTAIN_PROMPT}\n\n已有相关记忆:\n{existing_block}\n\n本轮对话:\n用户:{user}\n助手:{assistant}"
    );
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
    .ok()?
    .content;
    parse_operations(&reply)
}

/// 解析模型返回为操作数组:容错去代码块,截取首个 `[` 到末个 `]` 再解析。
fn parse_operations(reply: &str) -> Option<Vec<MemOp>> {
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
    let mut ops = Vec::new();
    for v in arr {
        let content: String = v
            .get("content")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .chars()
            .take(MAX_MEMORY_ITEM_CHARS)
            .collect();
        if content.is_empty() {
            continue;
        }
        let op = v.get("op").and_then(Value::as_str).unwrap_or("add").to_string();
        let id = v.get("id").and_then(Value::as_i64);
        let mem_type = normalize_type(v.get("type").and_then(Value::as_str));
        let importance =
            clamp_score(v.get("importance").and_then(Value::as_i64).unwrap_or(3) as i32);
        let confidence =
            clamp_score(v.get("confidence").and_then(Value::as_i64).unwrap_or(3) as i32);
        ops.push(MemOp {
            op,
            id,
            content,
            mem_type,
            importance,
            confidence,
        });
    }
    Some(ops)
}
