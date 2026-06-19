//! 编程 Agent:工具集(读/写/列目录/执行命令,均限定工作区)+ 系统提示词。
//! ReAct 循环在 `agent::coding::commands::send_coding_message`(便于逐步落库 + 推前端进度)。

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};

use crate::agent::resolve_in_workspace;
use crate::agent::core::{Tool, ToolDef, ToolRegistry, ToolResult};

/// 单次 read_file 返回上限(字节);超出截断,避免撑爆上下文。
const MAX_FILE_READ_BYTES: usize = 200_000;
/// run_command 超时(秒)。取 180s:编译 / 测试 / `npm install` 等常超 60s;
/// 注:长驻服务(`npm run dev`)请走「开发服务器」常驻进程,不要用 run_command(会被超时杀)。
const RUN_TIMEOUT_SECS: u64 = 180;
/// 工具结果回灌模型的字符上限。
const TOOL_OUTPUT_CAP: usize = 20_000;
/// 命令输出超长时的头部保留量(字符):保留命令开头的进度 / 环境信息。
const OUTPUT_HEAD_CAP: usize = 8_000;
/// 命令输出超长时的尾部保留量(字符):编译 / 测试的真因报错通常在末尾,优先保尾。
const OUTPUT_TAIL_CAP: usize = 12_000;

/// 编程 Agent 系统提示词。
pub const SYSTEM_PROMPT: &str = "你是一个编程 Agent,在一个受限的工作区目录内**亲自动手完成任务**,而不是教用户怎么做。\n\
你有这些工具:read_file(读文件)、write_file(整文件写入/新建,会覆盖)、replace_in_file(对已存在文件做局部精确替换)、list_dir(列目录)、search_files(按关键词搜索工作区内容、定位代码)、run_command(在工作区内执行命令,有超时)。\n\
铁律:\n\
- 用户要求创建 / 修改 / 运行代码时,**必须调用工具实际执行**:写文件就调用 write_file/replace_in_file,运行就调用 run_command。\n\
- **严禁**只给出「你可以这样做」「把代码保存为 xxx 再运行」之类的说明,或让用户自己去保存 / 运行 / 用在线编译器——那是错误做法。\n\
- **修改已存在的文件优先用 replace_in_file**(只替换变化片段,省 token 且不破坏其它内容);仅新建文件或需整体重写时才用 write_file。\n\
- 不确定改哪里时,先用 search_files / read_file 定位,再动手。\n\
- 路径一律用工作区内的相对路径;一般流程:(必要时)search_files/list_dir/read_file 了解现状 → write_file/replace_in_file 改代码 → run_command 运行验证 → 看输出/报错再修。\n\
- **网页 / 前端项目不要自己起预览或 HTTP 服务器**:`python -m http.server`、`npm run dev` 这类长驻进程别用 run_command 跑(会被超时杀),也别用 `&` 后台跑,更别 `pkill` 已有服务(会误杀应用自带的预览服务)。本应用有专门的「预览」面板负责起服务并在内嵌窗口展示,你只需把文件写好;要验证就用会结束的命令(如用 node 校验 JS 语法、`npm run build`/测试),长驻服务一律交给「预览」面板。\n\
- **若下方提供了【任务计划】**(由 Plan 模式产出的 todo 清单):按顺序逐项执行,**每完成一步就调用 update_plan 把对应项的 done 置为 true**(每次传完整清单,保持其余项不变),让进度可见;计划之外发现必要步骤可一并补进清单。\n\
- 只要还有未完成的步骤,就继续执行、不要停下来等我确认。\n\
- 【整个任务全部完成、确实没有更多步骤】时,先用简洁中文总结「做了什么、运行结果如何」,再调用 finish 工具声明完成(只在真正全部完成时调,有未完成项绝不调)。\n\
例:用户说「写个 python 脚本打印 hello 并运行」,你应当先调 write_file 写 hello.py,再调 run_command 执行 `python hello.py`,然后报告输出,而不是讲解手动步骤。";

