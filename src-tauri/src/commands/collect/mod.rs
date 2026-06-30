//! 采集执行引擎:从前端触发采集,到落库 / 媒体下载 / 语音转写 / 意向分析 /
//! Obsidian 同步的完整流水线编排。
//!
//! 设计分层:控制面(应用状态、系统配置、账号 / 平台命令)留在 `commands` 模块根;
//! 此处只承载「数据面」——采集任务的执行调度与持久化,逻辑虽长但内聚单一职责。

use super::{account_collect_lock, current_user, get_secret, lock_config, AppState};
use crate::adapter::{FetchContext, FetchOutput};
use crate::cookie::CookiePool;
use crate::model::{Author, Comment, Content, ContentKind, TaskKind};
use crate::webview::pool::{
    CollectBridge, CollectRequest, CollectStop, CommentCollectRequest, DetailFetchRequest,
};
use crate::webview::{emit_collect_entry, emit_collect_log, CollectEntry, RpaOutcome};
use chrono::Utc;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, IntoActiveModel, QueryFilter,
    QueryOrder, Set,
};
use serde::Serialize;
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, State};
use veltrix_core::error::{CrawlerError, Result};
use veltrix_core::db::entity::provider as provider_entity;
use crate::webview::slider::VisionProvider;
mod obsidian;
mod enrich;
pub use obsidian::*;
pub use enrich::*;

/// 从 providers 表查找第一个具备 vision 能力的模型,用于滑块验证码自动识别。
/// None=未配置视觉模型(不尝试自动滑块)。
async fn resolve_vision_provider(db: &DatabaseConnection) -> Option<VisionProvider> {
    let rows = provider_entity::Entity::find()
        .all(db)
        .await
        .ok()?;
    for row in rows {
        // models 字段是 JSON 数组,格式: [{"name":"xxx","capabilities":["text","vision"]}]
        if let Ok(models) = serde_json::from_str::<Vec<serde_json::Value>>(&row.models) {
            for m in &models {
                let caps = m.get("capabilities")
                    .and_then(|c| c.as_array())
                    .map(|a| a.iter().any(|v| v.as_str() == Some("vision")))
                    .unwrap_or(false);
                if caps {
                    let model_name = m.get("name")
                        .and_then(|n| n.as_str())
                        .unwrap_or("")
                        .to_string();
                    if !model_name.is_empty() && !row.api_key.is_empty() {
                        tracing::info!(
                            "视觉模型(自动滑块用):厂商「{}」模型「{model_name}」",
                            row.name
                        );
                        return Some(VisionProvider {
                            api_url: row.api_url.clone(),
                            api_key: row.api_key.clone(),
                            model: model_name,
                        });
                    }
                }
            }
        }
    }
    tracing::warn!("视觉模型(自动滑块用):providers 表里没有「已勾图片能力且配了密钥」的模型,自动滑块将不可用");
    None
}

/// 关键词采集阶段的共享状态,由 run_task_body 维护,传递给子阶段函数。
struct CollectSharedState {
    seen_contents: HashSet<String>,
    seen_comments: HashSet<String>,
    intercepted_total: i64,
    had_error: bool,
    contents_for_media: Vec<Content>,
    /// 采集途中用户手动关闭采集窗口 → 取消任务标记(见 run_task_body 据此收尾,不再后处理)。
    window_closed: bool,
    /// 采集途中用户点了 HUD「结束」→ 停止后续关键词/评论,但仍下载已采素材并正常完成。
    user_ended: bool,
}

/// 素材下载阶段的配置参数。
struct MediaDownloadParams<'a> {
    app: &'a AppHandle,
    db: &'a DatabaseConnection,
    task_id: &'a str,
    platform: &'a str,
    account_id: &'a str,
    config_dir: &'a PathBuf,
    media_cfg: &'a veltrix_core::config::MediaConfig,
    transcription_cfg: &'a veltrix_core::config::TranscriptionConfig,
    ai_extract: bool,
}

/// 直链补取阶段的配置参数。
struct StreamRefreshParams<'a> {
    app: &'a AppHandle,
    bridge: &'a CollectBridge,
    registry: &'a crate::adapter::AdapterRegistry,
    db: &'a DatabaseConnection,
    cfg: &'a veltrix_core::config::PlatformConfig,
    account_id: &'a str,
    task_id: &'a str,
}

/// 页面内拦截 hook 调用本命令回传一条命中的接口响应。
/// 字段命名与注入脚本中的 invoke 一致(camelCase: sessionId/url/body)。
#[tauri::command]
pub fn intercept_push(state: State<'_, AppState>, session_id: u64, url: String, body: String) {
    state.intercept_channel.push(session_id, url, body);
}

/// HUD「结束」按钮回传:请求停止采集。任务采集传 task_id(跨关键词稳定),联调单采传 session_id。
/// 两者都登记:session 用于当前关键词滚动循环即时停止,task 用于关键词切换时终止整任务
/// (避免在关键词空档点结束落到已结束的旧会话上而漏判)。
#[tauri::command]
pub fn stop_collect(
    state: State<'_, AppState>,
    session_id: Option<u64>,
    task_id: Option<String>,
) {
    if let Some(sid) = session_id {
        state.collect_control.request_stop(sid);
    }
    if let Some(tid) = task_id.filter(|t| !t.is_empty()) {
        state.collect_control.request_stop_task(&tid);
    }
}

/// 采集窗口验证弹窗自检回传:页面检测到 / 解除安全验证弹窗时上报。
/// 采集循环据此暂停 / 恢复滚动;并向前端推送 `collect-verify` 事件,便于主界面提示用户去窗口手动验证。
#[tauri::command]
pub fn report_collect_verify(
    app: AppHandle,
    state: State<'_, AppState>,
    session_id: u64,
    present: bool,
) {
    use tauri::Emitter;
    tracing::info!("验证检测:report_collect_verify session={session_id} present={present}");
    state.collect_control.set_verifying(session_id, present);
    // 推送状态 + sessionId;前端按 session 维护「待验证」集合,任一存在即显示全局提示条
    let _ = app.emit(
        "collect-verify",
        serde_json::json!({ "present": present, "sessionId": session_id }),
    );
}

/// 拟人 RPA 执行器跑完(或某步失败)时回传结果。
/// 字段与注入脚本一致(camelCase: runId/ok/failedStep/message)。
#[tauri::command]
pub fn rpa_done(
    state: State<'_, AppState>,
    run_id: u64,
    ok: bool,
    failed_step: i64,
    message: String,
) {
    state.rpa_channel.complete(
        run_id,
        RpaOutcome {
            ok,
            failed_step,
            message,
        },
    );
}

/// 一次采集的结果。`urls` 暴露命中的接口便于联调核对 `intercept_patterns`。
#[derive(Debug, Serialize)]
pub struct CollectResult {
    /// 拦截到的接口响应数量。
    pub intercepted: usize,
    /// 命中的接口 URL 列表。
    pub urls: Vec<String>,
    pub contents: Vec<Content>,
    pub comments: Vec<Comment>,
}

/// 用关键词在指定账号的可见 WebView 内执行一次 RPA 采集。
///
/// 流程:复用登录态窗口 → 导航搜索页 → 拦截接口响应 → 交平台适配器解析为统一模型。
/// 未注册该平台适配器时不报错,仅返回拦截到的原始接口信息,供联调阶段验证拦截链路。
#[tauri::command]
pub async fn start_collect(
    state: State<'_, AppState>,
    app: AppHandle,
    platform: String,
    keyword: String,
    account_id: String,
) -> Result<CollectResult> {
    // 先 clone 出平台配置,避免把配置锁的 guard 跨 await 持有
    let cfg = { lock_config(&state)?.platform(&platform)?.clone() };

    // 联调单采也竞争同账号互斥锁,避免与正在运行的任务并发驱动同一窗口互踩
    let account_lock =
        account_collect_lock(&state.collect_locks, &format!("{}-{account_id}", cfg.id));
    let _collect_guard = account_lock.lock().await;

    let bridge = CollectBridge::new(
        state.webviews.clone(),
        state.intercept_channel.clone(),
        state.rpa_channel.clone(),
        state.collect_control.clone(),
    );
    let outcome = bridge
        .collect(
            &app,
            CollectRequest {
                account_id: &account_id,
                keyword: &keyword,
                // 联调单采无任务,窗口标题回退账号 id(setup_collect_session 内部处理空值)
                task_name: "",
                platform_cfg: &cfg,
                task_id: None,
                // 联调单采:不设目标数量,退回固定轮数盲滚
                target_count: 0,
                adapter: None,
                // 联调单采不增量落库,行为不变
                content_tx: None,
                existing_ids: None,
                sort_mode: "",
                time_range: "",
                // 联调单采不按点赞过滤
                min_likes: 0,
                // 联调单采不按黑名单过滤
                blacklisted_uids: None,
                // 联调单采无额外筛选
                extra_filters: &[],
                vision_provider: resolve_vision_provider(&state.db).await,
            },
        )
        .await;
    // 联调单采:中途出错直接上报(此路径不落库,无需保留部分响应)
    if let Some(e) = outcome.error {
        return Err(e);
    }
    let responses = outcome.responses;

    let intercepted = responses.len();
    let urls = responses.iter().map(|r| r.url.clone()).collect();

    // 有适配器则解析为统一模型;暂未注册时降级为只返回原始拦截信息
    let (contents, comments) = match state.registry.get(&platform) {
        Ok(adapter) => {
            let ctx = FetchContext { keyword, responses };
            let output = adapter.parse(&TaskKind::Search, &ctx).await?;
            (output.contents, output.comments)
        }
        Err(_) => (Vec::new(), Vec::new()),
    };

    Ok(CollectResult {
        intercepted,
        urls,
        contents,
        comments,
    })
}

