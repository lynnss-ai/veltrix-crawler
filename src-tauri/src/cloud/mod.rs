//! 桌面端云端连接模块:配对、长连接、状态上报、远程指令执行。
//!
//! 流程总览:
//! 1. 用户在「远程连接」页配置云端地址 → cloud_save_base_url
//! 2. cloud_login(账号密码) → 拿到 user_token
//! 3. cloud_pair_init → 拿到 pair_code(显示二维码 + 连接码文本)、pc_token 自动保存
//! 4. CloudClient::run_loop 自动用 pc_token 建立 WS,失败指数退避重连
//! 5. WS 在线后:定时 status 上报 + 接收 command 经 ExecutorRegistry 分发 + 回 ack

mod client;
mod config;
mod executor;
mod status;

pub use client::{CloudClient, ConnectionState, PairResult};
pub use config::CloudConfig;