/// Plan(方案)模式系统提示词:只调研、不动手。仅注册只读工具(read_file/list_dir/search_files),
/// 引导模型先摸清现状再产出分步实现方案,把「写 / 跑」留到用户切到 Act 模式后执行。
pub const PLAN_SYSTEM_PROMPT: &str = "你现在处于「方案(Plan)」模式:**只做调研与方案设计,绝不动手改动或运行任何东西**。\n\
你只有这些只读工具:read_file(读文件)、list_dir(列目录)、search_files(按关键词搜索定位代码)。\n\
铁律:\n\
- 先用 search_files / list_dir / read_file 充分调研工作区现状,理解相关代码与依赖,再下结论。\n\
- 产出一份**分步实现方案**,清晰说明:要改 / 新建哪些文件(逐个列出)、每步具体做什么、潜在风险与边界、以及完成后如何验证(跑什么命令、看什么结果)。\n\
- **严禁**声称你已经动手做了、已写入或已运行过任何代码——本模式下你没有写入 / 执行能力。\n\
- **严禁**让用户自己去保存文件、复制代码或手动运行——方案是给「切到 Act 模式后由你亲自执行」用的,不是给用户照做的。\n\
- 调研清楚后,**必须调用 update_plan 工具**把方案登记为结构化分步清单(todos:每步一个 title,按执行顺序,done 全为 false);文字说明可补充,但可执行的步骤一定要进 update_plan。\n\
- 方案末尾提示用户:确认方案后切换到「执行(Act)」模式,我再按这份计划逐步落地。";

/// 命令失败后自动续修的引导词:run_command 非零退出但模型想收尾时注入,逼它定位并修复后重跑验证。
/// attempt 为本次是第几次自动续修(从 1 起);第 2 次起追加退避提示,促其换思路 / 先做最小复现。
pub fn auto_fix_prompt(attempt: usize) -> String {
    let mut s = String::from(
        "刚才执行的命令以非零退出码失败(详见上面的命令输出 / stderr)。\
请先用一句话点明【失败的根本原因】(重点看 stderr 最末尾的真正报错),再据此用 read_file / search_files 排查、\
用 replace_in_file / write_file 针对根因修改(不要盲目试错),然后重新 run_command 验证,直到命令成功退出为止。\
在确认通过前不要结束,也不要把失败当作已完成。",
    );
    // 退避:第 2 次起说明上一轮修复无效,引导换思路 / 先做最小复现,避免重复同一错误尝试。
    if attempt >= 2 {
        s.push_str(
            "\n注意:上一次的修复尝试仍未通过。不要重复同样的改法——请换一个思路:\
重新审视报错最末尾的真正原因,必要时先写一个最小复现(精简到能稳定触发该错误的最小代码 / 命令)再逐步排查。",
        );
    }
    s
}

/// 自主续航:模型过早收尾(还有未完成 todo)时注入的续写提示,推动它继续干而非停下等确认。
pub fn auto_continue_prompt() -> String {
    "当前任务还有未完成的步骤。请继续执行下一个未完成的 todo,不要停下来等我确认。\
每完成一步就用 update_plan 把它的 done 置 true;当【整个任务全部完成】时再调用 finish 工具声明结束。"
        .to_string()
}

/// 把会话存的 plan_todos(JSON 数组 `[{title,done}]`)渲染成注入 Act 模式的 system 文本;
/// 空 / 解析失败 / 空数组返回 None(不注入)。
pub fn plan_system_message(plan_todos: &str) -> Option<String> {
    let trimmed = plan_todos.trim();
    if trimmed.is_empty() {
        return None;
    }
    let items = serde_json::from_str::<Value>(trimmed).ok()?;
    let items = items.as_array()?;
    if items.is_empty() {
        return None;
    }
    let mut s =
        String::from("【任务计划】按顺序执行,每完成一步调 update_plan 把对应项 done 置 true:\n");
    for it in items {
        let title = it.get("title").and_then(Value::as_str).unwrap_or("");
        let done = it.get("done").and_then(Value::as_bool).unwrap_or(false);
        s.push_str(if done { "- [x] " } else { "- [ ] " });
        s.push_str(title);
        s.push('\n');
    }
    Some(s)
}

/// 命令执行环境:本机 host,或 Docker 沙盒(共享容器内的某工作目录)。
/// 文件读写工具始终走宿主(workspace 目录已挂载进容器),只有 run_command 按此路由。
#[derive(Clone)]
pub enum ExecConfig {
    Host,
    Docker { container: String, workdir: String },
}

/// Agent 工作模式:Plan(只调研出方案)/ Act(亲自动手执行)。
/// 同会话内每轮临时态,不持久化、不入库;仅决定本轮系统提示词与可用工具集。
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum AgentMode {
    Plan,
    Act,
}