/// 启动一个任务的采集:选该平台一个可用账号,后台遍历关键词逐个采集(自动开窗 + 拟人 RPA),
/// 命令立即返回,采集在后台进行,前端轮询 `list_tasks` 看进度。
///
/// 当前为最小闭环:`content_count` 暂记拦截到的接口响应数(非真实内容数),待解析落库后修正。
#[tauri::command]
pub async fn run_task(
    state: State<'_, AppState>,
    app: AppHandle,
    task_id: String,
) -> Result<()> {
    // entity 名与本模块 `mod task` 同名,别名规避冲突
    use veltrix_core::db::entity::task as task_entity;

    let model = task_entity::Entity::find_by_id(task_id.clone())
        .one(&state.db)
        .await
        .map_err(|e| CrawlerError::Config(format!("查询任务失败: {e}")))?
        .ok_or_else(|| CrawlerError::Config(format!("任务不存在: {task_id}")))?;

    // 防重复启动:任务已在进行中(双击「运行」/ 前端状态滞后)时再 spawn 一份采集,
    // 两份会写同一任务的进度与状态互相覆盖,直接拒绝
    if matches!(
        model.status.as_str(),
        "running" | "collecting_comments" | "analyzing_comments" | "downloading_media"
    ) {
        return Err(CrawlerError::Config("任务正在进行中,请勿重复启动".into()));
    }

    let platform = model.platform.clone();
    let owner = model.owner.clone();
    let keywords: Vec<String> = serde_json::from_str(&model.keywords).unwrap_or_default();
    if keywords.is_empty() {
        return Err(CrawlerError::Config("任务未配置关键词".into()));
    }

    // 选该平台一个可用账号并占用(轮换「最久未用」+ 乐观 CAS,自动恢复冷却到期账号);
    // account_id 作为采集窗口的隔离 key(对应独立 WebView2 数据目录)。
    // 改用 acquire 替代「永远取第一个 active」,真正实现多账号负载分摊与风控分压。
    let account = state.cookies.acquire(&platform).await.map_err(|_| {
        CrawlerError::Config(format!("平台 {platform} 无可用账号,请先在账号管理添加并登录"))
    })?;
    let account_id = account.id;

    // 采集窗口标题用任务名(平台名称 - 任务名称);任务名为空时回退账号 id,保证可辨识。
    // 必须在 model 被 into_active_model 消费前取出。
    let task_name = if model.name.trim().is_empty() {
        account_id.clone()
    } else {
        model.name.clone()
    };

    // clone 出平台配置,避免把配置锁 guard 跨 await 持有
    let cfg = { lock_config(&state)?.platform(&platform)?.clone() };

    // 媒体下载所需:存储配置与配置目录(用于解析素材根目录)。
    // 在 spawn 前 clone 出来 move 进后台任务,避免跨 await 持有配置锁。
    let media_cfg = { lock_config(&state)?.media.clone() };
    let config_dir = state.config_dir.clone();

    // 意向分析配置(provider/prompt 引用 + 模型 + 批大小);clone 出来 move 进后台任务
    let intent_cfg = { lock_config(&state)?.intent.clone() };
    // 语音转写配置(厂商引用 + 模型);clone 出来 move 进后台任务,采集结束后转写用
    let transcription_cfg = { lock_config(&state)?.transcription.clone() };

    // 每关键词目标数量:作为滚动「按量停止」的依据(<=0 视为不限,退回固定轮数盲滚)
    let per_keyword_limit = model.per_keyword_limit.max(0) as usize;
    // 最低点赞数:采集时过滤,点赞数低于此值的内容不计目标数、不落库(0=不限)
    let min_likes = model.min_likes.max(0);

    // 评论采集参数(model 即将被 into_active_model 消费,先取出 move 进后台任务)
    let collect_comments = model.collect_comments;
    let comment_time_range = model.comment_time_range.clone();
    let comment_limit = model.comment_limit.max(0) as usize;
    let analyze_comment_intent = model.analyze_comment_intent;
    // AI 文案提取:开 → 视频下载并转音频(供转写);关 → 视频不下载、不存储
    let ai_extract = model.ai_extract;
    // 采集完成后是否自动同步到发起者(owner)的 Obsidian vault
    let auto_sync_obsidian = model.auto_sync_obsidian;
    // 排序方式 / 发布时间:采集时在结果页做 RPA 文案点击筛选
    let sort_mode = model.sort_mode.clone();
    let time_range = model.time_range.clone();
    // 平台专属额外筛选(抖音:视频时长/搜索范围/内容形式):取对象里非空的选中文案,
    // 采集时在结果页「筛选」浮层逐个点击应用("any"/空视为不限,跳过)
    let extra_filter_clicks: Vec<String> =
        serde_json::from_str::<serde_json::Value>(&model.extra_filters)
            .ok()
            .and_then(|v| v.as_object().cloned())
            .map(|obj| {
                obj.values()
                    .filter_map(|v| v.as_str())
                    .filter(|s| !s.is_empty() && *s != "any")
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();

    // 先标记 running + started_at,前端立即看到状态翻转
    let now = Utc::now().timestamp();
    let mut am = model.into_active_model();
    am.status = Set("running".to_string());
    am.started_at = Set(Some(now));
    am.progress = Set(0);
    am.updated_at = Set(now);
    let started = am
        .update(&state.db)
        .await
        .map_err(|e| CrawlerError::Config(format!("更新任务状态失败: {e}")))?;
    // 启动瞬间推送一次,前端立即翻转为「运行中」并据此开启轮询兜底
    emit_task_progress(&app, started);

    // 后台采集,不阻塞命令返回。句柄均为 Clone/Arc,可安全 move 进 spawn
    let db = state.db.clone();
    let registry = state.registry.clone();
    let collect_locks = state.collect_locks.clone();
    // 账号池:采集结束据产出回写账号健康度(成功 release_ok 清风控计数 / 整体失败 mark_risk 冷却)
    let cookies = state.cookies.clone();
    // 全局采集并发闸:限制同时占用 WebView 窗口的任务数,防调度同点拉起多任务时爆窗耗尽资源
    let collect_semaphore = state.collect_semaphore.clone();
    let bridge = CollectBridge::new(
        state.webviews.clone(),
        state.intercept_channel.clone(),
        state.rpa_channel.clone(),
        state.collect_control.clone(),
    );
    // panic 兜底所需:任务体 panic 时仍能把任务落终态(否则永久卡「运行中」)
    let app_guard = app.clone();
    let db_guard = db.clone();
    let task_id_guard = task_id.clone();
    tauri::async_runtime::spawn(async move {
        // 任务体整体包一层 catch_unwind:解析 / 落库 / eval 任一处 panic 不再让 future 静默消失、
        // 任务永久停在「运行中」。捕获后统一落 failed,让调度页可见并允许重跑。
        use futures_util::FutureExt;
        let body = std::panic::AssertUnwindSafe(run_task_body(RunTaskCtx {
            app: app.clone(),
            db: db.clone(),
            cookies,
            collect_semaphore,
            collect_locks,
            bridge,
            registry,
            cfg,
            account_id,
            task_id: task_id.clone(),
            task_name,
            owner,
            keywords,
            per_keyword_limit,
            min_likes,
            collect_comments,
            comment_time_range,
            comment_limit,
            analyze_comment_intent,
            ai_extract,
            auto_sync_obsidian,
            sort_mode,
            time_range,
            extra_filter_clicks,
            media_cfg,
            config_dir,
            intent_cfg,
            transcription_cfg,
            run_started_at: now,
        }));
        if body.catch_unwind().await.is_err() {
            tracing::error!(task_id = %task_id_guard, "采集任务 panic,已落 failed");
            write_task_failed(
                &app_guard,
                &db_guard,
                &task_id_guard,
                "采集任务内部错误(已中断),可重新运行",
            )
            .await;
        }
    });

    Ok(())
}

/// `run_task` 后台任务体的入参集合。字段较多,聚成结构体避免超长函数签名。
struct RunTaskCtx {
    app: AppHandle,
    db: DatabaseConnection,
    cookies: Arc<CookiePool>,
    collect_semaphore: Arc<tokio::sync::Semaphore>,
    collect_locks: Arc<Mutex<std::collections::HashMap<String, Arc<tokio::sync::Mutex<()>>>>>,
    bridge: CollectBridge,
    registry: crate::adapter::AdapterRegistry,
    cfg: veltrix_core::config::PlatformConfig,
    account_id: String,
    task_id: String,
    /// 任务名称:采集窗口标题用(平台名称 - 任务名称)。
    task_name: String,
    owner: String,
    keywords: Vec<String>,
    per_keyword_limit: usize,
    min_likes: i32,
    collect_comments: bool,
    comment_time_range: String,
    comment_limit: usize,
    analyze_comment_intent: bool,
    ai_extract: bool,
    auto_sync_obsidian: bool,
    sort_mode: String,
    time_range: String,
    /// 平台专属额外筛选的「待点击文案」列表(抖音视频时长/搜索范围/内容形式),空=无额外筛选
    extra_filter_clicks: Vec<String>,
    media_cfg: veltrix_core::config::MediaConfig,
    config_dir: PathBuf,
    intent_cfg: veltrix_core::config::CommentIntentConfig,
    transcription_cfg: veltrix_core::config::TranscriptionConfig,
    run_started_at: i64,
}

/// 增量入库消费任务:从 channel 接收批次,逐条落库 + HUD 日志 + 进度回写。
#[allow(clippy::too_many_arguments)]
fn spawn_content_consumer(
    mut rx: tokio::sync::mpsc::UnboundedReceiver<Vec<Content>>,
    db: DatabaseConnection,
    task_id: String,
    owner: String,
    keyword: String,
    app: AppHandle,
    content_seq: std::sync::Arc<std::sync::atomic::AtomicI64>,
    platform: String,
    account_id: String,
    progress: i32,
) -> tauri::async_runtime::JoinHandle<()> {
    tauri::async_runtime::spawn(async move {
        let mut seen_c_local: HashSet<String> = HashSet::new();
        let mut seen_m_local: HashSet<String> = HashSet::new();
        while let Some(batch) = rx.recv().await {
            if batch.is_empty() {
                continue;
            }
            for c in &batch {
                let seq = content_seq
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
                    + 1;
                let title = log_content_title(c);
                let likes = c.stats.like_count.unwrap_or(0);
                let msg = format!("[{seq}] {title} | 点赞:{likes}");
                crate::webview::hud_log(&app, &platform, &account_id, "info", &msg);
                emit_collect_entry(
                    &app,
                    &task_id,
                    msg,
                    CollectEntry {
                        kind: "content".to_string(),
                        seq,
                        avatar: c.author.avatar.clone(),
                        nickname: c.author.nickname.clone(),
                        title,
                        content_kind: Some(content_kind_label(&c.kind).to_string()),
                    },
                );
            }
            let output = FetchOutput {
                contents: batch,
                comments: Vec::new(),
                authors: Vec::new(),
            };
            persist_collected(&db, &task_id, &owner, &keyword, output, &mut seen_c_local, &mut seen_m_local).await;
            let c = seen_c_local.len() as i64;
            write_task_progress(&app, &db, &task_id, progress, c, 0).await;
            emit_collect_log(&app, &task_id, "info", format!("📦 「{keyword}」已保存 {c} 条内容"));
        }
    })
}

/// 关键词采集阶段:遍历关键词,逐个调 bridge.collect,增量入库+兜底解析。
/// 返回 (total_contents, total_comments)。
#[allow(clippy::too_many_arguments)]
async fn collect_keywords(
    app: &AppHandle,
    db: &DatabaseConnection,
    bridge: &CollectBridge,
    cfg: &veltrix_core::config::PlatformConfig,
    account_id: &str,
    task_id: &str,
    task_name: &str,
    owner: &str,
    keywords: &[String],
    per_keyword_limit: usize,
    min_likes: i32,
    sort_mode: &str,
    time_range: &str,
    existing_ids: &HashSet<String>,
    blacklisted_uids: &HashSet<String>,
    adapter: &Option<Arc<dyn crate::adapter::PlatformAdapter>>,
    extra_filter_clicks: &[String],
    shared: &mut CollectSharedState,
) -> (i64, i64) {
    let total = keywords.len();
    emit_collect_log(app, task_id, "info", format!("🚀 开始采集 · 共 {total} 个关键词"));
    if adapter.is_none() {
        emit_collect_log(
            app,
            task_id,
            "warn",
            format!("平台 {} 未注册适配器,仅统计拦截数,明细不落库", cfg.id),
        );
    }
    // 内容逐条日志的任务内序号(跨关键词连续);consumer 子任务共享,故用原子量
    let content_seq = std::sync::Arc::new(std::sync::atomic::AtomicI64::new(0));

    for (idx, keyword) in keywords.iter().enumerate() {
        // 用户点 HUD「结束」(按 task_id 登记,跨关键词稳定)= 终止任务:在切下个关键词、重开窗口前
        // 即拦截,彻底消除「两个关键词之间空档点结束」落到旧会话上而漏判、窗口又跳出来继续采的竞态。
        if bridge.is_task_stopping(task_id) {
            shared.user_ended = true;
            emit_collect_log(
                app,
                task_id,
                "warn",
                "🛑 已手动结束采集 · 停止后续关键词(已采数据保留,继续下载素材)".to_string(),
            );
            break;
        }
        // 用户手动关闭采集窗口 = 终止任务:不再为后续关键词重建窗口继续采集(已采数据保留)
        if bridge.is_collect_window_closed(&cfg.id, account_id) {
            shared.window_closed = true;
            emit_collect_log(
                app,
                task_id,
                "warn",
                "🛑 采集窗口已被手动关闭 · 终止任务(已采数据保留)".to_string(),
            );
            break;
        }
        let progress = (((idx + 1) as f64 / total as f64) * 100.0) as i32;
        emit_collect_log(
            app,
            task_id,
            "info",
            format!("🔍 [{}/{}] 正在搜索「{keyword}」", idx + 1, total),
        );

        // 本关键词是否被用户主动停止(点结束 / 关窗);match 两分支各自赋值,循环末统一处理
        let stop_reason: Option<CollectStop>;
        let (content_count, comment_count) = match adapter {
            Some(adapter_arc) => {
                let (tx, rx) =
                    tokio::sync::mpsc::unbounded_channel::<Vec<Content>>();
                let consumer = spawn_content_consumer(
                    rx, db.clone(), task_id.to_string(), owner.to_string(),
                    keyword.clone(), app.clone(), content_seq.clone(),
                    cfg.id.clone(), account_id.to_string(), progress,
                );

                let collect_result = bridge
                    .collect(
                        app,
                        CollectRequest {
                            account_id,
                            keyword,
                            task_name,
                            platform_cfg: cfg,
                            task_id: Some(task_id),
                            target_count: per_keyword_limit,
                            adapter: adapter.clone(),
                            content_tx: Some(tx.clone()),
                            existing_ids: Some(existing_ids),
                            sort_mode,
                            time_range,
                            min_likes,
                            blacklisted_uids: Some(blacklisted_uids),
                            extra_filters: extra_filter_clicks,
                            vision_provider: resolve_vision_provider(&db).await,
                        },
                    )
                    .await;
                stop_reason = collect_result.stop;
                if let Some(e) = &collect_result.error {
                    shared.had_error = true;
                    tracing::error!(keyword = %keyword, "采集失败: {e}");
                    emit_collect_log(
                        app,
                        task_id,
                        "error",
                        format!("❌ 「{keyword}」采集异常 · 已保留已采数据 · 原因: {e}"),
                    );
                }
                drop(tx);
                let _ = consumer.await;

                let responses = collect_result.responses;
                if !responses.is_empty() {
                    let ctx = FetchContext {
                        keyword: keyword.clone(),
                        responses,
                    };
                    match adapter_arc.parse(&TaskKind::Search, &ctx).await {
                        Ok(mut output) => {
                            if min_likes > 0 {
                                output.contents.retain(|c| {
                                    c.stats
                                        .like_count
                                        .map(|likes| likes >= min_likes as i64)
                                        .unwrap_or(true)
                                });
                            }
                            if !blacklisted_uids.is_empty() {
                                output.contents.retain(|c| {
                                    c.author.uid.is_empty()
                                        || !blacklisted_uids.contains(&c.author.uid)
                                });
                            }
                            shared.contents_for_media.extend(output.contents.iter().cloned());
                            persist_collected(
                                db,
                                task_id,
                                owner,
                                keyword,
                                output,
                                &mut shared.seen_contents,
                                &mut shared.seen_comments,
                            )
                            .await;
                        }
                        Err(e) => {
                            tracing::warn!(keyword = %keyword, "兜底解析失败: {e}");
                        }
                    }
                }

                let (c, m) = (shared.seen_contents.len() as i64, shared.seen_comments.len() as i64);
                write_task_progress(app, db, task_id, progress, c, m).await;
                emit_collect_log(
                    app,
                    task_id,
                    "info",
                    format!("📦 「{keyword}」采集完成 · 已保存 {c} 条内容 / {m} 条评论"),
                );
                crate::webview::hud_log(
                    app,
                    &cfg.id,
                    account_id,
                    "info",
                    &format!("📦 「{keyword}」已保存 · 内容 {c} / 评论 {m}"),
                );
                (c, m)
            }
            None => {
                let outcome = bridge
                    .collect(
                        app,
                        CollectRequest {
                            account_id,
                            keyword,
                            task_name,
                            platform_cfg: cfg,
                            task_id: Some(task_id),
                            target_count: per_keyword_limit,
                            adapter: None,
                            content_tx: None,
                            existing_ids: Some(existing_ids),
                            sort_mode,
                            time_range,
                            min_likes,
                            blacklisted_uids: Some(blacklisted_uids),
                            extra_filters: extra_filter_clicks,
                            vision_provider: resolve_vision_provider(&db).await,
                        },
                    )
                    .await;
                stop_reason = outcome.stop;
                if let Some(e) = &outcome.error {
                    shared.had_error = true;
                    tracing::error!(keyword = %keyword, "采集失败: {e}");
                    emit_collect_log(
                        app,
                        task_id,
                        "error",
                        format!("❌ 「{keyword}」采集异常 · 原因: {e}"),
                    );
                }
                shared.intercepted_total += outcome.responses.len() as i64;
                (shared.intercepted_total, 0)
            }
        };

        write_task_progress(app, db, task_id, progress, content_count, comment_count).await;

        // 用户在本关键词采集途中主动停止:不再为后续关键词重开窗口继续采集(已采数据已增量入库)。
        // 关窗 → 取消任务;点结束 → 停止后续关键词/评论但仍完成素材下载。两者均在此终止关键词循环。
        match stop_reason {
            Some(CollectStop::WindowClosed) => {
                shared.window_closed = true;
                emit_collect_log(
                    app,
                    task_id,
                    "warn",
                    "🛑 采集窗口已被关闭 · 终止任务(已采数据保留)".to_string(),
                );
                break;
            }
            Some(CollectStop::UserEnded) => {
                shared.user_ended = true;
                emit_collect_log(
                    app,
                    task_id,
                    "warn",
                    "🛑 已手动结束采集 · 停止后续关键词(已采数据保留,继续下载素材)".to_string(),
                );
                break;
            }
            None => {}
        }
    }

    (shared.seen_contents.len() as i64, shared.seen_comments.len() as i64)
}

/// 评论采集阶段:遍历已采内容,逐视频采一级评论,逐条入库。
#[allow(clippy::too_many_arguments)]
async fn collect_comments_phase(
    app: &AppHandle,
    db: &DatabaseConnection,
    bridge: &CollectBridge,
    adapter: &Arc<dyn crate::adapter::PlatformAdapter>,
    cfg: &veltrix_core::config::PlatformConfig,
    account_id: &str,
    task_id: &str,
    owner: &str,
    comment_time_range: &str,
    comment_limit: usize,
    shared: &mut CollectSharedState,
) {
    let mut id_seen: HashSet<String> = HashSet::new();
    let keyword_map: std::collections::HashMap<String, String> = {
        use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QuerySelect};
        use veltrix_core::db::entity::content as ce;
        ce::Entity::find()
            .filter(ce::Column::TaskId.eq(task_id))
            .select_only()
            .column(ce::Column::ContentId)
            .column(ce::Column::Keyword)
            .into_tuple::<(String, String)>()
            .all(db)
            .await
            .unwrap_or_default()
            .into_iter()
            .collect()
    };
    let video_ids: Vec<(String, String, String, String)> = shared
        .contents_for_media
        .iter()
        .filter(|c| id_seen.insert(c.content_id.clone()))
        .filter(|c| c.stats.comment_count != Some(0))
        .map(|c| {
            let token = c
                .extra
                .get("xsec_token")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            let keyword = keyword_map.get(&c.content_id).cloned().unwrap_or_default();
            (c.content_id.clone(), token, log_content_title(c), keyword)
        })
        .collect();
    let cutoff = comment_time_cutoff(comment_time_range);
    let total_videos = video_ids.len();
    let mut comment_seq: i64 = 0;
    write_task_collecting_comments(app, db, task_id, total_videos as i32).await;
    emit_collect_log(app, task_id, "info", format!(
        "💬 开始采集评论 · 共 {} 个视频 · 每视频最多 {}",
        video_ids.len(),
        if comment_limit == 0 { "不限".to_string() } else { comment_limit.to_string() }
    ));

    for (vidx, (content_id, xsec_token, title, keyword)) in
        video_ids.iter().enumerate()
    {
        if vidx > 0 {
            tokio::time::sleep(random_comment_video_interval()).await;
        }
        emit_collect_log(app, task_id, "info",
            format!("💬 [{}/{}] 正在采集「{title}」的评论", vidx + 1, total_videos));
        match bridge
            .collect_comments(
                app,
                CommentCollectRequest {
                    account_id,
                    content_id,
                    title,
                    xsec_token,
                    platform_cfg: cfg,
                    task_id: Some(task_id),
                    limit: comment_limit,
                    adapter: adapter.clone(),
                    keyword,
                    video_index: vidx + 1,
                    video_total: total_videos,
                },
            )
            .await
        {
            Ok(responses) if !responses.is_empty() => {
                let ctx = FetchContext {
                    keyword: content_id.clone(),
                    responses,
                };
                match adapter.parse(&TaskKind::Comments, &ctx).await {
                    Ok(mut output) => {
                        output.comments =
                            filter_comments(output.comments, cutoff, comment_limit);
                        let comments = std::mem::take(&mut output.comments);
                        for cm in comments {
                            comment_seq += 1;
                            let text = truncate_chars(&cm.text, 60);
                            let likes = cm.like_count.unwrap_or(0);
                            let msg =
                                format!("[{comment_seq}] {text} | 点赞:{likes}");
                            crate::webview::hud_log(
                                app, &cfg.id, account_id, "info", &msg,
                            );
                            emit_collect_entry(app, task_id, msg, CollectEntry {
                                kind: "comment".to_string(), seq: comment_seq,
                                avatar: cm.author.avatar.clone(), nickname: cm.author.nickname.clone(),
                                title: text, content_kind: None,
                            });
                            let one = FetchOutput { contents: Vec::new(), comments: vec![cm], authors: Vec::new() };
                            persist_collected(db, task_id, owner, content_id, one,
                                &mut shared.seen_contents, &mut shared.seen_comments).await;
                        }
                        if !output.authors.is_empty() {
                            persist_collected(db, task_id, owner, content_id, output,
                                &mut shared.seen_contents, &mut shared.seen_comments).await;
                        }
                    }
                    Err(e) => {
                        tracing::warn!(content_id = %content_id, "评论解析失败: {e}")
                    }
                }
            }
            Ok(_) => {}
            Err(e) => {
                tracing::warn!(content_id = %content_id, "评论采集失败: {e}");
                emit_collect_log(
                    app,
                    task_id,
                    "warn",
                    format!("⚠️ 「{title}」评论采集失败 · 原因: {e}"),
                );
            }
        }
        write_task_comment_progress(
            app,
            db,
            task_id,
            (vidx + 1) as i32,
            shared.seen_comments.len() as i64,
        )
        .await;
    }
    emit_collect_log(
        app,
        task_id,
        "info",
        format!("✅ 评论采集完成 · 共采集 {} 条评论", shared.seen_comments.len()),
    );
    {
        use sea_orm::sea_query::Expr;
        use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
        use veltrix_core::db::entity::content as content_entity;
        let ids: Vec<String> = video_ids
            .iter()
            .map(|(cid, _, _, _)| format!("{task_id}-{}-{}", cfg.id, cid))
            .collect();
        if !ids.is_empty() {
            if let Err(e) = content_entity::Entity::update_many()
                .col_expr(content_entity::Column::CommentCollected, Expr::value(true))
                .filter(content_entity::Column::Id.is_in(ids))
                .exec(db)
                .await
            {
                tracing::warn!("标记 comment_collected 失败(可能导致下次重复采集评论): {e}");
            }
        }
    }
}

/// 后处理流水线:意向分析 + 直链补取 + 素材下载 + Obsidian 同步。
#[allow(clippy::too_many_arguments)]
async fn post_collect_pipeline(
    app: &AppHandle,
    db: &DatabaseConnection,
    bridge: &CollectBridge,
    registry: &crate::adapter::AdapterRegistry,
    cfg: &veltrix_core::config::PlatformConfig,
    account_id: &str,
    task_id: &str,
    owner: &str,
    config_dir: &PathBuf,
    media_cfg: &veltrix_core::config::MediaConfig,
    transcription_cfg: &veltrix_core::config::TranscriptionConfig,
    intent_cfg: &veltrix_core::config::CommentIntentConfig,
    ai_extract: bool,
    analyze_comment_intent: bool,
    collect_comments: bool,
    auto_sync_obsidian: bool,
    shared: &CollectSharedState,
) {
    let total_contents = shared.seen_contents.len();
    let intent_ready = analyze_comment_intent
        && collect_comments
        && total_contents > 0
        && !intent_cfg.api_url.is_empty()
        && !intent_cfg.model.is_empty();
    if intent_ready {
        write_task_analyzing(app, db, task_id).await;
        analyze_comments_intent(app, db, task_id, intent_cfg).await;
        {
            use sea_orm::sea_query::Expr;
            use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
            use veltrix_core::db::entity::content as content_entity;
            if let Err(e) = content_entity::Entity::update_many()
                .col_expr(content_entity::Column::IntentAnalyzed, Expr::value(true))
                .filter(content_entity::Column::TaskId.eq(task_id))
                .filter(content_entity::Column::CommentCollected.eq(true))
                .exec(db)
                .await
            {
                tracing::warn!("标记 intent_analyzed 失败: {e}");
            }
        }
    }

    let total_comments = shared.seen_comments.len();
    let to_download = if total_contents == 0 && shared.had_error {
        write_task_failed(app, db, task_id, "采集未获取到任何内容").await;
        emit_collect_log(
            app,
            task_id,
            "error",
            "任务失败 · 未采集到内容,请检查账号登录态 / 风控".to_string(),
        );
        Vec::new()
    } else {
        let pending = filter_pending_media(db, task_id, shared.contents_for_media.clone()).await;
        if pending.is_empty() {
            write_task_done(app, db, task_id).await;
        } else {
            write_task_downloading(app, db, task_id, pending.len() as i32).await;
        }
        emit_collect_log(
            app,
            task_id,
            "info",
            format!("✅ 内容采集完成 · {total_contents} 条内容 · {total_comments} 条评论"),
        );
        pending
    };

    let mut to_download = to_download;
    if ai_extract {
        let stream_params = StreamRefreshParams {
            app,
            bridge,
            registry,
            db,
            cfg,
            account_id,
            task_id,
        };
        refresh_stream_urls(
            &stream_params,
            &mut to_download,
            false,
        )
        .await;
    }

    let media_params = MediaDownloadParams {
        app,
        db,
        task_id,
        platform: &cfg.id,
        account_id,
        config_dir,
        media_cfg,
        transcription_cfg,
        ai_extract,
    };
    download_media_for_contents(
        &media_params,
        to_download,
    )
    .await;

    if auto_sync_obsidian {
        obsidian::sync_task_to_obsidian(db, task_id, owner).await;
        emit_collect_log(app, task_id, "info", "✅ 已自动同步内容到 Obsidian");
    }
}

/// 收尾执行历史:记终态 + 本次新增量。
async fn finalize_task_run(
    db: &DatabaseConnection,
    task_id: &str,
    run_id: &str,
    started_at: i64,
) {
    use sea_orm::{ColumnTrait, EntityTrait, IntoActiveModel, PaginatorTrait, QueryFilter};
    use veltrix_core::db::entity::{
        comment as comment_entity, content as content_entity, task as task_entity,
        task_run as run_entity,
    };
    let final_task = task_entity::Entity::find_by_id(task_id.to_string())
        .one(db)
        .await
        .ok()
        .flatten();
    let final_status = final_task
        .as_ref()
        .map(|t| t.status.clone())
        .unwrap_or_else(|| "completed".to_string());
    let final_error = final_task.and_then(|t| t.error_message);
    let content_delta = content_entity::Entity::find()
        .filter(content_entity::Column::TaskId.eq(task_id))
        .filter(content_entity::Column::CollectedAt.gte(started_at))
        .count(db)
        .await
        .unwrap_or(0) as i64;
    let comment_delta = comment_entity::Entity::find()
        .filter(comment_entity::Column::TaskId.eq(task_id))
        .filter(comment_entity::Column::CollectedAt.gte(started_at))
        .count(db)
        .await
        .unwrap_or(0) as i64;
    if let Ok(Some(run)) = run_entity::Entity::find_by_id(run_id.to_string()).one(db).await {
        let mut am = run.into_active_model();
        am.finished_at = Set(Some(Utc::now().timestamp()));
        am.status = Set(final_status);
        am.content_delta = Set(content_delta);
        am.comment_delta = Set(comment_delta);
        am.error_message = Set(final_error);
        if let Err(e) = am.update(db).await {
            tracing::warn!(task_id = %task_id, "收尾执行历史失败: {e}");
        }
    }
}

/// `run_task` 的后台采集主体。抽成独立 async fn 以便用 catch_unwind 包裹做 panic 兜底。
async fn run_task_body(ctx: RunTaskCtx) {
    let RunTaskCtx {
        app,
        db,
        cookies,
        collect_semaphore,
        collect_locks,
        bridge,
        registry,
        cfg,
        account_id,
        task_id,
        task_name,
        owner,
        keywords,
        per_keyword_limit,
        min_likes,
        collect_comments,
        comment_time_range,
        comment_limit,
        analyze_comment_intent,
        ai_extract,
        auto_sync_obsidian,
        sort_mode,
        time_range,
        extra_filter_clicks,
        media_cfg,
        config_dir,
        intent_cfg,
        transcription_cfg,
        run_started_at: now,
    } = ctx;
    {
        // 全局采集并发闸:先占一个名额再开窗,超过上限的任务在此排队,避免调度同点拉起多任务时
        // 同时弹出过多 WebView 把资源打满。permit 与 collect_guard 同寿命,WebView 阶段结束即释放。
        let collect_permit = collect_semaphore.acquire().await.ok();
        // 同账号采集互斥:占用 WebView 窗口的阶段(关键词采集 + 评论采集)串行,
        // 其他账号 / 平台的任务不受影响,可真正并行采集
        let account_lock =
            account_collect_lock(&collect_locks, &format!("{}-{}", cfg.id, account_id));
        let collect_guard = account_lock.lock().await;

        // 执行历史:本次运行先记一条 task_run(running);采集日志按 [started_at, finished_at]
        // 时间范围归到该次运行(见 list_run_logs)。run_id 用 task_id + 起始秒,串行运行起始秒唯一。
        let run_id = format!("{}-run-{}", task_id, now);
        {
            use veltrix_core::db::entity::task_run as run_entity;
            let am = run_entity::ActiveModel {
                id: Set(run_id.clone()),
                task_id: Set(task_id.clone()),
                owner: Set(owner.clone()),
                started_at: Set(now),
                finished_at: Set(None),
                status: Set("running".to_string()),
                content_delta: Set(0),
                comment_delta: Set(0),
                error_message: Set(None),
            };
            if let Err(e) = am.insert(&db).await {
                tracing::warn!(task_id = %task_id, "创建执行历史失败: {e}");
            }
        }

        // 平台适配器:有则解析落库并计真实数,无则降级为只累计拦截响应数(不落明细)
        let adapter: Option<Arc<dyn crate::adapter::PlatformAdapter>> = registry.get(&cfg.id).ok();

        // 该任务已采内容快照(content_id 集合) + 黑名单作者 uid:并发加载
        let (existing_ids, blacklisted_uids) = tokio::join!(
            load_existing_content_ids(&db, &task_id, &cfg.id),
            load_blacklisted_author_uids(&db, &owner, &cfg.id),
        );
        if !blacklisted_uids.is_empty() {
            emit_collect_log(
                &app,
                &task_id,
                "info",
                format!(
                    "ℹ️ 已加载 {} 个黑名单作者 · 采集将排除其内容",
                    blacklisted_uids.len()
                ),
            );
        }

        let mut shared = CollectSharedState {
            seen_contents: HashSet::new(),
            seen_comments: HashSet::new(),
            intercepted_total: 0,
            had_error: false,
            contents_for_media: Vec::new(),
            window_closed: false,
            user_ended: false,
        };

        // 重置本账号采集窗口的「已被手动关闭」标记,使本次任务能正常开窗
        bridge.reset_collect_window_closed(&cfg.id, &account_id);
        // 重置本任务的「结束」停止标记,避免上次运行点过结束影响本次重跑
        bridge.reset_task_stop(&task_id);

        // 阶段1:关键词采集
        let (total_contents, _total_comments) = collect_keywords(
            &app,
            &db,
            &bridge,
            &cfg,
            &account_id,
            &task_id,
            &task_name,
            &owner,
            &keywords,
            per_keyword_limit,
            min_likes,
            &sort_mode,
            &time_range,
            &existing_ids,
            &blacklisted_uids,
            &adapter,
            &extra_filter_clicks,
            &mut shared,
        )
        .await;

        // 用户中途手动关闭采集窗口 → 终止任务:不再采评论 / 不跑后处理(二者都会重建采集窗口),
        // 标记 cancelled 收尾;已增量落库的内容保留,素材可日后重跑补齐。
        if shared.window_closed || bridge.is_collect_window_closed(&cfg.id, &account_id) {
            emit_collect_log(
                &app,
                &task_id,
                "warn",
                "🛑 采集窗口已被手动关闭 · 任务终止".to_string(),
            );
            write_task_cancelled(&app, &db, &task_id, "采集窗口被手动关闭,任务已终止").await;
            finalize_task_run(&db, &task_id, &run_id, now).await;
            bridge.close_collect_window(&cfg.id, &account_id);
            return;
        }

        // 用户点了 HUD「结束」:停止采集,关闭采集窗口(避免评论 / 后处理再重开窗口继续弹出),
        // 但仍走素材下载并正常完成——已采内容不浪费。
        // 注:这里必须销毁(close)而非隐藏——隐藏会让旧窗口继续占着该账号 WebView2 数据目录锁,
        // 下次执行新建窗口用同一目录会冲突,导致「采集窗口根本不出现」。销毁释放目录,重开正常。
        if shared.user_ended {
            emit_collect_log(
                &app,
                &task_id,
                "info",
                "ℹ️ 已手动结束采集 · 跳过评论采集,继续下载已采素材后完成".to_string(),
            );
            bridge.close_collect_window(&cfg.id, &account_id);
        }

        // 阶段2:评论采集(手动结束后不再采评论,避免重开采集窗口)
        if collect_comments && total_contents > 0 && !shared.user_ended {
            if let Some(adapter_arc) = &adapter {
                collect_comments_phase(
                    &app,
                    &db,
                    &bridge,
                    adapter_arc,
                    &cfg,
                    &account_id,
                    &task_id,
                    &owner,
                    &comment_time_range,
                    comment_limit,
                    &mut shared,
                )
                .await;
            }
        }

        // WebView 占用阶段(关键词采集 + 评论采集)已结束,释放同账号互斥锁与全局并发名额;
        // 后续意向分析(LLM)与素材下载(HTTP)不占窗口,其他任务可立即用该账号 / 名额开采
        drop(collect_guard);
        drop(collect_permit);

        // 风控反馈闭环:据本次产出回写账号健康度。
        if total_contents > 0 {
            if let Err(e) = cookies.release_ok(&account_id).await {
                tracing::warn!(account_id = %account_id, "重置账号风控计数失败: {e}");
            }
        } else if shared.had_error {
            if let Err(e) = cookies.mark_risk(&account_id).await {
                tracing::warn!(account_id = %account_id, "标记账号风控失败: {e}");
            } else {
                emit_collect_log(
                    &app,
                    &task_id,
                    "warn",
                    "账号零产出且采集报错 · 已标记风控冷却,下次将自动轮换账号".to_string(),
                );
            }
        }

        // 阶段3:后处理(意向分析 + 直链补取 + 素材下载 + Obsidian 同步)
        post_collect_pipeline(
            &app,
            &db,
            &bridge,
            &registry,
            &cfg,
            &account_id,
            &task_id,
            &owner,
            &config_dir,
            &media_cfg,
            &transcription_cfg,
            &intent_cfg,
            ai_extract,
            analyze_comment_intent,
            collect_comments,
            auto_sync_obsidian,
            &shared,
        )
        .await;

        // 阶段4:收尾执行历史
        finalize_task_run(&db, &task_id, &run_id, now).await;

        // 采集完成,关闭采集窗口(销毁释放该账号 WebView2 数据目录;下次执行重建。
        // 不用隐藏复用:隐藏窗口会占住数据目录锁,下次新建同目录窗口冲突 → 窗口起不来)
        bridge.close_collect_window(&cfg.id, &account_id);
    }
}

/// 调度器:扫描到点的 daily / watching 任务并自动启动采集。lib.rs 后台循环每 30s 调一次。
///
/// 规则:
/// - 每日定时(daily):本地时间过了 scheduled_at(HH:mm)且今天还没跑过 → 启动;
/// - 持续监听(watching):上次结束(或启动)时间起算,间隔 watch_interval_min 分钟到点 → 再次启动;
///   从未运行过的监听任务不自动首启(由用户手动启动),手动停止(cancelled)即退出自动监听;
/// - 进行中(running/评论/分析/下载)一律跳过,与 run_task 的防重复启动一致。
pub async fn run_due_scheduled_tasks(app: &tauri::AppHandle) {
    use tauri::Manager;
    use veltrix_core::db::entity::task as task_entity;
    let state = app.state::<AppState>();
    let now = chrono::Local::now();
    let tasks = match task_entity::Entity::find()
        .filter(task_entity::Column::Archived.eq(false))
        .filter(task_entity::Column::TriggerType.is_in(["daily", "watching"]))
        .all(&state.db)
        .await
    {
        Ok(t) => t,
        Err(e) => {
            tracing::warn!("调度器扫描任务失败: {e}");
            return;
        }
    };
    for t in tasks {
        if matches!(
            t.status.as_str(),
            "running" | "collecting_comments" | "analyzing_comments" | "downloading_media"
        ) {
            continue;
        }
        let due = match t.trigger_type.as_str() {
            "daily" => daily_task_due(&t, &now),
            "watching" => t.status != "cancelled" && watching_task_due(&t, now.timestamp()),
            _ => false,
        };
        if !due {
            continue;
        }
        tracing::info!(task_id = %t.id, trigger = %t.trigger_type, "调度器自动启动任务");
        if let Err(e) = run_task(app.state::<AppState>(), app.clone(), t.id.clone()).await {
            tracing::warn!(task_id = %t.id, "调度器启动任务失败: {e}");
        }
    }
}

/// 每日定时是否到点:本地时间已过今日 HH:mm 且本日尚未启动过。
fn daily_task_due(
    t: &veltrix_core::db::entity::task::Model,
    now: &chrono::DateTime<chrono::Local>,
) -> bool {
    let Some(at) = t.scheduled_at.as_deref() else {
        return false;
    };
    let Ok(target_time) = chrono::NaiveTime::parse_from_str(at, "%H:%M") else {
        return false;
    };
    let today_target = now.date_naive().and_time(target_time);
    let chrono::LocalResult::Single(target) = today_target.and_local_timezone(chrono::Local)
    else {
        return false;
    };
    let target_ts = target.timestamp();
    if now.timestamp() < target_ts {
        return false;
    }
    // 今天已经跑过(本次启动时间晚于今日目标点)则不重复
    t.started_at.map(|s| s < target_ts).unwrap_or(true)
}

/// 持续监听是否到点:距上次结束(兜底取启动)已超过监听间隔。从未运行过不自动首启。
fn watching_task_due(t: &veltrix_core::db::entity::task::Model, now_ts: i64) -> bool {
    let Some(interval_min) = t.watch_interval_min else {
        return false;
    };
    if interval_min <= 0 {
        return false;
    }
    match t.finished_at.or(t.started_at) {
        Some(last) => now_ts - last >= interval_min as i64 * 60,
        None => false,
    }
}

/// 回写任务进度与已采内容/评论计数。查询/更新失败仅告警,不中断采集循环。
/// 任务进度/状态变更事件名。前端 listen 后就地刷新对应任务行,免等轮询。
const TASK_PROGRESS_EVENT: &str = "task-progress";

/// 进度/状态变更后向前端推送最新任务视图,前端据此就地更新该行(实时进度)。
/// emit 失败仅忽略(无前端监听时不影响采集);传引用即可满足 Serialize + Clone 约束。
fn emit_task_progress(app: &AppHandle, model: veltrix_core::db::entity::task::Model) {
    use tauri::Emitter;
    let view: crate::commands::task::TaskView = model.into();
    let _ = app.emit(TASK_PROGRESS_EVENT, &view);
}

async fn write_task_progress(
    app: &AppHandle,
    db: &DatabaseConnection,
    task_id: &str,
    progress: i32,
    content_count: i64,
    comment_count: i64,
) {
    use veltrix_core::db::entity::task as task_entity;
    match task_entity::Entity::find_by_id(task_id.to_string()).one(db).await {
        Ok(Some(m)) => {
            let mut am = m.into_active_model();
            am.progress = Set(progress);
            am.content_count = Set(content_count);
            am.comment_count = Set(comment_count);
            am.updated_at = Set(Utc::now().timestamp());
            match am.update(db).await {
                Ok(updated) => emit_task_progress(app, updated),
                Err(e) => tracing::warn!("回写任务进度失败: {e}"),
            }
        }
        Ok(None) => tracing::warn!(task_id, "回写进度时任务已不存在"),
        Err(e) => tracing::warn!("回写进度查询失败: {e}"),
    }
}

/// 语音转写间隔:1~3s 随机。ASR 调用之间串行插入,降低对厂商的请求频率。
/// 复用 pool 的廉价熵源做法(系统时间纳秒),不引额外依赖。
/// 评论采集的视频间隔:1.5~3s 随机。串行遍历视频之间插入,降低请求频率。
fn random_comment_video_interval() -> std::time::Duration {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as u64)
        .unwrap_or(0);
    // 1500 + [0,1500) → 1500~2999ms
    std::time::Duration::from_millis(1500 + nanos % 1500)
}

