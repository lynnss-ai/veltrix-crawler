// 编程 Agent 页面(IDE 双栏):左侧对话/步骤(消息流 + 工具卡),右侧工作区(文件 / 终端)。
// 数据来自会话里的工具消息(assistant.toolCalls 与 role=tool 结果按 toolCallId 关联)。
import { useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { listen } from "@tauri-apps/api/event";
import { openPath, openUrl } from "@tauri-apps/plugin-opener";
import {
  BatteryFull,
  Box,
  ChevronDown,
  ChevronRight,
  Copy,
  ExternalLink,
  Eye,
  FileCode,
  FileText,
  FolderOpen,
  History,
  Loader2,
  Monitor,
  Pencil,
  Play,
  RectangleHorizontal,
  RectangleVertical,
  RotateCw,
  Send,
  Signal,
  Smartphone,
  Square,
  SquareTerminal,
  Tablet,
  Trash2,
  Wifi,
  Wrench,
} from "lucide-react";
import { toast } from "sonner";

import {
  api,
  type ChatMessageView,
  type CheckpointView,
  type CheckpointDiffView,
  type CheckpointFileDiff,
  type DevServerStatus,
  type SandboxConfigView,
  type SandboxStatsView,
} from "@/lib/api";
import { useChat } from "@/hooks/use-chat";
import { useAgentStepListener } from "@/hooks/use-agent-step-listener";
import { MarkdownMessage } from "@/components/MarkdownMessage";
import { ReasoningBlock } from "@/components/ReasoningBlock";
import { CodeEditor } from "@/components/code-editor";
import { EmptyState } from "@/components/EmptyState";
import { SimpleTooltip } from "@/components/SimpleTooltip";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Textarea } from "@/components/ui/textarea";
import {
  Select,
  SelectContent,
  SelectGroup,
  SelectItem,
  SelectLabel,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { useSidebar } from "@/components/ui/sidebar";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
} from "@/components/ui/alert-dialog";
import { cn } from "@/lib/utils";

