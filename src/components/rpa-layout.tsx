// 浏览器 / RPA Agent 页面(左对话 + 右内嵌真实 webview 双栏)。
// 右栏不再是截图伪预览:后端经 Window::add_child 把真实可交互 webview 贴到 AgentWebviewHost
// 占位 div 的区域(前端按 DOM rect 同步 bounds);底部拦截面板实时显示页面发出的接口响应。
import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import { convertFileSrc } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import {
  ChevronDown,
  ChevronRight,
  Globe,
  Loader2,
  Network,
  Pencil,
  Plus,
  Send,
  Trash2,
  Video,
} from "lucide-react";
import { toast } from "sonner";

import { api, type ChatMessageView, type NetworkEntryView } from "@/lib/api";
import { useChat } from "@/hooks/use-chat";
import { useAgentStepListener } from "@/hooks/use-agent-step-listener";
import { useScreenRecording } from "@/hooks/use-screen-recording";
import { MarkdownMessage } from "@/components/MarkdownMessage";
import { ReasoningBlock } from "@/components/ReasoningBlock";
import { RecordingChip } from "@/components/RecordingChip";
import { EmptyState } from "@/components/EmptyState";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Textarea } from "@/components/ui/textarea";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import {
  Dialog,
  DialogContent,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
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

interface ToolCallJson {
  id: string;
  name: string;
  arguments: Record<string, unknown>;
}

function parseToolCalls(json: string | null | undefined): ToolCallJson[] {
  if (!json) return [];
  try {
    const arr = JSON.parse(json) as ToolCallJson[];
    return Array.isArray(arr) ? arr : [];
  } catch {
    return [];
  }
}

// 浏览器工具调用的简要参数(单行)
function briefArgs(name: string, args: Record<string, unknown>): string {
  if (name === "navigate") return String(args.url ?? "");
  if (name === "click") return String(args.selector ?? "");
  if (name === "type")
    return `${String(args.selector ?? "")} ← ${String(args.text ?? "")}`;
  if (name === "get_network") return String(args.url_contains ?? "(全部)");
  return JSON.stringify(args);
}

// 消息列表初始只渲染最近 N 条(长 ReAct 对话切换时避免一次性解析全部),可「加载更早」
const MSG_PAGE_SIZE = 30;

export function RpaLayout() {
  const {
    conversations,
    activeId,
    setActiveId,
    providers,
    pendingFirstMessage,
    setPendingFirstMessage,
    reload,
  } = useChat();
  const active = conversations.find((c) => c.id === activeId) ?? null;
  const pendingRef = useRef(pendingFirstMessage);
  pendingRef.current = pendingFirstMessage;
  // 抑制「本次发送自建的新会话」触发加载 effect:setActiveId 会让加载与发送抢跑致首条消息重复/闪烁
  const skipLoadRef = useRef<string | null>(null);

  const [messages, setMessages] = useState<ChatMessageView[]>([]);
  // 当前会话只渲染最近 visibleCount 条;切会话重置,「加载更早」时增加
  const [visibleCount, setVisibleCount] = useState(MSG_PAGE_SIZE);
  const [input, setInput] = useState("");
  const [sending, setSending] = useState(false);
  const [steps, setSteps] = useState<string[]>([]);
  // 会话标题操作(与普通对话一致):重命名弹框 + 删除二次确认
  const [renameOpen, setRenameOpen] = useState(false);
  const [renameValue, setRenameValue] = useState("");
  const [deleteOpen, setDeleteOpen] = useState(false);
  // 屏幕录制:加号点击打开悬浮控制条;停止后视频先「挂起」到输入区,发送时才一并加入对话
  const [pendingRecording, setPendingRecording] = useState<string | null>(null);
  const { openOverlay: openScreenRecording } =
    useScreenRecording(handleRecordingSaved);
  // 浏览器(RPA)智能体走 ReAct + function calling,只列具备「工具调用」能力的模型。
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
  const scrollRef = useRef<HTMLDivElement>(null);
  const sendingConvRef = useRef<string | null>(null);
  const dispatchingRef = useRef(false);

  // 切会话加载消息(交接时跳过,交给发送流程)
  useEffect(() => {
    setVisibleCount(MSG_PAGE_SIZE); // 切会话回到「只渲染最近 N 条」
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

  // 交接的首条消息:建好 rpa 会话后自动发送一次
  useEffect(() => {
    if (pendingFirstMessage && activeId) {
      const msg = pendingFirstMessage;
      setPendingFirstMessage(null);
      void doSend(msg);
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [pendingFirstMessage, activeId]);

  // Agent 进度事件(仅当前发送会话):统一走 useAgentStepListener
  useAgentStepListener(sendingConvRef, setSteps);

  useEffect(() => {
    const el = scrollRef.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, [messages, steps, sending]);

  // 录屏停止后:把视频「挂起」到输入区(待发送),由用户决定何时连同指令一并加入对话
  function handleRecordingSaved(path: string) {
    setPendingRecording(path);
    toast.success("屏幕录制已就绪,将随下条消息一并加入对话");
  }

  // 仅发送待发送的录屏(输入框无文字时):落库视频消息,不触发 Agent 回合
  async function sendRecordingOnly() {
    const recording = pendingRecording;
    if (!recording) return;
    try {
      let convId = activeId;
      if (!convId) {
        if (models.length === 0) {
          toast.error("尚无可用模型:请先配置模型或开始一个对话,再录屏");
          return;
        }
        const conv = await api.createConversation(
          crypto.randomUUID(),
          models[0].providerId,
          models[0].model,
          "rpa",
        );
        convId = conv.id;
        skipLoadRef.current = conv.id;
        setActiveId(conv.id);
      }
      const msg = await api.attachRecordingMessage(convId, recording);
      setMessages((prev) => [...prev, msg]);
      setPendingRecording(null);
      await reload();
    } catch (e) {
      toast.error(`添加录屏到对话失败: ${e}`);
    }
  }

  // 发送:有文字走 Agent 回合(回合内会先落库待发送录屏);仅录屏无文字则只落库视频
  function handleSend() {
    const text = input.trim();
    if (text) {
      void doSend(text);
    } else if (pendingRecording) {
      void sendRecordingOnly();
    }
  }

  async function doSend(text: string) {
    if (dispatchingRef.current || !text || sending) return;
    const recording = pendingRecording; // 本次随消息一并发送的待发送录屏(若有)
    if (!activeId && models.length === 0) {
      toast.error("尚无可用模型:浏览器 Agent 需具备「工具调用」能力的模型,请到系统配置 → 模型厂商勾选");
      return;
    }
    dispatchingRef.current = true;
    setSending(true);
    setSteps([]);
    setInput("");
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
          "rpa",
        );
        convId = conv.id;
        skipLoadRef.current = conv.id; // 抑制 setActiveId 触发的本会话加载,防首条消息重复
        setActiveId(conv.id);
      }
      // 待发送录屏:先落库为一条视频消息(排在本轮指令之前),清除挂起态
      if (recording) {
        await api.attachRecordingMessage(convId, recording);
        setPendingRecording(null);
      }
      sendingConvRef.current = convId;
      await api.sendBrowserMessage(convId, text);
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

  return (
    <div className="flex min-h-0 min-w-0 flex-1 overflow-hidden">
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
              确定删除「{active?.title || "新浏览器会话"}」?此操作不可恢复。
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

      {/* 左:对话 / 步骤(占比固定,右栏为内嵌真实 webview) */}
      <div className="flex min-h-0 min-w-0 flex-col" style={{ width: "42%" }}>
        {/* 会话标题:与普通对话一致——无分隔栏 + 可点下拉(重命名/删除);保留「实验」标识 */}
        <div className="flex shrink-0 items-center justify-between gap-2 px-4 py-2">
          {active ? (
            <DropdownMenu>
              <DropdownMenuTrigger asChild>
                <button
                  type="button"
                  className="inline-flex max-w-[60%] items-center gap-1 rounded-md px-1.5 py-1 text-sm font-medium text-foreground transition-colors hover:bg-accent"
                >
                  <span className="truncate">{active.title || "新浏览器会话"}</span>
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
          ) : (
            <span className="px-1.5 py-1 text-sm font-medium text-muted-foreground">
              新浏览器会话
            </span>
          )}
          <span className="shrink-0 rounded bg-amber-500/15 px-1.5 py-0.5 text-[10px] text-amber-600 dark:text-amber-400">
            实验
          </span>
        </div>

        {/* 消息流 */}
        <div
          ref={scrollRef}
          className="veltrix-thin-scrollbar min-h-0 flex-1 space-y-2.5 overflow-y-auto px-5 py-3"
        >
          {messages.length === 0 && !sending ? (
            <EmptyState
              icon={Globe}
              title="浏览器 Agent"
              description="描述要在浏览器里做的操作(导航 / 点击 / 输入 / 看接口)。我会在右侧内嵌浏览器中亲自操作,并实时回读页面与接口数据。"
            />
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
                <RpaMessage key={m.id} message={m} />
              ))}
            </>
          )}
          {sending && (
            <div className="space-y-1">
              {steps.slice(-6).map((s, i) => (
                <div key={i} className="flex items-center gap-1.5 text-xs text-muted-foreground">
                  <span className="text-emerald-500">·</span>
                  {s}
                </div>
              ))}
              <div className="inline-flex items-center gap-2 rounded-lg bg-muted px-3 py-2 text-sm text-muted-foreground">
                <Loader2 className="size-4 animate-spin" />
                执行中…
              </div>
            </div>
          )}
        </div>

        {/* 输入 */}
        <div className="shrink-0 px-4 pb-3">
          <div className="flex flex-col gap-1 rounded-2xl border bg-card p-2 shadow-lg">
            {pendingRecording && (
              <RecordingChip onRemove={() => setPendingRecording(null)} />
            )}
            <Textarea
              value={input}
              onChange={(e) => setInput(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter" && !e.shiftKey) {
                  e.preventDefault();
                  if (!sending) handleSend();
                }
              }}
              placeholder="描述浏览器操作,Enter 发送 / Shift+Enter 换行"
              className="veltrix-thin-scrollbar max-h-52 min-h-10 w-full resize-none border-0 bg-transparent px-2 py-2 text-[15px] leading-6 shadow-none focus-visible:ring-0 dark:bg-transparent"
              rows={1}
            />
            {/* 第二行:左侧加号(更多功能,含录屏)/ 右侧发送 */}
            <div className="flex items-center justify-between gap-2">
              <DropdownMenu>
                <DropdownMenuTrigger asChild>
                  <Button
                    type="button"
                    variant="ghost"
                    size="icon"
                    className="size-9 shrink-0 cursor-pointer rounded-xl"
                  >
                    <Plus />
                  </Button>
                </DropdownMenuTrigger>
                <DropdownMenuContent align="start" side="top">
                  <DropdownMenuItem onClick={() => void openScreenRecording()}>
                    <Video className="size-4" />
                    屏幕录制
                  </DropdownMenuItem>
                </DropdownMenuContent>
              </DropdownMenu>
              <Button
                type="button"
                size="icon"
                className="size-9 shrink-0 cursor-pointer rounded-xl"
                disabled={sending || (!input.trim() && !pendingRecording)}
                onClick={() => handleSend()}
              >
                {sending ? <Loader2 className="animate-spin" /> : <Send />}
              </Button>
            </div>
          </div>
        </div>
      </div>

      {/* 分割线 */}
      <div className="w-px shrink-0 bg-border" />

      {/* 右:内嵌真实 webview(上)+ 接口拦截面板(下) */}
      <div className="flex min-h-0 min-w-0 flex-1 flex-col">
        <AgentWebviewHost activeId={activeId} sending={sending} />
        <NetworkPanel activeId={activeId} />
      </div>
    </div>
  );
}

// 内嵌浏览器宿主:本身只是个占位区域,真实 webview 由后端 add_child 后按本区域 DOM rect 定位覆盖。
// 负责把区域坐标(挂载 / 窗口缩放 / 区域变化 / 后端就绪事件时)同步给后端,并控制显隐。
function AgentWebviewHost({
  activeId,
  sending,
}: {
  activeId: string | null;
  sending: boolean;
}) {
  const hostRef = useRef<HTMLDivElement>(null);
  const rafRef = useRef<number | null>(null);
  const activeRef = useRef(activeId);
  activeRef.current = activeId;

  // 把当前区域逻辑坐标(相对视口=主窗口客户区,因 decorations:false + 标题栏在 DOM 内)同步给后端。
  // 不乘 devicePixelRatio:Tauri 按 scale_factor 自动换算物理像素。
  const syncBounds = useCallback(() => {
    const id = activeRef.current;
    const el = hostRef.current;
    if (!id || !el) return;
    const r = el.getBoundingClientRect();
    void api.setAgentWebviewBounds(id, r.left, r.top, r.width, r.height).catch((e) => console.debug("设置 webview 边界失败:", e));
  }, []);

  // rAF 节流,避免拖动 / 连续 resize 时频繁 IPC 抖动
  const scheduleSync = useCallback(() => {
    if (rafRef.current != null) cancelAnimationFrame(rafRef.current);
    rafRef.current = requestAnimationFrame(() => {
      rafRef.current = null;
      syncBounds();
    });
  }, [syncBounds]);

  // 挂载 / 切会话:定位并显示当前会话 webview;ResizeObserver + window resize 持续同步;
  // 清理时藏掉(切走的会话或离开页面),避免原生层盖住别处。
  useLayoutEffect(() => {
    scheduleSync();
    if (activeId) void api.showAgentWebview(activeId).catch((e) => console.debug("显示 webview 失败:", e));
    const ro = new ResizeObserver(() => scheduleSync());
    if (hostRef.current) ro.observe(hostRef.current);
    window.addEventListener("resize", scheduleSync);
    return () => {
      ro.disconnect();
      window.removeEventListener("resize", scheduleSync);
      if (activeId) void api.hideAgentWebview(activeId).catch((e) => console.debug("隐藏 webview 失败:", e));
    };
  }, [activeId, scheduleSync]);

  // 发送结束后 webview 多半已被创建:重定位并显示
  useEffect(() => {
    if (!sending) {
      scheduleSync();
      if (activeRef.current) void api.showAgentWebview(activeRef.current).catch((e) => console.debug("显示 webview 失败:", e));
    }
  }, [sending, scheduleSync]);

  // 后端新建 webview 后通知(首次 navigate):定位并显示
  useEffect(() => {
    let dispose: (() => void) | undefined;
    let disposed = false;
    listen<{ conversationId: string }>("agent-webview-ready", (e) => {
      if (e.payload.conversationId !== activeRef.current) return;
      scheduleSync();
      void api.showAgentWebview(e.payload.conversationId).catch((e) => console.debug("显示新 webview 失败:", e));
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
  }, [scheduleSync]);

  // 离开 RPA 工作区(组件卸载):藏掉全部内嵌 webview,防原生层盖住其它页面
  useEffect(() => {
    return () => {
      void api.hideAllAgentWebviews().catch((e) => console.debug("隐藏全部 webview 失败:", e));
    };
  }, []);

  return (
    <div className="relative min-h-0 flex-1 bg-muted/20">
      {/* webview 覆盖此区域;占位 div 提供定位坐标 */}
      <div ref={hostRef} className="absolute inset-0" />
      {!activeId && (
        <div className="pointer-events-none absolute inset-0 flex items-center justify-center px-8 text-center text-xs text-muted-foreground">
          发送一条操作指令(如「打开 example.com 搜索 xxx」),Agent 会在这里的内嵌浏览器中实时操作。
        </div>
      )}
    </div>
  );
}

// 一条拦截记录(本地加自增 id 以稳定 React key / 展开态,后端响应本身无唯一标识)
interface NetRow {
  id: number;
  url: string;
  body: string;
}

// 接口拦截面板:实时显示内嵌 webview 发出的 JSON 响应(agent-network 事件增量 + 切会话回填),
// 支持 URL 关键词过滤、点行展开响应体。默认收起把空间让给 webview。
function NetworkPanel({ activeId }: { activeId: string | null }) {
  const [open, setOpen] = useState(false);
  const [rows, setRows] = useState<NetRow[]>([]);
  const [filter, setFilter] = useState("");
  const [expanded, setExpanded] = useState<number | null>(null);
  const idRef = useRef(0);
  const activeRef = useRef(activeId);
  activeRef.current = activeId;

  const toRows = useCallback(
    (list: NetworkEntryView[]): NetRow[] =>
      list.map((e) => ({ id: idRef.current++, url: e.url, body: e.body })),
    [],
  );

  // 切会话:清空并回填该会话已拦截的响应
  useEffect(() => {
    setRows([]);
    setExpanded(null);
    if (!activeId) return;
    api
      .getAgentNetwork(activeId)
      .then((list) => setRows(toRows(list)))
      .catch((e) => console.debug("加载网络拦截记录失败:", e));
  }, [activeId, toRows]);

  // 实时增量:监听 agent-network(仅当前会话),保留最近 200 条
  useEffect(() => {
    let dispose: (() => void) | undefined;
    let disposed = false;
    listen<{ conversationId: string; url: string; body: string }>("agent-network", (e) => {
      if (e.payload.conversationId !== activeRef.current) return;
      setRows((prev) => {
        const next = [...prev, { id: idRef.current++, url: e.payload.url, body: e.payload.body }];
        return next.length > 200 ? next.slice(next.length - 200) : next;
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

  // 过滤 + 最新在上
  const shown = useMemo(() => {
    const f = filter.trim().toLowerCase();
    const list = f ? rows.filter((r) => r.url.toLowerCase().includes(f)) : rows;
    return list.slice().reverse();
  }, [rows, filter]);

  return (
    <div className="flex shrink-0 flex-col border-t">
      <button
        type="button"
        onClick={() => setOpen((v) => !v)}
        className="flex items-center gap-1.5 px-3 py-2 text-xs text-muted-foreground hover:bg-muted/40"
      >
        {open ? <ChevronDown className="size-3.5" /> : <ChevronRight className="size-3.5" />}
        <Network className="size-3.5 text-primary" />
        <span className="font-medium text-foreground">接口拦截</span>
        <span className="rounded bg-muted px-1.5 py-0.5 tabular-nums">{rows.length}</span>
      </button>
      {open && (
        <div className="flex h-56 flex-col gap-2 px-3 pb-3">
          <Input
            value={filter}
            onChange={(e) => setFilter(e.target.value)}
            placeholder="按 URL 关键词过滤,如 api / search"
            className="h-7 text-xs"
          />
          <div className="veltrix-thin-scrollbar min-h-0 flex-1 space-y-1 overflow-y-auto">
            {shown.length === 0 ? (
              <div className="px-1 py-6 text-center text-xs text-muted-foreground">
                {rows.length === 0
                  ? "暂无拦截到的 JSON 接口响应。Agent 打开页面、触发数据加载后会实时出现在这里。"
                  : "没有匹配过滤词的接口"}
              </div>
            ) : (
              shown.map((r) => (
                <div key={r.id} className="rounded border bg-card/50">
                  <button
                    type="button"
                    onClick={() => setExpanded((cur) => (cur === r.id ? null : r.id))}
                    className="flex w-full items-start gap-1.5 px-2 py-1 text-left"
                  >
                    <span className="min-w-0 flex-1 break-all font-mono text-[11px] text-foreground">
                      {r.url}
                    </span>
                  </button>
                  {expanded === r.id && (
                    <pre className="veltrix-thin-scrollbar max-h-48 overflow-auto border-t bg-muted/30 px-2 py-1.5 text-[11px] leading-relaxed">
                      {r.body}
                    </pre>
                  )}
                </div>
              ))
            )}
          </div>
        </div>
      )}
    </div>
  );
}

// 单条消息:user 气泡 / assistant 文本+工具调用 / tool 结果行
function RpaMessage({ message: m }: { message: ChatMessageView }) {
  if (m.role === "user") {
    // 视频附件(如屏幕录制):内联播放器;有正文才渲染气泡
    const videos = (m.attachments ?? []).filter((a) =>
      a.mime.startsWith("video/"),
    );
    return (
      <div className="flex flex-col items-end gap-1.5">
        {videos.map((a, i) => (
          <video
            key={i}
            src={a.path ? convertFileSrc(a.path) : ""}
            controls
            preload="metadata"
            className="max-h-72 w-full max-w-md rounded-lg border border-border/60 bg-black"
          />
        ))}
        {m.content.trim() && (
          <div className="max-w-[85%] whitespace-pre-wrap break-words rounded-lg bg-primary/10 px-3 py-2 text-sm text-foreground">
            {m.content}
          </div>
        )}
      </div>
    );
  }
  if (m.role === "tool") {
    // 动作回读结果:取首行摘要展示(read_page 等多行内容只显首行,避免刷屏)。
    // 失败结果(未找到 / 超时 / 失败)用警示色。
    const firstLine = (m.content || "").split("\n")[0].trim().slice(0, 120);
    const failed = /未找到|超时|失败|未执行|不能为空|缺少参数/.test(firstLine);
    return (
      <div className="flex items-start gap-1.5 pl-1 text-[11px] text-muted-foreground">
        <span className={failed ? "text-amber-500" : "text-emerald-500"}>↳</span>
        <span className="shrink-0 font-medium">{m.toolName || "tool"}</span>
        <span className="min-w-0 break-words opacity-80">{firstLine || "(无结果)"}</span>
      </div>
    );
  }
  // assistant
  const calls = parseToolCalls(m.toolCalls);
  return (
    <div className="space-y-1.5">
      {m.reasoning?.trim() && <ReasoningBlock reasoning={m.reasoning} />}
      {m.content.trim() && <MarkdownMessage content={m.content} />}
      {calls.map((c) => (
        <div
          key={c.id}
          className="flex items-start gap-1.5 rounded-md border bg-muted/30 px-2 py-1 text-xs"
        >
          <span className="shrink-0 font-medium text-primary">{c.name}</span>
          <span className="min-w-0 break-all font-mono text-muted-foreground">
            {briefArgs(c.name, c.arguments ?? {})}
          </span>
        </div>
      ))}
    </div>
  );
}
