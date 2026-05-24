//! 对外 HTTP API(Axum)。版本化路由 `/api/v1`、统一响应结构、JWT 鉴权。
//! 复用全局 SeaORM 连接,随 Tauri 进程启动,也可抽出独立部署。

mod auth;
mod error;
mod response;
mod v1;

pub use error::AppError;
pub use response::ApiResponse;

use axum::Router;
use sea_orm::DatabaseConnection;
use std::net::SocketAddr;
use std::sync::Arc;
use tower_http::cors::CorsLayer;

/// API 共享状态:数据库连接(SeaORM 连接 clone 廉价,内部 Arc) + JWT 密钥。
#[derive(Clone)]
pub struct ApiState {
    pub db: DatabaseConnection,
    pub jwt_secret: Arc<Vec<u8>>,
}

/// 组装路由:业务接口挂在 `/api/v1` 下,后续加版本直接再 `nest("/api/v2", ...)`。
fn build_router(state: ApiState) -> Router {
    Router::new()
        .nest("/api/v1", v1::routes())
        // 开发期放开跨域,便于前端(vite)直接联调;生产应收紧来源
        .layer(CorsLayer::permissive())
        .with_state(state)
}

/// 启动 HTTP 服务。在 Tauri setup 中 spawn 调用,失败仅告警不阻塞主程序。
pub async fn serve(db: DatabaseConnection, addr: SocketAddr) -> std::io::Result<()> {
    let state = ApiState {
        db,
        jwt_secret: Arc::new(auth::jwt_secret()),
    };
    let app = build_router(state);
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!("HTTP API 已启动: http://{addr}/api/v1");
    axum::serve(listener, app).await
}
