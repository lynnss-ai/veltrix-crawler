//! 编程 Agent 命令:send_coding_message(基于通用 ReAct 运行器)、工作区读写。
//!
//! 工具集来自 `agent::coding::tools`,工具与 run_command 限定在「编程工作区」目录内(沙箱)。
//! 特殊逻辑:Plan/Act 模式、自主续航、run_command 自动修复、计划续航、验证闸门。

use crate::agent::coding::tools as coding;
use crate::sandbox::{SandboxManager, SandboxOptions};
use crate::agent::core::react::{
    FinishDecision, IterDecision, ReactConfig, ReactHooks, ToolPostAction,
};
use crate::agent::core::shared::{
    begin_agent_turn, finalize_conversation_meta, insert_final_assistant, live_windowed_messages,
    load_agent_guidelines, MessageView, MAX_ITERS,
};
use crate::agent::core::summary as conv_summary;
use crate::agent::core::{
    provider_for, ChatMsg, LlmOptions, LlmRequest, ProviderKind, ProviderRef, ToolResult,
};
use crate::commands::{current_user, AppState};
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QueryOrder, QuerySelect};
use serde::Serialize;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, State};
use tokio::io::AsyncBufReadExt;
use veltrix_core::db::entity::{
    agent_route_log, chat_conversation as conv, provider as provider_entity,
};
use veltrix_core::error::{CrawlerError, Result};

/// 自主续航(Act 模式)总步数硬上限:远大于 MAX_ITERS 让长任务一气呵成,但仍有界防失控。
const MAX_AUTO_ITERS: usize = 50;
/// 自主续航中「模型过早收尾但计划仍有未完成项」时,自动注入续写提示推进的最大次数(防空转)。
const MAX_CONTINUES: usize = 4;
/// run_command 失败后,模型若想直接收尾,自动注入引导逼它修复重试的最大次数(防卡死)。
const AUTO_FIX_MAX: usize = 2;
/// 硬验证闸门:改了代码文件却没成功跑过验证命令就想收尾 → 打回强制先验证的最大次数。
const MAX_VERIFY_GATE: usize = 2;
/// 工作区根目录在 app_secrets 的 key;空 = 默认 `<app_data>/coding-workspaces`(每会话一个子目录)。
const CODING_WORKSPACE_KEY: &str = "coding_workspace_path";
/// 沙盒内存上限在 app_secrets 的 key(MB;0 / 缺省 = 不限)。
const SANDBOX_MEMORY_LIMIT_KEY: &str = "sandbox_memory_limit_mb";
/// 沙盒出站网络限速在 app_secrets 的 key(KB/s;0 / 缺省 = 不限)。
const SANDBOX_NET_LIMIT_KEY: &str = "sandbox_net_limit_kbps";
/// 沙盒空闲自动回收阈值在 app_secrets 的 key(分钟;0 = 关闭)。pub(crate):lib.rs 回收循环也读。
pub(crate) const SANDBOX_IDLE_RECYCLE_KEY: &str = "sandbox_idle_recycle_minutes";
/// 沙盒 CPU 上限在 app_secrets 的 key(百分比 1-100;0 / 缺省 = 不限)。
const SANDBOX_CPU_LIMIT_KEY: &str = "sandbox_cpu_limit_percent";
/// 沙盒进程数上限在 app_secrets 的 key(0 / 缺省 = 不限)。
const SANDBOX_MAX_PROCESSES_KEY: &str = "sandbox_max_processes";
/// 沙盒磁盘 IO 限速在 app_secrets 的 key(KB/s;0 / 缺省 = 不限)。
const SANDBOX_IO_LIMIT_KEY: &str = "sandbox_io_limit_kbps";
/// 预览端口扫描区间 `[HOST_PORT_BASE, HOST_PORT_BASE + HOST_PORT_SPAN)`:
/// 每个会话按 id 派生一个区间内的起点端口(自定义、相对固定),被占用则在区间内顺延找空闲端口。
/// 多个程序 / 残留进程都挤同一端口,固定 5173 会撞车——故按会话分配并查占用。
const HOST_PORT_BASE: u16 = 5173;
const HOST_PORT_SPAN: u16 = 16;

/// 工作区根目录(自定义优先,否则默认数据目录下 coding-workspaces)。
fn workspace_base(config_dir: &Path, custom: &str) -> PathBuf {
    if custom.trim().is_empty() {
        config_dir.join("coding-workspaces")
    } else {
        PathBuf::from(custom.trim())
    }
}

/// 会话 id 规整为安全目录名(只留字母数字/-/_,防路径穿越)。
fn safe_id(id: &str) -> String {
    let s: String = id
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect();
    if s.is_empty() {
        "default".to_string()
    } else {
        s
    }
}

/// 某会话的宿主工作区目录 = 根目录 / 会话id。
fn conv_workspace(base: &Path, conv_id: &str) -> PathBuf {
    base.join(safe_id(conv_id))
}

/// 读取编程工作区路径(给前端展示);传 conversationId 则返回该会话目录,否则返回根目录。
#[tauri::command]
pub async fn get_coding_workspace(
    state: State<'_, AppState>,
    conversation_id: Option<String>,
) -> Result<String> {
    let base = workspace_base(&state.config_dir, &crate::commands::get_secret(&state.db, CODING_WORKSPACE_KEY).await);
    let p = match conversation_id {
        Some(id) if !id.trim().is_empty() => conv_workspace(&base, &id),
        _ => base,
    };
    Ok(p.display().to_string())
}

/// 设置工作区根目录(空串=恢复默认)。
#[tauri::command]
pub async fn set_coding_workspace(state: State<'_, AppState>, path: String) -> Result<()> {
    crate::commands::set_secret(&state.db, CODING_WORKSPACE_KEY, path.trim()).await
}

/// 编程执行环境解析所需的 clonable 句柄:供委派工具在 react 循环内(无 &AppState)惰性解析。
#[derive(Clone)]
pub struct CodingExecCtx {
    pub db: sea_orm::DatabaseConnection,
    pub config_dir: PathBuf,
    pub sandbox: Arc<SandboxManager>,
}

impl CodingExecCtx {
    /// 从 AppState 构造(命令层用;均为廉价 clone)。
    pub fn from_state(state: &AppState) -> Self {
        Self {
            db: state.db.clone(),
            config_dir: state.config_dir.clone(),
            sandbox: state.sandbox.clone(),
        }
    }
}

/// 解析某会话的执行环境:返回(宿主工作区目录, ExecConfig)。
async fn resolve_exec(state: &AppState, conv_id: &str) -> Result<(PathBuf, coding::ExecConfig)> {
    resolve_exec_ctx(&CodingExecCtx::from_state(state), conv_id).await
}

/// resolve_exec 的句柄版(供委派工具调用)。
/// 本地沙盒在此懒创建:首个编程动作才建 Job(微秒级),应用启动不做任何预热。
pub async fn resolve_exec_ctx(
    ctx: &CodingExecCtx,
    conv_id: &str,
) -> Result<(PathBuf, coding::ExecConfig)> {
    let base = workspace_base(
        &ctx.config_dir,
        &crate::commands::get_secret(&ctx.db, CODING_WORKSPACE_KEY).await,
    );
    let ws = conv_workspace(&base, conv_id);
    tokio::fs::create_dir_all(&ws)
        .await
        .map_err(|e| CrawlerError::Config(format!("创建工作区失败: {e}")))?;
    let opts = SandboxOptions {
        memory_limit_bytes: sandbox_memory_limit_bytes(&ctx.db).await,
        net_max_bandwidth_bytes_per_sec: sandbox_net_limit_bytes_per_sec(&ctx.db).await,
        cpu_limit_percent: sandbox_cpu_limit_percent(&ctx.db).await,
        max_processes: sandbox_max_processes(&ctx.db).await,
        io_max_bandwidth_bytes_per_sec: sandbox_io_limit_bytes_per_sec(&ctx.db).await,
        env_keep: Vec::new(), // coding 沙盒不放行任何疑似密钥变量
        storage_dir: Some(ws.clone()),
    };
    let sandbox = ctx
        .sandbox
        .get_or_create(conv_id, opts)
        .map_err(|e| CrawlerError::Config(format!("创建本地沙盒失败: {e}")))?;
    Ok((
        ws,
        coding::ExecConfig {
            sandbox,
            sandbox_id: conv_id.to_string(),
            config_dir: ctx.config_dir.clone(),
        },
    ))
}

/// 读沙盒内存上限配置(app_secrets,MB;0 / 缺省 / 非法值 = 不限)。
async fn sandbox_memory_limit_bytes(db: &sea_orm::DatabaseConnection) -> Option<u64> {
    let v = crate::commands::get_secret(db, SANDBOX_MEMORY_LIMIT_KEY).await;
    let mb: u64 = v.trim().parse().unwrap_or(0);
    if mb == 0 {
        None
    } else {
        Some(mb.saturating_mul(1024 * 1024))
    }
}

/// 读沙盒出站限速配置(app_secrets,KB/s;0 / 缺省 / 非法值 = 不限),返回字节/秒。
async fn sandbox_net_limit_bytes_per_sec(db: &sea_orm::DatabaseConnection) -> Option<u64> {
    let v = crate::commands::get_secret(db, SANDBOX_NET_LIMIT_KEY).await;
    let kbps: u64 = v.trim().parse().unwrap_or(0);
    if kbps == 0 {
        None
    } else {
        Some(kbps.saturating_mul(1024))
    }
}

/// 读空闲回收阈值(app_secrets,分钟;缺省 30,0 = 关闭)。
async fn sandbox_idle_recycle_minutes(db: &sea_orm::DatabaseConnection) -> u64 {
    let v = crate::commands::get_secret(db, SANDBOX_IDLE_RECYCLE_KEY).await;
    v.trim().parse().unwrap_or(30)
}

/// 读沙盒 CPU 上限(app_secrets,百分比;0 / 缺省 / 非法 = 不限)。
async fn sandbox_cpu_limit_percent(db: &sea_orm::DatabaseConnection) -> Option<u32> {
    let v = crate::commands::get_secret(db, SANDBOX_CPU_LIMIT_KEY).await;
    let p: u32 = v.trim().parse().unwrap_or(0);
    if p == 0 { None } else { Some(p.clamp(1, 100)) }
}

/// 读沙盒进程数上限(app_secrets;0 / 缺省 / 非法 = 不限)。
async fn sandbox_max_processes(db: &sea_orm::DatabaseConnection) -> Option<u32> {
    let v = crate::commands::get_secret(db, SANDBOX_MAX_PROCESSES_KEY).await;
    let n: u32 = v.trim().parse().unwrap_or(0);
    if n == 0 { None } else { Some(n) }
}

