//! 🖥️ 进程与系统资源工具模块(独立、可复用)。
//! 工具:list_processes / find_process / kill_process / system_info(sysinfo,跨平台直读,免起子进程)。
//! 不绑定具体 Agent、不绑前端;经 `tools::build_registry()` 拿到注册表后挂到任意 ReAct 循环。

pub mod tools;