/// 按字符截断(中文友好),超出 max 个字符则截断并加省略号。
fn truncate_chars(text: &str, max: usize) -> String {
    let trimmed = text.trim();
    let chars: Vec<char> = trimmed.chars().collect();
    if chars.len() <= max {
        return trimmed.to_string();
    }
    let mut out: String = chars[..max].iter().collect();
    out.push('…');
    out
}

/// 内容用于日志展示的标题:优先 title,空则用正文 desc,均空给占位;截断到 40 字。
fn log_content_title(content: &Content) -> String {
    let raw = content
        .title
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .or(content.desc.as_deref())
        .filter(|s| !s.trim().is_empty())
        .unwrap_or("(无标题)");
    truncate_chars(raw, 40)
}

/// ContentKind → 字符串标识(日志 entry 用)。
fn content_kind_label(kind: &ContentKind) -> &'static str {
    match kind {
        ContentKind::Video => "video",
        ContentKind::Image => "image",
        ContentKind::Article => "article",
        ContentKind::Unknown => "unknown",
    }
}

/// 为视频内容补取/刷新视频直链:对缺直链(典型:小红书搜索不含直链)或需刷新(签名过期)的
/// 视频内容,经「详情页拦截」拿到新鲜直链,回写到内存 `Content` 与 DB(content.video_url)。
///
/// - `force=false`:仅补「缺直链」的内容(初采:抖音/快手搜索已含直链不动;小红书搜索无直链 → 补)。
/// - `force=true`:即使已有直链也重取(单条重试:直链短期签名过期后刷新)。
///
/// 串行执行(共用同一账号窗口,导航不能并发);任一条失败仅告警跳过,不中断。
/// 平台无 `detail_url_template` 或适配器不支持 `ContentDetail` 解析时整体跳过(B站/TikTok/YouTube 等)。
async fn refresh_stream_urls(
    params: &StreamRefreshParams<'_>,
    contents: &mut [Content],
    force: bool,
) {
    if params.cfg.collect.detail_url_template.trim().is_empty() {
        return;
    }
    let adapter = match params.registry.get(&params.cfg.id) {
        Ok(a) if a.supports(&TaskKind::ContentDetail) => a,
        _ => return, // 平台不支持详情解析:整体跳过
    };
    for content in contents.iter_mut() {
        if content.kind != ContentKind::Video {
            continue;
        }
        let has_url = content
            .video_url
            .as_deref()
            .map(|s| !s.trim().is_empty())
            .unwrap_or(false);
        if has_url && !force {
            continue; // 已有直链且非强制刷新 → 不动(抖音/快手初采路径)
        }
        // 小红书详情页需 xsec_token(存于 content.extra);抖音/快手留空
        let token = content
            .extra
            .get("xsec_token")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let responses = match params.bridge
            .fetch_content_detail(
                params.app,
                DetailFetchRequest {
                    account_id: params.account_id,
                    content_id: &content.content_id,
                    xsec_token: &token,
                    platform_cfg: params.cfg,
                },
            )
            .await
        {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(content_id = %content.content_id, "补取视频直链导航失败: {e}");
                continue;
            }
        };
        let ctx = FetchContext {
            keyword: content.content_id.clone(),
            responses,
        };
        let fresh = match adapter.parse(&TaskKind::ContentDetail, &ctx).await {
            Ok(out) => out
                .contents
                .into_iter()
                .find(|c| c.content_id == content.content_id)
                .and_then(|c| c.video_url)
                .filter(|s| !s.trim().is_empty()),
            Err(e) => {
                tracing::warn!(content_id = %content.content_id, "解析详情直链失败: {e}");
                None
            }
        };
        match fresh {
            Some(url) => {
                content.video_url = Some(url.clone());
                // 回写 DB:content 行 id = "{task_id}-{platform}-{content_id}"(与落库口径一致)
                let row_id = format!("{}-{}-{}", params.task_id, content.platform, content.content_id);
                update_content_video_url(params.db, &row_id, &url).await;
            }
            None => {
                emit_collect_log(
                    params.app,
                    params.task_id,
                    "warn",
                    format!("补取直链未果 · {} · 该视频可能无音频可提", content.content_id),
                );
            }
        }
    }
}

