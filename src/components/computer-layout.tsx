// 电脑操作 Agent 页面(左对话 + 右桌面截图预览双栏)。
// 工具集聚合桌面/文件/进程/OCR/UIA/HTTP/终端;右栏不是内嵌 webview,而是定时拉桌面截图(capture_desktop_screenshot)显示。
import { useEffect, useMemo, useRef, useState } from "react";
import { convertFileSrc } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { ChevronDown, Loader2, Monitor, Pencil, Plus, Send, Trash2, Video } from "lucide-react";
import { toast } from "sonner";

import { api, type ChatMessageView } from "@/lib/api";
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

// 消息列表初始只渲染最近 N 条(长 ReAct 对话切换时避免一次性解析全部),可「加载更早」
const MSG_PAGE_SIZE = 30;

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
  // 抑制「本次发送自建的新会话」触发加载 effect:setActiveId 会让加载与发送抢跑致首条消息重复/闪烁
  const skipLoadRef = useRef<string | null>(null);

  const [messages, setMessages] = useState<ChatMessageView[]>([]);
  // 当前会话只渲染最近 visibleCount 条;切会话重置,「加载更早」时增加
  const [visibleCount, setVisibleCount] = useState(MSG_PAGE_SIZE);
  const [input, setInput] = useState("");
  const [sending, setSending] = useState(false);
  const [steps, setSteps] = useState<string[]>([]);
  // 危险操作待确认:收到 agent-confirm 事件即弹框,用户允许 / 拒绝后回执后端
  const [confirmReq, setConfirmReq] = useState<{
    confirmId: number;
    tool: string;
    args: Record<string, unknown>;
  } | null>(null);
  const [renameOpen, setRenameOpen] = useState(false);
  const [renameValue, setRenameValue] = useState("");
  const [deleteOpen, setDeleteOpen] = useState(false);
  // 屏幕录制:加号点击只打开悬浮控制条;停止后视频先「挂起」到输入区,发送时才一并加入对话
  const [pendingRecording, setPendingRecording] = useState<string | null>(null);
  const { openOverlay: openScreenRecording } =
    useScreenRecording(handleRecordingSaved);
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
  // 待确认 confirm_id:作同步幂等守卫,避免弹窗 onClick 与 onOpenChange 重复回执
  const pendingConfirmRef = useRef<number | null>(null);

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

  // 交接的首条消息:建好 computer 会话后自动发送一次
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

  // 危险操作确认事件(仅当前发送会话):弹框等用户允许 / 拒绝
  useEffect(() => {
    let dispose: (() => void) | undefined;
    let disposed = false;
    listen<{
      conversationId: string;
      confirmId: number;
      tool: string;
      args: Record<string, unknown>;
    }>("agent-confirm", (e) => {
      if (sendingConvRef.current !== e.payload.conversationId) return;
      pendingConfirmRef.current = e.payload.confirmId;
      setConfirmReq({
        confirmId: e.payload.confirmId,
        tool: e.payload.tool,
        args: e.payload.args ?? {},
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

  // 回执危险操作确认:允许则后端执行,拒绝则跳过(后端把「已拒绝」回灌模型)。
  // 用 ref 同步守卫——弹窗的「允许/拒绝」按钮与 onOpenChange 关闭可能各触发一次,取首次即清空。
  async function resolveConfirm(approved: boolean) {
    const id = pendingConfirmRef.current;
    if (id == null) return;
    pendingConfirmRef.current = null;
    setConfirmReq(null);
    try {
      await api.resolveAgentConfirm(id, approved);
    } catch (e) {
      toast.error(`确认失败: ${e}`);
    }
  }

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
          "computer",
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
        skipLoadRef.current = conv.id; // 抑制 setActiveId 触发的本会话加载,防首条消息重复
        setActiveId(conv.id);
      }
      // 待发送录屏:先落库为一条视频消息(排在本轮指令之前),清除挂起态
      if (recording) {
        await api.attachRecordingMessage(convId, recording);
        setPendingRecording(null);
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

      {/* 危险操作确认:Agent 命中删文件 / 结束进程等高危工具时暂停,允许后才真正执行 */}
      <AlertDialog
        open={confirmReq !== null}
        onOpenChange={(open) => {
          if (!open) void resolveConfirm(false);
        }}
      >
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>确认危险操作</AlertDialogTitle>
            <AlertDialogDescription>
              Agent 请求执行高危工具
              <span className="mx-1 rounded bg-muted px-1.5 py-0.5 font-mono text-foreground">
                {confirmReq?.tool}
              </span>
              ,允许后将真正在本机执行,可能不可逆。
            </AlertDialogDescription>
          </AlertDialogHeader>
          {confirmReq && briefArgs(confirmReq.tool, confirmReq.args) && (
            <div className="max-h-32 overflow-auto rounded-md border bg-muted/40 px-3 py-2 font-mono text-xs break-all text-muted-foreground">
              {briefArgs(confirmReq.tool, confirmReq.args)}
            </div>
          )}
          <AlertDialogFooter>
            <AlertDialogCancel onClick={() => void resolveConfirm(false)}>
              拒绝
            </AlertDialogCancel>
            <AlertDialogAction
              className="bg-destructive text-white hover:bg-destructive/90"
              onClick={() => void resolveConfirm(true)}
            >
              允许执行
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>

      {/* 对话 / 步骤(全宽,内容居中收窄便于阅读) */}
      <div className="flex min-h-0 min-w-0 flex-1 flex-col">
        <div className="mx-auto flex w-full max-w-3xl shrink-0 items-center justify-between gap-2 px-4 py-2">
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
          className="veltrix-thin-scrollbar mx-auto min-h-0 w-full max-w-3xl flex-1 space-y-2.5 overflow-y-auto px-5 py-3"
        >
          {messages.length === 0 && !sending ? (
            <EmptyState
              icon={Monitor}
              title="电脑操作 Agent"
              description="描述要在这台电脑上做的事(截图看屏幕、点按钮、敲命令、查/改文件、管进程…)。我会在这台电脑上亲自操作。"
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
                <ComputerMessage key={m.id} message={m} />
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

        <div className="mx-auto w-full max-w-3xl shrink-0 px-4 pb-3">
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
              placeholder="描述电脑操作,Enter 发送 / Shift+Enter 换行"
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
    </div>
  );
}

// 单条消息:user 气泡 / assistant 文本+工具调用 / tool 结果行
function ComputerMessage({ message: m }: { message: ChatMessageView }) {
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
      {m.reasoning?.trim() && <ReasoningBlock reasoning={m.reasoning} />}
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
