//! 云端连接相关 IPC 命令。
//!
//! 前端「远程连接」页通过这些命令完成:配置云端地址、登录、发起配对、查询连接态、断开。

use crate::cloud::{CloudConfig, ConnectionState, PairResult};
use crate::commands::AppState;
use tauri::State;

/// 视图层使用的配对结果:加 base_url 让前端拼连接码 / 二维码 payload
#[derive(serde::Serialize)]
pub struct PairView {
    pub code: String,
    pub expires_in: u64,
    /// 二维码内容:`veltrix://pair?base=<base_url>&code=<code>`,手机扫码后解析两段
    pub qr_payload: String,
    /// 手动输入用的连接码:同 code,前端可直接展示
    pub manual_code: String,
    pub base_url: String,
}

#[tauri::command]
pub async fn cloud_get_config(state: State<'_, AppState>) -> Result<CloudConfig, String> {
    Ok(state.cloud.get_config().await)
}

#[tauri::command]
pub async fn cloud_get_status(
    state: State<'_, AppState>,
) -> Result<ConnectionState, String> {
    Ok(state.cloud.get_state().await)
}

#[tauri::command]
pub async fn cloud_save_base_url(
    state: State<'_, AppState>,
    base_url: String,
) -> Result<(), String> {
    state
        .cloud
        .set_base_url(base_url)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn cloud_login(
    state: State<'_, AppState>,
    username: String,
    password: String,
) -> Result<(), String> {
    state.cloud.login(&username, &password).await
}

#[tauri::command]
pub async fn cloud_pair_init(state: State<'_, AppState>) -> Result<PairView, String> {
    let cfg = state.cloud.get_config().await;
    let result: PairResult = state.cloud.pair_init().await?;
    let qr_payload = format!(
        "veltrix://pair?base={}&code={}",
        urlencoding_simple(&cfg.base_url),
        result.code
    );
    Ok(PairView {
        manual_code: result.code.clone(),
        qr_payload,
        code: result.code,
        expires_in: result.expires_in,
        base_url: cfg.base_url,
    })
}

#[tauri::command]
pub async fn cloud_disconnect(state: State<'_, AppState>) -> Result<(), String> {
    state.cloud.disconnect().await.map_err(|e| e.to_string())
}

/// 简易 URL encode:只处理 base_url 里可能出现的字符,够二维码 payload 用
fn urlencoding_simple(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            'A'..='Z' | 'a'..='z' | '0'..='9' | '-' | '_' | '.' | '~' | ':' | '/' => c.to_string(),
            other => format!("%{:02X}", other as u32),
        })
        .collect()
}
