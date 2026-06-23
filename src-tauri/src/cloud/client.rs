//! HTTP/WebSocket 客户端实现。
//!
//! 核心循环 `run_loop`:
//! 1. 检查 pc_token 是否就绪(未配对则阻塞等待外部触发)
//! 2. 建 WS 连接 → 启动 read/write/heartbeat 三任务
//! 3. 任一任务退出 → 断开 → 指数退避 → 回到步骤 1

use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Mutex, RwLock, mpsc, watch};
use tokio_tungstenite::{
    connect_async,
    tungstenite::{Message, client::IntoClientRequest},
};

use super::config::CloudConfig;
use super::executor::ExecutorRegistry;
use super::status::StatusSnapshot;

const REPORT_INTERVAL_SECS: u64 = 30;
const PING_INTERVAL_SECS: u64 = 25;
const RECONNECT_MIN_SECS: u64 = 1;
const RECONNECT_MAX_SECS: u64 = 30;
/// 云端 HTTP(登录/配对)总超时:防止后端不通时永久挂起。
const CLOUD_HTTP_TIMEOUT_SECS: u64 = 20;

/// 带超时的 HTTP 客户端(登录/配对用);构建失败回退默认 client(默认即无超时,极端兜底)。
fn cloud_http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(CLOUD_HTTP_TIMEOUT_SECS))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new())
}

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct ConnectionState {
    pub connected: bool,
    pub paired: bool,
    pub last_report_at: Option<i64>,
    pub last_error: Option<String>,
}

#[derive(Clone)]
pub struct CloudClient {
    config_dir: std::path::PathBuf,
    cfg: Arc<RwLock<CloudConfig>>,
    state: Arc<RwLock<ConnectionState>>,
    /// 写 WS 用的 mpsc;无连接时 send 返回 Err
    ws_tx: Arc<Mutex<Option<mpsc::UnboundedSender<Value>>>>,
    executor: ExecutorRegistry,
    /// 唤醒 run_loop 重新检查配置/token(配对完成、配置变更时触发)
    wake_tx: watch::Sender<u64>,
}

impl CloudClient {
    pub fn new(config_dir: std::path::PathBuf) -> Self {
        let cfg = CloudConfig::load_or_default(&config_dir);
        let paired = cfg.pc_token.is_some();
        let (wake_tx, _wake_rx) = watch::channel(0u64);
        Self {
            config_dir,
            cfg: Arc::new(RwLock::new(cfg)),
            state: Arc::new(RwLock::new(ConnectionState {
                paired,
                ..Default::default()
            })),
            ws_tx: Arc::new(Mutex::new(None)),
            executor: ExecutorRegistry::with_defaults(),
            wake_tx,
        }
    }

    pub async fn get_config(&self) -> CloudConfig {
        self.cfg.read().await.clone()
    }

    pub async fn get_state(&self) -> ConnectionState {
        self.state.read().await.clone()
    }

    /// 更新基础地址(用户在设置页配置),持久化并唤醒 run_loop
    pub async fn set_base_url(&self, url: String) -> std::io::Result<()> {
        {
            let mut cfg = self.cfg.write().await;
            cfg.base_url = url;
            cfg.save(&self.config_dir)?;
        }
        let _ = self.wake_tx.send(self.wake_tx.borrow().wrapping_add(1));
        Ok(())
    }

    /// 调云端 /auth/login,成功后保存 user_token
    pub async fn login(&self, username: &str, password: &str) -> Result<(), String> {
        let base = self.cfg.read().await.base_url.clone();
        if base.is_empty() {
            return Err("尚未配置云端地址".into());
        }
        let url = format!("{}/api/v1/auth/login", base.trim_end_matches('/'));
        let body = json!({ "username": username, "password": password });
        let resp = cloud_http_client()
            .post(&url)
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("请求失败: {e}"))?;
        let json: Value = resp.json().await.map_err(|e| format!("响应解析失败: {e}"))?;
        let token = json
            .get("data")
            .and_then(|d| d.get("token"))
            .and_then(|t| t.as_str())
            .ok_or_else(|| format!("云端响应不含 token: {json}"))?
            .to_string();

