//! 📁 文件系统工具(独立工具模块,供任意 Agent 挂载复用)。
//!
//! 区别于编程 Agent 的 `coding` 文件工具(绑定工作区沙箱):本模块是**通用全机路径**操作,
//! 给 Agent 在本机任意位置查 / 读 / 写文件。高可用 / 高性能要点:
//! 纯 Rust 遍历(免起 shell)、遍历与结果数双上限(防遍历整盘卡死)、读文件大小上限、UTF-8 lossy 兜底。

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};
use walkdir::WalkDir;

use crate::agent::core::{Tool, ToolDef, ToolRegistry, ToolResult};

/// find_files 默认 / 上限:结果条数与遍历文件总数(后者防止误传 C:\ 把整盘走穿)。
const FIND_RESULTS_DEFAULT: usize = 200;
const FIND_RESULTS_MAX: usize = 2000;
const FIND_WALK_CAP: usize = 100_000;
/// read_file 默认 / 上限读取字节数。
const READ_BYTES_DEFAULT: u64 = 256 * 1024;
const READ_BYTES_MAX: u64 = 4 * 1024 * 1024;
/// list_dir 最多返回条目数。
const LIST_DIR_CAP: usize = 1000;

/// 构造文件系统工具注册表。无外部上下文(直接操作本机路径)。
pub fn build_registry() -> ToolRegistry {
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(FindFilesTool));
    registry.register(Arc::new(ReadFileTool));
    registry.register(Arc::new(WriteFileTool));
    registry.register(Arc::new(ListDirTool));
    registry.register(Arc::new(FileInfoTool));
    registry.register(Arc::new(CopyFileTool));
    registry.register(Arc::new(MovePathTool));
    registry.register(Arc::new(MakeDirTool));
    registry.register(Arc::new(DeletePathTool));
    registry
}

