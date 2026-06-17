// 编程 Agent 页面(IDE 双栏):左侧对话/步骤(消息流 + 工具卡),右侧工作区(文件 / 终端)。
// 数据来自会话里的工具消息(assistant.toolCalls 与 role=tool 结果按 toolCallId 关联)。
import { useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { listen } from "@tauri-apps/api/event";
import {
  Box,
  ChevronDown,
  ChevronRight,
  Eye,
  FileCode,
  FolderOpen,
  Loader2,
  Play,
  RotateCw,
  Send,
  Square,
  SquareTerminal,
  Undo2,
  Wrench,
} from "lucide-react";
import { toast } from "sonner";

import {
  api,
  type ChatMessageView,
  type DevServerStatus,
  type SandboxConfigView,
} from "@/lib/api";
import { useChat } from "@/hooks/use-chat";
import { MarkdownMessage } from "@/components/MarkdownMessage";
import { CodeEditor } from "@/components/code-editor";
import { EmptyState } from "@/components/EmptyState";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Textarea } from "@/components/ui/textarea";
import { useSidebar } from "@/components/ui/sidebar";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { cn } from "@/lib/utils";

// 左栏占比低于此值(百分比)视为「过窄」,联动自动收起侧边菜单给左栏腾空间。
// 用占比而非像素:不受窗口 / 侧栏宽度影响,拖到足够窄一定会触发(分割条拖动下限为 28%)
const LEFT_NARROW_PCT = 38;

interface ToolCallJson {
  id: string;
  name: string;
  arguments: Record<string, unknown>;
}

// 从 assistant.toolCalls(JSON 字符串)解析工具调用
function parseToolCalls(json: string | null | undefined): ToolCallJson[] {
  if (!json) return [];
  try {
    const arr = JSON.parse(json) as ToolCallJson[];
    return Array.isArray(arr) ? arr : [];
  } catch {
    return [];
  }
}

// 工具调用的简要参数(单行展示)
function briefArgs(name: string, args: Record<string, unknown>): string {
  if (name === "read_file" || name === "write_file" || name === "list_dir") {
    return String(args.path ?? "");
  }
  if (name === "run_command") {
    return String(args.command ?? "");
  }
  return JSON.stringify(args);
}

// 渲染块:user 气泡 / 最终回答 / 中间「思考过程」步骤(assistant 工具调用 + tool 结果聚成一块,可折叠)
type Block =
  | { type: "user"; m: ChatMessageView }
  | { type: "answer"; m: ChatMessageView }
  | { type: "steps"; key: number; items: ChatMessageView[] };

function buildBlocks(messages: ChatMessageView[]): Block[] {
  const blocks: Block[] = [];
  let steps: ChatMessageView[] = [];
  const flush = () => {
    if (steps.length) {
      blocks.push({ type: "steps", key: steps[0].id, items: steps });
      steps = [];
    }
  };
  for (const m of messages) {
    if (m.role === "user") {
      flush();
      blocks.push({ type: "user", m });
    } else if (m.role === "assistant" && parseToolCalls(m.toolCalls).length === 0) {
      // 无工具调用的 assistant = 最终回答
      flush();
      blocks.push({ type: "answer", m });
    } else {
      // assistant 的工具调用 / tool 结果 → 思考过程步骤
      steps.push(m);
    }
  }
  flush();
  return blocks;
}