/// 读沙盒磁盘 IO 限速(app_secrets,KB/s;0 / 缺省 / 非法 = 不限),返回字节/秒。
async fn sandbox_io_limit_bytes_per_sec(db: &sea_orm::DatabaseConnection) -> Option<u64> {
    let v = crate::commands::get_secret(db, SANDBOX_IO_LIMIT_KEY).await;
    let kbps: u64 = v.trim().parse().unwrap_or(0);
    if kbps == 0 {
        None
    } else {
        Some(kbps.saturating_mul(1024))
    }
}

/// 用户在终端直接执行一条命令(在该会话的工作区 / 沙盒内;超时);返回 exit/stdout/stderr 文本。
#[tauri::command]
pub async fn run_workspace_command(
    state: State<'_, AppState>,
    conversation_id: String,
    command: String,
) -> Result<String> {
    let cmd = command.trim();
    if cmd.is_empty() {
        return Err(CrawlerError::Config("命令为空".into()));
    }
    let (ws, exec) = resolve_exec(&state, &conversation_id).await?;
    Ok(coding::run_command_in(&ws, cmd, &exec).await.content)
}

/// 回退:丢弃本轮 Agent 的未提交改动,回到最近一次检查点(发送前状态)。
/// 已跟踪文件复位 + 删除本轮新建的未跟踪文件;仅文件系统层面,消息历史保留。
#[tauri::command]
pub async fn checkpoint_rollback(
    state: State<'_, AppState>,
    conversation_id: String,
) -> Result<String> {
    let (ws, exec) = resolve_exec(&state, &conversation_id).await?;
    let reset = coding::run_command_in(&ws, "git reset --hard HEAD", &exec).await;
    if reset.is_error {
        return Err(CrawlerError::Config(format!(
            "回退失败(可能尚无检查点 / 环境无 git): {}",
            reset.content.chars().take(200).collect::<String>()
        )));
    }
    // 删除本轮新建的未跟踪文件 / 目录(best-effort)
    let _ = coding::run_command_in(&ws, "git clean -fd", &exec).await;
    Ok("已回退到本轮发送前的文件状态(历史记录保留)".to_string())
}

/// 一个回退版本(git 检查点):commit 短哈希 + 提交时间(unix 秒)+ 该轮任务标签。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckpointView {
    pub hash: String,
    pub time: i64,
    pub message: String,
}

/// 列出某会话工作区的回退版本(git 检查点历史,新→旧;上限 50)。无 git / 无提交则返回空。
#[tauri::command]
pub async fn list_coding_checkpoints(
    state: State<'_, AppState>,
    conversation_id: String,
) -> Result<Vec<CheckpointView>> {
    let (ws, exec) = resolve_exec(&state, &conversation_id).await?;
    // 用 0x1f(单元分隔符)分隔字段,避免提交信息里的空格 / 制表符干扰解析
    let out = coding::run_command_in(&ws, "git log -n 50 --pretty=format:%h%x1f%ct%x1f%s", &exec)
        .await;
    if out.is_error {
        return Ok(Vec::new()); // 无 git / 无提交:无版本可列
    }
    let mut list = Vec::new();
    for line in out.content.lines() {
        let mut parts = line.splitn(3, '\u{1f}');
        let (Some(hash), Some(time), Some(message)) = (parts.next(), parts.next(), parts.next())
        else {
            continue;
        };
        list.push(CheckpointView {
            hash: hash.trim().to_string(),
            time: time.trim().parse().unwrap_or(0),
            message: message.to_string(),
        });
    }
    Ok(list)
}

/// 回退到指定检查点:git reset --hard <hash> + 清理未跟踪文件。hash 必须是十六进制(防 shell 注入)。
#[tauri::command]
pub async fn rollback_to_checkpoint(
    state: State<'_, AppState>,
    conversation_id: String,
    hash: String,
) -> Result<String> {
    let h = hash.trim();
    if h.is_empty() || !h.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(CrawlerError::Config("无效的版本标识".into()));
    }
    let (ws, exec) = resolve_exec(&state, &conversation_id).await?;
    let reset = coding::run_command_in(&ws, &format!("git reset --hard {h}"), &exec).await;
    if reset.is_error {
        return Err(CrawlerError::Config(format!(
            "回退失败(版本不存在 / 环境无 git): {}",
            reset.content.chars().take(200).collect::<String>()
        )));
    }
    let _ = coding::run_command_in(&ws, "git clean -fd", &exec).await;
    Ok("已回退到所选版本(文件已恢复,对话历史保留)".to_string())
}

/// 某版本里单个文件的改动:状态 + 路径 + 增删行数 + 该文件的 diff 正文。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckpointFileDiff {
    /// added / modified / deleted / renamed
    pub status: String,
    pub path: String,
    pub additions: u32,
    pub deletions: u32,
    /// 该文件的 unified diff 正文(@@ hunk + 增删/上下文行;二进制为提示行)
    pub diff: String,
}

/// 某版本(检查点)的完整改动详情:逐文件列出。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckpointDiffView {
    pub files: Vec<CheckpointFileDiff>,
}

/// 取某检查点的改动详情(git show 该提交的 patch,逐文件解析)。hash 必须是十六进制(防 shell 注入)。
#[tauri::command]
pub async fn get_checkpoint_diff(
    state: State<'_, AppState>,
    conversation_id: String,
    hash: String,
) -> Result<CheckpointDiffView> {
    let h = hash.trim();
    if h.is_empty() || !h.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(CrawlerError::Config("无效的版本标识".into()));
    }
    let (ws, exec) = resolve_exec(&state, &conversation_id).await?;
    // 排除 node_modules / target 等重目录:它们若被提交,按字母序排在 src 前,会把用户真实改动挤出
    // 输出截断窗口(且纯噪声)。双引号包裹 pathspec 对 cmd / sh 均安全。
    let excludes: String = WS_SKIP_DIRS
        .iter()
        .map(|d| format!(" \":(exclude){d}\""))
        .collect();
    // --format= 去掉提交头只留 patch;-M 识别重命名;--no-color 输出纯文本便于解析
    let out = coding::run_command_in(
        &ws,
        &format!("git show {h} --format= -M --no-color -- .{excludes}"),
        &exec,
    )
    .await;
    if out.is_error {
        return Err(CrawlerError::Config(format!(
            "读取版本改动失败(版本不存在 / 环境无 git): {}",
            out.content.chars().take(200).collect::<String>()
        )));
    }
    Ok(CheckpointDiffView {
        files: parse_unified_diff(extract_command_stdout(&out.content)),
    })
}

/// run_command_in 把输出包成 `exit: N\nstdout:\n<...>[\nstderr:\n<...>]`;取其中 stdout 段,
/// 避免尾部 stderr 段被并入最后一个文件的 diff 正文。
fn extract_command_stdout(wrapped: &str) -> &str {
    let body = wrapped
        .split_once("stdout:\n")
        .map(|(_, rest)| rest)
        .unwrap_or(wrapped);
    match body.rfind("\nstderr:\n") {
        Some(i) => &body[..i],
        None => body,
    }
}

/// 解析 `git show --format=` 的 unified diff:按 `diff --git ` 切块,逐文件解析状态/路径/增删/正文。
fn parse_unified_diff(patch: &str) -> Vec<CheckpointFileDiff> {
    let lines: Vec<&str> = patch.lines().collect();
    let mut files = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        if !lines[i].starts_with("diff --git ") {
            i += 1;
            continue;
        }
        let start = i;
        i += 1;
        while i < lines.len() && !lines[i].starts_with("diff --git ") {
            i += 1;
        }
        files.push(parse_file_block(&lines[start..i]));
    }
    files
}

/// 解析单个文件 diff 块(从一行 `diff --git` 到下一块之前)。
/// 须区分「文件头区」与「hunk 正文区」:文件头里取状态/路径,正文里才统计增删——否则正文中
/// 内容恰为 `--- xxx` / `+++ xxx` 的删/增行会被误判为头行(致计数与正文错乱)。
fn parse_file_block(block: &[&str]) -> CheckpointFileDiff {
    let mut status = "modified";
    let mut path = String::new();
    let mut rename_to: Option<String> = None;
    let mut additions = 0u32;
    let mut deletions = 0u32;
    // 正文起点:首个 `@@` hunk 头或 `Binary files` 行;此前为文件头区
    let mut body_start: Option<usize> = None;
    for (idx, &l) in block.iter().enumerate() {
        if body_start.is_none() {
            if l.starts_with("@@") || l.starts_with("Binary files ") {
                body_start = Some(idx);
            } else if l.starts_with("new file mode") {
                status = "added";
            } else if l.starts_with("deleted file mode") {
                status = "deleted";
            } else if l.starts_with("rename from ") {
                status = "renamed";
            } else if let Some(rest) = l.strip_prefix("rename to ") {
                rename_to = Some(rest.trim().to_string());
            } else if let Some(rest) = l.strip_prefix("+++ b/") {
                if path.is_empty() {
                    path = rest.trim().to_string();
                }
            } else if let Some(rest) = l.strip_prefix("--- a/") {
                // 删除文件 +++ 为 /dev/null,改从 --- a/ 取路径
                if path.is_empty() {
                    path = rest.trim().to_string();
                }
            }
        }
        // 正文区(含起始 @@ 行):统计增删(@@ / 上下文 / `\ No newline` 行均不以 +/- 起,自然不计)
        if body_start.is_some() {
            if l.starts_with('+') {
                additions += 1;
            } else if l.starts_with('-') {
                deletions += 1;
            }
        }
    }
    if let Some(to) = rename_to {
        path = to;
    }
    if path.is_empty() {
        // 兜底:从 "diff --git a/<p> b/<p>" 取 b/ 后路径(常规无空格情形)
        path = block
            .first()
            .and_then(|l| l.rsplit(" b/").next())
            .unwrap_or("")
            .trim()
            .to_string();
    }
    // 正文 = 首个 @@ / Binary 行起到块尾;无正文(纯重命名 / 模式变更)则为空
    let diff = match body_start {
        Some(s) => block[s..].join("\n"),
        None => String::new(),
    };
    CheckpointFileDiff {
        status: status.to_string(),
        path,
        additions,
        deletions,
        diff,
    }
}

