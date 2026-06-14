//! API v1 路由与示例 handler。新增业务接口在 `routes()` 内追加。
//!
//! Cloud / Desktop 模式共享 /health /auth/login /stats;/pair 和 /devices 仅 Cloud 装配。

use argon2::password_hash::{PasswordHash, PasswordVerifier};
use argon2::Argon2;
use axum::{
    Json, Router,
    extract::{ConnectInfo, State},
    http::HeaderMap,
    routing::{get, post},
};
use bb8_redis::redis::AsyncCommands;
use sea_orm::{ColumnTrait, EntityTrait, PaginatorTrait, QueryFilter};
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;

use super::auth::{AuthUser, encode_user_token};
use super::{ApiResponse, ApiState, AppError, ServerMode, commands, devices, pair, ws};
use crate::db::entity;

/// 登录限流:同 IP 5 分钟内失败 ≥ N 次拒绝。Cloud 模式计数存 Redis,
/// Desktop 模式(无 Redis)退化为进程内计数,防止本机 8787 端口被暴破。
const LOGIN_MAX_FAILS: u32 = 8;
const LOGIN_RATE_WINDOW_SECS: u64 = 300;

/// Desktop 模式的进程内登录失败计数:ip → (失败次数, 窗口起点)。
static LOCAL_LOGIN_FAILS: std::sync::OnceLock<
    std::sync::Mutex<std::collections::HashMap<String, (u32, std::time::Instant)>>,
> = std::sync::OnceLock::new();

fn local_login_fails(
) -> &'static std::sync::Mutex<std::collections::HashMap<String, (u32, std::time::Instant)>> {
    LOCAL_LOGIN_FAILS.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

/// 进程内限频:窗口内失败次数达上限即拒。顺带清理过期条目,防止表无限增长。
fn local_rate_limited(ip: &str) -> bool {
    let Ok(mut map) = local_login_fails().lock() else {
        return false;
    };
    let window = std::time::Duration::from_secs(LOGIN_RATE_WINDOW_SECS);
    map.retain(|_, (_, start)| start.elapsed() < window);
    map.get(ip)
        .map(|(fails, _)| *fails >= LOGIN_MAX_FAILS)
        .unwrap_or(false)
}

fn local_record_fail(ip: &str) {
    if let Ok(mut map) = local_login_fails().lock() {
        let entry = map
            .entry(ip.to_string())
            .or_insert((0, std::time::Instant::now()));
        entry.0 += 1;
    }
}

fn local_clear_fails(ip: &str) {
    if let Ok(mut map) = local_login_fails().lock() {
        map.remove(ip);
    }
}

fn client_ip(headers: &HeaderMap, fallback: SocketAddr) -> String {
    headers
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.split(',').next())
        .map(|s| s.trim().to_string())
        .or_else(|| {
            headers
                .get("x-real-ip")
                .and_then(|v| v.to_str().ok())
                .map(|s| s.to_string())
        })
        .unwrap_or_else(|| fallback.ip().to_string())
}

pub fn routes(mode: ServerMode) -> Router<ApiState> {
    let mut r = Router::new()
        .route("/health", get(health))
        .route("/auth/login", post(login))
        .route("/stats", get(stats));

    // 云端中转才暴露配对与设备状态接口;桌面端不挂(避免本地 127.0.0.1 也意外开放)
    if mode == ServerMode::Cloud {
        r = r
            .merge(pair::routes())
            .merge(devices::routes())
            .merge(commands::routes())
            .merge(ws::routes());
    }
    r
}

#[derive(Serialize)]
struct Health {
    status: &'static str,
    version: &'static str,
}

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

/// 登录:按 username 查 users,argon2 校验密码,成功后签发带 data_scope 的 token。
/// 失败错误信息统一为「用户名或密码错误」,避免用户枚举。
/// IP 限频:5 分钟内失败 ≥ 8 次直接拒(Cloud 计数存 Redis,Desktop 进程内计数)。
async fn login(
    State(state): State<ApiState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<LoginReq>,
) -> Result<ApiResponse<LoginResp>, AppError> {
    let username = req.username.trim();
    if username.is_empty() || req.password.is_empty() {
        return Err(AppError::Unauthorized("用户名或密码错误".into()));
    }

    // 先查 IP 失败计数:Cloud 走 Redis,Desktop 走进程内表
    let ip = client_ip(&headers, addr);
    let fail_key = format!("login:fail:{ip}");
    if let Some(pool) = state.redis.as_ref() {
        if let Ok(mut conn) = pool.get().await {
            let fails: u32 = conn
                .get::<_, Option<u32>>(&fail_key)
                .await
                .unwrap_or(None)
                .unwrap_or(0);
            if fails >= LOGIN_MAX_FAILS {
                return Err(AppError::Unauthorized(
                    "尝试次数过多,请稍后再试".into(),
                ));
            }
        }
    } else if local_rate_limited(&ip) {
        return Err(AppError::Unauthorized("尝试次数过多,请稍后再试".into()));
    }

    // 统一失败处理:失败计数自增 + 返回 401
    let auth_fail = || async {
        if let Some(pool) = state.redis.as_ref() {
            if let Ok(mut conn) = pool.get().await {
                if let Ok(new_count) = conn.incr::<_, _, u32>(&fail_key, 1u32).await {
                    if new_count == 1 {
                        let _: Result<(), _> =
                            conn.expire(&fail_key, LOGIN_RATE_WINDOW_SECS as i64).await;
                    }
                }
            }
        } else {
            local_record_fail(&ip);
        }
        AppError::Unauthorized("用户名或密码错误".into())
    };

    let model = match entity::user::Entity::find()
        .filter(entity::user::Column::Username.eq(username))
        .filter(entity::user::Column::DeletedAt.eq(0))
        .one(&state.db)
        .await
        .map_err(|e| AppError::Internal(format!("查询用户失败: {e}")))?
    {
        Some(m) if m.status == "enabled" => m,
        _ => return Err(auth_fail().await),
    };

    let parsed = match PasswordHash::new(&model.password_hash) {
        Ok(p) => p,
        Err(_) => return Err(auth_fail().await),
    };
    if Argon2::default()
        .verify_password(req.password.as_bytes(), &parsed)
        .is_err()
    {
        return Err(auth_fail().await);
    }

    // 登录成功 → 清失败计数
    if let Some(pool) = state.redis.as_ref() {
        if let Ok(mut conn) = pool.get().await {
            let _: Result<(), _> = conn.del(&fail_key).await;
        }
    } else {
        local_clear_fails(&ip);
    }

    let token = encode_user_token(&state.jwt_secret, &model.username, &model.data_scope)?;
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
