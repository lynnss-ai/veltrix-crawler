# 对话 → 多场景 Agent 平台:设计文档

> 目标:把现有「对话」模块演进为「调用第三方大模型 API + 多场景 Agent(编程 / RPA)+ 后期统一入口」的平台。
> 原则:**先做垂直场景跑通,再抽象统一入口;接口先行;会话状态外置;阶段三(Router)不提前做。**
> 现状基线:`llm/chat.rs` 为 OpenAI 兼容纯文本 chat(无工具);对话为一问一答;记忆中心已就位(= Agent Core 的 Memory)。

本文 = 三件「接缝」设计:① LLMProvider + Tool 接口;② 会话/消息 schema;③ 编程 Agent 模块(样板)。**均为设计稿,尚未实现。**

---

## ① LLMProvider + Tool 接口定义(地基,接口先行)

放在 `src-tauri/src/llm/agent/`(新子模块),与现有 `llm/chat.rs` 并存。

```rust
// 厂商协议类型:决定请求/响应/工具/流式的具体格式
pub enum ProviderKind {
    OpenAiCompatible, // DeepSeek/Qwen/MiMo/GLM/MiniMax(现有 5 家)
    Anthropic,        // Claude Messages API(原生工具:input_schema + content blocks)
}

/// 厂商引用(从 providers 表 + kind 解析得到)
pub struct ProviderRef {
    pub kind: ProviderKind,
    pub api_url: String,
    pub api_key: String,
    pub model: String,
}

/// 工具定义(场景无关,所有 Agent 共用的统一格式)
pub struct ToolDef {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value, // JSON Schema
}

/// 统一消息(含工具往返),取代现在的 {role, content} 裸 JSON
pub enum ChatMsg {
    System(String),
    User(Vec<ContentBlock>),                                  // 文本 / 图片(多模态)
    Assistant { text: Option<String>, tool_calls: Vec<ToolCall> },
    ToolResult { tool_call_id: String, content: String, is_error: bool },
}
pub enum ContentBlock { Text(String), ImageBase64 { mime: String, data: String } }

pub struct ToolCall { pub id: String, pub name: String, pub arguments: serde_json::Value }

/// 模型一次输出:文本 / 要求调用工具
pub struct LlmResponse {
    pub content: Option<String>,
    pub tool_calls: Vec<ToolCall>,
    pub finish_reason: FinishReason, // Stop | ToolUse | Length | Filtered
    pub usage: TokenUsage,
}
pub struct TokenUsage { pub prompt: u32, pub completion: u32 }

/// 一次请求(参数封装为结构体,符合「参数 ≤ 4」项目规范)
pub struct LlmRequest<'a> {
    pub provider: &'a ProviderRef,
    pub messages: Vec<ChatMsg>,
    pub tools: Vec<ToolDef>,
    pub options: LlmOptions, // temperature / max_tokens / tool_choice / stream
}

/// 核心接口:屏蔽各家差异。OpenAI 兼容与 Anthropic 各实现一份。
#[async_trait]
pub trait LlmProvider: Send + Sync {
    async fn chat(&self, req: LlmRequest<'_>) -> Result<LlmResponse>;
    async fn chat_stream(
        &self,
        req: LlmRequest<'_>,
        on_event: &mut dyn FnMut(StreamEvent),
    ) -> Result<LlmResponse>;
}
pub enum StreamEvent {
    TextDelta(String),
    ToolCallDelta { index: usize, name: Option<String>, args_delta: String },
    Done(FinishReason),
}
```

工具执行侧:

```rust
#[async_trait]
pub trait Tool: Send + Sync {
    fn def(&self) -> &ToolDef;
    async fn run(&self, args: serde_json::Value, ctx: &AgentContext) -> Result<ToolResult>;
}
pub struct ToolResult { pub content: String, pub is_error: bool }

/// 工具注册表:name -> Arc<dyn Tool>。给 LLM 提供 schema 列表 + 按 name 分发执行。
pub struct ToolRegistry { /* HashMap<String, Arc<dyn Tool>> */ }
```

**与现有代码的衔接(零破坏)**
- 现有 `chat_completion` / `chat_completion_stream`(被 intent 分析、标题生成、会话摘要、记忆提取使用)→ 保留,改为「`OpenAiCompatibleProvider` + 空 tools」的薄包装,行为完全不变。
- `providers` 表 / `PROVIDER_PRESETS` 加 `kind` 字段(默认 `openai`);新增 Anthropic 预设时填 `anthropic`,走原生分支。
- 上层 Agent 永远只面对 `LlmProvider` trait,换模型只改 `ProviderRef`。

