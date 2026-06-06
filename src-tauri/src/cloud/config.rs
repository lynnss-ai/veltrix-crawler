//! 云端连接配置:落盘到 config_dir/cloud.json,首次启动自动生成 device_id。
//!
//! 字段分两类:
//! - 用户填:base_url、登录后拿到的 user_token
//! - 自动管:device_id(首次生成)、pc_token(/pair/init 后获得)

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use uuid::Uuid;

const CONFIG_FILE: &str = "cloud.json";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CloudConfig {
    /// 云端基础地址,例如 https://veltrix.example.com 或 http://127.0.0.1:8787
    #[serde(default)]
    pub base_url: String,
    /// 桌面端登录云端后的用户 token(可用于调 /pair/init 等管理接口)
    #[serde(default)]
    pub user_token: Option<String>,
    /// 配对成功后云端签发的 PC token(用于 WS / REST 上报)
    #[serde(default)]
    pub pc_token: Option<String>,
    /// 设备 id,首次启动生成,持久化后绑定该设备
    #[serde(default)]
    pub device_id: String,
}

impl CloudConfig {
    pub fn path(config_dir: &Path) -> PathBuf {
        config_dir.join(CONFIG_FILE)
    }

    /// 读取配置;不存在时返回带新 device_id 的空配置,但不写盘(交给上层在改动时统一保存)
    pub fn load_or_default(config_dir: &Path) -> Self {
        let p = Self::path(config_dir);
        if let Ok(text) = std::fs::read_to_string(&p) {
            if let Ok(mut cfg) = serde_json::from_str::<CloudConfig>(&text) {
                if cfg.device_id.trim().is_empty() {
                    cfg.device_id = Uuid::new_v4().to_string();
                }
                return cfg;
            }
        }
        Self {
            base_url: String::new(),
            user_token: None,
            pc_token: None,
            device_id: Uuid::new_v4().to_string(),
        }
    }

    pub fn save(&self, config_dir: &Path) -> std::io::Result<()> {
        let p = Self::path(config_dir);
        let text = serde_json::to_string_pretty(self).unwrap_or_default();
        std::fs::write(&p, text)?;
        // 文件含 user_token / pc_token,缩到当前用户只读写;Windows 无 chmod,跳过
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600));
        }
        Ok(())
    }
}
