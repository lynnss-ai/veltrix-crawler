//! veltrix-server:独立 Web API 服务二进制。
//!
//! 复用 veltrix-core 的 Axum API,可脱离 Tauri 桌面端单独部署。
//! 连接串优先级与桌面端一致:环境变量 VELTRIX_DATABASE_URL > 配置 > 默认本地 SQLite,
//! 由 veltrix_core::db 内部的 resolve_url 处理。

use std::net::SocketAddr;
use std::path::PathBuf;

use veltrix_core::config::DatabaseConfig;

/// 默认监听端口,与桌面端内嵌 API 保持一致。
const DEFAULT_PORT: u16 = 8787;
/// 数据目录环境变量:决定默认本地 SQLite 与配置文件的落盘位置。
const DATA_DIR_ENV: &str = "VELTRIX_DATA_DIR";

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    // 数据目录:未设置时退回当前目录,默认 SQLite 文件落在此处
    let data_dir = std::env::var(DATA_DIR_ENV)
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."));

    // url 留空,交给 db::connect 内部按 VELTRIX_DATABASE_URL > 默认本地 SQLite 解析
    let cfg = DatabaseConfig::default();

    let db = veltrix_core::db::connect(&data_dir, &cfg)
        .await
        .expect("数据库连接失败");

    let addr = SocketAddr::from(([0, 0, 0, 0], DEFAULT_PORT));
    veltrix_core::api::serve(db, addr)
        .await
        .expect("HTTP 服务启动失败");
}