---

## ② 会话 / 消息 schema 重设计(让会话能承载多场景 + 工具往返)

遵循项目约定:**改 entity + `init_schema` 追加 `ALTER TABLE ... ADD COLUMN ... DEFAULT`(旧库兼容);逻辑外键,无物理 FK;owner + dataScope 复用。**

### chat_conversations(加列)
| 列 | 类型 | 说明 |
|---|---|---|
| `agent_type` | TEXT NOT NULL DEFAULT 'chat' | 场景:`chat`/`coding`/`rpa`/…;现有对话天然为 `chat` |

### chat_messages(加列 + 扩展 role 取值)
| 列 | 类型 | 说明 |
|---|---|---|
| `tool_calls` | TEXT(JSON,可空) | assistant 要求调用的工具列表;null=纯文本回复 |
| `tool_call_id` | TEXT(可空) | role=`tool` 时,对应哪次调用 |
| `name` | TEXT(可空) | 工具名(role=`tool` 时便于展示) |

- `role` 取值扩展为 `user` / `assistant` / `tool`(逻辑约定,不加 DB 枚举约束)。

### agent_runs(新表,可观测性,类比 task_runs)
`id` / `conversation_id` / `owner` / `agent_type` / `started_at` / `finished_at` / `status`(running/completed/failed)/ `prompt_tokens` / `completion_tokens` / `error_message`

### agent_steps(新表,逐步 trace;量大可后置)
`id`(自增)/ `run_id` / `seq` / `kind`(model_call / tool_call / tool_result)/ `payload_json` / `tokens` / `ms` / `ts`

### 会话状态外置(呼应你的建议 #2)
- 状态已全部在 DB(会话 / 消息 / 记忆),符合「外置」。
- Agent 运行时构造**内存** `AgentContext` 传递,**不在 Agent 实例里存状态**:

```rust
pub struct AgentContext<'a> {
    pub conversation_id: &'a str,
    pub owner: &'a str,
    pub db: &'a DatabaseConnection,
    pub llm: &'a dyn LlmProvider,
    pub tools: &'a ToolRegistry,
    // memory 注入复用现有 chat_memory::memory_system_message
}
```
这样阶段三多个 Agent 能共享同一会话上下文。

---

## ③ 编程 Agent 模块设计(样板)

```
CodingAgent = AgentCore(循环引擎) + 编程工具集 + 编程系统提示词
```

### Agent Loop(ReAct,带迭代上限——必须)
```
const MAX_ITERS = 25;
messages = [system_prompt, memory_system_message(owner)?, ...history, user_msg]
for i in 0..MAX_ITERS:
    resp = llm.chat_stream(messages + tools, on_event)   // 流式推前端
    落库 assistant 消息(text + tool_calls);记 agent_step(model_call)
    if resp.tool_calls.is_empty():
        return resp.content                              // 终态:直接回答
    for call in resp.tool_calls:
        result = tools.run(call.name, call.arguments, ctx)
        落库 tool 消息(tool_call_id);记 agent_step(tool_result)
        messages.push(ToolResult{...})
// 超过 MAX_ITERS:强制收尾,提示「达最大步数」
```

### 工具集(编程)
| 工具 | input_schema(要点) | 安全 |
|---|---|---|
| `read_file` | `{ path }` | 路径白名单(限沙箱工作区) |
| `write_file` | `{ path, content }` | 同上 |
| `list_dir` | `{ path }` | 同上 |
| `search_code` | `{ pattern, glob? }` | 只读 |
| `run_command` | `{ cmd }` | **沙箱**:限定工作目录、超时、输出截断、禁危险命令 |

- 沙箱根目录护栏可直接参考现有 `commands::save_text_file`「必须在 app 数据目录之下、禁 `..`」的实现。
- 每个工具 = `ToolDef` + `impl Tool`,注册进 `ToolRegistry`。

### 系统提示词(要点)
约定身份(编程 Agent)、可用工具、行为(先想再用工具、完成即停、报错回读再修)、边界(不越沙箱、敏感命令需确认)。

### RPA Agent(同 Core,复用现有 webview——本项目独有捷径)
- 工具集基于现有 `webview` 池 + 注入脚本能力封装:`open_url` / `click(selector)` / `type(selector, text)` / `screenshot()` / `read_dom()` / `scroll()`。
- **不可逆操作前 `confirm`**:复用采集已有的「检测验证 → 暂停 → 前端提示」机制(`report_collect_verify` 那条链路),在敏感动作前停下来问用户。
- 失败成本高 → 全程操作日志(agent_steps)。

