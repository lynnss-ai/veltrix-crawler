// AI 对话页(对话工作区):会话列表在侧栏,这里只负责消息流 + 文字/语音输入。参考桌面对话体验。
import { useEffect, useMemo, useRef, useState } from "react";
import {
  FileText,
  Image as ImageIcon,
  Loader2,
  Mic,
  Plus,
  Send,
  Sparkles,
  User,
  X,
} from "lucide-react";
import { listen } from "@tauri-apps/api/event";
import { toast } from "sonner";

import {
  api,
  type ChatAttachment,
  type ChatMessageView,
  type ProviderDto,
} from "@/lib/api";
import { Button } from "@/components/ui/button";
import { Textarea } from "@/components/ui/textarea";
import { SimpleTooltip } from "@/components/SimpleTooltip";
import { EmptyState } from "@/components/EmptyState";
import { useChat } from "@/components/chat-context";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";

// 一个可选模型 = 厂商 + 模型名(value 用 "providerId::model" 编码)
interface ModelOption {
  providerId: string;
  providerName: string;
  model: string;
  value: string;
}

// 附件限制:最多 10 个,单个 ≤ 10MB
const MAX_ATTACHMENTS = 10;
const MAX_ATTACHMENT_BYTES = 10 * 1024 * 1024;

// 带本地预览信息的附件(size 仅用于展示)
interface PendingAttachment extends ChatAttachment {
  size: number;
}

function fmtSize(bytes: number): string {
  if (bytes >= 1024 * 1024) return `${(bytes / 1024 / 1024).toFixed(1)}MB`;
  if (bytes >= 1024) return `${Math.round(bytes / 1024)}KB`;
  return `${bytes}B`;
}

// Uint8Array → base64(分块拼接,避免 String.fromCharCode 一次性展开大数组爆栈)
function bytesToBase64(bytes: Uint8Array): string {
  let binary = "";
  const CHUNK = 0x8000; // 32KB 一段
  for (let i = 0; i < bytes.length; i += CHUNK) {
    binary += String.fromCharCode(...bytes.subarray(i, i + CHUNK));
  }
  return btoa(binary);
}

// 从厂商列表展开出可用模型(有 apiKey + models 行才算可用)
function buildModelOptions(providers: ProviderDto[]): ModelOption[] {
  const opts: ModelOption[] = [];
  for (const p of providers) {
    if (!p.apiKey.trim()) continue;
    for (const line of p.models.split("\n")) {
      const model = line.trim();
      if (!model) continue;
      opts.push({
        providerId: p.id,
        providerName: p.name,
        model,
        value: `${p.id}::${model}`,
      });
    }
  }
  return opts;
}