/// 系统关键区护栏:对这些位置做写 / 移 / 删极易搞坏系统,工具层直接拒绝(防系统级灾难)。
/// 注:这是最低限度护栏,用户级重要文件的防护靠后续 Agent 层的「危险操作确认」链路。命中返回拒绝原因。
fn protected_path_reason(path: &str) -> Option<&'static str> {
    let raw = path.trim();
    // Windows:盘符根(c:\ / c:)或系统目录,统一小写 + 反斜杠后比较
    let win = raw.replace('/', "\\").to_lowercase();
    let win = win.trim_end_matches('\\');
    if win.len() == 2 && win.as_bytes().get(1) == Some(&b':') {
        return Some("不能直接操作盘符根目录");
    }
    const WIN_PROTECTED: &[&str] = &[
        "c:\\windows",
        "c:\\program files",
        "c:\\program files (x86)",
        "c:\\programdata",
    ];
    for pre in WIN_PROTECTED {
        if win == *pre || win.starts_with(&format!("{pre}\\")) {
            return Some("目标位于 Windows 系统关键目录,已拒绝以防搞坏系统");
        }
    }
    // 跨平台兜底:Unix 根与系统目录
    if raw == "/" {
        return Some("不能操作根目录");
    }
    let unix = raw.trim_end_matches('/');
    const UNIX_PROTECTED: &[&str] =
        &["/bin", "/sbin", "/etc", "/usr", "/boot", "/sys", "/dev", "/lib", "/proc"];
    for pre in UNIX_PROTECTED {
        if unix == *pre || unix.starts_with(&format!("{pre}/")) {
            return Some("目标位于系统关键目录,已拒绝");
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

struct FindFilesTool;
#[async_trait]
impl Tool for FindFilesTool {
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "find_files".into(),
            description: "在某目录下递归查找文件(按文件名子串 / 扩展名过滤),返回匹配路径列表。\
                有结果数与遍历总数上限,适合定位文件,不适合全盘扫描。"
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "root": { "type": "string", "description": "起始目录绝对路径" },
                    "name_contains": { "type": "string", "description": "可选:文件名包含的子串(忽略大小写)" },
                    "extension": { "type": "string", "description": "可选:扩展名过滤,不带点,如 rs、txt" },
                    "max_results": { "type": "integer", "description": "最多返回条数,缺省 200,上限 2000" }
                },
                "required": ["root"]
            }),
        }
    }
    async fn run(&self, args: Value) -> ToolResult {
        let Some(root) = args.get("root").and_then(Value::as_str) else {
            return ToolResult::err("缺少参数 root");
        };
        let root = root.trim().to_string();
        if !Path::new(&root).is_dir() {
            return ToolResult::err(format!("root 不是目录或不存在: {root}"));
        }
        let name_contains = args
            .get("name_contains")
            .and_then(Value::as_str)
            .map(|s| s.trim().to_lowercase())
            .filter(|s| !s.is_empty());
        let extension = args
            .get("extension")
            .and_then(Value::as_str)
            .map(|s| s.trim().trim_start_matches('.').to_string())
            .filter(|s| !s.is_empty());
        let max_results = args
            .get("max_results")
            .and_then(Value::as_u64)
            .map(|n| n as usize)
            .unwrap_or(FIND_RESULTS_DEFAULT)
            .clamp(1, FIND_RESULTS_MAX);

        let joined = tokio::task::spawn_blocking(move || {
            let mut found: Vec<String> = Vec::new();
            let mut walked = 0usize;
            let mut truncated = false;
            for entry in WalkDir::new(&root).into_iter().filter_map(Result::ok) {
                walked += 1;
                if walked > FIND_WALK_CAP {
                    truncated = true;
                    break;
                }
                if !entry.file_type().is_file() {
                    continue;
                }
                if let Some(nc) = &name_contains {
                    if !entry.file_name().to_string_lossy().to_lowercase().contains(nc) {
                        continue;
                    }
                }
                if let Some(ext) = &extension {
                    let ok = entry
                        .path()
                        .extension()
                        .and_then(|e| e.to_str())
                        .map(|e| e.eq_ignore_ascii_case(ext))
                        .unwrap_or(false);
                    if !ok {
                        continue;
                    }
                }
                found.push(entry.path().display().to_string());
                if found.len() >= max_results {
                    break;
                }
            }
            (found, truncated)
        })
        .await;

        match joined {
            Ok((found, truncated)) => {
                if found.is_empty() {
                    return ToolResult::ok("(未找到匹配文件)");
                }
                let mut s = format!("找到 {} 个文件:\n{}", found.len(), found.join("\n"));
                if truncated {
                    s.push_str("\n…(已达遍历上限,结果可能不完整,请缩小 root)");
                }
                ToolResult::ok(s)
            }
            Err(e) => ToolResult::err(format!("查找任务异常: {e}")),
        }
    }
}

struct ReadFileTool;
#[async_trait]
impl Tool for ReadFileTool {
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "read_file".into(),
            description: "读取一个文本文件的内容(有大小上限,超出截断;按 UTF-8 lossy 解码)".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "文件绝对路径" },
                    "max_bytes": { "type": "integer", "description": "最多读取字节数,缺省 262144(256KB),上限 4MB" }
                },
                "required": ["path"]
            }),
        }
    }
    async fn run(&self, args: Value) -> ToolResult {
        let Some(path) = args.get("path").and_then(Value::as_str) else {
            return ToolResult::err("缺少参数 path");
        };
        let path = path.trim().to_string();
        let max_bytes = args
            .get("max_bytes")
            .and_then(Value::as_u64)
            .unwrap_or(READ_BYTES_DEFAULT)
            .clamp(1, READ_BYTES_MAX);

        let joined = tokio::task::spawn_blocking(move || {
            use std::io::Read;
            let meta = std::fs::metadata(&path).map_err(|e| format!("读取失败: {e}"))?;
            if !meta.is_file() {
                return Err(format!("不是文件: {path}"));
            }
            let total = meta.len();
            let mut file = std::fs::File::open(&path).map_err(|e| format!("打开失败: {e}"))?;
            let mut buf = vec![0u8; max_bytes.min(total) as usize];
            let n = file.read(&mut buf).map_err(|e| format!("读取失败: {e}"))?;
            buf.truncate(n);
            let text = String::from_utf8_lossy(&buf).into_owned();
            let note = if total > max_bytes {
                format!("(文件共 {}, 仅读取前 {})\n", human_size(total), human_size(max_bytes))
            } else {
                String::new()
            };
            Ok(format!("{note}{text}"))
        })
        .await;
        match joined {
            Ok(Ok(s)) => ToolResult::ok(s),
            Ok(Err(e)) => ToolResult::err(e),
            Err(e) => ToolResult::err(format!("读取任务异常: {e}")),
        }
    }
}

