//! 💬 对话智能体:会话 / 消息命令(`commands`)+ 跨会话长期记忆 RAG(`memory`)。
//! 通用模型能力(chat completion / embedding)留在 `crate::llm`;滚动摘要在 `core::summary`。

pub mod commands;
pub mod memory;
