//! WebSocket 连接注册表。单节点 MVP 用 DashMap;后续多节点改 Redis pub/sub。
//!
//! 两类连接:
//! - PC     :按 device_id 唯一索引,新连接会顶掉旧连接(同一设备只允许一条)
//! - Mobile :按 pair_id 唯一索引,被踢的旧 pair_id 永远连不上(MobileAuth 已拦)

use dashmap::DashMap;
use serde_json::Value;
use tokio::sync::mpsc::{Sender, channel};

/// 服务端 → 客户端的消息(走 JSON,字段 flatten 即可)
pub type Outbound = Value;

/// 单连接缓冲深度:消费者慢于生产者时,缓冲到此即拒绝(避免 OOM)
const CHANNEL_BUFFER: usize = 64;

pub struct MobileEntry {
    pub device_id: String,
    pub tx: Sender<Outbound>,
}

#[derive(Default)]
pub struct WsHub {
    pcs: DashMap<String, Sender<Outbound>>,
    mobiles: DashMap<String, MobileEntry>,
}

impl WsHub {
    pub fn new() -> Self {
        Self::default()
    }

    /// 注册 PC,返回 receiver。若同 device_id 已有连接,旧连接 channel 关闭(让旧 socket 自然退出)
    pub fn register_pc(
        &self,
        device_id: &str,
    ) -> tokio::sync::mpsc::Receiver<Outbound> {
        let (tx, rx) = channel(CHANNEL_BUFFER);
        if let Some(old) = self.pcs.insert(device_id.to_string(), tx) {
            drop(old);
            tracing::info!(device_id, "PC 重复连接,旧连接已关闭");
        }
        rx
    }

    pub fn unregister_pc(&self, device_id: &str) {
        self.pcs.remove(device_id);
    }

    pub fn register_mobile(
        &self,
        pair_id: &str,
        device_id: &str,
    ) -> tokio::sync::mpsc::Receiver<Outbound> {
        let (tx, rx) = channel(CHANNEL_BUFFER);
        self.mobiles.insert(
            pair_id.to_string(),
            MobileEntry {
                device_id: device_id.to_string(),
                tx,
            },
        );
        rx
    }

    pub fn unregister_mobile(&self, pair_id: &str) {
        self.mobiles.remove(pair_id);
    }

    /// 向指定 PC 发送消息(指令下发)。返回 false 表示 PC 离线或写缓冲已满
    pub fn send_to_pc(&self, device_id: &str, msg: Outbound) -> bool {
        match self.pcs.get(device_id) {
            // 用 try_send 非阻塞;缓冲满表示客户端跟不上,丢弃这条
            Some(entry) => entry.try_send(msg).is_ok(),
            None => false,
        }
    }

    /// 广播给绑定该 device 的所有手机连接(理论上同时最多一台,但保留集合语义)。
    /// 缓冲已满的连接直接踢:让旧 sender 移除,客户端会触发重连。
    pub fn broadcast_to_mobiles(&self, device_id: &str, msg: Outbound) {
        let mut dead: Vec<String> = Vec::new();
        for entry in self.mobiles.iter() {
            if entry.device_id == device_id {
                if let Err(tokio::sync::mpsc::error::TrySendError::Full(_)) =
                    entry.tx.try_send(msg.clone())
                {
                    tracing::warn!(pair_id = entry.key().as_str(), "Mobile 缓冲已满,踢出");
                    dead.push(entry.key().clone());
                }
            }
        }
        for k in dead {
            self.mobiles.remove(&k);
        }
    }

    pub fn pc_online(&self, device_id: &str) -> bool {
        self.pcs.contains_key(device_id)
    }
}
