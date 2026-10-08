//! 低配设备识别:为采集窗口数 / 素材下载并发 / 缩略图回填 / HUD 渲染提供统一的降档开关。
//!
//! 弱机上卡顿的根源是多路重负载同跑:每个采集 WebView 都是独立进程树(独立用户数据目录),
//! 加上 10 路素材下载、ffmpeg 转码与缩略图编码,会在弱 CPU / 小内存机器上互相踩踏,
//! 表现为整窗渲染掉到个位数帧率、RPA 注入被页面加载冲掉。降档只砍并发与视觉成本,不砍功能。
//!
//! 判定一次、进程内缓存:逻辑 CPU ≤ 4 或物理内存 ≤ 8GB 视为低配。
//! 部署机可用环境变量 `VELTRIX_LOW_SPEC=1`(强制低配)/ `=0`(强制按高配跑)人工覆盖,
//! 优先级高于硬件自动判定。

use std::sync::OnceLock;

static LOW_SPEC: OnceLock<bool> = OnceLock::new();

/// 是否低配设备(首次调用判定,之后进程内缓存)
pub fn is_low_spec() -> bool {
    *LOW_SPEC.get_or_init(detect)
}

fn detect() -> bool {
    // 人工覆盖优先:弱机跑出问题时可强制降档,好机器想压满吞吐也可强制关闭降档
    if let Ok(v) = std::env::var("VELTRIX_LOW_SPEC") {
        match v.trim().to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" | "on" => return true,
            "0" | "false" | "no" | "off" => return false,
            _ => {} // 未识别的值不拦,继续走自动判定
        }
    }
    // 逻辑核心 ≤ 4:老平台低频核带不动「3 窗采集 + 10 路下载 + 转码」同跑
    let cores = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(0);
    if cores > 0 && cores <= 4 {
        return true;
    }
    // 物理内存 ≤ 8GB:多 WebView 各自独立数据目录都起进程树,内存先成瓶颈
    let ram_gb = total_ram_gb();
    ram_gb > 0.0 && ram_gb <= 8.0
}

/// 物理内存 GB;读取失败返回 0(调用方据此跳过内存维度,不误判)
fn total_ram_gb() -> f64 {
    use sysinfo::System;
    let mut sys = System::new();
    sys.refresh_memory();
    sys.total_memory() as f64 / (1024.0 * 1024.0 * 1024.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 判定结果稳定可重复() {
        // OnceLock 缓存:多次调用结果一致,且不 panic(实际档位随机器而异,不断言真假)
        let a = is_low_spec();
        let b = is_low_spec();
        assert_eq!(a, b);
    }
}