        let mut cfg = self.cfg.write().await;
        cfg.user_token = Some(token);
        cfg.save(&self.config_dir).map_err(|e| e.to_string())?;
        Ok(())
    }

    /// 调云端 /pair/init,返回配对码 + 自动保存返回的 pc_token
    pub async fn pair_init(&self) -> Result<PairResult, String> {
        let (base, user_token, device_id) = {
            let cfg = self.cfg.read().await;
            (
                cfg.base_url.clone(),
                cfg.user_token.clone(),
                cfg.device_id.clone(),
            )
        };
        if base.is_empty() {
            return Err("尚未配置云端地址".into());
        }
        let token = user_token.ok_or_else(|| "尚未登录云端".to_string())?;
        let url = format!("{}/api/v1/pair/init", base.trim_end_matches('/'));
        let resp = cloud_http_client()
            .post(&url)
            .bearer_auth(&token)
            .json(&json!({ "device_id": device_id }))
            .send()
            .await
            .map_err(|e| format!("请求失败: {e}"))?;
        let json: Value = resp.json().await.map_err(|e| format!("响应解析失败: {e}"))?;
        let data = json
            .get("data")
            .ok_or_else(|| format!("云端响应无 data: {json}"))?;
        let code = data
            .get("code")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "响应缺 code".to_string())?
            .to_string();
        let expires_in = data
            .get("expires_in")
            .and_then(|v| v.as_u64())
            .unwrap_or(300);
        let pc_token = data
            .get("pc_token")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "响应缺 pc_token".to_string())?
            .to_string();

        {
            let mut cfg = self.cfg.write().await;
            cfg.pc_token = Some(pc_token);
            cfg.save(&self.config_dir).map_err(|e| e.to_string())?;
        }
        {
            let mut st = self.state.write().await;
            st.paired = true;
        }
        // 唤醒 run_loop 让它尝试用新 token 建立 WS
        let _ = self.wake_tx.send(self.wake_tx.borrow().wrapping_add(1));

        Ok(PairResult { code, expires_in })
    }

    /// 清理凭证 + 关闭 WS。常用于「重新配对」场景
    pub async fn disconnect(&self) -> std::io::Result<()> {
        {
            let mut cfg = self.cfg.write().await;
            cfg.pc_token = None;
            cfg.save(&self.config_dir)?;
        }
        {
            let mut st = self.state.write().await;
            st.paired = false;
            st.connected = false;
        }
        // 关闭当前 ws_tx;writer 任务收到 None 后退出,触发主循环重新进入等待
        *self.ws_tx.lock().await = None;
        let _ = self.wake_tx.send(self.wake_tx.borrow().wrapping_add(1));
        Ok(())
    }

    /// 主循环:tokio::spawn 在 setup 里调用一次,长期运行
    pub async fn run_loop(self: Arc<Self>) {
        let mut backoff = RECONNECT_MIN_SECS;
        let mut wake_rx = self.wake_tx.subscribe();
        loop {
            // 取必要配置
            let (base_url, pc_token) = {
                let cfg = self.cfg.read().await;
                (cfg.base_url.clone(), cfg.pc_token.clone())
            };

            // 未配对或未配置 → 等待 wake 信号或定期重试
            if base_url.is_empty() || pc_token.is_none() {
                tokio::select! {
                    _ = wake_rx.changed() => {},
                    _ = tokio::time::sleep(Duration::from_secs(10)) => {},
                }
                continue;
            }

            match self.connect_and_run(&base_url, pc_token.as_ref().unwrap()).await {
                Ok(_) => {
                    backoff = RECONNECT_MIN_SECS;
                }
                Err(e) => {
                    tracing::warn!("云端 WS 连接异常: {e}");
                    self.state.write().await.last_error = Some(e);
                    backoff = (backoff * 2).min(RECONNECT_MAX_SECS);
                }
            }
            self.state.write().await.connected = false;
            // 等退避超时或被唤醒;被唤醒时立即重试,常见于刚 pair_init 完成
            tokio::select! {
                _ = wake_rx.changed() => {},
                _ = tokio::time::sleep(Duration::from_secs(backoff)) => {},
            }
        }
    }

    /// 单次 WS 会话:建连 + 跑读/写/心跳/上报四组任务,任一返回即整体退出
    async fn connect_and_run(&self, base_url: &str, pc_token: &str) -> Result<(), String> {
        let ws_url = build_ws_url(base_url, "/api/v1/ws/pc")?;
        let mut req = ws_url
            .into_client_request()
            .map_err(|e| format!("构造 WS 请求失败: {e}"))?;
        req.headers_mut().insert(
            "Authorization",
            format!("Bearer {pc_token}")
                .parse()
                .map_err(|e| format!("Authorization 头非法: {e}"))?,
        );

        let (ws, _) = connect_async(req)
            .await
            .map_err(|e| format!("WS 握手失败: {e}"))?;
        let (mut sink, mut stream) = ws.split();

        let (tx, mut rx) = mpsc::unbounded_channel::<Value>();
        *self.ws_tx.lock().await = Some(tx.clone());
        self.state.write().await.connected = true;
        tracing::info!("云端 WS 已连接: {base_url}");

        let executor = self.executor.clone();
        let state_for_read = self.state.clone();
        let tx_for_read = tx.clone();

        // 读任务:处理云端下行(主要是 command)
        let reader = tokio::spawn(async move {
            while let Some(Ok(msg)) = stream.next().await {
                match msg {
                    Message::Text(text) => {
                        let value: Value = match serde_json::from_str(&text) {
                            Ok(v) => v,
                            Err(_) => continue,
                        };
                        let kind = value
                            .get("type")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        match kind.as_str() {
                            "command" => {
                                let id =
                                    value.get("id").cloned().unwrap_or(Value::Null);
                                let action = value
                                    .get("action")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("")
                                    .to_string();
                                let params =
                                    value.get("params").cloned().unwrap_or(Value::Null);
                                let result = executor.dispatch(&action, params).await;
                                let _ = tx_for_read.send(json!({
                                    "type": "ack",
                                    "id": id,
                                    "ok": result.ok,
                                    "error": result.error,
                                }));
                            }
                            "pong" => {}
                            other => {
                                tracing::debug!("云端下行未知消息: {other}");
                            }
                        }
                    }
                    Message::Close(_) => break,
                    _ => {}
                }
            }
            state_for_read.write().await.connected = false;
        });

        // 写任务:消费 mpsc 推给云端
        let writer = tokio::spawn(async move {
            while let Some(v) = rx.recv().await {
                let text = match serde_json::to_string(&v) {
                    Ok(s) => s,
                    Err(_) => continue,
                };
                if sink.send(Message::Text(text.into())).await.is_err() {
                    break;
                }
            }
            let _ = sink.close().await;
        });

        // 状态上报任务
        let tx_report = tx.clone();
        let state_report = self.state.clone();
        let reporter = tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(REPORT_INTERVAL_SECS));
            loop {
                tick.tick().await;
                let snap = StatusSnapshot::snapshot();
                if tx_report
                    .send(json!({ "type": "status", "payload": snap }))
                    .is_err()
                {
                    break;
                }
                state_report.write().await.last_report_at = Some(chrono::Utc::now().timestamp());
            }
        });

        // 心跳
        let tx_ping = tx.clone();
        let heartbeat = tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(PING_INTERVAL_SECS));
            loop {
                tick.tick().await;
                if tx_ping.send(json!({ "type": "ping" })).is_err() {
                    break;
                }
            }
        });

        // 任一任务退出 → 显式 abort 其余三个,避免成为孤儿任务继续跑(reporter/heartbeat 是 interval 循环)
        let aborts = [
            reader.abort_handle(),
            writer.abort_handle(),
            reporter.abort_handle(),
            heartbeat.abort_handle(),
        ];
        tokio::select! {
            _ = reader => {},
            _ = writer => {},
            _ = reporter => {},
            _ = heartbeat => {},
        }
        for h in &aborts {
            h.abort();
        }

        *self.ws_tx.lock().await = None;
        self.state.write().await.connected = false;
        tracing::info!("云端 WS 会话结束");
        Ok(())
    }
}

#[derive(Debug, serde::Serialize)]
pub struct PairResult {
    pub code: String,
    pub expires_in: u64,
}

fn build_ws_url(base: &str, path: &str) -> Result<String, String> {
    let trimmed = base.trim_end_matches('/');
    if let Some(rest) = trimmed.strip_prefix("https://") {
        Ok(format!("wss://{rest}{path}"))
    } else if let Some(rest) = trimmed.strip_prefix("http://") {
        Ok(format!("ws://{rest}{path}"))
    } else {
        Err(format!("base_url 必须以 http(s):// 开头: {base}"))
    }
}
