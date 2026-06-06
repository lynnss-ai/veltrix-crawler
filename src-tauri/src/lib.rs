//! veltrix-crawler 后端入口。

mod adapter;
mod cloud;
mod commands;
mod cookie;
mod media;
mod model;
mod webview;

// 复用抽出到独立 crate 的核心模块,保持 config::/db::/api:: 用法不变
use veltrix_core::{api, config, db};

use commands::AppState;
use std::sync::Arc;
use tauri::{
    menu::{Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    Manager, WindowEvent,
};

// 主窗口 label:tauri.conf.json 中首个未显式命名的窗口默认即为 "main"
const MAIN_WINDOW_LABEL: &str = "main";

// 显示并聚焦主窗口:托盘「显示」与左键点击托盘图标共用
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
        .setup(|app| {
            let base = app.path().app_config_dir()?;
            let config_dir = config::resolve_config_dir(&base);
            let cfg = config::AppConfig::load_or_default(&config_dir)?;

            // 连接数据库(运行时二选一 SQLite / PG)并建表;setup 为同步上下文,阻塞等待完成
            let db = tauri::async_runtime::block_on(db::connect(&config_dir, &cfg.database))?;
            let cookies = Arc::new(cookie::CookiePool::new(db.clone()));
            tracing::info!(
                platforms = cfg.platforms.len(),
                "配置与账号池就绪,数据目录: {}",
                config_dir.display()
            );

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
            });

            // 系统托盘:菜单仅「显示 / 退出」,左键点击图标等同「显示」
            let show_item = MenuItem::with_id(app, "show", "显示", true, None::<&str>)?;
            let quit_item = MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?;
            let tray_menu = Menu::with_items(app, &[&show_item, &quit_item])?;

            let mut tray_builder = TrayIconBuilder::new()
                .menu(&tray_menu)
                .tooltip("veltrix-crawler")
                // 默认只在右键弹出菜单,左键事件留给 on_tray_icon_event 处理显示窗口
                .show_menu_on_left_click(false)
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "show" => show_main_window(app),
                    "quit" => app.exit(0),
                    _ => {}
                })
                .on_tray_icon_event(|tray, event| {
                    // 仅左键单击(按下抬起)显示窗口,避免右键菜单触发时误显示
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } = event
                    {
                        show_main_window(tray.app_handle());
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
            // 平台管理
            commands::get_app_config,
            commands::get_database_size,
            commands::get_data_dir,
            commands::get_database_path,
            commands::test_database_connection,
            commands::set_database_config,
            commands::set_storage_path,
            commands::save_text_file,
            // 鉴权 / 初始化
            commands::admin::has_users,
            commands::admin::login,
            // 会话:当前登录用户
            commands::set_current_user,
            commands::clear_current_user,
            // 用户管理
            commands::admin::list_users,
            commands::admin::upsert_user,
            commands::admin::remove_user,
            commands::admin::reset_user_password,
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
            // 平台 API 子列表
            commands::admin::list_apis,
            commands::admin::upsert_api,
            commands::admin::remove_api,
            // 账号管理
            commands::list_accounts,
            commands::upsert_account,
            commands::remove_account,
            commands::open_login_window,
            // 采集:拦截回传与启动
            commands::intercept_push,
            commands::stop_collect,
            commands::rpa_done,
            commands::start_collect,
            commands::run_task,
            // 任务调度
            commands::task::list_tasks,
            commands::task::upsert_task,
            commands::task::update_task_status,
            commands::task::remove_task,
            commands::task::list_contents,
            commands::task::remove_content,
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
