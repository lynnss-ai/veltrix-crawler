//! 远程指令入口(REST):手机 POST 指令 → 云端通过 PC WS 下发 → PC ack 回传走 WS。
//!
//! 指令本身是无副作用的 RPC 入口;真正的执行由 PC 端处理 WS command 消息时完成。

use axum::{Json, Router, extract::State, routing::post};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::auth::AuthMobile;
use super::{ApiResponse, ApiState, AppError};

pub fn routes() -> Router<ApiState> {
    Router::new().route("/devices/me/commands", post(submit_command))
}

#[derive(Deserialize)]
struct CommandReq {
    /// 指令类型:pause_task / resume_task / relogin_account / restart_engine 等(PC 端约定)
    action: String,
    /// 任意参数透传给 PC,例:{ "task_id": "..." } 或 { "account_id": "..." }
    #[serde(default)]
    params: Value,
}

#[derive(Serialize)]
struct CommandResp {
    /// 指令 id;PC ack 时会带回,手机端可用 WS 的 command_ack 消息按 id 匹配结果
    id: String,
    /// 是否已成功下发到 PC(true 不代表执行成功,只代表已进入 PC 进程)
    dispatched: bool,
}

async fn submit_command(
    AuthMobile(m): AuthMobile,
    State(state): State<ApiState>,
    Json(req): Json<CommandReq>,
) -> Result<ApiResponse<CommandResp>, AppError> {
    let action = req.action.trim();
    if action.is_empty() {
        return Err(AppError::BadRequest("action 不能为空".into()));
    }

    let id = uuid::Uuid::new_v4().to_string();
    let payload = json!({
        "type": "command",
        "id": id,
        "action": action,
        "params": req.params,
    });

    // 直接通过 WS hub 下发;PC 不在线直接告诉手机
    let dispatched = state.ws_hub.send_to_pc(&m.device_id, payload);
    if !dispatched {
        return Err(AppError::BadRequest("PC 当前离线,无法下发指令".into()));
    }

    Ok(ApiResponse::ok(CommandResp { id, dispatched }))
}
