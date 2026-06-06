//! WebSocket 端点:/ws/pc(PC 长连接)、/ws/mobile(手机长连接)。
//!
//! 鉴权直接复用 PcClaims / MobileClaims 提取器(WS 升级前由 axum 跑提取器,失败返回 401)。
//! WS 内消息走 JSON 文本帧,协议见 api/mod.rs 顶部注释。

use axum::{
    Router,
    extract::{
        State,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    response::Response,
    routing::get,
};
use bb8_redis::redis::AsyncCommands;
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};

use super::auth::{AuthMobile, AuthPc};
use super::{ApiState};

pub fn routes() -> Router<ApiState> {
    Router::new()
        .route("/ws/pc", get(ws_pc))
        .route("/ws/mobile", get(ws_mobile))
}

// ---------------- PC 端 ----------------

async fn ws_pc(
    AuthPc(pc): AuthPc,
    State(state): State<ApiState>,
    ws: WebSocketUpgrade,
) -> Response {
    let device_id = pc.sub.clone();
    ws.on_upgrade(move |socket| handle_pc_socket(state, device_id, socket))
}

async fn handle_pc_socket(state: ApiState, device_id: String, socket: WebSocket) {
    tracing::info!(device_id, "PC WS 已连接");
    let hub = state.ws_hub.clone();
    let mut rx = hub.register_pc(&device_id);
    let (mut sink, mut stream) = socket.split();

    // 写循环:把 hub 投递的 Outbound 转 JSON 发给 PC
    let device_id_writer = device_id.clone();
    let writer = tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            let text = match serde_json::to_string(&msg) {
                Ok(s) => s,
                Err(e) => {
                    tracing::warn!(device_id = device_id_writer, "序列化下发消息失败: {e}");
                    continue;
                }
            };
            if sink.send(Message::Text(text.into())).await.is_err() {
                break;
            }
        }
        let _ = sink.close().await;
    });

    // 读循环:处理 PC 上行消息
    while let Some(Ok(msg)) = stream.next().await {
        match msg {
            Message::Text(text) => {
                let value: Value = match serde_json::from_str(&text) {
                    Ok(v) => v,
                    Err(e) => {
                        tracing::warn!(device_id, "PC 上行非法 JSON: {e}");
                        continue;
                    }
                };
                handle_pc_inbound(&state, &device_id, value).await;
            }
            Message::Close(_) => break,
            Message::Ping(_) | Message::Pong(_) | Message::Binary(_) => {}
        }
    }

    hub.unregister_pc(&device_id);
    writer.abort();
    tracing::info!(device_id, "PC WS 已断开");

    // 通知绑定的手机:PC 离线
    hub.broadcast_to_mobiles(&device_id, json!({ "type": "device_offline" }));
}

async fn handle_pc_inbound(state: &ApiState, device_id: &str, value: Value) {
    let kind = value.get("type").and_then(|v| v.as_str()).unwrap_or("");
    match kind {
        // 实时状态推送 — 同时写 Redis(给离线手机轮询用)+ 广播到在线手机
        "status" => {
            if let Some(payload) = value.get("payload") {
                if let Some(redis) = state.redis.as_ref() {
                    if let Ok(mut conn) = redis.get().await {
                        let key = format!("device:status:{device_id}");
                        let json = payload.to_string();
                        let _: Result<(), _> = conn.set_ex(&key, &json, 600).await;
                    }
                }
                state.ws_hub.broadcast_to_mobiles(
                    device_id,
                    json!({ "type": "status", "payload": payload }),
                );
            }
        }
        // 指令执行结果回传 → 转发给手机
        "ack" => {
            let id = value.get("id").cloned().unwrap_or(Value::Null);
            let ok = value.get("ok").cloned().unwrap_or(Value::Bool(false));
            let error = value.get("error").cloned().unwrap_or(Value::Null);
            state.ws_hub.broadcast_to_mobiles(
                device_id,
                json!({ "type": "command_ack", "id": id, "ok": ok, "error": error }),
            );
        }
        // PC 主动上报风控/账号失效等事件 → 通知手机
        "notify" => {
            state.ws_hub.broadcast_to_mobiles(device_id, value);
        }
        "ping" => {
            // 让 hub 给自己发 pong;直接走 PC 的 tx
            state
                .ws_hub
                .send_to_pc(device_id, json!({ "type": "pong" }));
        }
        other => {
            tracing::debug!(device_id, "PC 未知消息类型: {other}");
        }
    }
}

// ---------------- 手机端 ----------------

async fn ws_mobile(
    AuthMobile(m): AuthMobile,
    State(state): State<ApiState>,
    ws: WebSocketUpgrade,
) -> Response {
    let pair_id = m.pair_id.clone();
    let device_id = m.device_id.clone();
    ws.on_upgrade(move |socket| handle_mobile_socket(state, pair_id, device_id, socket))
}

async fn handle_mobile_socket(
    state: ApiState,
    pair_id: String,
    device_id: String,
    socket: WebSocket,
) {
    tracing::info!(pair_id, device_id, "Mobile WS 已连接");
    let hub = state.ws_hub.clone();
    let mut rx = hub.register_mobile(&pair_id, &device_id);
    let (mut sink, mut stream) = socket.split();

    // 进门先送一次当前在线状态,免得手机等下一次心跳才知道
    let initial = json!({
        "type": "presence",
        "pc_online": hub.pc_online(&device_id),
    });
    let _ = sink
        .send(Message::Text(initial.to_string().into()))
        .await;

    let writer = tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            let text = match serde_json::to_string(&msg) {
                Ok(s) => s,
                Err(_) => continue,
            };
            if sink.send(Message::Text(text.into())).await.is_err() {
                break;
            }
        }
        let _ = sink.close().await;
    });

    while let Some(Ok(msg)) = stream.next().await {
        match msg {
            Message::Text(_) => {
                // 手机端目前只发 ping,不解析具体内容
            }
            Message::Close(_) => break,
            _ => {}
        }
    }

    hub.unregister_mobile(&pair_id);
    writer.abort();
    tracing::info!(pair_id, "Mobile WS 已断开");
}