/// 文件面板:列出工作区真实文件的上限 / 跳过目录 / 单文件预览字节上限。
const WS_LIST_MAX_FILES: usize = 2000;
const WS_SKIP_DIRS: &[&str] =
    &[".git", "node_modules", "target", "dist", "build", ".next", ".cache", "vendor"];
const WS_READ_MAX_BYTES: usize = 400_000;

/// 列出某会话工作区内的真实文件(相对路径,正斜杠;跳过大目录并排序)。
/// 供文件面板「真实反映」工作区(替代原先从消息派生),回退 / replace 后刷新即可看到当前状态。
#[tauri::command]
pub async fn list_workspace_files(
    state: State<'_, AppState>,
    conversation_id: String,
) -> Result<Vec<String>> {
    let base = workspace_base(&state.config_dir, &crate::commands::get_secret(&state.db, CODING_WORKSPACE_KEY).await);
    let root = conv_workspace(&base, &conversation_id);
    let mut files: Vec<String> = Vec::new();
    let mut stack: Vec<PathBuf> = vec![root.clone()];
    'walk: while let Some(dir) = stack.pop() {
        let mut rd = match tokio::fs::read_dir(&dir).await {
            Ok(rd) => rd,
            Err(_) => continue,
        };
        while let Ok(Some(entry)) = rd.next_entry().await {
            let p = entry.path();
            let is_dir = entry.file_type().await.map(|t| t.is_dir()).unwrap_or(false);
            if is_dir {
                let name = entry.file_name().to_string_lossy().into_owned();
                if !WS_SKIP_DIRS.contains(&name.as_str()) {
                    stack.push(p);
                }
                continue;
            }
            if let Ok(rel) = p.strip_prefix(&root) {
                files.push(rel.to_string_lossy().replace('\\', "/"));
                if files.len() >= WS_LIST_MAX_FILES {
                    break 'walk;
                }
            }
        }
    }
    files.sort();
    Ok(files)
}

/// 读取某会话工作区内一个文件的文本内容(限大小;二进制返回提示)。供文件面板预览。
#[tauri::command]
pub async fn read_workspace_file(
    state: State<'_, AppState>,
    conversation_id: String,
    path: String,
) -> Result<String> {
    let base = workspace_base(&state.config_dir, &crate::commands::get_secret(&state.db, CODING_WORKSPACE_KEY).await);
    let root = conv_workspace(&base, &conversation_id);
    let full = crate::agent::resolve_in_workspace(&root, &path)?;
    let bytes = tokio::fs::read(&full)
        .await
        .map_err(|e| CrawlerError::Config(format!("读取失败: {e}")))?;
    if bytes.contains(&0) {
        return Ok("(二进制文件,不预览)".to_string());
    }
    let truncated = bytes.len() > WS_READ_MAX_BYTES;
    let end = bytes.len().min(WS_READ_MAX_BYTES);
    let mut text = String::from_utf8_lossy(&bytes[..end]).into_owned();
    if truncated {
        text.push_str("\n…(文件过大,已截断)");
    }
    Ok(text)
}

/// 文件面板编辑后写回工作区内某文件(自动建父目录)。写入即落到工作区目录,
/// dev server(若在跑)会监听到变化并热更新。
#[tauri::command]
pub async fn write_workspace_file(
    state: State<'_, AppState>,
    conversation_id: String,
    path: String,
    content: String,
) -> Result<()> {
    let base = workspace_base(&state.config_dir, &crate::commands::get_secret(&state.db, CODING_WORKSPACE_KEY).await);
    let root = conv_workspace(&base, &conversation_id);
    let full = crate::agent::resolve_in_workspace(&root, &path)?;
    if let Some(parent) = full.parent() {
        tokio::fs::create_dir_all(parent).await.ok();
    }
    tokio::fs::write(&full, content.as_bytes())
        .await
        .map_err(|e| CrawlerError::Config(format!("保存失败: {e}")))?;
    Ok(())
}

/// 沙盒配置视图(本地沙盒:工作区 / 运行状态 / 内存上限配置)。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SandboxConfigView {
    /// 工作区根目录(每会话一个子目录)。
    pub workspace: String,
    /// 是否有沙盒进程在运行(dev server 等)。
    pub running: bool,
    /// 已创建的会话沙盒数。
    pub active_sessions: usize,
    /// 内存上限(MB;0 = 不限)。
    pub memory_limit_mb: u64,
    /// 出站网络限速(KB/s;0 = 不限)。
    pub net_limit_kbps: u64,
    /// 空闲自动回收阈值(分钟;0 = 关闭)。
    pub idle_recycle_minutes: u64,
    /// CPU 上限(百分比;0 = 不限)。
    pub cpu_limit_percent: u32,
    /// 进程数上限(0 = 不限)。
    pub max_processes: u32,
    /// 磁盘 IO 限速(KB/s;0 = 不限)。
    pub io_limit_kbps: u64,
}

/// 读取沙盒状态(工作区根路径 + 是否有沙盒进程在跑 + 各项限额配置)。
#[tauri::command]
pub async fn get_sandbox_config(state: State<'_, AppState>) -> Result<SandboxConfigView> {
    let base = workspace_base(&state.config_dir, &crate::commands::get_secret(&state.db, CODING_WORKSPACE_KEY).await);
    let entries = state.sandbox.list();
    let running = entries.iter().any(|e| e.stats.active_processes > 0);
    let memory_limit_mb = sandbox_memory_limit_bytes(&state.db)
        .await
        .map(|b| b / (1024 * 1024))
        .unwrap_or(0);
    let net_limit_kbps = sandbox_net_limit_bytes_per_sec(&state.db)
        .await
        .map(|b| b / 1024)
        .unwrap_or(0);
    let idle_recycle_minutes = sandbox_idle_recycle_minutes(&state.db).await;
    let cpu_limit_percent = sandbox_cpu_limit_percent(&state.db).await.unwrap_or(0);
    let max_processes = sandbox_max_processes(&state.db).await.unwrap_or(0);
    let io_limit_kbps = sandbox_io_limit_bytes_per_sec(&state.db)
        .await
        .map(|b| b / 1024)
        .unwrap_or(0);
    Ok(SandboxConfigView {
        workspace: base.display().to_string(),
        running,
        active_sessions: entries.len(),
        memory_limit_mb,
        net_limit_kbps,
        idle_recycle_minutes,
        cpu_limit_percent,
        max_processes,
        io_limit_kbps,
    })
}

/// 沙盒配置输入(单对象参数,避免命令签名无限膨胀;0 = 不限 / 关闭)。
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SandboxConfigInput {
    pub memory_limit_mb: u64,
    pub net_limit_kbps: u64,
    pub idle_recycle_minutes: u64,
    pub cpu_limit_percent: u32,
    pub max_processes: u32,
    pub io_limit_kbps: u64,
}

/// 写入沙盒配置(内存 / 网络 / CPU / 进程数 / IO / 空闲回收)。
/// 限额都在建 Job 时(SetInformationJobObject / setrlimit)固化,故改配置后把现存沙盒
/// 全部 terminate——下次编程动作按新配置惰性重建。
#[tauri::command]
pub async fn set_sandbox_config(
    state: State<'_, AppState>,
    config: SandboxConfigInput,
) -> Result<()> {
    let pairs: [(&str, u64); 6] = [
        (SANDBOX_MEMORY_LIMIT_KEY, config.memory_limit_mb),
        (SANDBOX_NET_LIMIT_KEY, config.net_limit_kbps),
        (SANDBOX_IDLE_RECYCLE_KEY, config.idle_recycle_minutes),
        (SANDBOX_CPU_LIMIT_KEY, config.cpu_limit_percent as u64),
        (SANDBOX_MAX_PROCESSES_KEY, config.max_processes as u64),
        (SANDBOX_IO_LIMIT_KEY, config.io_limit_kbps),
    ];
    for (key, value) in pairs {
        crate::commands::set_secret(&state.db, key, &value.to_string()).await?;
    }
    state.sandbox.terminate_all();
    Ok(())
}

/// 读取某沙盒的命令审计日志(尾部 limit 条,原始 JSON 行;无记录返回空)。
#[tauri::command]
pub async fn get_sandbox_audit(
    state: State<'_, AppState>,
    sandbox_id: String,
    limit: Option<u32>,
) -> Result<Vec<String>> {
    Ok(crate::sandbox::read_audit(
        &state.config_dir,
        &sandbox_id,
        limit.unwrap_or(50) as usize,
    ))
}

/// 清空某沙盒的存储目录(仅允许 manager 台账登记过的 storage_dir;先杀进程防文件占用,
/// 清完目录重建为空)。注意:这会删除该会话工作区内的全部文件,属高危操作,前端必须二次确认。
#[tauri::command]
pub async fn clear_sandbox_storage(state: State<'_, AppState>, sandbox_id: String) -> Result<()> {
    let info = state
        .sandbox
        .list()
        .into_iter()
        .find(|e| e.id == sandbox_id)
        .ok_or_else(|| CrawlerError::Config("该沙盒不存在(或已被回收),无存储可清".into()))?;
    let dir = info
        .storage_dir
        .ok_or_else(|| CrawlerError::Config("该沙盒未登记存储目录".into()))?;
    // 先 terminate + 摘台账:文件可能被沙盒进程占用;台账摘除后下次动作惰性重建
    state.sandbox.remove(&sandbox_id);
    crate::sandbox::clear_storage_dir(&dir)
        .map_err(|e| CrawlerError::Config(format!("清空存储失败: {e}")))?;
    Ok(())
}

/// 沙盒资源占用视图(全部会话沙盒聚合;字段名保留以兼容前端,语义为累计值)。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SandboxStatsView {
    pub running: bool,
    /// 累计 CPU 时间,如 "12.3s"(Job 会计,非实时百分比)。
    pub cpu_perc: String,
    /// 峰值内存,如 "45.6 MB"。
    pub mem_usage: String,
    /// 存活进程数,如 "3 进程"(进程级隔离下无配额百分比,复用此字段传给前端展示)。
    pub mem_perc: String,
    /// 全部沙盒存储目录占用合计(字节;工作区文件持久存在,与 running 无关)。
    pub storage_bytes: u64,
    /// 内存上限(字节;None = 不限)。
    pub mem_limit_bytes: Option<u64>,
    /// 出站网络限速(KB/s;0 = 不限)。
    pub net_limit_kbps: u64,
    /// CPU 上限(百分比;0 = 不限)。
    pub cpu_limit_percent: u32,
    /// 进程数上限(0 = 不限)。
    pub max_processes: u32,
    /// 磁盘 IO 限速(KB/s;0 = 不限)。
    pub io_limit_kbps: u64,
}

