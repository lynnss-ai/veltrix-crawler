//! 智能体平台:每个智能体一个模块(chat / coding / rpa / computer / local),共用 `core` 地基。
//! 各智能体 = 一组工具 + 提示词 + IPC 命令 + ReAct 循环(命令、记忆、工具自洽于各自目录)。
//! 横向扩展(加智能体)= 新增 `agent/<name>/` 并在此注册一行;纵向扩展(单智能体加深)在各模块内进行。

pub mod chat;
pub mod coding;
pub mod computer;
pub mod core;
pub mod desktop;
pub mod fs;
pub mod local;
pub mod net;
pub mod ocr;
pub mod orchestrator;
pub mod rpa;
pub mod shell;
pub mod system;
pub mod uia;

use std::path::{Component, Path, PathBuf};
use serde::Serialize;
use tauri::AppHandle;
use veltrix_core::error::{CrawlerError, Result};

/// 把工具传入的路径解析为「工作区内」的绝对路径(沙箱护栏)。
/// 仅允许工作区内相对路径:拒绝绝对路径与含 `..` 的越界路径。
pub fn resolve_in_workspace(workspace: &Path, input: &str) -> Result<PathBuf> {
    let p = Path::new(input);
    if p.is_absolute() {
        return Err(CrawlerError::Config(
            "工具路径需为工作区内的相对路径".into(),
        ));
    }
    // 逐组件校验:只允许普通段与 `.`。
    // 必须显式拒绝 Prefix(盘符,如 `C:foo`)与 RootDir(根起始,如 `\x`)——
    // 这类相对路径 is_absolute() 为 false,却会让 Path::join 丢弃工作区前缀造成沙箱逃逸。
    for c in p.components() {
        match c {
            Component::Normal(_) | Component::CurDir => {}
            Component::ParentDir => {
                return Err(CrawlerError::Config("路径包含非法的 .. 段".into()));
            }
            Component::Prefix(_) | Component::RootDir => {
                return Err(CrawlerError::Config(
                    "路径不能包含盘符或根目录前缀".into(),
                ));
            }
        }
    }
    Ok(workspace.join(p))
}

/// 一个「电脑操作」工具的对外信息(供前端展示 / 调试工具清单)。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentToolInfo {
    /// 所属模块:desktop(桌面 GUI)/ shell(跨平台终端)。
    pub module: String,
    pub name: String,
    pub description: String,
}

/// 拍照回传:截取桌面屏幕(或指定窗口),返回 PNG 的 base64 data URL(`data:image/png;base64,...`)。
/// 不落盘,直接把画面回传给调用方——前端可 `<img src>` 显示,或将来作为多模态消息喂给视觉模型。
/// `target` 留空=主显示器全屏;填窗口标题子串=截该窗口。Windows 首要支持(xcap 本身跨平台)。
#[tauri::command]
pub async fn capture_desktop_screenshot(target: Option<String>) -> std::result::Result<String, String> {
    let target = target.unwrap_or_default();
    // 截屏是阻塞调用,放 blocking 线程,避免占用 async 执行器
    tokio::task::spawn_blocking(move || desktop::tools::capture_screen_data_url(&target))
        .await
        .map_err(|e| format!("截屏任务异常: {e}"))?
}

/// 列出各工具模块提供的全部工具(供前端展示工具清单 / 调试)。
/// 这些模块已分别编排进具体 Agent:desktop/ocr/uia → computer(GUI);fs/system/shell → local(本机助手);
/// net → rpa。本命令只读取工具定义(不触发任何实际操作),按模块罗列以便核对。
#[tauri::command]
pub fn list_agent_tools(app: AppHandle) -> Vec<AgentToolInfo> {
    let mut out = Vec::new();
    let groups: Vec<(&str, core::ToolRegistry)> = vec![
        ("desktop", desktop::tools::build_registry(app)),
        ("shell", shell::tools::build_registry()),
        ("fs", fs::tools::build_registry()),
        ("system", system::tools::build_registry()),
        ("ocr", ocr::tools::build_registry()),
        ("uia", uia::tools::build_registry()),
        ("net", net::tools::build_registry()),
    ];
    for (module, registry) in groups {
        for d in registry.defs() {
            out.push(AgentToolInfo {
                module: module.to_string(),
                name: d.name,
                description: d.description,
            });
        }
    }
    out
}
