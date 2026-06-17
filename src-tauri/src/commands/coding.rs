//! 编程 Agent 命令:send_coding_message(ReAct 循环 + 工具往返落库 + 进度事件)、工作区读写。
//!
//! 复用 `llm::agent`(LlmProvider + ToolRegistry)与 `agent::coding`(工具集 + 提示词)。
//! 工具与 run_command 限定在「编程工作区」目录内(沙箱)。

use crate::agent::coding;
use crate::commands::chat::MessageView;
use crate::commands::conversation_summary as conv_summary;
use crate::commands::{current_user, AppState};
use crate::llm::agent::{
    provider_for, ChatMsg, LlmOptions, LlmRequest, ProviderKind, ProviderRef, ToolCall,
};
use chrono::Utc;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, EntityTrait, IntoActiveModel, QueryFilter, QueryOrder,
    QuerySelect, Set,
};
use serde::Serialize;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Emitter, State};
use tokio::io::AsyncBufReadExt;
use veltrix_core::db::entity::{
    chat_conversation as conv, chat_message as msg, provider as provider_entity,
};
use veltrix_core::error::{CrawlerError, Result};

/// ReAct 最大步数(防失控循环)。浏览器 Agent 命令复用,统一一处定义。
pub(crate) const MAX_ITERS: usize = 25;
/// run_command 失败后,模型若想直接收尾,自动注入引导逼它修复重试的最大次数(防卡死)。
const AUTO_FIX_MAX: usize = 2;
/// 工作区根目录在 app_secrets 的 key;空 = 默认 `<app_data>/coding-workspaces`(每会话一个子目录)。
const CODING_WORKSPACE_KEY: &str = "coding_workspace_path";
/// 沙盒配置:镜像、共享容器名。(默认即用 Docker 沙盒,Docker 不可用时自动回退本机,无手动模式开关)
const SANDBOX_IMAGE_KEY: &str = "coding_sandbox_image";
const SANDBOX_CONTAINER_KEY: &str = "coding_sandbox_container";
const DEFAULT_SANDBOX_IMAGE: &str = "node:20-bookworm";
const DEFAULT_SANDBOX_CONTAINER: &str = "veltrix-sandbox";
/// 沙盒发布到宿主的常用 dev 端口:容器内服务绑 0.0.0.0 后,宿主经 127.0.0.1:<port> 即可预览。
/// `create_container` 据此发布端口;`get_dev_server_status` 据此主动探测端口(日志解析失败时兜底)。
const DEV_PREVIEW_PORTS: [u16; 5] = [5173, 5174, 4173, 3000, 8080];

