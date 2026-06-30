//! 🖥️ 进程与系统资源工具(独立工具模块,供任意 Agent 挂载复用)。
//!
//! 用 sysinfo(跨平台纯 Rust)直读系统,**免起 tasklist / wmic 子进程**,高可用高性能。
//! 阻塞采集放 `spawn_blocking`;CPU 占用需两次采样间隔,故取数时 sleep 一个最小间隔再读。

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};
use sysinfo::{Disks, Pid, System};

use crate::agent::core::{Tool, ToolDef, ToolRegistry, ToolResult};

/// list_processes 默认 / 上限返回条数。
const PROC_LIMIT_DEFAULT: usize = 30;
const PROC_LIMIT_MAX: usize = 200;

/// 构造进程 / 系统工具注册表。无外部上下文(直读本机)。
pub fn build_registry() -> ToolRegistry {
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(ListProcessesTool));
    registry.register(Arc::new(FindProcessTool));
    registry.register(Arc::new(KillProcessTool));
    registry.register(Arc::new(SystemInfoTool));
    registry.register(Arc::new(GetEnvTool));
    registry.register(Arc::new(WhichTool));
    registry
}

/// 系统关键进程护栏:结束这些会导致系统崩溃,直接拒绝(防系统级灾难)。命中返回拒绝原因。
fn protected_process_reason(pid: u32, name: &str) -> Option<&'static str> {
    if pid == 0 || pid == 4 {
        return Some("系统核心进程(System / Idle),禁止结束");
    }
    let stem = name.to_lowercase();
    let stem = stem.trim_end_matches(".exe");
    const CRITICAL: &[&str] = &["system", "csrss", "wininit", "winlogon", "services", "lsass", "smss"];
    if CRITICAL.contains(&stem) {
        return Some("系统关键进程,结束会导致系统崩溃,已拒绝");
    }
    None
}

/// 在 PATH 里查找可执行文件的完整路径(Windows 自动尝试 PATHEXT 扩展名)。
fn which_in_path(name: &str) -> Option<String> {
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        let direct = dir.join(name);
        if direct.is_file() {
            return Some(direct.display().to_string());
        }
        // Windows:名字没带扩展名时,挨个试 PATHEXT
        if cfg!(windows) && Path::new(name).extension().is_none() {
            let exts = std::env::var("PATHEXT").unwrap_or_else(|_| ".EXE;.BAT;.CMD".to_string());
            for ext in exts.split(';') {
                let cand = dir.join(format!("{name}{ext}"));
                if cand.is_file() {
                    return Some(cand.display().to_string());
                }
            }
        }
    }
    None
}

/// 字节数转可读大小。
fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = bytes as f64;
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{bytes} B")
    } else {
        format!("{v:.1} {}", UNITS[i])
    }
}

/// 一条进程快照(从 sysinfo 拷出,避免持有 System)。
struct ProcRow {
    pid: u32,
    name: String,
    cpu: f32,
    mem: u64,
}

/// 采集进程列表(两次采样以拿到 CPU 占用)。name_filter 为空=全部。
fn collect_processes(name_filter: &str) -> Vec<ProcRow> {
    let mut sys = System::new_all();
    // CPU 占用是两次采样之差,sleep 一个最小间隔后再刷新才有意义
    std::thread::sleep(sysinfo::MINIMUM_CPU_UPDATE_INTERVAL);
    sys.refresh_all();
    let needle = name_filter.to_lowercase();
    let mut rows: Vec<ProcRow> = Vec::new();
    for (pid, proc_) in sys.processes() {
        let name = proc_.name().to_string_lossy().into_owned();
        if !needle.is_empty() && !name.to_lowercase().contains(&needle) {
            continue;
        }
        rows.push(ProcRow {
            pid: pid.as_u32(),
            name,
            cpu: proc_.cpu_usage(),
            mem: proc_.memory(),
        });
    }
    rows
}

