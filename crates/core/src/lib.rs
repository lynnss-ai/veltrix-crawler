//! veltrix-core:不依赖 Tauri 的共享后端库。
//!
//! 抽出配置、错误、数据库与对外 HTTP API 四个模块,
//! 既供 Tauri 桌面端复用,也供独立的 veltrix-server 二进制单独部署。

pub mod api;
pub mod config;
pub mod db;
pub mod error;
