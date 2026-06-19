//! 🖥️ 跨平台终端命令工具(独立工具模块,供任意 Agent 挂载复用)。
//!
//! 与编程 Agent 的 `coding::tools::run_command_in` 区别:那条绑定「工作区沙箱 + 可选 Docker」;
//! 本模块是**通用**的跨平台 shell:Windows→cmd / PowerShell,macOS / Linux→sh / bash,
//! 不绑工作区、不走容器,可指定任意工作目录,定位是「让 Agent 在本机三平台敲终端命令」的基础设施。

use std::path::Path;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{json, Value};

use crate::agent::core::{Tool, ToolDef, ToolRegistry, ToolResult};

/// 默认 / 上限超时(秒)。安装依赖、编译等可能较久,给足上限但不无限等。
const DEFAULT_TIMEOUT_SECS: u64 = 120;
const MAX_TIMEOUT_SECS: u64 = 600;
/// 输出截断:总上限 + 头部 / 尾部各保留段(真因报错常在尾部 stderr,故头尾都留)。
const OUTPUT_CAP: usize = 20_000;
const OUTPUT_HEAD: usize = 8_000;
const OUTPUT_TAIL: usize = 12_000;

/// 构造跨平台终端命令工具注册表。无外部上下文(命令在本机执行)。
pub fn build_registry() -> ToolRegistry {
    let mut registry = ToolRegistry::new();
    registry.register(std::sync::Arc::new(RunCommandTool));
    registry
}

/// 按 shell 选择构造命令。auto:**Windows→powershell**、其它→sh -c。
/// 显式指定时:cmd / powershell / bash / sh。
///
/// Windows 默认选 PowerShell 而非 cmd,是为了让 `ls / cat / pwd / cp / mv / rm / echo / cd` 等
/// 常用 Unix 命令直接可用——它们是 PowerShell 原生 cmdlet 别名(比手写 `ls→dir` 映射可靠)。
/// 局限:Unix **风格参数**(如 `ls -la` 的 `-la`、`rm -rf`)PowerShell 别名不识别,需用 PS 语法或显式 cmd。
fn build_command(shell: &str, command: &str) -> tokio::process::Command {
    let pick = if shell == "auto" || shell.is_empty() {
        if cfg!(windows) {
            "powershell"
        } else {
            "sh"
        }
    } else {
        shell
    };
    match pick {
        "powershell" | "pwsh" => {
            let mut c = tokio::process::Command::new(pick);
            // 强制控制台输入/输出编码为 UTF-8,避免中文 Windows 下 PowerShell 默认编码(GBK)导致捕获到乱码。
            // $OutputEncoding 管「PS→外部程序」,[Console]::OutputEncoding 管「外部程序→PS 读取」,两者都设最稳。
            // -NoProfile 避免加载用户 profile 拖慢 / 干扰;-Command 接整条命令串。
            let wrapped = format!(
                "$OutputEncoding=[Console]::OutputEncoding=[System.Text.Encoding]::UTF8; {command}"
            );
            c.arg("-NoProfile").arg("-Command").arg(wrapped);
            c
        }
        "cmd" => {
            let mut c = tokio::process::Command::new("cmd");
            // chcp 65001 让 cmd 内建命令按 UTF-8 输出,避免中文 Windows(默认 GBK 代码页)输出被
            // from_utf8_lossy 解成乱码;`>nul` 抑制 chcp 自身的提示行。(外部程序编码仍取决于其自身)
            c.arg("/C").arg(format!("chcp 65001>nul && {command}"));
            c
        }
        "bash" => {
            let mut c = tokio::process::Command::new("bash");
            c.arg("-c").arg(command);
            c
        }
        // 缺省 / "sh" / 其它未知值都退化到 sh -c(POSIX 通用)
        _ => {
            let mut c = tokio::process::Command::new("sh");
            c.arg("-c").arg(command);
            c
        }
    }
}