// 左栏占比低于此值(百分比)视为「过窄」,联动自动收起侧边菜单给左栏腾空间。
// 用占比而非像素:不受窗口 / 侧栏宽度影响,拖到足够窄一定会触发(分割条拖动下限为 28%)
const LEFT_NARROW_PCT = 38;
// 消息列表初始只渲染最近 N 条(长 ReAct 对话切换时避免一次性解析全部),可「加载更早」
const MSG_PAGE_SIZE = 30;

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
  // 抑制「本次发送自建的新会话」触发加载 effect:setActiveId 会让加载与发送抢跑致首条消息重复/闪烁
  const skipLoadRef = useRef<string | null>(null);
  // 供异步发送闭包读「当前激活会话」:切走后不把结果刷回错误会话,也用于过滤 agent-step
  const activeIdRef = useRef(activeId);
  activeIdRef.current = activeId;

  const [messages, setMessages] = useState<ChatMessageView[]>([]);
  // 当前会话只渲染最近 visibleCount 条;切会话重置,「加载更早」时增加
  const [visibleCount, setVisibleCount] = useState(MSG_PAGE_SIZE);
  const [input, setInput] = useState("");
  // 按会话维度的运行态:某会话在后台跑时,开/切到别的会话互不影响(后端本就并发执行)
  const [runningIds, setRunningIds] = useState<Set<string>>(() => new Set());
  const activeSending = activeId ? runningIds.has(activeId) : false;
  const [steps, setSteps] = useState<string[]>([]);
  // 会话标题操作(与普通对话一致):重命名弹框 + 删除二次确认
  const [renameOpen, setRenameOpen] = useState(false);
  const [renameValue, setRenameValue] = useState("");
  const [deleteOpen, setDeleteOpen] = useState(false);
  // 版本管理:检查点列表(下拉打开时拉取)+ 待确认回退的目标版本
  const [checkpoints, setCheckpoints] = useState<CheckpointView[]>([]);
  const [rollbackTarget, setRollbackTarget] = useState<CheckpointView | null>(
    null,
  );
  // 版本管理下拉:受控开合 + 当前展开查看改动的版本 hash + 各版本改动详情缓存(按 hash)
  const [versionMenuOpen, setVersionMenuOpen] = useState(false);
  const [expandedHash, setExpandedHash] = useState<string | null>(null);
  const [diffCache, setDiffCache] = useState<Record<string, CheckpointDiffView>>(
    {},
  );
  // 标记/清除某会话的运行态(增删 Set 元素,触发按会话渲染)。名取 ConvRunning 避开终端的 setRunning
  function setConvRunning(id: string, on: boolean) {
    setRunningIds((prev) => {
      const next = new Set(prev);
      if (on) next.add(id);
      else next.delete(id);
      return next;
    });
  }
  // Plan / Act 临时态:仅前端局部,不持久化;Plan 只调研出方案,Act 亲自动手执行。默认 act。
  const [mode, setMode] = useState<"plan" | "act">("act");
  // 可用模型由共享 providers 派生(避免重挂载重拉导致竞态)。
  // 编程智能体走 ReAct + function calling,故只列具备「工具调用」能力的模型。
  const models = useMemo(() => {
    const opts: { providerId: string; model: string }[] = [];
    for (const p of providers) {
      if (!p.apiKey.trim()) continue;
      for (const spec of p.models) {
        const m = spec.name.trim();
        if (m && spec.capabilities.includes("tools"))
          opts.push({ providerId: p.id, model: m });
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
  // 自动预览信号:每次 act 模式生成结束 +1,通知预览「自动编译并打开」(未跑则启动,已跑则重载)
  const [previewAutoStart, setPreviewAutoStart] = useState(0);
  // 预览 dev server 控制(状态轮询 + 启停/日志/刷新);在此持有,头部按钮与预览面板共享同一实例
  const dev = useDevServer(activeId ?? null, previewReload, previewAutoStart);
  // 用户在终端直接执行的命令(与 Agent 跑过的命令合并展示)
  const [userRuns, setUserRuns] = useState<{ command: string; output: string }[]>([]);
  const [termInput, setTermInput] = useState("");
  const [running, setRunning] = useState(false);
  const [sandboxOpen, setSandboxOpen] = useState(false);
  // 沙盒可用性(用于头部「未隔离」提示)+ 容器运行状态(用于「沙盒」按钮状态点);null=未知
  const [dockerOk, setDockerOk] = useState<boolean | null>(null);
  const [sandboxRunning, setSandboxRunning] = useState(false);
  useEffect(() => {
    api
      .getSandboxConfig()
      .then((c) => {
        setDockerOk(c.dockerAvailable);
        setSandboxRunning(c.containerRunning);
      })
      .catch((e) => console.debug("获取沙箱配置失败:", e));
  }, [sandboxOpen]);
  const scrollRef = useRef<HTMLDivElement>(null);
  // 发送重入锁:state 更新异步,挡住极快连点/交接与手动发送撞车造成的重复发送(仅护建会话窗口)
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
      .catch((e) => console.debug("获取工作区路径失败:", e));
  }, [activeId]);

  // 切换会话时加载消息;若有待交接的首条消息(此刻 DB 尚空),跳过加载,交给发送流程
  useEffect(() => {
    if (!activeId) {
      setMessages([]);
      return;
    }
    if (pendingRef.current) return;
    // 本次发送刚自建的会话:消息由发送流程维护,跳过这次加载
    if (skipLoadRef.current === activeId) {
      skipLoadRef.current = null;
      return;
    }
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
    setTermInput("");
    setSelectedFile(null);
    setVisibleCount(MSG_PAGE_SIZE); // 切会话回到「只渲染最近 N 条」
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

  // 监听 Agent 进度事件(逐步标签):只显示「当前正在查看的会话」的步骤,
  // 其他会话在后台跑、步骤不抢占视图。统一走 useAgentStepListener。
  useAgentStepListener(activeIdRef, setSteps);

  // 监听「Docker 沙盒不可用,已回退本机执行」事件 → 弹窗提示(后端在重新探测且回退时推送)。
  // 用固定 toast id:即便短时多次回退也只刷新同一条提示,不堆叠刷屏。
  useEffect(() => {
    let dispose: (() => void) | undefined;
    let disposed = false;
    listen<{ reason: string }>("coding-sandbox-fallback", (e) => {
      setDockerOk(false);
      toast.warning("Docker 沙盒不可用,已回退本机执行(未隔离)", {
        id: "coding-sandbox-fallback",
        description: e.payload.reason,
        duration: 8000,
      });
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
  }, [messages, steps, activeSending]);

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
      .catch((e) => console.debug("加载工作区文件列表失败:", e));
    return () => {
      alive = false;
    };
  }, [workTab, activeId, fileRefresh, activeSending]);

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
    if (!text || dispatchingRef.current) return;
    // 当前会话正在跑 → 忽略(同一会话不重复发送);其他会话在后台跑不影响本次
    if (activeId && runningIds.has(activeId)) return;
    // 仅「在本页新建会话」时才要求已配模型;交接来的会话(activeId 已存在)已绑定模型,
    // 此时本页的 models 可能尚未加载完,不能据此误报「尚无可用模型」
    if (!activeId && models.length === 0) {
      toast.error("尚无可用模型:编程 Agent 需具备「工具调用」能力的模型,请到系统配置 → 模型厂商勾选");
      return;
    }
    dispatchingRef.current = true;
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
    let convId = activeId;
    try {
      if (!convId) {
        const opt = models[0];
        const conv = await api.createConversation(
          crypto.randomUUID(),
          opt.providerId,
          opt.model,
          "coding",
        );
        convId = conv.id;
        skipLoadRef.current = conv.id; // 抑制 setActiveId 触发的本会话加载,防首条消息重复
        setActiveId(conv.id);
      }
      setConvRunning(convId, true); // 标记该会话运行中(按会话维度,不锁其他会话)
      dispatchingRef.current = false; // 会话已建/已知,释放瞬时建会话锁
      await api.sendCodingMessage(convId, text, sendMode);
      // 跑完:仅当仍停留在该会话才刷新视图与预览,切走了就只在后台完成、不串台
      if (activeIdRef.current === convId) {
        const fresh = await api.listChatMessages(convId);
        setMessages(fresh);
        if (sendMode === "act") {
          setWorkTab("preview");
          setPreviewAutoStart((k) => k + 1);
        }
      }
      await reload(); // 刷新侧栏会话列表(标题/排序),与当前视图无关
    } catch (e) {
      toast.error(`执行失败: ${e}`);
      if (activeIdRef.current === convId) {
        setMessages((prev) => prev.filter((m) => m.id !== optimistic.id));
        setInput(text);
      }
    } finally {
      if (convId) setConvRunning(convId, false);
      if (activeIdRef.current === convId) setSteps([]);
      dispatchingRef.current = false;
    }
  }

  function handleSend() {
    void doSend(input.trim(), mode);
  }

  // 停止自主续航:请求后端在下一步检查点优雅收尾(不强杀,已落库改动保留)
  async function handleStop() {
    if (!activeId) return;
    try {
      await api.stopCodingAgent(activeId);
      toast("已请求停止,正在收尾当前步骤…");
    } catch (e) {
      toast.error(`停止失败: ${e}`);
    }
  }

  // 会话标题:重命名(弹框)/ 删除(二次确认),与普通对话一致
  function openRename() {
    if (!active) return;
    setRenameValue(active.title);
    setRenameOpen(true);
  }
  async function submitRename() {
    if (!active) return;
    const title = renameValue.trim();
    if (!title || title === active.title) {
      setRenameOpen(false);
      return;
    }
    try {
      await api.renameConversation(active.id, title);
      await reload();
      setRenameOpen(false);
    } catch (e) {
      toast.error(`重命名失败: ${e}`);
    }
  }
  async function confirmDelete() {
    if (!active) return;
    try {
      await api.deleteConversation(active.id);
      setActiveId(null);
      setMessages([]);
      await reload();
      setDeleteOpen(false);
      toast.success("已删除会话");
    } catch (e) {
      toast.error(`删除失败: ${e}`);
    }
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
    if (!activeId || activeSending) return;
    setMode("act");
    void doSend(
      "请按上面的【任务计划】逐步执行,每完成一步用 update_plan 勾选进度。",
      "act",
    );
  }

  // 版本管理:打开下拉时拉取检查点列表;展开某版本看其改动详情;选中某版本 → 二次确认 → reset 到该提交
  async function loadCheckpoints() {
    if (!activeId) return;
    try {
      setCheckpoints(await api.listCodingCheckpoints(activeId));
    } catch (e) {
      toast.error(`读取版本失败: ${e}`);
    }
  }
  // 展开/收起某版本:展开时懒加载该版本改动详情(已缓存则直接用)
  async function toggleVersion(hash: string) {
    if (expandedHash === hash) {
      setExpandedHash(null);
      return;
    }
    setExpandedHash(hash);
    if (!activeId || diffCache[hash]) return;
    try {
      const detail = await api.getCheckpointDiff(activeId, hash);
      setDiffCache((prev) => ({ ...prev, [hash]: detail }));
    } catch (e) {
      toast.error(`读取版本改动失败: ${e}`);
    }
  }
  async function doRollback() {
    if (!activeId || !rollbackTarget) return;
    try {
      const msg = await api.rollbackToCheckpoint(activeId, rollbackTarget.hash);
      setRollbackTarget(null);
      setFileRefresh((k) => k + 1); // 回退后刷新文件面板
      setPreviewReload((k) => k + 1); // 预览重载反映回退结果
      toast.success(msg);
    } catch (e) {
      toast.error(`回退失败: ${e}`);
    }
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
      <SandboxDialog open={sandboxOpen} onOpenChange={setSandboxOpen} />

      {/* 会话重命名(与普通对话一致) */}
      <Dialog open={renameOpen} onOpenChange={setRenameOpen}>
        <DialogContent className="sm:max-w-sm">
          <DialogHeader>
            <DialogTitle>重命名会话</DialogTitle>
          </DialogHeader>
          <Input
            autoFocus
            value={renameValue}
            onChange={(e) => setRenameValue(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter") {
                e.preventDefault();
                void submitRename();
              }
            }}
            placeholder="输入会话标题"
          />
          <DialogFooter>
            <Button variant="outline" onClick={() => setRenameOpen(false)}>
              取消
            </Button>
            <Button onClick={() => void submitRename()}>确定</Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      {/* 删除会话:二次确认 */}
      <AlertDialog open={deleteOpen} onOpenChange={setDeleteOpen}>
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>删除会话</AlertDialogTitle>
            <AlertDialogDescription>
              确定删除「{active?.title || "编程 Agent"}」?此操作不可恢复。
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel>取消</AlertDialogCancel>
            <AlertDialogAction
              className="bg-destructive text-white hover:bg-destructive/90"
              onClick={() => void confirmDelete()}
            >
              删除
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>

      {/* 版本回退:二次确认 */}
      <AlertDialog
        open={!!rollbackTarget}
        onOpenChange={(o) => {
          if (!o) setRollbackTarget(null);
        }}
      >
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>回退到该版本</AlertDialogTitle>
            <AlertDialogDescription>
              将把工作区文件恢复到「{rollbackTarget?.message}」时的状态;该版本之后的文件改动会丢失(对话历史保留)。
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel>取消</AlertDialogCancel>
            <AlertDialogAction
              className="bg-destructive text-white hover:bg-destructive/90"
              onClick={() => void doRollback()}
            >
              回退
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>

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
          {/* 会话标题:与普通对话一致——无分隔栏 + 可点下拉(重命名/删除) */}
          {active && (
            <div className="flex shrink-0 items-center justify-between gap-2 px-4 py-2">
              <DropdownMenu>
                <DropdownMenuTrigger asChild>
                  <button
                    type="button"
                    className="inline-flex max-w-[70%] items-center gap-1 rounded-md px-1.5 py-1 text-sm font-medium text-foreground transition-colors hover:bg-accent"
                  >
                    <span className="truncate">{active.title || "编程 Agent"}</span>
                    <ChevronDown className="size-4 shrink-0 opacity-60" />
                  </button>
                </DropdownMenuTrigger>
                <DropdownMenuContent align="start">
                  <DropdownMenuItem onClick={openRename}>
                    <Pencil className="size-4" />
                    重命名
                  </DropdownMenuItem>
                  <DropdownMenuItem
                    onClick={() => setDeleteOpen(true)}
                    className="text-destructive focus:text-destructive"
                  >
                    <Trash2 className="size-4" />
                    删除
                  </DropdownMenuItem>
                </DropdownMenuContent>
              </DropdownMenu>
            </div>
          )}
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
                    disabled={activeSending}
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
            className="veltrix-thin-scrollbar min-h-0 flex-1 overflow-y-auto px-8 py-3"
          >
            {/* 内容限宽居中:对话内容宽度 == 输入框宽度,与普通对话一致 */}
            <div className="mx-auto max-w-3xl space-y-2.5">
            {messages.length === 0 && !activeSending ? (
              models.length === 0 ? (
                <EmptyState
                  icon={Wrench}
                  title="尚无可用模型"
                  description="编程 Agent 需具备「工具调用」能力的模型。请到系统配置 → 模型厂商,为模型勾选该能力后再开始。"
                />
              ) : (
                <EmptyState
                  icon={FileCode}
                  title="编程 Agent"
                  description="描述你的编程任务,我会在工作区内读写文件、执行命令并自我验证。"
                />
              )
            ) : (
              <>
                {messages.length > visibleCount && (
                  <div className="flex justify-center">
                    <Button
                      variant="ghost"
                      size="sm"
                      className="h-7 text-xs text-muted-foreground"
                      onClick={() => setVisibleCount((c) => c + MSG_PAGE_SIZE)}
                    >
                      加载更早的消息({messages.length - visibleCount})
                    </Button>
                  </div>
                )}
                {(messages.length > visibleCount
                  ? messages.slice(messages.length - visibleCount)
                  : messages
                ).map((m) => (
                  <CodingMessage key={m.id} message={m} />
                ))}
              </>
            )}
            {/* 进行中:实时思考过程(逐步累计,可滚动)——仅当前查看的会话 */}
            {activeSending && (
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
          </div>
          {/* 输入:与对话输入框风格一致(圆角卡片 + 无边框 textarea + 发送) */}
          <div className="shrink-0 px-8 pb-2">
            <div className="mx-auto max-w-3xl">
            <div className="flex flex-col gap-1 rounded-2xl border bg-card p-2 shadow-lg">
              <Textarea
                value={input}
                onChange={(e) => setInput(e.target.value)}
                onKeyDown={(e) => {
                  if (e.key === "Enter" && !e.shiftKey) {
                    e.preventDefault();
                    if (!activeSending) void handleSend();
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
                  <SimpleTooltip content="方案模式:只调研并产出分步实现方案,不改动 / 不运行">
                    <button
                      type="button"
                      onClick={() => setMode("plan")}
                      disabled={activeSending}
                      className={cn(
                        "rounded-md px-2.5 py-1 font-medium transition-colors",
                        mode === "plan"
                          ? "bg-background text-foreground shadow-sm"
                          : "text-muted-foreground hover:text-foreground",
                      )}
                    >
                      Plan
                    </button>
                  </SimpleTooltip>
                  <SimpleTooltip content="执行模式:在工作区内读写文件、运行命令并自我验证">
                    <button
                      type="button"
                      onClick={() => setMode("act")}
                      disabled={activeSending}
                      className={cn(
                        "rounded-md px-2.5 py-1 font-medium transition-colors",
                        mode === "act"
                          ? "bg-background text-foreground shadow-sm"
                          : "text-muted-foreground hover:text-foreground",
                      )}
                    >
                      Act
                    </button>
                  </SimpleTooltip>
                </div>
                {/* 自主续航中 → 可点的「停止」(下一步检查点收尾);否则 → 发送 */}
                {activeSending ? (
                  <SimpleTooltip content="停止自主续航(下一步收尾,已落库改动保留)">
                    <Button
                      size="icon"
                      variant="destructive"
                      className="size-9 shrink-0 cursor-pointer rounded-xl"
                      onClick={() => void handleStop()}
                    >
                      <Square className="size-3.5" />
                    </Button>
                  </SimpleTooltip>
                ) : (
                  <Button
                    size="icon"
                    className="size-9 shrink-0 cursor-pointer rounded-xl"
                    disabled={!input.trim()}
                    onClick={() => void handleSend()}
                  >
                    <Send />
                  </Button>
                )}
              </div>
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
            <div className="ml-auto flex items-center gap-1">
              {dockerOk === false && (
                <span className="rounded bg-amber-500/15 px-1.5 py-0.5 text-[10px] text-amber-600 dark:text-amber-400">
                  未隔离·本机
                </span>
              )}
              {/* 顺序:沙盒 → 启动/停止 → 回退。按钮自带图标+文字标签+悬浮态,不再额外加 tooltip */}
              <Button
                variant="ghost"
                size="sm"
                className="h-7 shrink-0 gap-1 text-xs"
                onClick={() => setSandboxOpen(true)}
              >
                <Box
                  className={cn(
                    "size-3.5",
                    dockerOk === false
                      ? "text-muted-foreground/60"
                      : sandboxRunning
                        ? "text-emerald-500"
                        : "text-amber-500",
                  )}
                />
                沙盒
              </Button>
              {/* 预览服务启停:仅预览 tab 显示(dev server 全局单实例,状态来自 useDevServer;日志按钮在预览工具栏内) */}
              {workTab === "preview" &&
                (dev.running ? (
                  <Button
                    variant="ghost"
                    size="sm"
                    className="h-7 shrink-0 gap-1 text-xs text-red-600 hover:text-red-600 dark:text-red-400 dark:hover:text-red-400"
                    disabled={dev.busy}
                    onClick={() => void dev.stop()}
                  >
                    <Square className="size-3.5" />
                    停止
                  </Button>
                ) : (
                  <Button
                    variant="ghost"
                    size="sm"
                    className="h-7 shrink-0 gap-1 text-xs text-emerald-600 hover:text-emerald-600 dark:text-emerald-400 dark:hover:text-emerald-400"
                    disabled={dev.busy}
                    onClick={() => void dev.start()}
                  >
                    <Play className="size-3.5" />
                    启动
                  </Button>
                ))}
              {/* 版本管理:点开列出版本历史(每轮任务前的快照),展开看该版本改动详情,选一个 → 二次确认 → reset */}
              <DropdownMenu
                open={versionMenuOpen}
                onOpenChange={(open) => {
                  setVersionMenuOpen(open);
                  if (open) {
                    setExpandedHash(null);
                    void loadCheckpoints();
                  }
                }}
              >
                <DropdownMenuTrigger asChild>
                  <Button
                    variant="ghost"
                    size="sm"
                    className="h-7 shrink-0 gap-1 text-xs text-amber-600 hover:text-amber-600 dark:text-amber-500 dark:hover:text-amber-500"
                    disabled={!activeId}
                  >
                    <History className="size-3.5" />
                    版本管理
                  </Button>
                </DropdownMenuTrigger>
                <DropdownMenuContent
                  align="end"
                  className="max-h-[70vh] w-[34rem] overflow-y-auto p-0"
                >
                  {checkpoints.length === 0 ? (
                    <div className="px-3 py-4 text-center text-xs text-muted-foreground">
                      暂无版本(发送任务后会自动建立检查点)
                    </div>
                  ) : (
                    checkpoints.map((c) => {
                      const expanded = expandedHash === c.hash;
                      const detail = diffCache[c.hash];
                      return (
                        <div key={c.hash} className="border-b last:border-b-0">
                          {/* 版本行:点击展开/收起其改动详情 */}
                          <button
                            type="button"
                            onClick={() => void toggleVersion(c.hash)}
                            className="flex w-full items-start gap-2 px-3 py-2 text-left transition-colors hover:bg-accent"
                          >
                            <ChevronRight
                              className={cn(
                                "mt-0.5 size-3.5 shrink-0 text-muted-foreground transition-transform",
                                expanded && "rotate-90",
                              )}
                            />
                            <div className="min-w-0 flex-1">
                              <div className="line-clamp-1 text-xs">{c.message}</div>
                              <div className="text-[10px] text-muted-foreground">
                                {new Date(c.time * 1000).toLocaleString()}
                              </div>
                            </div>
                          </button>
                          {expanded && (
                            <div className="px-3 pb-2.5">
                              {!detail ? (
                                <div className="py-2 text-center text-xs text-muted-foreground">
                                  <Loader2 className="mr-1 inline size-3 animate-spin" />
                                  加载改动…
                                </div>
                              ) : detail.files.length === 0 ? (
                                <div className="py-2 text-xs text-muted-foreground">
                                  该版本无文件改动
                                </div>
                              ) : (
                                <div className="space-y-1.5">
                                  {detail.files.map((f) => (
                                    <CheckpointFileItem key={f.path} file={f} />
                                  ))}
                                </div>
                              )}
                              <Button
                                size="sm"
                                variant="outline"
                                className="mt-2 h-7 w-full gap-1 text-xs text-amber-600 hover:text-amber-600 dark:text-amber-500 dark:hover:text-amber-500"
                                onClick={() => {
                                  setVersionMenuOpen(false);
                                  setRollbackTarget(c);
                                }}
                              >
                                <History className="size-3.5" />
                                回退到此版本
                              </Button>
                            </div>
                          )}
                        </div>
                      );
                    })
                  )}
                </DropdownMenuContent>
              </DropdownMenu>
              <Button
                variant="ghost"
                size="sm"
                className="h-7 shrink-0 gap-1 text-xs"
                disabled={!workspace}
                onClick={() =>
                  void openPath(workspace).catch((e) => toast.error(`打开目录失败: ${e}`))
                }
              >
                <FolderOpen className="size-3.5" />
                打开目录
              </Button>
            </div>
          </div>
          <div className="flex min-h-0 flex-1 flex-col overflow-hidden">
            {workTab === "files" ? (
              <div className="flex min-h-0 flex-1 flex-col">
                <div className="flex shrink-0 items-center justify-between border-b px-2 py-1 text-[11px] text-muted-foreground">
                  <span>工作区文件{wsFiles.length > 0 ? `(${wsFiles.length})` : ""}</span>
                  <SimpleTooltip content="刷新文件列表">
                    <Button
                      type="button"
                      size="icon"
                      variant="ghost"
                      className="size-6"
                      onClick={() => setFileRefresh((k) => k + 1)}
                    >
                      <RotateCw className="size-3" />
                    </Button>
                  </SimpleTooltip>
                </div>
                {wsFiles.length === 0 ? (
                  <div className="flex flex-1 items-center justify-center px-6 text-center text-xs text-muted-foreground">
                    工作区暂无文件;Agent 写文件后会自动出现(也可点右上角刷新)
                  </div>
                ) : (
                  <div className="flex min-h-0 flex-1">
                    <div className="veltrix-thin-scrollbar w-44 shrink-0 overflow-y-auto border-r p-1.5">
                      {wsFiles.map((f) => (
                        <SimpleTooltip key={f} content={f} side="right">
                          <button
                            type="button"
                            onClick={() => setSelectedFile(f)}
                            className={cn(
                              "block w-full truncate rounded px-2 py-1 text-left text-xs transition-colors",
                              shownFile === f
                                ? "bg-primary/10 font-medium text-primary"
                                : "text-foreground hover:bg-accent/50",
                            )}
                          >
                            {f}
                          </button>
                        </SimpleTooltip>
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
              <PreviewServer dev={dev} />
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

// 预览设备模拟器:宽高为「竖屏」CSS 视口尺寸(横屏时由代码对调);width=0 表示响应式铺满。
// 取主流真机的逻辑像素(CSS px,非物理像素),iframe 按此宽度渲染即触发被预览站点的响应式断点。
// frame:顶部形态——island(灵动岛)/ notch(刘海)/ punch(挖孔)/ none(无,SE / 平板)。
type DeviceGroup = "桌面" | "iPhone" | "Android" | "平板";
interface PreviewDevice {
  id: string;
  label: string;
  width: number;
  height: number;
  group: DeviceGroup;
  frame: "island" | "notch" | "punch" | "none";
}
// 分组展示顺序
const DEVICE_GROUPS: DeviceGroup[] = ["桌面", "iPhone", "Android", "平板"];
// 尺寸为各机型「竖屏 CSS 逻辑分辨率(点)」,经 yesviz 核对(2026-06)。
// 苹果覆盖最近 2 代(iPhone 17 / 16)全系;灵动岛(island)/刘海(16e notch)按真机区分。
const PREVIEW_DEVICES: PreviewDevice[] = [
  { id: "responsive", label: "响应式(铺满)", width: 0, height: 0, group: "桌面", frame: "none" },
  // —— iPhone(新代在前)——
  { id: "iphone-17-pro-max", label: "iPhone 17 Pro Max", width: 440, height: 956, group: "iPhone", frame: "island" },
  { id: "iphone-17-pro", label: "iPhone 17 Pro", width: 402, height: 874, group: "iPhone", frame: "island" },
  { id: "iphone-17-air", label: "iPhone 17 Air", width: 420, height: 912, group: "iPhone", frame: "island" },
  { id: "iphone-17", label: "iPhone 17", width: 402, height: 874, group: "iPhone", frame: "island" },
  { id: "iphone-16-pro-max", label: "iPhone 16 Pro Max", width: 440, height: 956, group: "iPhone", frame: "island" },
  { id: "iphone-16-pro", label: "iPhone 16 Pro", width: 402, height: 874, group: "iPhone", frame: "island" },
  { id: "iphone-16-plus", label: "iPhone 16 Plus", width: 430, height: 932, group: "iPhone", frame: "island" },
  { id: "iphone-16", label: "iPhone 16", width: 393, height: 852, group: "iPhone", frame: "island" },
  { id: "iphone-16e", label: "iPhone 16e", width: 390, height: 844, group: "iPhone", frame: "notch" },
  // —— Android ——
  { id: "pixel-7", label: "Pixel 7", width: 412, height: 915, group: "Android", frame: "punch" },
  { id: "galaxy-s8", label: "Galaxy S8+", width: 360, height: 740, group: "Android", frame: "punch" },
  // —— 平板 ——
  { id: "ipad-mini", label: "iPad Mini", width: 744, height: 1133, group: "平板", frame: "none" },
  { id: "ipad-air", label: "iPad Air", width: 820, height: 1180, group: "平板", frame: "none" },
  { id: "ipad-pro-11", label: 'iPad Pro 11"', width: 834, height: 1194, group: "平板", frame: "none" },
  { id: "ipad-pro-13", label: 'iPad Pro 12.9"', width: 1024, height: 1366, group: "平板", frame: "none" },
  { id: "surface-pro", label: "Surface Pro", width: 912, height: 1368, group: "平板", frame: "none" },
];
// 设备帧四周留白(px):缩放计算时从可用区扣除,避免设备贴边(需容纳边框+阴影)
const DEVICE_FRAME_PADDING = 40;

// 预览服务(dev server)控制:轮询状态 + 启动/停止/日志/刷新 + 访问地址派生。
// 抽成 hook 让「预览面板」与「工作区头部的启动/停止/日志按钮」共享同一份状态(单实例)。
function useDevServer(
  activeId: string | null,
  reloadSignal: number,
  autoStart: number,
) {
  // 默认前端项目(Vite)预览;容器内服务须绑 0.0.0.0(--host),宿主已发布端口,经 localhost:<port> 访问。
  // 加 CHOKIDAR_USEPOLLING:Docker(尤其 Win/WSL2)bind mount 的文件变更事件常不传播,开轮询让 Vite 感知保存并热更新。
  // --port + --strictPort:命令里的端口号会被后端按会话改写为「自定义且未被占用」的端口(本机模式查占用、
  // 多程序不撞 5173),strictPort 保证撞端口时直接报错可见,而非静默爬升到未知端口。实际端口在地址栏显示。
  const DEV_CMD =
    "CHOKIDAR_USEPOLLING=true npm run dev -- --host --port 5173 --strictPort";
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

  // 生成完成信号(autoStart 自增)→ 自动编译并打开预览(由 startSilently 据后端真实状态决定起 / 重载)
  useEffect(() => {
    if (!autoStart) return;
    void startSilently();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [autoStart]);

  // dev server 是全局单实例;仅当其归属当前会话时才认它,否则切到别的会话不串台
  const belongsToThis =
    !status?.conversationId || status.conversationId === activeId;
  const running = (status?.running ?? false) && belongsToThis;
  const port = belongsToThis ? (status?.port ?? null) : null;
  // 直连本机回环(Vite 新版按 Host 头做 allowedHosts 校验,localhost 最稳)
  const host = "localhost";
  const src = port ? `http://${host}:${port}/?_=${iframeKey}` : "";
  const externalUrl = port ? `http://${host}:${port}/` : "";

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
  // 自动启动(静默):已在跑只重载,未跑才启;空目录 / 纯方案失败不弹错
  async function startSilently() {
    if (busy) return;
    setBusy(true);
    try {
      const s = await api.getDevServerStatus();
      const alreadyRunning =
        s.running && (!s.conversationId || s.conversationId === activeId);
      if (!alreadyRunning) {
        await api.startDevServer(activeId ?? "", command);
      }
      setIframeKey((k) => k + 1);
    } catch {
      // 无可预览内容时静默忽略
    } finally {
      setBusy(false);
    }
  }
  const refresh = () => setIframeKey((k) => k + 1);

  return {
    command,
    setCommand,
    status,
    busy,
    iframeKey,
    refresh,
    showLogs,
    setShowLogs,
    running,
    port,
    src,
    externalUrl,
    start,
    stop,
  };
}

type DevServerController = ReturnType<typeof useDevServer>;

// 预览面板:地址栏(URL + 复制/刷新/打开)+ 右侧移动端模拟器(机型 / 横竖屏 / 状态栏)+ iframe。
// dev server 的启停 / 日志由父组件 useDevServer 提供并在工作区头部呈现(此处只渲染地址栏与画面)。
function PreviewServer({ dev }: { dev: DevServerController }) {
  // 设备模拟器:选中设备 + 横竖屏 + 自动缩放(按预览区大小把设备帧缩到能放下)
  const [deviceId, setDeviceId] = useState("responsive");
  const [orientation, setOrientation] = useState<"portrait" | "landscape">("portrait");
  const [showStatusBar, setShowStatusBar] = useState(true);
  const [scale, setScale] = useState(1);
  const previewAreaRef = useRef<HTMLDivElement>(null);

  const device = PREVIEW_DEVICES.find((d) => d.id === deviceId) ?? PREVIEW_DEVICES[0];
  const isResponsive = device.width === 0;
  // 横屏时对调宽高
  const frameW = orientation === "portrait" ? device.width : device.height;
  const frameH = orientation === "portrait" ? device.height : device.width;
  // 下拉框内的机型图标:按当前设备分组(手机 / 平板 / 桌面)切换
  const DeviceGroupIcon =
    device.group === "平板" ? Tablet : device.group === "桌面" ? Monitor : Smartphone;

  // 自动缩放:量预览区可用尺寸,把设备帧等比缩到能放下(只缩不放,最大 100%)。
  // 预览区尺寸变化(拖分割条等)经 ResizeObserver 重算;响应式模式无需缩放。
  useEffect(() => {
    if (isResponsive) {
      setScale(1);
      return;
    }
    const el = previewAreaRef.current;
    if (!el) return;
    const compute = () => {
      const availW = el.clientWidth - DEVICE_FRAME_PADDING;
      const availH = el.clientHeight - DEVICE_FRAME_PADDING;
      if (availW <= 0 || availH <= 0) return;
      const s = Math.min(1, availW / frameW, availH / frameH);
      setScale(s > 0 ? s : 1);
    };
    compute();
    const ro = new ResizeObserver(compute);
    ro.observe(el);
    return () => ro.disconnect();
  }, [isResponsive, frameW, frameH]);

  // dev server 状态与控制由父组件 useDevServer 提供(与头部启动/停止/日志共享同一实例)
  const { command, setCommand, status, iframeKey, refresh, showLogs, setShowLogs, running, port, src, externalUrl } =
    dev;

  return (
    <div className="flex h-full min-h-0 flex-col">
      <div className="flex shrink-0 items-center gap-1.5 border-b px-2 py-1.5">
        {/* 地址栏:运行且有端口 → 访问 URL + 复制/刷新/打开;探测端口中 → 提示;未运行 → 可编辑命令 */}
        {running ? (
          port ? (
            <div className="group flex h-7 min-w-0 flex-1 items-center gap-0.5 rounded border bg-muted/40 pl-2 pr-0.5">
              <span className="min-w-0 flex-1 select-all truncate font-mono text-xs text-foreground">
                {externalUrl}
              </span>
              {/* 动作按钮:默认隐藏,悬浮地址栏(或聚焦)才浮出——去掉 tooltip,靠图标 + 显隐交互 */}
              <Button
                type="button"
                size="icon"
                variant="ghost"
                aria-label="复制访问地址"
                className="size-6 shrink-0 opacity-0 transition-opacity group-hover:opacity-100 group-focus-within:opacity-100"
                onClick={() =>
                  void navigator.clipboard
                    .writeText(externalUrl)
                    .then(() => toast.success("已复制访问地址"))
                    .catch(() => toast.error("复制失败"))
                }
              >
                <Copy className="size-3.5" />
              </Button>
              <Button
                type="button"
                size="icon"
                variant="ghost"
                aria-label="刷新预览"
                className="size-6 shrink-0 opacity-0 transition-opacity group-hover:opacity-100 group-focus-within:opacity-100"
                onClick={refresh}
              >
                <RotateCw className="size-3.5" />
              </Button>
              <Button
                type="button"
                size="icon"
                variant="ghost"
                aria-label="在外部浏览器打开"
                className="size-6 shrink-0 opacity-0 transition-opacity group-hover:opacity-100 group-focus-within:opacity-100"
                onClick={() =>
                  void openUrl(externalUrl).catch((e) => toast.error(`打开浏览器失败: ${e}`))
                }
              >
                <ExternalLink className="size-3.5" />
              </Button>
            </div>
          ) : (
            <div className="flex h-7 min-w-0 flex-1 items-center rounded border bg-muted/40 px-2 text-xs text-muted-foreground">
              正在探测访问端口…
            </div>
          )
        ) : (
          <input
            value={command}
            onChange={(e) => setCommand(e.target.value)}
            placeholder="服务命令(在工作区 / 沙盒内常驻运行)"
            className="h-7 min-w-0 flex-1 rounded border bg-transparent px-2 font-mono text-xs outline-none"
          />
        )}
        {/* 右侧:移动端模拟器机型选择 */}
        <div className="flex shrink-0 items-center gap-1.5">
          <Select value={deviceId} onValueChange={setDeviceId}>
            <SelectTrigger
              size="sm"
              aria-label="选择预览设备"
              className="w-[13.5rem] px-2 text-[11px]"
            >
              <span className="flex min-w-0 items-center gap-1.5">
                <DeviceGroupIcon className="size-3.5 shrink-0 text-primary" />
                <SelectValue />
              </span>
            </SelectTrigger>
            <SelectContent>
              {DEVICE_GROUPS.map((group) => (
                <SelectGroup key={group}>
                  <SelectLabel>{group}</SelectLabel>
                  {PREVIEW_DEVICES.filter((d) => d.group === group).map((d) => (
                    <SelectItem key={d.id} value={d.id} className="text-xs">
                      {d.label}
                      {d.width > 0 ? ` · ${d.width}×${d.height}` : ""}
                    </SelectItem>
                  ))}
                </SelectGroup>
              ))}
            </SelectContent>
          </Select>
          <Button
            type="button"
            variant="outline"
            size="sm"
            className={cn("h-7 shrink-0 gap-1 px-2 text-[11px]", showLogs && "text-primary")}
            onClick={() => setShowLogs((v) => !v)}
          >
            <FileText className="size-3.5" />
            日志
          </Button>
        </div>
      </div>
      <div
        ref={previewAreaRef}
        className="relative flex min-h-0 flex-1 items-center justify-center overflow-hidden bg-muted/20 dark:bg-black/30"
      >
        {/* 设备控制浮层(内容区右上角):横竖屏 / 状态栏 / 当前比例;高度与设备筛选一致(h-7),分割线分组 */}
        {!isResponsive && (
          <div className="absolute right-2 top-2 z-10 flex h-7 items-center gap-0.5 rounded-md border bg-background/90 pl-1 pr-2 shadow-sm backdrop-blur">
            <SimpleTooltip content={orientation === "portrait" ? "切换为横屏" : "切换为竖屏"}>
              <Button
                type="button"
                size="icon"
                variant="ghost"
                className="size-6"
                onClick={() =>
                  setOrientation((o) => (o === "portrait" ? "landscape" : "portrait"))
                }
              >
                {orientation === "portrait" ? (
                  <RectangleVertical className="size-4" />
                ) : (
                  <RectangleHorizontal className="size-4" />
                )}
              </Button>
            </SimpleTooltip>
            <SimpleTooltip content={showStatusBar ? "隐藏状态栏" : "显示状态栏"}>
              <Button
                type="button"
                size="icon"
                variant="ghost"
                className={cn("size-6", showStatusBar && "text-primary")}
                onClick={() => setShowStatusBar((v) => !v)}
              >
                <Signal className="size-4" />
              </Button>
            </SimpleTooltip>
            {/* 分割线 */}
            <span className="mx-1 h-4 w-px shrink-0 bg-border" />
            <span className="text-[11px] tabular-nums text-muted-foreground">
              {frameW}×{frameH} · {Math.round(scale * 100)}%
            </span>
          </div>
        )}
        {port ? (
          isResponsive ? (
            <iframe
              key={iframeKey}
              src={src}
              title="预览"
              className="size-full border-0 bg-white"
            />
          ) : (
            <DeviceFrame
              device={device}
              frameW={frameW}
              frameH={frameH}
              scale={scale}
              orientation={orientation}
              showStatusBar={showStatusBar}
              src={src}
              iframeKey={iframeKey}
            />
          )
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

// 设备外框:深色边框 + 圆角屏幕 + 刘海/灵动岛/挖孔 + 底部 Home 指示条,按机型渲染。
// 屏幕容器尺寸 = 缩放后视口(sw×sh)、overflow-hidden 裁圆角;iframe 仍按真实视口渲染再 transform 缩放,
// 并比屏幕宽出 SCROLLBAR_HIDE_PX 把纵向滚动条挤出屏幕被裁掉(内容布局宽度仍 = 设备宽,断点准)。
function DeviceFrame({
  device,
  frameW,
  frameH,
  scale,
  orientation,
  showStatusBar,
  src,
  iframeKey,
}: {
  device: PreviewDevice;
  frameW: number;
  frameH: number;
  scale: number;
  orientation: "portrait" | "landscape";
  showStatusBar: boolean;
  src: string;
  iframeKey: number;
}) {
  const sw = frameW * scale;
  const sh = frameH * scale;
  const isPortrait = orientation === "portrait";
  // 边框/圆角固定 px(不随缩放,保证外观一致):平板更方、现代手机更圆、SE 居中
  const isTablet = device.group === "平板";
  const bezel = isTablet ? 14 : 12;
  const screenRadius = isTablet ? 12 : device.frame === "none" ? 16 : 38;
  const outerRadius = screenRadius + bezel;
  // 顶部形态仅竖屏渲染(横屏刘海在侧边,简化为不画);现代全面屏(岛/挖孔)显示底部 Home 指示条
  const showTop = isPortrait && device.frame !== "none";
  const showHomeBar = isPortrait && (device.frame === "island" || device.frame === "punch");
  // 状态栏高度:全面屏(岛/刘海/挖孔)更高(容纳岛),SE/平板矮一些
  const statusH = device.frame === "none" ? sh * 0.032 : sh * 0.048;
  // 状态栏是否需要为顶部岛/刘海让出中间(竖屏全面屏才有)
  const statusHasCenterGap = isPortrait && (device.frame === "island" || device.frame === "notch");

  return (
    <div
      className="relative shrink-0 bg-neutral-900 shadow-2xl ring-1 ring-black/30 dark:bg-neutral-950"
      style={{ padding: bezel, borderRadius: outerRadius }}
    >
      <div
        className="relative overflow-hidden bg-white"
        style={{ width: sw, height: sh, borderRadius: screenRadius }}
      >
        <iframe
          key={iframeKey}
          src={src}
          title="预览"
          // 精确按设备视口 frameW×frameH 渲染再等比 transform 缩放:屏幕容器 = sw×sh,
          // 比例严格 = frameW:frameH,内容不裁切;页面若滚动,其滚动条也随 scale 一并缩小,不突兀。
          className="block border-0 bg-white"
          style={{
            width: frameW,
            height: frameH,
            transform: `scale(${scale})`,
            transformOrigin: "top left",
          }}
        />
        {/* 状态栏(时间 + 信号/WiFi/电池),竖屏渲染,覆盖在内容顶部 */}
        {showStatusBar && isPortrait && (
          <StatusBar
            width={sw}
            height={statusH}
            centerGap={statusHasCenterGap}
          />
        )}
        {/* 灵动岛(iPhone 14 Pro / 15 / 16 / 17 等) */}
        {showTop && device.frame === "island" && (
          <div
            className="pointer-events-none absolute left-1/2 z-20 -translate-x-1/2 rounded-full bg-black"
            style={{ top: sw * 0.026, width: sw * 0.3, height: sw * 0.072 }}
          />
        )}
        {/* 刘海(顶边凸起,iPhone 14 / 16e 等) */}
        {showTop && device.frame === "notch" && (
          <div
            className="pointer-events-none absolute left-1/2 top-0 z-20 -translate-x-1/2 bg-black"
            style={{
              width: sw * 0.46,
              height: sw * 0.058,
              borderBottomLeftRadius: sw * 0.05,
              borderBottomRightRadius: sw * 0.05,
            }}
          />
        )}
        {/* 挖孔摄像头(Android) */}
        {showTop && device.frame === "punch" && (
          <div
            className="pointer-events-none absolute left-1/2 z-20 -translate-x-1/2 rounded-full bg-black"
            style={{ top: sw * 0.03, width: sw * 0.05, height: sw * 0.05 }}
          />
        )}
        {/* 底部 Home 指示条 */}
        {showHomeBar && (
          <div
            className="pointer-events-none absolute left-1/2 z-20 -translate-x-1/2 rounded-full bg-black/35"
            style={{ bottom: sh * 0.013, width: sw * 0.33, height: Math.max(3, sw * 0.012) }}
          />
        )}
      </div>
    </div>
  );
}

// 真机状态栏:左侧实时时间,右侧信号/WiFi/电池;覆盖在页面顶部(半透明深色 + 白字,跨内容可读)。
// centerGap=true(灵动岛/刘海机型)时左右元素分居两侧,给中间的岛/刘海让位。
function StatusBar({
  width,
  height,
  centerGap,
}: {
  width: number;
  height: number;
  centerGap: boolean;
}) {
  const [time, setTime] = useState("");
  useEffect(() => {
    const fmt = () => {
      const d = new Date();
      return `${d.getHours()}:${String(d.getMinutes()).padStart(2, "0")}`;
    };
    setTime(fmt());
    const timer = setInterval(() => setTime(fmt()), 20000);
    return () => clearInterval(timer);
  }, []);

  const fontSize = Math.max(9, height * 0.42);
  const iconSize = Math.max(10, height * 0.5);
  const padX = centerGap ? width * 0.08 : width * 0.06;

  return (
    <div
      className="pointer-events-none absolute inset-x-0 top-0 z-10 flex items-center justify-between bg-gradient-to-b from-black/30 to-transparent font-semibold text-white"
      style={{ height, paddingLeft: padX, paddingRight: padX, fontSize }}
    >
      <span className="tabular-nums">{time}</span>
      <span className="flex items-center" style={{ gap: iconSize * 0.35 }}>
        <Signal style={{ width: iconSize, height: iconSize }} />
        <Wifi style={{ width: iconSize, height: iconSize }} />
        <BatteryFull style={{ width: iconSize * 1.3, height: iconSize }} />
      </span>
    </div>
  );
}

// 把 docker stats 的百分比字符串(如 "12.34%")解析为 0~100 的数值(供进度条宽度;CPU 多核可 >100,夹到 100)。
function statPct(s?: string): number {
  if (!s) return 0;
  const n = parseFloat(s);
  return Number.isFinite(n) ? Math.max(0, Math.min(100, n)) : 0;
}

// 沙盒资源占用单条:标签 + 数值 + 细进度条(按占用高低变色:绿 / 黄 / 红)。
function SandboxStatBar({
  label,
  value,
  percent,
}: {
  label: string;
  value: string;
  percent: number;
}) {
  const color =
    percent >= 85 ? "bg-red-500" : percent >= 60 ? "bg-amber-500" : "bg-emerald-500";
  return (
    <div>
      <div className="flex items-center justify-between text-[11px]">
        <span className="text-muted-foreground">{label}</span>
        <span className="tabular-nums text-foreground">{value || "—"}</span>
      </div>
      <div className="mt-0.5 h-1 overflow-hidden rounded-full bg-muted">
        <div
          className={cn("h-full rounded-full transition-all", color)}
          style={{ width: `${percent}%` }}
        />
      </div>
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
  const [stats, setStats] = useState<SandboxStatsView | null>(null);
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
  // 资源占用:弹窗打开期间定时采样(docker stats 每次约 1~2s,3s 一轮足够);关闭即停
  useEffect(() => {
    if (!open) {
      setStats(null);
      return;
    }
    let alive = true;
    const tick = async () => {
      try {
        const s = await api.getSandboxStats();
        if (alive) setStats(s);
      } catch {
        // 忽略
      }
    };
    void tick();
    const timer = setInterval(() => void tick(), 3000);
    return () => {
      alive = false;
      clearInterval(timer);
    };
  }, [open]);

  async function withBusy(fn: () => Promise<void>) {
    setBusy(true);
    try {
      await fn();
    } finally {
      setBusy(false);
    }
  }
  // 去掉显式「保存」按钮:镜像 / 容器名改动在失焦及启动 / 重建前自动持久化,确保编辑即时生效。
  async function persistConfig() {
    try {
      await api.setSandboxConfig(image, container);
    } catch (e) {
      toast.error(`保存沙盒配置失败: ${e}`);
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
              <Input
                value={image}
                onChange={(e) => setImage(e.target.value)}
                onBlur={() => void persistConfig()}
                placeholder="node:20-bookworm"
              />
            </label>
            <label className="space-y-1">
              <span className="text-xs text-muted-foreground">容器名</span>
              <Input
                value={container}
                onChange={(e) => setContainer(e.target.value)}
                onBlur={() => void persistConfig()}
                placeholder="veltrix-sandbox"
              />
            </label>
          </div>
          <div className="rounded-md border bg-muted/20 px-2.5 py-2 text-xs text-muted-foreground">
            <div>
              Docker:
              <b className={cfg?.dockerAvailable ? "text-emerald-500" : "text-destructive"}>
                {cfg?.dockerAvailable ? "可用" : "不可用 / 未安装"}
              </b>
              {" · "}容器:
              <b className={cfg?.containerRunning ? "text-emerald-500" : "text-foreground"}>
                {cfg?.containerRunning ? "运行中" : "未运行"}
              </b>
            </div>
            {/* 资源占用:容器运行中显示 docker stats 实时采样(CPU / 内存各一条细进度条) */}
            <div className="mt-1.5 space-y-1.5 border-t pt-1.5">
              <SandboxStatBar
                label="CPU"
                value={stats?.running ? stats.cpuPerc : ""}
                percent={statPct(stats?.running ? stats.cpuPerc : undefined)}
              />
              <SandboxStatBar
                label="内存"
                value={
                  stats?.running
                    ? `${stats.memUsage}${stats.memPerc ? ` · ${stats.memPerc}` : ""}`
                    : ""
                }
                percent={statPct(stats?.running ? stats.memPerc : undefined)}
              />
            </div>
          </div>
          <div className="flex justify-end gap-2">
            {/* 运行中 → 红色「停止」(二次确认);否则 → 绿色「启动」 */}
            {cfg?.containerRunning ? (
              <Button
                size="sm"
                variant="outline"
                className="gap-1 text-red-600 hover:text-red-600 dark:text-red-400 dark:hover:text-red-400"
                disabled={busy || !cfg?.dockerAvailable}
                onClick={() =>
                  toast("停止沙盒容器?运行中的预览 / 命令会中断(工作区文件保留)", {
                    action: {
                      label: "停止",
                      onClick: () =>
                        void withBusy(async () => {
                          try {
                            await api.sandboxStop();
                            toast.success("已停止沙盒容器");
                            await refresh();
                          } catch (e) {
                            toast.error(`停止失败: ${e}`);
                          }
                        }),
                    },
                  })
                }
              >
                <Square className="size-3.5" />
                停止
              </Button>
            ) : (
              <Button
                size="sm"
                variant="outline"
                className="gap-1 text-emerald-600 hover:text-emerald-600 dark:text-emerald-400 dark:hover:text-emerald-400"
                disabled={busy || !cfg?.dockerAvailable}
                onClick={() =>
                  void withBusy(async () => {
                    try {
                      // 启动前先落盘当前镜像 / 容器名,确保用的是最新编辑值(后端按已保存配置拉容器)
                      await persistConfig();
                      toast.success(await api.sandboxStart());
                      await refresh();
                    } catch (e) {
                      toast.error(`启动失败: ${e}`);
                    }
                  })
                }
              >
                <Play className="size-3.5" />
                启动
              </Button>
            )}
            {/* 重建:红色 + 二次确认(删容器重建) */}
            <Button
              size="sm"
              variant="outline"
              className="gap-1 text-red-600 hover:text-red-600 dark:text-red-400 dark:hover:text-red-400"
              disabled={busy || !cfg?.dockerAvailable}
              onClick={() =>
                toast("重建沙盒容器?将删除现有容器并重新创建(工作区文件保留)", {
                  action: {
                    label: "重建",
                    onClick: () =>
                      void withBusy(async () => {
                        try {
                          // 重建前先落盘当前镜像 / 容器名,确保按最新编辑值重建
                          await persistConfig();
                          toast.success(await api.sandboxRecreate());
                          await refresh();
                        } catch (e) {
                          toast.error(`重建失败: ${e}`);
                        }
                      }),
                  },
                })
              }
            >
              <RotateCw className="size-3.5" />
              重建
            </Button>
          </div>
          <p className="text-[11px] text-muted-foreground">
            首次启动会拉取镜像(较慢);需本机已安装并运行 Docker,未安装时自动回退本机执行。
          </p>
          <p className="text-[11px] text-amber-600 dark:text-amber-400">
            若「生成的文件不在沙盒 / 无法预览」:多为旧容器挂载错误,点「重建容器」用正确挂载重建一次即可(文件保留在工作区)。
          </p>
        </div>
      </DialogContent>
    </Dialog>
  );
}

// 写代码类工具:对话中以折叠块呈现实际代码 / diff;其余工具调用一律隐藏。
const CODE_TOOLS = ["write_file", "replace_in_file"] as const;

// 编码折叠块:Agent 写文件 / 改文件时把代码或 diff 折叠展示(默认收起,点标题展开)。
function CollapsibleCode({
  label,
  path,
  code,
}: {
  label: string;
  path: string;
  code: string;
}) {
  const [open, setOpen] = useState(false);
  const lineCount = code ? code.split("\n").length : 0;
  return (
    <div className="overflow-hidden rounded-md border bg-card">
      <button
        type="button"
        onClick={() => setOpen((v) => !v)}
        className="flex w-full items-center gap-1.5 px-2.5 py-1.5 text-left text-xs transition-colors hover:bg-accent/40"
      >
        {open ? (
          <ChevronDown className="size-3.5 shrink-0 text-muted-foreground" />
        ) : (
          <ChevronRight className="size-3.5 shrink-0 text-muted-foreground" />
        )}
        <FileCode className="size-3.5 shrink-0 text-primary" />
        <span className="shrink-0 font-medium text-foreground">{label}</span>
        <SimpleTooltip content={path}>
          <span className="truncate text-muted-foreground">{path}</span>
        </SimpleTooltip>
        <span className="ml-auto shrink-0 text-[10px] text-muted-foreground">
          {lineCount} 行
        </span>
      </button>
      {open && (
        <pre className="veltrix-thin-scrollbar max-h-80 overflow-auto whitespace-pre-wrap break-words border-t bg-background/60 p-2 font-mono text-[11px] leading-5 text-muted-foreground">
          {code}
        </pre>
      )}
    </div>
  );
}

// 版本管理:单个文件的改动条目——状态徽标 + 路径 + 增删行数 + 可展开的完整 diff(按行着色)
const FILE_STATUS_META: Record<string, { label: string; cls: string }> = {
  added: { label: "新增", cls: "bg-emerald-500/15 text-emerald-600 dark:text-emerald-400" },
  modified: { label: "修改", cls: "bg-amber-500/15 text-amber-600 dark:text-amber-400" },
  deleted: { label: "删除", cls: "bg-red-500/15 text-red-600 dark:text-red-400" },
  renamed: { label: "重命名", cls: "bg-sky-500/15 text-sky-600 dark:text-sky-400" },
};

function CheckpointFileItem({ file }: { file: CheckpointFileDiff }) {
  const [open, setOpen] = useState(false);
  const meta = FILE_STATUS_META[file.status] ?? {
    label: file.status,
    cls: "bg-muted text-muted-foreground",
  };
  return (
    <div className="overflow-hidden rounded border bg-muted/20">
      <button
        type="button"
        onClick={() => setOpen((v) => !v)}
        className="flex w-full items-center gap-1.5 px-2 py-1 text-left text-[11px] transition-colors hover:bg-accent"
      >
        <span className={cn("shrink-0 rounded px-1 py-0.5 text-[10px] font-medium", meta.cls)}>
          {meta.label}
        </span>
        <span className="min-w-0 flex-1 truncate font-mono">{file.path}</span>
        {file.additions > 0 && (
          <span className="shrink-0 text-emerald-600 dark:text-emerald-400">
            +{file.additions}
          </span>
        )}
        {file.deletions > 0 && (
          <span className="shrink-0 text-red-600 dark:text-red-400">
            -{file.deletions}
          </span>
        )}
        {file.diff && (
          <ChevronRight
            className={cn(
              "size-3 shrink-0 text-muted-foreground transition-transform",
              open && "rotate-90",
            )}
          />
        )}
      </button>
      {open && file.diff && (
        <pre className="veltrix-thin-scrollbar max-h-72 overflow-auto border-t bg-background/40 px-2 py-1 font-mono text-[10px] leading-relaxed">
          {file.diff.split("\n").map((line, i) => (
            <div
              key={i}
              className={cn(
                "whitespace-pre",
                line.startsWith("@@")
                  ? "text-sky-600 dark:text-sky-400"
                  : line.startsWith("+")
                    ? "text-emerald-600 dark:text-emerald-400"
                    : line.startsWith("-")
                      ? "text-red-600 dark:text-red-400"
                      : "text-muted-foreground",
              )}
            >
              {line || " "}
            </div>
          ))}
        </pre>
      )}
    </div>
  );
}

// 单条消息渲染:
// - user → 右侧气泡
// - assistant → 思考过程 / 最终回答文字全量呈现(Markdown)+ 编码类工具折叠块;其余工具调用隐藏
// - tool(工具结果)→ 对话中不呈现(run_command 的输出仍在「终端」tab)
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
  if (m.role !== "assistant") return null;

  const hasText = m.content.trim().length > 0;
  const hasReasoning = !!m.reasoning?.trim();
  // 仅保留写代码类工具(write_file / replace_in_file)折叠展示;读取 / 列目录 / 跑命令 / 搜索 / 计划等隐藏
  const codeCalls = parseToolCalls(m.toolCalls).filter((c) =>
    (CODE_TOOLS as readonly string[]).includes(c.name),
  );
  // 既无文字、无编码、也无思考过程 → 纯工具调用步骤,整条隐藏
  if (!hasText && codeCalls.length === 0 && !hasReasoning) return null;
  return (
    <div className="space-y-1.5">
      {hasReasoning && <ReasoningBlock reasoning={m.reasoning ?? ""} />}
      {hasText && (
        <div className="text-sm text-foreground">
          <MarkdownMessage content={m.content} />
        </div>
      )}
      {codeCalls.map((c) => {
        const isWrite = c.name === "write_file";
        const args = c.arguments ?? {};
        const code = String((isWrite ? args.content : args.diff) ?? "");
        return (
          <CollapsibleCode
            key={c.id}
            label={isWrite ? "写入文件" : "修改文件"}
            path={String(args.path ?? "")}
            code={code}
          />
        );
      })}
    </div>
  );
}