impl AgentMode {
    /// 由前端传入的字符串推断模式:plan → Plan,其余(含缺省 / 旧前端不传)→ Act。
    /// 默认 Act 以向后兼容旧前端(不传 mode 时维持原有「动手执行」行为)。
    pub fn from_code(code: &str) -> Self {
        match code.trim().to_lowercase().as_str() {
            "plan" => AgentMode::Plan,
            _ => AgentMode::Act,
        }
    }
}

/// 构造编程 Agent 的工具注册表(文件工具绑定宿主工作区;run_command 按 exec 路由 host/Docker)。
/// 按 mode 从源头裁剪可用工具:Plan 只注册只读工具(read_file/list_dir/search_files),
/// 从根上不挂 write_file/replace_in_file/run_command,杜绝 Plan 模式越权改动或执行。
pub fn build_registry(workspace: PathBuf, exec: ExecConfig, mode: AgentMode) -> ToolRegistry {
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(ReadFileTool { workspace: workspace.clone() }));
    registry.register(Arc::new(ListDirTool { workspace: workspace.clone() }));
    registry.register(Arc::new(SearchFilesTool { workspace: workspace.clone() }));
    // 计划工具两模式都挂:Plan 产出 todo 清单,Act 按 todo 执行并勾选进度(持久化由 commands 拦截落库)
    registry.register(Arc::new(UpdatePlanTool));
    if mode == AgentMode::Act {
        registry.register(Arc::new(WriteFileTool { workspace: workspace.clone() }));
        registry.register(Arc::new(ReplaceInFileTool { workspace: workspace.clone() }));
        registry.register(Arc::new(RunCommandTool { workspace, exec }));
        // 自主续航的显式完成信号:模型全部做完时调 finish,命令层据此停止外层续航
        registry.register(Arc::new(FinishTool));
    }
    registry
}

/// 构造执行命令:Docker 走 `docker exec <容器> sh -lc "cd '<workdir>' && <cmd>"`;
/// host 走 cmd /C(Windows)或 sh -c 并设 cwd。不设 kill_on_drop / stdio(调用方设)。
pub fn build_exec_command(
    exec: &ExecConfig,
    workspace: &Path,
    command: &str,
) -> tokio::process::Command {
    match exec {
        ExecConfig::Docker { container, workdir } => {
            let script = format!("cd '{}' && {}", workdir.replace('\'', "'\\''"), command);
            let mut c = tokio::process::Command::new("docker");
            c.arg("exec").arg(container).arg("sh").arg("-lc").arg(script);
            c
        }
        ExecConfig::Host => {
            let mut c = if cfg!(windows) {
                let mut c = tokio::process::Command::new("cmd");
                c.arg("/C").arg(command);
                c
            } else {
                let mut c = tokio::process::Command::new("sh");
                c.arg("-c").arg(command);
                c
            };
            c.current_dir(workspace);
            c
        }
    }
}

/// update_plan:产出 / 更新分步计划。工具层只校验并回显进度、不碰 DB——
/// 持久化由 agent::coding::commands::send_coding_message 拦截本工具调用把 todos 写入会话(保持分层)。
struct UpdatePlanTool;
#[async_trait]
impl Tool for UpdatePlanTool {
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "update_plan".into(),
            description: "产出 / 更新本任务的分步计划(todo 清单)。Plan 模式调研后用它给出完整步骤;Act 模式每完成一步用它把该步 done 置 true。每次传【完整】清单(全量覆盖,其余项保持不变)。".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "todos": {
                        "type": "array",
                        "description": "完整步骤清单,按执行顺序",
                        "items": {
                            "type": "object",
                            "properties": {
                                "title": { "type": "string", "description": "该步骤一句话描述" },
                                "done": { "type": "boolean", "description": "是否已完成,缺省 false" }
                            },
                            "required": ["title"]
                        }
                    }
                },
                "required": ["todos"]
            }),
        }
    }
    async fn run(&self, args: Value) -> ToolResult {
        let Some(todos) = args.get("todos").and_then(Value::as_array) else {
            return ToolResult::err("缺少参数 todos(数组)");
        };
        if todos.is_empty() {
            return ToolResult::err("todos 不能为空");
        }
        let total = todos.len();
        let done = todos
            .iter()
            .filter(|t| t.get("done").and_then(Value::as_bool).unwrap_or(false))
            .count();
        ToolResult::ok(format!("计划已更新:共 {total} 步,已完成 {done} 步"))
    }
}

