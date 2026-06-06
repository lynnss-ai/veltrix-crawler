//! 指令执行器:按 action 分发到具体 handler。
//!
//! MVP 阶段内置 4 个 handler 全是 stub(打日志后返回成功),后续接采集引擎钩子时逐个替换。
//! 真实接入时需要从 AppState 拿到 webview pool / cookie pool / adapter registry 等句柄。

use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;

/// 指令执行结果。`ok=false` 时 `error` 必填。
#[derive(Debug, Clone, serde::Serialize)]
pub struct CommandResult {
    pub ok: bool,
    pub error: Option<String>,
}

impl CommandResult {
    pub fn ok() -> Self {
        Self { ok: true, error: None }
    }
    pub fn err(msg: impl Into<String>) -> Self {
        Self {
            ok: false,
            error: Some(msg.into()),
        }
    }
}

/// handler 签名:接收 params,返回结果。后续接 AppState 时可改成 Fn 闭包捕获 Arc<AppState>。
pub type HandlerFn =
    Arc<dyn Fn(Value) -> futures_util::future::BoxFuture<'static, CommandResult> + Send + Sync>;

#[derive(Default, Clone)]
pub struct ExecutorRegistry {
    handlers: Arc<HashMap<String, HandlerFn>>,
}

impl ExecutorRegistry {
    /// 内置 stub 注册:暂停 / 恢复任务、重登账号、重启引擎。
    pub fn with_defaults() -> Self {
        use futures_util::FutureExt;
        let mut m: HashMap<String, HandlerFn> = HashMap::new();

        m.insert(
            "pause_task".into(),
            Arc::new(|params: Value| {
                async move {
                    tracing::info!("[stub] pause_task: {params}");
                    CommandResult::ok()
                }
                .boxed()
            }),
        );
        m.insert(
            "resume_task".into(),
            Arc::new(|params: Value| {
                async move {
                    tracing::info!("[stub] resume_task: {params}");
                    CommandResult::ok()
                }
                .boxed()
            }),
        );
        m.insert(
            "relogin_account".into(),
            Arc::new(|params: Value| {
                async move {
                    tracing::info!("[stub] relogin_account: {params}");
                    CommandResult::ok()
                }
                .boxed()
            }),
        );
        m.insert(
            "restart_engine".into(),
            Arc::new(|params: Value| {
                async move {
                    tracing::info!("[stub] restart_engine: {params}");
                    CommandResult::ok()
                }
                .boxed()
            }),
        );

        Self {
            handlers: Arc::new(m),
        }
    }

    pub async fn dispatch(&self, action: &str, params: Value) -> CommandResult {
        match self.handlers.get(action) {
            Some(h) => h(params).await,
            None => CommandResult::err(format!("未知指令: {action}")),
        }
    }
}