/// 字节数格式化为人类可读(B/KB/MB/GB)。
fn human_bytes(n: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut v = n as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 {
        format!("{n} {}", UNITS[u])
    } else {
        format!("{v:.1} {}", UNITS[u])
    }
}

/// 读取全部会话沙盒的聚合资源占用(Job 会计:累计 CPU 秒数 + 峰值内存 + 存活进程数 + 存储占用)。
#[tauri::command]
pub async fn get_sandbox_stats(state: State<'_, AppState>) -> Result<SandboxStatsView> {
    let entries = state.sandbox.list();
    let mut cpu_secs = 0.0f64;
    let mut peak_mem = 0u64;
    let mut active = 0u32;
    let mut storage_bytes = 0u64;
    for e in &entries {
        cpu_secs += e.stats.cpu_secs;
        peak_mem += e.stats.peak_mem_bytes;
        active += e.stats.active_processes;
        storage_bytes += e.storage_bytes;
    }
    let running = active > 0;
    Ok(SandboxStatsView {
        running,
        cpu_perc: if running { format!("{cpu_secs:.1}s") } else { String::new() },
        mem_usage: if running { human_bytes(peak_mem) } else { String::new() },
        mem_perc: if running { format!("{active} 进程") } else { String::new() },
        storage_bytes,
        mem_limit_bytes: sandbox_memory_limit_bytes(&state.db).await,
        net_limit_kbps: sandbox_net_limit_bytes_per_sec(&state.db)
            .await
            .map(|b| b / 1024)
            .unwrap_or(0),
        cpu_limit_percent: sandbox_cpu_limit_percent(&state.db).await.unwrap_or(0),
        max_processes: sandbox_max_processes(&state.db).await.unwrap_or(0),
        io_limit_kbps: sandbox_io_limit_bytes_per_sec(&state.db)
            .await
            .map(|b| b / 1024)
            .unwrap_or(0),
    })
}

/// 停止全部沙盒:terminate 所有会话沙盒(整树杀净,含 dev server)并清空台账。
/// 工作区文件在宿主目录,不受影响;下次编程动作会惰性重建新沙盒。
#[tauri::command]
pub async fn sandbox_stop(state: State<'_, AppState>) -> Result<()> {
    // 先复位 dev server 状态(其进程挂在会话沙盒里,随下面的 terminate_all 一并死)
    stop_dev_inner(&state);
    state.sandbox.terminate_all();
    Ok(())
}

// ===================== 开发服务器预览(常驻进程) =====================

/// dev server 日志保留上限(行)。
const DEV_LOG_CAP: usize = 300;

/// 常驻开发服务器状态(如 `npm run dev`)。child 存句柄供停止;port 由输出解析。
#[derive(Default)]
pub struct DevServer {
    child: Option<tokio::process::Child>,
    /// 实际探测到的监听端口(日志解析 / TCP 探测得出);未知为 None。
    port: Option<u16>,
    /// 后端为本会话选定并注入命令的预览端口:供 `probe_dev_port` 精确探测该端口(免去全区间扫描)。
    intended_port: Option<u16>,
    command: String,
    running: bool,
    logs: Vec<String>,
    // 全局单实例,记当前 dev server 归属会话:供前端按 activeId 隔离,切到别的会话不串台显示
    conversation_id: String,
    // 启动代次:每次 start 自增;reader 仅在代次仍匹配时才据 EOF 置 running=false,
    // 避免「停旧→起新」时旧流的 EOF 把刚启动的新 server 误标为已停止
    generation: u64,
}

/// dev server 状态视图(给前端轮询)。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DevServerStatus {
    pub running: bool,
    pub port: Option<u16>,
    pub command: String,
    pub logs: Vec<String>,
    pub conversation_id: String,
}

/// 从一行输出里解析端口(匹配 localhost/127.0.0.1/0.0.0.0 后的端口号)。
fn parse_port(line: &str) -> Option<u16> {
    // 先剥离 ANSI 颜色码:Vite 等会把端口数字单独加粗着色,形如 `localhost:\x1b[1m5173`,
    // 不剥离时 `localhost:` 后紧跟的是转义码而非数字,会导致解析永远失败、预览卡在「正在探测端口」。
    let line = strip_ansi(line);
    for marker in ["localhost:", "127.0.0.1:", "0.0.0.0:"] {
        if let Some(idx) = line.find(marker) {
            let rest = &line[idx + marker.len()..];
            let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
            if let Ok(p) = digits.parse::<u16>() {
                return Some(p);
            }
        }
    }
    None
}

/// 剥离终端 ANSI 转义序列(CSI:`ESC [ … 终止字母`),让端口解析等纯文本处理不受着色干扰。
fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            // CSI 序列 `ESC [ 参数… 字母`:吞到结束字母(如 m / K)为止
            if chars.peek() == Some(&'[') {
                chars.next();
                for c2 in chars.by_ref() {
                    if c2.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
            // 其它形式的 ESC 转义:ESC 已吞掉,后续字符照常处理
        } else {
            out.push(c);
        }
    }
    out
}

/// 后台读取 dev server 的某个输出流:逐行入日志(限长)+ 解析端口;流结束置 running=false。
fn spawn_reader<R>(dev: Arc<Mutex<DevServer>>, stream: R, generation: u64)
where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
{
    tauri::async_runtime::spawn(async move {
        let mut lines = tokio::io::BufReader::new(stream).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let mut g = dev.lock().unwrap_or_else(|e| e.into_inner());
            // 已被新一轮 start 取代:旧 reader 退出,不再写新 server 的状态
            if g.generation != generation {
                return;
            }
            if g.port.is_none() {
                if let Some(p) = parse_port(&line) {
                    g.port = Some(p);
                }
            }
            g.logs.push(line);
            let len = g.logs.len();
            if len > DEV_LOG_CAP {
                g.logs.drain(0..len - DEV_LOG_CAP);
            }
        }
        // 流 EOF:进程多半已退出。仅当仍是本代次才标记停止,避免关掉已重启的新 server
        let mut g = dev.lock().unwrap_or_else(|e| e.into_inner());
        if g.generation == generation {
            g.running = false;
        }
    });
}

/// 停止当前 dev server(杀进程树 + 复位状态)。同步操作,不跨 await 持锁。
/// dev server 挂在该会话的会话级沙盒(Job)里:terminate 该 Job 即整树杀净(node 孙进程不残留、
/// 端口随之释放),随后把旧 Job 移出登记表——下次编程动作会惰性建一个全新 Job。
fn stop_dev_inner(state: &AppState) {
    let (child, conv_id) = {
        let mut g = state.dev_server.lock().unwrap_or_else(|e| e.into_inner());
        g.running = false;
        g.port = None;
        g.intended_port = None;
        (g.child.take(), std::mem::take(&mut g.conversation_id))
    };
    if let Some(mut child) = child {
        let _ = child.start_kill();
    }
    if !conv_id.is_empty() {
        // terminate 过的 Job 不能再接收新进程,故摘台账,下次编程动作惰性重建
        state.sandbox.remove(&conv_id);
    }
}

/// 内置静态预览服务器模板(node 内联脚本):无 package.json 的纯静态目录(单个 HTML 等)直接托管。
/// `__PORT__` 占位由 `static_server_js` 注入后端选定端口;绑 0.0.0.0 后宿主经 localhost:<port> 访问,
/// 带常见 MIME + 目录穿越防护,并打印 localhost:<port> 供端口探测。
/// 经 `node_eval_cmd` base64 内联执行(单引号包裹在 Windows cmd /C 下无效,历史 bug)。
const STATIC_SERVER_JS_TEMPLATE: &str = r#"const http=require("http"),fs=require("fs"),path=require("path");const root=process.cwd(),port=__PORT__;const M={".html":"text/html; charset=utf-8",".htm":"text/html; charset=utf-8",".css":"text/css",".js":"text/javascript",".mjs":"text/javascript",".json":"application/json",".svg":"image/svg+xml",".png":"image/png",".jpg":"image/jpeg",".jpeg":"image/jpeg",".gif":"image/gif",".webp":"image/webp",".ico":"image/x-icon",".woff":"font/woff",".woff2":"font/woff2",".ttf":"font/ttf",".txt":"text/plain; charset=utf-8",".map":"application/json"};http.createServer(function(req,res){var u=decodeURIComponent(req.url.split("?")[0]);var f=path.join(root,u);if(path.resolve(f).indexOf(path.resolve(root))!==0){res.statusCode=403;res.end("403");return;}try{if(fs.statSync(f).isDirectory())f=path.join(f,"index.html");}catch(e){}fs.readFile(f,function(e,d){if(e){res.statusCode=404;res.setHeader("Content-Type","text/plain; charset=utf-8");res.end("404 Not Found");return;}res.setHeader("Content-Type",M[path.extname(f).toLowerCase()]||"application/octet-stream");res.end(d);});}).listen(port,"0.0.0.0",function(){console.log("Static preview on http://localhost:"+port+"/");});"#;

/// 把静态服务器模板里的 `__PORT__` 替换为实际端口,生成可嵌入 `node -e '...'` 的脚本。
fn static_server_js(port: u16) -> String {
    STATIC_SERVER_JS_TEMPLATE.replace("__PORT__", &port.to_string())
}

/// 在工作区内探测「项目目录」:含 marker 文件(package.json / index.html)的目录。
/// 编程 Agent 常把项目建在子目录(如 my-app/),预览需在该目录起服务器而非工作区根。
/// 根目录命中直接返回 ".";否则向下最多找 3 层,跳过 node_modules/.git,已命中的项目目录不再
/// 往深处钻(其子目录是源码/依赖);多命中取最浅、再按路径字典序取最小(结果稳定)。
/// 返回相对工作区的目录路径("/" 分隔),无命中返回 None。
fn detect_project_dir(ws: &std::path::Path, marker: &str) -> Option<String> {
    if ws.join(marker).is_file() {
        return Some(".".to_string());
    }
    fn visit(
        dir: &std::path::Path,
        rel: String,
        depth: u32,
        marker: &str,
        best: &mut Option<(u32, String)>,
    ) {
        if depth > 3 {
            return;
        }
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for e in entries.flatten() {
            if !e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                continue;
            }
            let name = e.file_name().to_string_lossy().to_string();
            // 依赖/版本目录不钻;名字带双引号的没法安全拼进 cd "...",跳过
            if matches!(name.as_str(), "node_modules" | ".git") || name.contains('"') {
                continue;
            }
            let child_rel = if rel.is_empty() {
                name.clone()
            } else {
                format!("{rel}/{name}")
            };
            if e.path().join(marker).is_file() {
                let better = match best {
                    None => true,
                    Some((d, p)) => depth < *d || (depth == *d && child_rel < *p),
                };
                if better {
                    *best = Some((depth, child_rel));
                }
                continue;
            }
            visit(&e.path(), child_rel, depth + 1, marker, best);
        }
    }
    let mut best = None;
    visit(ws, String::new(), 1, marker, &mut best);
    best.map(|(_, rel)| rel)
}

