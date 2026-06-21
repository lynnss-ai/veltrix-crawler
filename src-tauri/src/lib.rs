//! veltrix-crawler 后端入口。

mod adapter;
mod agent;
mod cloud;
mod commands;
mod cookie;
mod llm;
mod media;
mod model;
mod obsidian;
mod webview;

// 复用抽出到独立 crate 的核心模块,保持 config::/db::/api:: 用法不变
use veltrix_core::{api, config, db};

use commands::AppState;
use std::sync::Arc;
use tauri::{
    tray::{MouseButtonState, TrayIconBuilder, TrayIconEvent},
    Manager, WebviewUrl, WebviewWindowBuilder, WindowEvent,
};

// 主窗口 label:tauri.conf.json 中首个未显式命名的窗口默认即为 "main"
const MAIN_WINDOW_LABEL: &str = "main";
// 托盘弹出面板窗口 label
const TRAY_POPUP_LABEL: &str = "tray-popup";

// 显示并聚焦主窗口:托盘面板「显示主窗口」按钮共用
fn show_main_window(app: &tauri::AppHandle) {
    let Some(window) = app.get_webview_window(MAIN_WINDOW_LABEL) else {
        tracing::warn!("未找到主窗口 {MAIN_WINDOW_LABEL},无法显示");
        return;
    };
    if let Err(e) = window.show() {
        tracing::error!("显示主窗口失败: {e}");
    }
    if let Err(e) = window.set_focus() {
        tracing::error!("聚焦主窗口失败: {e}");
    }
}

// 面板逻辑尺寸(与 WebviewWindowBuilder.inner_size 一致)
const POPUP_W: f64 = 260.0;
const POPUP_H: f64 = 300.0;

// 在托盘图标附近弹出自定义面板:默认放点击点左上方,并夹在点击所在显示器内,避免错位 / 移出屏幕。
fn show_tray_popup(app: &tauri::AppHandle, click_pos: tauri::PhysicalPosition<f64>) {
    let Some(popup) = app.get_webview_window(TRAY_POPUP_LABEL) else {
        return;
    };
    // 隐藏窗口的 outer_size 不可靠,用缩放因子把逻辑尺寸换算成物理像素
    let scale = popup.scale_factor().unwrap_or(1.0);
    let w = POPUP_W * scale;
    let h = POPUP_H * scale;

    // 取点击点所在显示器边界(多屏 / 高 DPI 下定位才正确)
    let monitor = app
        .monitor_from_point(click_pos.x, click_pos.y)
        .ok()
        .flatten()
        .or_else(|| popup.primary_monitor().ok().flatten());
    let (mx, my, mw, mh) = monitor
        .map(|m| {
            let p = *m.position();
            let s = *m.size();
            (p.x as f64, p.y as f64, s.width as f64, s.height as f64)
        })
        .unwrap_or((0.0, 0.0, 1920.0, 1080.0));

    let margin = 8.0;
    // 面板右边缘对齐点击点;上方空间够则放上方,否则放下方(适配任务栏在顶部的情况)
    let x = click_pos.x - w;
    let y = if click_pos.y - h - margin >= my {
        click_pos.y - h - margin
    } else {
        click_pos.y + margin
    };
    // 夹在显示器内
    let x = x.clamp(mx + margin, (mx + mw - w - margin).max(mx + margin));
    let y = y.clamp(my + margin, (my + mh - h - margin).max(my + margin));

    let _ = popup.set_position(tauri::PhysicalPosition::new(x, y));
    let _ = popup.show();
    let _ = popup.set_focus();
}

// 托盘面板「显示主窗口」:显示主窗口并收起面板
#[tauri::command]
fn show_main_from_tray(app: tauri::AppHandle) {
    show_main_window(&app);
    if let Some(popup) = app.get_webview_window(TRAY_POPUP_LABEL) {
        let _ = popup.hide();
    }
}