/// finish:模型在【整个任务真正完成】时调用,作为自主续航的显式结束信号。
/// 工具层只回显;是否结束循环由 agent::coding::commands::send_coding_message 拦截本工具决定(分层一致)。
struct FinishTool;
#[async_trait]
impl Tool for FinishTool {
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "finish".into(),
            description: "当【整个任务已全部完成、确实没有更多步骤】时调用,声明结束。还有未完成步骤时绝不调用——继续执行下一步。".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "summary": { "type": "string", "description": "对已完成工作的简短总结(可选)" }
                }
            }),
        }
    }
    async fn run(&self, _args: Value) -> ToolResult {
        ToolResult::ok("已确认任务完成。")
    }
}

struct ReadFileTool {
    workspace: PathBuf,
}
#[async_trait]
impl Tool for ReadFileTool {
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "read_file".into(),
            description: "读取工作区内某个文件的文本内容".into(),
            input_schema: json!({
                "type": "object",
                "properties": { "path": { "type": "string", "description": "工作区内相对路径" } },
                "required": ["path"]
            }),
        }
    }
    async fn run(&self, args: Value) -> ToolResult {
        let Some(path) = args.get("path").and_then(Value::as_str) else {
            return ToolResult::err("缺少参数 path");
        };
        let full = match resolve_in_workspace(&self.workspace, path) {
            Ok(p) => p,
            Err(e) => return ToolResult::err(e.to_string()),
        };
        match tokio::fs::read(&full).await {
            Ok(bytes) => {
                let truncated = bytes.len() > MAX_FILE_READ_BYTES;
                let end = bytes.len().min(MAX_FILE_READ_BYTES);
                let text = String::from_utf8_lossy(&bytes[..end]).into_owned();
                if truncated {
                    ToolResult::ok(format!("{text}\n…(文件过大,已截断)"))
                } else {
                    ToolResult::ok(text)
                }
            }
            Err(e) => ToolResult::err(format!("读取失败: {e}")),
        }
    }
}

struct WriteFileTool {
    workspace: PathBuf,
}
#[async_trait]
impl Tool for WriteFileTool {
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "write_file".into(),
            description: "把内容写入工作区内某个文件(覆盖;自动创建父目录)".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "工作区内相对路径" },
                    "content": { "type": "string", "description": "要写入的完整文本" }
                },
                "required": ["path", "content"]
            }),
        }
    }
    async fn run(&self, args: Value) -> ToolResult {
        let Some(path) = args.get("path").and_then(Value::as_str) else {
            return ToolResult::err("缺少参数 path");
        };
        let content = args.get("content").and_then(Value::as_str).unwrap_or("");
        let full = match resolve_in_workspace(&self.workspace, path) {
            Ok(p) => p,
            Err(e) => return ToolResult::err(e.to_string()),
        };
        if let Some(parent) = full.parent() {
            if let Err(e) = tokio::fs::create_dir_all(parent).await {
                return ToolResult::err(format!("创建目录失败: {e}"));
            }
        }
        match tokio::fs::write(&full, content.as_bytes()).await {
            Ok(_) => ToolResult::ok(format!("已写入 {} 字节到 {path}", content.len())),
            Err(e) => ToolResult::err(format!("写入失败: {e}")),
        }
    }
}

struct ListDirTool {
    workspace: PathBuf,
}
#[async_trait]
impl Tool for ListDirTool {
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "list_dir".into(),
            description: "列出工作区内某个目录的条目(目录名带 / 后缀)".into(),
            input_schema: json!({
                "type": "object",
                "properties": { "path": { "type": "string", "description": "工作区内相对路径,缺省为根" } }
            }),
        }
    }
    async fn run(&self, args: Value) -> ToolResult {
        let path = args.get("path").and_then(Value::as_str).unwrap_or(".");
        let full = match resolve_in_workspace(&self.workspace, path) {
            Ok(p) => p,
            Err(e) => return ToolResult::err(e.to_string()),
        };
        let mut rd = match tokio::fs::read_dir(&full).await {
            Ok(rd) => rd,
            Err(e) => return ToolResult::err(format!("列目录失败: {e}")),
        };
        let mut names: Vec<String> = Vec::new();
        while let Ok(Some(entry)) = rd.next_entry().await {
            let is_dir = entry.file_type().await.map(|t| t.is_dir()).unwrap_or(false);
            let name = entry.file_name().to_string_lossy().into_owned();
            names.push(if is_dir { format!("{name}/") } else { name });
        }
        names.sort();
        if names.is_empty() {
            ToolResult::ok("(空目录)")
        } else {
            ToolResult::ok(names.join("\n"))
        }
    }
}