export function CodingLayout() {
  const {
    conversations,
    activeId,
    setActiveId,
    providers,
    pendingAgentType,
    pendingFirstMessage,
    setPendingFirstMessage,
    reload,
  } = useChat();
  const active = conversations.find((c) => c.id === activeId) ?? null;
  // 当前是否有待交接的首条消息(读最新值,避免把它加进 load 副作用的依赖里导致重复加载)
  const pendingRef = useRef(pendingFirstMessage);
  pendingRef.current = pendingFirstMessage;

  const [messages, setMessages] = useState<ChatMessageView[]>([]);
  const [input, setInput] = useState("");
  const [sending, setSending] = useState(false);
  const [steps, setSteps] = useState<string[]>([]);
  // Plan / Act 临时态:仅前端局部,不持久化;Plan 只调研出方案,Act 亲自动手执行。默认 act。
  const [mode, setMode] = useState<"plan" | "act">("act");
  // 可用模型由共享 providers 派生(避免重挂载重拉导致竞态)
  const models = useMemo(() => {
    const opts: { providerId: string; model: string }[] = [];
    for (const p of providers) {
      if (!p.apiKey.trim()) continue;
      for (const line of p.models.split("\n")) {
        const m = line.trim();
        if (m) opts.push({ providerId: p.id, model: m });
      }
    }
    return opts;
  }, [providers]);
  const [workspace, setWorkspace] = useState("");
  const [workTab, setWorkTab] = useState<"preview" | "files" | "terminal">(
    "preview",
  );
  const [selectedFile, setSelectedFile] = useState<string | null>(null);
  // 文件面板:真实工作区文件树(列表 + 选中内容),回退/发送后刷新;替代原先从消息派生
  const [wsFiles, setWsFiles] = useState<string[]>([]);
  const [fileContent, setFileContent] = useState("");
  const [fileRefresh, setFileRefresh] = useState(0);
  // 预览刷新信号:文件保存后 +1,通知 PreviewServer 重新加载 iframe(实时反映编译结果)
  const [previewReload, setPreviewReload] = useState(0);
  // 用户在终端直接执行的命令(与 Agent 跑过的命令合并展示)
  const [userRuns, setUserRuns] = useState<{ command: string; output: string }[]>([]);
  const [termInput, setTermInput] = useState("");
  const [running, setRunning] = useState(false);
  // 已展开的「思考过程」步骤块(按块首条消息 id);默认折叠
  const [expandedSteps, setExpandedSteps] = useState<Set<number>>(new Set());
  const toggleSteps = (key: number) =>
    setExpandedSteps((prev) => {
      const next = new Set(prev);
      if (next.has(key)) next.delete(key);
      else next.add(key);
      return next;
    });
  const [sandboxOpen, setSandboxOpen] = useState(false);
  // 沙盒可用性(用于头部「未隔离」提示);null=未知
  const [dockerOk, setDockerOk] = useState<boolean | null>(null);
  useEffect(() => {
    api
      .getSandboxConfig()
      .then((c) => setDockerOk(c.dockerAvailable))
      .catch(() => {});
  }, [sandboxOpen]);
  const scrollRef = useRef<HTMLDivElement>(null);
  // 当前发送归属会话(过滤 agent-step 事件)
  const sendingConvRef = useRef<string | null>(null);
  // 发送重入锁:state 更新异步,挡住极快连点/交接与手动发送撞车造成的重复发送
  const dispatchingRef = useRef(false);
  // 左右栏宽度(左栏百分比)+ 拖动态,中间分割条可拖动调整
  const [leftPct, setLeftPct] = useState(46);
  const [dragging, setDragging] = useState(false);
  const splitRef = useRef<HTMLDivElement>(null);
  // 左栏过窄时联动收起侧边菜单(腾地方),拖回变宽再展开;仅管理「自动收起」的情形,
  // 用户手动收起后不再自动展开(autoCollapsedRef 标记本次收起是否由自动逻辑触发)。
  // open / setOpen 走 ref 读写:它们的引用会随侧栏开合变化,放进 effect 依赖会因联动自身触发而抖动(收了又立刻展开)
  const { open: sidebarOpen, setOpen: setSidebarOpen } = useSidebar();
  const sidebarOpenRef = useRef(sidebarOpen);
  sidebarOpenRef.current = sidebarOpen;
  const setSidebarOpenRef = useRef(setSidebarOpen);
  setSidebarOpenRef.current = setSidebarOpen;
  const autoCollapsedRef = useRef(false);
  const wasLeftNarrowRef = useRef(false);

  // 工作区路径展示(按当前会话取其专属目录)
  useEffect(() => {
    api
      .getCodingWorkspace(activeId ?? undefined)
      .then(setWorkspace)
      .catch(() => {});
  }, [activeId]);

  // 切换会话时加载消息;若有待交接的首条消息(此刻 DB 尚空),跳过加载,交给发送流程
  useEffect(() => {
    if (!activeId) {
      setMessages([]);
      return;
    }
    if (pendingRef.current) return;
    api
      .listChatMessages(activeId)
      .then(setMessages)
      .catch((e) => toast.error(`加载消息失败: ${e}`));
  }, [activeId]);

  // 切会话时重置仅属于上一会话的组件级 state——CodingLayout 在 ConversationShell 中常驻不卸载,
  // 不重置会让上个会话的终端记录 / 输入草稿 / 思考步骤展开态泄漏到新会话。
  useEffect(() => {
    setUserRuns([]);
    setInput("");
    setSteps([]);
    setExpandedSteps(new Set<number>());
    setTermInput("");
    setSelectedFile(null);
  }, [activeId]);

  // 交接的首条消息:建好 coding 会话后自动发送一次
  useEffect(() => {
    if (pendingFirstMessage && activeId) {
      const msg = pendingFirstMessage;
      setPendingFirstMessage(null);
      void doSend(msg);
    }
    // 仅在交接消息/会话变化时触发
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [pendingFirstMessage, activeId]);

  // 监听 Agent 进度事件(逐步标签),仅取当前发送会话
  useEffect(() => {
    let dispose: (() => void) | undefined;
    let disposed = false;
    listen<{ conversationId: string; label: string }>("agent-step", (e) => {
      if (sendingConvRef.current !== e.payload.conversationId) return;
      setSteps((prev) => [...prev, e.payload.label]);
    }).then(
      (fn) => {
        if (disposed) fn();
        else dispose = fn;
      },
      () => {},
    );
    return () => {
      disposed = true;
      dispose?.();
    };
  }, []);

  // 消息/步骤变化滚到底
  useEffect(() => {
    const el = scrollRef.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, [messages, steps, sending]);

  // 分割条拖动:全程在 document 上监听 move/up(鼠标移到 iframe 上也不丢事件,配合容器 select-none + iframe 禁指针)
  useEffect(() => {
    if (!dragging) return;
    const onMove = (e: MouseEvent) => {
      const box = splitRef.current;
      if (!box) return;
      const rect = box.getBoundingClientRect();
      const pct = ((e.clientX - rect.left) / rect.width) * 100;
      setLeftPct(Math.min(72, Math.max(28, pct))); // 限幅,避免某栏被拖没
    };
    const onUp = () => setDragging(false);
    document.addEventListener("mousemove", onMove);
    document.addEventListener("mouseup", onUp);
    return () => {
      document.removeEventListener("mousemove", onMove);
      document.removeEventListener("mouseup", onUp);
    };
  }, [dragging]);

  // 左栏占比跨过阈值时联动侧边菜单(按占比判定,与窗口 / 侧栏宽度无关,拖窄一定会触发):
  // 进入「过窄」→ 菜单仍展开则自动收起;离开「过窄」→ 仅当之前是自动收起的才自动展开。
  // 只在跨越阈值的瞬间动作(非持续强制);setOpen / open 走 ref,联动自身不会再触发本 effect → 不抖动。
  useEffect(() => {
    const narrow = leftPct < LEFT_NARROW_PCT;
    if (narrow && !wasLeftNarrowRef.current) {
      if (sidebarOpenRef.current) {
        setSidebarOpenRef.current(false);
        autoCollapsedRef.current = true;
      }
    } else if (!narrow && wasLeftNarrowRef.current) {
      if (autoCollapsedRef.current) {
        setSidebarOpenRef.current(true);
        autoCollapsedRef.current = false;
      }
    }
    wasLeftNarrowRef.current = narrow;
  }, [leftPct]);

  // 右侧终端记录:从消息流的 run_command 工具往返派生
  const terminal = useMemo(() => {
    const callsById: Record<string, { name: string; args: Record<string, unknown> }> = {};
    const list: { command: string; output: string }[] = [];
    for (const m of messages) {
      if (m.role === "assistant" && m.toolCalls) {
        for (const c of parseToolCalls(m.toolCalls)) {
          callsById[c.id] = { name: c.name, args: c.arguments ?? {} };
        }
      } else if (m.role === "tool" && m.toolCallId) {
        const call = callsById[m.toolCallId];
        if (call && call.name === "run_command") {
          list.push({ command: String(call.args.command ?? ""), output: m.content });
        }
      }
    }
    return list;
  }, [messages]);
  // 消息分块:user / 最终回答 / 思考过程步骤(用于可折叠展示)
  const blocks = useMemo(() => buildBlocks(messages), [messages]);
  // 终端显示 = Agent 跑过的命令 + 用户直接敲的命令
  const combinedTerminal = [...terminal, ...userRuns];

  // 当前展示文件:优先用户选中(仍在列表里),否则列表第一个
  const shownFile =
    selectedFile && wsFiles.includes(selectedFile) ? selectedFile : wsFiles[0] ?? null;

  // 拉真实文件列表:进入文件 tab / 切会话 / 发送结束 / 回退 / 手动刷新 时
  useEffect(() => {
    if (workTab !== "files" || !activeId) return;
    let alive = true;
    api
      .listWorkspaceFiles(activeId)
      .then((fs) => {
        if (alive) setWsFiles(fs);
      })
      .catch(() => {});
    return () => {
      alive = false;
    };
  }, [workTab, activeId, fileRefresh, sending]);

  // 拉当前选中文件内容
  useEffect(() => {
    if (!activeId || !shownFile) {
      setFileContent("");
      return;
    }
    let alive = true;
    api
      .readWorkspaceFile(activeId, shownFile)
      .then((c) => {
        if (alive) setFileContent(c);
      })
      .catch((e) => {
        if (alive) setFileContent(`读取失败: ${e}`);
      });
    return () => {
      alive = false;
    };
  }, [activeId, shownFile, fileRefresh]);

  // 用户在终端直接执行一条命令(工作区内)
  async function runUserCmd() {
    const c = termInput.trim();
    if (!c || running) return;
    // 未建会话(activeId 为空)时拦截:否则后端 safe_id("") 会退化到 "default" 目录、在无关工作区执行
    if (!activeId) {
      toast.error("请先发送一条消息创建会话,再使用终端");
      return;
    }
    setRunning(true);
    try {
      const out = await api.runWorkspaceCommand(activeId, c);
      setUserRuns((prev) => [...prev, { command: c, output: out }]);
      setTermInput("");
    } catch (e) {
      setUserRuns((prev) => [...prev, { command: c, output: `执行失败: ${e}` }]);
    } finally {
      setRunning(false);
    }
  }

  // 发送一条消息驱动编程 Agent;text 来自输入框或交接的首条消息。
  // sendMode 缺省 act:交接来的首条消息默认走 act(动手执行),手动发送由调用方传当前段控值。
  async function doSend(text: string, sendMode: "plan" | "act" = "act") {
    if (dispatchingRef.current || !text || sending) return;
    // 仅「在本页新建会话」时才要求已配模型;交接来的会话(activeId 已存在)已绑定模型,
    // 此时本页的 models 可能尚未加载完,不能据此误报「尚无可用模型」
    if (!activeId && models.length === 0) {
      toast.error("尚无可用模型,请先到系统配置 → 模型厂商填好 API Key 与模型");
      return;
    }
    dispatchingRef.current = true;
    setSending(true);
    setSteps([]);
    setInput("");
    // 乐观追加用户消息
    const optimistic: ChatMessageView = {
      id: Date.now(),
      conversationId: activeId ?? "",
      role: "user",
      content: text,
      createdAt: Math.floor(Date.now() / 1000),
    };
    setMessages((prev) => [...prev, optimistic]);
    try {
      let convId = activeId;
      if (!convId) {
        const opt = models[0];
        const conv = await api.createConversation(
          crypto.randomUUID(),
          opt.providerId,
          opt.model,
          "coding",
        );
        convId = conv.id;
        setActiveId(conv.id);
      }
      sendingConvRef.current = convId;
      await api.sendCodingMessage(convId, text, sendMode);
      // 重载完整线程(含工具往返)
      const fresh = await api.listChatMessages(convId);
      setMessages(fresh);
      await reload();
    } catch (e) {
      toast.error(`执行失败: ${e}`);
      setMessages((prev) => prev.filter((m) => m.id !== optimistic.id));
      setInput(text);
    } finally {
      sendingConvRef.current = null;
      setSending(false);
      setSteps([]);
      dispatchingRef.current = false;
    }
  }

  function handleSend() {
    void doSend(input.trim(), mode);
  }

  // 当前会话的分步计划(Plan 产出 / Act 勾选);从 active.planTodos(JSON)解析
  const planTodos = useMemo<{ title: string; done?: boolean }[]>(() => {
    const raw = active?.planTodos?.trim();
    if (!raw) return [];
    try {
      const arr = JSON.parse(raw);
      return Array.isArray(arr) ? arr : [];
    } catch {
      return [];
    }
  }, [active?.planTodos]);
  const planDone = planTodos.filter((t) => t.done).length;

  // 「按方案执行」:切到 Act 并让 Agent 按已产出的计划逐步落地
  function handleRunPlan() {
    if (!activeId || sending) return;
    setMode("act");
    void doSend(
      "请按上面的【任务计划】逐步执行,每完成一步用 update_plan 勾选进度。",
      "act",
    );
  }

  // 回退:确认后丢弃本轮 Agent 的文件改动,回到最近检查点(发送前状态)
  function handleRollback() {
    const id = activeId;
    if (!id) return;
    toast("回退到本轮发送前?将丢弃本轮 Agent 的文件改动(历史记录保留)", {
      action: {
        label: "回退",
        onClick: () => {
          void (async () => {
            try {
              toast.success(await api.checkpointRollback(id));
              setFileRefresh((k) => k + 1); // 回退后刷新文件面板,反映真实文件状态
            } catch (e) {
              toast.error(`回退失败: ${e}`);
            }
          })();
        },
      },
    });
  }

  // 文件面板保存:写回真实文件 → 重读(dirty 归零)+ 触发预览实时刷新/重编译
  async function saveFile(path: string, content: string) {
    if (!activeId) return;
    try {
      await api.writeWorkspaceFile(activeId, path, content);
      toast.success("已保存");
      setFileRefresh((k) => k + 1);
      setPreviewReload((k) => k + 1);
    } catch (e) {
      toast.error(`保存失败: ${e}`);
    }
  }

  const isNew = !active && pendingAgentType === "coding";

  return (
    <div className="flex min-h-0 min-w-0 flex-1 flex-col overflow-hidden">
      {/* 头:标题 + 工作区路径 */}
      <div className="flex shrink-0 items-center justify-between gap-2 border-b px-4 py-2">
        <div className="flex items-center gap-2 text-sm font-medium text-foreground">
          <FileCode className="size-4 text-primary" />
          {active?.title || "新编程会话"}
        </div>
        <div className="flex min-w-0 items-center gap-2">
          <div className="flex min-w-0 items-center gap-1.5 truncate text-xs text-muted-foreground">
            <FolderOpen className="size-3.5 shrink-0" />
            <span className="truncate" title={workspace}>
              {workspace || "工作区加载中…"}
            </span>
          </div>
          {dockerOk === false && (
            <span className="shrink-0 rounded bg-amber-500/15 px-1.5 py-0.5 text-[10px] text-amber-600 dark:text-amber-400">
              未隔离·本机
            </span>
          )}
          <Button
            variant="ghost"
            size="sm"
            className="h-7 shrink-0 gap-1 text-xs"
            disabled={!activeId}
            title="回退到本轮发送前(丢弃本轮文件改动)"
            onClick={handleRollback}
          >
            <Undo2 className="size-3.5" />
            回退
          </Button>
          <Button
            variant="ghost"
            size="sm"
            className="h-7 shrink-0 gap-1 text-xs"
            onClick={() => setSandboxOpen(true)}
          >
            <Box className="size-3.5" />
            沙盒
          </Button>
        </div>
      </div>
      <SandboxDialog open={sandboxOpen} onOpenChange={setSandboxOpen} />

      {/* 双栏(中间分割条可拖动调整宽度) */}
      <div
        ref={splitRef}
        className={cn(
          "flex min-h-0 flex-1",
          dragging && "select-none [&_iframe]:pointer-events-none",
        )}
      >
        {/* 左:对话 / 步骤 */}
        <div
          className="flex min-h-0 min-w-0 shrink-0 flex-col"
          style={{ width: `${leftPct}%` }}
        >
          {/* 任务计划:Plan 产出的 todo + 进度;Plan 模式下还有未完成项时给「按方案执行」 */}
          {planTodos.length > 0 && (
            <div className="shrink-0 border-b px-4 py-2">
              <div className="mb-1 flex items-center justify-between gap-2 text-xs font-medium text-foreground">
                <span>
                  任务计划 {planDone}/{planTodos.length}
                </span>
                {mode === "plan" && planDone < planTodos.length && (
                  <Button
                    size="sm"
                    variant="default"
                    className="h-6 shrink-0 gap-1 px-2 text-[11px]"
                    disabled={sending}
                    onClick={handleRunPlan}
                  >
                    <Play className="size-3" />
                    按方案执行
                  </Button>
                )}
              </div>
              <div className="veltrix-thin-scrollbar max-h-40 space-y-0.5 overflow-y-auto">
                {planTodos.map((t, i) => (
                  <div
                    key={i}
                    className="flex items-start gap-1.5 text-xs text-muted-foreground"
                  >
                    <span className={cn("mt-px shrink-0", t.done && "text-emerald-500")}>
                      {t.done ? "☑" : "☐"}
                    </span>
                    <span className={cn(t.done && "line-through opacity-60")}>
                      {t.title}
                    </span>
                  </div>
                ))}
              </div>
            </div>
          )}
          <div
            ref={scrollRef}
            className="veltrix-thin-scrollbar min-h-0 flex-1 space-y-2.5 overflow-y-auto px-5 py-3"
          >
            {messages.length === 0 && !sending ? (
              models.length === 0 ? (
                <EmptyState
                  icon={Wrench}
                  title="尚未配置模型"
                  description="请到系统配置 → 模型厂商,填好 API Key 与模型后再开始。"
                />
              ) : (
                <EmptyState
                  icon={FileCode}
                  title="编程 Agent"
                  description="描述你的编程任务,我会在工作区内读写文件、执行命令并自我验证。"
                />
              )
            ) : (
              blocks.map((b) =>
                b.type === "steps" ? (
                  <StepsBlock
                    key={b.key}
                    items={b.items}
                    expanded={expandedSteps.has(b.key)}
                    onToggle={() => toggleSteps(b.key)}
                  />
                ) : (
                  <CodingMessage key={b.m.id} message={b.m} />
                ),
              )
            )}
            {/* 进行中:实时思考过程(逐步累计,可滚动) */}
            {sending && (
              <div className="rounded-md border bg-muted/20 p-2 text-xs text-muted-foreground">
                <div className="flex items-center gap-2">
                  <Loader2 className="size-3.5 animate-spin" />
                  <span className="font-medium text-foreground">思考过程</span>
                </div>
                <div className="veltrix-thin-scrollbar mt-1.5 max-h-32 space-y-0.5 overflow-auto">
                  {steps.length > 0 ? (
                    steps.map((s, i) => <div key={i}>{s}</div>)
                  ) : (
                    <div>思考中…</div>
                  )}
                </div>
              </div>
            )}
          </div>
          {/* 输入:与对话输入框风格一致(圆角卡片 + 无边框 textarea + 发送) */}
          <div className="shrink-0 px-5 pb-2">
            <div className="flex flex-col gap-1 rounded-2xl border bg-card p-2 shadow-lg">
              <Textarea
                value={input}
                onChange={(e) => setInput(e.target.value)}
                onKeyDown={(e) => {
                  if (e.key === "Enter" && !e.shiftKey) {
                    e.preventDefault();
                    if (!sending) void handleSend();
                  }
                }}
                placeholder={
                  isNew
                    ? "描述编程任务,Enter 发送 / Shift+Enter 换行"
                    : "继续指示,Enter 发送 / Shift+Enter 换行"
                }
                className="veltrix-thin-scrollbar max-h-52 min-h-10 w-full resize-none border-0 bg-transparent px-2 py-2 text-[15px] leading-6 shadow-none focus-visible:ring-0 dark:bg-transparent"
                rows={1}
              />
              <div className="flex items-center justify-between gap-2">
                {/* Plan / Act 段控:Plan 只产出实现方案,Act 亲自动手执行(临时态,不持久化) */}
                <div className="flex shrink-0 items-center rounded-lg border bg-muted/40 p-0.5 text-xs">
                  <button
                    type="button"
                    onClick={() => setMode("plan")}
                    disabled={sending}
                    className={cn(
                      "rounded-md px-2.5 py-1 font-medium transition-colors",
                      mode === "plan"
                        ? "bg-background text-foreground shadow-sm"
                        : "text-muted-foreground hover:text-foreground",
                    )}
                    title="方案模式:只调研并产出分步实现方案,不改动 / 不运行"
                  >
                    Plan
                  </button>
                  <button
                    type="button"
                    onClick={() => setMode("act")}
                    disabled={sending}
                    className={cn(
                      "rounded-md px-2.5 py-1 font-medium transition-colors",
                      mode === "act"
                        ? "bg-background text-foreground shadow-sm"
                        : "text-muted-foreground hover:text-foreground",
                    )}
                    title="执行模式:在工作区内读写文件、运行命令并自我验证"
                  >
                    Act
                  </button>
                </div>
                <Button
                  size="icon"
                  className="size-9 shrink-0 cursor-pointer rounded-xl"
                  disabled={sending || !input.trim()}
                  onClick={() => void handleSend()}
                >
                  {sending ? <Loader2 className="animate-spin" /> : <Send />}
                </Button>
              </div>
            </div>
          </div>
        </div>

        {/* 中间分割条:按住向左右拖动调整两栏宽度 */}
        <div
          role="separator"
          aria-orientation="vertical"
          onMouseDown={() => setDragging(true)}
          className={cn(
            "w-1 shrink-0 cursor-col-resize bg-border transition-colors hover:bg-primary/40",
            dragging && "bg-primary/50",
          )}
        />

        {/* 右:工作区(文件 / 终端);底部留一点间距,内容不贴边 */}
        <div className="flex min-h-0 min-w-0 flex-1 flex-col pb-2">
          <div className="flex shrink-0 items-center gap-1 border-b px-2 py-1.5">
            <WorkTab active={workTab === "preview"} onClick={() => setWorkTab("preview")}>
              <Eye className="size-3.5" />
              预览
            </WorkTab>
            <WorkTab active={workTab === "files"} onClick={() => setWorkTab("files")}>
              <FileCode className="size-3.5" />
              文件{wsFiles.length > 0 ? ` (${wsFiles.length})` : ""}
            </WorkTab>
            <WorkTab active={workTab === "terminal"} onClick={() => setWorkTab("terminal")}>
              <SquareTerminal className="size-3.5" />
              终端{terminal.length > 0 ? ` (${terminal.length})` : ""}
            </WorkTab>
          </div>
          <div className="flex min-h-0 flex-1 flex-col overflow-hidden">
            {workTab === "files" ? (
              <div className="flex min-h-0 flex-1 flex-col">
                <div className="flex shrink-0 items-center justify-between border-b px-2 py-1 text-[11px] text-muted-foreground">
                  <span>工作区文件{wsFiles.length > 0 ? `(${wsFiles.length})` : ""}</span>
                  <Button
                    type="button"
                    size="icon"
                    variant="ghost"
                    className="size-6"
                    title="刷新文件列表"
                    onClick={() => setFileRefresh((k) => k + 1)}
                  >
                    <RotateCw className="size-3" />
                  </Button>
                </div>
                {wsFiles.length === 0 ? (
                  <div className="flex flex-1 items-center justify-center px-6 text-center text-xs text-muted-foreground">
                    工作区暂无文件;Agent 写文件后会自动出现(也可点右上角刷新)
                  </div>
                ) : (
                  <div className="flex min-h-0 flex-1">
                    <div className="veltrix-thin-scrollbar w-44 shrink-0 overflow-y-auto border-r p-1.5">
                      {wsFiles.map((f) => (
                        <button
                          key={f}
                          type="button"
                          onClick={() => setSelectedFile(f)}
                          className={cn(
                            "block w-full truncate rounded px-2 py-1 text-left text-xs transition-colors",
                            shownFile === f
                              ? "bg-primary/10 font-medium text-primary"
                              : "text-foreground hover:bg-accent/50",
                          )}
                          title={f}
                        >
                          {f}
                        </button>
                      ))}
                    </div>
                    <div className="flex min-h-0 min-w-0 flex-1 flex-col overflow-hidden">
                      {shownFile && (
                        <CodeEditor
                          key={shownFile}
                          path={shownFile}
                          value={fileContent}
                          onSave={(content) => saveFile(shownFile, content)}
                        />
                      )}
                    </div>
                  </div>
                )}
              </div>
            ) : workTab === "preview" ? (
              <PreviewServer reloadSignal={previewReload} />
            ) : (
              <div className="flex min-h-0 flex-1 flex-col">
                <div className="veltrix-thin-scrollbar min-h-0 flex-1 space-y-3 overflow-y-auto p-3">
                  {combinedTerminal.length === 0 ? (
                    <div className="py-8 text-center text-xs text-muted-foreground">
                      Agent 执行的命令会显示在这里;也可在下方直接输入命令执行(工作区内)
                    </div>
                  ) : (
                    combinedTerminal.map((t, i) => (
                      <div key={i} className="rounded-md border bg-muted/30">
                        <div className="border-b px-2 py-1 font-mono text-xs text-primary">
                          $ {t.command}
                        </div>
                        <pre className="whitespace-pre-wrap break-words p-2 font-mono text-[11px] text-muted-foreground">
                          {t.output}
                        </pre>
                      </div>
                    ))
                  )}
                  {running && (
                    <div className="flex items-center gap-2 text-xs text-muted-foreground">
                      <Loader2 className="size-3.5 animate-spin" />
                      执行中…
                    </div>
                  )}
                </div>
                {/* 直接操作终端:输入命令 Enter 执行 */}
                <form
                  onSubmit={(e) => {
                    e.preventDefault();
                    void runUserCmd();
                  }}
                  className="flex shrink-0 items-center gap-1.5 border-t px-2 py-1.5"
                >
                  <span className="font-mono text-xs text-primary">$</span>
                  <input
                    value={termInput}
                    onChange={(e) => setTermInput(e.target.value)}
                    placeholder={activeId ? "输入命令,Enter 执行(在工作区内)" : "发送一条消息创建会话后可用"}
                    disabled={running || !activeId}
                    className="min-w-0 flex-1 bg-transparent font-mono text-xs text-foreground outline-none placeholder:text-muted-foreground"
                  />
                </form>
              </div>
            )}
          </div>
        </div>
      </div>
    </div>
  );
}

function WorkTab({
  active,
  onClick,
  children,
}: {
  active: boolean;
  onClick: () => void;
  children: ReactNode;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      className={cn(
        "flex items-center gap-1 rounded-md px-2.5 py-1 text-xs font-medium transition-colors",
        active
          ? "bg-accent text-accent-foreground"
          : "text-muted-foreground hover:bg-accent/50",
      )}
    >
      {children}
    </button>
  );
}

// 预览统一走「预览服务」:在沙盒内常驻起一个服务进程并经 <名>.localhost:<port> 给出访问地址。
// 不再区分静态 / 开发服务器——前端项目用 `npm run dev`,单个 HTML / 纯静态目录用内置静态服务器,
// 二者都得到一个真实访问地址(单 HTML 不再用本地 file 协议直开)。轮询状态,检测到端口后 iframe 接入。
function PreviewServer({ reloadSignal }: { reloadSignal?: number }) {
  const { activeId } = useChat();
  // 默认前端项目(Vite)预览;容器内服务须绑 0.0.0.0(--host),宿主已发布同号端口,经 localhost:<port> 访问。
  // 加 CHOKIDAR_USEPOLLING:Docker(尤其 Win/WSL2)bind mount 的文件变更事件常不传播,开轮询让 Vite 感知保存并热更新。
  const DEV_CMD = "CHOKIDAR_USEPOLLING=true npm run dev -- --host";
  const [command, setCommand] = useState(DEV_CMD);
  const [status, setStatus] = useState<DevServerStatus | null>(null);
  const [busy, setBusy] = useState(false);
  const [iframeKey, setIframeKey] = useState(0);
  const [showLogs, setShowLogs] = useState(false);

  // 轮询 dev server 状态(端口 / 运行 / 日志)
  useEffect(() => {
    let alive = true;
    const tick = async () => {
      try {
        const s = await api.getDevServerStatus();
        if (alive) setStatus(s);
      } catch {
        // 忽略
      }
    };
    void tick();
    const timer = setInterval(() => void tick(), 1500);
    return () => {
      alive = false;
      clearInterval(timer);
    };
  }, []);

  // 外部信号(文件保存等)→ 重新加载预览 iframe,实时反映保存后的编译结果
  useEffect(() => {
    if (reloadSignal) setIframeKey((k) => k + 1);
  }, [reloadSignal]);

  // dev server 是全局单实例;仅当其归属当前会话时才认它,否则切到别的会话不串台(防显示他会话内容)
  const belongsToThis =
    !status?.conversationId || status.conversationId === activeId;
  const running = (status?.running ?? false) && belongsToThis;
  const port = belongsToThis ? (status?.port ?? null) : null;
  // 直连本机回环:Vite 新版会按 Host 头做 allowedHosts 校验,非 localhost/127.0.0.1 的子域会被拒
  // (返回 "Blocked request"),而 <slug>.localhost 并无反代按子域路由,纯属多余,故统一用 localhost。
  const host = "localhost";
  const src = port ? `http://${host}:${port}/?_=${iframeKey}` : "";

  async function start() {
    setBusy(true);
    try {
      await api.startDevServer(activeId ?? "", command);
      setIframeKey((k) => k + 1);
    } catch (e) {
      toast.error(`启动失败: ${e}`);
    } finally {
      setBusy(false);
    }
  }
  async function stop() {
    setBusy(true);
    try {
      await api.stopDevServer();
    } catch (e) {
      toast.error(`停止失败: ${e}`);
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="flex h-full min-h-0 flex-col">
      {/* 预设:一键填入前端项目(Vite)启动命令 */}
      <div className="flex shrink-0 items-center gap-1.5 border-b px-2 py-1 text-[11px]">
        <span className="text-muted-foreground">预设</span>
        <button
          type="button"
          onClick={() => setCommand(DEV_CMD)}
          className={cn(
            "rounded px-2 py-0.5 transition-colors",
            command === DEV_CMD
              ? "bg-primary/10 font-medium text-primary"
              : "text-muted-foreground hover:bg-accent/50",
          )}
        >
          前端项目(Vite)
        </button>
      </div>
      <div className="flex shrink-0 items-center gap-1.5 border-b px-2 py-1.5">
        <input
          value={command}
          onChange={(e) => setCommand(e.target.value)}
          placeholder="服务命令(在工作区 / 沙盒内常驻运行)"
          className="min-w-0 flex-1 rounded border bg-transparent px-2 py-1 font-mono text-xs outline-none"
        />
        {running ? (
          <Button
            type="button"
            size="sm"
            variant="outline"
            className="h-7 shrink-0 gap-1"
            disabled={busy}
            onClick={() => void stop()}
          >
            <Square className="size-3.5" />
            停止
          </Button>
        ) : (
          <Button
            type="button"
            size="sm"
            className="h-7 shrink-0 gap-1"
            disabled={busy}
            onClick={() => void start()}
          >
            <Play className="size-3.5" />
            启动
          </Button>
        )}
        <Button
          type="button"
          size="icon"
          variant="ghost"
          className="size-7 shrink-0"
          title="刷新预览"
          onClick={() => setIframeKey((k) => k + 1)}
        >
          <RotateCw className="size-3.5" />
        </Button>
        <Button
          type="button"
          size="sm"
          variant="ghost"
          className="h-7 shrink-0"
          onClick={() => setShowLogs((v) => !v)}
        >
          日志
        </Button>
      </div>
      <div className="flex shrink-0 items-center gap-1.5 px-2 py-1 text-[11px] text-muted-foreground">
        <span
          className={cn(
            "size-2 rounded-full",
            running ? "bg-emerald-500" : "bg-muted-foreground/40",
          )}
        />
        {running
          ? port
            ? `运行中 · ${host}:${port}`
            : "运行中 · 正在探测端口…"
          : "未运行"}
      </div>
      <div className="min-h-0 flex-1">
        {port ? (
          <iframe
            key={iframeKey}
            src={src}
            title="预览"
            className="size-full border-0 bg-white"
          />
        ) : (
          <div className="flex h-full items-center justify-center px-6 text-center text-xs text-muted-foreground">
            {running
              ? "已启动,正在探测访问端口…"
              : "点「启动」运行前端项目(Vite),获得预览访问地址"}
          </div>
        )}
      </div>
      {showLogs && (
        <pre className="veltrix-thin-scrollbar h-28 shrink-0 overflow-auto border-t bg-muted/30 p-2 font-mono text-[11px] text-muted-foreground">
          {(status?.logs ?? []).join("\n") || "(暂无日志)"}
        </pre>
      )}
    </div>
  );
}

// 思考过程步骤块(可折叠):折叠时显示「思考过程 · N 步(工具名)」,展开显示每步的
// 推理文字 / 工具调用(名+参数)/ 工具结果(可滚动)。
function StepsBlock({
  items,
  expanded,
  onToggle,
}: {
  items: ChatMessageView[];
  expanded: boolean;
  onToggle: () => void;
}) {
  const calls = items.flatMap((m) =>
    m.role === "assistant" ? parseToolCalls(m.toolCalls) : [],
  );
  const names = Array.from(new Set(calls.map((c) => c.name)));

  return (
    <div className="overflow-hidden rounded-md border bg-muted/20">
      <button
        type="button"
        onClick={onToggle}
        className="flex w-full items-center gap-1.5 px-2.5 py-1.5 text-left text-xs text-muted-foreground transition-colors hover:bg-accent/40"
      >
        {expanded ? (
          <ChevronDown className="size-3.5 shrink-0" />
        ) : (
          <ChevronRight className="size-3.5 shrink-0" />
        )}
        <Wrench className="size-3.5 shrink-0 text-primary" />
        <span className="font-medium text-foreground">思考过程</span>
        <span className="shrink-0">· {calls.length} 步</span>
        {names.length > 0 && (
          <span className="truncate">({names.join(", ")})</span>
        )}
      </button>
      {expanded && (
        <div className="space-y-2 border-t px-2.5 py-2">
          {items.map((m) =>
            m.role === "assistant" ? (
              <div key={m.id} className="space-y-1">
                {m.content.trim() && (
                  <p className="whitespace-pre-wrap break-words text-xs text-muted-foreground">
                    {m.content}
                  </p>
                )}
                {parseToolCalls(m.toolCalls).map((c) => (
                  <div key={c.id} className="flex items-center gap-1.5 text-xs">
                    <Wrench className="size-3 shrink-0 text-primary" />
                    <span className="font-medium text-foreground">{c.name}</span>
                    <span
                      className="truncate text-muted-foreground"
                      title={briefArgs(c.name, c.arguments ?? {})}
                    >
                      {briefArgs(c.name, c.arguments ?? {})}
                    </span>
                  </div>
                ))}
              </div>
            ) : (
              <div key={m.id} className="text-xs">
                <div className="mb-0.5 flex items-center gap-1 text-muted-foreground">
                  <span className="text-emerald-500">↳</span>
                  <span className="font-medium">{m.toolName || "tool"}</span>
                </div>
                <pre className="veltrix-thin-scrollbar max-h-40 overflow-auto whitespace-pre-wrap break-words rounded bg-background/60 p-1.5 font-mono text-[11px] text-muted-foreground">
                  {m.content}
                </pre>
              </div>
            ),
          )}
        </div>
      )}
    </div>
  );
}

// 编程沙盒设置:本机 / Docker(每会话共享容器内独立目录);镜像/容器名 + 启停 + 状态。
function SandboxDialog({
  open,
  onOpenChange,
}: {
  open: boolean;
  onOpenChange: (o: boolean) => void;
}) {
  const [cfg, setCfg] = useState<SandboxConfigView | null>(null);
  const [image, setImage] = useState("node:20-bookworm");
  const [container, setContainer] = useState("veltrix-sandbox");
  const [busy, setBusy] = useState(false);

  async function refresh() {
    try {
      const c = await api.getSandboxConfig();
      setCfg(c);
      setImage(c.image);
      setContainer(c.container);
    } catch {
      // 忽略
    }
  }
  useEffect(() => {
    if (open) void refresh();
  }, [open]);

  async function withBusy(fn: () => Promise<void>) {
    setBusy(true);
    try {
      await fn();
    } finally {
      setBusy(false);
    }
  }
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="sm:max-w-lg">
        <DialogHeader>
          <DialogTitle>运行沙盒(Docker)</DialogTitle>
          <DialogDescription>
            命令默认在 Docker 沙盒里跑:每个会话在共享容器内的独立目录,不污染本机,退出时自动停止(文件保留),跨 Win/Mac/Linux。Docker 未安装/未运行时自动回退本机执行(未隔离)。
          </DialogDescription>
        </DialogHeader>
        <div className="space-y-3">
          {!cfg?.dockerAvailable && (
            <div className="rounded-md border border-amber-500/40 bg-amber-500/10 px-2.5 py-2 text-xs text-amber-600 dark:text-amber-400">
              未检测到 Docker:命令将临时在本机执行(未隔离)。安装并启动 Docker 后即自动使用沙盒。
            </div>
          )}
          <div className="grid gap-2 sm:grid-cols-2">
            <label className="space-y-1">
              <span className="text-xs text-muted-foreground">基础镜像</span>
              <Input value={image} onChange={(e) => setImage(e.target.value)} placeholder="node:20-bookworm" />
            </label>
            <label className="space-y-1">
              <span className="text-xs text-muted-foreground">容器名</span>
              <Input value={container} onChange={(e) => setContainer(e.target.value)} placeholder="veltrix-sandbox" />
            </label>
          </div>
          <div className="rounded-md border bg-muted/20 px-2.5 py-2 text-xs text-muted-foreground">
            Docker:
            <b className={cfg?.dockerAvailable ? "text-emerald-500" : "text-destructive"}>
              {cfg?.dockerAvailable ? "可用" : "不可用 / 未安装"}
            </b>
            {" · "}容器:
            <b className={cfg?.containerRunning ? "text-emerald-500" : "text-foreground"}>
              {cfg?.containerRunning ? "运行中" : "未运行"}
            </b>
          </div>
          <div className="flex gap-2">
            {/* 启动/停止合并为一个开关:运行中显示「停止」,否则显示「启动」 */}
            <Button
              size="sm"
              variant="outline"
              className="gap-1"
              disabled={busy || !cfg?.dockerAvailable}
              onClick={() =>
                void withBusy(async () => {
                  try {
                    if (cfg?.containerRunning) {
                      await api.sandboxStop();
                      toast.success("已停止沙盒容器");
                    } else {
                      toast.success(await api.sandboxStart());
                    }
                    await refresh();
                  } catch (e) {
                    toast.error(`${cfg?.containerRunning ? "停止" : "启动"}失败: ${e}`);
                  }
                })
              }
            >
              {cfg?.containerRunning ? (
                <>
                  <Square className="size-3.5" />
                  停止沙盒
                </>
              ) : (
                <>
                  <Play className="size-3.5" />
                  启动沙盒
                </>
              )}
            </Button>
            <Button
              size="sm"
              variant="outline"
              className="gap-1"
              disabled={busy || !cfg?.dockerAvailable}
              onClick={() =>
                void withBusy(async () => {
                  try {
                    toast.success(await api.sandboxRecreate());
                    await refresh();
                  } catch (e) {
                    toast.error(`重建失败: ${e}`);
                  }
                })
              }
            >
              <RotateCw className="size-3.5" />
              重建容器
            </Button>
          </div>
          <p className="text-[11px] text-muted-foreground">
            首次启动会拉取镜像(较慢);需本机已安装并运行 Docker,未安装时自动回退本机执行。
          </p>
          <p className="text-[11px] text-amber-600 dark:text-amber-400">
            若「生成的文件不在沙盒 / 无法预览」:多为旧容器挂载错误,点「重建容器」用正确挂载重建一次即可(文件保留在工作区)。
          </p>
        </div>
        <DialogFooter>
          <Button variant="outline" onClick={() => onOpenChange(false)}>
            关闭
          </Button>
          <Button
            disabled={busy}
            onClick={() =>
              void withBusy(async () => {
                try {
                  await api.setSandboxConfig(image, container);
                  toast.success("沙盒配置已保存");
                  await refresh();
                } catch (e) {
                  toast.error(`保存失败: ${e}`);
                }
              })
            }
          >
            {busy && <Loader2 className="size-4 animate-spin" />}
            保存
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

// 单条消息渲染:user 气泡 / assistant 最终回答(无工具调用)
function CodingMessage({ message: m }: { message: ChatMessageView }) {
  if (m.role === "user") {
    return (
      <div className="flex justify-end">
        <div className="max-w-[85%] whitespace-pre-wrap break-words rounded-lg bg-primary/10 px-3 py-2 text-sm text-foreground">
          {m.content}
        </div>
      </div>
    );
  }
  if (m.role === "tool") {
    return (
      <div className="flex items-center gap-1.5 pl-1 text-[11px] text-muted-foreground">
        <span className="text-emerald-500">↳</span>
        <span className="font-medium">{m.toolName || "tool"}</span>
        <span className="opacity-70">结果 · {m.content.length} 字</span>
      </div>
    );
  }
  // assistant
  const calls = parseToolCalls(m.toolCalls);
  return (
    <div className="space-y-1.5">
      {m.content.trim() && (
        <div className="text-sm text-foreground">
          <MarkdownMessage content={m.content} />
        </div>
      )}
      {calls.map((c) => (
        <div
          key={c.id}
          className="flex items-center gap-1.5 rounded-md border bg-card px-2.5 py-1.5 text-xs"
        >
          <Wrench className="size-3.5 shrink-0 text-primary" />
          <span className="font-medium text-foreground">{c.name}</span>
          <span className="truncate text-muted-foreground" title={briefArgs(c.name, c.arguments ?? {})}>
            {briefArgs(c.name, c.arguments ?? {})}
          </span>
        </div>
      ))}
    </div>
  );
}