/// 仅更新 content.video_url 一列(补取/刷新直链后回写,不触碰其它字段)。
async fn update_content_video_url(db: &DatabaseConnection, id: &str, video_url: &str) {
    use veltrix_core::db::entity::content as content_entity;
    let am = content_entity::ActiveModel {
        id: Set(id.to_string()),
        video_url: Set(Some(video_url.to_string())),
        ..Default::default()
    };
    if let Err(e) = am.update(db).await {
        tracing::warn!(content_id = %id, "回写视频直链失败: {e}");
    }
}

/// 媒体下载 / 转写阶段的日志双写:既推前端日志面板(事件 + 落库),也写采集窗口 HUD 浮层。
/// 该阶段已无 window 句柄,故用 hud_log 按 platform+account 定位 HUD 窗口写入(窗口不存在则静默)。
fn emit_media_log(
    app: &AppHandle,
    task_id: &str,
    platform: &str,
    account_id: &str,
    level: &str,
    message: impl Into<String>,
) {
    let msg = message.into();
    emit_collect_log(app, task_id, level, msg.clone());
    crate::webview::hud_log(app, platform, account_id, level, &msg);
}

/// 平台主页 URL:按域取会话 Cookie 用(GetCookies 按该 URL 的域 / 路径 / secure 过滤命中的 Cookie)。
/// 未登记的平台返回 None(不读实时 Cookie,退回 DB)。
fn platform_home_url(platform: &str) -> Option<&'static str> {
    match platform {
        "tiktok" => Some("https://www.tiktok.com/"),
        "youtube" => Some("https://www.youtube.com/"),
        "douyin" => Some("https://www.douyin.com/"),
        "kuaishou" => Some("https://www.kuaishou.com/"),
        "xhs" => Some("https://www.xiaohongshu.com/"),
        "bilibili" => Some("https://www.bilibili.com/"),
        _ => None,
    }
}