struct RunCommandTool {
    workspace: PathBuf,
    exec: ExecConfig,
}
#[async_trait]
impl Tool for RunCommandTool {
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "run_command".into(),
            description: "在工作区目录内执行一条 shell 命令(有超时;返回退出码 + stdout/stderr)".into(),
            input_schema: json!({
                "type": "object",
                "properties": { "command": { "type": "string", "description": "要执行的命令行" } },
                "required": ["command"]
            }),
        }
    }
    async fn run(&self, args: Value) -> ToolResult {
        let Some(cmd) = args.get("command").and_then(Value::as_str) else {
            return ToolResult::err("缺少参数 command");
        };
        run_command_in(&self.workspace, cmd, &self.exec).await
    }
}

/// 执行一条 shell 命令(host 或 Docker 沙盒;超时 + kill_on_drop + 输出截断)。
/// 供编程 Agent 的 run_command 工具与「用户在终端直接敲命令」共用。
pub async fn run_command_in(workspace: &Path, command: &str, exec: &ExecConfig) -> ToolResult {
    let mut cmd = build_exec_command(exec, workspace, command);
    cmd.kill_on_drop(true)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());

    let out = match tokio::time::timeout(
        std::time::Duration::from_secs(RUN_TIMEOUT_SECS),
        cmd.output(),
    )
    .await
    {
        Ok(Ok(o)) => o,
        Ok(Err(e)) => {
            return ToolResult::err(match exec {
                ExecConfig::Docker { .. } => format!(
                    "Docker 命令启动失败: {e}(检查 Docker 是否运行、沙盒容器是否就绪,或在沙盒设置切回本机执行)"
                ),
                ExecConfig::Host => format!("命令启动失败: {e}"),
            });
        }
        Err(_) => return ToolResult::err(format!("命令超时(>{RUN_TIMEOUT_SECS}s)已终止")),
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
        content: cap_command_output(&s),
        is_error: !out.status.success(),
    }
}

/// 命令输出截断:不超上限直接返回;超长则保留头部 + 尾部并在中间标省略。
/// 编译 / 测试的真因报错常在末尾,纯头部截断会丢失 stderr 真因,故头尾各留一段。
fn cap_command_output(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= TOOL_OUTPUT_CAP {
        return s.to_string();
    }
    let head: String = chars[..OUTPUT_HEAD_CAP].iter().collect();
    let tail_start = chars.len() - OUTPUT_TAIL_CAP;
    let tail: String = chars[tail_start..].iter().collect();
    let omitted = tail_start - OUTPUT_HEAD_CAP;
    format!("{head}\n…(中间省略约 {omitted} 字符,已保留头部与尾部;真因报错通常在尾部)…\n{tail}")
}

// ===================== replace_in_file(SEARCH/REPLACE 局部替换) =====================

/// 解析 Cline 风格的 SEARCH/REPLACE 块(可多块)。标记宽松匹配:行以 `<<<<<<<`/`=======`/`>>>>>>>` 起始即可。
/// 返回每块的(原文, 替换为);格式错误返回 Err 描述。
fn parse_diff_blocks(diff: &str) -> std::result::Result<Vec<(String, String)>, String> {
    #[derive(Clone, Copy)]
    enum S {
        Idle,
        Search,
        Replace,
    }
    let mut state = S::Idle;
    let mut search: Vec<&str> = Vec::new();
    let mut replace: Vec<&str> = Vec::new();
    let mut blocks: Vec<(String, String)> = Vec::new();
    for line in diff.lines() {
        let marker = line.trim_end();
        // 标记识别与状态绑定:Replace 状态下只认 >>>>>>> 结束,其余行(即便以 ======= / <<<<<<<
        // 起始,如分隔线、被注释的冲突标记)都算替换内容,避免把内容里的 ======= 误判为格式错误。
        match state {
            S::Idle => {
                // 块外只识别 SEARCH 起始,其它文字忽略
                if marker.starts_with("<<<<<<<") {
                    state = S::Search;
                    search.clear();
                    replace.clear();
                }
            }
            S::Search => {
                if marker.starts_with("=======") {
                    state = S::Replace;
                } else {
                    search.push(line);
                }
            }
            S::Replace => {
                if marker.starts_with(">>>>>>>") {
                    blocks.push((search.join("\n"), replace.join("\n")));
                    state = S::Idle;
                } else {
                    replace.push(line);
                }
            }
        }
    }
    if !matches!(state, S::Idle) {
        return Err("diff 格式错误:存在未闭合的 SEARCH/REPLACE 块".into());
    }
    if blocks.is_empty() {
        return Err("未解析到任何 SEARCH/REPLACE 块(格式:<<<<<<< SEARCH … ======= … >>>>>>> REPLACE)".into());
    }
    Ok(blocks)
}

