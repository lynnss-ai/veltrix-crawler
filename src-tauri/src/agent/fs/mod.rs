//! 📁 文件系统工具模块(独立、可复用)。
//! 工具:find_files / read_file / write_file / list_dir / file_info(通用全机路径,非沙箱)。
//! 不绑定具体 Agent、不绑前端;经 `tools::build_registry()` 拿到注册表后挂到任意 ReAct 循环。

pub mod tools;
