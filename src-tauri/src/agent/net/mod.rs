//! 🌐 HTTP 请求工具模块(独立、可复用)。
//! 单一工具 `http_request`:GET/POST/... 调 REST API / 拉网页 / 调 webhook(reqwest async,仅 http/https)。
//! 不绑定具体 Agent、不绑前端;经 `tools::build_registry()` 拿到注册表后挂到任意 ReAct 循环。

pub mod tools;