/// 会话 id → 预览端口区间内的稳定偏移(FNV-1a 哈希取模):让每个程序有「自己的」相对固定端口,
/// 便于记忆 / 书签;同一会话每次预览倾向同一端口(占用时再顺延)。
fn conv_port_offset(conversation_id: &str) -> u16 {
    let mut hash: u32 = 2166136261; // FNV-1a 偏移基准
    for byte in conversation_id.as_bytes() {
        hash ^= *byte as u32;
        hash = hash.wrapping_mul(16777619);
    }
    (hash % HOST_PORT_SPAN as u32) as u16
}

/// 宿主某端口当前是否空闲:能成功 bind 127.0.0.1:port 即空闲(随即释放,仅做占用探测)。
fn host_port_free(port: u16) -> bool {
    std::net::TcpListener::bind(("127.0.0.1", port)).is_ok()
}

/// 为某会话挑选预览端口(满足「每个程序自定义端口 + 检查占用」):
/// 按会话 id 在区间内派生一个起点(每个程序相对固定的「自己的」端口),从起点环形扫描区间,
/// 返回第一个未被占用的端口——多个程序 / 残留进程不再挤同一端口;
/// 全被占用才回退派生端口(交给 `--strictPort` 给出明确报错,而非静默爬升到未知端口)。
fn pick_preview_port(conversation_id: &str) -> u16 {
    let offset = conv_port_offset(conversation_id);
    for i in 0..HOST_PORT_SPAN {
        let port = HOST_PORT_BASE + (offset + i) % HOST_PORT_SPAN;
        if host_port_free(port) {
            return port;
        }
    }
    HOST_PORT_BASE + offset % HOST_PORT_SPAN
}

/// 把命令里的预览端口统一改写为后端选定端口:替换 `--port <n>` / `--port=<n>` 的端口号,
/// 兼容 serve 的 `-l <n>` / `-l=<n>` / `--listen <n>` / `--listen=<n>`(无则原样返回)。
/// 前端默认 dev 命令固定带 `--port`、静态兜底命令带 `-l 5173`,替换后服务才监听在后端选定端口上,
/// 否则服务起了但 probe_dev_port 只探选定端口,永远探测不到、预览卡在「启动预览」按钮。
/// 静态服务器另由 `static_server_js` 直接注入端口,不走此函数。
fn apply_preview_port(command: &str, port: u16) -> String {
    const PORT_FLAGS: &[&str] = &["--port", "-l", "--listen"];
    let tokens: Vec<&str> = command.split_whitespace().collect();
    let mut out: Vec<String> = Vec::with_capacity(tokens.len());
    let mut i = 0;
    while i < tokens.len() {
        let token = tokens[i];
        if PORT_FLAGS.contains(&token) && i + 1 < tokens.len() {
            out.push(token.to_string());
            out.push(port.to_string());
            i += 2;
            continue;
        }
        if let Some(flag) = PORT_FLAGS
            .iter()
            .find(|f| token.starts_with(&format!("{f}=")))
        {
            out.push(format!("{flag}={port}"));
            i += 1;
            continue;
        }
        out.push(token.to_string());
        i += 1;
    }
    out.join(" ")
}

/// dev server 启动的全局串行锁。并发 start_dev_server(自动启动 + 手动连点、Tauri 命令不排队)
/// 会互踩:后一个调用的 stop 杀不掉「已 spawn 未登记」的进程 → 孤儿占住端口,后写者覆盖
/// DevServer 状态后自己又因端口被占而死 → EOF 把 running 翻成 false。最终状态:
/// 服务其实在跑(孤儿),状态却是「未运行」,UI 永远停在「启动预览」按钮、怎么点都没反应。
static DEV_START_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// 生成 `node -e` 执行内联脚本的命令:脚本 base64 后内联、外层用**双引号**包裹。
/// 不能直接 `node -e '<script>'`:单引号引用只在 sh 有效,Windows cmd /C 不认为单引号是引用,
/// 会把 `'` 原样传给 node → 直接语法错误退出(内置静态服务器因此在 Windows 上从未真正启动过)。
/// base64 字符集不含引号/空白,cmd 与 sh 下都安全。
fn node_eval_cmd(script: &str) -> String {
    use base64::Engine;
    let b64 = base64::engine::general_purpose::STANDARD.encode(script.as_bytes());
    format!("node -e \"eval(Buffer.from('{b64}','base64').toString('utf8'))\"")
}

/// 启动 / 重启开发服务器:在编程工作区内跑给定命令(如 `npm run dev`),常驻。
#[tauri::command]
pub async fn start_dev_server(
    state: State<'_, AppState>,
    conversation_id: String,
    command: String,
) -> Result<()> {
    let mut cmd = command.trim().to_string();
    if cmd.is_empty() {
        return Err(CrawlerError::Config("命令为空".into()));
    }
    // 串行化整个「停旧 → 选端口 → 起新 → 登记」过程,防并发调用互踩(见 DEV_START_LOCK 注释)
    let _start_guard = DEV_START_LOCK.lock().await;
    // 先解析执行环境(已有会话沙盒直接复用,不终止任何进程),拿到工作区以计算「最终要执行」的命令,
    // 供下面的幂等判断;真正需要(重)启动时才停旧并重新解析(见函数尾部)。
    let (ws, _) = resolve_exec(&state, &conversation_id).await?;

    // 为本会话挑选预览端口(每个程序自定义端口 + 检查占用,避免多程序 / 残留进程撞 5173)。
    // 本会话已有在跑的 dev server 时复用其既定端口:既不打断正在用的预览,
    // 也让下面「同命令幂等」的比较成立(重选端口必然不等 → 连点永远重启、永远起不来)。
    let port = {
        let g = state.dev_server.lock().unwrap_or_else(|e| e.into_inner());
        match g
            .intended_port
            .filter(|_| g.running && g.conversation_id == conversation_id)
        {
            Some(p) => p,
            None => pick_preview_port(&conversation_id),
        }
    };

    // npm/npx/yarn/vite 这类命令依赖 package.json(npx serve 等静态兜底命令也走这里:
    // 无 package.json 时自动改用内置静态服务器,免去 npx 冷下载的数十秒等待)。
    // 项目目录探测直接在 Rust 侧读工作区目录
    // (此前的 shell 探测用 POSIX 语法,Windows 走 cmd /C 根本不生效,恒落空):
    // 有 package.json(根或子目录)→ 按原命令,先 cd 进项目目录再跑——编程 Agent 常把项目建在
    // 子目录(如 my-app/),此前必须手动改命令带 cd 才能预览;有文件但找不到 package.json
    // (纯静态,如单个 HTML)→ 自动改用内置静态服务器托管 index.html 所在目录;空目录 → 直接报错,
    // 而不是让 npm 吐一长串 ENOENT。
    let mut is_static = false;
    let needs_pkg = cmd.contains("npm") || cmd.contains("npx") || cmd.contains("yarn") || cmd.contains("vite");
    // 启动命令的工作目录(相对工作区):命中项目子目录时先 cd 进去
    let project_dir: Option<String>;
    if needs_pkg {
        let ws_empty = std::fs::read_dir(&ws)
            .map(|mut it| it.next().is_none())
            .unwrap_or(false);
        if ws_empty {
            return Err(CrawlerError::Config(
                "预览启动失败:该会话工作区是空的,没有可预览的内容。\
                 先让编程 Agent 生成文件——前端项目用 `npm create vite`、纯静态写个 index.html——再点预览。"
                    .into(),
            ));
        }
        project_dir = match detect_project_dir(&ws, "package.json") {
            Some(dir) => Some(dir),
            // 无 package.json → 纯静态目录,内置 node 静态服务器托管 index.html 所在目录(单个 HTML 也能预览)
            None => {
                is_static = true;
                cmd = node_eval_cmd(&static_server_js(port));
                detect_project_dir(&ws, "index.html").or_else(|| Some(".".to_string()))
            }
        };
    } else {
        // 自定义命令(如 python -m http.server):同样尽量落到项目目录里跑
        project_dir = detect_project_dir(&ws, "package.json")
            .or_else(|| detect_project_dir(&ws, "index.html"));
    }

    // 非静态命令(npm/vite 等):把命令里的 `--port` 改写为后端选定端口;静态服务器已注入端口,跳过。
    if !is_static {
        cmd = apply_preview_port(&cmd, port);
    }
    // 项目在工作区子目录:先 cd 进去再启动(cmd /C 与 sh -c 都支持 `cd "dir" && ...` 链式写法)
    if let Some(dir) = &project_dir {
        if dir != "." {
            cmd = format!("cd \"{dir}\" && {cmd}");
        }
    }

    // 幂等:同会话、同最终命令、已在运行(含正在启动、端口尚未探测到)的 dev server 直接复用,
    // 不重启。此前每次调用都先杀后启——`npx` 冷下载要数十秒,用户连点 / 自动启动叠加手动点击
    // 等于每次都把还没下完的上一次杀掉重头来,预览永远起不来(审计里同一秒 5 连 spawn 即此)。
    {
        let g = state.dev_server.lock().unwrap_or_else(|e| e.into_inner());
        if g.running && g.conversation_id == conversation_id && g.command == cmd {
            return Ok(());
        }
    }
    // 确实要(重)启动:先停旧(连带 terminate 会话沙盒,node 孙进程一并杀净)——
    // 上面 resolve_exec 复用的会话沙盒随这一步作废,故停完必须重新解析出全新沙盒再 spawn,
    // 否则新进程会被 assign 进一个已 terminate 的 Job。
    stop_dev_inner(&state);
    let (_, exec) = resolve_exec(&state, &conversation_id).await?;

    let mut launcher = coding::build_exec_command(&exec, &ws, &cmd);
    launcher
        .kill_on_drop(false)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let mut child = launcher
        .spawn()
        .map_err(|e| CrawlerError::Config(format!("启动开发服务器失败: {e}")))?;
    // 挂进会话级沙盒:stop / 停止沙盒 / 退出应用时整树杀净(失败仅降级为无隔离,告警不阻断)
    if let Some(pid) = child.id() {
        if let Err(e) = exec.sandbox.assign_pid(pid) {
            tracing::warn!("dev server 挂入沙盒失败(降级为无隔离执行): {e}");
        }
    }
    // 长驻进程:spawn 时记一条审计(exit_code 为 null,结束不补记)
    crate::sandbox::audit(&state.config_dir, &conversation_id, &cmd, None, 0, false);
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let generation;
    {
        let mut g = state.dev_server.lock().unwrap_or_else(|e| e.into_inner());
        g.generation = g.generation.wrapping_add(1); // 新代次,旧流 EOF 不再影响本 server
        generation = g.generation;
        g.child = Some(child);
        g.port = None;
        g.intended_port = Some(port); // 记后端选定端口,供 probe_dev_port 精确探测
        g.command = cmd;
        g.running = true;
        g.logs.clear();
        g.conversation_id = conversation_id.clone();
    }
    if let Some(out) = stdout {
        spawn_reader(state.dev_server.clone(), out, generation);
    }
    if let Some(err) = stderr {
        spawn_reader(state.dev_server.clone(), err, generation);
    }
    Ok(())
}

