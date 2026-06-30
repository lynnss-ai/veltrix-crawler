//! 编排器的 4 个委派工具:把 coding/rpa/computer/local 当 tool。每个工具捕获 clonable 句柄,
//! 在 `run()` 里调对应子智能体的 `run_*_subtask`(同 conversation_id 串行执行),把子智能体最终文本作工具结果回传。

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::{json, Value};
use tauri::AppHandle;

use crate::agent::coding::commands::CodingExecCtx;
use crate::agent::core::shared::AgentConfirmChannel;
use crate::agent::core::{ProviderRef, Tool, ToolDef, ToolRegistry, ToolResult};
use crate::webview::pool::WebviewPool;

/// 单 task 参数的工具 schema。
fn task_def(name: &str, description: &str) -> ToolDef {
    ToolDef {
        name: name.to_string(),
        description: description.to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "task": {
                    "type": "string",
                    "description": "要委派执行的任务,需完整自包含(子智能体看不到本对话历史,写全目标 / 要求 / 产物)"
                }
            },
            "required": ["task"]
        }),
    }
}

fn task_arg(args: &Value) -> std::result::Result<String, ToolResult> {
    match args.get("task").and_then(Value::as_str) {
        Some(t) if !t.trim().is_empty() => Ok(t.to_string()),
        _ => Err(ToolResult::err("缺少参数 task")),
    }
}

struct DelegateCoding {
    db: sea_orm::DatabaseConnection,
    app: AppHandle,
    exec_ctx: CodingExecCtx,
    config_dir: PathBuf,
    agent_cancel: Arc<Mutex<HashSet<String>>>,
    conversation_id: String,
    owner: String,
    provider_ref: ProviderRef,
    provider_id: String,
}

#[async_trait]
impl Tool for DelegateCoding {
    fn def(&self) -> ToolDef {
        task_def(
            "delegate_to_coding",
            "委派给编程子智能体:写 / 改 / 调试代码、做网页 / H5 / 前端页面、在隔离工作区跑项目命令。涉及编程 / 建站 / 脚本时用;只是操作本机文件或网页时不用。",
        )
    }
    async fn run(&self, args: Value) -> ToolResult {
        let task = match task_arg(&args) {
            Ok(t) => t,
            Err(e) => return e,
        };
        match crate::agent::coding::commands::run_coding_subtask(
            &self.db,
            &self.app,
            &self.exec_ctx,
            &self.config_dir,
            &self.agent_cancel,
            &self.conversation_id,
            &self.owner,
            &self.provider_ref,
            &self.provider_id,
            &task,
        )
        .await
        {
            Ok(t) => ToolResult::ok(t),
            Err(e) => ToolResult::err(format!("编程子任务失败: {e}")),
        }
    }
}

struct DelegateRpa {
    db: sea_orm::DatabaseConnection,
    app: AppHandle,
    pool: Arc<WebviewPool>,
    config_dir: PathBuf,
    conversation_id: String,
    owner: String,
    provider_ref: ProviderRef,
    provider_id: String,
}

#[async_trait]
impl Tool for DelegateRpa {
    fn def(&self) -> ToolDef {
        task_def(
            "delegate_to_rpa",
            "委派给浏览器子智能体:在内嵌浏览器自动操作网页(打开网站、搜索、点按钮、填表、抓页面数据)。任务在某网站 / 网页上时用;本机文件 / 程序操作、不涉及网页时不用。",
        )
    }
    async fn run(&self, args: Value) -> ToolResult {
        let task = match task_arg(&args) {
            Ok(t) => t,
            Err(e) => return e,
        };
        match crate::agent::rpa::commands::run_rpa_subtask(
            &self.db,
            &self.app,
            &self.pool,
            &self.config_dir,
            &self.conversation_id,
            &self.owner,
            &self.provider_ref,
            &self.provider_id,
            &task,
        )
        .await
        {
            Ok(t) => ToolResult::ok(t),
            Err(e) => ToolResult::err(format!("浏览器子任务失败: {e}")),
        }
    }
}

struct DelegateComputer {
    db: sea_orm::DatabaseConnection,
    app: AppHandle,
    agent_confirm: Arc<AgentConfirmChannel>,
    config_dir: PathBuf,
    conversation_id: String,
    owner: String,
    provider_ref: ProviderRef,
    provider_id: String,
}