/// 输出截断:不超上限直接返回;超长保留头尾并标省略。
fn cap_output(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= OUTPUT_CAP {
        return s.to_string();
    }
    let head: String = chars[..OUTPUT_HEAD].iter().collect();
    let tail_start = chars.len() - OUTPUT_TAIL;
    let tail: String = chars[tail_start..].iter().collect();
    let omitted = tail_start - OUTPUT_HEAD;
    format!("{head}\n…(中间省略约 {omitted} 字符,已保留头尾;真因常在尾部)…\n{tail}")
}

struct RunCommandTool;

#[async_trait]
impl Tool for RunCommandTool {
    fn def(&self) -> ToolDef {
        // 把当前操作系统写进工具说明,让 Agent 知道该用哪套命令语法——「多系统适配」的关键一环
        let os = std::env::consts::OS;
        let default_shell = if cfg!(windows) { "powershell" } else { "sh" };
        // Windows 默认走 PowerShell,故 ls / cat / pwd 等命令名可用;说清以免模型退回 dir 之类、或乱用 Unix 参数
        let windows_hint = if cfg!(windows) {
            " 本机为 Windows,auto 默认用 PowerShell——`ls / cat / pwd / cp / mv / rm / echo / cd` 等命令名可直接用\
             (但 Unix 参数如 `-la`、`rm -rf` 不识别,需改用 PowerShell 语法;要原生 cmd 请传 shell=\"cmd\")。"
        } else {
            ""
        };
        ToolDef {
            name: "run_command".into(),
            description: format!(
                "在本机执行一条终端命令,返回 exit 码、stdout、stderr。\
                 **当前操作系统:{os}**(auto 模式默认走 {default_shell})。{windows_hint}\
                 适合查看目录、运行脚本、调用 CLI。注意:单次执行,不保留 shell 状态(cd / 环境变量不跨命令记忆)。"
            ),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "command": { "type": "string", "description": "要执行的命令,如 `ls -la`、`git status`、`python script.py`" },
                    "cwd": { "type": "string", "description": "可选:命令的工作目录绝对路径;不填则用进程当前目录" },
                    "shell": {
                        "type": "string",
                        "description": "可选 shell:auto(默认,Win→cmd / 其它→sh)、cmd、powershell、bash、sh",
                        "enum": ["auto", "cmd", "powershell", "bash", "sh"]
                    },
                    "timeout_secs": { "type": "integer", "description": "可选超时秒数,缺省 120,上限 600" }
                },
                "required": ["command"]
            }),
        }
    }

    async fn run(&self, args: Value) -> ToolResult {
        let Some(command) = args.get("command").and_then(Value::as_str) else {
            return ToolResult::err("缺少参数 command");
        };
        let command = command.trim();
        if command.is_empty() {
            return ToolResult::err("command 不能为空");
        }
        let shell = args.get("shell").and_then(Value::as_str).unwrap_or("auto");
        let timeout = args
            .get("timeout_secs")
            .and_then(Value::as_u64)
            .unwrap_or(DEFAULT_TIMEOUT_SECS)
            .clamp(1, MAX_TIMEOUT_SECS);

        let mut cmd = build_command(shell, command);
        if let Some(cwd) = args.get("cwd").and_then(Value::as_str) {
            let cwd = cwd.trim();
            if !cwd.is_empty() {
                if !Path::new(cwd).is_dir() {
                    return ToolResult::err(format!("工作目录不存在或不是目录: {cwd}"));
                }
                cmd.current_dir(cwd);
            }
        }
        cmd.kill_on_drop(true)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());

        let out = match tokio::time::timeout(Duration::from_secs(timeout), cmd.output()).await {
            Ok(Ok(o)) => o,
            Ok(Err(e)) => return ToolResult::err(format!("命令启动失败: {e}")),
            Err(_) => return ToolResult::err(format!("命令超时(>{timeout}s)已终止")),
        };

        let mut s = format!("exit: {}\n", out.status.code().unwrap_or(-1));
        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        if !stdout.trim().is_empty() {
            s.push_str("stdout:\n");
            s.push_str(&stdout);
        }
        if !stderr.trim().is_empty() {
            s.push_str("\nstderr:\n");
            s.push_str(&stderr);
        }
        ToolResult {
            content: cap_output(&s),
            is_error: !out.status.success(),
        }
    }
}