export function ChatPage() {
  // 会话列表 + 当前会话来自侧栏共享的 ChatProvider
  const { conversations, activeId, setActiveId, reload } = useChat();
  const [messages, setMessages] = useState<ChatMessageView[]>([]);
  const [input, setInput] = useState("");
  const [attachments, setAttachments] = useState<PendingAttachment[]>([]);
  const fileInputRef = useRef<HTMLInputElement>(null);
  const [sending, setSending] = useState(false);
  const [models, setModels] = useState<ModelOption[]>([]);
  // 新会话默认模型(value);开新会话时用它
  const [pickedModel, setPickedModel] = useState("");
  // 流式输出:正在生成的 assistant 文本(null=未在生成);streamingConvRef 记录归属会话
  const [streamingContent, setStreamingContent] = useState<string | null>(null);
  const streamingConvRef = useRef<string | null>(null);
  // 录音状态
  const [recording, setRecording] = useState(false);
  const [transcribing, setTranscribing] = useState(false);
  const recorderRef = useRef<MediaRecorder | null>(null);
  const chunksRef = useRef<Blob[]>([]);
  const scrollRef = useRef<HTMLDivElement>(null);

  const active = conversations.find((c) => c.id === activeId) ?? null;

  // 监听后端流式增量事件,按归属会话拼接到 streamingContent(打字机效果)
  useEffect(() => {
    let dispose: (() => void) | undefined;
    let disposed = false;
    listen<{ conversationId: string; delta: string }>("chat-stream", (e) => {
      if (streamingConvRef.current === e.payload.conversationId) {
        setStreamingContent((prev) => (prev ?? "") + e.payload.delta);
      }
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

  // 初始加载可用模型
  useEffect(() => {
    api
      .listProviders()
      .then((ps) => {
        const opts = buildModelOptions(ps);
        setModels(opts);
        if (opts.length > 0) setPickedModel(opts[0].value);
      })
      .catch(() => {});
  }, []);

  // 切换会话时加载消息
  useEffect(() => {
    if (!activeId) {
      setMessages([]);
      return;
    }
    api
      .listChatMessages(activeId)
      .then(setMessages)
      .catch((e) => toast.error(`加载消息失败: ${e}`));
  }, [activeId]);

  // 消息变化 / 流式增量时滚到底部
  useEffect(() => {
    const el = scrollRef.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, [messages, sending, streamingContent]);

  // 当前会话用的模型(已有会话用其绑定的;新会话用 picked)
  const currentModelLabel = useMemo(() => {
    if (active) {
      const opt = models.find(
        (m) => m.providerId === active.providerId && m.model === active.model,
      );
      return opt ? `${opt.providerName} / ${opt.model}` : active.model;
    }
    const opt = models.find((m) => m.value === pickedModel);
    return opt ? `${opt.providerName} / ${opt.model}` : "未配置模型";
  }, [active, models, pickedModel]);

  // 选择附件:校验数量 + 单个大小,读为 base64 加入待发列表
  async function onPickFiles(files: FileList | null) {
    if (!files || files.length === 0) return;
    const incoming = Array.from(files);
    const room = MAX_ATTACHMENTS - attachments.length;
    if (room <= 0) {
      toast.error(`最多上传 ${MAX_ATTACHMENTS} 个附件`);
      return;
    }
    const accepted: PendingAttachment[] = [];
    for (const f of incoming.slice(0, room)) {
      if (f.size > MAX_ATTACHMENT_BYTES) {
        toast.error(`「${f.name}」超过 ${fmtSize(MAX_ATTACHMENT_BYTES)},已跳过`);
        continue;
      }
      try {
        const buf = await f.arrayBuffer();
        accepted.push({
          name: f.name,
          mime: f.type || "application/octet-stream",
          data: bytesToBase64(new Uint8Array(buf)),
          size: f.size,
        });
      } catch {
        toast.error(`读取「${f.name}」失败`);
      }
    }
    if (incoming.length > room) {
      toast.error(`最多 ${MAX_ATTACHMENTS} 个,超出部分未添加`);
    }
    if (accepted.length > 0) {
      setAttachments((prev) => [...prev, ...accepted]);
    }
  }

  function removeAttachment(idx: number) {
    setAttachments((prev) => prev.filter((_, i) => i !== idx));
  }

  // 按类型打开文件选择:图片(accept=image/*)或任意文件
  function openPicker(accept: string) {
    const el = fileInputRef.current;
    if (!el) return;
    el.accept = accept;
    el.click();
  }

  async function handleSend() {
    const text = input.trim();
    if ((!text && attachments.length === 0) || sending) return;
    if (models.length === 0) {
      toast.error("尚无可用模型,请先到系统配置 → 模型厂商填好 API Key 与模型");
      return;
    }

    setSending(true);
    const sentAttachments = attachments;
    // 乐观追加用户消息(正文 + 附件名提示,与后端落库口径一致)
    const optimisticContent =
      sentAttachments.length > 0
        ? [text, ...sentAttachments.map((a) => `[附件: ${a.name}]`)]
            .filter(Boolean)
            .join("\n")
        : text;
    const optimistic: ChatMessageView = {
      id: Date.now(),
      conversationId: activeId ?? "",
      role: "user",
      content: optimisticContent,
      createdAt: Math.floor(Date.now() / 1000),
    };
    setMessages((prev) => [...prev, optimistic]);
    setInput("");
    setAttachments([]);

    try {
      let convId = activeId;
      // 新会话:先按所选模型建会话
      if (!convId) {
        const opt = models.find((m) => m.value === pickedModel) ?? models[0];
        const id = crypto.randomUUID();
        const conv = await api.createConversation(id, opt.providerId, opt.model);
        convId = conv.id;
        setActiveId(conv.id);
      }
      // 标记本次流式归属会话,准备接收增量
      streamingConvRef.current = convId;
      setStreamingContent("");
      const reply = await api.sendChatMessageStream(
        convId,
        text,
        sentAttachments.map((a) => ({
          name: a.name,
          mime: a.mime,
          data: a.data,
        })),
      );
      setMessages((prev) => [...prev, reply]);
      // 刷新侧栏会话列表(标题可能由首句生成 + 排序更新)
      await reload();
    } catch (e) {
      toast.error(`发送失败: ${e}`);
      // 回滚乐观消息与附件
      setMessages((prev) => prev.filter((m) => m.id !== optimistic.id));
      setInput(text);
      setAttachments(sentAttachments);
    } finally {
      streamingConvRef.current = null;
      setStreamingContent(null);
      setSending(false);
    }
  }

  // 语音输入:开始/停止录音;停止后转写并填入输入框
  async function toggleRecording() {
    if (recording) {
      recorderRef.current?.stop();
      return;
    }
    try {
      const stream = await navigator.mediaDevices.getUserMedia({ audio: true });
      const recorder = new MediaRecorder(stream);
      chunksRef.current = [];
      recorder.ondataavailable = (e) => {
        if (e.data.size > 0) chunksRef.current.push(e.data);
      };
      recorder.onstop = async () => {
        stream.getTracks().forEach((t) => t.stop());
        setRecording(false);
        const blob = new Blob(chunksRef.current, {
          type: recorder.mimeType || "audio/webm",
        });
        if (blob.size === 0) return;
        // 从 mimeType 推扩展名(audio/webm;codecs=opus → webm)
        const fmt =
          (recorder.mimeType || "audio/webm")
            .split(";")[0]
            .split("/")[1] || "webm";
        setTranscribing(true);
        try {
          const buf = await blob.arrayBuffer();
          const b64 = bytesToBase64(new Uint8Array(buf));
          const text = await api.transcribeChatAudio(b64, fmt);
          setInput((prev) => (prev ? `${prev} ${text}` : text));
        } catch (e) {
          toast.error(`语音转写失败: ${e}`);
        } finally {
          setTranscribing(false);
        }
      };
      recorder.start();
      recorderRef.current = recorder;
      setRecording(true);
    } catch {
      toast.error("无法访问麦克风,请检查权限");
    }
  }

  return (
    // 整页全幅:会话列表在侧栏,这里只有模型选择 + 消息流 + 输入,无标题栏、无外层卡片边框
    <div className="flex min-h-0 min-w-0 flex-1 flex-col overflow-hidden">
        {/* 左上角:模型厂商 - 模型选择(新会话可选,已有会话显示其绑定模型) */}
        <div className="flex shrink-0 items-center px-3 py-2">
          {active ? (
            <span className="inline-flex items-center gap-1.5 rounded-md px-2 py-1 text-xs text-muted-foreground">
              <Sparkles className="size-3.5" />
              {currentModelLabel}
            </span>
          ) : models.length > 0 ? (
            <Select value={pickedModel} onValueChange={setPickedModel}>
              <SelectTrigger
                size="sm"
                className="h-8 w-auto gap-1.5 border-0 bg-transparent text-xs text-muted-foreground shadow-none hover:bg-accent hover:text-foreground focus-visible:ring-0 dark:bg-transparent"
              >
                <Sparkles className="size-3.5" />
                <SelectValue placeholder="选择模型厂商 - 模型" />
              </SelectTrigger>
              <SelectContent>
                {models.map((m) => (
                  <SelectItem key={m.value} value={m.value}>
                    {m.providerName} - {m.model}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          ) : (
            <span className="px-2 text-xs text-destructive">
              未配置模型(请到系统配置 → 模型厂商)
            </span>
          )}
        </div>

        {/* 消息区 */}
        <div
          ref={scrollRef}
          className="veltrix-thin-scrollbar min-h-0 flex-1 overflow-y-auto p-4"
        >
          {messages.length === 0 && !sending ? (
            <div className="flex h-full items-center justify-center">
              {models.length === 0 ? (
                <EmptyState
                  title="尚未配置模型"
                  description="请到系统配置 → 模型厂商,填好 API Key 与模型后再开始对话"
                />
              ) : (
                <div className="text-center text-muted-foreground">
                  <Sparkles className="mx-auto mb-3 size-10 opacity-40" />
                  <p className="text-sm">输入问题开始对话</p>
                </div>
              )}
            </div>
          ) : (
            <div className="mx-auto max-w-3xl space-y-4">
              {messages.map((m) => (
                <MessageBubble key={m.id} message={m} />
              ))}
              {/* 流式生成中的 assistant 气泡:先「思考中」,首个 token 到达后边收边渲染 */}
              {sending && (
                <div className="flex gap-3">
                  <div className="flex size-7 shrink-0 items-center justify-center rounded-full bg-primary/10 text-primary">
                    <Sparkles className="size-4" />
                  </div>
                  {streamingContent ? (
                    <div className="max-w-[80%] whitespace-pre-wrap break-words rounded-lg bg-muted px-3.5 py-2 text-sm leading-relaxed text-foreground">
                      {streamingContent}
                      <span className="ml-0.5 inline-block h-4 w-1.5 animate-pulse bg-foreground/60 align-middle" />
                    </div>
                  ) : (
                    <div className="flex items-center gap-2 rounded-lg bg-muted px-3 py-2 text-sm text-muted-foreground">
                      <Loader2 className="size-4 animate-spin" />
                      思考中…
                    </div>
                  )}
                </div>
              )}
            </div>
          )}
        </div>

        {/* 输入区:无分割线,悬浮在底部的圆角卡片;附件 + 模型选择内嵌在卡片内 */}
        <div className="shrink-0 px-4 pb-4">
          {/* 隐藏文件选择;「+」按钮触发,支持多选 */}
          <input
            ref={fileInputRef}
            type="file"
            multiple
            className="hidden"
            onChange={(e) => {
              void onPickFiles(e.target.files);
              e.target.value = ""; // 允许重复选同一文件
            }}
          />
          <div className="mx-auto flex max-w-3xl flex-col gap-1 rounded-2xl border bg-card p-2 shadow-lg">
            {/* 附件预览条:每个附件一枚 chip,可移除 */}
            {attachments.length > 0 && (
              <div className="flex flex-wrap gap-1.5 px-1 pt-1">
                {attachments.map((a, i) => (
                  <span
                    key={i}
                    className="inline-flex items-center gap-1.5 rounded-lg border bg-muted/50 py-1 pl-2 pr-1 text-xs"
                  >
                    {a.mime.startsWith("image/") ? (
                      <ImageIcon className="size-3.5 shrink-0 text-muted-foreground" />
                    ) : (
                      <FileText className="size-3.5 shrink-0 text-muted-foreground" />
                    )}
                    <span className="max-w-40 truncate">{a.name}</span>
                    <span className="text-[10px] text-muted-foreground">
                      {fmtSize(a.size)}
                    </span>
                    <button
                      type="button"
                      onClick={() => removeAttachment(i)}
                      className="rounded p-0.5 text-muted-foreground hover:bg-accent hover:text-foreground"
                    >
                      <X className="size-3.5" />
                    </button>
                  </span>
                ))}
              </div>
            )}
            {/* 输入行:加号 + 文本框 + 语音 + 发送 */}
            <div className="flex items-end gap-2">
              <DropdownMenu>
                <DropdownMenuTrigger asChild>
                  <Button
                    type="button"
                    variant="ghost"
                    size="icon"
                    className="size-9 shrink-0 cursor-pointer rounded-xl"
                    disabled={sending || attachments.length >= MAX_ATTACHMENTS}
                  >
                    <Plus />
                  </Button>
                </DropdownMenuTrigger>
                <DropdownMenuContent align="start" side="top">
                  <DropdownMenuItem onClick={() => openPicker("image/*")}>
                    <ImageIcon className="size-4" />
                    图片
                  </DropdownMenuItem>
                  <DropdownMenuItem onClick={() => openPicker("")}>
                    <FileText className="size-4" />
                    文件
                  </DropdownMenuItem>
                </DropdownMenuContent>
              </DropdownMenu>
              <Textarea
                value={input}
                onChange={(e) => setInput(e.target.value)}
                onKeyDown={(e) => {
                  // Enter 发送,Shift+Enter 换行
                  if (e.key === "Enter" && !e.shiftKey) {
                    e.preventDefault();
                    void handleSend();
                  }
                }}
                placeholder={
                  recording
                    ? "录音中,再次点击麦克风结束…"
                    : "输入消息,Enter 发送 / Shift+Enter 换行"
                }
                disabled={sending}
                className="max-h-40 min-h-[2.5rem] flex-1 resize-none border-0 bg-transparent shadow-none focus-visible:ring-0 dark:bg-transparent"
                rows={1}
              />
              <SimpleTooltip content={recording ? "结束录音" : "语音输入"}>
                <Button
                  type="button"
                  variant={recording ? "destructive" : "ghost"}
                  size="icon"
                  className="size-9 shrink-0 cursor-pointer rounded-xl"
                  disabled={sending || transcribing}
                  onClick={toggleRecording}
                >
                  {transcribing ? (
                    <Loader2 className="animate-spin" />
                  ) : (
                    <Mic className={recording ? "animate-pulse" : ""} />
                  )}
                </Button>
              </SimpleTooltip>
              <Button
                type="button"
                size="icon"
                className="size-9 shrink-0 cursor-pointer rounded-xl"
                disabled={
                  sending || (!input.trim() && attachments.length === 0)
                }
                onClick={handleSend}
              >
                {sending ? <Loader2 className="animate-spin" /> : <Send />}
              </Button>
            </div>
          </div>
        </div>
      </div>
  );
}

// 单条消息气泡:用户右侧、助手左侧。内容按纯文本换行渲染。
function MessageBubble({ message }: { message: ChatMessageView }) {
  const isUser = message.role === "user";
  return (
    <div className={`flex gap-3 ${isUser ? "flex-row-reverse" : ""}`}>
      <div
        className={`flex size-7 shrink-0 items-center justify-center rounded-full ${
          isUser
            ? "bg-sky-500/10 text-sky-600 dark:text-sky-400"
            : "bg-primary/10 text-primary"
        }`}
      >
        {isUser ? <User className="size-4" /> : <Sparkles className="size-4" />}
      </div>
      <div
        className={`max-w-[80%] whitespace-pre-wrap break-words rounded-lg px-3.5 py-2 text-sm leading-relaxed ${
          isUser
            ? "bg-primary text-primary-foreground"
            : "bg-muted text-foreground"
        }`}
      >
        {message.content}
      </div>
    </div>
  );
}