/// 解析素材下载用的 Cookie。**优先**从仍存活的采集窗口读实时 Cookie(含 httponly 的
/// `tt_chain_token`,且与本次会话签发的直链匹配——这是 TikTok 能下到音频的关键);取不到再退回
/// DB 账号 Cookie。素材下载阶段(post_collect_pipeline)采集窗口尚未关闭(关窗在其后),故能读到。
async fn resolve_session_cookie(
    app: &AppHandle,
    db: &DatabaseConnection,
    platform: &str,
    account_id: &str,
) -> Option<String> {
    use tauri::Manager;
    if let Some(home) = platform_home_url(platform) {
        let label = crate::webview::pool::window_label(platform, account_id);
        if let Some(window) = app.get_webview_window(&label) {
            if let Some(cookie) = crate::webview::cookies::read_cookies(&window, home).await {
                return Some(cookie);
            }
        }
    }
    fetch_account_cookie(db, account_id).await
}

/// 取指定账号的完整 Cookie 串,供 ffmpeg 拉流时带上(TikTok 等防盗链 CDN 校验会话 Cookie)。
/// 账号不存在 / 查询失败 / Cookie 为空都返回 None(降级为不带 Cookie,不阻断下载)。
async fn fetch_account_cookie(db: &DatabaseConnection, account_id: &str) -> Option<String> {
    use veltrix_core::db::entity::account;
    if account_id.is_empty() {
        return None;
    }
    account::Entity::find_by_id(account_id.to_string())
        .one(db)
        .await
        .ok()
        .flatten()
        .map(|m| m.cookie.trim().to_string())
        .filter(|cookie| !cookie.is_empty())
}

/// 取某平台一个可用账号的 Cookie(补偿重试无绑定账号时用):优先最近使用的 active 账号。
/// 无可用账号 / Cookie 为空返回 None。
async fn fetch_platform_cookie(db: &DatabaseConnection, platform: &str) -> Option<String> {
    use veltrix_core::db::entity::account;
    account::Entity::find()
        .filter(account::Column::Platform.eq(platform))
        .filter(account::Column::Status.eq("active"))
        .order_by_desc(account::Column::LastUsedAt)
        .one(db)
        .await
        .ok()
        .flatten()
        .map(|m| m.cookie.trim().to_string())
        .filter(|cookie| !cookie.is_empty())
}

/// 采集落库后下载内容素材。并发处理(限 10 路、不再限速),按 content_id 去重避免重复下载;
/// 副产品失败已在 media::process_content 内部吞为告警,主素材成败回写到 contents 表。
/// `platform`/`account_id` 用于把素材下载日志写进该账号采集窗口的 HUD 浮层。
async fn download_media_for_contents(
    params: &MediaDownloadParams<'_>,
    contents: Vec<Content>,
) {
    if contents.is_empty() {
        return;
    }
    let root = crate::media::media_root(params.config_dir, params.media_cfg);
    use futures_util::StreamExt;
    // 该任务账号的会话 Cookie:整批同平台同账号取一次,给所有内容的 ffmpeg 拉流复用。
    // 优先读存活采集窗口的实时 Cookie(含 httponly tt_chain_token,与本次直链匹配),退回 DB。
    let cookie = resolve_session_cookie(params.app, params.db, params.platform, params.account_id).await;
    let cookie_ref = cookie.as_deref();
    // 收集视频转出的音频(content row id, mp3 路径),供素材下载结束后统一转写
    let mut audios: Vec<(String, String)> = Vec::new();
    // 跨关键词同一内容只下一次(取 owned,move 进并发任务,避免 async 闭包借用的生命周期问题)
    let mut downloaded: HashSet<String> = HashSet::new();
    let targets: Vec<Content> = contents
        .into_iter()
        .filter(|c| downloaded.insert(c.content_id.clone()))
        .collect();
    let total = targets.len();
    emit_media_log(params.app, params.task_id, params.platform, params.account_id, "info", format!("开始下载素材 · 共 {total} 条"));
    let mut count = 0usize;
    let mut failed = 0usize;
    // 并发下载(限 10 路并发,不再串行限速),边完成边回写结果与进度
    let root_ref = &root;
    let mut stream = futures_util::stream::iter(targets.into_iter().map(|content| async move {
        // 标题在下载前取(content 随后 move 进 process_content);用于 HUD 逐条日志展示
        let title = log_content_title(&content);
        let outcome = crate::media::process_content(
            &content,
            root_ref,
            params.media_cfg,
            params.ai_extract,
            cookie_ref,
        )
        .await;
        let id = format!("{}-{}-{}", params.task_id, content.platform, content.content_id);
        (id, title, outcome)
    }))
    .buffer_unordered(10);
    while let Some((id, title, outcome)) = stream.next().await {
        let ok = is_media_ok(&outcome);
        if !ok {
            failed += 1;
        }
        record_media_outcome(params.db, &id, &outcome).await;
        // 视频转出音频的,记下供采集结束后统一转写(不占采集通道)
        if let Some(audio_path) = &outcome.audio_path {
            audios.push((id.clone(), audio_path.clone()));
        }
        count += 1;
        // 逐条素材下载日志:HUD 面板可见下载过程(成功标题 / 失败原因)
        if ok {
            // 视频转音频成功时额外标注,便于看出转写素材已就绪
            let extra = if outcome.audio_extracted == Some(true) {
                " · 已转音频"
            } else {
                ""
            };
            emit_media_log(
                params.app,
                params.task_id,
                params.platform,
                params.account_id,
                "info",
                format!("素材 {count}/{total} · {title} · 完成{extra}"),
            );
        } else {
            let reason = outcome.error.as_deref().unwrap_or("未知原因");
            emit_media_log(
                params.app,
                params.task_id,
                params.platform,
                params.account_id,
                "warn",
                format!("素材 {count}/{total} · {title} · 失败:{reason}"),
            );
        }
        // 逐条回写进度,调度页据此刷新「素材下载中 done/total」
        write_task_media_done(params.app, params.db, params.task_id, count as i32).await;
    }
    emit_media_log(
        params.app,
        params.task_id,
        params.platform,
        params.account_id,
        "info",
        format!(
            "素材下载完成,共处理 {count} 条内容(失败 {failed} 条),输出目录: {}",
            root.display()
        ),
    );
    // 素材下载完成后统一做语音转写(视频音频→文案),失败仅告警不影响任务终态
    transcribe_for_contents(params.app, params.db, params.task_id, params.platform, params.account_id, params.transcription_cfg, audios).await;
    // 素材全部处理完毕,任务从 downloading_media 收尾为 completed
    write_task_done(params.app, params.db, params.task_id).await;
}

/// 采集结束后统一语音转写:把每条视频转出的音频逐条调 ASR 厂商,回写 content.transcript。
/// 串行 + 限速,失败仅告警不中断;未配置/厂商不支持 ASR 则跳过。不占采集通道(主体已结束)。
#[allow(clippy::too_many_arguments)]
async fn transcribe_for_contents(
    app: &AppHandle,
    db: &DatabaseConnection,
    task_id: &str,
    platform: &str,
    account_id: &str,
    transcription_cfg: &veltrix_core::config::TranscriptionConfig,
    audios: Vec<(String, String)>,
) {
    if audios.is_empty() {
        return;
    }
    let api_key = get_secret(db, "transcription_api_key").await;
    if api_key.trim().is_empty() {
        emit_media_log(
            app,
            task_id,
            platform,
            account_id,
            "warn",
            "未配置语音转写 API Key,跳过转写 · 请到「系统设置 → 语音转写」填写 API Key".to_string(),
        );
        return;
    }

    let total = audios.len();
    emit_media_log(app, task_id, platform, account_id, "info", format!("开始语音转写 · 共 {total} 条 · 并发 5 路"));
    // 并发调 ASR API:buffer_unordered(5) 限制同时在飞的请求数,避免打爆 rate limit
    let done = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    use futures_util::StreamExt;
    let mut stream = futures_util::stream::iter(audios.into_iter().map(|(id, audio_path)| {
        let api_key = api_key.clone();
        let cfg_url = transcription_cfg.api_url.clone();
        let cfg_model = transcription_cfg.model.clone();
        let db = db.clone();
        let app = app.clone();
        let task_id = task_id.to_string();
        let platform = platform.to_string();
        let account_id = account_id.to_string();
        let done = done.clone();
        let total_s = total.to_string();
        async move {
            let result = crate::llm::transcribe(crate::llm::TranscribeRequest {
                provider_code: "mimo",
                api_url: &cfg_url,
                api_key: &api_key,
                model: &cfg_model,
                audio_path: std::path::Path::new(&audio_path),
            })
            .await;
            match result {
                Ok(text) => record_transcript(&db, &id, Some(text), None).await,
                Err(e) => {
                    tracing::warn!(content_id = %id, "语音转写失败: {e}");
                    record_transcript(&db, &id, None, Some(format!("{e}"))).await;
                }
            }
            let n = done.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
            emit_media_log(&app, &task_id, &platform, &account_id, "info", format!("转写进度 {n}/{total_s}"));
        }
    }))
    .buffer_unordered(5);
    while stream.next().await.is_some() {}
    emit_media_log(app, task_id, platform, account_id, "info", format!("语音转写完成 · {total}/{total}"));
}

/// 回写单条内容的转写结果(只更新 transcript / transcript_error 两列,不触碰其它字段)。
async fn record_transcript(
    db: &DatabaseConnection,
    id: &str,
    text: Option<String>,
    err: Option<String>,
) {
    use veltrix_core::db::entity::content as content_entity;
    let am = content_entity::ActiveModel {
        id: Set(id.to_string()),
        transcript: Set(text),
        transcript_error: Set(err),
        ..Default::default()
    };
    if let Err(e) = am.update(db).await {
        tracing::warn!(content_id = %id, "回写转写文本失败: {e}");
    }
}

/// 加载任务已采内容的 content_id 集合:智能停止「只数新增」(重复不占目标配额)的依据。
/// 并入采集去重台账(同平台)的 content_id:这些内容曾采过,既不计入本次目标配额,
/// persist 阶段也不会重复保存,从而让「计数 / 停止」与「实际入库」口径一致
///(否则清空业务数据后重采会一路滚到目标数却实际只存 0 条)。
async fn load_existing_content_ids(
    db: &DatabaseConnection,
    task_id: &str,
    platform: &str,
) -> HashSet<String> {
    use sea_orm::{ColumnTrait, QueryFilter, QuerySelect};
    use veltrix_core::db::entity::collect_record as ledger_entity;
    use veltrix_core::db::entity::content as content_entity;
    let mut ids: HashSet<String> = content_entity::Entity::find()
        .filter(content_entity::Column::TaskId.eq(task_id))
        .select_only()
        .column(content_entity::Column::ContentId)
        .into_tuple::<String>()
        .all(db)
        .await
        .unwrap_or_else(|e| {
            tracing::warn!("加载已采内容失败: {e}");
            Vec::new()
        })
        .into_iter()
        .collect();
    let recorded: Vec<String> = ledger_entity::Entity::find()
        .filter(ledger_entity::Column::Platform.eq(platform))
        .select_only()
        .column(ledger_entity::Column::ContentId)
        .into_tuple::<String>()
        .all(db)
        .await
        .unwrap_or_else(|e| {
            tracing::warn!("加载采集去重台账失败: {e}");
            Vec::new()
        });
    ids.extend(recorded);
    ids
}

/// 加载某 owner+platform 下被拉黑的作者 uid 集合:采集时据此排除其内容。查询失败按空处理(不阻断采集)。
async fn load_blacklisted_author_uids(
    db: &DatabaseConnection,
    owner: &str,
    platform: &str,
) -> HashSet<String> {
    use sea_orm::{ColumnTrait, QueryFilter, QuerySelect};
    use veltrix_core::db::entity::author as author_entity;
    author_entity::Entity::find()
        .filter(author_entity::Column::Owner.eq(owner))
        .filter(author_entity::Column::Platform.eq(platform))
        .filter(author_entity::Column::IsBlacklisted.eq(true))
        .select_only()
        .column(author_entity::Column::Uid)
        .into_tuple::<String>()
        .all(db)
        .await
        .unwrap_or_else(|e| {
            tracing::warn!("加载黑名单作者失败: {e}");
            Vec::new()
        })
        .into_iter()
        .collect()
}

/// 过滤出真正需要下载素材的内容:按 content_id 去重,并排除库中已成功下载过的旧内容。
/// 重复内容(media_status=success)只在 persist 阶段更新统计,这里不再重复下载素材。
/// 一次性取「已成功」集合避免逐条查库(N+1)。
async fn filter_pending_media(
    db: &DatabaseConnection,
    task_id: &str,
    contents: Vec<Content>,
) -> Vec<Content> {
    use sea_orm::{ColumnTrait, QueryFilter, QuerySelect};
    use veltrix_core::db::entity::content as content_entity;
    let done: HashSet<String> = content_entity::Entity::find()
        .filter(content_entity::Column::TaskId.eq(task_id))
        .filter(content_entity::Column::MediaStatus.eq("success"))
        .select_only()
        .column(content_entity::Column::ContentId)
        .into_tuple::<String>()
        .all(db)
        .await
        .unwrap_or_default()
        .into_iter()
        .collect();
    let mut seen: HashSet<String> = HashSet::new();
    let mut pending = Vec::new();
    for c in contents {
        // 跨关键词同一内容只下一次
        if !seen.insert(c.content_id.clone()) {
            continue;
        }
        // 已成功下载过 → 媒体不重下(统计已由 persist upsert 更新)
        if done.contains(&c.content_id) {
            continue;
        }
        pending.push(c);
    }
    pending
}

/// 素材是否整体成功:主素材就绪且音频提取未失败(开启提取时)。
fn is_media_ok(outcome: &crate::media::MediaOutcome) -> bool {
    outcome.ok && outcome.audio_extracted != Some(false)
}

/// 把素材处理结果回写到 contents 表(仅更新状态相关三列,不触碰其它字段)。
async fn record_media_outcome(db: &DatabaseConnection, id: &str, outcome: &crate::media::MediaOutcome) {
    use veltrix_core::db::entity::content as content_entity;
    let status = if is_media_ok(outcome) { "success" } else { "failed" };
    let mut am = content_entity::ActiveModel {
        id: Set(id.to_string()),
        media_status: Set(Some(status.to_string())),
        audio_extracted: Set(outcome.audio_extracted),
        media_error: Set(outcome.error.clone()),
        ..Default::default()
    };
    // 下载成功才回写本地路径;失败/未下不覆盖旧值(NotSet),便于重试后保留上次成功路径
    if let Some(p) = &outcome.cover_path {
        am.cover_path = Set(Some(p.clone()));
    }
    if let Some(p) = &outcome.avatar_path {
        am.avatar_path = Set(Some(p.clone()));
    }
    // 音频路径回写:详情页播放音频用(仅视频 + 提取成功时有值)
    if let Some(p) = &outcome.audio_path {
        am.audio_path = Set(Some(p.clone()));
    }
    if let Some(v) = outcome.video_downloaded {
        am.video_downloaded = Set(Some(v));
    }
    if let Some(v) = outcome.image_total {
        am.image_total = Set(Some(v));
    }
    if let Some(v) = outcome.image_done {
        am.image_done = Set(Some(v));
    }
    if let Err(e) = am.update(db).await {
        tracing::warn!(content_id = %id, "回写素材状态失败: {e}");
    }
}

