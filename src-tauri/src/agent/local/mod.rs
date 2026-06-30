//! 🗂️ 本机助手智能体:把 fs / system / shell 三个独立工具模块**组装**成一个能跑的 Agent
//! (文件读写/查找、进程/系统信息、终端命令)。不看屏幕、不碰 GUI(那是 computer agent 的事)。
//! `commands` 的 ReAct 循环 + `tools` 的工具聚合 / 提示词 / 危险工具判定。

pub mod commands;
pub mod tools;
