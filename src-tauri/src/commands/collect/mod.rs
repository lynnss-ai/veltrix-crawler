//! 采集执行引擎:从前端触发采集,到落库 / 媒体下载 / 语音转写 / 意向分析 /
//! Obsidian 同步的完整流水线编排。
//!
//! 设计分层:控制面(应用状态、系统配置、账号 / 平台命令)留在 `commands` 模块根;
//! 此处只承载「数据面」——采集任务的执行调度与持久化,逻辑虽长但内聚单一职责。

use super::{account_collect_lock, account_lock_key, current_user, get_secret, lock_config, AppState};
use crate::adapter::{FetchContext, FetchOutput};
use crate::cookie::CookiePool;
use crate::model::{Author, Comment, Content, ContentKind, TaskKind};
use crate::webview::pool::{
    CollectBridge, CollectRequest, CollectStop, CommentCollectRequest, DetailFetchRequest,
    DirectCollectRequest, ProfilePostsCollectRequest,
};
use crate::webview::{emit_collect_entry, emit_collect_log, CollectEntry, RpaOutcome};
use chrono::Utc;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, IntoActiveModel, QueryFilter,
    QueryOrder, Set, TransactionTrait,
};
use serde::Serialize;
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, State};
use veltrix_core::error::{CrawlerError, Result};
mod obsidian;
mod enrich;
pub use obsidian::*;
pub use enrich::*;

/// 关键词采集阶段的共享状态,由 run_task_body 维护,传递给子阶段函数。
struct CollectSharedState {
    seen_contents: HashSet<String>,
    seen_comments: HashSet<String>,
    intercepted_total: i64,
    /// 适配器解析失败的次数(搜索 / 定向 / 评论各解析入口累加),供运行指标与排查
    parse_failures: usize,
    had_error: bool,
    contents_for_media: Vec<Content>,
    /// 采集途中用户手动关闭采集窗口 → 取消任务标记(见 run_task_body 据此收尾,不再后处理)。
    window_closed: bool,
    /// 采集途中用户点了 HUD「结束」→ 停止后续关键词/评论,但仍下载已采素材并正常完成。
    user_ended: bool,
}

/// 单次运行的采集指标,收尾时序列化进 task_runs.metrics_json(事后排查 / 横向对比)。
#[derive(Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct RunMetrics {
    /// 拦截到的接口响应总数(原生缓冲 + 页面通道合并,含重复)
    intercepted: i64,
    /// 适配器解析失败次数(搜索 / 定向 / 评论)
    parse_failures: usize,
    /// 本次运行去重后落库的内容数
    persisted_contents: usize,
    /// 本次运行去重后落库的评论数
    persisted_comments: usize,
    /// 各阶段耗时(毫秒):collect / comments / enrich / media / total
    stages_ms: std::collections::HashMap<String, u64>,
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
    /// 音频提取(视频下载 + 转 mp3);含 AI 文案提取隐含的音频需求
    audio_extract: bool,
    /// 任务停止标记来源:素材下载 / 语音转写阶段据此中断(每完成一条检查一次)
    bridge: &'a CollectBridge,
    /// AI 文案提取:素材结束后对音频做语音转写
    ai_extract: bool,
    /// 关窗前解析并留存的会话 Cookie(见 resolve_session_cookie);有则素材下载直接复用,
    /// 无则下载时现场解析(补偿/重试等窗口可能仍存活的路径)
    session_cookie: Option<String>,
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