/// contents 实体 → model::Content,供失败重试时重跑素材下载。
/// 只填下载所需字段(链接/形态/作者头像),统计等无关字段走 Default。
fn content_from_model(m: &veltrix_core::db::entity::content::Model) -> Content {
    let kind = match m.kind.as_str() {
        "video" => ContentKind::Video,
        "image" => ContentKind::Image,
        "article" => ContentKind::Article,
        _ => ContentKind::Unknown,
    };
    let image_urls: Vec<String> = serde_json::from_str(&m.image_urls).unwrap_or_default();
    let avatar = serde_json::from_str::<serde_json::Value>(&m.author_json)
        .ok()
        .and_then(|v| v.get("avatar").and_then(|a| a.as_str()).map(str::to_string));
    Content {
        platform: m.platform.clone(),
        content_id: m.content_id.clone(),
        kind,
        title: m.title.clone(),
        desc: m.desc.clone(),
        author: Author {
            platform: m.platform.clone(),
            uid: m.author_uid.clone(),
            nickname: m.author_nickname.clone(),
            avatar,
            ..Default::default()
        },
        video_url: m.video_url.clone(),
        cover_url: m.cover_url.clone(),
        image_urls,
        duration: m.duration,
        // 保留 extra:小红书详情页补取直链需其中的 xsec_token
        extra: serde_json::from_str(&m.extra).unwrap_or(serde_json::Value::Null),
        ..Default::default()
    }
}

/// 单条内容素材状态视图:retry_content_media 返回最新状态,前端就地刷新该行。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaStatusView {
    pub id: String,
    pub media_status: Option<String>,
    pub audio_extracted: Option<bool>,
    pub media_error: Option<String>,
}

/// 失败重试:对单条内容重跑素材下载并回写状态。
///
/// 平台视频直链多为带时效签名的 CDN 地址(douyinvod 等),过期后用旧链重试会 403:
/// 故视频内容首次转音频失败时,经详情页拦截重取一次新鲜直链再试,治「签名过期」。
#[tauri::command]
pub async fn retry_content_media(
    state: State<'_, AppState>,
    app: AppHandle,
    id: String,
) -> Result<MediaStatusView> {
    use veltrix_core::db::entity::content as content_entity;
    let me = current_user(&state).ok_or_else(|| CrawlerError::Config("未登录".into()))?;
    let row = content_entity::Entity::find_by_id(id.clone())
        .one(&state.db)
        .await
        .map_err(|e| CrawlerError::Config(format!("查询内容失败: {e}")))?
        .ok_or_else(|| CrawlerError::Config("内容不存在".into()))?;
    // 数据归属:self 用户只能操作自己的内容
    if me.scope == "self" && row.owner != me.name {
        return Err(CrawlerError::Config("无权操作该内容".into()));
    }

    // clone 出媒体配置,避免跨 await 持有配置锁
    let media_cfg = { lock_config(&state)?.media.clone() };
    let root = crate::media::media_root(&state.config_dir, &media_cfg);
    // 重试遵循任务的「AI 文案提取」设置:决定视频是否下载并转音频
    let ai_extract = veltrix_core::db::entity::task::Entity::find_by_id(row.task_id.clone())
        .one(&state.db)
        .await
        .ok()
        .flatten()
        .map(|t| t.ai_extract)
        .unwrap_or(false);
    let mut content = content_from_model(&row);
    // 重试无绑定账号:取该平台一个可用账号的 Cookie 供 ffmpeg 拉流(防盗链 CDN 校验会话)
    let cookie = fetch_platform_cookie(&state.db, &row.platform).await;
    let mut outcome =
        crate::media::process_content(&content, &root, &media_cfg, ai_extract, cookie.as_deref())
            .await;

    // 视频转音频失败(典型:直链短期签名过期)→ 经详情页强制重取新鲜直链后再试一次。
    if ai_extract && content.kind == ContentKind::Video && outcome.audio_extracted == Some(false) {
        let platform_cfg = { lock_config(&state)?.platforms.get(&row.platform).cloned() };
        if let Some(cfg) = platform_cfg {
            if let Ok(acc) = state.cookies.acquire(&row.platform).await {
                let bridge = CollectBridge::new(
                    state.webviews.clone(),
                    state.intercept_channel.clone(),
                    state.rpa_channel.clone(),
                    state.collect_control.clone(),
                );
                let before = content.video_url.clone();
                let stream_params = StreamRefreshParams {
                    app: &app,
                    bridge: &bridge,
                    registry: &state.registry,
                    db: &state.db,
                    cfg: &cfg,
                    account_id: &acc.id,
                    task_id: &row.task_id,
                };
                refresh_stream_urls(
                    &stream_params,
                    std::slice::from_mut(&mut content),
                    true,
                )
                .await;
                // 直链确有刷新才重试,避免拿同一过期链接再失败一次。
                // 直链与会话绑定,口径要一致:从刷新直链那个账号(acc)的存活窗口读实时 Cookie
                // (含 httponly tt_chain_token;DB 里 acc.cookie 往往是空的,故必须读实时)
                if content.video_url != before {
                    let session_cookie =
                        resolve_session_cookie(&app, &state.db, &row.platform, &acc.id).await;
                    outcome = crate::media::process_content(
                        &content,
                        &root,
                        &media_cfg,
                        ai_extract,
                        session_cookie.as_deref(),
                    )
                    .await;
                }
            }
        }
    }
    record_media_outcome(&state.db, &id, &outcome).await;

    let status = if is_media_ok(&outcome) { "success" } else { "failed" };
    Ok(MediaStatusView {
        id,
        media_status: Some(status.to_string()),
        audio_extracted: outcome.audio_extracted,
        media_error: outcome.error,
    })
}

/// 失败任务补偿:对已采内容补做缺失的后处理(意向分析、素材下载+转写),按任务采集参数。
/// 仅 failed 任务;无已采内容时落 failed 并提示用「重新运行」重采。
/// 评论缺失需用「重新运行」(评论采集依赖 WebView,不在补偿范围)。
#[tauri::command]
pub async fn compensate_task(
    state: State<'_, AppState>,
    app: AppHandle,
    id: String,
) -> Result<()> {
    use veltrix_core::db::entity::task as task_entity;
    let me = current_user(&state).ok_or_else(|| CrawlerError::Config("未登录".into()))?;
    let model = task_entity::Entity::find_by_id(id.clone())
        .one(&state.db)
        .await
        .map_err(|e| CrawlerError::Config(format!("查询任务失败: {e}")))?
        .ok_or_else(|| CrawlerError::Config("任务不存在".into()))?;
    if model.status != "failed" {
        return Err(CrawlerError::Config("仅失败任务可补偿".into()));
    }
    if me.scope == "self" && model.owner != me.name {
        return Err(CrawlerError::Config("无权操作该任务".into()));
    }
    let ai_extract = model.ai_extract;
    let analyze_comment_intent = model.analyze_comment_intent;
    let media_cfg = { lock_config(&state)?.media.clone() };
    let transcription_cfg = { lock_config(&state)?.transcription.clone() };
    let intent_cfg = { lock_config(&state)?.intent.clone() };
    let config_dir = state.config_dir.clone();
    let db = state.db.clone();
    // 直链补取所需句柄:平台配置 + 采集桥 + 账号池(失败任务常因直链失效/小红书初采无直链)
    let platform = model.platform.clone();
    let platform_cfg = lock_config(&state)?.platforms.get(&platform).cloned();
    let registry = state.registry.clone();
    let bridge = CollectBridge::new(
        state.webviews.clone(),
        state.intercept_channel.clone(),
        state.rpa_channel.clone(),
        state.collect_control.clone(),
    );
    let cookies = state.cookies.clone();

    // 立即翻转为「素材下载中」,前端开始轮询;后台按采集参数补做
    write_task_downloading(&app, &db, &id, 0).await;

    tauri::async_runtime::spawn(async move {
        use sea_orm::sea_query::Expr;
        use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
        use veltrix_core::db::entity::content as content_entity;

        let rows = match content_entity::Entity::find()
            .filter(content_entity::Column::TaskId.eq(&id))
            .all(&db)
            .await
        {
            Ok(r) => r,
            Err(e) => {
                write_task_failed(&app, &db, &id, &format!("补偿查询内容失败: {e}")).await;
                return;
            }
        };
        if rows.is_empty() {
            write_task_failed(&app, &db, &id, "补偿:本任务无已采内容,请用「重新运行」重新采集")
                .await;
            return;
        }
        emit_collect_log(&app, &id, "info", format!("开始补偿 · 已采内容 {} 条", rows.len()));

        // 意向分析补做(analyze_comments_intent 内部按 intent_level IS NULL 幂等筛选)
        let intent_ready = analyze_comment_intent
            && !intent_cfg.api_url.trim().is_empty()
            && !intent_cfg.model.trim().is_empty();
        if intent_ready {
            write_task_analyzing(&app, &db, &id).await;
            analyze_comments_intent(&app, &db, &id, &intent_cfg).await;
            if let Err(e) = content_entity::Entity::update_many()
                .col_expr(content_entity::Column::IntentAnalyzed, Expr::value(true))
                .filter(content_entity::Column::TaskId.eq(&id))
                .filter(content_entity::Column::CommentCollected.eq(true))
                .exec(&db)
                .await
            {
                tracing::warn!("补偿:标记 intent_analyzed 失败: {e}");
            }
        }

        // 素材下载 + 转写补做(仅 ai_extract;filter_pending_media 排除已成功的)
        if ai_extract {
            let contents: Vec<Content> = rows.iter().map(content_from_model).collect();
            let mut pending = filter_pending_media(&db, &id, contents).await;
            if !pending.is_empty() {
                write_task_downloading(&app, &db, &id, pending.len() as i32).await;
                // 补直链:小红书无直链 / 缺直链的视频先经详情页拦截补取,再下载转音频。
                // 顺带记下补取用的账号,作为媒体下载日志写 HUD 的目标窗口。
                let mut hud_account = String::new();
                if let Some(cfg) = &platform_cfg {
                    if let Ok(acc) = cookies.acquire(&platform).await {
                        hud_account = acc.id.clone();
                        let stream_params = StreamRefreshParams {
                            app: &app,
                            bridge: &bridge,
                            registry: &registry,
                            db: &db,
                            cfg,
                            account_id: &acc.id,
                            task_id: &id,
                        };
                        refresh_stream_urls(
                            &stream_params, &mut pending, false,
                        )
                        .await;
                    }
                }
                let media_params = MediaDownloadParams {
                    app: &app,
                    db: &db,
                    task_id: &id,
                    platform: &platform,
                    account_id: &hud_account,
                    config_dir: &config_dir,
                    media_cfg: &media_cfg,
                    transcription_cfg: &transcription_cfg,
                    ai_extract,
                };
                download_media_for_contents(
                    &media_params,
                    pending,
                )
                .await; // 内部末尾会 write_task_done
                emit_collect_log(&app, &id, "info", "补偿完成");
                return;
            }
        }
        // 无素材可补 → 直接收尾为完成
        write_task_done(&app, &db, &id).await;
        emit_collect_log(&app, &id, "info", "补偿完成");
    });
    Ok(())
}

/// ffmpeg 探测结果:供前端在「AI 文案提取」处按是否已安装切换提示——已装则隐藏下载引导。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FfmpegStatus {
    /// 是否检测到可用的 ffmpeg。
    pub available: bool,
    /// 可用时的版本信息首行(形如 "ffmpeg version ...");不可用为 None。
    pub version: Option<String>,
}

/// 检测 ffmpeg 是否可用:用配置的 ffmpeg_path(空则系统 PATH 的 `ffmpeg`)跑一次 `-version`。
/// 探测是阻塞的进程调用,挪到阻塞线程池,避免占用异步运行时工作线程。
#[tauri::command]
pub async fn check_ffmpeg(state: State<'_, AppState>) -> Result<FfmpegStatus> {
    // clone 出路径再 spawn_blocking,避免把配置锁 guard 跨 await 持有
    let ffmpeg_path = { lock_config(&state)?.media.ffmpeg_path.clone() };
    let version = tauri::async_runtime::spawn_blocking(move || {
        crate::media::probe_ffmpeg(ffmpeg_path.as_deref())
    })
    .await
    .map_err(|e| CrawlerError::Config(format!("ffmpeg 探测任务失败: {e}")))?;
    // 顺带刷新录屏的 ffmpeg 可用性标记:用户手动检测后即时生效,免重启
    state.recording.set_ffmpeg_available(version.is_some());
    Ok(FfmpegStatus {
        available: version.is_some(),
        version,
    })
}


/// 把适配器解析出的内容/评论落库。调用方维护跨关键词去重集合,
/// 避免同任务多关键词命中同一条造成主键冲突。落库失败仅告警,不中断采集。
async fn persist_collected(
    db: &DatabaseConnection,
    task_id: &str,
    owner: &str,
    keyword: &str,
    output: FetchOutput,
    seen_contents: &mut HashSet<String>,
    seen_comments: &mut HashSet<String>,
) {
    persist_contents(db, task_id, owner, keyword, &output.contents, seen_contents).await;
    persist_comments(db, task_id, owner, &output.comments, seen_comments).await;
    persist_authors(db, owner, &output.contents).await;
}