// 托盘面板「退出」:真正退出进程。退出前停掉编程沙盒容器(释放资源,工作区文件保留)。
#[tauri::command]
fn quit_app(app: tauri::AppHandle) {
    if let Some(state) = app.try_state::<AppState>() {
        let db = state.db.clone();
        tauri::async_runtime::block_on(agent::coding::commands::stop_sandbox_on_exit(&db));
    }
    app.exit(0);
}

/// 弹系统保存对话框并把文本写到用户选定的文件(代码块下载等)。
/// WebView2 不支持 `<a download>` 触发保存,改走原生对话框 + 写文件。
/// 返回 Some(本地绝对路径)=已保存 / None=用户取消。(命令在工作线程运行,可安全用 blocking 对话框)
#[tauri::command]
fn save_text_dialog(
    app: tauri::AppHandle,
    content: String,
    file_name: String,
) -> std::result::Result<Option<String>, String> {
    use tauri_plugin_dialog::DialogExt;
    let chosen = app
        .dialog()
        .file()
        .set_file_name(&file_name)
        .blocking_save_file();
    let Some(file_path) = chosen else {
        return Ok(None); // 用户取消
    };
    // FilePath 在桌面保存场景为本地路径,Display 即路径字符串
    let path = std::path::PathBuf::from(file_path.to_string());
    std::fs::write(&path, content.as_bytes()).map_err(|e| format!("写文件失败: {e}"))?;
    Ok(Some(path.to_string_lossy().into_owned()))
}

/// 保存二进制文件(base64 解码后写盘):弹原生保存对话框,供导出 PDF 等渲染产物。
/// 返回 Some(本地绝对路径)=已保存 / None=用户取消。
#[tauri::command]
fn save_binary_dialog(
    app: tauri::AppHandle,
    content_base64: String,
    file_name: String,
) -> std::result::Result<Option<String>, String> {
    use base64::Engine;
    use tauri_plugin_dialog::DialogExt;
    let chosen = app
        .dialog()
        .file()
        .set_file_name(&file_name)
        .blocking_save_file();
    let Some(file_path) = chosen else {
        return Ok(None); // 用户取消
    };
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(content_base64.as_bytes())
        .map_err(|e| format!("内容解码失败: {e}"))?;
    let path = std::path::PathBuf::from(file_path.to_string());
    std::fs::write(&path, &bytes).map_err(|e| format!("写文件失败: {e}"))?;
    Ok(Some(path.to_string_lossy().into_owned()))
}

/// 文件是否仍存在于本地(下载历史标记「在 / 已丢失」用)。
#[tauri::command]
fn path_exists(path: String) -> bool {
    std::path::Path::new(&path).is_file()
}

