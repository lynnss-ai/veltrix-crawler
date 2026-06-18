// 浏览器 / RPA Agent 页面(单栏对话,MVP)。发送走 send_browser_message(navigate/click/type)。
// 当前为 fire-and-forget(不回读页面内容,无截图/预览),后续接入取 DOM + 截图后再做双栏。
import { useEffect, useMemo, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { Globe, Loader2, Monitor, RotateCw, Send } from "lucide-react";
import { toast } from "sonner";

import { api, type ChatMessageView } from "@/lib/api";
import { useChat } from "@/hooks/use-chat";
import { MarkdownMessage } from "@/components/MarkdownMessage";
import { EmptyState } from "@/components/EmptyState";
import { Button } from "@/components/ui/button";
import { Textarea } from "@/components/ui/textarea";
import { cn } from "@/lib/utils";

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
  return JSON.stringify(args);
}

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

  const [messages, setMessages] = useState<ChatMessageView[]>([]);
  const [input, setInput] = useState("");
  const [sending, setSending] = useState(false);
  const [steps, setSteps] = useState<string[]>([]);
  // 预览刷新信号:每次发送结束 +1,让右栏预览立刻抓一帧反映最新页面
  const [previewRefresh, setPreviewRefresh] = useState(0);
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

  // 交接的首条消息:建好 rpa 会话后自动发送一次
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
      toast.error("尚无可用模型,请先到系统配置 → 模型厂商填好 API Key 与模型");
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
        setActiveId(conv.id);
      }
      sendingConvRef.current = convId;
      await api.sendBrowserMessage(convId, text);
      const fresh = await api.listChatMessages(convId);
      setMessages(fresh);
      await reload();
      setPreviewRefresh((k) => k + 1); // 操作完成,刷新预览
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

  return (
    <div className="flex min-h-0 min-w-0 flex-1 overflow-hidden">
      {/* 左:对话 / 步骤(占比固定,右栏为实时预览) */}
      <div className="flex min-h-0 min-w-0 flex-col" style={{ width: "46%" }}>
        {/* 头 */}
        <div className="flex shrink-0 items-center gap-2 border-b px-4 py-2 text-sm font-medium text-foreground">
          <Globe className="size-4 text-primary" />
          <span className="truncate">{active?.title || "新浏览器会话"}</span>
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
              description="描述要在浏览器里做的操作(导航 / 点击 / 输入)。我会读取页面、确认命中,右侧实时预览操作过程。"
            />
          ) : (
            messages.map((m) => <RpaMessage key={m.id} message={m} />)
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
              placeholder="描述浏览器操作,Enter 发送 / Shift+Enter 换行"
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

      {/* 分割线 */}
      <div className="w-px shrink-0 bg-border" />

      {/* 右:浏览器实时预览(定时截取 Agent 窗口画面) */}
      <BrowserPreview activeId={activeId} refreshSignal={previewRefresh} busy={sending} />
    </div>
  );
}

// 浏览器 Agent 实时预览:定时截取该会话 Agent 窗口画面并展示。
// 窗口在 Agent 首次 navigate 后才创建;此前(或平台不支持截图)显示占位。
function BrowserPreview({
  activeId,
  refreshSignal,
  busy,
}: {
  activeId: string | null;
  refreshSignal: number;
  busy: boolean;
}) {
  const [src, setSrc] = useState<string | null>(null);
  const [everShown, setEverShown] = useState(false);
  // 防重入:上一帧截图(COM 调用)未回来就跳过本次,避免轮询叠加并发
  const inFlightRef = useRef(false);

  useEffect(() => {
    if (!activeId) {
      setSrc(null);
      setEverShown(false);
      return;
    }
    let alive = true;
    const tick = async () => {
      if (inFlightRef.current) return;
      inFlightRef.current = true;
      try {
        const png = await api.captureAgentPreview(activeId);
        if (!alive) return;
        setSrc(png);
        if (png) setEverShown(true);
      } catch {
        // 忽略单帧失败
      } finally {
        inFlightRef.current = false;
      }
    };
    void tick();
    // 执行中刷新更勤(看操作过程),空闲时放缓减少 COM 截图开销
    const timer = setInterval(() => void tick(), busy ? 900 : 2000);
    return () => {
      alive = false;
      clearInterval(timer);
    };
  }, [activeId, refreshSignal, busy]);

  return (
    <div className="flex min-h-0 min-w-0 flex-1 flex-col">
      <div className="flex shrink-0 items-center gap-1.5 border-b px-3 py-2 text-xs text-muted-foreground">
        <Monitor className="size-3.5 text-primary" />
        <span className="font-medium text-foreground">浏览器预览</span>
        <span
          className={cn(
            "ml-1 size-2 rounded-full",
            src ? "bg-emerald-500" : "bg-muted-foreground/40",
          )}
        />
        <span>{src ? "实时" : "未连接"}</span>
        <Button
          type="button"
          size="icon"
          variant="ghost"
          className="ml-auto size-7"
          title="立即刷新预览"
          disabled={!activeId}
          onClick={() => {
            if (!activeId) return;
            void api
              .captureAgentPreview(activeId)
              .then((png) => {
                setSrc(png);
                if (png) setEverShown(true);
              })
              .catch(() => {});
          }}
        >
          <RotateCw className="size-3.5" />
        </Button>
      </div>
      <div className="relative flex min-h-0 flex-1 items-center justify-center overflow-auto bg-muted/20 p-2">
        {src ? (
          <img
            src={src}
            alt="浏览器实时预览"
            className="max-h-full max-w-full rounded border object-contain shadow-sm"
          />
        ) : (
          <div className="px-6 text-center text-xs text-muted-foreground">
            {everShown
              ? "暂时无法获取画面(窗口可能已关闭或最小化)"
              : "浏览器窗口尚未打开。发送一条操作指令(如「打开 example.com」),Agent 导航后这里会实时显示页面画面。"}
          </div>
        )}
      </div>
    </div>
  );
}

// 单条消息:user 气泡 / assistant 文本+工具调用 / tool 结果行
function RpaMessage({ message: m }: { message: ChatMessageView }) {
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