/// 内容 upsert:按 content_id 去重,on_conflict 更新互动数与标题/文案。
async fn persist_contents(
    db: &DatabaseConnection,
    task_id: &str,
    owner: &str,
    keyword: &str,
    contents: &[Content],
    seen: &mut HashSet<String>,
) {
    use veltrix_core::db::entity::collect_record as ledger_entity;
    use veltrix_core::db::entity::content as content_entity;

    // 本运行内去重(seen)后的候选:(内容行主键, 内容引用)
    let candidates: Vec<(String, &Content)> = contents
        .iter()
        .filter_map(|c| {
            let id = format!("{task_id}-{}-{}", c.platform, c.content_id);
            if !seen.insert(id.clone()) {
                return None;
            }
            Some((id, c))
        })
        .collect();
    if candidates.is_empty() {
        return;
    }

    // 采集去重台账:查出本批哪些 (platform, content_id) 已登记。
    let ledger_ids: Vec<String> = candidates
        .iter()
        .map(|(_, c)| ledger_entity::ledger_key(&c.platform, &c.content_id))
        .collect();
    let recorded = load_recorded_ledger_ids(db, &ledger_ids).await;
    // 内容表中已存在的行主键:区分「同任务重跑」(行在 → 走 upsert 刷新统计)与
    // 「台账有记录但库里无此行」(被清空 / 他任务采过 → 视为重复,按需求不再保存)。
    let row_ids: Vec<String> = candidates.iter().map(|(id, _)| id.clone()).collect();
    let present = load_present_content_ids(db, &row_ids).await;

    let now = Utc::now().timestamp();
    let mut active_models: Vec<content_entity::ActiveModel> = Vec::with_capacity(candidates.len());
    let mut to_record: Vec<ledger_entity::ActiveModel> = Vec::new();
    for (id, c) in candidates {
        let key = ledger_entity::ledger_key(&c.platform, &c.content_id);
        let is_recorded = recorded.contains(&key);
        // 台账已登记 且 当前库里无对应行 → 曾采过(被清空 / 他任务采过),不再重复入库
        if is_recorded && !present.contains(&id) {
            continue;
        }
        active_models.push(content_to_active(id, c, task_id, keyword, owner));
        // 首次见到的内容登记进台账(已登记的不重复写)
        if !is_recorded {
            to_record.push(ledger_entity::ActiveModel {
                id: Set(key),
                platform: Set(c.platform.clone()),
                content_id: Set(c.content_id.clone()),
                created_at: Set(now),
            });
        }
    }
    if active_models.is_empty() {
        return;
    }
    // 判重 upsert:主键(task-平台-内容)已存在时刷新会随时间变化的字段(互动数 + 标题/文案),
    // 不重复插入。标题/文案也可能被作者编辑,一并刷新避免漂移。
    let on_conflict = sea_orm::sea_query::OnConflict::column(content_entity::Column::Id)
        .update_columns([
            content_entity::Column::LikeCount,
            content_entity::Column::CommentCount,
            content_entity::Column::CollectCount,
            content_entity::Column::ShareCount,
            content_entity::Column::PlayCount,
            content_entity::Column::Title,
            content_entity::Column::Desc,
            // 不刷新 collected_at:保留首次采集时间,使采集明细「collected_at >= 本次 started_at」
            // 恰好只统计本次新增内容,重复采到的已有内容(首次时间早)被排除。
        ])
        .to_owned();
    // 先整批 upsert(快路径);整批失败时降级逐条 upsert,避免一条冲突 / 坏数据
    // 拖垮整条 insert 语句、把本批已采内容全部丢弃(「采集失败也要保住已采数据」)。
    if let Err(e) = content_entity::Entity::insert_many(active_models.clone())
        .on_conflict(on_conflict.clone())
        .exec(db)
        .await
    {
        tracing::warn!("批量落库采集内容失败,降级逐条保存: {e}");
        let (mut ok, mut lost) = (0usize, 0usize);
        for am in active_models {
            match content_entity::Entity::insert(am)
                .on_conflict(on_conflict.clone())
                .exec(db)
                .await
            {
                Ok(_) => ok += 1,
                Err(e2) => {
                    lost += 1;
                    tracing::warn!("逐条落库内容失败(跳过该条): {e2}");
                }
            }
        }
        tracing::warn!("内容降级保存完成 · 成功 {ok} 条 · 丢弃 {lost} 条");
    }

    // 登记采集去重台账(尽力而为):主键冲突忽略;台账写失败不影响采集主流程。
    // 注:即便上面个别内容落库失败也照样登记,极端情况下该条不会被未来重采补回——概率低,可接受。
    if !to_record.is_empty() {
        let on_conflict_ledger =
            sea_orm::sea_query::OnConflict::column(ledger_entity::Column::Id)
                .do_nothing()
                .to_owned();
        match ledger_entity::Entity::insert_many(to_record)
            .on_conflict(on_conflict_ledger)
            .exec(db)
            .await
        {
            Ok(_) | Err(sea_orm::DbErr::RecordNotInserted) => {}
            Err(e) => tracing::warn!("写入采集去重台账失败(忽略): {e}"),
        }
    }
}

/// 查给定主键集合中已登记在采集去重台账里的 id。空集合直接返回空,查询失败按空处理(不阻断采集)。
async fn load_recorded_ledger_ids(db: &DatabaseConnection, ids: &[String]) -> HashSet<String> {
    use sea_orm::{ColumnTrait, QueryFilter, QuerySelect};
    use veltrix_core::db::entity::collect_record as ledger_entity;
    if ids.is_empty() {
        return HashSet::new();
    }
    ledger_entity::Entity::find()
        .filter(ledger_entity::Column::Id.is_in(ids.iter().cloned()))
        .select_only()
        .column(ledger_entity::Column::Id)
        .into_tuple::<String>()
        .all(db)
        .await
        .unwrap_or_else(|e| {
            tracing::warn!("查询采集去重台账失败: {e}");
            Vec::new()
        })
        .into_iter()
        .collect()
}

/// 查给定行主键中已存在于 contents 表的 id(用于区分同任务重跑刷新 vs 清空后重复)。
/// 空集合直接返回空,查询失败按空处理。
async fn load_present_content_ids(db: &DatabaseConnection, ids: &[String]) -> HashSet<String> {
    use sea_orm::{ColumnTrait, QueryFilter, QuerySelect};
    use veltrix_core::db::entity::content as content_entity;
    if ids.is_empty() {
        return HashSet::new();
    }
    content_entity::Entity::find()
        .filter(content_entity::Column::Id.is_in(ids.iter().cloned()))
        .select_only()
        .column(content_entity::Column::Id)
        .into_tuple::<String>()
        .all(db)
        .await
        .unwrap_or_else(|e| {
            tracing::warn!("查询已存在内容失败: {e}");
            Vec::new()
        })
        .into_iter()
        .collect()
}

/// 评论 upsert:按 comment_id 去重,on_conflict 更新点赞/回复数。
async fn persist_comments(
    db: &DatabaseConnection,
    task_id: &str,
    owner: &str,
    comments: &[Comment],
    seen: &mut HashSet<String>,
) {
    use veltrix_core::db::entity::comment as comment_entity;

    let active_models: Vec<comment_entity::ActiveModel> = comments
        .iter()
        .filter_map(|c| {
            let id = format!("{task_id}-{}-{}", c.platform, c.comment_id);
            if !seen.insert(id.clone()) {
                return None;
            }
            Some(comment_to_active(id, c, task_id, owner))
        })
        .collect();
    if active_models.is_empty() {
        return;
    }
    // 评论同样判重 upsert:已存在时刷新点赞 / 回复数
    let on_conflict = sea_orm::sea_query::OnConflict::column(comment_entity::Column::Id)
        .update_columns([
            comment_entity::Column::LikeCount,
            comment_entity::Column::ReplyCount,
            // 同 content:不刷新 collected_at,采集明细只统计本次新增、排除重复采到的已有评论
        ])
        .to_owned();
    // 同内容:整批失败降级逐条,保住其余已采评论
    if let Err(e) = comment_entity::Entity::insert_many(active_models.clone())
        .on_conflict(on_conflict.clone())
        .exec(db)
        .await
    {
        tracing::warn!("批量落库采集评论失败,降级逐条保存: {e}");
        let (mut ok, mut lost) = (0usize, 0usize);
        for am in active_models {
            match comment_entity::Entity::insert(am)
                .on_conflict(on_conflict.clone())
                .exec(db)
                .await
            {
                Ok(_) => ok += 1,
                Err(e2) => {
                    lost += 1;
                    tracing::warn!("逐条落库评论失败(跳过该条): {e2}");
                }
            }
        }
        tracing::warn!("评论降级保存完成 · 成功 {ok} 条 · 丢弃 {lost} 条");
    }
}

/// 作者档案 upsert:含 7 天节流刷新画像。新作者建档;已有作者距上次采集超过 7 天
/// 才刷新画像(粉丝/获赞/签名等),7 天内不动,避免每次采集都写库。
/// first_collected_at 与 is_monitored 始终保留。
async fn persist_authors(db: &DatabaseConnection, owner: &str, contents: &[Content]) {
    use veltrix_core::db::entity::author as author_entity;

    const AUTHOR_REFRESH_SECS: i64 = 7 * 24 * 3600;
    let now = Utc::now().timestamp();
    let mut seen_authors: HashSet<String> = HashSet::new();
    for c in contents {
        let a = &c.author;
        if a.uid.is_empty() {
            continue;
        }
        let aid = format!("{owner}-{}-{}", a.platform, a.uid);
        if !seen_authors.insert(aid.clone()) {
            continue;
        }
        let existing = author_entity::Entity::find_by_id(aid.clone())
            .one(db)
            .await
            .ok()
            .flatten();
        match existing {
            None => {
                if let Err(e) = author_to_active(&aid, a, owner, now).insert(db).await {
                    tracing::warn!("建作者档案失败: {e}");
                }
            }
            Some(m) if now - m.last_collected_at >= AUTHOR_REFRESH_SECS => {
                let fresh = author_to_active(&aid, a, owner, now);
                let mut am = m.into_active_model();
                am.nickname = fresh.nickname;
                am.avatar = fresh.avatar;
                am.platform_id = fresh.platform_id;
                am.short_id = fresh.short_id;
                am.signature = fresh.signature;
                am.follower_count = fresh.follower_count;
                am.following_count = fresh.following_count;
                am.total_favorited = fresh.total_favorited;
                am.location = fresh.location;
                am.last_collected_at = Set(now);
                if let Err(e) = am.update(db).await {
                    tracing::warn!("刷新作者档案失败: {e}");
                }
            }
            Some(_) => { /* 7 天内,跳过不更新 */ }
        }
    }
}

/// model::Author → authors 表 ActiveModel。平台号/属地/获赞从 author.extra 取(各适配器按需填)。
fn author_to_active(
    id: &str,
    a: &crate::model::Author,
    owner: &str,
    now: i64,
) -> veltrix_core::db::entity::author::ActiveModel {
    let extra_str = |key: &str| {
        a.extra
            .get(key)
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    let extra_i64 = |key: &str| a.extra.get(key).and_then(|v| v.as_i64());
    veltrix_core::db::entity::author::ActiveModel {
        id: Set(id.to_string()),
        owner: Set(owner.to_string()),
        platform: Set(a.platform.clone()),
        uid: Set(a.uid.clone()),
        nickname: Set(a.nickname.clone()),
        avatar: Set(a.avatar.clone()),
        platform_id: Set(extra_str("unique_id")),
        short_id: Set(extra_str("uid")),
        signature: Set(a.signature.clone()),
        follower_count: Set(a.follower_count),
        following_count: Set(a.following_count),
        total_favorited: Set(extra_i64("total_favorited")),
        location: Set(extra_str("ip_location")),
        is_monitored: Set(false),
        is_blacklisted: Set(false),
        first_collected_at: Set(now),
        last_collected_at: Set(now),
    }
}

/// 把可序列化值转 JSON 文本;失败回退 "null",不让单条脏字段中断整批落库。
fn to_json_text<T: Serialize>(value: &T) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "null".to_string())
}


/// model::Content → contents 实体 ActiveModel。复合字段(作者/图片/扩展)序列化为 JSON 文本。
fn content_to_active(
    id: String,
    c: &Content,
    task_id: &str,
    keyword: &str,
    owner: &str,
) -> veltrix_core::db::entity::content::ActiveModel {
    use veltrix_core::db::entity::content as content_entity;
    let kind = match c.kind {
        ContentKind::Video => "video",
        ContentKind::Image => "image",
        ContentKind::Article => "article",
        ContentKind::Unknown => "unknown",
    };
    content_entity::ActiveModel {
        id: Set(id),
        task_id: Set(task_id.to_string()),
        platform: Set(c.platform.clone()),
        content_id: Set(c.content_id.clone()),
        keyword: Set(keyword.to_string()),
        kind: Set(kind.to_string()),
        title: Set(c.title.clone()),
        desc: Set(c.desc.clone()),
        author_uid: Set(c.author.uid.clone()),
        author_nickname: Set(c.author.nickname.clone()),
        author_json: Set(to_json_text(&c.author)),
        like_count: Set(c.stats.like_count),
        comment_count: Set(c.stats.comment_count),
        collect_count: Set(c.stats.collect_count),
        share_count: Set(c.stats.share_count),
        play_count: Set(c.stats.play_count),
        published_at: Set(c.published_at),
        video_url: Set(c.video_url.clone()),
        cover_url: Set(c.cover_url.clone()),
        image_urls: Set(to_json_text(&c.image_urls)),
        duration: Set(c.duration),
        topics: Set(to_json_text(&c.topics)),
        extra: Set(to_json_text(&c.extra)),
        owner: Set(owner.to_string()),
        collected_at: Set(c.collected_at),
        // 初始置「待处理」,素材下载完成后由 record_media_outcome 回写成败
        media_status: Set(Some("pending".to_string())),
        audio_extracted: Set(None),
        media_error: Set(None),
        // 本地素材路径采集时未知,素材下载成功后回写
        cover_path: Set(None),
        avatar_path: Set(None),
        audio_path: Set(None),
        // 转写文本采集时未知,语音转写后回写
        transcript: Set(None),
        transcript_error: Set(None),
        // 细粒度处理状态:媒体下载/评论采集/意向分析后回写
        video_downloaded: Set(None),
        image_total: Set(None),
        image_done: Set(None),
        comment_collected: Set(None),
        intent_analyzed: Set(None),
    }
}

/// model::Comment → comments 实体 ActiveModel。
fn comment_to_active(
    id: String,
    c: &Comment,
    task_id: &str,
    owner: &str,
) -> veltrix_core::db::entity::comment::ActiveModel {
    use veltrix_core::db::entity::comment as comment_entity;
    comment_entity::ActiveModel {
        id: Set(id),
        task_id: Set(task_id.to_string()),
        platform: Set(c.platform.clone()),
        content_id: Set(c.content_id.clone()),
        comment_id: Set(c.comment_id.clone()),
        parent_id: Set(c.parent_id.clone()),
        author_uid: Set(c.author.uid.clone()),
        author_nickname: Set(c.author.nickname.clone()),
        author_json: Set(to_json_text(&c.author)),
        text: Set(c.text.clone()),
        like_count: Set(c.like_count),
        reply_count: Set(c.reply_count),
        created_at: Set(c.created_at),
        owner: Set(owner.to_string()),
        collected_at: Set(c.collected_at),
        // 新采集评论尚未分析,意向字段留空,待意向分析阶段回写
        intent_level: Set(None),
        intent_reason: Set(None),
    }
}

/// 标记任务完成(status=completed, progress=100, finished_at)。
async fn write_task_done(app: &AppHandle, db: &DatabaseConnection, task_id: &str) {
    use veltrix_core::db::entity::task as task_entity;
    if let Ok(Some(m)) = task_entity::Entity::find_by_id(task_id.to_string()).one(db).await {
        let now = Utc::now().timestamp();
        let mut am = m.into_active_model();
        am.status = Set("completed".to_string());
        am.progress = Set(100);
        am.finished_at = Set(Some(now));
        am.updated_at = Set(now);
        match am.update(db).await {
            Ok(updated) => emit_task_progress(app, updated),
            Err(e) => tracing::warn!("标记任务完成失败: {e}"),
        }
    }
}

