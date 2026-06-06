//! 对外 HTTP API(Axum)。版本化路由 `/api/v1`、统一响应结构、JWT 鉴权。
//!
//! 两种部署形态(由 ServerMode 区分):
//! - Desktop:随 Tauri 桌面端启动,绑 127.0.0.1,不挂 /pair /devices(无需 Redis)
//! - Cloud  :云端中转服务,绑 0.0.0.0,挂 /pair /devices 并连 Redis

mod auth;
mod commands;
mod devices;
mod error;
mod pair;
mod response;
mod v1;
mod ws;
mod ws_hub;

pub use error::AppError;
pub use response::ApiResponse;

use axum::Router;
use axum::http::{HeaderValue, Method, header};
use bb8::Pool;
use bb8_redis::RedisConnectionManager;
use sea_orm::DatabaseConnection;
use std::net::SocketAddr;
use std::sync::Arc;
use tower_http::cors::{AllowOrigin, CorsLayer};

pub type RedisPool = Pool<RedisConnectionManager>;

/// 部署形态。同一个 binary 通过 VELTRIX_MODE 切换路由装配和资源依赖。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServerMode {
    Desktop,
    Cloud,
}

impl ServerMode {
    pub fn from_env() -> Self {
        match std::env::var("VELTRIX_MODE")
            .ok()
            .as_deref()
            .map(str::trim)
        {
            Some("cloud") => Self::Cloud,
            _ => Self::Desktop,
        }
    }
}

#[derive(Clone)]
pub struct ApiState {
    pub db: DatabaseConnection,
    pub jwt_secret: Arc<Vec<u8>>,
    pub mode: ServerMode,
    /// 仅 Cloud 模式下有值;Desktop 模式为 None
    pub redis: Option<RedisPool>,
    /// WS 连接注册表;Cloud / Desktop 均持有(Desktop 模式下无连接,但保持类型一致)
    pub ws_hub: Arc<ws_hub::WsHub>,
}

/// CORS 配置:
/// - Cloud 模式:从 `VELTRIX_CORS_ORIGINS` 读白名单(逗号分隔),未配置时仅允许同源(不放开任何跨域)
/// - Desktop 模式:permissive(本地开发友好)
fn cors_layer(mode: ServerMode) -> CorsLayer {
    if mode == ServerMode::Desktop {
        return CorsLayer::permissive();
    }
    let methods = [
        Method::GET,
        Method::POST,
        Method::PUT,
        Method::DELETE,
        Method::OPTIONS,
    ];
    let allow_headers = [header::AUTHORIZATION, header::CONTENT_TYPE];
    let origins: Vec<HeaderValue> = std::env::var("VELTRIX_CORS_ORIGINS")
        .ok()
        .map(|s| {
            s.split(',')
                .map(|x| x.trim().to_string())
                .filter(|x| !x.is_empty())
                .filter_map(|x| HeaderValue::from_str(&x).ok())
                .collect()
        })
        .unwrap_or_default();
    if origins.is_empty() {
        tracing::warn!(
            "Cloud 模式未配置 VELTRIX_CORS_ORIGINS,所有跨域请求将被拒绝;同源请求仍可访问"
        );
        CorsLayer::new().allow_methods(methods).allow_headers(allow_headers)
    } else {
        CorsLayer::new()
            .allow_origin(AllowOrigin::list(origins))
            .allow_methods(methods)
            .allow_headers(allow_headers)
            .allow_credentials(true)
    }
}

fn build_router(state: ApiState) -> Router {
    let mode = state.mode;
    Router::new()
        .nest("/api/v1", v1::routes(mode))
        .layer(cors_layer(mode))
        .with_state(state)
}

/// 建 Redis 连接池。失败直接返回 Err,由调用方决定是否 panic。
pub async fn build_redis_pool(url: &str) -> Result<RedisPool, String> {
    let mgr = RedisConnectionManager::new(url).map_err(|e| format!("Redis URL 非法: {e}"))?;
    Pool::builder()
        .max_size(16)
        .build(mgr)
        .await
        .map_err(|e| format!("Redis 连接池构建失败: {e}"))
}

/// 启动 HTTP 服务。
pub async fn serve(
    db: DatabaseConnection,
    addr: SocketAddr,
    mode: ServerMode,
    redis: Option<RedisPool>,
) -> std::io::Result<()> {
    let state = ApiState {
        db,
        jwt_secret: Arc::new(auth::jwt_secret(mode)),
        mode,
        redis,
        ws_hub: Arc::new(ws_hub::WsHub::new()),
    };
    let app = build_router(state);
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!("HTTP API 已启动: http://{addr}/api/v1 (mode={:?})", mode);
    // into_make_service_with_connect_info:让 handler 能通过 ConnectInfo<SocketAddr> 拿客户端 IP
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await
}