struct WriteFileTool;
#[async_trait]
impl Tool for WriteFileTool {
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "write_file".into(),
            description: "把文本内容写入文件(覆盖已有内容;自动创建上级目录)".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "目标文件绝对路径" },
                    "content": { "type": "string", "description": "要写入的文本内容" }
                },
                "required": ["path", "content"]
            }),
        }
    }
    async fn run(&self, args: Value) -> ToolResult {
        let Some(path) = args.get("path").and_then(Value::as_str) else {
            return ToolResult::err("缺少参数 path");
        };
        let path = path.trim().to_string();
        if path.is_empty() {
            return ToolResult::err("path 不能为空");
        }
        let content = args.get("content").and_then(Value::as_str).unwrap_or("").to_string();
        let joined = tokio::task::spawn_blocking(move || {
            if let Some(reason) = protected_path_reason(&path) {
                return Err(format!("拒绝写入:{reason}"));
            }
            if let Some(parent) = Path::new(&path).parent() {
                std::fs::create_dir_all(parent).map_err(|e| format!("创建上级目录失败: {e}"))?;
            }
            let bytes = content.len();
            std::fs::write(&path, content).map_err(|e| format!("写入失败: {e}"))?;
            Ok::<String, String>(format!("已写入 {path}({} )", human_size(bytes as u64)))
        })
        .await;
        match joined {
            Ok(Ok(s)) => ToolResult::ok(s),
            Ok(Err(e)) => ToolResult::err(e),
            Err(e) => ToolResult::err(format!("写入任务异常: {e}")),
        }
    }
}

struct ListDirTool;
#[async_trait]
impl Tool for ListDirTool {
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "list_dir".into(),
            description: "列出某目录的直接子项(文件 / 子目录),含类型与大小".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "目录绝对路径" }
                },
                "required": ["path"]
            }),
        }
    }
    async fn run(&self, args: Value) -> ToolResult {
        let Some(path) = args.get("path").and_then(Value::as_str) else {
            return ToolResult::err("缺少参数 path");
        };
        let path = path.trim().to_string();
        let joined = tokio::task::spawn_blocking(move || {
            let rd = std::fs::read_dir(&path).map_err(|e| format!("读取目录失败: {e}"))?;
            let mut lines: Vec<String> = Vec::new();
            for entry in rd.filter_map(Result::ok) {
                if lines.len() >= LIST_DIR_CAP {
                    break;
                }
                let name = entry.file_name().to_string_lossy().into_owned();
                let meta = match entry.metadata() {
                    Ok(m) => m,
                    Err(_) => continue,
                };
                if meta.is_dir() {
                    lines.push(format!("[目录] {name}/"));
                } else {
                    lines.push(format!("[文件] {name}  {}", human_size(meta.len())));
                }
            }
            if lines.is_empty() {
                return Ok::<String, String>("(空目录)".to_string());
            }
            Ok(format!("{} 项:\n{}", lines.len(), lines.join("\n")))
        })
        .await;
        match joined {
            Ok(Ok(s)) => ToolResult::ok(s),
            Ok(Err(e)) => ToolResult::err(e),
            Err(e) => ToolResult::err(format!("列目录任务异常: {e}")),
        }
    }
}

