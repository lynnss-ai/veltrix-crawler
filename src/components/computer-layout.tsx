// 电脑操作 Agent 页面(左对话 + 右桌面截图预览双栏)。
// 工具集聚合桌面/文件/进程/OCR/UIA/HTTP/终端;右栏不是内嵌 webview,而是定时拉桌面截图(capture_desktop_screenshot)显示。
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { ChevronDown, Loader2, Monitor, Pencil, Send, Trash2 } from "lucide-react";
import { toast } from "sonner";

import { api, type ChatMessageView } from "@/lib/api";
import { useChat } from "@/hooks/use-chat";
import { MarkdownMessage } from "@/components/MarkdownMessage";
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

// 危险工具(与后端 computer::tools::DANGEROUS_TOOLS 对应),消息里标红提示
const DANGEROUS = new Set([
  "delete_path",
  "kill_process",
  "write_file",
  "move_path",
  "control_window",
  "launch_program",
  "click_control",
]);

// 电脑操作工具调用的简要参数(单行)
function briefArgs(name: string, args: Record<string, unknown>): string {
  const s = (k: string) => String(args[k] ?? "");
  switch (name) {
    case "run_command":
      return s("command");
    case "type_text":
      return s("text");
    case "press_keys":
      return s("keys");
    case "mouse_click":
      return `${args.button ?? "left"}${args.x != null ? ` @(${args.x},${args.y})` : ""}`;
    case "find_control":
    case "click_control":
    case "focus_window":
      return String(args.title ?? args.name ?? "");
    case "control_window":
      return `${s("action")} ${s("title")}`;
    case "read_file":
    case "write_file":
    case "list_dir":
    case "file_info":
    case "delete_path":
    case "make_dir":
      return s("path");
    case "copy_file":
    case "move_path":
      return `${s("src")} → ${s("dest")}`;
    case "find_files":
      return `${s("root")} ${args.name_contains ?? args.extension ?? ""}`;
    case "kill_process":
      return `PID ${s("pid")}`;
    case "find_process":
    case "list_processes":
      return String(args.name ?? args.name_contains ?? "");
    case "get_env":
    case "which":
      return s("name");
    case "http_request":
      return `${args.method ?? "GET"} ${s("url")}`;
    case "launch_program":
      return s("program");
    case "open_path":
      return s("target");
    default:
      return Object.keys(args).length === 0 ? "" : JSON.stringify(args);
  }
}

