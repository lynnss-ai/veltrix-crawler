//! 🌐 浏览器 / RPA 智能体:IPC 命令 + ReAct 循环(`commands`),浏览器工具集(`tools`)。
//! 底层真实 WebView / 接口拦截设施在 `crate::webview`,被本智能体复用。

pub mod commands;
pub mod tools;