struct ReplaceInFileTool {
    workspace: PathBuf,
}
#[async_trait]
impl Tool for ReplaceInFileTool {
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "replace_in_file".into(),
            description: "对工作区内**已存在**文件做局部精确替换(SEARCH/REPLACE 块);改局部时优先用它,省 token 且不动其它内容".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "工作区内相对路径(文件须已存在)" },
                    "diff": { "type": "string", "description": "一个或多个替换块,格式:\n<<<<<<< SEARCH\n(原文,需与文件中片段逐字一致含缩进)\n=======\n(替换为的新内容)\n>>>>>>> REPLACE" }
                },
                "required": ["path", "diff"]
            }),
        }
    }
    async fn run(&self, args: Value) -> ToolResult {
        let Some(path) = args.get("path").and_then(Value::as_str) else {
            return ToolResult::err("缺少参数 path");
        };
        let Some(diff) = args.get("diff").and_then(Value::as_str) else {
            return ToolResult::err("缺少参数 diff");
        };
        let full = match resolve_in_workspace(&self.workspace, path) {
            Ok(p) => p,
            Err(e) => return ToolResult::err(e.to_string()),
        };
        let original = match tokio::fs::read_to_string(&full).await {
            Ok(s) => s,
            Err(e) => {
                return ToolResult::err(format!(
                    "读取失败(replace_in_file 要求文件已存在,新建请用 write_file): {e}"
                ))
            }
        };
        let blocks = match parse_diff_blocks(diff) {
            Ok(b) => b,
            Err(e) => return ToolResult::err(e),
        };
        let mut content = original;
        let mut applied = 0usize;
        for (search, replace) in &blocks {
            if search.is_empty() {
                return ToolResult::err("某个 SEARCH 块为空,无法定位替换位置");
            }
            let found = content.find(search.as_str());
            match found {
                Some(idx) => {
                    content.replace_range(idx..idx + search.len(), replace.as_str());
                    applied += 1;
                }
                None => {
                    return ToolResult::err(format!(
                        "第 {} 个 SEARCH 块未在文件中找到(需逐字一致,含缩进与换行)。可先 read_file 看最新内容再重试。",
                        applied + 1
                    ))
                }
            }
        }
        if let Err(e) = tokio::fs::write(&full, content.as_bytes()).await {
            return ToolResult::err(format!("写回失败: {e}"));
        }
        ToolResult::ok(format!("已在 {path} 应用 {applied} 处替换"))
    }
}

// ===================== search_files(工作区内容搜索) =====================

/// search_files 上限:最多匹配行 / 遍历文件;跳过的常见大目录。
const SEARCH_MAX_MATCHES: usize = 200;
const SEARCH_MAX_FILES: usize = 5000;
const SEARCH_SKIP_DIRS: &[&str] =
    &[".git", "node_modules", "target", "dist", "build", ".next", ".cache", "vendor"];

