//! 💻 编程智能体:IPC 命令 + ReAct 循环 + 开发服务器 / 沙盒(`commands`),工具集与提示词(`tools`)。
//! 纵向扩展点:后续把写死的语言运行时(Node/Vite)抽成可插拔 runtime,平行接入 Python 等环境。

pub mod commands;
pub mod tools;