/// 抖音评论 API 直采的页内脚本完成回调:回传本次直采结果(JSON 字符串),
/// 存 collect_control 由 pool 侧轮询取走(与 intercept_push 同属页面 → Rust 回传通道)。
#[tauri::command]
pub fn comment_api_done(state: State<'_, AppState>, session_id: u64, result: String) {
    state.collect_control.set_api_done(session_id, result);
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
/// 命令立即返回,采集在后台进行,前端轮询 `list_tasks` 看进度;
/// 拦截 / 落库 / 解析失败等计数按次写入 task_runs 运行指标(见 finalize_task_run)。
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
    // JSON 解析失败按空处理会误报「未配置关键词」,掩盖数据损坏——单独报错
    let keywords: Vec<String> = serde_json::from_str(&model.keywords)
        .map_err(|e| CrawlerError::Config(format!("任务关键词数据损坏: {e}")))?;
    // 定向采集目标链接(视频/主页链接);定向任务 keywords 只存占位词「定向采集」,故非空校验要看两者
    let target_urls: Vec<String> = serde_json::from_str(&model.target_urls)
        .map_err(|e| CrawlerError::Config(format!("任务定向目标数据损坏: {e}")))?;
    if keywords.is_empty() && target_urls.is_empty() {
        return Err(CrawlerError::Config("任务未配置关键词或定向目标".into()));
    }

    // 选该平台一个可用账号并占用(轮换「最久未用」+ 乐观 CAS,自动恢复冷却到期账号);
    // account_id 作为采集窗口的隔离 key(对应独立 WebView2 数据目录)。
    // 改用 acquire 替代「永远取第一个 active」,真正实现多账号负载分摊与风控分压。
    let account = state.cookies.acquire(&platform).await.map_err(|e| {
        // 保留底层原因:并发争用/账号冷却等与「无可用账号」是不同问题,吞掉会误导排查
        CrawlerError::Config(format!(
            "平台 {platform} 获取可用账号失败: {e}(若无账号请先在账号管理添加并登录)"
        ))
    })?;
    let account_id = account.id;

    // 采集窗口标题用任务名(平台名称 - 任务名称);任务名为空(或纯空白)时回退账号 id,保证可辨识
    let task_name = if model.name.trim().is_empty() {
        account_id.clone()
    } else {
        model.name.trim().to_string()
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
    // 音频提取:开 → 视频下载并转音频(mp3 留存);AI 文案提取依赖音频(upsert 已强制,这里 || 兜底)。
    // 两者皆关 → 视频不下载、不存储
    let audio_extract = model.audio_extract || model.ai_extract;
    // AI 文案提取:开 → 素材阶段结束后对音频做语音转写;关 → 只留音频不转写
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

    // 先标记 running + started_at,前端立即看到状态翻转。
    // 原子防重启动:UPDATE 自带「当前不在进行中」条件,影响 0 行 = 已被并发请求抢占,拒绝。
    // (此前的「先查状态、后写 running」是 check-then-act,双击「运行」或与调度器同时触发时
    // 两个请求都能通过检查各自 spawn,同一任务双跑、双写进度)
    let now = Utc::now().timestamp();
    {
        use sea_orm::sea_query::Expr;
        use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
        let res = task_entity::Entity::update_many()
            .col_expr(task_entity::Column::Status, Expr::value("running"))
            .col_expr(task_entity::Column::StartedAt, Expr::value(now))
            .col_expr(task_entity::Column::Progress, Expr::value(0))
            .col_expr(task_entity::Column::UpdatedAt, Expr::value(now))
            .filter(task_entity::Column::Id.eq(task_id.clone()))
            .filter(task_entity::Column::Status.is_not_in([
                "running",
                "collecting_comments",
                "analyzing_comments",
                "downloading_media",
            ]))
            .exec(&state.db)
            .await
            .map_err(|e| CrawlerError::Config(format!("更新任务状态失败: {e}")))?;
        if res.rows_affected == 0 {
            return Err(CrawlerError::Config("任务正在进行中,请勿重复启动".into()));
        }
    }
    // 自动重试序列管理:调度器按 next_retry_at 拉起的重试保留 retry_count 以累计次数;
    // 其余启动(手动 / 定时 / 监听)视为新序列,清零重试计数与排期。
    if !(model.status == "failed" && model.next_retry_at.is_some()) {
        // 清零失败仅告警:重试计数错位会影响退避与上限判定,必须留痕
        if let Err(e) = task_entity::Entity::update_many()
            .col_expr(
                task_entity::Column::RetryCount,
                sea_orm::sea_query::Expr::value(0),
            )
            .col_expr(
                task_entity::Column::NextRetryAt,
                sea_orm::sea_query::Expr::value(None::<i64>),
            )
            .filter(task_entity::Column::Id.eq(task_id.clone()))
            .exec(&state.db)
            .await
        {
            tracing::warn!(task_id = %task_id, "清零重试计数失败: {e}");
        }
    }
    // 回读更新后的任务行用于进度推送(update_many 不返回模型)。
    // 注意:此处失败必须回滚状态——上面原子 UPDATE 已把任务翻成 running,
    // 若直接 return,任务会永久卡在 running(调度器跳过进行中任务、防重 guard 拒绝重启)。
    let started = match task_entity::Entity::find_by_id(task_id.clone())
        .one(&state.db)
        .await
    {
        Ok(Some(m)) => m,
        other => {
            let reason = match other {
                Ok(None) => format!("任务不存在: {task_id}"),
                Err(e) => format!("查询任务失败: {e}"),
                Ok(Some(_)) => unreachable!(),
            };
            tracing::warn!(task_id = %task_id, "启动失败,回滚任务状态: {reason}");
            let _ = task_entity::Entity::update_many()
                .col_expr(
                    task_entity::Column::Status,
                    sea_orm::sea_query::Expr::value(model.status.clone()),
                )
                .filter(task_entity::Column::Id.eq(task_id.clone()))
                .exec(&state.db)
                .await;
            return Err(CrawlerError::Config(reason));
        }
    };
    // 启动瞬间推送一次,前端立即翻转为「运行中」并据此开启轮询兜底
    emit_task_progress(&app, started);

    // 后台采集,不阻塞命令返回。句柄均为 Clone/Arc,可安全 move 进 spawn
    let db = state.db.clone();
    let registry = state.registry.clone();
    let collect_locks = state.collect_locks.clone();
    // 账号池:采集成功 release_ok 清风控计数(零产出不再标记风控,避免网络波动误伤账号)
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
    // panic 兜底还需关闭采集窗口:panic 发生在采集中途时 take_session / control.clear 不再执行,
    // 拦截会话条目残留,窗口 JS 会继续往死会话推响应体;销毁窗口一并终止推送源
    let bridge_guard = bridge.clone();
    let platform_guard = cfg.id.clone();
    let account_id_guard = account_id.clone();
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
            target_urls,
            per_keyword_limit,
            min_likes,
            collect_comments,
            comment_time_range,
            comment_limit,
            analyze_comment_intent,
            audio_extract,
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
            bridge_guard.close_collect_window(&platform_guard, &account_id_guard, Some(&task_id_guard));
            // 自清主动关窗置位的「被手动关闭」标记,防污染下次「关窗即终止」判定
            bridge_guard.reset_collect_window_closed(&platform_guard, &account_id_guard, Some(&task_id_guard));
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
    /// 定向采集目标链接(视频 / 主页链接);非空时按定向采集处理
    target_urls: Vec<String>,
    per_keyword_limit: usize,
    min_likes: i32,
    collect_comments: bool,
    comment_time_range: String,
    comment_limit: usize,
    analyze_comment_intent: bool,
    /// 音频提取(视频下载 + 转 mp3);含 AI 文案提取隐含的音频需求
    audio_extract: bool,
    /// AI 文案提取(语音转写);依赖 audio_extract
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
/// 去重集合以调用方的累计快照为种子(跨关键词连续),结束后整套返还调用方合并——
/// 兜底解析据此跳过已增量入库的内容,消除主路径「增量 + 兜底」对同一内容的双写;
/// 计数也因此是任务级累计,不会在切换关键词时从累计值跌回本关键词的小数字。
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
    existing_ids: std::sync::Arc<HashSet<String>>,
    mut seen_contents: HashSet<String>,
    mut seen_comments: HashSet<String>,
) -> tauri::async_runtime::JoinHandle<(HashSet<String>, HashSet<String>)> {
    tauri::async_runtime::spawn(async move {
        while let Some(mut batch) = rx.recv().await {
            // 去重台账:本任务已采 / 同平台台账已登记的内容整体跳过——不再入库,
            // 后续评论 / 素材阶段也不会处理(兜底解析处同口径过滤)
            batch.retain(|c| !existing_ids.contains(&c.content_id));
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
                crate::webview::hud_log(&app, &platform, &account_id, Some(&task_id), "info", &msg);
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
            persist_collected(&db, &task_id, &owner, &keyword, output, &mut seen_contents, &mut seen_comments).await;
            let (c, m) = (seen_contents.len() as i64, seen_comments.len() as i64);
            write_task_progress(&app, &db, &task_id, progress, c, m, false).await;
            emit_collect_log(&app, &task_id, "info", format!("📦 「{keyword}」已保存 {c} 条内容"));
        }
        (seen_contents, seen_comments)
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
    // 去重跳过快照:consumer 子任务按 Arc 共享(本任务已采 ∪ 台账已登记,运行开始后不变)
    let existing_ids_shared = std::sync::Arc::new(existing_ids.clone());

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
        if bridge.is_collect_window_closed(&cfg.id, account_id, Some(task_id)) {
            shared.window_closed = true;
            emit_collect_log(
                app,
                task_id,
                "warn",
                "🛑 采集窗口已被手动关闭 · 终止任务(已采数据保留)".to_string(),
            );
            break;
        }
        // 已完成进度(不含进行中的本项):最后一项采完约 (total-1)/total,
        // 剩余进度由评论/素材阶段与 write_task_done 的 100 收尾,不再提前预支到 100%
        let progress = ((idx as f64 / total as f64) * 100.0) as i32;
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
                    existing_ids_shared.clone(),
                    shared.seen_contents.clone(), shared.seen_comments.clone(),
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
                            content_tx: Some(tx),
                            existing_ids: Some(existing_ids),
                            sort_mode,
                            time_range,
                            min_likes,
                            blacklisted_uids: Some(blacklisted_uids),
                            extra_filters: extra_filter_clicks,
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
                // (tx 已 move 进 CollectRequest,collect 返回即析构,消费端通道自然关闭)
                // 取回消费端累计的去重集合:下方兜底解析据此跳过已增量入库的内容(消除双写)。
                // 消费任务异常(panic)时不合并,兜底解析按原集合全量补救,数据不丢。
                if let Ok((seen_c, seen_m)) = consumer.await {
                    shared.seen_contents = seen_c;
                    shared.seen_comments = seen_m;
                }

                // 累计拦截响应数(此前只在无适配器分支累计,适配器路径 metrics.intercepted 恒为 0)
                shared.intercepted_total += collect_result.responses.len() as i64;
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
                            // 去重台账:本任务已采 / 同平台台账已登记的内容整体跳过
                            // (不入库、不进 contents_for_media,评论 / 素材阶段自然不处理)
                            let before = output.contents.len();
                            output
                                .contents
                                .retain(|c| !existing_ids.contains(&c.content_id));
                            let skipped = before - output.contents.len();
                            if skipped > 0 {
                                emit_collect_log(
                                    app,
                                    task_id,
                                    "info",
                                    format!("⏭️ 「{keyword}」跳过已采内容 {skipped} 条(去重台账)"),
                                );
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
                            shared.parse_failures += 1;
                            tracing::warn!(keyword = %keyword, "兜底解析失败: {e}");
                            // 增量通道已落库的内容不能因此丢素材:从 DB 回读本任务内容补进下载列表,
                            // 否则这些行永久停在 media_status=pending(补偿只认 failed 任务,够不到)
                            backfill_contents_for_media(db, task_id, shared).await;
                        }
                    }
                }

                let (c, m) = (shared.seen_contents.len() as i64, shared.seen_comments.len() as i64);
                write_task_progress(app, db, task_id, progress, c, m, false).await;
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
                    Some(task_id),
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

        write_task_progress(app, db, task_id, progress, content_count, comment_count, false).await;

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

/// 判定定向链接是否为「作者主页链接」(区别于单条内容链接)。
/// 抖音主页模态详情(/user/{sec_uid}?modal_id=…)含 /user/ 但实为内容链接,用 modal_id 排除。
fn is_profile_url(platform: &str, url: &str) -> bool {
    match platform {
        "douyin" => url.contains("douyin.com/user/") && !url.contains("modal_id"),
        "xhs" => url.contains("xiaohongshu.com/user/profile/"),
        "kuaishou" => url.contains("kuaishou.com/profile/"),
        "bilibili" => url.contains("space.bilibili.com"),
        "tiktok" => url.contains("tiktok.com/@") && !url.contains("/video/"),
        "youtube" => {
            url.contains("youtube.com/@")
                || url.contains("youtube.com/channel/")
                || url.contains("youtube.com/c/")
        }
        _ => false,
    }
}

/// 定向采集阶段:keywords 里 http(s) 开头的条目是定向链接——内容链接(含前端拼好的视频 ID 链接)
/// 直接导航详情页等 detail 接口;作者主页链接导航主页后滚动加载作品列表(aweme/post)。
/// 不拼搜索模板、不跑筛选。落库 / 进度 / 停止(点结束、关窗)语义与 collect_keywords 一致;
/// 解析分别走 TaskKind::ContentDetail / TaskKind::UserPosts。
/// 之后的评论采集 / 画像补采 / 素材下载各阶段只消费 shared.contents_for_media,零改动自然覆盖。
#[allow(clippy::too_many_arguments)]
async fn collect_direct_urls(
    app: &AppHandle,
    db: &DatabaseConnection,
    bridge: &CollectBridge,
    cfg: &veltrix_core::config::PlatformConfig,
    account_id: &str,
    task_id: &str,
    task_name: &str,
    owner: &str,
    urls: &[String],
    existing_ids: &HashSet<String>,
    adapter: &Option<Arc<dyn crate::adapter::PlatformAdapter>>,
    shared: &mut CollectSharedState,
) {
    let total = urls.len();
    emit_collect_log(
        app,
        task_id,
        "info",
        format!("🚀 开始定向采集 · 共 {total} 个链接"),
    );
    let Some(adapter_arc) = adapter else {
        emit_collect_log(
            app,
            task_id,
            "warn",
            format!("平台 {} 未注册适配器,定向采集跳过", cfg.id),
        );
        return;
    };
    let detail_pattern = adapter_arc.detail_pattern();
    let posts_pattern = adapter_arc.posts_pattern();

    for (idx, url) in urls.iter().enumerate() {
        // 与 collect_keywords 相同的提前终止检查:点结束 / 关窗即停,不再为后续链接重开窗口
        if bridge.is_task_stopping(task_id) {
            shared.user_ended = true;
            emit_collect_log(
                app,
                task_id,
                "warn",
                "🛑 已手动结束采集 · 停止后续链接(已采数据保留,继续下载素材)".to_string(),
            );
            break;
        }
        if bridge.is_collect_window_closed(&cfg.id, account_id, Some(task_id)) {
            shared.window_closed = true;
            emit_collect_log(
                app,
                task_id,
                "warn",
                "🛑 采集窗口已被手动关闭 · 终止任务(已采数据保留)".to_string(),
            );
            break;
        }
        // 已完成进度(不含进行中的本项):最后一项采完约 (total-1)/total,
        // 剩余进度由评论/素材阶段与 write_task_done 的 100 收尾,不再提前预支到 100%
        let progress = ((idx as f64 / total as f64) * 100.0) as i32;
        // 链接分类:主页链接抓作者全部作品(UserPosts),其余按单条内容链接抓详情(ContentDetail)
        let profile = is_profile_url(&cfg.id, url);
        let (kind, supported) = if profile {
            (
                TaskKind::UserPosts,
                adapter_arc.supports(&TaskKind::UserPosts),
            )
        } else {
            (
                TaskKind::ContentDetail,
                adapter_arc.supports(&TaskKind::ContentDetail),
            )
        };
        // 平台适配器不支持对应解析(如 B站/TikTok/YouTube 无详情解析):提示后跳过该链接
        if !supported {
            shared.had_error = true;
            emit_collect_log(
                app,
                task_id,
                "warn",
                format!(
                    "⚠️ 平台 {} 暂不支持{}采集,已跳过 {url}",
                    cfg.name,
                    if profile { "主页" } else { "定向" }
                ),
            );
            continue;
        }
        emit_collect_log(
            app,
            task_id,
            "info",
            if profile {
                format!("👤 [{}/{}] 正在采集主页 {}", idx + 1, total, url)
            } else {
                format!("🔗 [{}/{}] 正在打开链接 {}", idx + 1, total, url)
            },
        );

        let outcome = if profile {
            bridge
                .collect_profile_posts(
                    app,
                    ProfilePostsCollectRequest {
                        account_id,
                        url,
                        task_name,
                        platform_cfg: cfg,
                        task_id: Some(task_id),
                        posts_pattern,
                    },
                )
                .await
        } else {
            bridge
                .collect_direct(
                    app,
                    DirectCollectRequest {
                        account_id,
                        url,
                        task_name,
                        platform_cfg: cfg,
                        task_id: Some(task_id),
                        detail_pattern,
                    },
                )
                .await
        };
        let stop_reason = outcome.stop;
        if let Some(e) = &outcome.error {
            shared.had_error = true;
            tracing::error!(url = %url, "定向采集失败: {e}");
            emit_collect_log(
                app,
                task_id,
                "error",
                format!("❌ 链接 {url} 采集异常 · 已保留已采数据 · 原因: {e}"),
            );
        }

        // 累计拦截响应数(同关键词阶段口径)
        shared.intercepted_total += outcome.responses.len() as i64;
        let responses = outcome.responses;
        if responses.is_empty() {
            shared.had_error = true;
            emit_collect_log(
                app,
                task_id,
                "warn",
                format!("⚠️ 链接 {url} 未拦到任何接口响应(页面可能不存在或需登录)"),
            );
        } else {
            let ctx = FetchContext {
                keyword: url.clone(),
                responses,
            };
            match adapter_arc.parse(&kind, &ctx).await {
                Ok(mut output) => {
                    // 去重台账:已采过的内容 / 主页作品整体跳过(口径同关键词阶段)
                    let before = output.contents.len();
                    output
                        .contents
                        .retain(|c| !existing_ids.contains(&c.content_id));
                    let skipped = before - output.contents.len();
                    if skipped > 0 {
                        emit_collect_log(
                            app,
                            task_id,
                            "info",
                            format!("⏭️ 链接 {url} 跳过已采内容 {skipped} 条(去重台账)"),
                        );
                    }
                    if output.contents.is_empty() {
                        if skipped > 0 {
                            // 解析正常但全部已采过:去重跳过的正常情形,不算异常
                            emit_collect_log(
                                app,
                                task_id,
                                "info",
                                format!("✅ 链接 {url} 的内容此前均已采集 · 本次跳过"),
                            );
                        } else {
                            shared.had_error = true;
                            emit_collect_log(
                                app,
                                task_id,
                                "warn",
                                format!("⚠️ 链接 {url} 未解析到内容(视频可能已删除 / 被风控)"),
                            );
                        }
                    } else {
                        emit_collect_log(
                            app,
                            task_id,
                            "info",
                            format!("📦 链接 {url} 解析到 {} 条内容 · 入库中", output.contents.len()),
                        );
                    }
                    shared.contents_for_media.extend(output.contents.iter().cloned());
                    // keyword 列存链接本身:全量库按词筛选 / HUD tab 归属都可用它定位
                    persist_collected(
                        db,
                        task_id,
                        owner,
                        url,
                        output,
                        &mut shared.seen_contents,
                        &mut shared.seen_comments,
                    )
                    .await;
                }
                Err(e) => {
                    shared.had_error = true;
                    shared.parse_failures += 1;
                    tracing::warn!(url = %url, "定向解析失败: {e}");
                }
            }
        }

        let (c, m) = (
            shared.seen_contents.len() as i64,
            shared.seen_comments.len() as i64,
        );
        write_task_progress(app, db, task_id, progress, c, m, false).await;

        // 用户在本链接采集途中主动停止:与 collect_keywords 同语义,终止整个任务
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
                    "🛑 已手动结束采集 · 停止后续链接(已采数据保留,继续下载素材)".to_string(),
                );
                break;
            }
            None => {}
        }
    }
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
        match ce::Entity::find()
            .filter(ce::Column::TaskId.eq(task_id))
            .select_only()
            .column(ce::Column::ContentId)
            .column(ce::Column::Keyword)
            .into_tuple::<(String, String)>()
            .all(db)
            .await
        {
            Ok(rows) => rows.into_iter().collect(),
            Err(e) => {
                // 查询失败按空 map 继续会把所有评论的 keyword 归属静默写空,必须留痕
                tracing::warn!(task_id = %task_id, "查询内容关键词归属失败,评论 keyword 将留空: {e}");
                std::collections::HashMap::new()
            }
        }
    };
    // 评论数为 0 的视频直接跳过(接口统计已知无评论,采了也是空跑);
    // 数量未知(None)的仍尝试:部分平台不回传统计(如 YouTube 恒 None),按 0 处理会全量漏采
    let zero_comment = shared
        .contents_for_media
        .iter()
        .filter(|c| c.stats.comment_count == Some(0))
        .count();
    let video_ids: Vec<(String, String, String, String)> = shared
        .contents_for_media
        .iter()
        .filter(|c| id_seen.insert(c.content_id.clone()))
        .filter(|c| c.stats.comment_count != Some(0))
        .map(|c| {
            // 详情页导航所需的第二参数({token} 占位):
            // 抖音走「主页模态」/user/{sec_uid}?modal_id=,用作者 sec_uid(author.uid 存的就是 sec_uid);
            // 其他平台(小红书)详情导航用内容自带的 xsec_token。
            let token = if cfg.id == "douyin" {
                c.author.uid.clone()
            } else {
                c.extra
                    .get("xsec_token")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string()
            };
            let keyword = keyword_map.get(&c.content_id).cloned().unwrap_or_default();
            (c.content_id.clone(), token, log_content_title(c), keyword)
        })
        .collect();
    let cutoff = comment_time_cutoff(comment_time_range);
    let total_videos = video_ids.len();
    write_task_collecting_comments(app, db, task_id, total_videos as i32).await;
    emit_collect_log(app, task_id, "info", format!(
        "💬 开始采集评论 · 共 {} 个视频 · 每视频最多 {}",
        video_ids.len(),
        if comment_limit == 0 { "不限".to_string() } else { comment_limit.to_string() }
    ));
    if zero_comment > 0 {
        emit_collect_log(
            app,
            task_id,
            "info",
            format!("⏭️ 跳过 {zero_comment} 个评论数为 0 的视频(接口统计无评论,不空跑)"),
        );
    }

    // 评论采集成功(拿到非空响应)的视频 id:仅这些在阶段末标 comment_collected=true;
    // 采集失败 / 零响应的留 false,下次运行可重采(此前全量标记,失败视频永久失去重采机会)
    let mut comment_done_ids: Vec<String> = Vec::new();
    for (vidx, (content_id, xsec_token, title, keyword)) in
        video_ids.iter().enumerate()
    {
        // 与关键词 / 定向阶段一致的提前终止检查:点结束 / 关窗即停,不再为后续视频开新会话
        // (此前漏检:关窗后下一个视频的 ensure_window 会把窗口重新建出来)
        if bridge.is_task_stopping(task_id) {
            shared.user_ended = true;
            emit_collect_log(
                app,
                task_id,
                "warn",
                "🛑 已手动结束采集 · 停止后续视频评论(已采数据保留,继续下载素材)".to_string(),
            );
            break;
        }
        if bridge.is_collect_window_closed(&cfg.id, account_id, Some(task_id)) {
            shared.window_closed = true;
            emit_collect_log(
                app,
                task_id,
                "warn",
                "🛑 采集窗口已被手动关闭 · 终止任务(已采数据保留)".to_string(),
            );
            break;
        }
        if vidx > 0 {
            tokio::time::sleep(random_comment_video_interval()).await;
        }
        // 由详情页模板还原视频链接({id}=内容 id,{token}=sec_uid/xsec_token,与导航口径一致),
        // 打进日志:排查「这条评论属于哪个视频」时可直接点链接核对,不用反查 content_id
        let video_link = if cfg.collect.detail_url_template.is_empty() {
            String::new()
        } else {
            cfg.collect
                .detail_url_template
                .replace("{id}", content_id)
                .replace("{token}", xsec_token)
        };
        let link_part = if video_link.is_empty() {
            String::new()
        } else {
            format!(" · {video_link}")
        };
        emit_collect_log(app, task_id, "info",
            format!("💬 [{}/{}] 正在采集「{title}」的评论{link_part}", vidx + 1, total_videos));
        // HUD 同步一条(HUD 默认只显示逐条评论,看不到当前在采哪个视频)
        crate::webview::hud_log(
            app,
            &cfg.id,
            account_id,
            Some(task_id),
            "info",
            &format!("💬 [{}/{}] 采集评论「{title}」{link_part}", vidx + 1, total_videos),
        );
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
                // 累计拦截响应数(同采集阶段口径)
                shared.intercepted_total += responses.len() as i64;
                let ctx = FetchContext {
                    keyword: content_id.clone(),
                    responses,
                };
                match adapter.parse(&TaskKind::Comments, &ctx).await {
                    Ok(mut output) => {
                        // 解析成功才标记已采;解析失败留 false 供下次重采(此前在 parse 前 push,
                        // 解析失败的视频会被误标已采,永久失去重采机会)
                        comment_done_ids.push(format!("{task_id}-{}-{}", cfg.id, content_id));
                        output.comments =
                            filter_comments(output.comments, cutoff, comment_limit);
                        // 评论编号按视频从 1 开始,并带「第几/共几个视频」——全局累加编号
                        // 看不出评论属于哪个视频,跨视频排查时对不上号
                        let mut vseq: i64 = 0;
                        for cm in &output.comments {
                            vseq += 1;
                            let text = truncate_chars(&cm.text, 60);
                            let likes = cm.like_count.unwrap_or(0);
                            let msg = format!(
                                "[视频{}/{total_videos} 评论{vseq}] {text} | 点赞:{likes}",
                                vidx + 1
                            );
                            crate::webview::hud_log(
                                app, &cfg.id, account_id, Some(task_id), "info", &msg,
                            );
                            emit_collect_entry(app, task_id, msg, CollectEntry {
                                kind: "comment".to_string(), seq: vseq,
                                avatar: cm.author.avatar.clone(), nickname: cm.author.nickname.clone(),
                                title: text, content_kind: None,
                            });
                        }
                        // 评论解析不产出内容,清空防误入库(keyword 口径是 content_id,混入会污染)
                        output.contents = Vec::new();
                        // 整批一次 upsert:此前逐条落库,一个视频上百条评论就是上百次 DB 往返
                        persist_collected(db, task_id, owner, content_id, output,
                            &mut shared.seen_contents, &mut shared.seen_comments).await;
                    }
                    Err(e) => {
                        shared.parse_failures += 1;
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
        // 每视频两次 DB 往返(写 + 回读推送),百视频任务即数百次写——
        // 复用进度节流合并,最后一个视频强制写保证计数不滞后
        write_task_comment_progress(
            app,
            db,
            task_id,
            (vidx + 1) as i32,
            shared.seen_comments.len() as i64,
            vidx + 1 == total_videos,
        )
        .await;
    }
    if shared.user_ended || shared.window_closed {
        emit_collect_log(
            app,
            task_id,
            "info",
            format!("⏹ 评论采集提前终止 · 已采集 {} 条评论", shared.seen_comments.len()),
        );
    } else {
        emit_collect_log(
            app,
            task_id,
            "info",
            format!("✅ 评论采集完成 · 共采集 {} 条评论", shared.seen_comments.len()),
        );
    }
    {
        use sea_orm::sea_query::Expr;
        use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
        use veltrix_core::db::entity::content as content_entity;
        // 只标确有评论响应的视频(见 comment_done_ids);失败/零响应留 false 供重采。
        // 分批:大任务 id 数可破千,单批 is_in 超 SQLite 变量上限(999)会整批失败
        for chunk in comment_done_ids.chunks(500) {
            if let Err(e) = content_entity::Entity::update_many()
                .col_expr(content_entity::Column::CommentCollected, Expr::value(true))
                .filter(content_entity::Column::Id.is_in(chunk.iter().cloned()))
                .exec(db)
                .await
            {
                tracing::warn!("标记 comment_collected 失败(可能导致下次重复采集评论): {e}");
            }
        }
    }
}

/// 作者画像自动补采的单次运行上限:补采需逐个打开作者主页(每个数秒~十几秒),
/// 量大时会明显拖长任务;超出部分留待下次运行继续,或作者库手动补采。
const AUTHOR_AUTO_ENRICH_MAX_PER_RUN: usize = 20;

/// 阶段2.5:作者画像自动补采。搜索响应的 author 对象不带粉丝/关注/获赞/属地
/// (这些字段只在作者主页画像接口返回),故在评论采集之后、仍占着采集窗口时,
/// 对本次涉及且画像缺失(粉丝数为空)的作者逐个打开主页补齐。
/// 补齐过的作者粉丝数非空,后续运行天然跳过,不重复开主页。
async fn auto_enrich_authors_phase(
    args: &EnrichAuthorArgs<'_>,
    owner: &str,
    shared: &CollectSharedState,
) {
    use veltrix_core::db::entity::author as author_entity;
    // 流水线内调用恒有 task_id(见 run_task 组装处);兜底空串仅为不 panic
    let task_id = args.task_id.unwrap_or_default();

    // 本次采集涉及的作者(uid 去重)→ 作者档案行主键(owner-platform-uid)
    let mut seen_uids: HashSet<String> = HashSet::new();
    let author_ids: Vec<String> = shared
        .contents_for_media
        .iter()
        .filter(|c| !c.author.uid.is_empty())
        .filter(|c| seen_uids.insert(c.author.uid.clone()))
        .map(|c| format!("{owner}-{}-{}", c.author.platform, c.author.uid))
        .collect();
    if author_ids.is_empty() {
        return;
    }
    // 只补画像缺失的(粉丝数为空):已补齐的不重复打开主页。
    // 分批查:大任务作者数可破千,单批 is_in 超 SQLite 变量上限(999)会整批失败
    let mut missing: Vec<author_entity::Model> = Vec::new();
    for chunk in author_ids.chunks(500) {
        match author_entity::Entity::find()
            .filter(author_entity::Column::Id.is_in(chunk.iter().cloned()))
            .filter(author_entity::Column::FollowerCount.is_null())
            .all(args.db)
            .await
        {
            Ok(rows) => missing.extend(rows),
            Err(e) => {
                tracing::warn!("查询画像缺失作者失败,跳过自动补采: {e}");
                return;
            }
        }
    }
    if missing.is_empty() {
        return;
    }
    let total_missing = missing.len();
    let batch: Vec<_> = missing
        .into_iter()
        .take(AUTHOR_AUTO_ENRICH_MAX_PER_RUN)
        .collect();
    emit_collect_log(
        args.app,
        task_id,
        "info",
        format!("👤 作者画像补采 · 本次待补 {} 个(粉丝/关注/获赞/属地)", batch.len()),
    );
    if total_missing > batch.len() {
        emit_collect_log(
            args.app,
            task_id,
            "info",
            format!(
                "ℹ️ 画像缺失作者共 {total_missing} 个 · 本次仅补前 {} 个 · 其余下次运行继续",
                batch.len()
            ),
        );
    }

    let mut updated = 0usize;
    for (idx, author) in batch.iter().enumerate() {
        // 手动结束 / 关窗即停:与评论采集同规则,不为补采重开窗口
        if args.bridge.is_task_stopping(task_id)
            || args.bridge.is_collect_window_closed(&args.cfg.id, args.account_id, args.task_id)
        {
            emit_collect_log(
                args.app,
                task_id,
                "warn",
                "🛑 已手动结束 / 采集窗口关闭 · 停止作者画像补采".to_string(),
            );
            break;
        }
        // 串行限速:首个不等,之后每个之间随机间隔降频
        if idx > 0 {
            tokio::time::sleep(random_comment_video_interval()).await;
        }
        match enrich_author_profile(args, author).await {
            EnrichOutcome::Updated => updated += 1,
            EnrichOutcome::Skipped(msg) | EnrichOutcome::Failed(msg) => {
                emit_collect_log(
                    args.app,
                    task_id,
                    "warn",
                    format!("⚠️ 「{}」画像补采未成功 · {msg}", author.nickname),
                );
            }
        }
    }
    emit_collect_log(
        args.app,
        task_id,
        "info",
        format!("✅ 作者画像补采完成 · 刷新 {updated}/{} 个", batch.len()),
    );
}

/// 后处理(前半):意向分析 + 待下载清单 + 任务状态回写。均不占采集窗口,
/// 阶段2.7 前半:零产出失败判定 + 待下载清单 + 进度/状态回写(不占窗口)。
/// 意向分析移到收尾阶段(评论采集之后,见 analyze_intent_phase);
/// 终态 completed 也由调用方在所有阶段结束后统一写,这里只在失败时写 failed。
/// 返回 (task_failed, 待下载内容列表),交给「直链补取(占窗口)」与素材下载继续处理。
async fn post_collect_prepare(
    app: &AppHandle,
    db: &DatabaseConnection,
    task_id: &str,
    shared: &CollectSharedState,
) -> (bool, Vec<Content>) {
    let total_contents = shared.seen_contents.len();
    let total_comments = shared.seen_comments.len();
    if total_contents == 0 && shared.had_error {
        write_task_failed(app, db, task_id, "采集未获取到任何内容").await;
        emit_collect_log(
            app,
            task_id,
            "error",
            "任务失败 · 未采集到内容,请检查账号登录态 / 风控".to_string(),
        );
        (true, Vec::new())
    } else {
        // 终态切换前强制落一次最新计数:节流窗口内的最后一批计数不能滞后到 UI
        write_task_progress(
            app,
            db,
            task_id,
            100,
            total_contents as i64,
            total_comments as i64,
            true,
        )
        .await;
        let pending = filter_pending_media(db, task_id, shared.contents_for_media.clone()).await;
        if !pending.is_empty() {
            write_task_downloading(app, db, task_id, pending.len() as i32).await;
        }
        emit_collect_log(
            app,
            task_id,
            "info",
            format!("✅ 内容采集完成 · {total_contents} 条内容 · {total_comments} 条评论"),
        );
        (false, pending)
    }
}

/// 收尾阶段:评论意向分析(LLM,不占窗口)。排在评论采集之后,分析的是本次最新采到的评论。
async fn analyze_intent_phase(
    app: &AppHandle,
    db: &DatabaseConnection,
    task_id: &str,
    intent_cfg: &veltrix_core::config::CommentIntentConfig,
    analyze_comment_intent: bool,
    collect_comments: bool,
    total_contents: i64,
) {
    let intent_ready = analyze_comment_intent
        && collect_comments
        && total_contents > 0
        && !intent_cfg.api_url.is_empty()
        && !intent_cfg.model.is_empty();
    if !intent_ready {
        return;
    }
    write_task_analyzing(app, db, task_id).await;
    let analyzed = analyze_comments_intent(app, db, task_id, intent_cfg).await;
    // 确有评论被分析出结果才标记;0 产出(缺 key / 批次全失败)标记会掩盖「实际没分析」,
    // 库内再无标记能区分真假,也断了后续补偿的识别依据
    if analyzed > 0 {
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

/// 收尾执行历史:记终态 + 本次新增量 + 运行指标(拦截 / 解析失败 / 阶段耗时)。
async fn finalize_task_run(
    db: &DatabaseConnection,
    task_id: &str,
    run_id: &str,
    started_at: i64,
    metrics: Option<&RunMetrics>,
) {
    use sea_orm::{ColumnTrait, EntityTrait, IntoActiveModel, PaginatorTrait, QueryFilter};
    use veltrix_core::db::entity::{
        comment as comment_entity, content as content_entity, task as task_entity,
        task_run as run_entity,
    };
    let final_task = match task_entity::Entity::find_by_id(task_id.to_string()).one(db).await {
        Ok(t) => t,
        Err(e) => {
            // 查询失败不能兜底成 completed(会把 failed 的运行在执行历史里记成成功);
            // 保留 run 行原状态,仅补 finished_at / delta / 指标
            tracing::warn!(task_id = %task_id, "收尾:查询任务终态失败,执行历史保留原状态: {e}");
            None
        }
    };
    let final_status = final_task.as_ref().map(|t| t.status.clone());
    let final_error = final_task.and_then(|t| t.error_message);
    let content_delta = match content_entity::Entity::find()
        .filter(content_entity::Column::TaskId.eq(task_id))
        .filter(content_entity::Column::CollectedAt.gte(started_at))
        .count(db)
        .await
    {
        Ok(n) => n as i64,
        Err(e) => {
            tracing::warn!(task_id = %task_id, "收尾:统计内容增量失败,记 0: {e}");
            0
        }
    };
    let comment_delta = match comment_entity::Entity::find()
        .filter(comment_entity::Column::TaskId.eq(task_id))
        .filter(comment_entity::Column::CollectedAt.gte(started_at))
        .count(db)
        .await
    {
        Ok(n) => n as i64,
        Err(e) => {
            tracing::warn!(task_id = %task_id, "收尾:统计评论增量失败,记 0: {e}");
            0
        }
    };
    match run_entity::Entity::find_by_id(run_id.to_string()).one(db).await {
        Ok(Some(run)) => {
            let mut am = run.into_active_model();
            am.finished_at = Set(Some(Utc::now().timestamp()));
            if let Some(s) = final_status {
                am.status = Set(s);
            }
            am.content_delta = Set(content_delta);
            am.comment_delta = Set(comment_delta);
            am.error_message = Set(final_error);
            am.metrics_json =
                Set(metrics.map(|m| serde_json::to_string(m).unwrap_or_default()));
            if let Err(e) = am.update(db).await {
                tracing::warn!(task_id = %task_id, "收尾执行历史失败: {e}");
            }
        }
        other => {
            // run 行缺失(插入失败 / 主键冲突)此前静默跳过,旧行永久停在 running
            tracing::warn!(task_id = %task_id, run_id = %run_id, "收尾:执行历史行不存在或查询失败({other:?})");
        }
    }
    if let Some(m) = metrics {
        tracing::info!(
            task_id,
            run_id,
            intercepted = m.intercepted,
            parse_failures = m.parse_failures,
            persisted_contents = m.persisted_contents,
            persisted_comments = m.persisted_comments,
            stages_ms = ?m.stages_ms,
            "任务收尾指标"
        );
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
        target_urls,
        per_keyword_limit,
        min_likes,
        collect_comments,
        comment_time_range,
        comment_limit,
        analyze_comment_intent,
        audio_extract,
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
            account_collect_lock(&collect_locks, &account_lock_key(&cfg.id, &account_id));
        let collect_guard = account_lock.lock().await;

        // 执行历史:本次运行先记一条 task_run(running);采集日志按 [started_at, finished_at]
        // 时间范围归到该次运行(见 list_run_logs)。run_id 用 task_id + 起始毫秒——此前用起始秒,
        // 同任务秒级重跑撞主键仅 warn,旧 run 行永久停在 running
        let run_id = format!("{}-run-{}", task_id, Utc::now().timestamp_millis());
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
                metrics_json: Set(None),
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
            parse_failures: 0,
            had_error: false,
            contents_for_media: Vec::new(),
            window_closed: false,
            user_ended: false,
        };
        // 运行指标与阶段计时:采集 / 评论 / 画像 / 素材各段分别累计,收尾写入 task_runs
        let mut metrics = RunMetrics::default();
        let run_started = std::time::Instant::now();

        // 重置本任务采集窗口的「已被手动关闭」标记,使本次任务能正常开窗
        bridge.reset_collect_window_closed(&cfg.id, &account_id, Some(&task_id));
        // 重置本任务的「结束」停止标记,避免上次运行点过结束影响本次重跑
        bridge.reset_task_stop(&task_id);

        // 素材下载用的会话 Cookie:须在关闭采集窗口前解析留存(窗口销毁后实时 Cookie 不可取)
        let mut session_cookie: Option<String> = None;

        // 阶段1:内容采集。定向目标来自 target_urls 列(创建定向任务时前端已把视频 ID 拼成链接,
        // 定向任务 keywords 只存占位词「定向采集」,需剔除、不参与搜索);兼容早期把链接存进
        // keywords 的任务:http(s) 开头的条目仍按定向处理。两类可混合——先搜索后定向,共用同一采集窗口。
        let (mut direct_urls, mut search_keywords): (Vec<String>, Vec<String>) = keywords
            .iter()
            .cloned()
            .partition(|k| {
                let lower = k.to_ascii_lowercase();
                lower.starts_with("http://") || lower.starts_with("https://")
            });
        // 定向任务的展示占位词不是真实搜索词,剔除避免误触发一次关键词搜索
        search_keywords.retain(|k| k != "定向采集");
        direct_urls.extend(target_urls);
        let collect_start = std::time::Instant::now();
        let mut total_contents = 0i64;
        if !search_keywords.is_empty() {
            let (c, _total_comments) = collect_keywords(
                &app,
                &db,
                &bridge,
                &cfg,
                &account_id,
                &task_id,
                &task_name,
                &owner,
                &search_keywords,
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
            total_contents += c;
        }
        // 搜索阶段被停止/关窗则不再进入定向阶段(与关键词间停止语义一致)
        if !direct_urls.is_empty() && !shared.window_closed && !shared.user_ended {
            collect_direct_urls(
                &app,
                &db,
                &bridge,
                &cfg,
                &account_id,
                &task_id,
                &task_name,
                &owner,
                &direct_urls,
                &existing_ids,
                &adapter,
                &mut shared,
            )
            .await;
            total_contents = shared.seen_contents.len() as i64;
        }
        metrics.stages_ms.insert(
            "collect".to_string(),
            collect_start.elapsed().as_millis() as u64,
        );

        // 用户中途手动关闭采集窗口 → 终止任务:不再采评论 / 不跑后处理(二者都会重建采集窗口),
        // 标记 cancelled 收尾;已增量落库的内容保留,素材可日后重跑补齐。
        if shared.window_closed || bridge.is_collect_window_closed(&cfg.id, &account_id, Some(&task_id)) {
            emit_collect_log(
                &app,
                &task_id,
                "warn",
                "🛑 采集窗口已被手动关闭 · 任务终止".to_string(),
            );
            write_task_cancelled(&app, &db, &task_id, "采集窗口被手动关闭,任务已终止").await;
            metrics.intercepted = shared.intercepted_total;
            metrics.persisted_contents = shared.seen_contents.len();
            metrics.persisted_comments = shared.seen_comments.len();
            metrics.parse_failures = shared.parse_failures;
            metrics.stages_ms.insert(
                "total".to_string(),
                run_started.elapsed().as_millis() as u64,
            );
            finalize_task_run(&db, &task_id, &run_id, now, Some(&metrics)).await;
            bridge.close_collect_window(&cfg.id, &account_id, Some(&task_id));
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
            // 关窗前先留存会话 Cookie(含 httponly tt_chain_token),供后续素材下载复用——
            // 此前先关窗后下载,下载只能退回 DB Cookie(常为空),TikTok 类 CDN 批量 403
            session_cookie = resolve_session_cookie(&app, &db, &cfg.id, &account_id, Some(&task_id)).await;
            bridge.close_collect_window(&cfg.id, &account_id, Some(&task_id));
            // 程序主动关窗(非用户手动关窗),自清 Destroyed 置位的标记,理由同主链路收尾
            bridge.reset_collect_window_closed(&cfg.id, &account_id, Some(&task_id));
        }

        // 阶段2:作者画像自动补采(粉丝/关注/获赞/属地缺失的作者,复用采集窗口开主页补齐;
        // 手动结束 / 关窗后跳过,避免重开窗口)。评论采集同在窗口占用阶段内(见阶段3)。
        let enrich_start = std::time::Instant::now();
        if !shared.user_ended && !shared.window_closed {
            if let Some(adapter_arc) = &adapter {
                if adapter_arc.supports(&TaskKind::UserProfile)
                    && !cfg.collect.profile_url_template.is_empty()
                {
                    let enrich_args = EnrichAuthorArgs {
                        app: &app,
                        db: &db,
                        bridge: &bridge,
                        cfg: &cfg,
                        adapter: adapter_arc.clone(),
                        account_id: &account_id,
                        task_id: Some(&task_id),
                    };
                    auto_enrich_authors_phase(&enrich_args, &owner, &shared).await;
                }
            }
        }
        metrics.stages_ms.insert(
            "enrich".to_string(),
            enrich_start.elapsed().as_millis() as u64,
        );

        // 阶段2.7:待下载清单(DB 计算,不占窗口);task_failed / to_download 供后续阶段用
        let media_start = std::time::Instant::now();
        let (task_failed, mut to_download) =
            post_collect_prepare(&app, &db, &task_id, &shared).await;

        // 阶段3:评论采集(占窗口)。与内容采集同一次开窗内做完:此前放在素材下载之后,
        // 窗口关了又开、开了又关,且重开窗口途中弹出的风控滑块用户不易察觉。
        // 现收回窗口占用阶段——一次开窗做完所有需要窗口的事,关窗后只剩纯 HTTP 阶段。
        // 仍持有采集锁与全局并发名额,无需重新获取;手动结束 / 关窗 / 任务失败则跳过
        // (窗口已销毁,不为评论重开)
        let comments_start = std::time::Instant::now();
        if collect_comments
            && total_contents > 0
            && !shared.user_ended
            && !shared.window_closed
            && !task_failed
        {
            if let Some(adapter_arc) = &adapter {
                let comments_aborted = bridge.is_task_stopping(&task_id)
                    || bridge.is_collect_window_closed(&cfg.id, &account_id, Some(&task_id));
                if comments_aborted {
                    emit_collect_log(
                        &app,
                        &task_id,
                        "info",
                        "ℹ️ 已手动结束 · 跳过评论采集".to_string(),
                    );
                } else {
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
            } else {
                // 用户开了「采评论」但平台无适配器:此前静默跳过,用户无从得知评论没采
                emit_collect_log(
                    &app,
                    &task_id,
                    "warn",
                    format!("⚠️ 平台 {} 未注册适配器,跳过评论采集", cfg.id),
                );
            }
        }
        metrics.stages_ms.insert(
            "comments".to_string(),
            comments_start.elapsed().as_millis() as u64,
        );

        // 阶段4:直链补取(逐条开详情页,占窗口)。刻意排在评论采集之后、关窗之前:
        // 评论耗时可能很长,先补取再评论会让签名直链在评论期间过期;最后补取保证
        // 关窗即下载时直链最新。手动结束 / 关窗后跳过(缺直链按失败落库,可日后重试)。
        // 需要下载视频(音频提取含 AI 文案提取)才补直链;不下载视频则无需刷新
        if audio_extract
            && !to_download.is_empty()
            && !shared.user_ended
            && !shared.window_closed
        {
            let refresh_aborted = bridge.is_task_stopping(&task_id)
                || bridge.is_collect_window_closed(&cfg.id, &account_id, Some(&task_id));
            if refresh_aborted {
                emit_collect_log(
                    &app, &task_id, "info",
                    "ℹ️ 已手动结束 · 跳过直链补取".to_string(),
                );
            } else {
                let stream_params = StreamRefreshParams {
                    app: &app,
                    bridge: &bridge,
                    registry: &registry,
                    db: &db,
                    cfg: &cfg,
                    account_id: &account_id,
                    task_id: &task_id,
                };
                refresh_stream_urls(
                    &stream_params,
                    &mut to_download,
                    false,
                )
                .await;
            }
        }

        // WebView 占用阶段(内容采集 + 画像补采 + 评论采集 + 直链补取)结束,释放同账号互斥锁与全局并发名额;
        // 后续素材下载(HTTP)不占窗口,其他任务可立即用该账号 / 名额开采。
        // 放锁前必须完成关窗:锁一放,同账号下一任务即可复用本窗口开采,窗口若留到媒体下载后
        // 才关,会误杀新任务正在使用的窗口(新任务据此误判「窗口被手动关闭」而取消)。
        // 关窗前留存会话 Cookie 供素材下载(销毁而非隐藏:隐藏会占住数据目录锁,下次新建冲突)。
        if !shared.user_ended && !shared.window_closed {
            session_cookie = resolve_session_cookie(&app, &db, &cfg.id, &account_id, Some(&task_id)).await;
            // 用户在画像 / 补取期间手动关窗时标记已置位,此处只清「未被用户关闭」的情形
            // (窗口是我们主动关的,Destroyed 置位的标记需自清)
            let user_closed = bridge.is_collect_window_closed(&cfg.id, &account_id, Some(&task_id));
            bridge.close_collect_window(&cfg.id, &account_id, Some(&task_id));
            if !user_closed {
                bridge.reset_collect_window_closed(&cfg.id, &account_id, Some(&task_id));
            }
        }
        drop(collect_guard);
        drop(collect_permit);

        // 采集收尾:成功则清风控计数。零产出只记日志提示,不再标记账号风控/冷却——
        // 零产出多为网络慢、页面加载不出来等环境因素,不应惩罚账号、影响后续轮换。
        if total_contents > 0 {
            if let Err(e) = cookies.release_ok(&account_id).await {
                tracing::warn!(account_id = %account_id, "重置账号风控计数失败: {e}");
            }
        } else if shared.had_error {
            emit_collect_log(
                &app,
                &task_id,
                "warn",
                "账号零产出且采集报错 · 多为网络/页面加载问题,建议检查网络后重跑".to_string(),
            );
        }

        // 阶段5:素材下载 + 音频提取(纯 HTTP,不占窗口——评论采集已前移至窗口占用阶段)
        let audios = if task_failed {
            Vec::new()
        } else {
            let media_params = MediaDownloadParams {
                app: &app,
                db: &db,
                task_id: &task_id,
                platform: &cfg.id,
                account_id: &account_id,
                config_dir: &config_dir,
                media_cfg: &media_cfg,
                transcription_cfg: &transcription_cfg,
                audio_extract,
                ai_extract,
                session_cookie,
                bridge: &bridge,
            };
            download_media_core(&media_params, to_download).await
        };
        metrics.stages_ms.insert(
            "media".to_string(),
            media_start.elapsed().as_millis() as u64,
        );

        // 阶段6:语音转写(AI 文案提取):素材音频已就绪,统一下载后转写;
        // 失败仅告警不影响任务终态
        if ai_extract && !audios.is_empty() && !bridge.is_task_stopping(&task_id) {
            transcribe_for_contents(
                &app,
                &db,
                &task_id,
                &cfg.id,
                &account_id,
                &transcription_cfg,
                media_cfg.ffmpeg_path.clone(),
                Some(&bridge),
                audios,
            )
            .await;
        }

        // 阶段7:评论意向分析(LLM,不占窗口):排在评论采集之后,分析本次最新采到的评论
        analyze_intent_phase(
            &app,
            &db,
            &task_id,
            &intent_cfg,
            analyze_comment_intent,
            collect_comments,
            total_contents,
        )
        .await;

        // Obsidian 同步:排在转写 / 意向之后,同步出去的文案与意向最全
        if auto_sync_obsidian {
            let synced = obsidian::sync_task_to_obsidian(&db, &task_id, &owner).await;
            emit_collect_log(
                &app,
                &task_id,
                "info",
                format!("✅ 已自动同步 {synced} 条内容到 Obsidian"),
            );
        }

        // 终态:failed / cancelled 已在前面分支写入,这里统一收尾 completed
        if !task_failed {
            write_task_done(&app, &db, &task_id).await;
        }

        // 收尾:执行历史(终态 + 本次新增量 + 运行指标)
        metrics.intercepted = shared.intercepted_total;
        metrics.persisted_contents = shared.seen_contents.len();
        metrics.persisted_comments = shared.seen_comments.len();
        metrics.parse_failures = shared.parse_failures;
        metrics.stages_ms.insert(
            "total".to_string(),
            run_started.elapsed().as_millis() as u64,
        );
        finalize_task_run(&db, &task_id, &run_id, now, Some(&metrics)).await;

        // 采集窗口已在释放账号锁前关闭(见上),此处不再迟关——迟关会误杀同账号下一任务
        // 复用的窗口;销毁式关窗同时释放该账号 WebView2 数据目录,下次执行重建即可
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

    // 失败自动重试:任意触发类型(含 once-now)的 failed 任务,已到 next_retry_at 即重新拉起。
    // 与上面 daily/watching 扫描互不干扰:run_task 的原子防重入保证同一任务不会双跑。
    let now_ts = now.timestamp();
    let retry_tasks = match task_entity::Entity::find()
        .filter(task_entity::Column::Archived.eq(false))
        .filter(task_entity::Column::Status.eq("failed"))
        .filter(task_entity::Column::NextRetryAt.is_not_null())
        .all(&state.db)
        .await
    {
        Ok(t) => t,
        Err(e) => {
            tracing::warn!("调度器扫描重试任务失败: {e}");
            return;
        }
    };
    for t in retry_tasks {
        if t.max_retries <= 0 {
            continue;
        }
        let Some(at) = t.next_retry_at else {
            continue;
        };
        if at > now_ts {
            continue;
        }
        tracing::info!(
            task_id = %t.id,
            retry_count = t.retry_count,
            max_retries = t.max_retries,
            "调度器自动重试失败任务"
        );
        if let Err(e) = run_task(app.state::<AppState>(), app.clone(), t.id.clone()).await {
            tracing::warn!(task_id = %t.id, "自动重试启动失败: {e}");
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

/// 进度回写最小间隔(毫秒):采集期高频调用(每批 / 每关键词 / 每视频)合并写,降低 SQLite 写压力;
/// 终态切换前由调用方 force=true 强制落一次,保证最终计数不滞后。
const PROGRESS_WRITE_MIN_INTERVAL_MS: u64 = 600;
/// task_id → 上次进度回写时刻。按任务隔离,避免一个任务的节流饿到其它并行任务。
static LAST_PROGRESS_WRITE: std::sync::LazyLock<
    std::sync::Mutex<std::collections::HashMap<String, std::time::Instant>>,
> = std::sync::LazyLock::new(|| {
    std::sync::Mutex::new(std::collections::HashMap::new())
});

/// 进度回写节流判断:force=true 或距上次回写超过间隔才放行;放行时更新时间戳。
/// 任务条目超过 64 个时清掉 5 分钟前的旧条目,防长时间运行内存增长。
fn progress_write_allowed(task_id: &str, force: bool) -> bool {
    let mut map = LAST_PROGRESS_WRITE.lock().unwrap_or_else(|e| e.into_inner());
    let now = std::time::Instant::now();
    let allowed = if force {
        map.insert(task_id.to_string(), now);
        true
    } else {
        let allowed = match map.get(task_id) {
            Some(prev) => {
                now.duration_since(*prev).as_millis() as u64 >= PROGRESS_WRITE_MIN_INTERVAL_MS
            }
            None => true,
        };
        if allowed {
            map.insert(task_id.to_string(), now);
        }
        allowed
    };
    // 条目上限清理(force 路径也做:终态密集切换的纯 force 流下条目只增不清)
    if map.len() > 64 {
        map.retain(|_, t| now.duration_since(*t).as_secs() < 300);
    }
    allowed
}

async fn write_task_progress(
    app: &AppHandle,
    db: &DatabaseConnection,
    task_id: &str,
    progress: i32,
    content_count: i64,
    comment_count: i64,
    force: bool,
) {
    // 节流:高频调用合并写;force(终态切换前)必写,保证最终计数不滞后
    if !progress_write_allowed(task_id, force) {
        return;
    }
    // 用 update_many 直接 UPDATE 部分列,避免 find_by_id → into_active_model → update
    // 的整行读改写——后者用陈旧的全行值覆盖 status 等并发状态迁移(running→downloading_media→completed),
    // 可能导致任务状态被回退、永不结束。
    use sea_orm::sea_query::Expr;
    use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
    use veltrix_core::db::entity::task as task_entity;
    let now = Utc::now().timestamp();
    match task_entity::Entity::update_many()
        .col_expr(task_entity::Column::Progress, Expr::value(progress))
        .col_expr(task_entity::Column::ContentCount, Expr::value(content_count))
        .col_expr(task_entity::Column::CommentCount, Expr::value(comment_count))
        .col_expr(task_entity::Column::UpdatedAt, Expr::value(now))
        .filter(task_entity::Column::Id.eq(task_id))
        .exec(db)
        .await
    {
        Ok(_) => {
            // update_many 不返回模型,重新读取用于前端推送
            if let Ok(Some(updated)) =
                task_entity::Entity::find_by_id(task_id.to_string()).one(db).await
            {
                emit_task_progress(app, updated);
            }
        }
        Err(e) => tracing::warn!("回写任务进度失败: {e}"),
    }
}

/// 采集动作间隔:1.5~3s 随机。评论采集逐视频 / 画像补采逐作者之间串行插入,降低请求频率。
/// 复用 pool 的廉价熵源做法(系统时间纳秒),不引额外依赖。
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
    // 只取 max+1 个字符即知是否超长,不整串 collect(长评论避免不必要的内存放大)
    let mut chars = trimmed.chars();
    let head: String = chars.by_ref().take(max).collect();
    if chars.next().is_none() {
        return head;
    }
    let mut out = head;
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
        // 用户手动关闭采集窗口 = 终止补取(与采集主链路「关窗即终止」语义一致),
        // 不再为后续内容把刚关掉的窗口重新弹出
        if params
            .bridge
            .is_collect_window_closed(&params.cfg.id, params.account_id, Some(params.task_id))
        {
            emit_collect_log(
                params.app,
                params.task_id,
                "warn",
                "🛑 采集窗口已被手动关闭 · 终止直链补取".to_string(),
            );
            break;
        }
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
                    // 直链补取复用本任务的采集窗口(任务级 label)
                    task_id: Some(params.task_id),
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
/// 该阶段已无 window 句柄,故用 hud_log 按 平台+账号+任务 定位 HUD 窗口写入;
/// 此时采集窗口已主动关闭,定位不到即静默丢弃属预期(日志仍经 emit_collect_log 落库推前端,不丢)。
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
    crate::webview::hud_log(app, platform, account_id, Some(task_id), level, &msg);
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
/// DB 账号 Cookie。注意:采集主链路在释放账号锁前就会关窗,调用方须在关窗前调用并留存结果。
/// task_id 用于定位任务级采集窗口(与开窗 label 口径一致);None / 空回退账号级窗口。
async fn resolve_session_cookie(
    app: &AppHandle,
    db: &DatabaseConnection,
    platform: &str,
    account_id: &str,
    task_id: Option<&str>,
) -> Option<String> {
    use tauri::Manager;
    if let Some(home) = platform_home_url(platform) {
        let label = crate::webview::pool::task_window_label(platform, account_id, task_id);
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

/// 采集落库后下载内容素材。并发处理(限 15 路、不再限速),按 content_id 去重避免重复下载;
/// 副产品失败已在 media::process_content 内部吞为告警,主素材成败回写到 contents 表。
/// `platform`/`account_id` 用于把素材下载日志写进该账号采集窗口的 HUD 浮层。
/// 本 wrapper 保持「下载 → 语音转写 → 写终态」的旧行为,供补偿 / 重试路径使用;
/// 主链路(run_task)改用 download_media_core,把转写与终态排到评论采集之后。
async fn download_media_for_contents(
    params: &MediaDownloadParams<'_>,
    contents: Vec<Content>,
) {
    if contents.is_empty() {
        return;
    }
    let audios = download_media_core(params, contents).await;
    // 素材下载完成后统一做语音转写(视频音频→文案),仅任务开了「AI 文案提取」才转写;
    // 只开「音频提取」时音频留存即可。失败仅告警不影响任务终态
    if params.ai_extract && !params.bridge.is_task_stopping(params.task_id) {
        transcribe_for_contents(params.app, params.db, params.task_id, params.platform, params.account_id, params.transcription_cfg, params.media_cfg.ffmpeg_path.clone(), Some(params.bridge), audios).await;
    }
    // 素材全部处理完毕,任务从 downloading_media 收尾为 completed
    write_task_done(params.app, params.db, params.task_id).await;
}

/// 素材下载主体:并发下载 + 音频提取,逐条回写素材结果与进度。
/// 返回转出的音频清单(content row id, mp3 路径);语音转写与任务终态由调用方决定。
async fn download_media_core(
    params: &MediaDownloadParams<'_>,
    contents: Vec<Content>,
) -> Vec<(String, String)> {
    if contents.is_empty() {
        return Vec::new();
    }
    let root = crate::media::media_root(params.config_dir, params.media_cfg);
    use futures_util::StreamExt;
    // 该任务账号的会话 Cookie:整批同平台同账号取一次,给所有内容的 ffmpeg 拉流复用。
    // 采集主链路在关窗前已解析并传入(下载阶段窗口已销毁);未传入的调用方(补偿/重试)现场解析,
    // 优先读存活采集窗口的实时 Cookie(含 httponly tt_chain_token,与本次直链匹配),退回 DB。
    let cookie = match &params.session_cookie {
        Some(c) => Some(c.clone()),
        None => {
            resolve_session_cookie(params.app, params.db, params.platform, params.account_id, Some(params.task_id)).await
        }
    };
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
    // 任务在采集阶段已被手动结束:整条素材阶段不再启动,音频清单为空(终态由调用方写)
    if params.bridge.is_task_stopping(params.task_id) {
        emit_media_log(params.app, params.task_id, params.platform, params.account_id, "info", "🛑 已手动结束 · 跳过素材下载与语音转写".to_string());
        return Vec::new();
    }
    let mut count = 0usize;
    let mut failed = 0usize;
    // 素材结果攒批回写:每攒够 MEDIA_OUTCOME_FLUSH_SIZE 条在一个事务里统一 UPDATE
    let mut pending_outcomes: Vec<(String, crate::media::MediaOutcome)> = Vec::new();
    // 素材进度回写节流:≤600ms 合并,最后一条必写
    let mut last_media_write =
        std::time::Instant::now() - std::time::Duration::from_secs(1);
    // 并发下载(限 15 路并发,不再串行限速),边完成边回写结果与进度
    let root_ref = &root;
    // 任务停止标志:停止时置位,在飞的 ffmpeg 拉流转码 500ms 内被强杀(见 media::extract_audio_from_url)
    let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let mut stream = futures_util::stream::iter(targets.into_iter().map(|content| {
        let cancel = cancel.clone();
        async move {
        // 标题在下载前取(content 随后 move 进 process_content);用于 HUD 逐条日志展示
        let title = log_content_title(&content);
        // 素材类型标签(实时日志按类型着色):视频且开了音频提取 → [音频];图文 → [图片];其余(仅封面/头像)→ [封面]
        let tag = if content.kind == ContentKind::Video && params.audio_extract {
            "素材[音频]"
        } else if content.kind == ContentKind::Video {
            "素材[封面]"
        } else {
            "素材[图片]"
        };
        let outcome = crate::media::process_content(
            &content,
            root_ref,
            params.media_cfg,
            params.audio_extract,
            cookie_ref,
            Some(cancel),
        )
        .await;
        let id = format!("{}-{}-{}", params.task_id, content.platform, content.content_id);
        (id, title, tag, outcome)
        }
    }))
    .buffer_unordered(15);
    while let Some((id, title, tag, outcome)) = stream.next().await {
        // 任务被手动结束:不再启动新下载(stream 随 break 丢弃,未开始的条目不执行;在飞 ≤15 条跑完即弃)
        if params.bridge.is_task_stopping(params.task_id) {
            emit_media_log(params.app, params.task_id, params.platform, params.account_id, "info", format!("🛑 已手动结束 · 停止素材下载(已完成 {count}/{total} 条保留)"));
            cancel.store(true, std::sync::atomic::Ordering::Relaxed);
            break;
        }
        let ok = is_media_ok(&outcome);
        if !ok {
            failed += 1;
        }
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
                format!("{tag} {count}/{total} · {title} · 完成{extra}"),
            );
        } else {
            let reason = outcome.error.as_deref().unwrap_or("未知原因");
            emit_media_log(
                params.app,
                params.task_id,
                params.platform,
                params.account_id,
                "warn",
                format!("{tag} {count}/{total} · {title} · 失败:{reason}"),
            );
        }
        // 逐条回写进度(节流合并),调度页据此刷新「素材下载中 done/total」;最后一条必写
        if count == total || last_media_write.elapsed().as_millis() as u64 >= PROGRESS_WRITE_MIN_INTERVAL_MS
        {
            write_task_media_done(params.app, params.db, params.task_id, count as i32).await;
            last_media_write = std::time::Instant::now();
        }
        // 素材结果攒批:满一批事务性回写,减少 SQLite 锁获取与提交次数
        pending_outcomes.push((id, outcome));
        if pending_outcomes.len() >= MEDIA_OUTCOME_FLUSH_SIZE {
            flush_media_outcomes(params.db, &mut pending_outcomes).await;
        }
    }
    // 收尾 flush 剩余素材回写
    flush_media_outcomes(params.db, &mut pending_outcomes).await;
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
    audios
}

/// 采集结束后统一语音转写:把每条视频转出的音频逐条调 ASR 厂商,回写 content.transcript。
/// 按系统设置「语音转写」的并发数限速并发,失败仅告警不中断;未配置/厂商不支持 ASR 则跳过。不占采集通道(主体已结束)。
#[allow(clippy::too_many_arguments)]
async fn transcribe_for_contents(
    app: &AppHandle,
    db: &DatabaseConnection,
    task_id: &str,
    platform: &str,
    account_id: &str,
    transcription_cfg: &veltrix_core::config::TranscriptionConfig,
    // 大音频(>10MB)切片用的 ffmpeg 路径;None 用系统 PATH
    ffmpeg_path: Option<String>,
    // 任务停止标记;None 表示不检查(内容库单条重试等无任务上下文的调用方)
    bridge: Option<&CollectBridge>,
    audios: Vec<(String, String)>,
) {
    if audios.is_empty() {
        return;
    }
    use tauri::Emitter;
    // 任务已被手动结束(素材下载期间点的结束):不再发起任何转写请求
    if bridge.map(|b| b.is_task_stopping(task_id)).unwrap_or(false) {
        emit_media_log(app, task_id, platform, account_id, "info", "🛑 已手动结束 · 跳过语音转写".to_string());
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
    // 并发调 ASR API:并发数取系统设置「语音转写」配置(0 兜底默认 5);buffer_unordered 即滚动补位:同时在飞 ≤ concurrency,完成一个立刻拉取下一个,避免打爆 rate limit
    let concurrency = if transcription_cfg.concurrency == 0 {
        veltrix_core::config::DEFAULT_ASR_CONCURRENCY
    } else {
        transcription_cfg.concurrency
    } as usize;
    emit_media_log(app, task_id, platform, account_id, "info", format!("开始语音转写 · 共 {total} 条 · 并发 {concurrency} 路"));
    let done = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    // 在飞计数监控:进入转写 +1、完成 -1,进度日志可见实时在飞路数;一旦超上限说明并发控制退化,记 error 便于排查
    let in_flight = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    // SQLite 写串行化:并发 ASR 调用完成后,DB 写入(record_transcription_usage + record_transcript)
    // 通过此 Mutex 串行执行,避免「database is locked」导致转写结果静默丢失。
    let db_write_lock = std::sync::Arc::new(tokio::sync::Mutex::new(()));
    use futures_util::StreamExt;
    let mut stream = futures_util::stream::iter(audios.into_iter().map(|(id, audio_path)| {
        let api_key = api_key.clone();
        let cfg_provider = transcription_cfg.provider.clone();
        let cfg_url = transcription_cfg.api_url.clone();
        let cfg_model = transcription_cfg.model.clone();
        let ffmpeg_path = ffmpeg_path.clone();
        let db = db.clone();
        let app = app.clone();
        let task_id = task_id.to_string();
        let platform = platform.to_string();
        let account_id = account_id.to_string();
        let done = done.clone();
        let in_flight = in_flight.clone();
        let concurrency = concurrency;
        let total_s = total.to_string();
        let db_write_lock = db_write_lock.clone();
        async move {
            // 在飞 +1:buffer_unordered 保证完成一个才补一个,正常不会超 concurrency
            let flying = in_flight.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
            if flying > concurrency {
                tracing::error!(flying, concurrency, "转写在飞数超并发上限,并发控制失效");
            }
            let result = crate::llm::transcribe(crate::llm::TranscribeRequest {
                provider_code: &cfg_provider,
                api_url: &cfg_url,
                api_key: &api_key,
                model: &cfg_model,
                audio_path: std::path::Path::new(&audio_path),
                ffmpeg_path: ffmpeg_path.as_deref(),
            })
            .await;
            match result {
                Ok(outcome) => {
                    // SQLite 写需串行:并发 ASR 结果回写时持锁,防止 database is locked
                    let _guard = db_write_lock.lock().await;
                    record_transcription_usage(&db, &id, &cfg_model, &cfg_provider, &outcome.usages).await;
                    record_transcript(&db, &id, Some(outcome.text.clone()), None).await;
                    drop(_guard);
                    // 通知前端就地刷新该行文案:批量/采集后转写期间,全量库「未转写」计数实时随完成递减
                    let _ = app.emit(
                        "content-transcript-updated",
                        serde_json::json!({ "id": id, "transcript": outcome.text, "transcriptError": null }),
                    );
                }
                Err(e) => {
                    tracing::warn!(content_id = %id, "语音转写失败: {e}");
                    let err = format!("{e}");
                    let _guard = db_write_lock.lock().await;
                    record_transcript(&db, &id, None, Some(err.clone())).await;
                    drop(_guard);
                    let _ = app.emit(
                        "content-transcript-updated",
                        serde_json::json!({ "id": id, "transcript": null, "transcriptError": err }),
                    );
                }
            }
            let n = done.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
            let remain = in_flight.fetch_sub(1, std::sync::atomic::Ordering::Relaxed) - 1;
            emit_media_log(&app, &task_id, &platform, &account_id, "info", format!("转写进度 {n}/{total_s} · 在飞 {remain} 路"));
        }
    }))
    .buffer_unordered(concurrency);
    while stream.next().await.is_some() {
        // 任务被手动结束:停止拉取后续转写(在飞条目跑完即弃,已完成结果已回写保留)
        if bridge.map(|b| b.is_task_stopping(task_id)).unwrap_or(false) {
            emit_media_log(app, task_id, platform, account_id, "info", "🛑 已手动结束 · 停止后续语音转写(已完成条目保留)".to_string());
            break;
        }
    }
    emit_media_log(app, task_id, platform, account_id, "info", format!("语音转写完成 · {total}/{total}"));
}

/// 记录一次转写的 ASR 用量到账单(model_usage_records):每次 API 请求一条记录
/// (切片转写一段一条,GLM 无 usage 字段则 token 记 0、仅计请求次数)。
/// 归属取内容行的 owner(self 用户账单只统计自己名下);查不到归属 / 写库失败仅告警跳过,不影响转写。
async fn record_transcription_usage(
    db: &DatabaseConnection,
    content_id: &str,
    model: &str,
    provider: &str,
    usages: &[crate::llm::chat::TokenUsage],
) {
    use veltrix_core::db::entity::{content as content_entity, model_usage_record};
    let owner = match content_entity::Entity::find_by_id(content_id.to_string())
        .one(db)
        .await
    {
        Ok(Some(row)) => row.owner,
        Ok(None) => {
            tracing::warn!(content_id, "转写记账跳过:内容不存在");
            return;
        }
        Err(e) => {
            tracing::warn!(content_id, "转写记账跳过:查询内容归属失败: {e}");
            return;
        }
    };
    for u in usages {
        if let Err(e) = model_usage_record::Model::record(
            db,
            model,
            provider,
            u.prompt,
            u.completion,
            "transcription",
            &owner,
        )
        .await
        {
            tracing::warn!(content_id, "转写账单记录写入失败: {e}");
        }
    }
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
/// 并入采集去重台账(同平台)的 content_id:本任务已采 ∪ 台账已登记的内容构成「去重跳过集」,
/// 采集时整体跳过(不再入库刷新,评论 / 素材阶段也不处理),避免重复采集。
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
    // 台账仅加载近 90 天记录:台账无限增长,全量加载随历史线性膨胀内存;
    // 90 天外的历史内容极少被搜索召回,即便漏掉也只是把个别老视频当作新内容
    // 重采一次(upsert 刷新 + 评论重采),不会丢数据。时间窗口是 pragmatic 的折中,
    // 避免 O(台账规模) 加载。
    let cutoff = Utc::now().timestamp() - 90 * 24 * 3600;
    let recorded: Vec<String> = ledger_entity::Entity::find()
        .filter(ledger_entity::Column::Platform.eq(platform))
        .filter(ledger_entity::Column::CreatedAt.gte(cutoff))
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
/// 一次性取本任务内容行的 content_id + media_status,避免逐条查库(N+1)。
/// 只下载本任务实际落了库的内容:正常流程 persist 先于下载,行必然存在;
/// 「无行」只剩 persist 失败的异常路径,此时回写也无行可写,跳过下载避免白下。
async fn filter_pending_media(
    db: &DatabaseConnection,
    task_id: &str,
    contents: Vec<Content>,
) -> Vec<Content> {
    use sea_orm::{ColumnTrait, QueryFilter, QuerySelect};
    use veltrix_core::db::entity::content as content_entity;
    let rows: Vec<(String, Option<String>)> = match content_entity::Entity::find()
        .filter(content_entity::Column::TaskId.eq(task_id))
        .select_only()
        .column(content_entity::Column::ContentId)
        .column(content_entity::Column::MediaStatus)
        .into_tuple::<(String, Option<String>)>()
        .all(db)
        .await
    {
        Ok(r) => r,
        Err(e) => {
            // 查询失败不能按「无记录」处理:present 为空会把待下载清单整个过滤掉,
            // 素材阶段静默跳过、任务却照常落 completed,内容永久停在 pending。
            // 宁可原样返回待下载列表(重下一次),不可漏下
            tracing::warn!(task_id = %task_id, "查询素材状态失败,按全部待下载处理: {e}");
            let mut seen: HashSet<String> = HashSet::new();
            return contents
                .into_iter()
                .filter(|c| seen.insert(c.content_id.clone()))
                .collect();
        }
    };
    let present: HashSet<String> = rows.iter().map(|(id, _)| id.clone()).collect();
    let done: HashSet<String> = rows
        .into_iter()
        .filter(|(_, status)| status.as_deref() == Some("success"))
        .map(|(id, _)| id)
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
        // 本任务无此行(台账判重被 persist 跳过)→ 不下,回写也无行可写
        if !present.contains(&c.content_id) {
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

/// 素材回写攒批条数:每攒够一批在一个事务里逐条 UPDATE,减少 SQLite 锁获取与提交次数。
const MEDIA_OUTCOME_FLUSH_SIZE: usize = 20;

/// 构造素材处理结果回写的 ActiveModel(仅更新状态相关列,不触碰其它字段)。
fn media_outcome_active(
    id: &str,
    outcome: &crate::media::MediaOutcome,
) -> veltrix_core::db::entity::content::ActiveModel {
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
    am
}

/// 把单条素材处理结果回写到 contents 表(补偿 / 重试等低频路径)。
async fn record_media_outcome(db: &DatabaseConnection, id: &str, outcome: &crate::media::MediaOutcome) {
    let am = media_outcome_active(id, outcome);
    if let Err(e) = am.update(db).await {
        tracing::warn!(content_id = %id, "回写素材状态失败: {e}");
    }
}

/// 素材回写批量 flush:攒批后在一个事务里逐条 UPDATE,减少 SQLite 锁获取与提交次数。
/// 任一条失败仅告警不阻断;事务开启失败退回逐条写,行为与旧版一致。
async fn flush_media_outcomes(
    db: &DatabaseConnection,
    batch: &mut Vec<(String, crate::media::MediaOutcome)>,
) {
    if batch.is_empty() {
        return;
    }
    let items = std::mem::take(batch);
    match db.begin().await {
        Ok(tx) => {
            for (id, outcome) in &items {
                let am = media_outcome_active(id, outcome);
                if let Err(e) = am.update(&tx).await {
                    tracing::warn!(content_id = %id, "回写素材状态失败: {e}");
                }
            }
            if let Err(e) = tx.commit().await {
                tracing::warn!("素材状态批量回写提交失败: {e}");
            }
        }
        Err(e) => {
            tracing::warn!("开启素材状态回写事务失败,退回逐条写: {e}");
            for (id, outcome) in &items {
                let am = media_outcome_active(id, outcome);
                if let Err(e) = am.update(db).await {
                    tracing::warn!(content_id = %id, "回写素材状态失败: {e}");
                }
            }
        }
    }
}

/// 兜底解析失败时:从 DB 回读本任务已落库的内容补进素材下载列表(按 content_id 去重)。
/// 增量通道落库的内容此前只在兜底解析成功时才进列表;解析失败时这些行会永久停在 pending。
async fn backfill_contents_for_media(
    db: &DatabaseConnection,
    task_id: &str,
    shared: &mut CollectSharedState,
) {
    use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
    use veltrix_core::db::entity::content as content_entity;
    let existing: HashSet<String> = shared
        .contents_for_media
        .iter()
        .map(|c| c.content_id.clone())
        .collect();
    match content_entity::Entity::find()
        .filter(content_entity::Column::TaskId.eq(task_id))
        .all(db)
        .await
    {
        Ok(rows) => {
            let mut added = 0usize;
            for row in rows {
                if existing.contains(&row.content_id) {
                    continue;
                }
                shared.contents_for_media.push(content_from_model(&row));
                added += 1;
            }
            if added > 0 {
                tracing::info!(task_id, added, "兜底解析失败,已从 DB 回补素材下载列表");
            }
        }
        Err(e) => tracing::warn!(task_id, "兜底回补素材列表失败: {e}"),
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

/// 单条内容素材状态视图:retry_content_media / retry_content_transcript 返回最新状态,前端就地刷新该行。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaStatusView {
    pub id: String,
    pub media_status: Option<String>,
    pub audio_extracted: Option<bool>,
    pub media_error: Option<String>,
    /// 最新转写文本 / 失败原因(音频重试成功会顺带补转写,转写重试会更新两者)
    pub transcript: Option<String>,
    pub transcript_error: Option<String>,
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
    // 重试遵循任务的「音频提取 / AI 文案提取」设置:前者决定视频是否下载并转音频,后者决定是否补转写
    let (audio_extract, ai_extract) = match veltrix_core::db::entity::task::Entity::find_by_id(
        row.task_id.clone(),
    )
    .one(&state.db)
    .await
    {
        Ok(Some(t)) => (t.audio_extract || t.ai_extract, t.ai_extract),
        other => {
            // 查不到任务行(DB 错误 / 任务已删)不能静默按「不提取」处理:那会让视频
            // 「封面下载成功即判成功」而音频仍缺。按内容行自身状态推断:
            // 视频且尚无音频 → 仍需下载转音频;不补转写(无任务设置可依)
            tracing::warn!(content_id = %id, "重试:查询任务提取设置失败({other:?}),按内容行状态推断");
            let need_audio = row.kind == "video"
                && row.audio_path.as_deref().map(|s| s.is_empty()).unwrap_or(true);
            (need_audio, false)
        }
    };
    let mut content = content_from_model(&row);
    // 重试无绑定账号:取该平台一个可用账号的 Cookie 供 ffmpeg 拉流(防盗链 CDN 校验会话)
    let cookie = fetch_platform_cookie(&state.db, &row.platform).await;
    let mut outcome =
        crate::media::process_content(&content, &root, &media_cfg, audio_extract, cookie.as_deref(), None)
            .await;

    // 视频素材失败(典型:直链短期签名过期;无直链时 process_video 未执行、
    // audio_extracted 为 None,同样是缺直链场景)→ 经详情页强制重取新鲜直链后再试一次。
    if audio_extract && content.kind == ContentKind::Video && outcome.audio_extracted != Some(true) {
        let platform_cfg = { lock_config(&state)?.platforms.get(&row.platform).cloned() };
        match (platform_cfg, state.cookies.acquire(&row.platform).await) {
            (Some(cfg), Ok(acc)) => {
                // 直链补取要占用采集窗口:先拿同账号互斥锁,防与在跑的采集任务
                // 并发操控同一 WebView、互吃拦截响应(与采集主链路同一把锁)
                let refreshed = {
                    let refresh_lock = account_collect_lock(
                        &state.collect_locks,
                        &account_lock_key(&row.platform, &acc.id),
                    );
                    let refresh_guard = refresh_lock.lock().await;
                    let bridge = CollectBridge::new(
                        state.webviews.clone(),
                        state.intercept_channel.clone(),
                        state.rpa_channel.clone(),
                        state.collect_control.clone(),
                    );
                    // 重置残留的「手动关窗」标记(理由同补偿路径)
                    bridge.reset_collect_window_closed(&row.platform, &acc.id, Some(&row.task_id));
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
                    let changed = content.video_url != before;
                    let session_cookie = if changed {
                        resolve_session_cookie(&app, &state.db, &row.platform, &acc.id, Some(&row.task_id)).await
                    } else {
                        None
                    };
                    // 持锁期间关窗(防窗口遗留驻留 / 迟关误杀下一任务复用的窗口)。
                    // 媒体重试(下载 + ffmpeg,可达数分钟)不占窗口,必须移出锁外——
                    // 此前整个重试都在锁内,期间同账号所有采集/补采被堵死
                    bridge.close_collect_window(&row.platform, &acc.id, Some(&row.task_id));
                    // 自清主动关窗置位的「被手动关闭」标记(理由同采集主链路)
                    bridge.reset_collect_window_closed(&row.platform, &acc.id, Some(&row.task_id));
                    drop(refresh_guard);
                    changed.then_some(session_cookie)
                };
                if let Some(session_cookie) = refreshed {
                    outcome = crate::media::process_content(
                        &content,
                        &root,
                        &media_cfg,
                        audio_extract,
                        session_cookie.as_deref(),
                        None, // 单条重试:无任务停止标志
                    )
                    .await;
                }
            }
            (cfg_opt, acc_res) => {
                // 跳过刷新要说明原因:此前静默跳过,用户看到「重试仍失败」却不知连刷新都没发生
                let reason = match (cfg_opt.is_none(), acc_res.err()) {
                    (true, _) => "无平台配置".to_string(),
                    (false, Some(e)) => format!("无可用账号: {e}"),
                    _ => "未知原因".to_string(),
                };
                tracing::warn!(content_id = %id, "重试:跳过直链刷新({reason})");
            }
        }
    }
    record_media_outcome(&state.db, &id, &outcome).await;

    // 音频提取重试成功且任务开了「AI 文案提取」且尚无文案:顺带补一次语音转写,
    // 让「音频提取失败重试」一次修复全链路(只开音频提取的任务不自动转写,可手动单条转写)。
    if ai_extract && outcome.audio_extracted == Some(true) && row.transcript.is_none() {
        if let Some(audio_path) = outcome.audio_path.clone() {
            let transcription_cfg = { lock_config(&state)?.transcription.clone() };
            transcribe_for_contents(
                &app,
                &state.db,
                &row.task_id,
                &row.platform,
                "",
                &transcription_cfg,
                media_cfg.ffmpeg_path.clone(),
                None, // 单条重试:无任务停止标记可查
                vec![(id.clone(), audio_path)],
            )
            .await;
        }
    }

    let status = if is_media_ok(&outcome) { "success" } else { "failed" };
    // 回读最新转写结果(可能刚被补上)一并返回,前端就地刷新行内三个徽章
    let (transcript, transcript_error) = content_entity::Entity::find_by_id(id.clone())
        .one(&state.db)
        .await
        .ok()
        .flatten()
        .map(|r| (r.transcript, r.transcript_error))
        .unwrap_or((None, None));
    Ok(MediaStatusView {
        id,
        media_status: Some(status.to_string()),
        audio_extracted: outcome.audio_extracted,
        media_error: outcome.error,
        transcript,
        transcript_error,
    })
}

/// 文案转写失败重试:对已有音频的单条内容重跑语音转写并回写结果。
/// 覆盖「素材成功但转写失败 / 当时未配 API Key 被跳过」的内容——这类内容补偿与重跑都不会再碰。
#[tauri::command]
pub async fn retry_content_transcript(
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
    let Some(audio_path) = row.audio_path.clone() else {
        return Err(CrawlerError::Config(
            "该内容没有可用音频,请先重试素材 / 音频提取".into(),
        ));
    };
    // 提前校验 API Key:转写缺少 Key 会被静默跳过,命令层应给用户明确反馈
    let api_key = get_secret(&state.db, "transcription_api_key").await;
    if api_key.trim().is_empty() {
        return Err(CrawlerError::Config(
            "未配置语音转写 API Key,请到「系统设置 → 语音转写」填写".into(),
        ));
    }
    let transcription_cfg = { lock_config(&state)?.transcription.clone() };
    let ffmpeg_path = { lock_config(&state)?.media.ffmpeg_path.clone() };
    transcribe_for_contents(
        &app,
        &state.db,
        &row.task_id,
        &row.platform,
        "",
        &transcription_cfg,
        ffmpeg_path,
                None, // 单条重试:无任务停止标记可查
        vec![(id.clone(), audio_path)],
    )
    .await;
    let (transcript, transcript_error) = content_entity::Entity::find_by_id(id.clone())
        .one(&state.db)
        .await
        .ok()
        .flatten()
        .map(|r| (r.transcript, r.transcript_error))
        .unwrap_or((None, None));
    Ok(MediaStatusView {
        id,
        media_status: row.media_status,
        audio_extracted: row.audio_extracted,
        media_error: row.media_error,
        transcript,
        transcript_error,
    })
}

/// 批量转写文案:对指定内容(前端当前筛选列表中「有音频但无文案」的条目)重跑语音转写并回写。
/// 按任务分组逐组执行(日志归入各自任务的采集日志),组内并发遵循系统设置「语音转写」的并发数。
/// 返回本次处理的条数(0 = 没有可转写的)。
#[tauri::command]
pub async fn retry_failed_transcripts(
    state: State<'_, AppState>,
    app: AppHandle,
    ids: Vec<String>,
) -> Result<u32> {
    use veltrix_core::db::entity::content as content_entity;
    let me = current_user(&state).ok_or_else(|| CrawlerError::Config("未登录".into()))?;
    if ids.is_empty() {
        return Ok(0);
    }
    // 提前校验 API Key:转写缺少 Key 会被静默跳过,命令层应给用户明确反馈
    let api_key = get_secret(&state.db, "transcription_api_key").await;
    if api_key.trim().is_empty() {
        return Err(CrawlerError::Config(
            "未配置语音转写 API Key,请到「系统设置 → 语音转写」填写".into(),
        ));
    }
    let mut query = content_entity::Entity::find()
        .filter(content_entity::Column::Id.is_in(ids))
        // 防御:仅处理确有音频的条目(前端口径之外的数据改动不放大)
        .filter(content_entity::Column::AudioPath.is_not_null())
        .filter(content_entity::Column::AudioPath.ne(""));
    // 数据归属:self 用户只转写自己的内容
    if me.scope == "self" {
        query = query.filter(content_entity::Column::Owner.eq(&me.name));
    }
    let rows = query
        .all(&state.db)
        .await
        .map_err(|e| CrawlerError::Config(format!("查询待转写内容失败: {e}")))?;
    // 防御:已有文案的条目不重试(如行级重试刚成功,前端列表还没刷新)
    let rows: Vec<_> = rows
        .into_iter()
        .filter(|r| r.transcript.as_deref().map(str::trim).unwrap_or("").is_empty())
        .collect();
    if rows.is_empty() {
        return Ok(0);
    }
    let total = rows.len() as u32;
    let transcription_cfg = { lock_config(&state)?.transcription.clone() };
    let ffmpeg_path = { lock_config(&state)?.media.ffmpeg_path.clone() };
    // 按 (任务, 平台) 分组:逐组调 transcribe_for_contents,组内并发按配置,组间串行
    let mut by_task: std::collections::HashMap<(String, String), Vec<(String, String)>> =
        std::collections::HashMap::new();
    for r in rows {
        by_task
            .entry((r.task_id.clone(), r.platform.clone()))
            .or_default()
            .push((r.id.clone(), r.audio_path.clone().unwrap_or_default()));
    }
    for ((task_id, platform), audios) in by_task {
        transcribe_for_contents(
            &app,
            &state.db,
            &task_id,
            &platform,
            "",
            &transcription_cfg,
            ffmpeg_path.clone(),
            None, // 批量补转写:无任务停止标记可查
            audios,
        )
        .await;
    }
    Ok(total)
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
    use sea_orm::sea_query::Expr;
    use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
    use veltrix_core::db::entity::task as task_entity;

    let me = current_user(&state).ok_or_else(|| CrawlerError::Config("未登录".into()))?;

    // 先查 owner 做越权校验（在原子 UPDATE 之前，避免对不存在/无权限的任务改状态）
    let model = task_entity::Entity::find_by_id(id.clone())
        .one(&state.db)
        .await
        .map_err(|e| CrawlerError::Config(format!("查询任务失败: {e}")))?
        .ok_or_else(|| CrawlerError::Config("任务不存在".into()))?;
    if me.scope == "self" && model.owner != me.name {
        return Err(CrawlerError::Config("无权操作该任务".into()));
    }

    // 原子防重入:仅当 status=failed 时才翻转为 downloading_media,与 run_task 的原子 guard 同模式。
    // 两次并发补偿点击只有一个能成功(rows_affected==0 即被抢占)。
    let now = Utc::now().timestamp();
    let res = task_entity::Entity::update_many()
        .col_expr(task_entity::Column::Status, Expr::value("downloading_media"))
        .col_expr(task_entity::Column::Progress, Expr::value(100))
        .col_expr(task_entity::Column::MediaDone, Expr::value(0))
        .col_expr(task_entity::Column::UpdatedAt, Expr::value(now))
        .filter(task_entity::Column::Id.eq(id.clone()))
        .filter(task_entity::Column::Status.eq("failed"))
        .exec(&state.db)
        .await
        .map_err(|e| CrawlerError::Config(format!("更新任务状态失败: {e}")))?;
    if res.rows_affected == 0 {
        return Err(CrawlerError::Config(
            "仅失败任务可补偿,且不可重复补偿".into(),
        ));
    }
    // 推送状态变更,前端立即翻转为「素材下载中」
    if let Ok(Some(updated)) = task_entity::Entity::find_by_id(id.clone())
        .one(&state.db)
        .await
    {
        emit_task_progress(&app, updated);
    }

    let ai_extract = model.ai_extract;
    // 音频提取(含 AI 文案提取隐含需求):决定补偿时视频是否下载转音频
    let audio_extract = model.audio_extract || model.ai_extract;
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
    // 直链补取占采集窗口:同账号互斥锁(与采集主链路同一把),防并发操控同一 WebView
    let collect_locks = state.collect_locks.clone();

    // panic 兜底所需:补偿体 panic 时仍能把任务落终态(否则永久卡「素材下载中」)
    let app_guard = app.clone();
    let db_guard = db.clone();
    let id_guard = id.clone();
    tauri::async_runtime::spawn(async move {
        use futures_util::FutureExt;
        let body = std::panic::AssertUnwindSafe(async move {
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
            let analyzed = analyze_comments_intent(&app, &db, &id, &intent_cfg).await;
            // 确有评论被分析出结果才标记(与采集主链路同口径;0 产出标记会掩盖「实际没分析」)
            if analyzed > 0 {
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
        }

        // 素材下载 + 转写补做(音频提取 / AI 文案提取任一开即补;filter_pending_media 排除已成功的)
        if audio_extract {
            let contents: Vec<Content> = rows.iter().map(content_from_model).collect();
            let mut pending = filter_pending_media(&db, &id, contents).await;
            if !pending.is_empty() {
                write_task_downloading(&app, &db, &id, pending.len() as i32).await;
                // 补直链:小红书无直链 / 缺直链的视频先经详情页拦截补取,再下载转音频。
                // 顺带记下补取用的账号,作为媒体下载日志写 HUD 的目标窗口。
                let mut hud_account = String::new();
                let mut session_cookie: Option<String> = None;
                match (&platform_cfg, cookies.acquire(&platform).await) {
                    (Some(cfg), Ok(acc)) => {
                        hud_account = acc.id.clone();
                        // 直链补取占采集窗口:同账号互斥锁,防与在跑的采集任务
                        // 并发操控同一 WebView、互吃拦截响应(与采集主链路同一把锁)
                        let refresh_lock = account_collect_lock(
                            &collect_locks,
                            &account_lock_key(&platform, &acc.id),
                        );
                        let _refresh_guard = refresh_lock.lock().await;
                        // 持锁期间该账号无采集在跑,残留的「手动关窗」标记只会是历史遗留,
                        // 重置后再补取(否则补取循环第一轮就误判终止)
                        bridge.reset_collect_window_closed(&platform, &acc.id, Some(&id));
                        let stream_params = StreamRefreshParams {
                            app: &app,
                            bridge: &bridge,
                            registry: &registry,
                            db: &db,
                            cfg,
                            account_id: &acc.id,
                            task_id: &id,
                        };
                        // force=true:补偿对象多为「有直链但签名过期」的失败内容,
                        // force=false 会因「已有直链」跳过重取,下载仍 403,补偿反复失败
                        refresh_stream_urls(
                            &stream_params, &mut pending, true,
                        )
                        .await;
                        // 直链与本次窗口会话绑定:关窗前留存 Cookie 供下载;
                        // 持锁期间关窗,避免窗口遗留驻留、或迟关误杀下一任务复用的窗口
                        session_cookie =
                            resolve_session_cookie(&app, &db, &platform, &acc.id, Some(&id)).await;
                        bridge.close_collect_window(&platform, &acc.id, Some(&id));
                        // 自清主动关窗置位的「被手动关闭」标记(理由同采集主链路)
                        bridge.reset_collect_window_closed(&platform, &acc.id, Some(&id));
                    }
                    (cfg_opt, acc_res) => {
                        // 不补直链也要说明原因:无平台配置 / 无可用账号时静默跳过,
                        // 随后无直链的内容下载必失败,排查时无从得知补取根本没发生
                        let reason = match (cfg_opt.is_none(), acc_res.err()) {
                            (true, _) => "无平台配置".to_string(),
                            (false, Some(e)) => format!("无可用账号: {e}"),
                            _ => "未知原因".to_string(),
                        };
                        tracing::warn!(task_id = %id, "补偿:跳过直链补取({reason})");
                        emit_collect_log(
                            &app, &id, "warn",
                            format!("⚠️ 跳过直链补取 · {reason},缺直链的内容可能下载失败"),
                        );
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
                    audio_extract,
                    ai_extract,
                    session_cookie,
                    bridge: &bridge,
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
        // 与 run_task 相同的 panic 兜底:补偿体 panic 不再让任务永久卡「素材下载中」
        if body.catch_unwind().await.is_err() {
            tracing::error!(task_id = %id_guard, "补偿任务 panic,已落 failed");
            write_task_failed(
                &app_guard,
                &db_guard,
                &id_guard,
                "补偿任务内部错误(已中断),可重试补偿",
            )
            .await;
        }
    });
    Ok(())
}

/// 全量库「补采评论」的结果汇总。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecollectCommentsSummary {
    /// 请求的内容条数(前端选中数)。
    pub requested: usize,
    /// 实际发起评论采集的视频数(排除评论数为 0 / 平台不支持等跳过项)。
    pub attempted: usize,
    /// 采到非空评论响应且解析成功的视频数。
    pub succeeded: usize,
    pub failed: usize,
    pub skipped: usize,
    /// 本次入库的评论条数(按时间范围 / 单视频上限过滤后)。
    pub comments: usize,
    /// 跳过 / 失败明细(逐条;前端 toast 只显汇总,明细打控制台)。
    pub messages: Vec<String>,
}

/// 全量库「补采评论」:对选中内容按评论参数(时间范围 / 单视频上限 / 意向分析)重采一级评论。
/// 采集通道与任务评论阶段共用 `CollectBridge::collect_comments`(导航详情页 + 拦截 / API 直采);
/// 窗口用账号级(task_id=None,与画像补采同口径),评论落库归属各内容原任务,
/// 采成功的内容回写 comment_collected=true,可选按任务补做意向分析。
#[tauri::command]
pub async fn recollect_comments(
    state: State<'_, AppState>,
    app: AppHandle,
    ids: Vec<String>,
    comment_time_range: String,
    comment_limit: usize,
    analyze_intent: bool,
) -> Result<RecollectCommentsSummary> {
    use sea_orm::sea_query::Expr;
    use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
    use veltrix_core::db::entity::content as content_entity;

    let me = current_user(&state).ok_or_else(|| CrawlerError::Config("未登录".into()))?;
    let mut summary = RecollectCommentsSummary {
        requested: ids.len(),
        attempted: 0,
        succeeded: 0,
        failed: 0,
        skipped: 0,
        comments: 0,
        messages: Vec::new(),
    };
    if ids.is_empty() {
        return Ok(summary);
    }

    let mut query = content_entity::Entity::find().filter(content_entity::Column::Id.is_in(ids));
    // 数据归属:self 用户只补采自己的内容
    if me.scope == "self" {
        query = query.filter(content_entity::Column::Owner.eq(&me.name));
    }
    let rows = query
        .all(&state.db)
        .await
        .map_err(|e| CrawlerError::Config(format!("查询内容失败: {e}")))?;
    if rows.len() < summary.requested {
        let missing = summary.requested - rows.len();
        summary.skipped += missing;
        summary
            .messages
            .push(format!("{missing} 条内容不存在(可能已删除),已跳过"));
    }

    // 按平台分组:评论采集按平台取配置 / 适配器 / 账号;组间串行、组内逐视频串行
    let mut by_platform: std::collections::HashMap<String, Vec<content_entity::Model>> =
        std::collections::HashMap::new();
    for r in rows {
        by_platform.entry(r.platform.clone()).or_default().push(r);
    }

    let bridge = CollectBridge::new(
        state.webviews.clone(),
        state.intercept_channel.clone(),
        state.rpa_channel.clone(),
        state.collect_control.clone(),
    );
    let intent_cfg = { lock_config(&state)?.intent.clone() };
    let cutoff = comment_time_cutoff(&comment_time_range);
    let mut seen_contents: HashSet<String> = HashSet::new();
    let mut seen_comments: HashSet<String> = HashSet::new();
    let mut comment_done_ids: Vec<String> = Vec::new();
    // 采成功的内容所属任务集合:意向分析按任务跑(任务级幂等)
    let mut succeeded_task_ids: HashSet<String> = HashSet::new();
    // 本批开过采集窗口的(平台, 账号),结束后统一归还——只关自己开过的
    let mut opened_windows: Vec<(String, String)> = Vec::new();
    let mut processed = 0usize;

    for (platform, group) in by_platform {
        let cfg = {
            lock_config(&state)
                .ok()
                .and_then(|c| c.platform(&platform).ok().cloned())
        };
        let adapter = state
            .registry
            .get(&platform)
            .ok()
            .filter(|ad| ad.supports(&TaskKind::Comments));
        let (cfg, adapter) = match (cfg, adapter) {
            (Some(c), Some(ad)) if !c.collect.detail_url_template.is_empty() => (c, ad),
            _ => {
                summary.skipped += group.len();
                summary.messages.push(format!(
                    "平台 {platform} 不支持评论补采(平台未启用 / 适配器不支持 / 未配置详情模板),已跳过 {} 条",
                    group.len()
                ));
                continue;
            }
        };
        let account_id = match state.cookies.acquire(&platform).await {
            Ok(acc) => acc.id,
            Err(_) => {
                summary.skipped += group.len();
                summary.messages.push(format!(
                    "平台 {} 无可用账号,已跳过 {} 条",
                    cfg.name,
                    group.len()
                ));
                continue;
            }
        };
        // 账号采集互斥(与采集主链路同一把锁):30s 等不到就跳过本平台,不阻塞整批
        let account_lock =
            account_collect_lock(&state.collect_locks, &account_lock_key(&platform, &account_id));
        let _guard = match tokio::time::timeout(
            std::time::Duration::from_secs(30),
            account_lock.lock(),
        )
        .await
        {
            Ok(guard) => guard,
            Err(_) => {
                summary.skipped += group.len();
                summary.messages.push(format!(
                    "平台 {} 账号窗口被采集任务占用,已跳过 {} 条",
                    cfg.name,
                    group.len()
                ));
                continue;
            }
        };
        // 持锁期间该账号无采集在跑,残留的「手动关窗」标记只会是历史遗留,重置后再开始
        // (否则首轮检查就误判终止);补采途中的关窗由逐条检查捕获
        bridge.reset_collect_window_closed(&platform, &account_id, None);
        opened_windows.push((platform.clone(), account_id.clone()));

        let total = group.len();
        let mut zero_comment = 0usize;
        for (idx, row) in group.iter().enumerate() {
            // 评论数为 0 的内容直接跳过(接口统计已知无评论,采了也是空跑);
            // 数量未知(None)的仍尝试,与任务评论阶段同口径
            if row.comment_count == Some(0) {
                zero_comment += 1;
                summary.skipped += 1;
                continue;
            }
            // 补采途中手动关窗 = 终止本平台:剩余内容记跳过
            if bridge.is_collect_window_closed(&platform, &account_id, None) {
                let remaining = total - idx;
                summary.skipped += remaining;
                summary.messages.push(format!(
                    "采集窗口已被手动关闭 · 终止平台 {} 补采(剩余 {remaining} 条跳过)",
                    cfg.name
                ));
                break;
            }
            // 串行限速:首个不等,之后每个之间随机间隔降频
            if processed > 0 {
                tokio::time::sleep(random_comment_video_interval()).await;
            }
            processed += 1;
            summary.attempted += 1;

            let c = content_from_model(row);
            // 详情页导航的第二参数({token} 占位)口径与任务评论阶段一致:
            // 抖音走「主页模态」用作者 sec_uid;其他平台(小红书)用内容自带 xsec_token
            let token = if cfg.id == "douyin" {
                c.author.uid.clone()
            } else {
                c.extra
                    .get("xsec_token")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string()
            };
            let title = log_content_title(&c);
            crate::webview::hud_log(
                &app,
                &platform,
                &account_id,
                None,
                "info",
                &format!("💬 [{}/{}] 补采评论「{title}」", idx + 1, total),
            );
            match bridge
                .collect_comments(
                    &app,
                    CommentCollectRequest {
                        account_id: &account_id,
                        content_id: &row.content_id,
                        title: &title,
                        xsec_token: &token,
                        platform_cfg: &cfg,
                        task_id: None,
                        limit: comment_limit,
                        adapter: adapter.clone(),
                        keyword: &row.keyword,
                        video_index: idx + 1,
                        video_total: total,
                    },
                )
                .await
            {
                Ok(responses) if !responses.is_empty() => {
                    let ctx = FetchContext {
                        keyword: row.content_id.clone(),
                        responses,
                    };
                    match adapter.parse(&TaskKind::Comments, &ctx).await {
                        Ok(mut output) => {
                            output.comments =
                                filter_comments(output.comments, cutoff, comment_limit);
                            summary.comments += output.comments.len();
                            // 逐条评论 HUD 日志:按视频从 1 编号 + 带「第几/共几个视频」,
                            // 与任务评论阶段的日志格式一致
                            let mut vseq: i64 = 0;
                            for cm in &output.comments {
                                vseq += 1;
                                let text = truncate_chars(&cm.text, 60);
                                let likes = cm.like_count.unwrap_or(0);
                                crate::webview::hud_log(
                                    &app,
                                    &platform,
                                    &account_id,
                                    None,
                                    "info",
                                    &format!(
                                        "[视频{}/{total} 评论{vseq}] {text} | 点赞:{likes}",
                                        idx + 1
                                    ),
                                );
                            }
                            // 评论解析不产出内容,清空防误入库(keyword 口径是 content_id,混入会污染)
                            output.contents = Vec::new();
                            persist_collected(
                                &state.db,
                                &row.task_id,
                                &row.owner,
                                &row.content_id,
                                output,
                                &mut seen_contents,
                                &mut seen_comments,
                            )
                            .await;
                            comment_done_ids.push(row.id.clone());
                            succeeded_task_ids.insert(row.task_id.clone());
                            summary.succeeded += 1;
                        }
                        Err(e) => {
                            summary.failed += 1;
                            summary.messages.push(format!("「{title}」评论解析失败: {e}"));
                        }
                    }
                }
                Ok(_) => {
                    summary.failed += 1;
                    summary.messages.push(format!("「{title}」未采到评论响应"));
                }
                Err(e) => {
                    summary.failed += 1;
                    summary.messages.push(format!("「{title}」评论采集失败: {e}"));
                    crate::webview::hud_log(
                        &app,
                        &platform,
                        &account_id,
                        None,
                        "warn",
                        &format!("⚠️ 「{title}」评论补采失败: {e}"),
                    );
                }
            }
        }
        if zero_comment > 0 {
            summary.messages.push(format!(
                "平台 {} 跳过 {zero_comment} 条评论数为 0 的内容(接口统计无评论,不空跑)",
                cfg.name
            ));
        }
    }

    // 只标确有评论响应且解析成功的内容(与任务评论阶段同口径);失败/零响应留 false 供下次重采。
    // 分批:is_in 超 SQLite 变量上限(999)会整批失败
    for chunk in comment_done_ids.chunks(500) {
        if let Err(e) = content_entity::Entity::update_many()
            .col_expr(content_entity::Column::CommentCollected, Expr::value(true))
            .filter(content_entity::Column::Id.is_in(chunk.iter().cloned()))
            .exec(&state.db)
            .await
        {
            tracing::warn!("补采评论:标记 comment_collected 失败: {e}");
        }
    }

    // 任务累计评论数回写:任务列表「采集结果」的评论总数直接读 tasks.comment_count
    // (采集时增量维护),补采入库的评论不经过该链路——按任务实算一次,避免总数少计
    {
        use sea_orm::PaginatorTrait;
        use veltrix_core::db::entity::{
            comment as comment_entity, task as task_entity,
        };
        for task_id in &succeeded_task_ids {
            let count = comment_entity::Entity::find()
                .filter(comment_entity::Column::TaskId.eq(task_id))
                .count(&state.db)
                .await;
            match count {
                Ok(n) => {
                    if let Err(e) = task_entity::Entity::update_many()
                        .col_expr(task_entity::Column::CommentCount, Expr::value(n as i64))
                        .filter(task_entity::Column::Id.eq(task_id))
                        .exec(&state.db)
                        .await
                    {
                        tracing::warn!("补采评论:回写任务评论总数失败(task {task_id}): {e}");
                    }
                }
                Err(e) => {
                    tracing::warn!("补采评论:统计任务评论数失败(task {task_id}): {e}");
                }
            }
        }
    }

    // 意向分析(可选):按内容所属任务逐个跑,analyze_comments_intent 内部按
    // intent_level IS NULL 幂等筛选,只分析本次新采 + 历史未分析的评论
    let intent_ready = analyze_intent
        && !intent_cfg.api_url.trim().is_empty()
        && !intent_cfg.model.trim().is_empty();
    if analyze_intent && !intent_ready {
        // 用户选了「是」但未配置,静默跳过会误以为已分析,必须留痕
        summary.messages.push(
            "意向分析未配置(系统设置 → 意向分析),已跳过该步".to_string(),
        );
    }
    if intent_ready {
        for task_id in &succeeded_task_ids {
            let analyzed = analyze_comments_intent(&app, &state.db, task_id, &intent_cfg).await;
            // 确有评论被分析出结果才标记(与采集主链路同口径)
            if analyzed > 0 {
                if let Err(e) = content_entity::Entity::update_many()
                    .col_expr(content_entity::Column::IntentAnalyzed, Expr::value(true))
                    .filter(content_entity::Column::TaskId.eq(task_id))
                    .filter(content_entity::Column::CommentCollected.eq(true))
                    .exec(&state.db)
                    .await
                {
                    tracing::warn!("补采评论:标记 intent_analyzed 失败: {e}");
                }
            }
        }
    }

    // 归还补采期间打开的采集窗口(正常结束或中断都执行):关窗会触发 Destroyed 把
    // 「被手动关闭」标记置位,随即重置,避免自己关窗留下的标记污染下次补采(与画像补采同口径)
    for (platform, account_id) in &opened_windows {
        bridge.close_collect_window(platform, account_id, None);
        bridge.reset_collect_window_closed(platform, account_id, None);
    }

    Ok(summary)
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

/// 检测 ffmpeg 是否可用:依次探测 配置路径 → 安装包内置 → 系统 PATH 的 `ffmpeg`,
/// 任一可用即视为已安装(配置路径失效/内置被杀软清理时仍有 PATH 兜底)。
/// 探测是阻塞的进程调用,挪到阻塞线程池,避免占用异步运行时工作线程。
#[tauri::command]
pub async fn check_ffmpeg(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<FfmpegStatus> {
    // clone 出路径再 spawn_blocking,避免把配置锁 guard 跨 await 持有
    let ffmpeg_path = { lock_config(&state)?.media.ffmpeg_path.clone() };
    let version = tauri::async_runtime::spawn_blocking(move || {
        crate::media::probe_ffmpeg(ffmpeg_path.as_deref())
            .or_else(|| {
                let bundled = crate::media::bundled_ffmpeg_path(&app);
                crate::media::probe_ffmpeg(bundled.as_deref().and_then(|p| p.to_str()))
            })
            .or_else(|| crate::media::probe_ffmpeg(None))
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

    // 采集去重台账:查出本批哪些 (platform, content_id) 已登记。上游(consumer / 兜底解析 /
    // 定向解析)已按「本任务已采 ∪ 台账(近 90 天)」整体跳过已采内容,能走到这里的基本都是
    // 新内容;此处的 recorded 仅用于首次登记,避免重复写台账行。
    let ledger_ids: Vec<String> = candidates
        .iter()
        .map(|(_, c)| ledger_entity::ledger_key(&c.platform, &c.content_id))
        .collect();
    let recorded = load_recorded_ledger_ids(db, &ledger_ids).await;

    let now = Utc::now().timestamp();
    let mut to_record: Vec<ledger_entity::ActiveModel> = Vec::new();
    for (_, c) in &candidates {
        let key = ledger_entity::ledger_key(&c.platform, &c.content_id);
        // 首次见到的内容登记进台账(已登记的不重复写)
        if !recorded.contains(&key) {
            to_record.push(ledger_entity::ActiveModel {
                id: Set(key),
                platform: Set(c.platform.clone()),
                content_id: Set(c.content_id.clone()),
                created_at: Set(now),
            });
        }
    }
    // 模型按需构建:快路径只建一次喂整批 upsert;整批失败降级逐条时再建一次,
    // 不再每批预克隆一整份(响应体大、批次多时克隆是纯浪费)
    let build_models = || {
        candidates
            .iter()
            .map(|(id, c)| content_to_active(id.clone(), c, task_id, keyword, owner))
            .collect::<Vec<_>>()
    };
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
    //
    // 台账登记改为同生命周期:内容落库成功才登记,落库失败的不登记,
    // 避免未来重采时因台账已标记而跳过(此前是"先登记再落库,失败也登记"→永久丢失)。
    let mut upserted_ledger: Vec<ledger_entity::ActiveModel> = Vec::new();
    if let Err(e) = content_entity::Entity::insert_many(build_models())
        .on_conflict(on_conflict.clone())
        .exec(db)
        .await
    {
        tracing::warn!("批量落库采集内容失败,降级逐条保存: {e}");
        let (mut ok, mut lost) = (0usize, 0usize);
        // 建立台账 key → 台账行映射,逐条成功时按 key 取出对应台账条目登记
        let ledger_by_key: std::collections::HashMap<String, ledger_entity::ActiveModel> =
            to_record
                .into_iter()
                .map(|am| {
                    let k = am.id.as_ref().clone();
                    (k, am)
                })
                .collect();
        for (i, am) in build_models().into_iter().enumerate() {
            // 行主键 id(非平台 content_id),日志字段据此命名
            let row_id = am.id.as_ref().clone();
            match content_entity::Entity::insert(am)
                .on_conflict(on_conflict.clone())
                .exec(db)
                .await
            {
                Ok(_) => {
                    ok += 1;
                    // 尝试找到对应的台账条目并登记(不是每条内容都有新台账条目)
                    let ledger_key = candidates
                        .get(i)
                        .map(|(_, c)| ledger_entity::ledger_key(&c.platform, &c.content_id));
                    if let Some(entry) = ledger_key
                    .as_deref()
                    .and_then(|k| ledger_by_key.get(k))
                {
                    upserted_ledger.push(entry.clone());
                }
                }
                Err(e2) => {
                    lost += 1;
                    tracing::warn!(
                        row_id = %row_id,
                        "逐条落库内容失败(跳过该条,台账不登记可被未来重采补回): {e2}"
                    );
                }
            }
        }
        tracing::warn!("内容降级保存完成 · 成功 {ok} 条 · 丢弃 {lost} 条");
    } else {
        // 批量落库成功,该批次所有内容对应的台账条目均可登记
        upserted_ledger = to_record;
    }

    // 登记采集去重台账(仅登记实际操作成功的条目):
    // 主键冲突忽略;台账写失败不影响采集主流程。
    if !upserted_ledger.is_empty() {
        let on_conflict_ledger =
            sea_orm::sea_query::OnConflict::column(ledger_entity::Column::Id)
                .do_nothing()
                .to_owned();
        match ledger_entity::Entity::insert_many(upserted_ledger)
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

/// 评论正文是否无文本价值:空 / 纯空白 / 纯表情。
/// 判定:剔除「[表情名]」占位符(抖音自定义表情在 text 里的形态)后,
/// 剩余字符不含任何字母 / 数字(含 CJK)即视为无文本——纯 emoji、纯标点都落在此列。
fn is_textless_comment(text: &str) -> bool {
    let mut in_placeholder = false;
    for c in text.chars() {
        match c {
            '[' => in_placeholder = true,
            ']' if in_placeholder => in_placeholder = false,
            _ if !in_placeholder && c.is_alphanumeric() => return false,
            _ => {}
        }
    }
    true
}

/// 评论 upsert:按 comment_id 去重,on_conflict 更新点赞/回复数。
/// 空文本 / 纯空白 / 纯表情评论无价值,直接不落库。
async fn persist_comments(
    db: &DatabaseConnection,
    task_id: &str,
    owner: &str,
    comments: &[Comment],
    seen: &mut HashSet<String>,
) {
    use veltrix_core::db::entity::comment as comment_entity;

    let candidates: Vec<(String, &Comment)> = comments
        .iter()
        .filter(|c| !is_textless_comment(&c.text))
        .filter_map(|c| {
            let id = format!("{task_id}-{}-{}", c.platform, c.comment_id);
            if !seen.insert(id.clone()) {
                return None;
            }
            Some((id, c))
        })
        .collect();
    if candidates.is_empty() {
        return;
    }
    // 模型按需构建(同 persist_contents):快路径建一次,整批失败降级逐条时再建,不预克隆
    let build_models = || {
        candidates
            .iter()
            .map(|(id, c)| comment_to_active(id.clone(), c, task_id, owner))
            .collect::<Vec<_>>()
    };
    // 评论同样判重 upsert:已存在时刷新点赞 / 回复数
    let on_conflict = sea_orm::sea_query::OnConflict::column(comment_entity::Column::Id)
        .update_columns([
            comment_entity::Column::LikeCount,
            comment_entity::Column::ReplyCount,
            // 同 content:不刷新 collected_at,采集明细只统计本次新增、排除重复采到的已有评论
        ])
        .to_owned();
    // 同内容:整批失败降级逐条,保住其余已采评论
    if let Err(e) = comment_entity::Entity::insert_many(build_models())
        .on_conflict(on_conflict.clone())
        .exec(db)
        .await
    {
        tracing::warn!("批量落库采集评论失败,降级逐条保存: {e}");
        let (mut ok, mut lost) = (0usize, 0usize);
        for am in build_models() {
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
    use sea_orm::{ColumnTrait, QueryFilter};
    use veltrix_core::db::entity::author as author_entity;

    const AUTHOR_REFRESH_SECS: i64 = 7 * 24 * 3600;
    let now = Utc::now().timestamp();
    // 本批内按作者去重后的 (行主键, 作者引用)
    let mut seen_authors: HashSet<String> = HashSet::new();
    let candidates: Vec<(String, &Author)> = contents
        .iter()
        .filter(|c| !c.author.uid.is_empty())
        .filter_map(|c| {
            let aid = format!("{owner}-{}-{}", c.author.platform, c.author.uid);
            seen_authors.insert(aid.clone()).then_some((aid, &c.author))
        })
        .collect();
    if candidates.is_empty() {
        return;
    }
    // 一次 IN 批查替代逐作者 find_by_id(N+1);查询失败按「都不存在」处理,
    // 走下方 on_conflict do_nothing 的批量插入,已有档案不会被误覆盖
    let mut existing: std::collections::HashMap<String, author_entity::Model> =
        author_entity::Entity::find()
            .filter(author_entity::Column::Id.is_in(candidates.iter().map(|(id, _)| id.clone())))
            .all(db)
            .await
            .unwrap_or_else(|e| {
                tracing::warn!("批查作者档案失败: {e}");
                Vec::new()
            })
            .into_iter()
            .map(|m| (m.id.clone(), m))
            .collect();

    let mut to_insert: Vec<author_entity::ActiveModel> = Vec::new();
    for (aid, a) in candidates {
        match existing.remove(&aid) {
            None => to_insert.push(author_to_active(&aid, a, owner, now)),
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
    if !to_insert.is_empty() {
        // 主键冲突忽略:同 owner 的并发任务可能同时给同一作者建档
        let on_conflict = sea_orm::sea_query::OnConflict::column(author_entity::Column::Id)
            .do_nothing()
            .to_owned();
        match author_entity::Entity::insert_many(to_insert)
            .on_conflict(on_conflict)
            .exec(db)
            .await
        {
            // RecordNotInserted = 全部冲突被忽略,非错误
            Ok(_) | Err(sea_orm::DbErr::RecordNotInserted) => {}
            Err(e) => tracing::warn!("批量建作者档案失败: {e}"),
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

/// 任务字段定向更新 + 进度推送。用 update_many 只写目标列——此前的
/// find_by_id → into_active_model → update 会把所有列标 Set 整行覆盖,与暂停/取消等
/// 并发写者交错时会用陈旧值冲掉对方写入(典型:进度回写把 cancelled 状态「复活」,
/// 配合 next_retry_at 还会被调度器重新拉起)。写后回读整行仅用于推送,不写回。
async fn update_task_fields(
    app: &AppHandle,
    db: &DatabaseConnection,
    task_id: &str,
    warn_msg: &str,
    build: impl FnOnce(
        sea_orm::UpdateMany<veltrix_core::db::entity::task::Entity>,
    ) -> sea_orm::UpdateMany<veltrix_core::db::entity::task::Entity>,
) {
    use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
    use veltrix_core::db::entity::task as task_entity;
    let res = build(task_entity::Entity::update_many())
        .filter(task_entity::Column::Id.eq(task_id.to_string()))
        .exec(db)
        .await;
    match res {
        Ok(_) => {
            // 回读整行推送进度;读取失败仅丢一次推送,下一条进度会补上
            if let Ok(Some(m)) =
                task_entity::Entity::find_by_id(task_id.to_string()).one(db).await
            {
                emit_task_progress(app, m);
            }
        }
        Err(e) => tracing::warn!("{warn_msg}: {e}"),
    }
}

/// 标记任务完成(status=completed, progress=100, finished_at)。
async fn write_task_done(app: &AppHandle, db: &DatabaseConnection, task_id: &str) {
    use sea_orm::sea_query::Expr;
    use veltrix_core::db::entity::task::Column as C;
    let now = Utc::now().timestamp();
    update_task_fields(app, db, task_id, "标记任务完成失败", move |u| {
        u.col_expr(C::Status, Expr::value("completed"))
            .col_expr(C::Progress, Expr::value(100))
            .col_expr(C::FinishedAt, Expr::value(Some(now)))
            // 成功 = 重试序列结束:清零重试计数与排期
            .col_expr(C::RetryCount, Expr::value(0))
            .col_expr(C::NextRetryAt, Expr::value(None::<i64>))
            .col_expr(C::UpdatedAt, Expr::value(now))
    })
    .await;
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
    use sea_orm::sea_query::Expr;
    use veltrix_core::db::entity::task::Column as C;
    let now = Utc::now().timestamp();
    update_task_fields(app, db, task_id, "标记任务评论采集态失败", move |u| {
        u.col_expr(C::Status, Expr::value("collecting_comments"))
            .col_expr(C::CommentVideoTotal, Expr::value(video_total))
            .col_expr(C::CommentVideoDone, Expr::value(0))
            .col_expr(C::UpdatedAt, Expr::value(now))
    })
    .await;
}

/// 回写评论采集进度(已采视频数 comment_video_done + 累计评论数 comment_count)。失败仅告警。
async fn write_task_comment_progress(
    app: &AppHandle,
    db: &DatabaseConnection,
    task_id: &str,
    video_done: i32,
    comment_count: i64,
    force: bool,
) {
    // 节流:高频调用合并写;force(循环末)必写,保证最终计数不滞后
    if !progress_write_allowed(task_id, force) {
        return;
    }
    use sea_orm::sea_query::Expr;
    use veltrix_core::db::entity::task::Column as C;
    update_task_fields(app, db, task_id, "回写评论采集进度失败", move |u| {
        u.col_expr(C::CommentVideoDone, Expr::value(video_done))
            .col_expr(C::CommentCount, Expr::value(comment_count))
            .col_expr(C::UpdatedAt, Expr::value(Utc::now().timestamp()))
    })
    .await;
}

/// 标记任务进入意向分析态(status=analyzing_comments)。不写 finished_at。
async fn write_task_analyzing(app: &AppHandle, db: &DatabaseConnection, task_id: &str) {
    use sea_orm::sea_query::Expr;
    use veltrix_core::db::entity::task::Column as C;
    update_task_fields(app, db, task_id, "标记任务意向分析态失败", move |u| {
        u.col_expr(C::Status, Expr::value("analyzing_comments"))
            .col_expr(C::UpdatedAt, Expr::value(Utc::now().timestamp()))
    })
    .await;
}

/// 对任务评论分批做意向分析并写回 comment.intent_*。读 intent 配置引用的厂商 / 提示词,
/// 按 batch_size 分批调 LLM。任一环节失败仅告警,不影响任务终态。
async fn analyze_comments_intent(
    app: &AppHandle,
    db: &DatabaseConnection,
    task_id: &str,
    intent_cfg: &veltrix_core::config::CommentIntentConfig,
) -> usize {
    use veltrix_core::db::entity::{
        comment as comment_entity,
    };

    // 未配置 API Key 直接跳过——否则空 Bearer 会让每个批次 401 全失败、0 条评论被标注,
    // 还无意义地刷一串「批次失败」(早返回守卫在重构中被误删,这里补回)。
    let api_key = get_secret(db, "intent_api_key").await;
    if api_key.trim().is_empty() {
        tracing::warn!("意向分析未配置 API Key,跳过本任务意向分析(task {task_id})");
        return 0;
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
            return 0;
        }
    };
    if rows.is_empty() {
        emit_collect_log(app, task_id, "info", "无待分析评论");
        return 0;
    }

    // 钳制上限:回写 SQL 每行占 5 个变量(SQLite),用户配置的 batch_size 过大
    // 会撑爆 SQLite 变量上限(999)导致整批 UPDATE 失败;100 行 × 5 = 500,留有富余
    let batch_size = if intent_cfg.batch_size > 0 {
        (intent_cfg.batch_size as usize).min(100)
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
    // analyzed 只累计「确有判定结果」的评论数(失败批次 verdicts 为空,不计入),
    // 调用方据此决定能否把内容标为已分析(0 产出标记会掩盖「实际没分析」)
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
        let updates_count = updates.len();
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
            // 写库成功才计入 analyzed:失败不计,否则内容会被误标 intent_analyzed
            // 而 intent_level 仍为空,永久停在「已标记未分析」(重跑也不再补标)
            match db.execute(Statement::from_sql_and_values(backend, sql, params)).await {
                Ok(_) => analyzed += updates_count,
                Err(e) => tracing::warn!("批量回写意向失败(影响 {} 条): {e}", updates.len()),
            }
        }
    }
    emit_collect_log(app, task_id, "info", format!("意向分析进度 {analyzed}/{total}"));
    emit_collect_log(
        app,
        task_id,
        "info",
        format!("意向分析完成 · 已处理 {analyzed} 条"),
    );
    analyzed
}

/// 标记任务进入素材下载态(status=downloading_media, progress=100, 记录素材总数,清零已处理数)。
/// 采集主体已结束但素材未下完时调用,调度页据此显示「素材下载中 done/total」;不写 finished_at。
async fn write_task_downloading(
    app: &AppHandle,
    db: &DatabaseConnection,
    task_id: &str,
    media_total: i32,
) {
    use sea_orm::sea_query::Expr;
    use veltrix_core::db::entity::task::Column as C;
    let now = Utc::now().timestamp();
    update_task_fields(app, db, task_id, "标记任务素材下载态失败", move |u| {
        u.col_expr(C::Status, Expr::value("downloading_media"))
            .col_expr(C::Progress, Expr::value(100))
            .col_expr(C::MediaTotal, Expr::value(media_total))
            .col_expr(C::MediaDone, Expr::value(0))
            .col_expr(C::UpdatedAt, Expr::value(now))
    })
    .await;
}

/// 回写素材下载进度(仅更新 media_done)。失败仅告警,不中断下载循环。
async fn write_task_media_done(
    app: &AppHandle,
    db: &DatabaseConnection,
    task_id: &str,
    media_done: i32,
) {
    use sea_orm::sea_query::Expr;
    use veltrix_core::db::entity::task::Column as C;
    update_task_fields(app, db, task_id, "回写素材进度失败", move |u| {
        u.col_expr(C::MediaDone, Expr::value(media_done))
            .col_expr(C::UpdatedAt, Expr::value(Utc::now().timestamp()))
    })
    .await;
}

/// 标记任务取消(status=cancelled, finished_at, error_message)。
/// 用户主动终止(如采集途中手动关闭采集窗口)时调用;cancelled 为终态,监听任务据此停止自动轮转。
async fn write_task_cancelled(app: &AppHandle, db: &DatabaseConnection, task_id: &str, message: &str) {
    use sea_orm::sea_query::Expr;
    use veltrix_core::db::entity::task::Column as C;
    let now = Utc::now().timestamp();
    let message = message.to_string();
    update_task_fields(app, db, task_id, "标记任务取消状态失败", move |u| {
        u.col_expr(C::Status, Expr::value("cancelled"))
            .col_expr(C::FinishedAt, Expr::value(Some(now)))
            .col_expr(C::ErrorMessage, Expr::value(Some(message)))
            // 取消是用户主动终止:清掉可能的自动重试排期,调度器不会再拉起
            .col_expr(C::NextRetryAt, Expr::value(None::<i64>))
            .col_expr(C::UpdatedAt, Expr::value(now))
    })
    .await;
}

/// 标记任务失败(status=failed, finished_at, error_message)。
/// 采集零产出且过程出错时调用,避免失败任务被误标「已完成」。
/// 若任务开了自动重试(max_retries>0)且未达上限,按 1min / 5min / 15min 指数退避排期重试,
/// 由调度器到点自动重跑;重试耗尽才落终态失败。
async fn write_task_failed(app: &AppHandle, db: &DatabaseConnection, task_id: &str, message: &str) {
    use sea_orm::sea_query::Expr;
    use sea_orm::EntityTrait;
    use veltrix_core::db::entity::task as task_entity;
    use veltrix_core::db::entity::task::Column as C;
    // 需要当前重试计数计算退避排期(只读;写回走定向列更新,不整行覆盖)
    let m = match task_entity::Entity::find_by_id(task_id.to_string()).one(db).await {
        Ok(Some(m)) => m,
        Ok(None) => return,
        Err(e) => {
            tracing::warn!("标记任务失败状态失败(查询任务): {e}");
            return;
        }
    };
    let now = Utc::now().timestamp();
    let max_retries = m.max_retries.max(0);
    let retry_count = m.retry_count;
    // 自动重试:仅当任务配置了上限、且当前失败序列未耗尽。排期后状态仍为 failed,
    // 调度器扫描 next_retry_at 到点翻转 running;重试次数累加供 UI 与日志展示。
    let will_retry = max_retries > 0 && retry_count < max_retries;
    let next_retry = retry_count + 1;
    let delay_secs = retry_backoff_secs(next_retry);
    if will_retry {
        emit_collect_log(
            app,
            task_id,
            "warn",
            format!(
                "⚠️ 任务失败,{delay_secs} 秒后自动重试(第 {next_retry}/{max_retries} 次)· {message}"
            ),
        );
    } else if max_retries > 0 {
        emit_collect_log(
            app,
            task_id,
            "error",
            format!(
                "❌ 自动重试已耗尽({}/{max_retries}),任务终止 · {message}",
                retry_count
            ),
        );
    }
    let message = message.to_string();
    update_task_fields(app, db, task_id, "标记任务失败状态失败", move |u| {
        let u = u
            .col_expr(C::Status, Expr::value("failed"))
            .col_expr(C::FinishedAt, Expr::value(Some(now)))
            .col_expr(C::ErrorMessage, Expr::value(Some(message)))
            .col_expr(C::UpdatedAt, Expr::value(now));
        if will_retry {
            u.col_expr(C::RetryCount, Expr::value(next_retry))
                .col_expr(C::NextRetryAt, Expr::value(Some(now + delay_secs)))
        } else {
            u.col_expr(C::NextRetryAt, Expr::value(None::<i64>))
        }
    })
    .await;
}

/// 自动重试退避(秒):第 1 次重试 1 分钟、第 2 次 5 分钟、第 3 次起 15 分钟封顶。
fn retry_backoff_secs(retry_no: i32) -> i64 {
    const BASE: i64 = 60;
    const CAP: i64 = 900;
    // saturating_*:retry_no 很大(≥26)时 5^n 与乘法都会溢出,release 下溢出会
    // wrap 成负数、把重试排到过去;饱和到 i64::MAX 再被 CAP 截断即可。
    // .max(0) 不可省:signed saturating_sub 不饱和到 0(0-1 = -1),负数 as u32 会变巨大
    let factor = 5i64.saturating_pow(retry_no.saturating_sub(1).max(0) as u32);
    BASE.saturating_mul(factor).min(CAP)
}

#[cfg(test)]
mod tests {
    //! 采集模块关键纯逻辑与台账/素材过滤查询的回归测试(三轮 bug 修复后首次引入)。

    use super::*;
    use sea_orm::ConnectionTrait;
    use veltrix_core::db::entity::{
        collect_record as ledger_entity, content as content_entity, task as task_entity,
    };

    // ---------- truncate_chars ----------

    #[test]
    fn truncate_chars_short_returns_trimmed() {
        assert_eq!(truncate_chars("hello", 10), "hello", "短串应原样返回");
        assert_eq!(truncate_chars("  hello  ", 10), "hello", "首尾空白应被 trim");
    }

    #[test]
    fn truncate_chars_over_max_appends_ellipsis() {
        assert_eq!(truncate_chars("abcdefghij", 5), "abcde…", "超长应截断并加省略号");
        assert_eq!(truncate_chars("abcde", 5), "abcde", "恰好 max 个字符不应加省略号");
    }

    #[test]
    fn truncate_chars_multibyte_no_panic() {
        assert_eq!(truncate_chars("你好世界啊", 3), "你好世…", "中文应按字符截断");
        assert_eq!(truncate_chars("😀😀😀😀", 2), "😀😀…", "emoji 截断不应 panic");
    }

    // ---------- comment_time_cutoff ----------

    #[test]
    fn comment_time_cutoff_known_ranges() {
        let now = Utc::now().timestamp();
        for (range, days) in [("3d", 3i64), ("7d", 7), ("14d", 14)] {
            let cutoff = comment_time_cutoff(range).expect("已知范围应返回 Some");
            let expect = now - days * 86400;
            assert!(
                (cutoff - expect).abs() <= 5,
                "{range} 的下限应在 now-{days}d 前后几秒内"
            );
        }
    }

    #[test]
    fn comment_time_cutoff_any_or_unknown_returns_none() {
        assert_eq!(comment_time_cutoff("any"), None, "any 不应按时间过滤");
        assert_eq!(comment_time_cutoff(""), None, "空串不应按时间过滤");
        assert_eq!(comment_time_cutoff("30d"), None, "未知范围不应按时间过滤");
    }

    // ---------- filter_comments ----------

    fn comment(id: &str, created_at: Option<i64>) -> Comment {
        Comment {
            comment_id: id.to_string(),
            created_at,
            ..Default::default()
        }
    }

    fn comment_ids(comments: &[Comment]) -> Vec<&str> {
        comments.iter().map(|c| c.comment_id.as_str()).collect()
    }

    #[test]
    fn filter_comments_cutoff_keeps_recent_and_timeless() {
        let now = Utc::now().timestamp();
        let comments = vec![
            comment("old", Some(now - 10 * 86400)),
            comment("new", Some(now - 100)),
            comment("no-time", None),
        ];
        let out = filter_comments(comments, Some(now - 86400), 0);
        assert_eq!(
            comment_ids(&out),
            ["new", "no-time"],
            "过期评论应被过滤,created_at 缺失的评论应保留"
        );
    }

    #[test]
    fn filter_comments_no_cutoff_keeps_all() {
        let comments = vec![comment("a", Some(1)), comment("b", None)];
        let out = filter_comments(comments, None, 0);
        assert_eq!(out.len(), 2, "cutoff=None 不应做时间过滤");
    }

    #[test]
    fn filter_comments_limit_truncates_but_zero_means_unlimited() {
        let comments = vec![comment("a", None), comment("b", None), comment("c", None)];
        let out = filter_comments(comments.clone(), None, 2);
        assert_eq!(comment_ids(&out), ["a", "b"], "limit 应从头截断保留前 N 条");
        let out = filter_comments(comments, None, 0);
        assert_eq!(out.len(), 3, "limit=0 表示不截断");
    }

    // ---------- retry_backoff_secs ----------

    #[test]
    fn retry_backoff_secs_ladder_and_cap() {
        assert_eq!(retry_backoff_secs(1), 60, "第 1 次重试应为 60s");
        assert_eq!(retry_backoff_secs(2), 300, "第 2 次重试应为 300s");
        assert_eq!(retry_backoff_secs(3), 900, "第 3 次起应封顶 900s");
        assert_eq!(retry_backoff_secs(10), 900, "更大次数仍应封顶 900s");
        assert_eq!(retry_backoff_secs(0), 60, "非正次数兜底按第 1 次");
        assert_eq!(retry_backoff_secs(26), 900, "溢出边界(5^25×60 超 i64)应饱和封顶,不得 panic/wrap 成负数");
        assert_eq!(retry_backoff_secs(i32::MAX), 900, "极大次数仍封顶 900s");
    }

    // ---------- is_profile_url ----------

    #[test]
    fn is_profile_url_douyin_modal_id_is_content() {
        assert!(is_profile_url("douyin", "https://www.douyin.com/user/MS4wLjABxxxx"));
        assert!(
            !is_profile_url(
                "douyin",
                "https://www.douyin.com/user/MS4wLjABxxxx?modal_id=7300123456"
            ),
            "带 modal_id 的主页模态是内容链接,应判 false"
        );
    }

    #[test]
    fn is_profile_url_tiktok_at_user() {
        assert!(is_profile_url("tiktok", "https://www.tiktok.com/@someuser"));
        assert!(
            !is_profile_url("tiktok", "https://www.tiktok.com/@someuser/video/7300123456"),
            "@user/video/xxx 是内容链接,应判 false"
        );
    }

    #[test]
    fn is_profile_url_other_platforms() {
        assert!(is_profile_url("xhs", "https://www.xiaohongshu.com/user/profile/abc"));
        assert!(is_profile_url("kuaishou", "https://www.kuaishou.com/profile/abc"));
        assert!(is_profile_url("bilibili", "https://space.bilibili.com/123"));
        assert!(is_profile_url("youtube", "https://www.youtube.com/@chan"));
        assert!(is_profile_url("youtube", "https://www.youtube.com/channel/UCxxx"));
        assert!(
            !is_profile_url("unknown", "https://www.douyin.com/user/xxx"),
            "未知平台应判 false"
        );
    }

    // ---------- platform_home_url / content_kind_label ----------

    #[test]
    fn platform_home_url_covers_known_platforms() {
        assert_eq!(platform_home_url("tiktok"), Some("https://www.tiktok.com/"));
        assert_eq!(platform_home_url("youtube"), Some("https://www.youtube.com/"));
        assert_eq!(platform_home_url("douyin"), Some("https://www.douyin.com/"));
        assert_eq!(platform_home_url("kuaishou"), Some("https://www.kuaishou.com/"));
        assert_eq!(platform_home_url("xhs"), Some("https://www.xiaohongshu.com/"));
        assert_eq!(platform_home_url("bilibili"), Some("https://www.bilibili.com/"));
        assert_eq!(platform_home_url("weibo"), None, "未登记平台应返回 None");
    }

    #[test]
    fn content_kind_label_covers_all_variants() {
        assert_eq!(content_kind_label(&ContentKind::Video), "video");
        assert_eq!(content_kind_label(&ContentKind::Image), "image");
        assert_eq!(content_kind_label(&ContentKind::Article), "article");
        assert_eq!(content_kind_label(&ContentKind::Unknown), "unknown");
    }

    // ---------- is_media_ok ----------

    fn outcome(ok: bool, audio_extracted: Option<bool>) -> crate::media::MediaOutcome {
        crate::media::MediaOutcome {
            ok,
            audio_extracted,
            error: None,
            cover_path: None,
            avatar_path: None,
            audio_path: None,
            video_downloaded: None,
            image_total: None,
            image_done: None,
        }
    }

    #[test]
    fn is_media_ok_requires_main_ok_and_audio_not_failed() {
        assert!(is_media_ok(&outcome(true, Some(true))), "主素材 ok + 音频成功 → true");
        assert!(is_media_ok(&outcome(true, None)), "主素材 ok + 未提取音频 → true");
        assert!(!is_media_ok(&outcome(true, Some(false))), "音频提取失败 → false");
        assert!(!is_media_ok(&outcome(false, Some(true))), "主素材失败 → false");
    }

    // ---------- content_from_model ----------

    fn content_model(kind: &str) -> content_entity::Model {
        content_entity::Model {
            id: "t1-douyin-c1".into(),
            task_id: "t1".into(),
            platform: "douyin".into(),
            content_id: "c1".into(),
            keyword: "k".into(),
            kind: kind.into(),
            title: Some("标题".into()),
            desc: Some("正文".into()),
            author_uid: "u1".into(),
            author_nickname: "作者".into(),
            author_json: r#"{"avatar":"https://a.b/c.png"}"#.into(),
            like_count: None,
            comment_count: None,
            collect_count: None,
            share_count: None,
            play_count: None,
            published_at: None,
            video_url: Some("https://v.cdn/x.mp4".into()),
            cover_url: Some("https://a.b/cover.jpg".into()),
            image_urls: r#"["u1","u2"]"#.into(),
            duration: Some(12),
            topics: "[]".into(),
            extra: r#"{"xsec_token":"tok"}"#.into(),
            owner: "tester".into(),
            collected_at: 0,
            media_status: None,
            audio_extracted: None,
            media_error: None,
            cover_path: None,
            avatar_path: None,
            audio_path: None,
            transcript: None,
            transcript_error: None,
            video_downloaded: None,
            image_total: None,
            image_done: None,
            comment_collected: None,
            intent_analyzed: None,
        }
    }

    #[test]
    fn content_from_model_maps_kind_string() {
        assert!(matches!(content_from_model(&content_model("video")).kind, ContentKind::Video));
        assert!(matches!(content_from_model(&content_model("image")).kind, ContentKind::Image));
        assert!(matches!(content_from_model(&content_model("article")).kind, ContentKind::Article));
        assert!(
            matches!(content_from_model(&content_model("live")).kind, ContentKind::Unknown),
            "未知 kind 应兜底 Unknown"
        );
    }

    #[test]
    fn content_from_model_carries_media_fields() {
        let c = content_from_model(&content_model("video"));
        assert_eq!(c.platform, "douyin");
        assert_eq!(c.content_id, "c1");
        assert_eq!(c.title.as_deref(), Some("标题"));
        assert_eq!(c.video_url.as_deref(), Some("https://v.cdn/x.mp4"));
        assert_eq!(c.duration, Some(12));
        assert_eq!(
            c.author.avatar.as_deref(),
            Some("https://a.b/c.png"),
            "头像应从 author_json 提取"
        );
        assert_eq!(c.image_urls, vec!["u1".to_string(), "u2".to_string()]);
    }

    // ---------- log_content_title ----------

    fn titled(title: Option<&str>, desc: Option<&str>) -> Content {
        Content {
            title: title.map(str::to_string),
            desc: desc.map(str::to_string),
            ..Default::default()
        }
    }

    #[test]
    fn log_content_title_prefers_title_then_desc_then_placeholder() {
        assert_eq!(log_content_title(&titled(Some("标题"), Some("正文"))), "标题");
        assert_eq!(
            log_content_title(&titled(Some("  "), Some("正文"))),
            "正文",
            "title 全空白应回退 desc"
        );
        assert_eq!(
            log_content_title(&titled(None, None)),
            "(无标题)",
            "title/desc 均空应给占位"
        );
    }

    #[test]
    fn log_content_title_truncates_to_40_chars() {
        let long = "一".repeat(50);
        let got = log_content_title(&titled(Some(&long), None));
        assert_eq!(got.chars().count(), 41, "应截断到 40 字 + 省略号");
        assert!(got.ends_with('…'));
    }

    // ---------- is_textless_comment ----------

    #[test]
    fn textless_comment_covers_empty_blank_emoji_and_placeholders() {
        for s in [
            "",
            "   ",
            "😂😂",
            "👍",
            "[捂脸]",
            "[捂脸][看]",
            "[捂脸] 😂",
            "。。。",
            "[图片]",
        ] {
            assert!(is_textless_comment(s), "应判为无文本: {s:?}");
        }
    }

    #[test]
    fn textless_comment_keeps_any_alphanumeric_content() {
        for s in [
            "哈哈哈",
            "666",
            "[捂脸]太真实了",
            "好用吗?",
            "1",
            "苏州",
        ] {
            assert!(!is_textless_comment(s), "应保留: {s:?}");
        }
    }

    // ---------- daily_task_due / watching_task_due ----------

    fn task_stub() -> task_entity::Model {
        task_entity::Model {
            id: "task-test".into(),
            name: "t".into(),
            industry: "i".into(),
            platform: "douyin".into(),
            keywords: "[]".into(),
            trigger_type: "daily".into(),
            scheduled_at: None,
            watch_interval_min: None,
            sort_mode: "synthetic".into(),
            time_range: "any".into(),
            per_keyword_limit: 0,
            min_likes: 0,
            audio_extract: false,
            ai_extract: false,
            collect_comments: false,
            comment_time_range: "any".into(),
            comment_limit: 0,
            analyze_comment_intent: false,
            status: "pending".into(),
            progress: 0,
            media_total: 0,
            media_done: 0,
            comment_video_total: 0,
            comment_video_done: 0,
            content_count: 0,
            comment_count: 0,
            started_at: None,
            finished_at: None,
            error_message: None,
            owner: "tester".into(),
            archived: false,
            auto_sync_obsidian: false,
            extra_filters: "{}".into(),
            target_urls: "[]".into(),
            max_retries: 0,
            retry_count: 0,
            next_retry_at: None,
            created_at: 0,
            updated_at: 0,
        }
    }

    /// 构造确定时分的本地时间,避免测试依赖真实当前时刻。
    fn local_dt(h: u32, m: u32) -> chrono::DateTime<chrono::Local> {
        chrono::NaiveDate::from_ymd_opt(2026, 1, 15)
            .unwrap()
            .and_hms_opt(h, m, 0)
            .unwrap()
            .and_local_timezone(chrono::Local)
            .single()
            .expect("测试时刻应可解析为唯一本地时间")
    }

    #[test]
    fn daily_task_due_requires_valid_schedule() {
        let now = local_dt(10, 0);
        let mut t = task_stub();
        assert!(!daily_task_due(&t, &now), "无 scheduled_at 不应到点");
        t.scheduled_at = Some("九点半".into());
        assert!(!daily_task_due(&t, &now), "非法时间格式不应到点");
    }

    #[test]
    fn daily_task_due_before_target_not_due() {
        let mut t = task_stub();
        t.scheduled_at = Some("09:30".into());
        assert!(!daily_task_due(&t, &local_dt(9, 0)), "未到今日目标点不应触发");
    }

    #[test]
    fn daily_task_due_after_target_and_not_run_today() {
        let mut t = task_stub();
        t.scheduled_at = Some("09:30".into());
        let now = local_dt(10, 0);
        assert!(daily_task_due(&t, &now), "已过点且从未运行应触发");
        // 上次启动早于今日目标点(昨天跑的),今天仍应触发
        t.started_at = Some(local_dt(9, 0).timestamp());
        assert!(daily_task_due(&t, &now), "到点前启动不算今天已跑");
        // 今天到点后才启动过 → 不重复
        t.started_at = Some(local_dt(9, 45).timestamp());
        assert!(!daily_task_due(&t, &now), "今天已跑过不应重复触发");
    }

    #[test]
    fn watching_task_due_requires_positive_interval_and_history() {
        let now = 1_700_000_000i64;
        let mut t = task_stub();
        assert!(!watching_task_due(&t, now), "无监听间隔不应到点");
        t.watch_interval_min = Some(0);
        assert!(!watching_task_due(&t, now), "间隔非正不应到点");
        t.watch_interval_min = Some(30);
        assert!(!watching_task_due(&t, now), "从未运行不应自动首启");
    }

    #[test]
    fn watching_task_due_interval_elapsed() {
        let now = 1_700_000_000i64;
        let mut t = task_stub();
        t.watch_interval_min = Some(30);
        t.finished_at = Some(now - 30 * 60);
        assert!(watching_task_due(&t, now), "距上次结束满一个间隔应到点");
        t.finished_at = Some(now - 30 * 60 + 1);
        assert!(!watching_task_due(&t, now), "间隔未满不应到点");
        // 无 finished_at 时兜底取 started_at
        t.finished_at = None;
        t.started_at = Some(now - 3600);
        assert!(watching_task_due(&t, now), "无结束时间应兜底取启动时间");
        // finished_at 优先于 started_at
        t.finished_at = Some(now - 60);
        assert!(!watching_task_due(&t, now), "应以更近的 finished_at 为准");
    }

    // ---------- progress_write_allowed ----------

    #[test]
    fn progress_write_first_call_allowed() {
        assert!(progress_write_allowed("test-pw-first", false), "首次回写应放行");
    }

    #[test]
    fn progress_write_within_interval_rejected() {
        let id = "test-pw-interval";
        assert!(progress_write_allowed(id, false), "首次应放行");
        assert!(!progress_write_allowed(id, false), "最小间隔内应拒绝");
    }

    #[test]
    fn progress_write_force_always_allowed() {
        let id = "test-pw-force";
        assert!(progress_write_allowed(id, false));
        assert!(progress_write_allowed(id, true), "force 应无条件放行");
    }

    #[test]
    fn progress_write_after_interval_allowed() {
        let id = "test-pw-after";
        assert!(progress_write_allowed(id, false));
        std::thread::sleep(std::time::Duration::from_millis(
            PROGRESS_WRITE_MIN_INTERVAL_MS + 100,
        ));
        assert!(progress_write_allowed(id, false), "超过最小间隔应放行");
    }

    // ---------- DB:filter_pending_media / load_recorded_ledger_ids ----------

    /// 每个 DB 测试用独立内存连接,库随连接销毁,互不影响。
    async fn mem_db() -> DatabaseConnection {
        sea_orm::Database::connect("sqlite::memory:")
            .await
            .expect("内存 SQLite 连接失败")
    }

    /// 测试内自建表(与 crates/core 的 init_schema 同写法,仅建需要的表)。
    async fn create_table<E: EntityTrait>(db: &DatabaseConnection, entity: E) {
        let backend = db.get_database_backend();
        let schema = sea_orm::Schema::new(backend);
        let mut stmt = schema.create_table_from_entity(entity);
        stmt.if_not_exists();
        db.execute(backend.build(&stmt)).await.expect("建表失败");
    }

    fn content_row(id: &str, task_id: &str, content_id: &str, media_status: Option<&str>) -> content_entity::ActiveModel {
        content_entity::ActiveModel {
            id: Set(id.into()),
            task_id: Set(task_id.into()),
            platform: Set("douyin".into()),
            content_id: Set(content_id.into()),
            keyword: Set("k".into()),
            kind: Set("video".into()),
            author_uid: Set("u".into()),
            author_nickname: Set("n".into()),
            author_json: Set("{}".into()),
            image_urls: Set("[]".into()),
            topics: Set("[]".into()),
            extra: Set("{}".into()),
            owner: Set("tester".into()),
            collected_at: Set(0),
            media_status: Set(media_status.map(str::to_string)),
            ..Default::default()
        }
    }

    fn content_stub(content_id: &str) -> Content {
        Content {
            content_id: content_id.to_string(),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn filter_pending_media_excludes_done_other_task_and_dups() {
        let db = mem_db().await;
        create_table(&db, content_entity::Entity).await;
        // 本任务已成功 / 待下载;其它任务的行不影响本任务判断
        for am in [
            content_row("t1-douyin-c1", "t1", "c1", Some("success")),
            content_row("t1-douyin-c2", "t1", "c2", Some("pending")),
            content_row("t2-douyin-c3", "t2", "c3", Some("success")),
        ] {
            am.insert(&db).await.expect("插入内容行失败");
        }
        let input = vec![
            content_stub("c1"), // 已成功 → 排除
            content_stub("c2"),
            content_stub("c2"), // 重复 → 去重
            content_stub("c3"), // 只有其它任务的行 → 排除
            content_stub("c4"), // 本任务无行 → 排除
        ];
        let out = filter_pending_media(&db, "t1", input).await;
        let ids: Vec<&str> = out.iter().map(|c| c.content_id.as_str()).collect();
        assert_eq!(
            ids,
            ["c2"],
            "success / 其它任务 / 无行 / 重复项都应排除,仅保留本任务 pending"
        );
    }

    #[tokio::test]
    async fn load_recorded_ledger_ids_returns_hits_only() {
        let db = mem_db().await;
        create_table(&db, ledger_entity::Entity).await;
        for (id, content_id) in [("douyin::c1", "c1"), ("douyin::c2", "c2")] {
            ledger_entity::ActiveModel {
                id: Set(id.to_string()),
                platform: Set("douyin".into()),
                content_id: Set(content_id.to_string()),
                created_at: Set(0),
            }
            .insert(&db)
            .await
            .expect("插入台账行失败");
        }
        let hits = load_recorded_ledger_ids(
            &db,
            &["douyin::c1".to_string(), "douyin::c9".to_string()],
        )
        .await;
        assert_eq!(hits.len(), 1, "只应返回已登记的 id");
        assert!(hits.contains("douyin::c1"));
        assert!(
            load_recorded_ledger_ids(&db, &[]).await.is_empty(),
            "空入参应返回空集合"
        );
    }
}