/// 停止开发服务器(连带 terminate 会话沙盒,整树杀净不留端口占用)。
#[tauri::command]
pub async fn stop_dev_server(state: State<'_, AppState>) -> Result<()> {
    stop_dev_inner(&state);
    Ok(())
}

/// 主动探测监听端口:返回第一个能建立 TCP 连接的端口。
/// 兜底用——dev server 的就绪 banner 可能被缓冲 / 着色吞掉,日志解析不到端口,
/// 但服务确在 0.0.0.0:<port> 监听,直接连宿主回环即可定位,避免预览永远卡在「正在探测端口」。
/// 已知后端选定端口(intended)时只精确探测它;未知则回退扫描预览端口区间。
async fn probe_dev_port(intended: Option<u16>) -> Option<u16> {
    let candidates: Vec<u16> = match intended {
        Some(p) => vec![p],
        None => (HOST_PORT_BASE..HOST_PORT_BASE + HOST_PORT_SPAN).collect(),
    };
    for p in candidates {
        let connected = tokio::time::timeout(
            std::time::Duration::from_millis(250),
            tokio::net::TcpStream::connect(("127.0.0.1", p)),
        )
        .await
        .ok()
        .and_then(|r| r.ok())
        .is_some();
        if connected {
            return Some(p);
        }
    }
    None
}

/// 查询开发服务器状态(运行中 / 端口 / 命令 / 最近日志)。
#[tauri::command]
pub async fn get_dev_server_status(state: State<'_, AppState>) -> Result<DevServerStatus> {
    // 先取快照即释放锁(std Mutex 绝不跨 await 持有)
    let (running, port, intended, command, logs, conversation_id) = {
        let g = state.dev_server.lock().unwrap_or_else(|e| e.into_inner());
        (
            g.running,
            g.port,
            g.intended_port,
            g.command.clone(),
            g.logs.clone(),
            g.conversation_id.clone(),
        )
    };
    // 日志没解析到端口(banner 被缓冲 / 着色吞掉)→ 主动探测兜底,并回填(仅当仍是同一运行实例)
    if running && port.is_none() {
        if let Some(p) = probe_dev_port(intended).await {
            let mut g = state.dev_server.lock().unwrap_or_else(|e| e.into_inner());
            if g.running && g.port.is_none() {
                g.port = Some(p);
            }
            return Ok(DevServerStatus { running, port: Some(p), command, logs, conversation_id });
        }
    }
    Ok(DevServerStatus { running, port, command, logs, conversation_id })
}

/// 关键词启发式分类:覆盖 coding / rpa / computer / local 信号,其余归 chat。
fn classify_by_keywords(text: &str) -> &'static str {
    let lower = text.to_lowercase();
    // 浏览器自动化(RPA)信号优先判:这些词较明确,且可能混入 coding 词(打开 / 运行)
    const RPA_SIGNALS: &[&str] = &[
        "浏览器自动", "网页自动", "自动点击", "自动填写", "自动填表", "模拟点击",
        "网页操作", "网站上", "抓取网页", "爬取网页", "自动化浏览", "帮我打开网页", "rpa",
    ];
    if RPA_SIGNALS.iter().any(|k| lower.contains(k)) {
        return "rpa";
    }
    // 网页任务再判:命中「具体网站/平台名」或「网页语境」且带「网页动作」→ RPA(本应用 RPA = 内嵌浏览器操作网页)。
    // 覆盖「打开抖音搜索…」「在淘宝查…」「访问某网址并…」这类——纯关键词难穷举,故用 站点/语境 × 动作 组合判定。
    const SITE_NAMES: &[&str] = &[
        "抖音", "快手", "小红书", "淘宝", "天猫", "京东", "拼多多", "闲鱼", "微博", "知乎",
        "豆瓣", "哔哩", "bilibili", "b站", "百度", "谷歌", "google", "bing", "youtube",
        "tiktok", "今日头条", "头条", "美团", "大众点评", "携程", "12306", "公众号", "网易",
        "搜狐", "新浪", "优酷", "腾讯视频",
    ];
    const WEB_CONTEXT: &[&str] = &[
        "网页", "网站", "官网", "网址", "url", "http", "www.", ".com", ".cn", ".net",
        "浏览器", "平台", "页面",
    ];
    const WEB_ACTION: &[&str] = &[
        "打开", "访问", "进入", "登录", "搜索", "搜一下", "查一下", "查找", "浏览", "点击",
        "填写", "下单", "购买", "抓取", "爬取", "采集", "翻页", "滚动", "评论", "点赞", "关注",
    ];
    let has_web_action = WEB_ACTION.iter().any(|k| lower.contains(k));
    if has_web_action
        && (SITE_NAMES.iter().any(|k| lower.contains(k))
            || WEB_CONTEXT.iter().any(|k| lower.contains(k)))
    {
        return "rpa";
    }
    // 本机助手(文件 / 进程 / 终端,纯文本工具、不看屏):优先于 computer 与 coding 判定,
    // 让「读写删本机文件 / 查杀进程 / 跑命令」这类**不看屏**的请求落到 local,而非 GUI computer 或沙箱 coding。
    const LOCAL_SIGNALS: &[&str] = &[
        // 文件 / 磁盘
        "本机文件", "本地文件", "读取文件", "写入文件", "写文件", "删除文件", "移动文件",
        "重命名文件", "复制文件", "查找文件", "列目录", "列出目录", "新建文件夹", "建文件夹",
        "d盘", "c盘", "e盘", "f盘", "磁盘", "我的电脑", "此电脑", "多少文件", "多少个文件",
        "文件数量", "统计文件",
        // 进程 / 系统
        "进程", "任务管理器", "结束进程", "杀进程", "查进程", "进程列表", "环境变量",
        "系统信息",
        // 终端(限定「本机/本地」语境,避免抢走 coding 的工作区终端请求)
        "本机终端", "本地终端", "本机命令", "本地命令", "在本机", "在本地",
    ];
    if LOCAL_SIGNALS.iter().any(|k| lower.contains(k)) {
        return "local";
    }
    // 电脑操作(GUI:看屏 / 鼠标键盘 / 窗口 / 控件 / 启动程序),优先于 coding——
    // 避免"打开 / 运行"这类词被 coding 信号吃掉。文件 / 进程 / 终端类已在上面归 local。
    const COMPUTER_SIGNALS: &[&str] = &[
        "截图", "截屏", "屏幕", "桌面", "鼠标", "键盘", "剪贴板", "打开程序", "启动程序",
        "打开软件", "打开应用", "切换窗口", "关闭窗口",
        "电脑操作", "操作电脑", "控制电脑", "识别屏幕", "看屏幕",
    ];
    if COMPUTER_SIGNALS.iter().any(|k| lower.contains(k)) {
        return "computer";
    }
    if lower.contains("```") {
        return "coding";
    }
    const CODING_SIGNALS: &[&str] = &[
        "代码", "脚本", "函数", "报错", "编译", "调试", "重构", "算法", "正则",
        "命令行", "终端", "跑一下", "运行一下", "执行命令", "写个", "实现一个",
        "修复", "bug", "python", "rust", "golang", "java", "kotlin", "typescript",
        "javascript", "react", "vue", "sql", "shell", "terminal", "git ", "npm ",
        "cargo ", "bun ", "pip ", "def ", "class ", "function ", "import ",
        "#include", "console.log", "print(", ".py", ".rs", ".ts", ".js", ".java",
        ".go", ".sh",
    ];
    if CODING_SIGNALS.iter().any(|k| lower.contains(k)) {
        "coding"
    } else {
        "chat"
    }
}

/// LLM 兜底路由提示词:把每个 Agent 写成「负责什么 + 何时选 + 何时别选」三段式(负向边界防误路由)。
/// description 几乎等于路由准确率本身,优先打磨这里的「不要选我」部分。
const ROUTER_PROMPT: &str = "你是一个意图路由器。把用户这句话归到下面某一个助手,只输出它的英文 key,不要任何解释或标点。\n\
- chat:普通对话 / 知识问答 / 闲聊 / 写作建议。用户只是聊天、问知识、要想法时选它;要在本机或网页上「实际执行操作」时不要选它。\n\
- coding:写 / 改 / 调试代码,在隔离工作区里跑项目命令。涉及代码、脚本、报错、某编程语言或框架时选它;只是操作本机文件 / 进程或网页时不要选它。\n\
- rpa:在内嵌浏览器里自动操作网页(打开网站、点按钮、填表单、抓页面数据)。任务发生在某个网站 / 网页上时选它;是本机文件 / 程序操作、不涉及网页时不要选它。\n\
- computer:看屏幕 + 操作鼠标键盘 / 窗口 / 控件(GUI 自动化)。需要截图看屏、点桌面程序的按钮、操作窗口时选它;只是读写文件 / 查进程 / 跑命令(不看屏)时不要选它。\n\
- local:本机文件 / 进程 / 终端操作(读写删文件、查 / 杀进程、跑命令行)。在本机管理文件、查系统、跑命令时选它;任务在网页里、或需要看屏点 GUI 时不要选它。\n\
只输出一个 key:chat、coding、rpa、computer 或 local。";