/// 工作区根目录(自定义优先,否则默认数据目录下 coding-workspaces)。
fn workspace_base(state: &AppState, custom: &str) -> PathBuf {
    if custom.trim().is_empty() {
        state.config_dir.join("coding-workspaces")
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

/// 读 secret,空则取默认(已 trim)。
async fn secret_or(db: &sea_orm::DatabaseConnection, key: &str, default: &str) -> String {
    let v = super::get_secret(db, key).await;
    if v.trim().is_empty() {
        default.to_string()
    } else {
        v.trim().to_string()
    }
}

/// 读取编程工作区路径(给前端展示);传 conversationId 则返回该会话目录,否则返回根目录。
#[tauri::command]
pub async fn get_coding_workspace(
    state: State<'_, AppState>,
    conversation_id: Option<String>,
) -> Result<String> {
    let base = workspace_base(&state, &super::get_secret(&state.db, CODING_WORKSPACE_KEY).await);
    let p = match conversation_id {
        Some(id) if !id.trim().is_empty() => conv_workspace(&base, &id),
        _ => base,
    };
    Ok(p.display().to_string())
}

/// 设置工作区根目录(空串=恢复默认)。
#[tauri::command]
pub async fn set_coding_workspace(state: State<'_, AppState>, path: String) -> Result<()> {
    super::set_secret(&state.db, CODING_WORKSPACE_KEY, path.trim()).await
}

/// 解析某会话的执行环境:返回(宿主工作区目录, ExecConfig)。
/// docker 模式确保共享容器就绪(容器内 /workspace/<会话id>);容器不可用则回退本机执行(同一目录,只是不隔离)。
async fn resolve_exec(state: &AppState, conv_id: &str) -> Result<(PathBuf, coding::ExecConfig)> {
    let base = workspace_base(state, &super::get_secret(&state.db, CODING_WORKSPACE_KEY).await);
    let ws = conv_workspace(&base, conv_id);
    tokio::fs::create_dir_all(&ws)
        .await
        .map_err(|e| CrawlerError::Config(format!("创建工作区失败: {e}")))?;
    // 默认就用 Docker 沙盒;Docker 不可用 / 创建失败时自动回退本机执行(见下 ensure_container 的 Err 分支)。
    let image = secret_or(&state.db, SANDBOX_IMAGE_KEY, DEFAULT_SANDBOX_IMAGE).await;
    let container = secret_or(&state.db, SANDBOX_CONTAINER_KEY, DEFAULT_SANDBOX_CONTAINER).await;
    match ensure_container(&base, &image, &container).await {
        Ok(()) => Ok((
            ws,
            coding::ExecConfig::Docker {
                container,
                workdir: format!("/workspace/{}", safe_id(conv_id)),
            },
        )),
        Err(e) => {
            tracing::warn!("Docker 沙盒不可用,回退本机执行: {e}");
            Ok((ws, coding::ExecConfig::Host))
        }
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
    let base = workspace_base(&state, &super::get_secret(&state.db, CODING_WORKSPACE_KEY).await);
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
    let base = workspace_base(&state, &super::get_secret(&state.db, CODING_WORKSPACE_KEY).await);
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

/// 文件面板编辑后写回工作区内某文件(自动建父目录)。写入即落到挂载目录,
/// 容器内 dev server(若在跑)会监听到变化并热更新(配合 CHOKIDAR_USEPOLLING)。
#[tauri::command]
pub async fn write_workspace_file(
    state: State<'_, AppState>,
    conversation_id: String,
    path: String,
    content: String,
) -> Result<()> {
    let base = workspace_base(&state, &super::get_secret(&state.db, CODING_WORKSPACE_KEY).await);
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

/// 跑一条 docker 子命令。
async fn docker(args: &[&str]) -> std::io::Result<std::process::Output> {
    tokio::process::Command::new("docker").args(args).output().await
}

/// 确保共享沙盒容器存在并运行,且把工作区根目录正确挂到 /workspace。
/// 已存在但没有 /workspace 挂载(旧版残留)→ 删掉重建,保证宿主写的文件在容器里可见。
async fn ensure_container(base: &Path, image: &str, container: &str) -> Result<()> {
    tokio::fs::create_dir_all(base).await.ok();
    let exists = docker(&["container", "inspect", container])
        .await
        .map(|o| o.status.success())
        .unwrap_or(false);
    if exists {
        if container_has_workspace_mount(container).await {
            let _ = docker(&["start", container]).await;
            return Ok(());
        }
        // 旧容器没挂载工作区 → 删掉重建(否则宿主文件在容器里看不到)
        tracing::warn!("沙盒容器缺少 /workspace 挂载,删除并重建");
        let _ = docker(&["rm", "-f", container]).await;
    }
    create_container(base, image, container).await
}

/// 容器是否有 Destination=/workspace 的挂载。
async fn container_has_workspace_mount(container: &str) -> bool {
    docker(&[
        "container",
        "inspect",
        "-f",
        "{{range .Mounts}}{{.Destination}};{{end}}",
        container,
    ])
    .await
    .ok()
    .filter(|o| o.status.success())
    .map(|o| String::from_utf8_lossy(&o.stdout).contains("/workspace"))
    .unwrap_or(false)
}

/// 创建共享沙盒容器:`--mount`(避开 Windows 盘符冒号与 -v 分隔冲突)挂载工作区根目录 →
/// /workspace,常驻 sleep infinity,并发布常用 dev 端口。端口被占用导致失败时回退不发布端口。
async fn create_container(base: &Path, image: &str, container: &str) -> Result<()> {
    // source 用正斜杠(Docker Desktop 接受;避免反斜杠歧义)
    let source = base.display().to_string().replace('\\', "/");
    let bind = format!("type=bind,source={source},target=/workspace");

    let mut args: Vec<&str> =
        vec!["run", "-d", "--name", container, "--mount", bind.as_str(), "-w", "/workspace"];
    // 发布常用 dev 端口(与 DEV_PREVIEW_PORTS 探测集一致);publish 须比 args 活得久。
    let publish: Vec<String> = DEV_PREVIEW_PORTS.iter().map(|p| format!("{p}:{p}")).collect();
    for p in &publish {
        args.push("-p");
        args.push(p.as_str());
    }
    args.push(image);
    args.push("sleep");
    args.push("infinity");
    let out = docker(&args)
        .await
        .map_err(|e| CrawlerError::Config(format!("docker run 失败(Docker 是否已安装并运行?): {e}")))?;
    if out.status.success() {
        return Ok(());
    }
    // 回退:清掉残留同名容器,不发布端口重建(命令执行仍可用,仅 dev 预览不可达)
    let _ = docker(&["rm", "-f", container]).await;
    let out2 = docker(&[
        "run", "-d", "--name", container, "--mount", bind.as_str(), "-w", "/workspace", image,
        "sleep", "infinity",
    ])
    .await
    .map_err(|e| CrawlerError::Config(format!("docker run 失败: {e}")))?;
    if out2.status.success() {
        tracing::warn!("沙盒容器未发布端口(端口可能被占用),dev server 预览将不可达;命令执行正常");
        Ok(())
    } else {
        Err(CrawlerError::Config(format!(
            "创建沙盒容器失败: {}",
            String::from_utf8_lossy(&out2.stderr).chars().take(300).collect::<String>()
        )))
    }
}

/// 强制重建沙盒容器(删旧 + 用正确挂载新建)。用于旧容器挂载错误 / 想换镜像时一键修复。
#[tauri::command]
pub async fn sandbox_recreate(state: State<'_, AppState>) -> Result<String> {
    let base = workspace_base(&state, &super::get_secret(&state.db, CODING_WORKSPACE_KEY).await);
    let image = secret_or(&state.db, SANDBOX_IMAGE_KEY, DEFAULT_SANDBOX_IMAGE).await;
    let container = secret_or(&state.db, SANDBOX_CONTAINER_KEY, DEFAULT_SANDBOX_CONTAINER).await;
    let _ = docker(&["rm", "-f", &container]).await;
    create_container(&base, &image, &container).await?;
    Ok(format!("沙盒容器 {container} 已重建并正确挂载工作区"))
}

/// 沙盒配置视图。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SandboxConfigView {
    pub image: String,
    pub container: String,
    /// Docker 是否可用;false 时命令会自动回退本机执行(未隔离)
    pub docker_available: bool,
    pub container_running: bool,
}

/// 读取沙盒配置 + 探测 Docker 可用性 / 容器运行状态。
#[tauri::command]
pub async fn get_sandbox_config(state: State<'_, AppState>) -> Result<SandboxConfigView> {
    let image = secret_or(&state.db, SANDBOX_IMAGE_KEY, DEFAULT_SANDBOX_IMAGE).await;
    let container = secret_or(&state.db, SANDBOX_CONTAINER_KEY, DEFAULT_SANDBOX_CONTAINER).await;
    let docker_available = docker(&["version"]).await.map(|o| o.status.success()).unwrap_or(false);
    let container_running = docker_available
        && docker(&["container", "inspect", "-f", "{{.State.Running}}", &container])
            .await
            .ok()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim() == "true")
            .unwrap_or(false);
    Ok(SandboxConfigView { image, container, docker_available, container_running })
}

/// 写入沙盒配置(image / container 空则用默认)。
#[tauri::command]
pub async fn set_sandbox_config(
    state: State<'_, AppState>,
    image: String,
    container: String,
) -> Result<()> {
    let img = image.trim();
    super::set_secret(&state.db, SANDBOX_IMAGE_KEY, if img.is_empty() { DEFAULT_SANDBOX_IMAGE } else { img })
        .await?;
    let c = container.trim();
    super::set_secret(
        &state.db,
        SANDBOX_CONTAINER_KEY,
        if c.is_empty() { DEFAULT_SANDBOX_CONTAINER } else { c },
    )
    .await?;
    Ok(())
}

/// 手动拉起沙盒容器(按需 pull 镜像)。返回状态文本。
#[tauri::command]
pub async fn sandbox_start(state: State<'_, AppState>) -> Result<String> {
    let base = workspace_base(&state, &super::get_secret(&state.db, CODING_WORKSPACE_KEY).await);
    let image = secret_or(&state.db, SANDBOX_IMAGE_KEY, DEFAULT_SANDBOX_IMAGE).await;
    let container = secret_or(&state.db, SANDBOX_CONTAINER_KEY, DEFAULT_SANDBOX_CONTAINER).await;
    ensure_container(&base, &image, &container).await?;
    Ok(format!("沙盒容器 {container} 已就绪(镜像 {image})"))
}

/// 停止沙盒容器(释放资源;工作区是宿主挂载卷,文件保留)。
#[tauri::command]
pub async fn sandbox_stop(state: State<'_, AppState>) -> Result<()> {
    let container = secret_or(&state.db, SANDBOX_CONTAINER_KEY, DEFAULT_SANDBOX_CONTAINER).await;
    let _ = docker(&["stop", "-t", "2", &container]).await;
    Ok(())
}

/// 启动时:后台拉起沙盒容器(供 lib.rs setup 调用;Docker 不可用则忽略,运行时自动回退本机)。
pub async fn ensure_sandbox_on_start(db: &sea_orm::DatabaseConnection, config_dir: &Path) {
    let base = {
        let c = super::get_secret(db, CODING_WORKSPACE_KEY).await;
        if c.trim().is_empty() {
            config_dir.join("coding-workspaces")
        } else {
            PathBuf::from(c.trim())
        }
    };
    let image = secret_or(db, SANDBOX_IMAGE_KEY, DEFAULT_SANDBOX_IMAGE).await;
    let container = secret_or(db, SANDBOX_CONTAINER_KEY, DEFAULT_SANDBOX_CONTAINER).await;
    if let Err(e) = ensure_container(&base, &image, &container).await {
        tracing::warn!("启动时拉起沙盒容器失败(忽略): {e}");
    }
}

/// 退出时:停止沙盒容器(释放资源,文件保留)。
pub async fn stop_sandbox_on_exit(db: &sea_orm::DatabaseConnection) {
    let container = secret_or(db, SANDBOX_CONTAINER_KEY, DEFAULT_SANDBOX_CONTAINER).await;
    let _ = docker(&["stop", "-t", "2", &container]).await;
}

// ===================== 开发服务器预览(常驻进程) =====================

/// dev server 日志保留上限(行)。
const DEV_LOG_CAP: usize = 300;

/// 常驻开发服务器状态(如 `npm run dev`)。child 存句柄供停止;port 由输出解析。
#[derive(Default)]
pub struct DevServer {
    child: Option<tokio::process::Child>,
    port: Option<u16>,
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
                while let Some(c2) = chars.next() {
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

/// 停止当前 dev server(杀进程 + 复位状态)。同步操作,不跨 await 持锁。
fn stop_dev_inner(dev: &Arc<Mutex<DevServer>>) {
    let mut g = dev.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(mut child) = g.child.take() {
        let _ = child.start_kill();
    }
    g.running = false;
    g.port = None;
    g.conversation_id.clear();
}

/// 启动 / 重启开发服务器:在编程工作区内跑给定命令(如 `npm run dev`),常驻。
#[tauri::command]
pub async fn start_dev_server(
    state: State<'_, AppState>,
    conversation_id: String,
    command: String,
) -> Result<()> {
    let cmd = command.trim().to_string();
    if cmd.is_empty() {
        return Err(CrawlerError::Config("命令为空".into()));
    }
    // 在该会话的工作区 / 沙盒内常驻运行(docker 模式经 docker exec)。dev server 需绑 0.0.0.0,
    // 容器已发布常用端口,故可经 <名>.localhost:<port> 预览。
    let (ws, exec) = resolve_exec(&state, &conversation_id).await?;

    // 先停掉已有的(避免端口冲突 / 进程泄漏)
    stop_dev_inner(&state.dev_server);

    let mut launcher = coding::build_exec_command(&exec, &ws, &cmd);
    launcher
        .kill_on_drop(false)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let mut child = launcher
        .spawn()
        .map_err(|e| CrawlerError::Config(format!("启动开发服务器失败: {e}")))?;
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let generation;
    {
        let mut g = state.dev_server.lock().unwrap_or_else(|e| e.into_inner());
        g.generation = g.generation.wrapping_add(1); // 新代次,旧流 EOF 不再影响本 server
        generation = g.generation;
        g.child = Some(child);
        g.port = None;
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

/// 停止开发服务器。
#[tauri::command]
pub async fn stop_dev_server(state: State<'_, AppState>) -> Result<()> {
    stop_dev_inner(&state.dev_server);
    Ok(())
}

/// 主动探测已发布端口:返回第一个能建立 TCP 连接的端口(按 Vite 优先序)。
/// 兜底用——docker exec 非 TTY 流里 Vite 的就绪 banner 常被缓冲 / 着色吞掉,日志解析不到端口,
/// 但服务确在 0.0.0.0:<port> 监听,直接连宿主回环即可定位,避免预览永远卡在「正在探测端口」。
async fn probe_dev_port() -> Option<u16> {
    for p in DEV_PREVIEW_PORTS {
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
    let (running, port, command, logs, conversation_id) = {
        let g = state.dev_server.lock().unwrap_or_else(|e| e.into_inner());
        (g.running, g.port, g.command.clone(), g.logs.clone(), g.conversation_id.clone())
    };
    // 日志没解析到端口(docker exec 非 TTY 常吞 banner)→ 主动探测兜底,并回填(仅当仍是同一运行实例)
    if running && port.is_none() {
        if let Some(p) = probe_dev_port().await {
            let mut g = state.dev_server.lock().unwrap_or_else(|e| e.into_inner());
            if g.running && g.port.is_none() {
                g.port = Some(p);
            }
            return Ok(DevServerStatus { running, port: Some(p), command, logs, conversation_id });
        }
    }
    Ok(DevServerStatus { running, port, command, logs, conversation_id })
}

/// 意图分类的 LLM 调用超时(秒):只回一个词,取较短值;超时即回退关键词结果,避免拖慢首条消息。
const CLASSIFY_TIMEOUT_SECS: u64 = 8;

/// 关键词启发式分类:明显编程的快路径 + LLM 不可用时的兜底。
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

/// 意图分类:判断首条消息走哪个 Agent,返回 "coding" / "rpa" / "chat"。
/// 关键词明显命中 coding / rpa → 直接返回(省一次调用);否则用一次轻量 LLM 兜底三类;
/// LLM 不可用 / 失败 / 乱答 → 回退 chat。provider_id/model 由前端传入(用当前选中的模型)。
#[tauri::command]
pub async fn classify_agent_type(
    state: State<'_, AppState>,
    text: String,
    provider_id: Option<String>,
    model: Option<String>,
) -> Result<String> {
    // 关键词明确命中 coding / rpa → 直接返回(省一次 LLM);否则交给 LLM 兜底
    let kw = classify_by_keywords(&text);
    if kw != "chat" {
        return Ok(kw.to_string());
    }
    if let (Some(pid), Some(m)) = (provider_id.as_deref(), model.as_deref()) {
        if let Some(r) = classify_via_llm(&state.db, pid, m, &text).await {
            return Ok(r);
        }
    }
    Ok("chat".to_string())
}

/// 一次 LLM 分类:只让模型回答 coding / chat。失败或乱答返回 None(由上层回退关键词结果)。
/// 分类属杂活,优先走 Classify 角色单独配置的便宜模型;未配置则回退前端传入的会话模型。
async fn classify_via_llm(
    db: &sea_orm::DatabaseConnection,
    provider_id: &str,
    model: &str,
    text: &str,
) -> Option<String> {
    let provider = provider_entity::Entity::find_by_id(provider_id.to_string())
        .one(db)
        .await
        .ok()
        .flatten()?;
    if provider.api_key.trim().is_empty() {
        return None;
    }
    // 前端传入的 provider/model 退化为回退档;命中 Classify 角色配置则改走便宜模型
    let fallback = ProviderRef {
        kind: ProviderKind::from_code(&provider.code),
        api_url: provider.api_url.clone(),
        api_key: provider.api_key.clone(),
        model: model.to_string(),
    };
    let chosen =
        crate::commands::resolve_role_provider(db, crate::llm::AgentRole::Classify, fallback).await;
    let snippet: String = text.chars().take(1000).collect();
    let prompt = format!(
        "判断下面这条用户消息属于哪类任务,只回答一个词:\
coding(编程 / 写代码 / 改代码 / 跑命令 / 做软件工具)、\
rpa(操作浏览器或网页:打开网页、点击、填表单、网页自动化 / 抓取)、\
chat(其它:闲聊 / 问答 / 写作等)。只回答 coding、rpa 或 chat,不要解释。\n\n消息:{snippet}"
    );
    let reply = crate::llm::chat::chat_completion(crate::llm::chat::ChatRequest {
        api_url: &chosen.api_url,
        api_key: &chosen.api_key,
        model: &chosen.model,
        messages: json!([{ "role": "user", "content": prompt }]),
        extra_body: None,
        timeout_secs: CLASSIFY_TIMEOUT_SECS,
        retry_server_errors: false,
    })
    .await
    .ok()?;
    let lower = reply.to_lowercase();
    if lower.contains("coding") {
        Some("coding".to_string())
    } else if lower.contains("rpa") {
        Some("rpa".to_string())
    } else if lower.contains("chat") {
        Some("chat".to_string())
    } else {
        None
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

    let conversation = conv::Entity::find_by_id(conversation_id.clone())
        .one(&state.db)
        .await
        .map_err(|e| CrawlerError::Config(format!("查询会话失败: {e}")))?
        .ok_or_else(|| CrawlerError::Config("会话不存在".into()))?;
    if conversation.owner != me.name {
        return Err(CrawlerError::Config("无权操作该会话".into()));
    }
    let provider = provider_entity::Entity::find_by_id(conversation.provider_id.clone())
        .one(&state.db)
        .await
        .map_err(|e| CrawlerError::Config(format!("查询厂商失败: {e}")))?
        .ok_or_else(|| CrawlerError::Config("会话绑定的模型厂商不存在,请新建会话".into()))?;
    if provider.api_key.trim().is_empty() {
        return Err(CrawlerError::Config(
            "该模型厂商未配置 API Key,请到系统配置补全".into(),
        ));
    }

    // 是否首轮(决定是否用首句起标题)
    let had_messages = msg::Entity::find()
        .filter(msg::Column::ConversationId.eq(&conversation_id))
        .one(&state.db)
        .await
        .ok()
        .flatten()
        .is_some();

    let now = Utc::now().timestamp();
    // 落库 user 消息
    msg::ActiveModel {
        conversation_id: Set(conversation_id.clone()),
        role: Set("user".to_string()),
        content: Set(text.clone()),
        created_at: Set(now),
        ..Default::default()
    }
    .insert(&state.db)
    .await
    .map_err(|e| CrawlerError::Config(format!("保存消息失败: {e}")))?;

    // 准备本会话工作区 + 执行环境(host 或 Docker 沙盒)+ 工具注册表
    let (workspace, exec) = resolve_exec(&state, &conversation_id).await?;
    // 本轮开始前打检查点(git commit),Agent 改崩可经 checkpoint_rollback 回到发送前
    coding::checkpoint(&workspace, &exec).await;
    let registry = coding::build_registry(workspace, exec, agent_mode);
    let tool_defs = registry.defs();

    // live 原文:id 大于已折叠进摘要的边界(含刚落库的 user 消息);更早的由会话滚动摘要承载,
    // 长会话不再硬截断丢早期上下文(与 chat 一致的「live 窗口 + 滚动摘要」策略)。
    let mut rows = msg::Entity::find()
        .filter(msg::Column::ConversationId.eq(&conversation_id))
        .filter(msg::Column::Id.gt(conversation.summarized_upto_id))
        .order_by_desc(msg::Column::Id)
        .limit(conv_summary::LIVE_HARD_CAP)
        .all(&state.db)
        .await
        .map_err(|e| CrawlerError::Config(format!("读取历史失败: {e}")))?;
    // 取「最新」LIVE_HARD_CAP 条后翻回升序(与 chat 一致):desc+limit 保证未折叠消息超额时
    // 尾部仍是刚落库的本轮 user,不会被挤出窗口(此前 asc+limit 取最旧会丢本轮提问)。
    rows.reverse();
    // 兜底:窗口须从第一条 user 开始——否则可能以 tool / assistant(tool_calls)开头
    //(其配对消息已折进摘要),OpenAI 会报 400。找不到 user 则整窗为空(全靠摘要 + 本轮提问)。
    let windowed: &[msg::Model] = match rows.iter().position(|m| m.role == "user") {
        Some(start) => &rows[start..],
        None => &[],
    };
    // 按模式选系统提示词:Plan 只引导出方案(配合 build_registry 只挂只读工具),Act 亲自动手
    let system_prompt = match agent_mode {
        coding::AgentMode::Plan => coding::PLAN_SYSTEM_PROMPT,
        coding::AgentMode::Act => coding::SYSTEM_PROMPT,
    };
    let mut messages: Vec<ChatMsg> = vec![ChatMsg::System(system_prompt.to_string())];
    // 会话滚动摘要:早期消息压缩后的前情提要,注入在原文之前(为空则不注入)
    if let Some(sys) = conv_summary::summary_system_message(&conversation.summary) {
        if let Some(text) = sys.get("content").and_then(|v| v.as_str()) {
            messages.push(ChatMsg::System(text.to_string()));
        }
    }
    // Act 模式且已有计划:注入当前 todo 清单,引导按序执行并用 update_plan 勾选进度
    if matches!(agent_mode, coding::AgentMode::Act) {
        if let Some(plan_sys) = coding::plan_system_message(&conversation.plan_todos) {
            messages.push(ChatMsg::System(plan_sys));
        }
    }
    messages.extend(windowed.iter().filter_map(row_to_chat_msg));

    let provider_ref = ProviderRef {
        kind: ProviderKind::from_code(&provider.code),
        api_url: provider.api_url.clone(),
        api_key: provider.api_key.clone(),
        model: conversation.model.clone(),
    };
    let llm = provider_for(provider_ref.kind);
    let options = LlmOptions::default();

    let emit = |label: String| {
        let _ = app.emit(
            "agent-step",
            json!({ "conversationId": &conversation_id, "label": label }),
        );
    };

    // ReAct 循环。auto_fix:run_command 失败后模型若想直接收尾,自动注入引导再修(有配额防卡死)
    let mut final_text = String::new();
    let mut auto_fix_used = 0usize;
    let mut last_run_failed = false;
    for iter in 0..MAX_ITERS {
        emit(format!("思考中…(第 {} 步)", iter + 1));
        let resp = llm
            .chat(LlmRequest {
                provider: &provider_ref,
                messages: &messages,
                tools: &tool_defs,
                options: &options,
            })
            .await?;

        // 无工具调用 → 模型想收尾
        if resp.tool_calls.is_empty() {
            // 但若刚才有 run_command 失败且仍有配额 → 不收尾,注入引导逼它修复后重试
            if last_run_failed && auto_fix_used < AUTO_FIX_MAX {
                auto_fix_used += 1;
                last_run_failed = false;
                emit(format!("命令未通过,自动尝试修复…(第 {auto_fix_used} 次)"));
                // 把模型这轮的话纳入上下文(让它记得刚说过什么),再追加修复引导
                if resp.content.as_deref().map(|t| !t.trim().is_empty()).unwrap_or(false) {
                    messages.push(ChatMsg::Assistant {
                        text: resp.content.clone(),
                        tool_calls: vec![],
                    });
                }
                messages.push(ChatMsg::User(coding::auto_fix_prompt(auto_fix_used)));
                continue;
            }
            final_text = resp.content.unwrap_or_default();
            break;
        }

        // 落库 assistant(带 tool_calls)
        let assistant_text = resp.content.clone().unwrap_or_default();
        let tc_json = tool_calls_to_json(&resp.tool_calls);
        msg::ActiveModel {
            conversation_id: Set(conversation_id.clone()),
            role: Set("assistant".to_string()),
            content: Set(assistant_text.clone()),
            tool_calls: Set(Some(tc_json)),
            created_at: Set(Utc::now().timestamp()),
            ..Default::default()
        }
        .insert(&state.db)
        .await
        .map_err(|e| CrawlerError::Config(format!("保存回复失败: {e}")))?;
        messages.push(ChatMsg::Assistant {
            text: resp.content.clone(),
            tool_calls: resp.tool_calls.clone(),
        });

        // 逐个执行工具,结果落库 + 回灌;记录本轮是否有 run_command 失败(供自动续修判定)
        let mut run_failed = false;
        for call in &resp.tool_calls {
            emit(format!("🔧 {}", call.name));
            let result = registry.run(&call.name, call.arguments.clone()).await;
            let flag = if result.is_error { "✗" } else { "✓" };
            emit(format!("{flag} {}", call.name));
            if call.name == "run_command" && result.is_error {
                run_failed = true;
            }
            // 拦截 update_plan:把模型给的完整 todo 清单落库到会话(工具层不碰 DB,持久化在此保持分层)
            if call.name == "update_plan" && !result.is_error {
                if let Some(todos) = call.arguments.get("todos") {
                    let _ = conv::Entity::update_many()
                        .col_expr(
                            conv::Column::PlanTodos,
                            sea_orm::sea_query::Expr::value(todos.to_string()),
                        )
                        .filter(conv::Column::Id.eq(&conversation_id))
                        .exec(&state.db)
                        .await;
                }
            }
            msg::ActiveModel {
                conversation_id: Set(conversation_id.clone()),
                role: Set("tool".to_string()),
                content: Set(result.content.clone()),
                tool_call_id: Set(Some(call.id.clone())),
                tool_name: Set(Some(call.name.clone())),
                created_at: Set(Utc::now().timestamp()),
                ..Default::default()
            }
            .insert(&state.db)
            .await
            .map_err(|e| CrawlerError::Config(format!("保存工具结果失败: {e}")))?;
            messages.push(ChatMsg::Tool {
                tool_call_id: call.id.clone(),
                content: result.content,
            });
        }
        last_run_failed = run_failed;

        // 达上限:强制收尾
        if iter == MAX_ITERS - 1 {
            final_text = format!("(已达最大步数 {MAX_ITERS},已停止。可继续追问以推进。)");
        }
    }

    // 落库最终 assistant 消息
    let final_msg = msg::ActiveModel {
        conversation_id: Set(conversation_id.clone()),
        role: Set("assistant".to_string()),
        content: Set(final_text),
        created_at: Set(Utc::now().timestamp()),
        ..Default::default()
    }
    .insert(&state.db)
    .await
    .map_err(|e| CrawlerError::Config(format!("保存回复失败: {e}")))?;
    emit("完成".to_string());

    // 更新会话时间;首轮用用户首句起标题(截断)
    let mut am = conversation.into_active_model();
    am.updated_at = Set(Utc::now().timestamp());
    if !had_messages {
        am.title = Set(truncate_title(&text));
    }
    let _ = am.update(&state.db).await;

    // 滚动摘要维护:本轮工具往返可能产生很多消息,live 过长时把较旧的折叠进会话摘要,
    // 异步进行不阻塞返回。会话模型作 fallback,命中 Summary 角色配置则改走便宜模型。
    spawn_coding_summary_maintenance(&state.db, &conversation_id, provider_ref);

    Ok(final_msg.into())
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

/// DB 消息行 → 统一 ChatMsg;无法识别的角色跳过。
pub(crate) fn row_to_chat_msg(m: &msg::Model) -> Option<ChatMsg> {
    match m.role.as_str() {
        "user" => Some(ChatMsg::User(m.content.clone())),
        "assistant" => {
            let tool_calls = m
                .tool_calls
                .as_deref()
                .map(parse_tool_calls)
                .unwrap_or_default();
            let text = if m.content.is_empty() {
                None
            } else {
                Some(m.content.clone())
            };
            Some(ChatMsg::Assistant { text, tool_calls })
        }
        "tool" => Some(ChatMsg::Tool {
            tool_call_id: m.tool_call_id.clone().unwrap_or_default(),
            content: m.content.clone(),
        }),
        _ => None,
    }
}

/// 解析 DB 里存的 tool_calls JSON([{id,name,arguments}])为 Vec<ToolCall>。
pub(crate) fn parse_tool_calls(json_str: &str) -> Vec<ToolCall> {
    serde_json::from_str::<Value>(json_str)
        .ok()
        .and_then(|v| v.as_array().cloned())
        .map(|arr| {
            arr.iter()
                .filter_map(|tc| {
                    Some(ToolCall {
                        id: tc.get("id")?.as_str()?.to_string(),
                        name: tc.get("name")?.as_str()?.to_string(),
                        arguments: tc.get("arguments").cloned().unwrap_or(Value::Null),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Vec<ToolCall> → 落库用 JSON 字符串。
pub(crate) fn tool_calls_to_json(calls: &[ToolCall]) -> String {
    let arr: Vec<Value> = calls
        .iter()
        .map(|tc| json!({ "id": tc.id, "name": tc.name, "arguments": tc.arguments }))
        .collect();
    Value::Array(arr).to_string()
}

/// 用首条用户消息生成标题:取前 24 个字符,去换行。
pub(crate) fn truncate_title(text: &str) -> String {
    let one_line = text.replace(['\n', '\r'], " ");
    let trimmed = one_line.trim();
    let chars: Vec<char> = trimmed.chars().collect();
    if chars.len() <= 24 {
        trimmed.to_string()
    } else {
        let mut s: String = chars[..24].iter().collect();
        s.push('…');
        s
    }
}