/// 评论发布时间范围转 Unix 秒下限;`any` / 空 / 未知值返回 None(不按时间过滤)。
fn comment_time_cutoff(range: &str) -> Option<i64> {
    let days: i64 = match range {
        "3d" => 3,
        "7d" => 7,
        "14d" => 14,
        _ => return None,
    };
    Some(Utc::now().timestamp() - days * 24 * 3600)
}

/// 过滤评论:按发布时间下限保留 + 按单视频上限截断。
/// cutoff=None 不按时间过滤;limit=0 不截断;created_at 缺失的评论保留(不因无时间而误删)。
fn filter_comments(mut comments: Vec<Comment>, cutoff: Option<i64>, limit: usize) -> Vec<Comment> {
    if let Some(min_ts) = cutoff {
        comments.retain(|c| c.created_at.map(|t| t >= min_ts).unwrap_or(true));
    }
    if limit > 0 && comments.len() > limit {
        comments.truncate(limit);
    }
    comments
}

/// 标记任务进入评论采集态(status=collecting_comments,记录待采视频总数,清零已采)。
/// 内容采集已结束但评论未采完时调用,调度页据此显示「评论采集中 done/total」;不写 finished_at。
async fn write_task_collecting_comments(
    app: &AppHandle,
    db: &DatabaseConnection,
    task_id: &str,
    video_total: i32,
) {
    use veltrix_core::db::entity::task as task_entity;
    if let Ok(Some(m)) = task_entity::Entity::find_by_id(task_id.to_string()).one(db).await {
        let now = Utc::now().timestamp();
        let mut am = m.into_active_model();
        am.status = Set("collecting_comments".to_string());
        am.comment_video_total = Set(video_total);
        am.comment_video_done = Set(0);
        am.updated_at = Set(now);
        match am.update(db).await {
            Ok(updated) => emit_task_progress(app, updated),
            Err(e) => tracing::warn!("标记任务评论采集态失败: {e}"),
        }
    }
}

/// 回写评论采集进度(已采视频数 comment_video_done + 累计评论数 comment_count)。失败仅告警。
async fn write_task_comment_progress(
    app: &AppHandle,
    db: &DatabaseConnection,
    task_id: &str,
    video_done: i32,
    comment_count: i64,
) {
    use veltrix_core::db::entity::task as task_entity;
    if let Ok(Some(m)) = task_entity::Entity::find_by_id(task_id.to_string()).one(db).await {
        let mut am = m.into_active_model();
        am.comment_video_done = Set(video_done);
        am.comment_count = Set(comment_count);
        am.updated_at = Set(Utc::now().timestamp());
        match am.update(db).await {
            Ok(updated) => emit_task_progress(app, updated),
            Err(e) => tracing::warn!("回写评论采集进度失败: {e}"),
        }
    }
}

/// 标记任务进入意向分析态(status=analyzing_comments)。不写 finished_at。
async fn write_task_analyzing(app: &AppHandle, db: &DatabaseConnection, task_id: &str) {
    use veltrix_core::db::entity::task as task_entity;
    if let Ok(Some(m)) = task_entity::Entity::find_by_id(task_id.to_string()).one(db).await {
        let mut am = m.into_active_model();
        am.status = Set("analyzing_comments".to_string());
        am.updated_at = Set(Utc::now().timestamp());
        match am.update(db).await {
            Ok(updated) => emit_task_progress(app, updated),
            Err(e) => tracing::warn!("标记任务意向分析态失败: {e}"),
        }
    }
}

/// 对任务评论分批做意向分析并写回 comment.intent_*。读 intent 配置引用的厂商 / 提示词,
/// 按 batch_size 分批调 LLM。任一环节失败仅告警,不影响任务终态。
async fn analyze_comments_intent(
    app: &AppHandle,
    db: &DatabaseConnection,
    task_id: &str,
    intent_cfg: &veltrix_core::config::CommentIntentConfig,
) {
    use veltrix_core::db::entity::{
        comment as comment_entity,
    };

    // 未配置 API Key 直接跳过——否则空 Bearer 会让每个批次 401 全失败、0 条评论被标注,
    // 还无意义地刷一串「批次失败」(早返回守卫在重构中被误删,这里补回)。
    let api_key = get_secret(db, "intent_api_key").await;
    if api_key.trim().is_empty() {
        tracing::warn!("意向分析未配置 API Key,跳过本任务意向分析(task {task_id})");
        return;
    }
    // 提示词(可选;未配置 / 为空则用内置默认)
    let configured_prompt = intent_cfg.intent_prompt.clone();
    let system_prompt = if configured_prompt.trim().is_empty() {
        "你是评论意向分析助手,判断每条评论作者的购买 / 咨询 / 合作意向强度。".to_string()
    } else {
        configured_prompt
    };

    // 取本任务尚未分析(intent_level 为空)的评论
    let rows = match comment_entity::Entity::find()
        .filter(comment_entity::Column::TaskId.eq(task_id))
        .filter(comment_entity::Column::IntentLevel.is_null())
        .all(db)
        .await
    {
        Ok(r) => r,
        Err(e) => {
            emit_collect_log(app, task_id, "warn", format!("查询待分析评论失败: {e}"));
            return;
        }
    };
    if rows.is_empty() {
        emit_collect_log(app, task_id, "info", "无待分析评论");
        return;
    }

    let batch_size = if intent_cfg.batch_size > 0 {
        intent_cfg.batch_size as usize
    } else {
        20
    };
    let total = rows.len();
    emit_collect_log(
        app,
        task_id,
        "info",
        format!("开始意向分析 · 共 {total} 条 · 批大小 {batch_size}"),
    );

    // 多批次并发调接口:buffer_unordered 限并发,避免打爆 rate limit;
    // LLM 调用并行收集结果,DB 回写延后串行(SQLite 写需串行,规避并发锁冲突)。
    // chunk / 提示词等一律 clone 成 owned 移入 future,避免借用栈变量引发 FnOnce 生命周期不通用。
    use futures_util::StreamExt;
    const MAX_CONCURRENCY: usize = 4;
    let chunks: Vec<Vec<comment_entity::Model>> =
        rows.chunks(batch_size).map(|c| c.to_vec()).collect();
    let batch_total = chunks.len();
    let returned = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let batch_results: Vec<(Vec<comment_entity::Model>, Vec<crate::llm::IntentVerdict>)> =
        futures_util::stream::iter(chunks)
            .map(|chunk| {
                let returned = std::sync::Arc::clone(&returned);
                let api_url = intent_cfg.api_url.clone();
                let api_key = api_key.clone();
                let model = intent_cfg.model.clone();
                let system_prompt = system_prompt.clone();
                let app = app.clone();
                let task_id = task_id.to_string();
                async move {
                    let batch: Vec<(String, String)> = chunk
                        .iter()
                        .map(|c| (c.comment_id.clone(), c.text.clone()))
                        .collect();
                    let verdicts = match crate::llm::analyze_intent(crate::llm::IntentRequest {
                        api_url: &api_url,
                        api_key: &api_key,
                        model: &model,
                        system_prompt: &system_prompt,
                        comments: &batch,
                    })
                    .await
                    {
                        Ok(v) => v,
                        Err(e) => {
                            emit_collect_log(&app, &task_id, "warn", format!("意向分析批次失败: {e}"));
                            Vec::new()
                        }
                    };
                    let done = returned.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
                    emit_collect_log(&app, &task_id, "info", format!("意向分析批次 {done}/{batch_total} 已返回"));
                    (chunk, verdicts)
                }
            })
            .buffer_unordered(MAX_CONCURRENCY)
            .collect()
            .await;

    // 回写:按批次收集结果后批量 SQL 更新,单条 UPDATE 替代 N 次逐条回写
    let mut analyzed = 0usize;
    for (chunk, verdicts) in batch_results {
        let verdict_map: std::collections::HashMap<String, crate::llm::IntentVerdict> = verdicts
            .into_iter()
            .map(|v| (v.comment_id.clone(), v))
            .collect();
        let updates: Vec<(String, String, String)> = chunk
            .iter()
            .filter_map(|c| {
                verdict_map.get(&c.comment_id).map(|v| {
                    (c.id.clone(), v.level.clone(), v.reason.clone())
                })
            })
            .collect();
        if !updates.is_empty() {
            use sea_orm::{ConnectionTrait, Statement};
            let backend = db.get_database_backend();
            let is_pg = matches!(backend, sea_orm::DatabaseBackend::Postgres);
            let mut level_cases = String::from("CASE id");
            let mut reason_cases = String::from("CASE id");
            let mut in_list = String::new();
            // PG 用 $N 可复用同一参数;SQLite ? 每个位置需独立参数,故 id 重复 3 次
            let cap = if is_pg { updates.len() * 3 } else { updates.len() * 5 };
            let mut params: Vec<sea_orm::Value> = Vec::with_capacity(cap);
            if is_pg {
                for (i, (id, level, reason)) in updates.iter().enumerate() {
                    let pi = params.len();
                    let (ph1, ph2, ph3) = (
                        format!("${}", pi + 1),
                        format!("${}", pi + 2),
                        format!("${}", pi + 3),
                    );
                    level_cases.push_str(&format!(" WHEN {ph1} THEN {ph2}"));
                    reason_cases.push_str(&format!(" WHEN {ph1} THEN {ph3}"));
                    if i > 0 {
                        in_list.push_str(", ");
                    }
                    in_list.push_str(&ph1);
                    params.push(id.clone().into());
                    params.push(level.clone().into());
                    params.push(reason.clone().into());
                }
            } else {
                // SQLite 的 `?` 按出现顺序绑定,故参数必须与 SQL 文本顺序一致:
                // 先全部 level-CASE 的 (id,level),再全部 reason-CASE 的 (id,reason),最后 WHERE IN 的 id。
                // 原先按行交错 push(id,level,id,reason,id)会与分组后的占位符错位,导致绑定全乱。
                let mut level_params: Vec<sea_orm::Value> = Vec::with_capacity(updates.len() * 2);
                let mut reason_params: Vec<sea_orm::Value> = Vec::with_capacity(updates.len() * 2);
                let mut in_params: Vec<sea_orm::Value> = Vec::with_capacity(updates.len());
                for (i, (id, level, reason)) in updates.iter().enumerate() {
                    level_cases.push_str(" WHEN ? THEN ?");
                    reason_cases.push_str(" WHEN ? THEN ?");
                    if i > 0 {
                        in_list.push_str(", ");
                    }
                    in_list.push('?');
                    level_params.push(id.clone().into());
                    level_params.push(level.clone().into());
                    reason_params.push(id.clone().into());
                    reason_params.push(reason.clone().into());
                    in_params.push(id.clone().into());
                }
                params.extend(level_params);
                params.extend(reason_params);
                params.extend(in_params);
            }
            level_cases.push_str(" END");
            reason_cases.push_str(" END");
            let sql = format!(
                "UPDATE comments SET intent_level = {level_cases}, intent_reason = {reason_cases} WHERE id IN ({in_list})",
            );
            if let Err(e) = db.execute(Statement::from_sql_and_values(backend, sql, params)).await {
                tracing::warn!("批量回写意向失败(影响 {} 条): {e}", updates.len());
            }
        }
        analyzed += chunk.len();
    }
    emit_collect_log(app, task_id, "info", format!("意向分析进度 {analyzed}/{total}"));
    emit_collect_log(
        app,
        task_id,
        "info",
        format!("意向分析完成 · 已处理 {analyzed} 条"),
    );
}

/// 标记任务进入素材下载态(status=downloading_media, progress=100, 记录素材总数,清零已处理数)。
/// 采集主体已结束但素材未下完时调用,调度页据此显示「素材下载中 done/total」;不写 finished_at。
async fn write_task_downloading(
    app: &AppHandle,
    db: &DatabaseConnection,
    task_id: &str,
    media_total: i32,
) {
    use veltrix_core::db::entity::task as task_entity;
    if let Ok(Some(m)) = task_entity::Entity::find_by_id(task_id.to_string()).one(db).await {
        let now = Utc::now().timestamp();
        let mut am = m.into_active_model();
        am.status = Set("downloading_media".to_string());
        am.progress = Set(100);
        am.media_total = Set(media_total);
        am.media_done = Set(0);
        am.updated_at = Set(now);
        match am.update(db).await {
            Ok(updated) => emit_task_progress(app, updated),
            Err(e) => tracing::warn!("标记任务素材下载态失败: {e}"),
        }
    }
}

/// 回写素材下载进度(仅更新 media_done)。失败仅告警,不中断下载循环。
async fn write_task_media_done(
    app: &AppHandle,
    db: &DatabaseConnection,
    task_id: &str,
    media_done: i32,
) {
    use veltrix_core::db::entity::task as task_entity;
    if let Ok(Some(m)) = task_entity::Entity::find_by_id(task_id.to_string()).one(db).await {
        let mut am = m.into_active_model();
        am.media_done = Set(media_done);
        am.updated_at = Set(Utc::now().timestamp());
        match am.update(db).await {
            Ok(updated) => emit_task_progress(app, updated),
            Err(e) => tracing::warn!("回写素材进度失败: {e}"),
        }
    }
}

/// 标记任务取消(status=cancelled, finished_at, error_message)。
/// 用户主动终止(如采集途中手动关闭采集窗口)时调用;cancelled 为终态,监听任务据此停止自动轮转。
async fn write_task_cancelled(app: &AppHandle, db: &DatabaseConnection, task_id: &str, message: &str) {
    use veltrix_core::db::entity::task as task_entity;
    if let Ok(Some(m)) = task_entity::Entity::find_by_id(task_id.to_string()).one(db).await {
        let now = Utc::now().timestamp();
        let mut am = m.into_active_model();
        am.status = Set("cancelled".to_string());
        am.finished_at = Set(Some(now));
        am.error_message = Set(Some(message.to_string()));
        am.updated_at = Set(now);
        match am.update(db).await {
            Ok(updated) => emit_task_progress(app, updated),
            Err(e) => tracing::warn!("标记任务取消状态失败: {e}"),
        }
    }
}

/// 标记任务失败(status=failed, finished_at, error_message)。
/// 采集零产出且过程出错时调用,避免失败任务被误标「已完成」。
async fn write_task_failed(app: &AppHandle, db: &DatabaseConnection, task_id: &str, message: &str) {
    use veltrix_core::db::entity::task as task_entity;
    if let Ok(Some(m)) = task_entity::Entity::find_by_id(task_id.to_string()).one(db).await {
        let now = Utc::now().timestamp();
        let mut am = m.into_active_model();
        am.status = Set("failed".to_string());
        am.finished_at = Set(Some(now));
        am.error_message = Set(Some(message.to_string()));
        am.updated_at = Set(now);
        match am.update(db).await {
            Ok(updated) => emit_task_progress(app, updated),
            Err(e) => tracing::warn!("标记任务失败状态失败: {e}"),
        }
    }
}