struct FileInfoTool;
#[async_trait]
impl Tool for FileInfoTool {
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "file_info".into(),
            description: "查看文件 / 目录的元信息:类型、大小、是否只读、修改时间".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "文件或目录绝对路径" }
                },
                "required": ["path"]
            }),
        }
    }
    async fn run(&self, args: Value) -> ToolResult {
        let Some(path) = args.get("path").and_then(Value::as_str) else {
            return ToolResult::err("缺少参数 path");
        };
        let path = path.trim().to_string();
        let joined = tokio::task::spawn_blocking(move || {
            let meta = std::fs::metadata(&path).map_err(|e| format!("读取元信息失败: {e}"))?;
            let kind = if meta.is_dir() { "目录" } else { "文件" };
            let readonly = if meta.permissions().readonly() { "是" } else { "否" };
            // 修改时间转 Unix 秒(失败留空)
            let mtime = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs())
                .map(|s| format!("{s}(Unix 秒)"))
                .unwrap_or_else(|| "未知".to_string());
            Ok::<String, String>(format!(
                "路径: {path}\n类型: {kind}\n大小: {}\n只读: {readonly}\n修改时间: {mtime}",
                human_size(meta.len())
            ))
        })
        .await;
        match joined {
            Ok(Ok(s)) => ToolResult::ok(s),
            Ok(Err(e)) => ToolResult::err(e),
            Err(e) => ToolResult::err(format!("查询任务异常: {e}")),
        }
    }
}

struct CopyFileTool;
#[async_trait]
impl Tool for CopyFileTool {
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "copy_file".into(),
            description: "复制一个文件到目标路径(自动创建目标上级目录;仅文件,不支持目录)".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "src": { "type": "string", "description": "源文件绝对路径" },
                    "dest": { "type": "string", "description": "目标文件绝对路径" }
                },
                "required": ["src", "dest"]
            }),
        }
    }
    async fn run(&self, args: Value) -> ToolResult {
        let (Some(src), Some(dest)) = (
            args.get("src").and_then(Value::as_str),
            args.get("dest").and_then(Value::as_str),
        ) else {
            return ToolResult::err("缺少参数 src / dest");
        };
        let src = src.trim().to_string();
        let dest = dest.trim().to_string();
        let joined = tokio::task::spawn_blocking(move || {
            // dest 会被覆盖写入,过护栏(与 write/move/delete 一致;src 仅读不设限)
            if let Some(reason) = protected_path_reason(&dest) {
                return Err(format!("拒绝复制:{reason}"));
            }
            if !Path::new(&src).is_file() {
                return Err(format!("源不是文件或不存在: {src}"));
            }
            if let Some(parent) = Path::new(&dest).parent() {
                std::fs::create_dir_all(parent).map_err(|e| format!("创建目标上级目录失败: {e}"))?;
            }
            let n = std::fs::copy(&src, &dest).map_err(|e| format!("复制失败: {e}"))?;
            Ok::<String, String>(format!("已复制 {src} → {dest}({})", human_size(n)))
        })
        .await;
        match joined {
            Ok(Ok(s)) => ToolResult::ok(s),
            Ok(Err(e)) => ToolResult::err(e),
            Err(e) => ToolResult::err(format!("复制任务异常: {e}")),
        }
    }
}

struct MovePathTool;
#[async_trait]
impl Tool for MovePathTool {
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "move_path".into(),
            description: "移动或重命名文件 / 目录(src→dest;同盘为重命名,跨盘可能失败需改用复制+删除)".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "src": { "type": "string", "description": "源文件 / 目录绝对路径" },
                    "dest": { "type": "string", "description": "目标绝对路径" }
                },
                "required": ["src", "dest"]
            }),
        }
    }
    async fn run(&self, args: Value) -> ToolResult {
        let (Some(src), Some(dest)) = (
            args.get("src").and_then(Value::as_str),
            args.get("dest").and_then(Value::as_str),
        ) else {
            return ToolResult::err("缺少参数 src / dest");
        };
        let src = src.trim().to_string();
        let dest = dest.trim().to_string();
        let joined = tokio::task::spawn_blocking(move || {
            // src 被移走、dest 被覆盖,两端都要过护栏
            if let Some(reason) = protected_path_reason(&src).or_else(|| protected_path_reason(&dest)) {
                return Err(format!("拒绝移动:{reason}"));
            }
            if !Path::new(&src).exists() {
                return Err(format!("源不存在: {src}"));
            }
            if let Some(parent) = Path::new(&dest).parent() {
                std::fs::create_dir_all(parent).map_err(|e| format!("创建目标上级目录失败: {e}"))?;
            }
            std::fs::rename(&src, &dest).map_err(|e| format!("移动失败: {e}"))?;
            Ok::<String, String>(format!("已移动 {src} → {dest}"))
        })
        .await;
        match joined {
            Ok(Ok(s)) => ToolResult::ok(s),
            Ok(Err(e)) => ToolResult::err(e),
            Err(e) => ToolResult::err(format!("移动任务异常: {e}")),
        }
    }
}