/// 关键词落到 chat 时,判断这句话是否「像个可执行任务」(值得用 LLM 复核),而非纯闲聊 / 知识问答。
/// 用一小撮祈使动作词作门槛:纯问候 / 知识问题不带这些词 → 直接走 chat,不浪费一次 LLM 往返。
fn looks_actionable(lower: &str) -> bool {
    const ACTION_HINTS: &[&str] = &[
        "帮我", "帮忙", "打开", "运行", "执行", "启动", "查一下", "查询", "查找", "搜一下",
        "搜索", "找一下", "下载", "安装", "生成", "写个", "写一个", "创建", "新建", "删除",
        "整理", "统计", "操作", "处理", "把", "给我", "自动",
    ];
    ACTION_HINTS.iter().any(|k| lower.contains(k))
}

/// 从 LLM 输出里解析路由标签(容忍多余文字 / 标点):命中 5 个 key 之一返回,否则 None。
/// 各 key 互不为子串,按出现即取,顺序不影响正确性。
fn parse_route_label(content: &str) -> Option<&'static str> {
    let lower = content.to_lowercase();
    ["coding", "computer", "local", "rpa", "chat"]
        .into_iter()
        .find(|k| lower.contains(k))
}

/// LLM 兜底意图分类:把 5 个 Agent 当「带三段式 description 的选项」让模型二选一。
/// 用会话选用的厂商 / 模型做一次低温短输出;调用 / 解析失败返回 None,由调用方回退关键词结果。
async fn llm_route_tiebreak(
    db: &sea_orm::DatabaseConnection,
    provider_id: &str,
    model: &str,
    text: &str,
) -> Option<&'static str> {
    let provider = provider_entity::Entity::find_by_id(provider_id.to_string())
        .one(db)
        .await
        .ok()??;
    let provider_ref = ProviderRef {
        kind: ProviderKind::from_code(&provider.code),
        api_url: provider.api_url,
        api_key: provider.api_key,
        model: model.to_string(),
    };
    let messages = [
        ChatMsg::System(ROUTER_PROMPT.to_string()),
        ChatMsg::User(text.to_string()),
    ];
    // 低温 + 极短输出:只要一个 key
    let options = LlmOptions {
        temperature: Some(0.0),
        max_tokens: Some(16),
    };
    let resp = provider_for(provider_ref.kind)
        .chat(LlmRequest {
            provider: &provider_ref,
            messages: &messages,
            tools: &[],
            options: &options,
            cancel: None,
        })
        .await
        .ok()?;
    parse_route_label(resp.content.as_deref().unwrap_or_default())
}

/// 意图分类:判断首条消息走哪个 Agent,返回 "coding" / "rpa" / "computer" / "local" / "chat"。
/// 混合策略:先关键词(零延迟);仅当关键词落到 chat、且这句话像个「可执行任务」时,才用一次
/// LLM 在「agent 即 tool(三段式 description)」里二选一——把 LLM 成本只花在不确定的少数 case,
/// 既保留首条响应快、又兜住无明显关键词的 agent 任务。每次决策都落 agent_route_logs 遥测。
#[tauri::command]
pub async fn classify_agent_type(
    state: State<'_, AppState>,
    text: String,
    provider_id: Option<String>,
    model: Option<String>,
) -> Result<String> {
    let keyword = classify_by_keywords(&text);
    let lower = text.to_lowercase();

    // 兜底闸门:仅关键词没命中任何领域(chat)、且像可执行任务时,才用 LLM 复核
    let mut llm_route: Option<&'static str> = None;
    let mut used_model = String::new();
    if keyword == "chat" && looks_actionable(&lower) {
        if let (Some(pid), Some(m)) = (provider_id.as_deref(), model.as_deref()) {
            if let Some(route) = llm_route_tiebreak(&state.db, pid, m, &text).await {
                llm_route = Some(route);
                used_model = m.to_string();
            }
        }
    }
    let final_route = llm_route.unwrap_or(keyword);

    // 落遥测(失败仅忽略,不影响路由):路由优化的唯一抓手
    let owner = current_user(&state).map(|u| u.name).unwrap_or_default();
    let _ = agent_route_log::Model::record(
        &state.db,
        &owner,
        &text,
        keyword,
        llm_route,
        final_route,
        &used_model,
    )
    .await;

    Ok(final_route.to_string())
}

/// 一条路由遥测的对外视图(字段对应前端 camelCase)。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RouteLogView {
    pub id: i64,
    pub text: String,
    pub keyword_route: String,
    pub llm_used: bool,
    pub llm_route: String,
    pub final_route: String,
    pub model: String,
    pub owner: String,
    pub created_at: i64,
}

/// 列出最近的意图路由遥测(默认 200 条、上限 2000,按时间倒序),供分析路由准确率 / 排查误路由。
#[tauri::command]
pub async fn list_agent_route_logs(
    state: State<'_, AppState>,
    limit: Option<u64>,
) -> Result<Vec<RouteLogView>> {
    let rows = agent_route_log::Entity::find()
        .order_by_desc(agent_route_log::Column::Id)
        .limit(limit.unwrap_or(200).min(2000))
        .all(&state.db)
        .await
        .map_err(|e| CrawlerError::Config(format!("读取路由遥测失败: {e}")))?;
    Ok(rows
        .into_iter()
        .map(|r| RouteLogView {
            id: r.id,
            text: r.text,
            keyword_route: r.keyword_route,
            llm_used: r.llm_used,
            llm_route: r.llm_route,
            final_route: r.final_route,
            model: r.model,
            owner: r.owner,
            created_at: r.created_at,
        })
        .collect())
}

/// Coding Agent 钩子:处理 Plan/Act 模式、自主续航、自动修复、验证闸门等。
struct CodingHooks {
    db: sea_orm::DatabaseConnection,
    conversation_id: String,
    /// 本回合的取消令牌:on_iter_end 每步检查(双保险;步内取消由流式读循环 select! 即时响应)。
    cancel_token: tokio_util::sync::CancellationToken,
    autonomous: bool,
    auto_fix_used: usize,
    continue_used: usize,
    last_run_failed: bool,
    goal_done: bool,
    finish_summary: Option<String>,
    code_edited_since_verify: bool,
    verify_gate_used: usize,
    latest_todos: Value,
}

impl CodingHooks {
    /// 本回合是否已被请求停止(令牌每回合新建,无需消费语义)。
    fn is_cancelled(&self) -> bool {
        self.cancel_token.is_cancelled()
    }
}

// on_before_tool 不覆盖(用默认放行),故无需 async_trait;其余钩子均为同步。
impl ReactHooks for CodingHooks {
    fn on_after_tool(&mut self, name: &str, args: &Value, result: &ToolResult) -> ToolPostAction {
        // 追踪 run_command 失败
        if name == "run_command" && result.is_error {
            self.last_run_failed = true;
        }
        // 追踪「改了代码但还没成功验证」
        if !result.is_error {
            match name {
                "write_file" | "replace_in_file" => {
                    if let Some(path) = args.get("path").and_then(Value::as_str) {
                        if coding::is_code_file(path) {
                            self.code_edited_since_verify = true;
                        }
                    }
                }
                "run_command" => self.code_edited_since_verify = false,
                _ => {}
            }
        }
        // 拦截 update_plan:把模型给的完整 todo 清单落库到会话
        if name == "update_plan" && !result.is_error {
            if let Some(todos) = args.get("todos") {
                self.latest_todos = todos.clone();
                let db = self.db.clone();
                let cid = self.conversation_id.clone();
                let todos_str = todos.to_string();
                tauri::async_runtime::spawn(async move {
                    let _ = conv::Entity::update_many()
                        .col_expr(conv::Column::PlanTodos, sea_orm::sea_query::Expr::value(todos_str))
                        .filter(conv::Column::Id.eq(cid))
                        .exec(&db)
                        .await;
                });
            }
        }
        // 拦截 finish:模型显式声明整个任务完成
        if name == "finish" && !result.is_error {
            self.goal_done = true;
            self.finish_summary = args
                .get("summary")
                .and_then(Value::as_str)
                .map(str::to_string);
        }
        ToolPostAction::Continue
    }

    fn on_model_finish(&mut self, content: Option<String>) -> FinishDecision {
        // run_command 失败后自动修复
        if self.last_run_failed && self.auto_fix_used < AUTO_FIX_MAX {
            self.auto_fix_used += 1;
            self.last_run_failed = false;
            return FinishDecision::ContinueWithPrompt(coding::auto_fix_prompt(self.auto_fix_used));
        }
        // 自主续航:模型过早收尾但计划仍有未完成项
        if self.autonomous
            && !self.goal_done
            && has_unfinished_todos(&self.latest_todos)
            && self.continue_used < MAX_CONTINUES
        {
            self.continue_used += 1;
            return FinishDecision::ContinueWithPrompt(coding::auto_continue_prompt());
        }
        FinishDecision::Finish(content.unwrap_or_default())
    }

    fn on_iter_end(&mut self, iter: usize) -> IterDecision {
        let max_iters = if self.autonomous { MAX_AUTO_ITERS } else { MAX_ITERS };
        // 用户手动停止:每步检查取消令牌,命中则优雅收尾(不强杀,保证落库一致)。
        // 旧实现把此检查丢在循环外只跑一次,导致「停止」按钮对续航中的 Agent 失效。
        if self.is_cancelled() {
            return IterDecision::Finish("已按用户请求停止。".to_string());
        }
        // 模型声明完成 → 收尾
        if self.goal_done {
            // 硬验证闸门:改了代码却没成功跑过验证就想收尾 → 注入验证提示,逼它先验证再收尾
            if self.code_edited_since_verify && self.verify_gate_used < MAX_VERIFY_GATE {
                self.verify_gate_used += 1;
                self.goal_done = false;
                self.finish_summary = None;
                return IterDecision::InjectAndContinue(coding::verify_before_finish_prompt());
            }
            return IterDecision::Finish(
                self.finish_summary
                    .clone()
                    .filter(|s| !s.trim().is_empty())
                    .unwrap_or_else(|| "任务已完成。".to_string()),
            );
        }
        // 达上限
        if iter == max_iters - 1 {
            return IterDecision::Finish(if self.autonomous {
                format!("(已达自主续航上限 {max_iters} 步,先停下。可继续追问以推进。)")
            } else {
                format!("(已达最大步数 {max_iters},已停止。可继续追问以推进。)")
            });
        }
        IterDecision::Continue
    }
}