/// 在系统文件管理器中打开文件所在目录:文件还在则定位并选中它;文件已删则退而打开其父目录。
#[tauri::command]
fn reveal_path(app: tauri::AppHandle, path: String) -> std::result::Result<(), String> {
    use tauri_plugin_opener::OpenerExt;
    let p = std::path::PathBuf::from(&path);
    if p.is_file() {
        app.opener()
            .reveal_item_in_dir(&p)
            .map_err(|e| format!("打开目录失败: {e}"))
    } else if let Some(parent) = p.parent().filter(|d| d.exists()) {
        app.opener()
            .open_path(parent.to_string_lossy().into_owned(), None::<&str>)
            .map_err(|e| format!("打开目录失败: {e}"))
    } else {
        Err("文件与所在目录均已不存在".into())
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .setup(|app| {
            let base = app.path().app_config_dir()?;
            let config_dir = config::resolve_config_dir(&base);
            let cfg = config::AppConfig::load_or_default(&config_dir)?;

            // 连接数据库(运行时二选一 SQLite / PG)并建表;setup 为同步上下文,阻塞等待完成
            let db = tauri::async_runtime::block_on(db::connect(&config_dir, &cfg.database))?;

            // 应用重启后内存里的采集 spawn 已丢失:把残留的「进行中」任务标记为中断,
            // 避免界面一直显示假进度(运行中 / 评论采集中 / 意向分析中 / 素材下载中)。
            {
                use sea_orm::sea_query::Expr;
                use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
                use veltrix_core::db::entity::task;
                let reset_db = db.clone();
                if let Err(e) = tauri::async_runtime::block_on(async {
                    task::Entity::update_many()
                        .col_expr(task::Column::Status, Expr::value("failed"))
                        .col_expr(
                            task::Column::ErrorMessage,
                            Expr::value("应用重启,采集已中断,可重新运行"),
                        )
                        .filter(task::Column::Status.is_in([
                            "running",
                            "collecting_comments",
                            "analyzing_comments",
                            "downloading_media",
                        ]))
                        .exec(&reset_db)
                        .await
                }) {
                    tracing::warn!("重置残留进行中任务失败: {e}");
                }
            }

            // 作者表存量回填:authors 为空时,从 content 历史数据回填一次(幂等)
            {
                let migrate_db = db.clone();
                tauri::async_runtime::block_on(async {
                    commands::task::migrate_authors_from_contents(&migrate_db).await;
                });
            }

            // 话题存量回填:历史内容 topics 为空但正文含 #话题 的,从正文补提取一次(幂等,不改正文)
            {
                let backfill_db = db.clone();
                tauri::async_runtime::block_on(async {
                    commands::task::backfill_empty_topics(&backfill_db).await;
                });
            }

            // 旧版 provider.code 为随机值(PRV-XXXX);本次起改用标准厂商 code
            // (deepseek/qwen/mimo/glm/minimax),语音转写按 code 判 ASR。这里按 name / api_url
            // 关键词把旧 provider 的 code 迁移为标准值,使已有的 MiMo 等厂商在转写配置里可被识别;
            // 匹配不到的保留原值(视为非标准厂商,不支持 ASR)。
            {
                use sea_orm::{ActiveModelTrait, EntityTrait, IntoActiveModel, Set};
                use veltrix_core::db::entity::provider;
                let migrate_db = db.clone();
                if let Err(e) = tauri::async_runtime::block_on(async {
                    for p in provider::Entity::find().all(&migrate_db).await? {
                        if matches!(
                            p.code.as_str(),
                            "deepseek" | "qwen" | "mimo" | "glm" | "minimax"
                        ) {
                            continue;
                        }
                        let hay = format!("{} {}", p.name, p.api_url).to_lowercase();
                        // 注意:MiMo 的 api.xiaomimimo.com 含 "mimo",能命中
                        let mapped = if hay.contains("deepseek") {
                            Some("deepseek")
                        } else if hay.contains("qwen")
                            || hay.contains("千问")
                            || hay.contains("通义")
                            || hay.contains("dashscope")
                        {
                            Some("qwen")
                        } else if hay.contains("mimo") || hay.contains("小米") {
                            Some("mimo")
                        } else if hay.contains("glm")
                            || hay.contains("智谱")
                            || hay.contains("bigmodel")
                        {
                            Some("glm")
                        } else if hay.contains("minimax") {
                            Some("minimax")
                        } else {
                            None
                        };
                        if let Some(code) = mapped {
                            let mut am = p.into_active_model();
                            am.code = Set(code.to_string());
                            am.update(&migrate_db).await?;
                        }
                    }
                    Ok::<(), sea_orm::DbErr>(())
                }) {
                    tracing::warn!("迁移 provider code 失败(忽略): {e}");
                }
            }

            // 首次启动初始化 5 家标准模型厂商(apiKey/models 留空待用户配),按 id 幂等跳过已有
            {
                use sea_orm::{ActiveModelTrait, EntityTrait, Set};
                use veltrix_core::db::entity::provider;
                let seed_db = db.clone();
                if let Err(e) = tauri::async_runtime::block_on(async {
                    use sea_orm::{ColumnTrait, QueryFilter};
                    let now = chrono::Utc::now().timestamp();
                    for cap in llm::all_capabilities() {
                        // 按 code 判重:用户可能已手动加过该厂商(id 不同),避免重复初始化
                        let exists = provider::Entity::find()
                            .filter(provider::Column::Code.eq(cap.code.as_str()))
                            .one(&seed_db)
                            .await?
                            .is_some();
                        if !exists {
                            provider::ActiveModel {
                                id: Set(format!("prv-{}", cap.code)),
                                code: Set(cap.code),
                                name: Set(cap.name),
                                api_url: Set(cap.api_url),
                                api_key: Set(String::new()),
                                models: Set(String::new()),
                                created_at: Set(now),
                                updated_at: Set(now),
                            }
                            .insert(&seed_db)
                            .await?;
                        }
                    }
                    Ok::<(), sea_orm::DbErr>(())
                }) {
                    tracing::warn!("初始化标准厂商失败(忽略): {e}");
                }
            }

            let cookies = Arc::new(cookie::CookiePool::new(db.clone()));
            tracing::info!(
                platforms = cfg.platforms.len(),
                "配置与账号池就绪,数据目录: {}",
                config_dir.display()
            );

            // 采集日志落库:全局通道 + 后台 writer,把 emit 的日志异步持久化到 collect_logs。
            // 异步落库不阻塞采集;通道未初始化时 emit 静默跳过落库(仅推前端事件)。
            let (log_tx, mut log_rx) =
                tokio::sync::mpsc::unbounded_channel::<webview::CollectLog>();
            webview::init_log_sink(log_tx);
            let log_db = db.clone();
            tauri::async_runtime::spawn(async move {
                use sea_orm::{ActiveModelTrait, Set};
                use veltrix_core::db::entity::collect_log;
                while let Some(log) = log_rx.recv().await {
                    let entry_json =
                        log.entry.as_ref().and_then(|e| serde_json::to_string(e).ok());
                    let am = collect_log::ActiveModel {
                        task_id: Set(log.task_id),
                        ts: Set(log.ts),
                        level: Set(log.level),
                        message: Set(log.message),
                        entry_json: Set(entry_json),
                        ..Default::default()
                    };
                    if let Err(e) = am.insert(&log_db).await {
                        tracing::warn!("采集日志落库失败: {e}");
                    }
                }
            });

            // 启动对外 HTTP API(复用同一数据库连接);失败仅告警,不阻塞应用
            let api_db = db.clone();
            tauri::async_runtime::spawn(async move {
                let addr = std::net::SocketAddr::from(([127, 0, 0, 1], 8787));
                // 桌面端固定 Desktop 模式:不挂 /pair /devices,不连 Redis
                if let Err(e) =
                    api::serve(api_db, addr, api::ServerMode::Desktop, None).await
                {
                    tracing::error!("HTTP API 启动失败: {e}");
                }
            });

            // 编程沙盒:若设为 Docker 模式,启动时后台拉起共享容器(失败仅告警)
            {
                let sb_db = db.clone();
                let sb_dir = config_dir.clone();
                tauri::async_runtime::spawn(async move {
                    agent::coding::commands::ensure_sandbox_on_start(&sb_db, &sb_dir).await;
                });
            }

            // 云端客户端:启动后自动检查是否已配对,若有 pc_token 直接拉起 WS
            let cloud = Arc::new(cloud::CloudClient::new(config_dir.clone()));
            let cloud_runner = cloud.clone();
            tauri::async_runtime::spawn(async move {
                cloud_runner.run_loop().await;
            });

            // 注册平台适配器:把拦截到的接口响应解析为统一模型
            let mut registry = adapter::AdapterRegistry::new();
            registry.register(Arc::new(adapter::douyin::DouyinAdapter::new()));
            registry.register(Arc::new(adapter::xhs::XhsAdapter::new()));
            registry.register(Arc::new(adapter::kuaishou::KuaishouAdapter::new()));
            registry.register(Arc::new(adapter::bilibili::BilibiliAdapter::new()));
            registry.register(Arc::new(adapter::tiktok::TiktokAdapter::new()));
            registry.register(Arc::new(adapter::youtube::YoutubeAdapter::new()));

            // 启动时探测一次 ffmpeg 可用性,写入录屏状态;后续录屏命令直接读标记,不再每次启子进程探测
            let ffmpeg_available =
                crate::media::probe_ffmpeg(cfg.media.ffmpeg_path.as_deref()).is_some();
            tracing::info!("ffmpeg 启动探测:可用={ffmpeg_available}");

            app.manage(AppState {
                config: std::sync::Mutex::new(cfg),
                config_dir,
                registry,
                db,
                cookies,
                webviews: Arc::new(webview::pool::WebviewPool::new()),
                intercept_channel: Arc::new(webview::InterceptChannel::new()),
                rpa_channel: Arc::new(webview::RpaChannel::new()),
                collect_control: Arc::new(webview::CollectControl::new()),
                current_user: std::sync::Mutex::new(None),
                cloud,
                collect_locks: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
                collect_semaphore: Arc::new(tokio::sync::Semaphore::new(
                    commands::MAX_CONCURRENT_COLLECT,
                )),
                login_verdicts: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
                dev_server: Arc::new(std::sync::Mutex::new(
                    agent::coding::commands::DevServer::default(),
                )),
                sandbox_ready: std::sync::Mutex::new(
                    agent::coding::commands::SandboxReady::default(),
                ),
                app_handle: app.handle().clone(),
                agent_cancel: Arc::new(std::sync::Mutex::new(std::collections::HashSet::new())),
                agent_confirm: Arc::new(agent::core::shared::AgentConfirmChannel::new()),
                recording: {
                    let rec = agent::computer::recorder::RecordingState::new();
                    rec.set_ffmpeg_available(ffmpeg_available);
                    rec
                },
            });

            // 任务调度器:每 30s 扫描 daily / watching 任务,到点自动启动采集
            // (前端「定时任务队列」的倒计时与此对齐,误差 ≤ 一个扫描周期)
            {
                let handle = app.handle().clone();
                tauri::async_runtime::spawn(async move {
                    loop {
                        tokio::time::sleep(std::time::Duration::from_secs(30)).await;
                        commands::run_due_scheduled_tasks(&handle).await;
                    }
                });
            }

            // 托盘弹出面板窗口:无边框 / 透明 / 不进任务栏 / 置顶,隐藏待用。
            // 点击托盘图标时由 on_tray_icon_event 定位并显示,替代传统系统右键菜单。
            let popup = WebviewWindowBuilder::new(
                app,
                TRAY_POPUP_LABEL,
                WebviewUrl::App("index.html".into()),
            )
            .title("Veltrix")
            .inner_size(260.0, 300.0)
            .decorations(false)
            .transparent(true)
            .skip_taskbar(true)
            .always_on_top(true)
            .resizable(false)
            .shadow(false)
            .visible(false)
            .build()?;

            // 面板失焦自动隐藏(点击面板外即收起)
            {
                let popup_for_event = popup.clone();
                popup.on_window_event(move |event| {
                    if let WindowEvent::Focused(false) = event {
                        let _ = popup_for_event.hide();
                    }
                });
            }

            let mut tray_builder = TrayIconBuilder::new()
                .tooltip("VeltrixLoop")
                .on_tray_icon_event(|tray, event| {
                    // 单击(按下抬起)托盘图标弹出自定义面板
                    if let TrayIconEvent::Click {
                        button_state: MouseButtonState::Up,
                        position,
                        ..
                    } = event
                    {
                        show_tray_popup(tray.app_handle(), position);
                    }
                });

            // 复用应用打包图标(tauri.conf.json bundle.icon)作为托盘图标
            if let Some(icon) = app.default_window_icon().cloned() {
                tray_builder = tray_builder.icon(icon);
            }
            tray_builder.build(app)?;

            // 拦截主窗口关闭:改为隐藏到托盘,防止系统级关闭直接退出进程
            if let Some(main_window) = app.get_webview_window(MAIN_WINDOW_LABEL) {
                let window_for_event = main_window.clone();
                main_window.on_window_event(move |event| {
                    if let WindowEvent::CloseRequested { api, .. } = event {
                        api.prevent_close();
                        if let Err(e) = window_for_event.hide() {
                            tracing::error!("隐藏主窗口到托盘失败: {e}");
                        }
                    }
                });
            }

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            // 托盘弹出面板
            show_main_from_tray,
            quit_app,
            // 代码块下载 / 导出:保存对话框 + 写文件
            save_text_dialog,
            save_binary_dialog,
            // 下载历史:查文件是否存在 + 打开所在目录
            path_exists,
            reveal_path,
            // 平台管理
            commands::get_app_config,
            commands::get_database_size,
            commands::get_data_dir,
            commands::get_media_root,
            commands::get_database_path,
            commands::test_database_connection,
            commands::set_database_config,
            commands::set_storage_path,
            commands::get_agent_guidelines,
            commands::set_agent_guidelines,
            commands::set_intent_config,
            commands::set_transcription_config,
            commands::get_role_models,
            commands::set_role_models,
            commands::list_provider_capabilities,
            commands::save_text_file,
            commands::save_binary_file,
            commands::clear_business_data,
            // 鉴权 / 初始化
            commands::admin::has_users,
            commands::admin::verify_session_user,
            commands::admin::login,
            // 会话:当前登录用户
            commands::set_current_user,
            commands::clear_current_user,
            // 用户管理
            commands::admin::list_users,
            commands::admin::upsert_user,
            commands::admin::remove_user,
            commands::admin::reset_user_password,
            commands::admin::change_password,
            commands::admin::update_profile,
            // 系统配置:模型厂商 / 提示词
            commands::admin::list_providers,
            commands::admin::upsert_provider,
            commands::admin::remove_provider,
            commands::admin::list_prompts,
            commands::admin::upsert_prompt,
            commands::admin::remove_prompt,
            // 客户管理
            commands::admin::list_customers,
            commands::admin::upsert_customer,
            commands::admin::remove_customer,
            // 行业类别 / 关键词
            commands::admin::list_industries,
            commands::admin::upsert_industry,
            commands::admin::remove_industry,
            commands::admin::list_keywords,
            commands::admin::create_keywords,
            commands::admin::upsert_keyword,
            commands::admin::remove_keyword,
            commands::list_platforms,
            commands::upsert_platform,
            commands::remove_platform,
            commands::registered_adapters,
            // 账号管理
            commands::list_accounts,
            commands::upsert_account,
            commands::remove_account,
            commands::clear_account_login,
            commands::open_login_window,
            commands::login_status_report,
            // 采集:拦截回传与启动
            commands::intercept_push,
            commands::stop_collect,
            commands::report_collect_verify,
            commands::rpa_done,
            commands::start_collect,
            commands::run_task,
            // 任务调度
            commands::task::list_tasks,
            commands::task::upsert_task,
            commands::task::update_task_status,
            commands::task::remove_task,
            commands::task::list_contents,
            commands::task::list_comments,
            commands::task::list_collect_logs,
            commands::task::list_task_runs,
            commands::task::list_run_logs,
            commands::dashboard::dashboard_overview,
            commands::task::remove_content,
            commands::task::remove_contents,
            commands::task::get_content_detail,
            commands::task::set_author_monitored,
            commands::task::list_authors,
            commands::task::set_author_monitored_by_id,
            commands::task::set_author_blacklisted_by_id,
            commands::enrich_authors,
            commands::retry_content_media,
            commands::compensate_task,
            commands::check_ffmpeg,
            commands::set_obsidian_vault,
            commands::get_obsidian_vault,
            commands::sync_contents_to_obsidian,
            // AI 对话
            agent::chat::commands::list_conversations,
            agent::chat::commands::create_conversation,
            agent::chat::commands::rename_conversation,
            agent::chat::commands::archive_conversation,
            agent::chat::commands::update_conversation_model,
            agent::chat::commands::get_conversation_summary,
            agent::chat::commands::update_conversation_summary,
            agent::chat::commands::delete_conversation,
            agent::chat::commands::list_chat_messages,
            agent::chat::commands::send_chat_message,
            agent::chat::commands::send_chat_message_stream,
            agent::chat::commands::attach_recording_message,
            agent::chat::commands::transcribe_chat_audio,
            agent::chat::commands::build_content_attachments,
            // AI 对话:长期记忆
            agent::chat::memory::list_chat_memories,
            agent::chat::memory::add_chat_memory,
            agent::chat::memory::update_chat_memory,
            agent::chat::memory::delete_chat_memory,
            agent::chat::memory::clear_chat_memories,
            agent::chat::memory::get_chat_memory_enabled,
            agent::chat::memory::set_chat_memory_enabled,
            agent::chat::memory::set_chat_memory_pinned,
            agent::chat::memory::get_embedding_config,
            agent::chat::memory::set_embedding_config,
            // 编程 Agent
            agent::coding::commands::send_coding_message,
            agent::coding::commands::stop_coding_agent,
            agent::coding::commands::get_coding_workspace,
            agent::coding::commands::set_coding_workspace,
            agent::coding::commands::run_workspace_command,
            agent::coding::commands::checkpoint_rollback,
            agent::coding::commands::list_coding_checkpoints,
            agent::coding::commands::rollback_to_checkpoint,
            agent::coding::commands::get_checkpoint_diff,
            agent::coding::commands::list_workspace_files,
            agent::coding::commands::read_workspace_file,
            agent::coding::commands::write_workspace_file,
            agent::coding::commands::classify_agent_type,
            agent::coding::commands::start_dev_server,
            agent::coding::commands::stop_dev_server,
            agent::coding::commands::get_dev_server_status,
            agent::coding::commands::get_sandbox_config,
            agent::coding::commands::get_sandbox_stats,
            agent::coding::commands::set_sandbox_config,
            agent::coding::commands::sandbox_start,
            agent::coding::commands::sandbox_stop,
            agent::coding::commands::sandbox_recreate,
            // 浏览器 Agent(RPA:内嵌右栏真实 webview + 回读 + 接口拦截)
            agent::rpa::commands::send_browser_message,
            agent::rpa::commands::set_agent_webview_bounds,
            agent::rpa::commands::show_agent_webview,
            agent::rpa::commands::hide_agent_webview,
            agent::rpa::commands::hide_all_agent_webviews,
            agent::rpa::commands::get_agent_network,
            // 「电脑操作」工具基础设施(桌面 GUI + 跨平台终端;独立模块,列出供前端 / 编排挑选)
            agent::list_agent_tools,
            // 电脑操作 Agent:聚合全部工具的 ReAct 智能体
            agent::computer::commands::send_computer_message,
            agent::computer::commands::resolve_agent_confirm,
            // 屏幕录制:打开悬浮条 / 取消 / 开始(最小化主窗口 + ffmpeg 录全屏)/ 停止 / 查状态
            agent::computer::recorder::open_recording_overlay,
            agent::computer::recorder::cancel_recording_overlay,
            agent::computer::recorder::start_screen_recording,
            agent::computer::recorder::stop_screen_recording,
            agent::computer::recorder::get_recording_status,
            // 拍照回传:截桌面屏幕 → base64 data URL(前端预览 / 将来喂视觉模型)
            agent::capture_desktop_screenshot,
            // 云端连接(配对 / WS / 远程指令)
            commands::cloud::cloud_get_config,
            commands::cloud::cloud_get_status,
            commands::cloud::cloud_save_base_url,
            commands::cloud::cloud_login,
            commands::cloud::cloud_pair_init,
            commands::cloud::cloud_disconnect,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
