//! 智能体共享地基(场景无关):
//! - `llm`:统一的 LlmProvider + Tool/ToolRegistry 接口(屏蔽各厂商差异),换模型只改 `ProviderRef`。
//! - `summary`:长会话上下文策略(live 原文窗口 + 滚动摘要),chat / coding 共用。
//! - `shared`:跨智能体复用的消息视图(落库行↔前端视图)+ ReAct 持久化/标题工具。
//!
//! chat / coding / rpa 三个智能体只依赖本 core,彼此之间不直接耦合。

pub mod llm;
pub mod react;
pub mod shared;
pub mod summary;

// 重导出 llm 接口,使调用方用 `crate::agent::core::{Tool, ProviderRef, ChatMsg, ...}` 即可。
pub use llm::*;