/// 发送一条用户消息,驱动编程 Agent 的 ReAct 循环;过程逐步落库 + 推 `agent-step` 进度事件,
/// 返回最终的 assistant 消息(前端在 resolve 后重载消息以渲染完整工具往返)。
#[tauri::command]
pub async fn send_coding_message(
    state: State<'_, AppState>,
    app: AppHandle,
    conversation_id: String,
    content: String,
    mode: Option<String>,
) -> Result<MessageView> {
    // Plan / Act 临时态:仅本轮生效,不持久化、不入库。缺省(旧前端不传)走 Act 向后兼容。
    let agent_mode = coding::AgentMode::from_code(mode.as_deref().unwrap_or("act"));
    let me = current_user(&state).ok_or_else(|| CrawlerError::Config("未登录".into()))?;
    let text = content.trim().to_string();
    if text.is_empty() {
        return Err(CrawlerError::Config("消息内容为空".into()));
    }
    // 前奏(归属 / api_key 校验 + 首轮判定 + 落库 user 消息)统一走 core::shared
    let (conversation, provider, had_messages) =
        begin_agent_turn(&state.db, &me.name, &conversation_id, &text).await?;

    // 准备本会话工作区 + 执行环境(本地进程沙盒,首个动作惰性创建)+ 工具注册表
    let (workspace, exec) = resolve_exec(&state, &conversation_id).await?;
    // 本轮开始前打检查点(git commit,带本轮提问作标签),供版本回退识别快照对应哪次任务
    coding::checkpoint(&workspace, &exec, &text).await;
    let registry = coding::build_registry(workspace, exec, agent_mode);

    // 构建上下文:系统提示词 + 滚动摘要 + live 原文窗口
    let mut messages: Vec<ChatMsg> = Vec::new();
    // 按模式选系统提示词:Plan 只引导出方案(配合 build_registry 只挂只读工具),Act 亲自动手
    let system_prompt = match agent_mode {
        coding::AgentMode::Plan => coding::PLAN_SYSTEM_PROMPT,
        coding::AgentMode::Act => coding::SYSTEM_PROMPT,
    };
    messages.push(ChatMsg::System(system_prompt.to_string()));
    // 用户可编辑的附加规范(<config_dir>/agent-guidelines/coding.md):有则注入
    if let Some(g) = load_agent_guidelines(&state.config_dir, "coding").await {
        messages.push(ChatMsg::System(format!("【附加规范(用户自定义,务必遵守)】\n{g}")));
    }
    // 会话滚动摘要
    if let Some(sys) = conv_summary::summary_system_message(&conversation.summary) {
        if let Some(summary_text) = sys.get("content").and_then(|v| v.as_str()) {
            messages.push(ChatMsg::System(summary_text.to_string()));
        }
    }
    // Act 模式且已有计划:注入当前 todo 清单
    if matches!(agent_mode, coding::AgentMode::Act) {
        if let Some(plan_sys) = coding::plan_system_message(&conversation.plan_todos) {
            messages.push(ChatMsg::System(plan_sys));
        }
    }
    // live 原文窗口
    messages.extend(live_windowed_messages(&state.db, &conversation).await?);

    let provider_ref = ProviderRef {
        kind: ProviderKind::from_code(&provider.code),
        api_url: provider.api_url.clone(),
        api_key: provider.api_key.clone(),
        model: conversation.model.clone(),
    };

    // 自主续航:Act 模式默认开启
    let autonomous = matches!(agent_mode, coding::AgentMode::Act);
    let max_iters = if autonomous { MAX_AUTO_ITERS } else { MAX_ITERS };
    // 取消令牌:每回合新建(无残留问题),守卫在回合结束(含错误路径)自动摘除
    let (cancel_token, _cancel_guard) =
        crate::agent::core::shared::begin_cancel_token(&state.cancel_tokens, &conversation_id);

    let config = ReactConfig {
        max_iters,
        temperature: 0.2, // 低温:编程 Agent 要精准、确定、可复现的代码与工具调用
        enable_streaming: true, // 启用流式输出
        context_window_size: 120, // 编程场景需要更大的上下文窗口
        enable_parallel_tools: false, // 编程场景禁用并行，确保执行顺序
        max_retries: 2, // LLM 调用失败时重试 2 次
        auto_fix_on_tool_error: false, // 编程场景已有自己的自动修复逻辑
    };

    let mut hooks = CodingHooks {
        db: state.db.clone(),
        conversation_id: conversation_id.clone(),
        cancel_token: cancel_token.clone(),
        autonomous,
        auto_fix_used: 0,
        continue_used: 0,
        last_run_failed: false,
        goal_done: false,
        finish_summary: None,
        code_edited_since_verify: false,
        verify_gate_used: 0,
        latest_todos: serde_json::from_str(&conversation.plan_todos).unwrap_or(Value::Null),
    };

    let result = crate::agent::core::react::react_run(
        &state.db,
        &app,
        &conversation_id,
        &provider_ref,
        config,
        &mut hooks,
        &registry,
        &mut messages,
        Some(&cancel_token),
    )
    .await?;

    // 记录 token 用量(多步 ReAct 累计;source=coding 供账单按场景拆分)
    let _ = veltrix_core::db::entity::model_usage_record::Model::record(
        &state.db,
        &conversation.model,
        &provider.id,
        result.usage.prompt,
        result.usage.completion,
        "coding",
        &me.name,
    )
    .await;

    // 落库最终 assistant 消息
    let final_msg = insert_final_assistant(
        &state.db,
        &conversation_id,
        result.final_text,
        result.final_reasoning,
    )
    .await?;

    // 更新会话时间;首轮用用户首句起标题
    finalize_conversation_meta(&state.db, conversation, had_messages, &text).await;

    // 滚动摘要维护
    spawn_coding_summary_maintenance(&state.db, &conversation_id, provider_ref);

    Ok(final_msg.into())
}

/// 把一段编程任务作为「子任务」在指定会话(通常是编排器会话)下跑完,返回最终文本。供编排器委派工具调用。
/// 与 send_coding_message 的区别:固定 Act + 自主续航、不带会话历史(仅 system+task)、不落最终消息 / 不收尾。
#[allow(clippy::too_many_arguments)]
pub async fn run_coding_subtask(
    db: &sea_orm::DatabaseConnection,
    app: &AppHandle,
    exec_ctx: &CodingExecCtx,
    config_dir: &Path,
    cancel_token: &tokio_util::sync::CancellationToken,
    conversation_id: &str,
    owner: &str,
    provider_ref: &ProviderRef,
    provider_id: &str,
    task: &str,
) -> Result<String> {
    // 惰性解析执行环境(首个编程动作才创建会话沙盒)
    let (workspace, exec) = resolve_exec_ctx(exec_ctx, conversation_id).await?;
    coding::checkpoint(&workspace, &exec, task).await;
    let registry = coding::build_registry(workspace, exec, coding::AgentMode::Act);

    let mut messages: Vec<ChatMsg> = vec![ChatMsg::System(coding::SYSTEM_PROMPT.to_string())];
    if let Some(g) = load_agent_guidelines(config_dir, "coding").await {
        messages.push(ChatMsg::System(format!("【附加规范(用户自定义,务必遵守)】\n{g}")));
    }
    messages.push(ChatMsg::User(task.to_string()));

    let config = ReactConfig {
        max_iters: MAX_AUTO_ITERS,
        temperature: 0.2,
        enable_streaming: true,
        context_window_size: 120,
        enable_parallel_tools: false,
        max_retries: 2,
        auto_fix_on_tool_error: false,
    };
    let mut hooks = CodingHooks {
        db: db.clone(),
        conversation_id: conversation_id.to_string(),
        cancel_token: cancel_token.clone(),
        autonomous: true,
        auto_fix_used: 0,
        continue_used: 0,
        last_run_failed: false,
        goal_done: false,
        finish_summary: None,
        code_edited_since_verify: false,
        verify_gate_used: 0,
        latest_todos: Value::Null,
    };
    let result = crate::agent::core::react::react_run(
        db, app, conversation_id, provider_ref, config, &mut hooks, &registry, &mut messages,
        Some(cancel_token),
    )
    .await?;
    let _ = veltrix_core::db::entity::model_usage_record::Model::record(
        db,
        &provider_ref.model,
        provider_id,
        result.usage.prompt,
        result.usage.completion,
        "coding",
        owner,
    )
    .await;
    Ok(result.final_text)
}

/// 请求停止某会话正在自主续航的编程 Agent;取消令牌触发后流式读循环立即中断、
/// 迭代间检查点优雅收尾(保证落库一致)。幂等:该会话无进行中回合时安全忽略。
#[tauri::command]
pub fn stop_coding_agent(state: State<'_, AppState>, conversation_id: String) -> Result<()> {
    let tokens = state
        .cancel_tokens
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    if let Some(token) = tokens.get(&conversation_id) {
        token.cancel();
    }
    Ok(())
}

/// 计划里是否还有未完成项(自主续航判定用;无计划 / 解析失败视为「无未完成」,不强制续写)。
fn has_unfinished_todos(todos: &Value) -> bool {
    todos
        .as_array()
        .map(|arr| {
            arr.iter()
                .any(|t| !t.get("done").and_then(Value::as_bool).unwrap_or(false))
        })
        .unwrap_or(false)
}

/// 把编程会话的滚动摘要维护放到后台 spawn 执行,避免阻塞回复返回。
/// 摘要属杂活,优先走 Summary 角色单独配置的便宜模型;未配置则回退会话模型(fallback)。
/// 复用 chat 的 `maintain_conversation_summary`,但带 coding 强化提示:额外保留
/// 已创建 / 修改文件清单、关键命令及结果、未决报错 / 待办——这些是编程会话续接的关键上下文。
fn spawn_coding_summary_maintenance(
    db: &sea_orm::DatabaseConnection,
    conversation_id: &str,
    fallback: ProviderRef,
) {
    let db = db.clone();
    let conversation_id = conversation_id.to_string();
    tauri::async_runtime::spawn(async move {
        let p =
            crate::commands::resolve_role_provider(&db, crate::llm::AgentRole::Summary, fallback)
                .await;
        // coding 强化提示:让摘要额外保留对续接编程任务最有用的状态
        const CODING_HINT: &str =
            "已创建 / 修改的文件清单、执行过的关键命令及其结果(成功 / 失败)、当前未决的报错与待办事项,\
以及【已踩过的坑及其解决办法】——哪些命令 / 改法失败过、根因是什么、最终如何修好(供后续避免重犯同一错误)";
        conv_summary::maintain_conversation_summary(
            &db,
            &conversation_id,
            &p.api_url,
            &p.api_key,
            &p.model,
            CODING_HINT,
        )
        .await;
    });
}

// 消息行 ↔ ChatMsg 转换、tool_calls 序列化、标题截断已上移到 `crate::agent::core::shared`(三智能体共用)。