struct ListProcessesTool;
#[async_trait]
impl Tool for ListProcessesTool {
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "list_processes".into(),
            description: "列出正在运行的进程(名 / PID / CPU% / 内存),可按名过滤、按 cpu 或 memory 排序、限条数".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "name_contains": { "type": "string", "description": "可选:进程名包含的子串(忽略大小写)" },
                    "sort_by": { "type": "string", "description": "排序:memory(默认)或 cpu", "enum": ["memory", "cpu"] },
                    "limit": { "type": "integer", "description": "最多返回条数,缺省 30,上限 200" }
                }
            }),
        }
    }
    async fn run(&self, args: Value) -> ToolResult {
        let name = args.get("name_contains").and_then(Value::as_str).unwrap_or("").trim().to_string();
        let by_cpu = args.get("sort_by").and_then(Value::as_str) == Some("cpu");
        let limit = args
            .get("limit")
            .and_then(Value::as_u64)
            .map(|n| n as usize)
            .unwrap_or(PROC_LIMIT_DEFAULT)
            .clamp(1, PROC_LIMIT_MAX);

        let joined = tokio::task::spawn_blocking(move || {
            let mut rows = collect_processes(&name);
            if by_cpu {
                rows.sort_by(|a, b| b.cpu.partial_cmp(&a.cpu).unwrap_or(std::cmp::Ordering::Equal));
            } else {
                rows.sort_by_key(|r| std::cmp::Reverse(r.mem));
            }
            rows.truncate(limit);
            rows
        })
        .await;

        match joined {
            Ok(rows) => {
                if rows.is_empty() {
                    return ToolResult::ok("(无匹配进程)");
                }
                let mut s = format!("共 {} 个进程(按 {} 排序):\n", rows.len(), if by_cpu { "CPU" } else { "内存" });
                for r in &rows {
                    s.push_str(&format!("- {} (PID {}) CPU {:.1}% 内存 {}\n", r.name, r.pid, r.cpu, human_size(r.mem)));
                }
                ToolResult::ok(s)
            }
            Err(e) => ToolResult::err(format!("采集进程任务异常: {e}")),
        }
    }
}

struct FindProcessTool;
#[async_trait]
impl Tool for FindProcessTool {
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "find_process".into(),
            description: "按名称子串查找进程,返回匹配的进程名 / PID / CPU% / 内存".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "name": { "type": "string", "description": "进程名子串(忽略大小写)" }
                },
                "required": ["name"]
            }),
        }
    }
    async fn run(&self, args: Value) -> ToolResult {
        let Some(name) = args.get("name").and_then(Value::as_str) else {
            return ToolResult::err("缺少参数 name");
        };
        let name = name.trim().to_string();
        if name.is_empty() {
            return ToolResult::err("name 不能为空");
        }
        let needle = name.clone();
        let joined = tokio::task::spawn_blocking(move || collect_processes(&needle)).await;
        match joined {
            Ok(rows) => {
                if rows.is_empty() {
                    return ToolResult::ok(format!("(未找到名称含「{name}」的进程)"));
                }
                let mut s = format!("找到 {} 个匹配进程:\n", rows.len());
                for r in rows.iter().take(PROC_LIMIT_MAX) {
                    s.push_str(&format!("- {} (PID {}) CPU {:.1}% 内存 {}\n", r.name, r.pid, r.cpu, human_size(r.mem)));
                }
                ToolResult::ok(s)
            }
            Err(e) => ToolResult::err(format!("查找进程任务异常: {e}")),
        }
    }
}

struct KillProcessTool;
#[async_trait]
impl Tool for KillProcessTool {
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "kill_process".into(),
            description: "按 PID 结束一个进程(请先用 find_process / list_processes 确认 PID,避免误杀)".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "pid": { "type": "integer", "description": "目标进程 PID" }
                },
                "required": ["pid"]
            }),
        }
    }
    async fn run(&self, args: Value) -> ToolResult {
        let Some(pid) = args.get("pid").and_then(Value::as_u64) else {
            return ToolResult::err("缺少参数 pid");
        };
        let pid = pid as u32;
        let joined = tokio::task::spawn_blocking(move || {
            let mut sys = System::new_all();
            sys.refresh_all();
            match sys.process(Pid::from_u32(pid)) {
                Some(p) => {
                    let name = p.name().to_string_lossy().into_owned();
                    if let Some(reason) = protected_process_reason(pid, &name) {
                        return Err(format!("{reason}:{name}(PID {pid})"));
                    }
                    if p.kill() {
                        Ok(format!("已结束进程 {name}(PID {pid})"))
                    } else {
                        Err(format!("结束进程失败(权限不足?):{name}(PID {pid})"))
                    }
                }
                None => Err(format!("未找到 PID 为 {pid} 的进程")),
            }
        })
        .await;
        match joined {
            Ok(Ok(s)) => ToolResult::ok(s),
            Ok(Err(e)) => ToolResult::err(e),
            Err(e) => ToolResult::err(format!("结束进程任务异常: {e}")),
        }
    }
}