struct MakeDirTool;
#[async_trait]
impl Tool for MakeDirTool {
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "make_dir".into(),
            description: "创建目录(含多级上级目录;已存在不报错)".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "要创建的目录绝对路径" }
                },
                "required": ["path"]
            }),
        }
    }
    async fn run(&self, args: Value) -> ToolResult {
        let Some(path) = args.get("path").and_then(Value::as_str) else {
            return ToolResult::err("缺少参数 path");
        };
        let path = path.trim().to_string();
        if path.is_empty() {
            return ToolResult::err("path 不能为空");
        }
        let joined = tokio::task::spawn_blocking(move || {
            // 在系统关键目录下建目录同样有害,过护栏
            if let Some(reason) = protected_path_reason(&path) {
                return Err(format!("拒绝创建:{reason}"));
            }
            std::fs::create_dir_all(&path).map_err(|e| format!("创建目录失败: {e}"))?;
            Ok::<String, String>(format!("已创建目录 {path}"))
        })
        .await;
        match joined {
            Ok(Ok(s)) => ToolResult::ok(s),
            Ok(Err(e)) => ToolResult::err(e),
            Err(e) => ToolResult::err(format!("创建目录任务异常: {e}")),
        }
    }
}

struct DeletePathTool;
#[async_trait]
impl Tool for DeletePathTool {
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "delete_path".into(),
            description: "删除文件或目录。删非空目录需 recursive=true。⚠️ 直接删除、不进回收站、不可恢复,务必先确认路径正确。".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "要删除的文件 / 目录绝对路径" },
                    "recursive": { "type": "boolean", "description": "目录非空时需置 true 才递归删除(默认 false)" }
                },
                "required": ["path"]
            }),
        }
    }
    async fn run(&self, args: Value) -> ToolResult {
        let Some(path) = args.get("path").and_then(Value::as_str) else {
            return ToolResult::err("缺少参数 path");
        };
        let path = path.trim().to_string();
        if path.is_empty() {
            return ToolResult::err("path 不能为空");
        }
        let recursive = args.get("recursive").and_then(Value::as_bool).unwrap_or(false);
        let joined = tokio::task::spawn_blocking(move || {
            if let Some(reason) = protected_path_reason(&path) {
                return Err(format!("拒绝删除:{reason}"));
            }
            let meta = std::fs::symlink_metadata(&path).map_err(|e| format!("路径不存在或不可访问: {e}"))?;
            if meta.is_dir() {
                if recursive {
                    std::fs::remove_dir_all(&path).map_err(|e| format!("删除目录失败: {e}"))?;
                } else {
                    // 非递归只删空目录;非空时给出明确提示而非笼统报错
                    std::fs::remove_dir(&path)
                        .map_err(|e| format!("删除目录失败(非空目录需 recursive=true): {e}"))?;
                }
                Ok::<String, String>(format!("已删除目录 {path}"))
            } else {
                std::fs::remove_file(&path).map_err(|e| format!("删除文件失败: {e}"))?;
                Ok(format!("已删除文件 {path}"))
            }
        })
        .await;
        match joined {
            Ok(Ok(s)) => ToolResult::ok(s),
            Ok(Err(e)) => ToolResult::err(e),
            Err(e) => ToolResult::err(format!("删除任务异常: {e}")),
        }
    }
}
