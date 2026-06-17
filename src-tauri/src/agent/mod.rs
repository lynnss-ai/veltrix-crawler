//! 场景 Agent(编程 / RPA …)。共用 `llm::agent` 的 LlmProvider + Tool 接口;
//! 各场景 = 一组工具 + 提示词 + ReAct 循环(循环目前在对应 command 内,便于落库与事件)。

pub mod browser;
pub mod coding;

use std::path::{Component, Path, PathBuf};
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
