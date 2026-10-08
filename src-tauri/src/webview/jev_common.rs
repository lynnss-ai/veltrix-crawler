//! Jev 模块共用的密钥读取。缺失时进程内告警一次:
//! 没有这条日志,「未触发兜底」与「触发但密钥缺失被跳过」在日志上无法区分。

use std::sync::OnceLock;

/// 读取 `TYPESAFE_API_KEY`;未设置或为空时返回 None,并在本进程首次遇到时告警
/// (环境变量在应用启动时快照,启动后再 setx 不会生效,需重启应用)。
pub fn load_api_key() -> Option<String> {
    static MISSING_WARNED: OnceLock<()> = OnceLock::new();
    match std::env::var("TYPESAFE_API_KEY") {
        Ok(value) if !value.trim().is_empty() => Some(value),
        _ => {
            if MISSING_WARNED.set(()).is_ok() {
                tracing::warn!(
                    "TYPESAFE_API_KEY 未设置或为空,Jev 兜底(筛选/搜索定位、失败重试判断)已跳过;设置后需重启应用生效"
                );
            }
            None
        }
    }
}