struct SearchFilesTool {
    workspace: PathBuf,
}
#[async_trait]
impl Tool for SearchFilesTool {
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "search_files".into(),
            description: "在工作区内按关键词搜索文件内容(返回 路径:行号: 行内容);不知道改哪里时用它定位代码".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "要搜索的文本(子串匹配)" },
                    "path": { "type": "string", "description": "限定搜索的子目录(相对路径),缺省为整个工作区" },
                    "ignore_case": { "type": "boolean", "description": "是否忽略大小写,默认 false" }
                },
                "required": ["query"]
            }),
        }
    }
    async fn run(&self, args: Value) -> ToolResult {
        let Some(query) = args.get("query").and_then(Value::as_str) else {
            return ToolResult::err("缺少参数 query");
        };
        if query.is_empty() {
            return ToolResult::err("query 不能为空");
        }
        let sub = args.get("path").and_then(Value::as_str).unwrap_or(".");
        let ignore_case = args.get("ignore_case").and_then(Value::as_bool).unwrap_or(false);
        let root = match resolve_in_workspace(&self.workspace, sub) {
            Ok(p) => p,
            Err(e) => return ToolResult::err(e.to_string()),
        };
        let needle = if ignore_case { query.to_lowercase() } else { query.to_string() };

        let mut out: Vec<String> = Vec::new();
        let mut files_scanned = 0usize;
        let mut stack: Vec<PathBuf> = vec![root];
        'outer: while let Some(dir) = stack.pop() {
            let mut rd = match tokio::fs::read_dir(&dir).await {
                Ok(rd) => rd,
                Err(_) => continue,
            };
            while let Ok(Some(entry)) = rd.next_entry().await {
                let p = entry.path();
                let is_dir = entry.file_type().await.map(|t| t.is_dir()).unwrap_or(false);
                if is_dir {
                    let name = entry.file_name().to_string_lossy().into_owned();
                    if !SEARCH_SKIP_DIRS.contains(&name.as_str()) {
                        stack.push(p);
                    }
                    continue;
                }
                files_scanned += 1;
                if files_scanned > SEARCH_MAX_FILES {
                    break 'outer;
                }
                let bytes = match tokio::fs::read(&p).await {
                    Ok(b) => b,
                    Err(_) => continue,
                };
                if bytes.contains(&0) {
                    continue; // 含 NUL,按二进制跳过
                }
                let text = String::from_utf8_lossy(&bytes);
                let rel = p
                    .strip_prefix(&self.workspace)
                    .unwrap_or(&p)
                    .to_string_lossy()
                    .replace('\\', "/");
                for (i, line) in text.lines().enumerate() {
                    let hit = if ignore_case {
                        line.to_lowercase().contains(&needle)
                    } else {
                        line.contains(&needle)
                    };
                    if hit {
                        let shown: String = line.chars().take(300).collect();
                        out.push(format!("{}:{}: {}", rel, i + 1, shown.trim_end()));
                        if out.len() >= SEARCH_MAX_MATCHES {
                            break 'outer;
                        }
                    }
                }
            }
        }
        if out.is_empty() {
            ToolResult::ok(format!("未找到 “{query}”"))
        } else {
            let mut s = out.join("\n");
            if out.len() >= SEARCH_MAX_MATCHES {
                s.push_str(&format!("\n…(已达 {SEARCH_MAX_MATCHES} 条上限,可缩小范围)"));
            }
            ToolResult::ok(s)
        }
    }
}

// ===================== checkpoint(每轮前快照,供回退) =====================

/// 本轮开始前打检查点:git 初始化(幂等)+ 暂存全部 + 提交(--allow-empty 保证首轮即建立 HEAD)。
/// 这样 Agent 本轮的改动若改崩,可经 checkpoint_rollback 一键回到发送前状态。
/// best-effort:失败只记日志,不阻断发送(如本机/容器无 git)。
pub async fn checkpoint(workspace: &Path, exec: &ExecConfig, label: &str) {
    let msg = checkpoint_message(label);
    // label 已清洗掉会破坏双引号串 / 注入的字符,可安全嵌入 -m "..."
    let commit = format!(
        "git -c user.email=agent@veltrix.local -c user.name=veltrix commit -q -m \"{msg}\" --allow-empty"
    );
    for cmd in ["git init -q", "git add -A", commit.as_str()] {
        let r = run_command_in(workspace, cmd, exec).await;
        if r.is_error {
            tracing::warn!(
                "checkpoint 步骤失败 [{cmd}]: {}",
                r.content.chars().take(160).collect::<String>()
            );
        }
    }
}

/// 把本轮提问清洗成安全的检查点提交信息:剔除会破坏 shell 双引号串 / 注入的字符,折叠空白并截断;
/// 空则回退固定文案。供「版本回退」列表识别每个快照对应哪次任务。
fn checkpoint_message(label: &str) -> String {
    let cleaned: String = label
        .chars()
        .filter(|c| !"\"'`$\\;|&<>\n\r\t".contains(*c))
        .collect();
    let folded = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    let trimmed: String = folded.chars().take(50).collect();
    if trimmed.trim().is_empty() {
        "veltrix checkpoint".to_string()
    } else {
        trimmed
    }
}
