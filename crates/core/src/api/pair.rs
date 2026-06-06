//! 设备配对:PC 申请配对码 → 手机扫码 → 云端签发手机 token。
//!
//! Redis 数据布局:
//! - pair:code:{code}       → JSON{ device_id, user, created_at }, TTL=300s
//! - device:bind:{device_id} → pair_id(单值,覆盖即踢旧手机)

use axum::{
    Json, Router,
    extract::{ConnectInfo, State},
    http::HeaderMap,
    routing::post,
};
use bb8_redis::redis::AsyncCommands;
use rand::Rng;
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;

use super::auth::{AuthUser, encode_mobile_token, encode_pc_token};
use super::{ApiResponse, ApiState, AppError};

/// 配对码有效期(秒)
const PAIR_CODE_TTL_SECS: u64 = 300;
/// 设备绑定关系 TTL(秒):30 天;手机端正常使用会被滑动续期
const DEVICE_BIND_TTL_SECS: u64 = 30 * 24 * 3600;
/// 每 IP 在 RATE_LIMIT_WINDOW_SECS 窗口内允许的 confirm 失败次数,超过则锁定
const CONFIRM_MAX_FAILS: u32 = 8;
const RATE_LIMIT_WINDOW_SECS: u64 = 300;

/// 从代理 / 直连请求中解析客户端 IP,做 rate-limit key
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

pub fn routes() -> Router<ApiState> {
    Router::new()
        .route("/pair/init", post(pair_init))
        .route("/pair/confirm", post(pair_confirm))
}

#[derive(Deserialize)]
struct PairInitReq {
    /// PC 自报的设备 id(首次启动生成的稳定 UUID,持久化在桌面端)
    device_id: String,
}

#[derive(Serialize)]
struct PairInitResp {
    code: String,
    expires_in: u64,
    /// PC 拿到这个 PC token 作为后续上报的凭证 — 一发完成,不等手机扫码
    /// (允许 PC 在手机绑定前就开始上报,云端先存着等查询)
    pc_token: String,
}

#[derive(Serialize, Deserialize)]
struct PairCodePayload {
    device_id: String,
    user: String,
    created_at: i64,
}

fn gen_pair_code() -> String {
    let n: u32 = rand::thread_rng().gen_range(0..1_000_000);
    format!("{:06}", n)
}

async fn redis_conn(state: &ApiState) -> Result<bb8::PooledConnection<'_, bb8_redis::RedisConnectionManager>, AppError> {
    let pool = state
        .redis
        .as_ref()
        .ok_or_else(|| AppError::Internal("当前部署模式未启用 Redis".into()))?;
    pool.get()
        .await
        .map_err(|e| AppError::Internal(format!("Redis 连接获取失败: {e}")))
}

/// PC 调:发起配对,云端生成 6 位短码 + 直接签发 PC token。
async fn pair_init(
    AuthUser(user): AuthUser,
    State(state): State<ApiState>,
    Json(req): Json<PairInitReq>,
) -> Result<ApiResponse<PairInitResp>, AppError> {
    let device_id = req.device_id.trim();
    if device_id.is_empty() {
        return Err(AppError::BadRequest("device_id 不能为空".into()));
    }

    let code = gen_pair_code();
    let payload = PairCodePayload {
        device_id: device_id.to_string(),
        user: user.sub.clone(),
        created_at: chrono::Utc::now().timestamp(),
    };
    let json = serde_json::to_string(&payload)
        .map_err(|e| AppError::Internal(format!("序列化配对载荷失败: {e}")))?;

    let mut conn = redis_conn(&state).await?;
    let key = format!("pair:code:{code}");
    // SETEX:简单覆盖即可,极小概率撞码也只是踢掉前一个码
    let _: () = conn
        .set_ex(&key, &json, PAIR_CODE_TTL_SECS)
        .await
        .map_err(|e| AppError::Internal(format!("Redis 写入配对码失败: {e}")))?;

    let pc_token = encode_pc_token(&state.jwt_secret, device_id, &user.sub)?;
    Ok(ApiResponse::ok(PairInitResp {
        code,
        expires_in: PAIR_CODE_TTL_SECS,
        pc_token,
    }))
}

#[derive(Deserialize)]
struct PairConfirmReq {
    code: String,
    /// 手机端自报的用户标识(MVP 直接接受,后续接入手机账号体系)
    mobile_user: String,
}

#[derive(Serialize)]
struct PairConfirmResp {
    device_id: String,
    mobile_token: String,
}

/// 手机调:扫码确认。
/// - IP 限频:同一 IP 5 分钟内失败 > N 次直接拒(防 6 位码暴力)
/// - 配对码一次性消费
/// - 设备绑定关系写入时带 TTL(30 天),手机端每次使用 token 时由 AuthMobile 提取器滑动续期
async fn pair_confirm(
    State(state): State<ApiState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<PairConfirmReq>,
) -> Result<ApiResponse<PairConfirmResp>, AppError> {
    let code = req.code.trim();
    let mobile_user = req.mobile_user.trim();
    if code.is_empty() || mobile_user.is_empty() {
        return Err(AppError::BadRequest("code / mobile_user 不能为空".into()));
    }

    let ip = client_ip(&headers, addr);
    let mut conn = redis_conn(&state).await?;

    // IP 失败次数检查
    let fail_key = format!("pair:fail:{ip}");
    let fails: u32 = conn
        .get::<_, Option<u32>>(&fail_key)
        .await
        .map_err(|e| AppError::Internal(format!("Redis 查询失败计数失败: {e}")))?
        .unwrap_or(0);
    if fails >= CONFIRM_MAX_FAILS {
        return Err(AppError::Unauthorized(
            "尝试次数过多,请稍后再试".into(),
        ));
    }

    let code_key = format!("pair:code:{code}");
    let json: Option<String> = conn
        .get(&code_key)
        .await
        .map_err(|e| AppError::Internal(format!("Redis 查询配对码失败: {e}")))?;
    let payload: PairCodePayload = match json {
        Some(s) => serde_json::from_str(&s)
            .map_err(|e| AppError::Internal(format!("解析配对载荷失败: {e}")))?,
        None => {
            // 失败计数 +1,首次写入设 TTL
            let new_count: u32 = conn
                .incr(&fail_key, 1u32)
                .await
                .map_err(|e| AppError::Internal(format!("Redis 自增失败计数失败: {e}")))?;
            if new_count == 1 {
                let _: () = conn
                    .expire(&fail_key, RATE_LIMIT_WINDOW_SECS as i64)
                    .await
                    .map_err(|e| AppError::Internal(format!("Redis 设置 TTL 失败: {e}")))?;
            }
            return Err(AppError::BadRequest("配对码无效或已过期".into()));
        }
    };

    // 一次性消费,防止同一个码被多台手机使用
    let _: () = conn
        .del(&code_key)
        .await
        .map_err(|e| AppError::Internal(format!("Redis 删除配对码失败: {e}")))?;

    let pair_id = uuid::Uuid::new_v4().to_string();
    let bind_key = format!("device:bind:{}", payload.device_id);
    let _: () = conn
        .set_ex(&bind_key, &pair_id, DEVICE_BIND_TTL_SECS)
        .await
        .map_err(|e| AppError::Internal(format!("Redis 写入绑定关系失败: {e}")))?;

    let mobile_token =
        encode_mobile_token(&state.jwt_secret, mobile_user, &payload.device_id, &pair_id)?;
    Ok(ApiResponse::ok(PairConfirmResp {
        device_id: payload.device_id,
        mobile_token,
    }))
}