struct SystemInfoTool;
#[async_trait]
impl Tool for SystemInfoTool {
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "system_info".into(),
            description: "查看本机系统概况:操作系统 / 主机名 / 内核、运行时长、CPU 核数与总占用、内存、各磁盘容量".into(),
            input_schema: json!({ "type": "object", "properties": {} }),
        }
    }
    async fn run(&self, _args: Value) -> ToolResult {
        let joined = tokio::task::spawn_blocking(|| {
            let mut sys = System::new_all();
            std::thread::sleep(sysinfo::MINIMUM_CPU_UPDATE_INTERVAL);
            sys.refresh_all();

            let os = System::long_os_version().unwrap_or_else(|| "未知".to_string());
            let host = System::host_name().unwrap_or_else(|| "未知".to_string());
            let kernel = System::kernel_version().unwrap_or_else(|| "未知".to_string());
            let uptime_secs = System::uptime();
            let uptime = format!("{}小时{}分", uptime_secs / 3600, (uptime_secs % 3600) / 60);

            let cpu_count = sys.cpus().len();
            let cpu_usage = sys.global_cpu_usage();
            let total_mem = sys.total_memory();
            let used_mem = sys.used_memory();
            let avail_mem = sys.available_memory();

            let mut s = format!(
                "操作系统: {os}\n主机名: {host}\n内核: {kernel}\n已运行: {uptime}\n\
                 CPU: {cpu_count} 核,总占用 {cpu_usage:.1}%\n\
                 内存: 已用 {} / 共 {}(可用 {})\n磁盘:\n",
                human_size(used_mem),
                human_size(total_mem),
                human_size(avail_mem),
            );
            let disks = Disks::new_with_refreshed_list();
            if disks.is_empty() {
                s.push_str("  (无)\n");
            } else {
                for d in &disks {
                    let total = d.total_space();
                    let avail = d.available_space();
                    s.push_str(&format!(
                        "  {} 可用 {} / 共 {}\n",
                        d.mount_point().display(),
                        human_size(avail),
                        human_size(total),
                    ));
                }
            }
            s
        })
        .await;
        match joined {
            Ok(s) => ToolResult::ok(s),
            Err(e) => ToolResult::err(format!("采集系统信息任务异常: {e}")),
        }
    }
}

struct GetEnvTool;
#[async_trait]
impl Tool for GetEnvTool {
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "get_env".into(),
            description: "读取一个环境变量的值(如 PATH、USERPROFILE、TEMP)".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "name": { "type": "string", "description": "环境变量名" }
                },
                "required": ["name"]
            }),
        }
    }
    async fn run(&self, args: Value) -> ToolResult {
        let Some(name) = args.get("name").and_then(Value::as_str) else {
            return ToolResult::err("缺少参数 name");
        };
        let name = name.trim();
        if name.is_empty() {
            return ToolResult::err("name 不能为空");
        }
        match std::env::var(name) {
            Ok(v) => ToolResult::ok(format!("{name}={v}")),
            Err(_) => ToolResult::ok(format!("(环境变量 {name} 未设置)")),
        }
    }
}

struct WhichTool;
#[async_trait]
impl Tool for WhichTool {
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "which".into(),
            description: "在 PATH 里查找可执行文件的完整路径(Windows 自动尝试 .exe/.bat/.cmd 等 PATHEXT 扩展名)".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "name": { "type": "string", "description": "可执行文件名,如 git、python、node" }
                },
                "required": ["name"]
            }),
        }
    }
    async fn run(&self, args: Value) -> ToolResult {
        let Some(name) = args.get("name").and_then(Value::as_str) else {
            return ToolResult::err("缺少参数 name");
        };
        let name = name.trim().to_string();
        if name.is_empty() {
            return ToolResult::err("name 不能为空");
        }
        let joined = tokio::task::spawn_blocking(move || which_in_path(&name).ok_or(name)).await;
        match joined {
            Ok(Ok(path)) => ToolResult::ok(path),
            Ok(Err(name)) => ToolResult::ok(format!("(PATH 中未找到可执行文件 {name})")),
            Err(e) => ToolResult::err(format!("查找任务异常: {e}")),
        }
    }
}