#[async_trait]
impl Tool for DelegateComputer {
    fn def(&self) -> ToolDef {
        task_def(
            "delegate_to_computer",
            "委派给电脑操作子智能体:看屏幕 + 操作鼠标键盘 / 窗口 / 控件(GUI 自动化)。需截图看屏、点桌面程序按钮、操作窗口时用;只读写文件 / 跑命令(不看屏)不用。",
        )
    }
    async fn run(&self, args: Value) -> ToolResult {
        let task = match task_arg(&args) {
            Ok(t) => t,
            Err(e) => return e,
        };
        match crate::agent::computer::commands::run_computer_subtask(
            &self.db,
            &self.app,
            &self.agent_confirm,
            &self.config_dir,
            &self.conversation_id,
            &self.owner,
            &self.provider_ref,
            &self.provider_id,
            &task,
        )
        .await
        {
            Ok(t) => ToolResult::ok(t),
            Err(e) => ToolResult::err(format!("电脑操作子任务失败: {e}")),
        }
    }
}

struct DelegateLocal {
    db: sea_orm::DatabaseConnection,
    app: AppHandle,
    agent_confirm: Arc<AgentConfirmChannel>,
    config_dir: PathBuf,
    conversation_id: String,
    owner: String,
    provider_ref: ProviderRef,
    provider_id: String,
}

#[async_trait]
impl Tool for DelegateLocal {
    fn def(&self) -> ToolDef {
        task_def(
            "delegate_to_local",
            "委派给本机子智能体:文件 / 进程 / 终端(读写删文件、查 / 杀进程、跑命令)。本机管理文件 / 查系统 / 跑命令时用;任务在网页里、或要看屏点 GUI 时不用。",
        )
    }
    async fn run(&self, args: Value) -> ToolResult {
        let task = match task_arg(&args) {
            Ok(t) => t,
            Err(e) => return e,
        };
        match crate::agent::local::commands::run_local_subtask(
            &self.db,
            &self.app,
            &self.agent_confirm,
            &self.config_dir,
            &self.conversation_id,
            &self.owner,
            &self.provider_ref,
            &self.provider_id,
            &task,
        )
        .await
        {
            Ok(t) => ToolResult::ok(t),
            Err(e) => ToolResult::err(format!("本机子任务失败: {e}")),
        }
    }
}

/// 构造编排器工具注册表(唯一含委派工具的注册表)。各句柄从命令层的 AppState 廉价 clone 而来。
#[allow(clippy::too_many_arguments)]
pub fn build_registry(
    db: sea_orm::DatabaseConnection,
    app: AppHandle,
    conversation_id: String,
    owner: String,
    provider_ref: ProviderRef,
    provider_id: String,
    config_dir: PathBuf,
    agent_cancel: Arc<Mutex<HashSet<String>>>,
    exec_ctx: CodingExecCtx,
    pool: Arc<WebviewPool>,
    agent_confirm: Arc<AgentConfirmChannel>,
) -> ToolRegistry {
    let mut r = ToolRegistry::new();
    r.register(Arc::new(DelegateCoding {
        db: db.clone(),
        app: app.clone(),
        exec_ctx,
        config_dir: config_dir.clone(),
        agent_cancel,
        conversation_id: conversation_id.clone(),
        owner: owner.clone(),
        provider_ref: provider_ref.clone(),
        provider_id: provider_id.clone(),
    }));
    r.register(Arc::new(DelegateRpa {
        db: db.clone(),
        app: app.clone(),
        pool,
        config_dir: config_dir.clone(),
        conversation_id: conversation_id.clone(),
        owner: owner.clone(),
        provider_ref: provider_ref.clone(),
        provider_id: provider_id.clone(),
    }));
    r.register(Arc::new(DelegateComputer {
        db: db.clone(),
        app: app.clone(),
        agent_confirm: agent_confirm.clone(),
        config_dir: config_dir.clone(),
        conversation_id: conversation_id.clone(),
        owner: owner.clone(),
        provider_ref: provider_ref.clone(),
        provider_id: provider_id.clone(),
    }));
    r.register(Arc::new(DelegateLocal {
        db,
        app,
        agent_confirm,
        config_dir,
        conversation_id,
        owner,
        provider_ref,
        provider_id,
    }));
    r
}
