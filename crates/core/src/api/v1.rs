//! API v1 路由与示例 handler。新增业务接口在 `routes()` 内追加。

use axum::{
    Json, Router,
    extract::State,
    routing::{get, post},
};
use sea_orm::{EntityTrait, PaginatorTrait};
use serde::{Deserialize, Serialize};

use super::auth::{AuthUser, encode_token};
use super::{ApiResponse, ApiState, AppError};
use crate::db::entity;

pub fn routes() -> Router<ApiState> {
    Router::new()
        // 公开接口
        .route("/health", get(health))
        .route("/auth/login", post(login))
        // 受保护接口:handler 参数带 AuthUser 即需 JWT。后续可:
        //   .nest("/users", users::routes())
        //   .nest("/customers", customers::routes())
        .route("/stats", get(stats))
}

#[derive(Serialize)]
struct Health {
    status: &'static str,
    version: &'static str,
}

/// 健康检查(公开)。
async fn health() -> ApiResponse<Health> {
    ApiResponse::ok(Health {
        status: "ok",
        version: env!("CARGO_PKG_VERSION"),
    })
}

#[derive(Deserialize)]
struct LoginReq {
    username: String,
    password: String,
}

#[derive(Serialize)]
struct LoginResp {
    token: String,
    token_type: &'static str,
}

/// 登录签发 token(占位:接 users 表 + 密码哈希校验后替换,并读取该用户 data_scope)。
async fn login(
    State(state): State<ApiState>,
    Json(req): Json<LoginReq>,
) -> Result<ApiResponse<LoginResp>, AppError> {
    if req.username.trim().is_empty() || req.password.is_empty() {
        return Err(AppError::BadRequest("用户名或密码为空".into()));
    }
    // TODO: 查 users 表校验 password_hash,取出该用户的 data_scope 写入 token
    let token = encode_token(&state.jwt_secret, req.username.trim(), "self")?;
    Ok(ApiResponse::ok(LoginResp {
        token,
        token_type: "Bearer",
    }))
}

#[derive(Serialize)]
struct Stats {
    accounts: u64,
    customers: u64,
    users: u64,
}

/// 受保护示例:AuthUser 自动校验 JWT(失败 401),并用现有 SeaORM 连接统计行数。
async fn stats(
    AuthUser(claims): AuthUser,
    State(state): State<ApiState>,
) -> Result<ApiResponse<Stats>, AppError> {
    tracing::debug!("stats requested by {}", claims.sub);
    let accounts = entity::account::Entity::find().count(&state.db).await?;
    let customers = entity::customer::Entity::find().count(&state.db).await?;
    let users = entity::user::Entity::find().count(&state.db).await?;
    Ok(ApiResponse::ok(Stats {
        accounts,
        customers,
        users,
    }))
}