export function ComputerLayout() {
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

  const [messages, setMessages] = useState<ChatMessageView[]>([]);
  const [input, setInput] = useState("");
  const [sending, setSending] = useState(false);
  const [steps, setSteps] = useState<string[]>([]);
  const [renameOpen, setRenameOpen] = useState(false);
  const [renameValue, setRenameValue] = useState("");
  const [deleteOpen, setDeleteOpen] = useState(false);
  // 电脑操作走 ReAct + function calling,只列具备「工具调用」能力的模型(看屏幕还需 vision,优先选两者都有的)
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

  // 交接的首条消息:建好 computer 会话后自动发送一次
  useEffect(() => {
    if (pendingFirstMessage && activeId) {
      const msg = pendingFirstMessage;
      setPendingFirstMessage(null);
      void doSend(msg);
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [pendingFirstMessage, activeId]);

  // Agent 进度事件(仅当前发送会话)
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

  useEffect(() => {
    const el = scrollRef.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, [messages, steps, sending]);

  async function doSend(text: string) {
    if (dispatchingRef.current || !text || sending) return;
    if (!activeId && models.length === 0) {
      toast.error("尚无可用模型:电脑操作 Agent 需具备「工具调用」能力的模型,请到系统配置 → 模型厂商勾选");
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
          "computer",
        );
        convId = conv.id;
        setActiveId(conv.id);
      }
      sendingConvRef.current = convId;
      await api.sendComputerMessage(convId, text);
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

      <AlertDialog open={deleteOpen} onOpenChange={setDeleteOpen}>
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>删除会话</AlertDialogTitle>
            <AlertDialogDescription>
              确定删除「{active?.title || "新电脑操作会话"}」?此操作不可恢复。
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

      {/* 左:对话 / 步骤 */}
      <div className="flex min-h-0 min-w-0 flex-col" style={{ width: "46%" }}>
        <div className="flex shrink-0 items-center justify-between gap-2 px-4 py-2">
          {active ? (
            <DropdownMenu>
              <DropdownMenuTrigger asChild>
                <button
                  type="button"
                  className="inline-flex max-w-[60%] items-center gap-1 rounded-md px-1.5 py-1 text-sm font-medium text-foreground transition-colors hover:bg-accent"
                >
                  <span className="truncate">{active.title || "新电脑操作会话"}</span>
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
              新电脑操作会话
            </span>
          )}
          <span className="shrink-0 rounded bg-amber-500/15 px-1.5 py-0.5 text-[10px] text-amber-600 dark:text-amber-400">
            实验
          </span>
        </div>

        <div
          ref={scrollRef}
          className="veltrix-thin-scrollbar min-h-0 flex-1 space-y-2.5 overflow-y-auto px-5 py-3"
        >
          {messages.length === 0 && !sending ? (
            <EmptyState
              icon={Monitor}
              title="电脑操作 Agent"
              description="描述要在这台电脑上做的事(截图看屏幕、点按钮、敲命令、查/改文件、管进程…)。我会亲自操作,右侧实时显示桌面画面。"
            />
          ) : (
            messages.map((m) => <ComputerMessage key={m.id} message={m} />)
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

        <div className="shrink-0 px-4 pb-3">
          <div className="flex items-end gap-2 rounded-2xl border bg-card p-2 shadow-lg">
            <Textarea
              value={input}
              onChange={(e) => setInput(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter" && !e.shiftKey) {
                  e.preventDefault();
                  if (!sending) void doSend(input.trim());
                }
              }}
              placeholder="描述电脑操作,Enter 发送 / Shift+Enter 换行"
              className="veltrix-thin-scrollbar max-h-52 min-h-10 w-full resize-none border-0 bg-transparent px-2 py-2 text-[15px] leading-6 shadow-none focus-visible:ring-0 dark:bg-transparent"
              rows={1}
            />
            <Button
              type="button"
              size="icon"
              className="size-9 shrink-0 cursor-pointer rounded-xl"
              disabled={sending || !input.trim()}
              onClick={() => void doSend(input.trim())}
            >
              {sending ? <Loader2 className="animate-spin" /> : <Send />}
            </Button>
          </div>
        </div>
      </div>

      <div className="w-px shrink-0 bg-border" />

      {/* 右:桌面截图预览 */}
      <DesktopPreview sending={sending} />
    </div>
  );
}

// 桌面截图预览:执行中定时拉截图(capture_desktop_screenshot),空闲只在发送结束后截一次最终态。
function DesktopPreview({ sending }: { sending: boolean }) {
  const [shot, setShot] = useState<string | null>(null);
  const inFlight = useRef(false);

  const capture = useCallback(async () => {
    if (inFlight.current) return;
    inFlight.current = true;
    try {
      const url = await api.captureDesktopScreenshot();
      setShot(url);
    } catch {
      // 截屏失败(无权限等)静默忽略,保留上一帧
    } finally {
      inFlight.current = false;
    }
  }, []);

  useEffect(() => {
    // 空闲:截一次最终态即可,不持续轮询(全屏 base64 较重)
    if (!sending) {
      void capture();
      return;
    }
    // 执行中:1.5s 轮询展示实时画面
    void capture();
    const timer = window.setInterval(() => void capture(), 1500);
    return () => clearInterval(timer);
  }, [sending, capture]);

  return (
    <div className="relative min-h-0 flex-1 bg-muted/20">
      {shot ? (
        <img
          src={shot}
          alt="桌面画面"
          className="absolute inset-0 h-full w-full object-contain"
        />
      ) : (
        <div className="pointer-events-none absolute inset-0 flex items-center justify-center px-8 text-center text-xs text-muted-foreground">
          正在获取桌面画面…(macOS 需授予屏幕录制权限)
        </div>
      )}
    </div>
  );
}

// 单条消息:user 气泡 / assistant 文本+工具调用 / tool 结果行
function ComputerMessage({ message: m }: { message: ChatMessageView }) {
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
    const firstLine = (m.content || "").split("\n")[0].trim().slice(0, 120);
    const failed = /未找到|超时|失败|未执行|不能为空|缺少参数|拒绝|不存在|异常/.test(firstLine);
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
      {m.content.trim() && <MarkdownMessage content={m.content} />}
      {calls.map((c) => {
        const danger = DANGEROUS.has(c.name);
        return (
          <div
            key={c.id}
            className={`flex items-start gap-1.5 rounded-md border px-2 py-1 text-xs ${
              danger ? "border-amber-500/40 bg-amber-500/10" : "bg-muted/30"
            }`}
          >
            <span
              className={`shrink-0 font-medium ${danger ? "text-amber-600 dark:text-amber-400" : "text-primary"}`}
            >
              {danger ? "⚠ " : ""}
              {c.name}
            </span>
            <span className="min-w-0 break-all font-mono text-muted-foreground">
              {briefArgs(c.name, c.arguments ?? {})}
            </span>
          </div>
        );
      })}
    </div>
  );
}