---

## ④ 对话模块:按 agent_type 切换页面布局

对话模块不再一个布局通吃。结构 = **共享外壳 + 按 `conversation.agent_type` 切换的工作区**。

```
ConversationShell(ChatPage 演进而来)
├─ 共享:会话列表侧栏 / 会话头(标题·模型·操作) / 记忆注入 / 输入区(能力按 agent 增减)
├─ <ToolCallCard>:工具调用/结果的统一卡片组件(各布局复用)
└─ <ConversationWorkArea agentType>(布局分发):
   ├─ chat   → 单栏消息流(现有,全宽,保留)
   ├─ coding → IDE 双栏
   └─ rpa    → 对话 + 实时预览 双栏
```

### chat(保留现状)
单栏消息流 + 输入,即现有 ChatPage 行为。

### coding(已选:IDE 双栏)
```
┌─ 对话/步骤(左) ─────┬─ 工作区(右) ──────────────┐
│ user / ai 消息流      │ Tab: [文件树] [Diff] [终端]   │
│ 🔧 工具卡(read/write  │ ┌────────────────────────┐ │
│    /run,内联可展开)  │ │ 当前聚焦的代码 / diff /   │ │
│ [输入框(+附加文件)]  │ │ 命令输出                  │ │
└──────────────────────┴────────────────────────────┘
```
- 右侧工作区是「最新相关产物的聚焦视图」,数据来自消息流里的工具调用:`read_file`→文件内容、`write_file`→diff、`run_command`→终端输出、文件树由会话累计触达文件构成。
- 工具卡仍内联在左侧对话(完整往返可回放),右侧只放「当前在看的那一个」。

### rpa(已选:对话 + 实时预览 双栏)
```
┌─ 对话/步骤(左) ─────┬─ 实时预览(右) ────────────┐
│ user / ai 消息流      │ ┌────────────────────────┐ │
│ 🔧 open_url / click   │ │ 浏览器截图 / 实时画面     │ │
│ ⚠ 敏感操作确认         │ │                        │ │
│   [确认] [取消]        │ └────────────────────────┘ │
│ [输入框]              │ 动作时间线(可回放每步)▸   │
└──────────────────────┴────────────────────────────┘
```
- 右侧实时预览来自 `screenshot`/`read_dom` 工具结果;底部动作时间线 = agent_steps 的可视化。
- **不可逆操作前确认**:确认 UI 出现在左侧对话流(复用采集 `report_collect_verify` 的暂停-确认链路)。

### 前端落地(接口先行,与 schema/agent 同步)
- `ChatPage` 演进为 `ConversationShell`;抽出 `<ChatLayout>`(=现有消息流),新增 `<CodingLayout>` / `<RpaLayout>`;由 `agent_type` 分发(默认 `chat`,行为不变)。
- 抽 `<ToolCallCard>` 与右侧 `<CodeWorkPanel>` / `<RpaPreviewPanel>`,数据均来自消息流里的工具消息(依赖 ② 的工具消息 schema)。
- **依赖项**:本节实现依赖 ②(`agent_type` + 工具消息)与对应 agent 落地;在那之前所有会话都是 `chat`,分发恒走单栏——可先搭外壳 + 占位布局。

---

## 现在做 / 现在不做

**现在(接缝改造,行为基本不变)**
1. `llm/agent/`:落 `LlmProvider` trait + `ChatMsg`/`ToolDef`/`ToolCall` 等结构;`OpenAiCompatibleProvider` 实现 + 旧函数包装;`providers.kind` 字段。
2. schema:`chat_conversations.agent_type`、`chat_messages` 工具列、`agent_runs`(+ 可选 `agent_steps`),全走 ALTER 兼容旧库。
3. 对话仍是 `agent_type=chat` 的无工具 Agent,UI 与行为不变。

**现在不做**
- 不重写对话 UI 成多 Agent 编排台;
- 不做 Router / Orchestrator(阶段三);
- 不在两个垂直场景跑通前抽通用 Agent Loop(会抽错)。

**后续路线**
- 阶段一:对话=场景 #0(已有)+ 编程 Agent + RPA Agent(各自入口跑通,先不进对话)。
- 阶段二:从两个场景提炼 Agent Core(Loop / ToolRegistry / Memory / 安全 / 可观测性)。
- 阶段三:对话入口前加 Router(先「关键词 + 一次 LLM 分类」,再考虑 Orchestrator / Agent-as-Tool)。
