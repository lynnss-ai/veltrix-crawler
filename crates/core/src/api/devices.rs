//! 设备状态:PC 覆盖式上报,手机查自己绑定的 PC。
//!
//! Redis 数据布局:
//! - device:status:{device_id} → JSON(整张快照), TTL=600s(过期视为 PC 离线)

use axum::{Json, Router, extract::State, routing::{get, post}};
use bb8_redis::redis::AsyncCommands;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::auth::{AuthMobile, AuthPc};
use super::{ApiResponse, ApiState, AppError};

/// 状态过期(秒):超过即视为 PC 离线。
const STATUS_TTL_SECS: u64 = 600;

pub fn routes() -> Router<ApiState> {
    Router::new()
        .route("/devices/status", post(report_status))
        .route("/devices/me/status", get(get_my_status))
}

#[derive(Deserialize, Serialize)]
struct StatusSnapshot {
    /// 当前任务列表:[{ id, name, progress, status }]
    #[serde(default)]
    tasks: Vec<Value>,
    /// 账号状态:[{ id, platform, state: normal|cooling|invalid }]
    #[serde(default)]
    accounts: Vec<Value>,
    #[serde(default)]
    today_collected: u64,
    #[serde(default)]
    risk_count_24h: u64,
    #[serde(default)]
    recent_activities: Vec<String>,
    #[serde(default)]
    last_error: Option<String>,
}

#[derive(Serialize)]
struct ReportResp {
    accepted: bool,
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

/// PC 调:覆盖式上报当前快照。
async fn report_status(
    AuthPc(pc): AuthPc,
    State(state): State<ApiState>,
    Json(snap): Json<StatusSnapshot>,
) -> Result<ApiResponse<ReportResp>, AppError> {
    let snap_json = serde_json::to_string(&snap)
        .map_err(|e| AppError::Internal(format!("序列化状态失败: {e}")))?;
    let mut conn = redis_conn(&state).await?;
    let key = format!("device:status:{}", pc.sub);
    let _: () = conn
        .set_ex(&key, &snap_json, STATUS_TTL_SECS)
        .await
        .map_err(|e| AppError::Internal(format!("Redis 写入状态失败: {e}")))?;

    // 同步广播给在线手机,REST 上报也能立即触达 — PC 未启 WS 时这条路径仍生效
    let payload = serde_json::to_value(&snap).unwrap_or(Value::Null);
    state
        .ws_hub
        .broadcast_to_mobiles(&pc.sub, json!({ "type": "status", "payload": payload }));

    Ok(ApiResponse::ok(ReportResp { accepted: true }))
}

#[derive(Serialize)]
struct StatusView {
    device_id: String,
    online: bool,
    reported_at: Option<i64>,
    /// 直接透传 PC 上报的快照内容
    #[serde(flatten)]
    snapshot: StatusSnapshot,
}

/// 手机调:查自己绑定的 PC 的状态。
async fn get_my_status(
    AuthMobile(m): AuthMobile,
    State(state): State<ApiState>,
) -> Result<ApiResponse<StatusView>, AppError> {
    let mut conn = redis_conn(&state).await?;
    let key = format!("device:status:{}", m.device_id);
    let json: Option<String> = conn
        .get(&key)
        .await
        .map_err(|e| AppError::Internal(format!("Redis 查询状态失败: {e}")))?;

    // 同时拿剩余 TTL 估算 reported_at(写入时 TTL=STATUS_TTL_SECS)
    let ttl: i64 = conn
        .ttl(&key)
        .await
        .map_err(|e| AppError::Internal(format!("Redis 查询 TTL 失败: {e}")))?;

    let view = match json {
        Some(s) => {
            let snap: StatusSnapshot = serde_json::from_str(&s)
                .map_err(|e| AppError::Internal(format!("解析状态失败: {e}")))?;
            let reported_at = if ttl > 0 {
                Some(chrono::Utc::now().timestamp() - (STATUS_TTL_SECS as i64 - ttl))
            } else {
                None
            };
            StatusView {
                device_id: m.device_id.clone(),
                online: true,
                reported_at,
                snapshot: snap,
            }
        }
        None => StatusView {
            device_id: m.device_id.clone(),
            online: false,
            reported_at: None,
            snapshot: StatusSnapshot {
                tasks: vec![],
                accounts: vec![],
                today_collected: 0,
                risk_count_24h: 0,
                recent_activities: vec![],
                last_error: None,
            },
        },
    };
    Ok(ApiResponse::ok(view))
}
