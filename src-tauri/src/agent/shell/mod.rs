//! 🖥️ 跨平台终端命令工具模块(独立、可复用)。
//! 单一工具 `run_command`:在 Windows / macOS / Linux 上执行 shell 命令(自动选 cmd / sh,亦可显式指定)。
//! 不绑定具体 Agent、不绑前端;经 `tools::build_registry()` 拿到注册表后挂到任意 ReAct 循环即可。

pub mod tools;
