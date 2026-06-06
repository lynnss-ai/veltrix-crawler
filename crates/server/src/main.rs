//! veltrix-server:独立 Web API 服务二进制。
//!
//! 部署形态由环境变量 `VELTRIX_MODE` 决定:
//! - cloud   → 绑 0.0.0.0:8787,挂 /pair /devices,连 Redis(VELTRIX_REDIS_URL)
//! - desktop → 绑 127.0.0.1:8787,等同桌面端内嵌 API(MVP 与 src-tauri 内嵌等价,主要做调试)
//!
//! 数据库连接串与桌面端一致,由 VELTRIX_DATABASE_URL 控制。

use std::net::SocketAddr;
use std::path::PathBuf;

use veltrix_core::api::ServerMode;
use veltrix_core::config::DatabaseConfig;

const DEFAULT_PORT: u16 = 8787;
const DATA_DIR_ENV: &str = "VELTRIX_DATA_DIR";
const REDIS_URL_ENV: &str = "VELTRIX_REDIS_URL";
const DEFAULT_REDIS_URL: &str = "redis://127.0.0.1:6379";

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let mode = ServerMode::from_env();
    tracing::info!("启动模式: {mode:?}");

    let data_dir = std::env::var(DATA_DIR_ENV)
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."));

    let cfg = DatabaseConfig::default();
    let db = veltrix_core::db::connect(&data_dir, &cfg)
        .await
        .expect("数据库连接失败");

    // cloud 模式必须接 Redis,起不来直接 panic;desktop 不依赖
    let redis = if mode == ServerMode::Cloud {
        let url = std::env::var(REDIS_URL_ENV).unwrap_or_else(|_| DEFAULT_REDIS_URL.to_string());
        tracing::info!("连接 Redis: {url}");
        Some(
            veltrix_core::api::build_redis_pool(&url)
                .await
                .expect("Redis 连接池构建失败"),
        )
    } else {
        None
    };

    let bind_ip = match mode {
        ServerMode::Cloud => [0, 0, 0, 0],
        ServerMode::Desktop => [127, 0, 0, 1],
    };
    let addr = SocketAddr::from((bind_ip, DEFAULT_PORT));

    veltrix_core::api::serve(db, addr, mode, redis)
        .await
        .expect("HTTP 服务启动失败");
}
