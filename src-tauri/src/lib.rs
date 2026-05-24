//! veltrix-crawler 后端入口。

mod adapter;
mod commands;
mod cookie;
mod model;
mod webview;

// 复用抽出到独立 crate 的核心模块,保持 config::/db::/api:: 用法不变
use veltrix_core::{api, config, db};

use commands::AppState;
use std::sync::Arc;
use tauri::Manager;

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
                if let Err(e) = api::serve(api_db, addr).await {
                    tracing::error!("HTTP API 启动失败: {e}");
                }
            });

            app.manage(AppState {
                config: std::sync::Mutex::new(cfg),
                config_dir,
                registry: adapter::AdapterRegistry::new(),
                db,
                cookies,
                webviews: Arc::new(webview::pool::WebviewPool::new()),
                intercept_channel: Arc::new(webview::InterceptChannel::new()),
                current_user: std::sync::Mutex::new(None),
            });
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
            commands::start_collect,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
