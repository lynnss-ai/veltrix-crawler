// AI 对话页(对话工作区):会话列表在侧栏,这里只负责消息流 + 文字/语音输入。参考桌面对话体验。
import {
  memo,
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type ReactNode,
} from "react";
import {
  Check,
  ChevronDown,
  ChevronLeft,
  ChevronRight,
  Copy,
  FileDown,
  FileText,
  Images,
  Loader2,
  Mic,
  MoreHorizontal,
  NotebookPen,
  Paperclip,
  Pencil,
  Plus,
  Radar,
  RotateCcw,
  Send,
  Share,
  ThumbsDown,
  ThumbsUp,
  Trash2,
  X,
} from "lucide-react";
import { listen } from "@tauri-apps/api/event";
import { invoke, convertFileSrc } from "@tauri-apps/api/core";
import html2canvas from "html2canvas-pro";
import { jsPDF } from "jspdf";
import { toast } from "sonner";

import {
  api,
  type ChatAttachment,
  type ChatMessageView,
  type ContentView,
  type MessageAttachment,
  type ProviderDto,
} from "@/lib/api";
import {
  contentDetailUrl,
  authorProfileUrl,
  platformLabel,
} from "@/lib/platforms";
import { recordDownload } from "@/lib/download-history";
import { Button } from "@/components/ui/button";
import { Textarea } from "@/components/ui/textarea";
import { Input } from "@/components/ui/input";
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
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { SimpleTooltip } from "@/components/SimpleTooltip";
import { EmptyState } from "@/components/EmptyState";
import { MarkdownMessage } from "@/components/MarkdownMessage";
import {
  ContentPickerDialog,
  type AssetPickMode,
  type AssetPickResult,
} from "@/components/content-picker-dialog";
import { useChat } from "@/hooks/use-chat";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSub,
  DropdownMenuSubContent,
  DropdownMenuSubTrigger,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";

// 一个可选模型 = 厂商 + 模型名(value 用 "providerId::model" 编码)
interface ModelOption {
  providerId: string;
  providerName: string;
  model: string;
  value: string;
}

// 记住用户上次选用的模型(value=providerId::model),作为下次新会话的默认。
// 桌面端 UI 偏好,与登录态一致存 localStorage;localStorage 不可用时静默回退。
const LAST_MODEL_KEY = "veltrix:chat:last-model";
function readLastModel(): string {
  try {
    return localStorage.getItem(LAST_MODEL_KEY) ?? "";
  } catch {
    return "";
  }
}
function rememberLastModel(value: string) {
  try {
    localStorage.setItem(LAST_MODEL_KEY, value);
  } catch {
    // 隐私模式等 localStorage 不可用时跳过,不影响模型选择本身
  }
}

// 附件限制:最多 12 个,单个 ≤ 10MB
const MAX_ATTACHMENTS = 12;
const MAX_ATTACHMENT_BYTES = 10 * 1024 * 1024;

// 外部附件可选类型:图片 / PDF / Word / Excel / Markdown
const EXTERNAL_ACCEPT =
  "image/*,.pdf,.doc,.docx,.xls,.xlsx,.md,.markdown";

// 外部附件允许的扩展名(accept 仅是对话框默认过滤,落库前再按此校验拦截非法类型;
// 图片优先按 MIME 判断,扩展名兜底)
const ALLOWED_ATTACHMENT_EXTS = [
  ".jpg",
  ".jpeg",
  ".png",
  ".gif",
  ".webp",
  ".bmp",
  ".svg",
  ".pdf",
  ".doc",
  ".docx",
  ".xls",
  ".xlsx",
  ".md",
  ".markdown",
];

function isAllowedAttachment(file: File): boolean {
  if (file.type.startsWith("image/")) return true;
  const lower = file.name.toLowerCase();
  return ALLOWED_ATTACHMENT_EXTS.some((ext) => lower.endsWith(ext));
}

// 带本地预览信息的附件(size 仅用于展示)
interface PendingAttachment extends ChatAttachment {
  size: number;
  // 来源资产图片 key(`${内容id}#位置`):用于与资产弹窗勾选同步;外部附件无此字段
  assetKey?: string;
}

function fmtSize(bytes: number): string {
  if (bytes >= 1024 * 1024) return `${(bytes / 1024 / 1024).toFixed(1)}MB`;
  if (bytes >= 1024) return `${Math.round(bytes / 1024)}KB`;
  return `${bytes}B`;
}

// 消息时间:今天只显示「时:分」,其它日期显示「年-月-日 时:分」
function fmtMsgTime(unixSec: number): string {
  const d = new Date(unixSec * 1000);
  const now = new Date();
  const pad = (n: number) => String(n).padStart(2, "0");
  const hm = `${pad(d.getHours())}:${pad(d.getMinutes())}`;
  const sameDay =
    d.getFullYear() === now.getFullYear() &&
    d.getMonth() === now.getMonth() &&
    d.getDate() === now.getDate();
  return sameDay
    ? hm
    : `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${hm}`;
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

// 从厂商列表展开出可用模型(有 apiKey + 具备「对话」能力的模型才算可用)
function buildModelOptions(providers: ProviderDto[]): ModelOption[] {
  const opts: ModelOption[] = [];
  for (const p of providers) {
    if (!p.apiKey.trim()) continue;
    for (const spec of p.models) {
      const model = spec.name.trim();
      // 对话场景只列能「对话」的模型
      if (!model || !spec.capabilities.includes("text")) continue;
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

// 视频时长(秒)→ `分:秒`;无/非视频返回空串(供条件省略该行)
function fmtDuration(sec: number | null): string {
  if (!sec || sec <= 0) return "";
  const m = Math.floor(sec / 60);
  const s = sec % 60;
  return `${m}:${String(s).padStart(2, "0")}`;
}

// PDF 分页辅助:消息块在源截图中的纵向边界(canvas px),用于在块之间分页,避免拦腰截断。
type BlockBound = { top: number; bottom: number };

// 读取容器直接子元素(每条消息气泡)相对容器顶部的纵向区间,换算到 canvas px。
function computeBlockBounds(
  container: HTMLElement,
  containerTop: number,
  scale: number,
  canvasHeight: number,
): BlockBound[] {
  const bounds: BlockBound[] = [];
  for (const child of Array.from(container.children)) {
    const rect = child.getBoundingClientRect();
    const top = Math.max(0, Math.round((rect.top - containerTop) * scale));
    const bottom = Math.min(
      canvasHeight,
      Math.round((rect.bottom - containerTop) * scale),
    );
    if (bottom > top) bounds.push({ top, bottom });
  }
  return bounds;
}

// 贪心分页:按块边界把整块塞入当前页,放不下则换页;单块高于整页时才在块内部切分。
// 返回每页对应源图的 [top, bottom) 区段(canvas px)。
function paginateByBlocks(
  canvasHeight: number,
  blocks: BlockBound[],
  firstPageH: number,
  fullPageH: number,
): BlockBound[] {
  // 无块信息时退化为按整页等高切分,保证仍能导出
  if (blocks.length === 0) {
    const segments: BlockBound[] = [];
    let y = 0;
    let isFirst = true;
    while (y < canvasHeight) {
      const cap = isFirst ? firstPageH : fullPageH;
      const bottom = Math.min(canvasHeight, y + cap);
      segments.push({ top: y, bottom });
      y = bottom;
      isFirst = false;
    }
    return segments;
  }

  const segments: BlockBound[] = [];
  let pageTop = 0; // 当前页起始 y(canvas px)
  let isFirst = true;
  let i = 0;
  while (i < blocks.length) {
    const cap = isFirst ? firstPageH : fullPageH;
    const block = blocks[i];
    const wouldBottom = block.bottom - pageTop;

    if (wouldBottom <= cap) {
      // 整块能放进当前页,继续累积下一块
      i++;
      // 收尾:最后一块直接结页
      if (i >= blocks.length) {
        segments.push({ top: pageTop, bottom: block.bottom });
      }
      continue;
    }

    // 放不下当前块
    const blockHeight = block.bottom - block.top;
    if (blockHeight > fullPageH) {
      // 单块本身超过整页:先把当前页已累积的内容结掉(若有),再对该块按整页切分
      if (block.top > pageTop) {
        segments.push({ top: pageTop, bottom: block.top });
        isFirst = false;
      }
      let y = block.top;
      const cap2 = isFirst ? firstPageH : fullPageH;
      // 块内逐页切分(此处不得已的内部截断)
      let firstSliceCap = cap2;
      while (block.bottom - y > 0) {
        const sliceBottom = Math.min(block.bottom, y + firstSliceCap);
        segments.push({ top: y, bottom: sliceBottom });
        y = sliceBottom;
        isFirst = false;
        firstSliceCap = fullPageH;
        if (block.bottom - y <= 0) break;
      }
      pageTop = block.bottom;
      i++;
      continue;
    }

    // 普通块放不下当前页
    if (block.top > pageTop) {
      // 当前页已有内容:在该块上方换页,下一轮把整块放进新页
      segments.push({ top: pageTop, bottom: block.top });
      pageTop = block.top;
      isFirst = false;
      continue; // 不自增 i
    }
    // 块已位于当前页顶部却仍放不下(仅发生在首页因标题区高度缩水时):
    // 退用整页高度重试,避免空段死循环
    if (isFirst) {
      isFirst = false;
      continue; // pageTop / i 不变,下一轮用 fullPageH 容量重试
    }
    // 整页容量也放不下(理论上 blockHeight<=fullPageH 不会到这里),兜底整页切分
    {
      const bottom = Math.min(block.bottom, pageTop + fullPageH);
      segments.push({ top: pageTop, bottom });
      pageTop = bottom;
      if (pageTop >= block.bottom) i++;
    }
  }

  // 过滤掉零高/负高段
  return segments.filter((s) => s.bottom > s.top);
}

// 资产文案结构化为输入框文本:平台 / 标题 / 文案(描述 + 视频转写)/ 话题 / 视频时长 / 互动数据 / 地址。
// 多条文案时前缀「【序号】」便于区分;数值缺失记 0;话题取已剥离的 topics 字段;
// 无话题/非视频/无公开链接的项自动省略对应行。
function buildCopyBlock(c: ContentView, index: number, total: number): string {
  const num = (v: number | null) => v ?? 0;
  const lines: string[] = [];
  if (total > 1) lines.push(`【${index + 1}】`);
  lines.push(`平台:${platformLabel(c.platform)}`);
  lines.push(`标题:${c.title?.trim() || "(无标题)"}`);
  const body = [c.desc?.trim(), c.transcript?.trim()]
    .filter(Boolean)
    .join("\n");
  lines.push(`文案:${body || "(无文案)"}`);
  if (c.topics.length > 0) lines.push(`话题:${c.topics.join(" ")}`);
  const dur = fmtDuration(c.duration);
  if (dur) lines.push(`视频时长:${dur}`);
  lines.push(
    `点赞数量:${num(c.likeCount)}  评论数量:${num(c.commentCount)}  转发数量:${num(c.shareCount)}  收藏数量:${num(c.collectCount)}`,
  );
  const contentUrl = contentDetailUrl(c.platform, c.contentId);
  if (contentUrl) lines.push(`文案地址:${contentUrl}`);
  const authorUrl = authorProfileUrl(c.platform, c.authorUid);
  if (authorUrl) lines.push(`作者地址:${authorUrl}`);
  return lines.join("\n");
}

export function ChatPage() {
  // 会话列表 + 当前会话来自侧栏共享的 ChatProvider
  const {
    conversations,
    activeId,
    setActiveId,
    providers,
    setPendingAgentType,
    setPendingFirstMessage,
    reload,
  } = useChat();
  const [messages, setMessages] = useState<ChatMessageView[]>([]);
  const [input, setInput] = useState("");
  const [attachments, setAttachments] = useState<PendingAttachment[]>([]);
  // 历史消息里点开的图片(全屏预览);null=未预览。与输入区附件灯箱(previewIndex)相互独立
  const [historyImagePreview, setHistoryImagePreview] = useState<string | null>(
    null,
  );
  const fileInputRef = useRef<HTMLInputElement>(null);
  const [sending, setSending] = useState(false);
  // 意图判断(新会话首条消息分类)进行中:此阶段较慢且会发 LLM 请求,需挡住连点造成的重复发送
  const [classifying, setClassifying] = useState(false);
  // 发送动作的同步重入锁:state 更新是异步的,光靠 sending/classifying 挡不住极快连点,用 ref 兜底
  const dispatchingRef = useRef(false);
  // 可用模型由共享 providers 派生(避免布局重挂载时各自重拉导致「尚无可用模型」竞态)
  const models = useMemo(() => buildModelOptions(providers), [providers]);
  // 新会话默认模型(value);开新会话时用它
  const [pickedModel, setPickedModel] = useState("");
  // 智能搜索开关(输入框加号右侧):开启后本轮对话走智能搜索增强(后端能力接入前先作为状态保留)
  const [smartSearch, setSmartSearch] = useState(false);
  // 流式输出:正在生成的 assistant 文本(null=未在生成);streamingConvRef 记录归属会话
  const [streamingContent, setStreamingContent] = useState<string | null>(null);
  const streamingConvRef = useRef<string | null>(null);
  // 流式增量按帧批处理:增量先攒到 ref,每帧合并刷一次,避免每 token 全量重渲染/重解析
  const pendingDeltaRef = useRef("");
  const rafRef = useRef<number | null>(null);
  // 用户是否贴在底部:决定流式时要不要自动滚到底(向上翻阅时不打断)
  const atBottomRef = useRef(true);
  // 录音状态
  const [recording, setRecording] = useState(false);
  const [transcribing, setTranscribing] = useState(false);
  const recorderRef = useRef<MediaRecorder | null>(null);
  const chunksRef = useRef<Blob[]>([]);
  const scrollRef = useRef<HTMLDivElement>(null);
  // 消息内容容器(导出 PDF 时截这块渲染后的内容)
  const messagesContentRef = useRef<HTMLDivElement>(null);
  // 资产选择弹窗:null=未开;否则区分文案/图片两种用途
  const [assetPickerMode, setAssetPickerMode] = useState<AssetPickMode | null>(
    null,
  );
  // 各用途已选内容 id(按用途分开记忆),再次打开弹窗时回填勾选
  const [assetSelected, setAssetSelected] = useState<
    Record<AssetPickMode, string[]>
  >({ copy: [], image: [] });
  // 图片附件预览(灯箱):当前预览的图片在图片附件列表中的下标;null=未预览
  const [previewIndex, setPreviewIndex] = useState<number | null>(null);
  // 每条 AI 回复的点赞/点踩(纯前端态,按消息 id)
  const [feedback, setFeedback] = useState<
    Record<number, "like" | "dislike">
  >({});
  // 重命名 / 删除会话的自定义弹框(替代原生 prompt/confirm)
  const [renameOpen, setRenameOpen] = useState(false);
  const [renameValue, setRenameValue] = useState("");
  const [deleteOpen, setDeleteOpen] = useState(false);
  // 本会话记忆(滚动摘要)查看/编辑弹窗
  const [summaryOpen, setSummaryOpen] = useState(false);
  const [summaryText, setSummaryText] = useState("");
  const [summaryLoading, setSummaryLoading] = useState(false);
  const [summarySaving, setSummarySaving] = useState(false);

  const active = conversations.find((c) => c.id === activeId) ?? null;
  // 发送相关「忙碌」总开关:意图判断 + 发送中都算,统一控制发送按钮禁用与回车拦截
  const busy = sending || classifying;

  // 监听后端流式增量事件:增量先攒到 ref,每帧(rAF)合并刷一次,避免每 token 全量重渲染
  useEffect(() => {
    let dispose: (() => void) | undefined;
    let disposed = false;
    listen<{ conversationId: string; delta: string }>("chat-stream", (e) => {
      if (streamingConvRef.current !== e.payload.conversationId) return;
      pendingDeltaRef.current += e.payload.delta;
      if (rafRef.current == null) {
        rafRef.current = requestAnimationFrame(() => {
          rafRef.current = null;
          const chunk = pendingDeltaRef.current;
          pendingDeltaRef.current = "";
          if (chunk) setStreamingContent((prev) => (prev ?? "") + chunk);
        });
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
      if (rafRef.current != null) cancelAnimationFrame(rafRef.current);
    };
  }, []);

  // providers 加载后初始化默认模型:优先上次记住的(仍有效),否则回退首个可用
  useEffect(() => {
    if (models.length === 0 || pickedModel) return;
    const remembered = readLastModel();
    const valid = remembered && models.some((m) => m.value === remembered);
    setPickedModel(valid ? remembered : models[0].value);
  }, [models, pickedModel]);

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

  // 消息变化 / 流式增量时滚到底部:仅当用户已贴在底部(向上翻阅时不打断)
  useEffect(() => {
    if (!atBottomRef.current) return;
    const el = scrollRef.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, [messages, sending, streamingContent]);

  // 记录用户是否贴在底部(滚动时更新)
  function onMessagesScroll() {
    const el = scrollRef.current;
    if (el) {
      atBottomRef.current = el.scrollHeight - el.scrollTop - el.clientHeight < 80;
    }
  }

  // 清空流式增量缓冲与待刷帧(开始/结束流式前调用,避免残留增量误刷出气泡)
  function resetStreamingBuffer() {
    if (rafRef.current != null) {
      cancelAnimationFrame(rafRef.current);
      rafRef.current = null;
    }
    pendingDeltaRef.current = "";
  }

  // 模型选择器选项:已有会话若绑定了已不在可用列表里的模型(厂商删了 Key / 改了模型行),
  // 补一个占位项,保证下拉仍能显示当前模型,不至于空白。
  const modelOptions = useMemo(() => {
    if (
      active &&
      !models.some(
        (m) => m.providerId === active.providerId && m.model === active.model,
      )
    ) {
      return [
        ...models,
        {
          providerId: active.providerId,
          providerName: "已失效厂商",
          model: active.model,
          value: `${active.providerId}::${active.model}`,
        },
      ];
    }
    return models;
  }, [active, models]);

  // 模型选择器当前值:已有会话用其绑定模型编码,新会话用待建会话所选
  const selectedModelValue = active
    ? `${active.providerId}::${active.model}`
    : pickedModel;

  // 两级模型选择:先按厂商分组(保持出现顺序),二级再列该厂商的模型
  const groupedModels = useMemo(() => {
    const groups: { providerId: string; providerName: string; items: ModelOption[] }[] =
      [];
    for (const m of modelOptions) {
      let g = groups.find((x) => x.providerId === m.providerId);
      if (!g) {
        g = { providerId: m.providerId, providerName: m.providerName, items: [] };
        groups.push(g);
      }
      g.items.push(m);
    }
    return groups;
  }, [modelOptions]);

  // 触发按钮只显示模型名(厂商不展示)
  const currentModel = modelOptions.find((m) => m.value === selectedModelValue);
  const currentModelLabel = currentModel ? currentModel.model : "选择模型";

  // 切换模型:
  // - 新会话(未建)→ 只改本地待用值
  // - 已有会话且已产生消息 → 开一个新会话(不改当前会话绑定),避免同会话混用模型
  // - 已有会话但空(无消息)→ 直接回写后端绑定模型
  async function handleModelChange(value: string) {
    // 记住本次选择,作为下次新会话的默认(占位的失效模型不记)
    if (models.some((m) => m.value === value)) rememberLastModel(value);
    if (!active) {
      setPickedModel(value);
      return;
    }
    const opt = modelOptions.find((m) => m.value === value);
    if (
      !opt ||
      (opt.providerId === active.providerId && opt.model === active.model)
    ) {
      return;
    }
    if (messages.length > 0) {
      // 已有对话内容:换模型即开新会话(下次发送时按新模型建会话)
      setActiveId(null);
      setPendingAgentType("chat"); // 清残留的待用场景类型,避免 active=null 时回落到 coding 误翻布局
      setMessages([]);
      setInput("");
      setAttachments([]);
      setPickedModel(value);
      return;
    }
    try {
      await api.updateConversationModel(active.id, opt.providerId, opt.model);
      await reload();
    } catch (e) {
      toast.error(`切换模型失败: ${e}`);
    }
  }

  // 选择附件:先按类型(扩展名/MIME)拦截非法文件,再校验数量 + 单个大小,读为 base64 加入待发列表
  async function onPickFiles(files: FileList | null) {
    if (!files || files.length === 0) return;
    // 类型校验:非允许类型直接跳过并提示(accept 可能被系统绕过)
    const incoming: File[] = [];
    for (const f of Array.from(files)) {
      if (isAllowedAttachment(f)) {
        incoming.push(f);
      } else {
        toast.error(
          `「${f.name}」类型不支持,仅支持图片 / PDF / Word / Excel / Markdown`,
        );
      }
    }
    if (incoming.length === 0) return;
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

  // 批量加入附件(资产素材),受 MAX_ATTACHMENTS 上限约束;size 由 base64 长度估算供 chip 展示。
  // incoming 可带 assetKey(资产图片来源),透传以便与资产弹窗勾选同步
  function addAttachments(incoming: Array<ChatAttachment & { assetKey?: string }>) {
    if (incoming.length === 0) return;
    setAttachments((prev) => {
      const room = MAX_ATTACHMENTS - prev.length;
      if (room <= 0) {
        toast.error(`最多上传 ${MAX_ATTACHMENTS} 个附件`);
        return prev;
      }
      if (incoming.length > room) {
        toast.error(`最多 ${MAX_ATTACHMENTS} 个,超出部分未添加`);
      }
      const accepted: PendingAttachment[] = incoming.slice(0, room).map((a) => ({
        ...a,
        size: Math.floor((a.data.length * 3) / 4), // base64 → 原始字节数估算
      }));
      return [...prev, ...accepted];
    });
  }

  // 资产选择弹窗确认后的处理:文案拼接插入输入框,图片按逐条挑选拉本地素材作附件
  async function handleAssetPick(result: AssetPickResult) {
    if (result.mode === "copy") {
      // 结构化插入:每条 = 标题 / 文案 / 互动数据 / 文案地址 / 作者地址;多条时加序号区隔。
      const total = result.contents.length;
      const blocks = result.contents.map((c, i) =>
        buildCopyBlock(c, i, total),
      );
      if (blocks.length === 0) {
        toast.error("所选内容没有可用的文案");
        return;
      }
      // 多条之间空行分隔,整体插入输入框(保留已输入内容)
      const copy = blocks.join("\n\n");
      setInput((prev) => (prev.trim() ? `${prev}\n\n${copy}` : copy));
      return;
    }
    // 资产图片:逐条让后端读本地视觉素材(封面 / 指定位置图片)转 base64,汇总后入附件。
    // 给每个附件打上来源 assetKey(`${内容id}#位置`),便于删除后与资产弹窗勾选同步
    const results = await Promise.allSettled(
      result.picks.map((p) =>
        api.buildContentAttachments(
          p.content.id,
          p.coverOnly,
          p.coverOnly ? undefined : p.indices,
        ),
      ),
    );
    const tagged: Array<ChatAttachment & { assetKey: string }> = [];
    let failed = 0;
    results.forEach((r, pi) => {
      if (r.status !== "fulfilled") {
        failed += 1;
        return;
      }
      const p = result.picks[pi];
      r.value.forEach((att, i) => {
        // 封面位置固定 0;图文按请求位置(已排序)对应回填
        const pos = p.coverOnly ? 0 : (p.indices[i] ?? i);
        tagged.push({ ...att, assetKey: `${p.content.id}#${pos}` });
      });
    });
    if (tagged.length > 0) addAttachments(tagged);
    if (failed > 0) {
      toast.error(`${failed} 条内容的素材引入失败(可能未下载到本地)`);
    }
  }

  // 图片附件子集(供灯箱预览 + 上/下一张);previewIndex 即此列表的下标
  const imageAttachments = attachments.filter((a) =>
    a.mime.startsWith("image/"),
  );
  function openPreview(att: PendingAttachment) {
    const idx = imageAttachments.findIndex((x) => x === att);
    if (idx >= 0) setPreviewIndex(idx);
  }
  const stepPreview = (delta: number) =>
    setPreviewIndex((i) =>
      i === null || imageAttachments.length === 0
        ? null
        : (i + delta + imageAttachments.length) % imageAttachments.length,
    );

  // 灯箱打开时支持键盘:Esc 关闭,←/→ 上/下一张
  useEffect(() => {
    if (previewIndex === null) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") setPreviewIndex(null);
      else if (e.key === "ArrowLeft") stepPreview(-1);
      else if (e.key === "ArrowRight") stepPreview(1);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [previewIndex, imageAttachments.length]);

  // 发送核心:乐观追加用户消息 → 建会话(必要时)→ 流式取回复落库。返回是否成功。
  async function sendMessage(
    text: string,
    atts: PendingAttachment[],
  ): Promise<boolean> {
    if ((!text && atts.length === 0) || sending) return false;
    if (models.length === 0) {
      toast.error("尚无可用模型,请先到系统配置 → 模型厂商填好 API Key 与模型");
      return false;
    }
    setSending(true);
    // 乐观追加用户消息:正文 + 附件(图片带内联 base64 即时预览,落库后由后端 path 接管)
    const optimistic: ChatMessageView = {
      id: Date.now(),
      conversationId: activeId ?? "",
      role: "user",
      content: text,
      attachments: atts.map((a) => ({
        name: a.name,
        mime: a.mime,
        data: a.data,
      })),
      createdAt: Math.floor(Date.now() / 1000),
    };
    // 发送即视为回到底部,确保新消息与回复滚动可见
    atBottomRef.current = true;
    setMessages((prev) => [...prev, optimistic]);

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
      resetStreamingBuffer();
      setStreamingContent("");
      const reply = await api.sendChatMessageStream(
        convId,
        text,
        atts.map((a) => ({ name: a.name, mime: a.mime, data: a.data })),
      );
      setMessages((prev) => [...prev, reply]);
      // 刷新侧栏会话列表(标题由 AI 概括生成 + 排序更新)
      await reload();
      return true;
    } catch (e) {
      toast.error(`发送失败: ${e}`);
      setMessages((prev) => prev.filter((m) => m.id !== optimistic.id));
      return false;
    } finally {
      streamingConvRef.current = null;
      resetStreamingBuffer();
      setStreamingContent(null);
      setSending(false);
    }
  }

  async function handleSend() {
    // 重入锁:意图判断阶段较慢,挡住连点造成的重复发送(state 异步,需 ref 同步兜底)
    if (dispatchingRef.current) return;
    const text = input.trim();
    if ((!text && attachments.length === 0) || sending) return;
    dispatchingRef.current = true;
    try {
      // 新会话且纯文本:先按意图判断,编程 / 浏览器任务自动转交对应 Agent(页面切对应布局)
      if (!activeId && text && attachments.length === 0 && models.length > 0) {
        const opt = models.find((m) => m.value === pickedModel) ?? models[0];
        let type = "chat";
        setClassifying(true);
        try {
          type = await api.classifyAgentType(text, opt.providerId, opt.model);
        } catch {
          // 分类失败按普通对话继续
        } finally {
          setClassifying(false);
        }
        if (type === "coding" || type === "rpa" || type === "computer") {
          const id = crypto.randomUUID();
          try {
            await api.createConversation(id, opt.providerId, opt.model, type);
          } catch (e) {
            toast.error(`创建会话失败: ${e}`);
            return;
          }
          setInput("");
          // 交接:建好对应会话后切布局,首条消息由 CodingLayout / RpaLayout / ComputerLayout 自动发送
          setPendingFirstMessage(text);
          setPendingAgentType(type);
          setActiveId(id);
          await reload();
          return;
        }
      }
      const atts = attachments;
      setInput("");
      setAttachments([]);
      const ok = await sendMessage(text, atts);
      // 失败回滚输入与附件,便于重发
      if (!ok) {
        setInput(text);
        setAttachments(atts);
      }
    } finally {
      dispatchingRef.current = false;
    }
  }

  // 重新生成:把某条用户消息内容再发一轮(用 ref 取最新发送函数,保持回调稳定供 memo)
  const sendMessageRef = useRef(sendMessage);
  sendMessageRef.current = sendMessage;
  const regenerate = useCallback((content: string) => {
    void sendMessageRef.current(content, []);
  }, []);

  // 复制到剪贴板(分享=复制可粘贴文本)
  const copyToClipboard = useCallback(async (text: string, okMsg = "已复制") => {
    try {
      await navigator.clipboard.writeText(text);
      toast.success(okMsg);
    } catch {
      toast.error("复制失败");
    }
  }, []);

  // 点赞 / 点踩:同值再点取消(纯前端态)
  const toggleFeedback = useCallback((id: number, v: "like" | "dislike") => {
    setFeedback((prev) => {
      const next = { ...prev };
      if (next[id] === v) delete next[id];
      else next[id] = v;
      return next;
    });
  }, []);

  // 整段对话拼成 markdown(分享/下载共用)
  function conversationMarkdown(): string {
    return messages
      .map((m) => `**${m.role === "user" ? "我" : "AI"}**:\n\n${m.content}`)
      .join("\n\n---\n\n");
  }

  // 分享整段对话:复制 markdown
  function shareConversation() {
    if (messages.length === 0) return;
    void copyToClipboard(conversationMarkdown(), "已复制整段对话,可粘贴分享");
  }

  // 下载整段对话为 markdown(走后端保存对话框)
  async function downloadMarkdown() {
    if (messages.length === 0) return;
    try {
      const path = await invoke<string | null>("save_text_dialog", {
        content: conversationMarkdown(),
        fileName: `${active?.title || "对话"}.md`,
      });
      if (path) {
        recordDownload({ path, kind: "对话Markdown" });
        toast.success("已保存");
      }
    } catch (e) {
      toast.error(`保存失败: ${e}`);
    }
  }

  // 导出整段对话为 PDF:截取已渲染的消息区(含高亮代码 / Mermaid 图)→ 按消息块边界
  // 分页(避免拦腰截断)→ jsPDF 加页边距 + 首页标题 → 后端保存对话框写文件。
  async function downloadPdf() {
    if (messages.length === 0) return;
    const el = messagesContentRef.current;
    if (!el) return;
    const t = toast.loading("正在生成 PDF…");
    try {
      const isDark = document.documentElement.classList.contains("dark");
      const bgColor = isDark ? "#0b0b0c" : "#ffffff";
      // 截图缩放:2 倍兼顾清晰度与体积
      const SCALE = 2;
      const canvas = await html2canvas(el, {
        scale: SCALE,
        backgroundColor: bgColor,
        useCORS: true,
        logging: false,
      });

      // 消息块在容器内的纵向边界(CSS px → canvas px),用于在块之间而非块内部分页。
      // messagesContentRef 的直接子元素即一条条消息气泡。
      const containerTop = el.getBoundingClientRect().top;
      const blockBounds = computeBlockBounds(el, containerTop, SCALE, canvas.height);

      const pdf = new jsPDF({ unit: "px", format: "a4", compress: true });
      const pageW = pdf.internal.pageSize.getWidth();
      const pageH = pdf.internal.pageSize.getHeight();
      const MARGIN = 32; // 上下左右页边距(px)
      const contentW = pageW - MARGIN * 2;
      // canvas px → pdf px 的换算比例(按内容宽度等比缩放)
      const pxToPdf = contentW / canvas.width;
      // 一页可容纳的源图高度(canvas px)
      const fullPageCanvasH = (pageH - MARGIN * 2) / pxToPdf;

      // 首页标题区:标题 + 日期小字,占用首页顶部高度(pdf px)
      const title = active?.title || "对话";
      const dateLine = new Date().toLocaleDateString("zh-CN", {
        year: "numeric",
        month: "long",
        day: "numeric",
      });
      const TITLE_FONT = 18;
      const DATE_FONT = 10;
      const headerH = TITLE_FONT + 8 + DATE_FONT + 18; // 标题 + 间距 + 日期 + 底部留白
      // 首页正文可用源图高度比后续页少一个标题区
      const firstPageCanvasH = fullPageCanvasH - headerH / pxToPdf;

      // 计算分页切点:贪心地把整块塞进当前页,放不下就换页;单块高于整页才允许块内切分。
      const segments = paginateByBlocks(
        canvas.height,
        blockBounds,
        firstPageCanvasH,
        fullPageCanvasH,
      );

      segments.forEach((seg, idx) => {
        if (idx > 0) pdf.addPage();
        const isFirst = idx === 0;
        let yPdf = MARGIN;
        if (isFirst) {
          // 首页标题
          pdf.setTextColor(isDark ? "#e7e7ea" : "#18181b");
          pdf.setFontSize(TITLE_FONT);
          pdf.text(title, MARGIN, MARGIN + TITLE_FONT);
          pdf.setFontSize(DATE_FONT);
          pdf.setTextColor(isDark ? "#9a9aa3" : "#71717a");
          pdf.text(dateLine, MARGIN, MARGIN + TITLE_FONT + 8 + DATE_FONT);
          yPdf = MARGIN + headerH;
        }
        const segH = seg.bottom - seg.top;
        if (segH <= 0) return;
        // 把源图的 [seg.top, seg.bottom) 区段切到临时画布,再贴进 PDF
        const slice = document.createElement("canvas");
        slice.width = canvas.width;
        slice.height = segH;
        const ctx = slice.getContext("2d");
        if (!ctx) return;
        ctx.fillStyle = bgColor;
        ctx.fillRect(0, 0, slice.width, slice.height);
        ctx.drawImage(
          canvas,
          0,
          seg.top,
          canvas.width,
          segH,
          0,
          0,
          slice.width,
          segH,
        );
        const sliceData = slice.toDataURL("image/jpeg", 0.92);
        pdf.addImage(sliceData, "JPEG", MARGIN, yPdf, contentW, segH * pxToPdf);
      });

      const b64 = pdf.output("datauristring").split(",")[1] ?? "";
      const path = await invoke<string | null>("save_binary_dialog", {
        contentBase64: b64,
        fileName: `${active?.title || "对话"}.pdf`,
      });
      toast.dismiss(t);
      if (path) {
        recordDownload({ path, kind: "对话PDF" });
        toast.success("已导出 PDF");
      }
    } catch (e) {
      toast.dismiss(t);
      toast.error(`导出 PDF 失败: ${e}`);
    }
  }

  // 重命名:打开自定义弹框,回填当前标题
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

  // 删除:打开二次确认弹框
  function openDelete() {
    if (!active) return;
    setDeleteOpen(true);
  }
  async function confirmDelete() {
    if (!active) return;
    try {
      await api.deleteConversation(active.id);
      setActiveId(null);
      setPendingAgentType("chat"); // 清残留的待用场景类型,避免 active=null 时回落到 coding 误翻布局
      setMessages([]);
      await reload();
      setDeleteOpen(false);
      toast.success("已删除会话");
    } catch (e) {
      toast.error(`删除失败: ${e}`);
    }
  }

  // 打开「本会话记忆」:拉取当前会话的滚动摘要(只读+可编辑)
  async function openSummary() {
    if (!active) return;
    setSummaryText("");
    setSummaryOpen(true);
    setSummaryLoading(true);
    try {
      const s = await api.getConversationSummary(active.id);
      setSummaryText(s);
    } catch (e) {
      toast.error(`加载会话记忆失败: ${e}`);
    } finally {
      setSummaryLoading(false);
    }
  }

  // 保存「本会话记忆」编辑
  async function saveSummary() {
    if (!active) return;
    setSummarySaving(true);
    try {
      await api.updateConversationSummary(active.id, summaryText);
      setSummaryOpen(false);
      toast.success("已保存本会话记忆");
    } catch (e) {
      toast.error(`保存失败: ${e}`);
    } finally {
      setSummarySaving(false);
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
    // 整页全幅:会话列表在侧栏,这里只有消息流 + 输入(模型选择内嵌在输入框第二行),无标题栏、无外层卡片边框
    <div className="flex min-h-0 min-w-0 flex-1 flex-col overflow-hidden">
        {/* 左上角:会话标题(AI 概括)+ 下拉(重命名/删除);右侧分享整段对话。无分隔栏 */}
        {active && (
          <div className="flex shrink-0 items-center justify-between gap-2 px-4 py-2">
            <DropdownMenu>
              <DropdownMenuTrigger asChild>
                <button
                  type="button"
                  className="inline-flex max-w-[70%] items-center gap-1 rounded-md px-1.5 py-1 text-sm font-medium text-foreground transition-colors hover:bg-accent"
                >
                  <span className="truncate">{active.title || "新对话"}</span>
                  <ChevronDown className="size-4 shrink-0 opacity-60" />
                </button>
              </DropdownMenuTrigger>
              <DropdownMenuContent align="start">
                <DropdownMenuItem onClick={() => void openSummary()}>
                  <NotebookPen className="size-4" />
                  本会话记忆
                </DropdownMenuItem>
                <DropdownMenuItem onClick={openRename}>
                  <Pencil className="size-4" />
                  重命名
                </DropdownMenuItem>
                <DropdownMenuItem
                  onClick={openDelete}
                  className="text-destructive focus:text-destructive"
                >
                  <Trash2 className="size-4" />
                  删除
                </DropdownMenuItem>
              </DropdownMenuContent>
            </DropdownMenu>
            <div className="flex shrink-0 items-center gap-0.5">
              {/* 分享:单独图标,点击直接复制整段对话 */}
              <SimpleTooltip content="分享整段对话">
                <Button
                  type="button"
                  variant="ghost"
                  size="icon"
                  className="size-8"
                  onClick={shareConversation}
                >
                  <Share className="size-4" />
                </Button>
              </SimpleTooltip>
              {/* 更多:展开导出 Markdown / PDF */}
              <DropdownMenu>
                <DropdownMenuTrigger asChild>
                  <Button
                    type="button"
                    variant="ghost"
                    size="icon"
                    className="size-8"
                    title="更多 / 导出"
                  >
                    <MoreHorizontal className="size-4" />
                  </Button>
                </DropdownMenuTrigger>
                <DropdownMenuContent align="end" className="w-44">
                  <DropdownMenuItem
                    onClick={() => void downloadMarkdown()}
                    className="whitespace-nowrap"
                  >
                    <FileText className="size-4" />
                    导出 Markdown
                  </DropdownMenuItem>
                  <DropdownMenuItem
                    onClick={downloadPdf}
                    className="whitespace-nowrap"
                  >
                    <FileDown className="size-4" />
                    导出 PDF
                  </DropdownMenuItem>
                </DropdownMenuContent>
              </DropdownMenu>
            </div>
          </div>
        )}
        {/* 消息区。scrollbar-gutter both-edges:两侧对称预留滚动条位,使内容列与输入框等宽对齐;
            pb-[54px]:加上隐藏的操作栏(~26px)后,最后一条消息与输入框的间距 ≈ 80px */}
        <div
          ref={scrollRef}
          onScroll={onMessagesScroll}
          className="veltrix-thin-scrollbar min-h-0 flex-1 overflow-y-auto px-4 pt-4 pb-[54px] [scrollbar-gutter:stable_both-edges]"
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
                  {/* 品牌 logo 标识(与侧栏一致:Radar + 圆角主题色块) */}
                  <div className="mx-auto mb-3 flex size-14 items-center justify-center rounded-2xl bg-primary text-primary-foreground shadow-sm">
                    <Radar className="size-7" />
                  </div>
                  <p className="text-sm">你好,有什么可以帮你的?</p>
                </div>
              )}
            </div>
          ) : (
            <div ref={messagesContentRef} className="mx-auto max-w-3xl space-y-3.5">
              {messages.map((m) => (
                <MessageBubble
                  key={m.id}
                  message={m}
                  feedback={feedback[m.id]}
                  onCopy={copyToClipboard}
                  onRegenerate={regenerate}
                  onToggleFeedback={toggleFeedback}
                  onPreviewImage={setHistoryImagePreview}
                />
              ))}
              {/* 流式生成中:无头像,Markdown 实时渲染;首 token 前显示三点跳动 loading 动画 */}
              {sending && (
                <div>
                  {streamingContent ? (
                    <div className="text-foreground">
                      <MarkdownMessage content={streamingContent} streaming />
                      {/* 流式生成中:内容下方持续显示三点跳动 loading */}
                      <div className="mt-1.5 inline-flex items-center gap-1">
                        <span className="size-1.5 animate-bounce rounded-full bg-muted-foreground/70 [animation-delay:-0.3s]" />
                        <span className="size-1.5 animate-bounce rounded-full bg-muted-foreground/70 [animation-delay:-0.15s]" />
                        <span className="size-1.5 animate-bounce rounded-full bg-muted-foreground/70" />
                      </div>
                    </div>
                  ) : (
                    // 首 token 前:思考中(转圈)
                    <div className="inline-flex items-center gap-2 rounded-lg bg-muted px-3 py-2 text-sm text-muted-foreground">
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
        <div className="shrink-0 px-4 pb-2">
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
          <div className="mx-auto max-w-3xl">
            {/* 附件 / 图片预览:输入框外侧上方。12 格栅格与输入框等宽、单行不换行;
                图片显示缩略图(可点开预览),其余文件显示方形图标块,均可移除 */}
            {attachments.length > 0 && (
              <div className="mb-2 grid grid-cols-12 gap-1.5">
                {attachments.map((a, i) => (
                  <div
                    key={i}
                    className="group relative aspect-square overflow-hidden rounded-md border bg-muted shadow-sm"
                  >
                    {a.mime.startsWith("image/") ? (
                      <img
                        src={`data:${a.mime};base64,${a.data}`}
                        alt={a.name}
                        className="size-full cursor-zoom-in object-cover"
                        onClick={() => openPreview(a)}
                      />
                    ) : (
                      <div
                        className="flex size-full flex-col items-center justify-center gap-0.5 bg-card p-1 text-muted-foreground"
                        title={a.name}
                      >
                        <FileText className="size-5" />
                        <span className="w-full truncate text-center text-[9px] uppercase">
                          {a.name.split(".").pop() || "file"}
                        </span>
                      </div>
                    )}
                    <button
                      type="button"
                      onClick={() => removeAttachment(i)}
                      className="absolute right-0.5 top-0.5 rounded-full bg-black/55 p-0.5 text-white transition-colors hover:bg-black/80"
                    >
                      <X className="size-3" />
                    </button>
                  </div>
                ))}
              </div>
            )}
            {/* 输入卡片 */}
            <div className="flex flex-col gap-1 rounded-2xl border bg-card p-2 shadow-lg">
            {/* 第一行:纯文本输入。field-sizing-content 随内容增高,
                15px 字号 + 行高 24px(leading-6)+ py-2,max-h-52(208px)= 8 行后出滚动条 */}
            <Textarea
              value={input}
              onChange={(e) => setInput(e.target.value)}
              onKeyDown={(e) => {
                // Enter 发送(忙碌中不发送,但允许继续输入),Shift+Enter 换行
                if (e.key === "Enter" && !e.shiftKey) {
                  e.preventDefault();
                  if (!busy) void handleSend();
                }
              }}
              placeholder={
                recording
                  ? "录音中,再次点击麦克风结束…"
                  : classifying
                    ? "正在判断任务类型…"
                    : sending
                      ? "回复生成中,可继续输入,完成后发送…"
                      : smartSearch
                        ? "智能搜索:输入要搜索的内容,Enter 发送 / Shift+Enter 换行"
                        : "输入消息,Enter 发送 / Shift+Enter 换行"
              }
              className="veltrix-thin-scrollbar max-h-52 min-h-10 w-full resize-none border-0 bg-transparent px-2 py-2 text-[15px] leading-6 tracking-normal shadow-none focus-visible:ring-0 dark:bg-transparent"
              rows={1}
            />
            {/* 第二行:开头放更多功能(附件);结尾依次为模型选择 → 语音 → 发送(发送仅在有文字/附件时出现在语音右侧) */}
            <div className="flex items-center justify-between gap-2">
              {/* 左侧:加号(附件/资产)+ 智能搜索开关 */}
              <div className="flex shrink-0 items-center gap-1">
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
                    {/* 外部附件:从磁盘选文件,限图片 / PDF / Word / Excel / Markdown */}
                    <DropdownMenuItem onClick={() => openPicker(EXTERNAL_ACCEPT)}>
                      <Paperclip className="size-4" />
                      外部附件
                    </DropdownMenuItem>
                    {/* 资产两项:从全量库已采集内容引入(封面与图片已合并到「资产图片」) */}
                    <DropdownMenuItem onClick={() => setAssetPickerMode("image")}>
                      <Images className="size-4" />
                      资产图片
                    </DropdownMenuItem>
                    <DropdownMenuItem onClick={() => setAssetPickerMode("copy")}>
                      <FileText className="size-4" />
                      资产文案
                    </DropdownMenuItem>
                  </DropdownMenuContent>
                </DropdownMenu>
                {/* 智能搜索开关:开启后高亮;点击切换 */}
                <SimpleTooltip
                  content={smartSearch ? "智能搜索已开启" : "开启智能搜索"}
                >
                  <Button
                    type="button"
                    variant="ghost"
                    size="sm"
                    aria-pressed={smartSearch}
                    onClick={() => setSmartSearch((v) => !v)}
                    className={`h-8 shrink-0 gap-1 rounded-xl border px-2 text-xs transition-colors ${
                      smartSearch
                        ? "border-primary/40 bg-primary/10 text-primary hover:bg-primary/15"
                        : "border-border text-muted-foreground hover:bg-accent hover:text-foreground"
                    }`}
                  >
                    智能搜索
                  </Button>
                </SimpleTooltip>
              </div>
              <div className="flex min-w-0 items-center gap-1">
                {/* 语音按钮左侧:两级模型选择(厂商 → 模型),新会话与已有会话都可切换 */}
                {modelOptions.length > 0 ? (
                  <DropdownMenu>
                    <DropdownMenuTrigger asChild>
                      <Button
                        type="button"
                        variant="ghost"
                        size="sm"
                        className="h-8 w-auto max-w-48 gap-1 rounded-xl border border-border px-2 text-xs text-muted-foreground hover:bg-accent hover:text-foreground"
                      >
                        <span className="truncate">{currentModelLabel}</span>
                        <ChevronDown className="size-3.5 shrink-0 opacity-60" />
                      </Button>
                    </DropdownMenuTrigger>
                    <DropdownMenuContent align="end" side="top" className="w-44">
                      {groupedModels.map((g) => (
                        <DropdownMenuSub key={g.providerId}>
                          <DropdownMenuSubTrigger>
                            <span className="truncate">{g.providerName}</span>
                          </DropdownMenuSubTrigger>
                          <DropdownMenuSubContent className="max-h-72 overflow-y-auto">
                            {g.items.map((m) => (
                              <DropdownMenuItem
                                key={m.value}
                                onClick={() => void handleModelChange(m.value)}
                              >
                                <span className="truncate">{m.model}</span>
                                {m.value === selectedModelValue && (
                                  <Check className="ml-auto size-3.5" />
                                )}
                              </DropdownMenuItem>
                            ))}
                          </DropdownMenuSubContent>
                        </DropdownMenuSub>
                      ))}
                    </DropdownMenuContent>
                  </DropdownMenu>
                ) : (
                  <span className="px-1 text-xs text-destructive">
                    未配置模型
                  </span>
                )}
                {/* 语音:默认就在右侧显示 */}
                <SimpleTooltip content={recording ? "结束录音" : "语音输入"}>
                  <Button
                    type="button"
                    variant={recording ? "destructive" : "ghost"}
                    size="icon"
                    className="size-9 shrink-0 cursor-pointer rounded-xl"
                    disabled={transcribing}
                    onClick={toggleRecording}
                  >
                    {transcribing ? (
                      <Loader2 className="animate-spin" />
                    ) : (
                      <Mic className={recording ? "animate-pulse" : ""} />
                    )}
                  </Button>
                </SimpleTooltip>
                {/* 发送:有文字或附件(或正在发送/判断中)时才出现在语音右侧 */}
                {(busy || input.trim() || attachments.length > 0) && (
                  <Button
                    type="button"
                    size="icon"
                    className="size-9 shrink-0 cursor-pointer rounded-xl"
                    disabled={busy}
                    onClick={handleSend}
                  >
                    {busy ? <Loader2 className="animate-spin" /> : <Send />}
                  </Button>
                )}
              </div>
            </div>
            </div>
            {/* AI 生成内容温馨提示:输入框下方 */}
            <p className="mt-2 text-center text-[11px] text-muted-foreground">
              内容由 AI 生成,仅供参考,请注意甄别
            </p>
          </div>
        </div>

        {/* 资产选择弹窗:加号菜单的资产两项共用,mode 决定标题/可选范围与选中后的处理 */}
        <ContentPickerDialog
          open={assetPickerMode !== null}
          mode={assetPickerMode ?? "copy"}
          initialSelectedIds={
            // 文案=内容 id 记忆;图片=直接由当前附件的来源 key 派生
            // (删除附件后 key 随之消失,重开不再勾选)
            assetPickerMode === "image"
              ? attachments
                  .map((a) => a.assetKey)
                  .filter((k): k is string => !!k)
              : assetPickerMode === "copy"
                ? assetSelected.copy
                : []
          }
          onOpenChange={(open) => {
            if (!open) setAssetPickerMode(null);
          }}
          onPick={(result) => {
            // 文案用途记忆已选内容 id(图片用途由附件派生,无需单独记忆)
            if (result.mode === "copy") {
              setAssetSelected((prev) => ({
                ...prev,
                copy: result.contents.map((c) => c.id),
              }));
            }
            void handleAssetPick(result);
          }}
        />

        {/* 图片附件预览灯箱:点缩略图打开;多张时左右切换 + 计数;点背景/Esc 关闭 */}
        {previewIndex !== null && imageAttachments[previewIndex] && (
          <div
            className="fixed inset-0 z-[100] flex items-center justify-center bg-black/80 p-10"
            onClick={() => setPreviewIndex(null)}
          >
            <button
              type="button"
              className="absolute right-4 top-4 rounded-full bg-white/10 p-2 text-white transition-colors hover:bg-white/20"
              onClick={() => setPreviewIndex(null)}
            >
              <X className="size-5" />
            </button>
            {imageAttachments.length > 1 && (
              <button
                type="button"
                className="absolute left-4 top-1/2 -translate-y-1/2 rounded-full bg-white/10 p-2 text-white transition-colors hover:bg-white/20"
                onClick={(e) => {
                  e.stopPropagation();
                  stepPreview(-1);
                }}
              >
                <ChevronLeft className="size-6" />
              </button>
            )}
            <img
              src={`data:${imageAttachments[previewIndex].mime};base64,${imageAttachments[previewIndex].data}`}
              alt={imageAttachments[previewIndex].name}
              className="max-h-full max-w-full object-contain"
              onClick={(e) => e.stopPropagation()}
            />
            {imageAttachments.length > 1 && (
              <button
                type="button"
                className="absolute right-4 top-1/2 -translate-y-1/2 rounded-full bg-white/10 p-2 text-white transition-colors hover:bg-white/20"
                onClick={(e) => {
                  e.stopPropagation();
                  stepPreview(1);
                }}
              >
                <ChevronRight className="size-6" />
              </button>
            )}
            {imageAttachments.length > 1 && (
              <span className="absolute bottom-5 left-1/2 -translate-x-1/2 rounded-full bg-black/50 px-3 py-1 text-xs text-white">
                {previewIndex + 1} / {imageAttachments.length}
              </span>
            )}
          </div>
        )}

        {/* 历史消息图片全屏预览:点消息里的缩略图打开;点背景关闭 */}
        {historyImagePreview && (
          <div
            className="fixed inset-0 z-[100] flex items-center justify-center bg-black/80 p-10"
            onClick={() => setHistoryImagePreview(null)}
          >
            <button
              type="button"
              className="absolute right-4 top-4 rounded-full bg-white/10 p-2 text-white transition-colors hover:bg-white/20"
              onClick={() => setHistoryImagePreview(null)}
            >
              <X className="size-5" />
            </button>
            <img
              src={historyImagePreview}
              alt="预览"
              className="max-h-full max-w-full object-contain"
              onClick={(e) => e.stopPropagation()}
            />
          </div>
        )}

        {/* 重命名会话:自定义弹框(替代原生 prompt) */}
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

        {/* 删除会话:二次确认(与全局删除弹窗统一用 AlertDialog) */}
        <AlertDialog open={deleteOpen} onOpenChange={setDeleteOpen}>
          <AlertDialogContent>
            <AlertDialogHeader>
              <AlertDialogTitle>删除会话</AlertDialogTitle>
              <AlertDialogDescription>
                确定删除「{active?.title || "新对话"}」?此操作不可恢复。
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

        {/* 本会话记忆:当前会话的滚动摘要(查看 / 编辑) */}
        <Dialog open={summaryOpen} onOpenChange={setSummaryOpen}>
          <DialogContent className="sm:max-w-xl">
            <DialogHeader>
              <DialogTitle>本会话记忆</DialogTitle>
              <DialogDescription>
                「{active?.title || "本会话"}」较早内容的压缩摘要,随对话变长自动更新,并在每次回答时作为前情提要提供给 AI。可手动修正。
              </DialogDescription>
            </DialogHeader>
            {summaryLoading ? (
              <div className="flex items-center justify-center py-10 text-sm text-muted-foreground">
                <Loader2 className="mr-2 size-4 animate-spin" />
                加载中…
              </div>
            ) : (
              <Textarea
                value={summaryText}
                onChange={(e) => setSummaryText(e.target.value)}
                placeholder="本会话还没有生成记忆摘要(对话较短时无需摘要)。你也可以在此手动写入要点。"
                className="veltrix-thin-scrollbar max-h-[50vh] min-h-40 resize-y text-sm [field-sizing:content]"
              />
            )}
            <DialogFooter>
              <Button variant="outline" onClick={() => setSummaryOpen(false)}>
                取消
              </Button>
              <Button
                onClick={() => void saveSummary()}
                disabled={summaryLoading || summarySaving}
              >
                {summarySaving && <Loader2 className="size-4 animate-spin" />}
                保存
              </Button>
            </DialogFooter>
          </DialogContent>
        </Dialog>
      </div>
  );
}

// 操作栏小图标按钮
function IconBtn({
  title,
  active,
  onClick,
  children,
}: {
  title: string;
  active?: boolean;
  onClick: () => void;
  children: ReactNode;
}) {
  return (
    <button
      type="button"
      title={title}
      onClick={onClick}
      className={`rounded p-1 transition-colors hover:bg-accent hover:text-foreground ${
        active ? "text-primary" : ""
      }`}
    >
      {children}
    </button>
  );
}

// 单条消息:用户右侧气泡(纯文本),助手左侧全宽(Markdown);各带时间 + 操作栏。
// memo + 稳定回调:流式时已完成消息不重渲染(性能)。
// 历史附件取显示 src:有本地 path 走 asset 协议;否则用乐观消息的内联 base64;都没有返回空。
function messageAttachmentSrc(a: MessageAttachment): string {
  if (a.path) return convertFileSrc(a.path);
  if (a.data) return `data:${a.mime};base64,${a.data}`;
  return "";
}

const MessageBubble = memo(function MessageBubble({
  message,
  feedback,
  onCopy,
  onRegenerate,
  onToggleFeedback,
  onPreviewImage,
}: {
  message: ChatMessageView;
  feedback?: "like" | "dislike";
  onCopy: (text: string, okMsg?: string) => void;
  onRegenerate: (content: string) => void;
  onToggleFeedback: (id: number, v: "like" | "dislike") => void;
  onPreviewImage: (src: string) => void;
}) {
  const isUser = message.role === "user";
  const time = fmtMsgTime(message.createdAt);

  if (isUser) {
    const atts = message.attachments ?? [];
    const images = atts.filter((a) => a.mime.startsWith("image/"));
    const files = atts.filter((a) => !a.mime.startsWith("image/"));
    return (
      <div className="group flex flex-col items-end gap-1">
        {/* 图片附件:右对齐缩略图,点击全屏预览 */}
        {images.length > 0 && (
          <div className="flex max-w-[80%] flex-wrap justify-end gap-1.5">
            {images.map((a, i) => {
              const src = messageAttachmentSrc(a);
              return (
                <img
                  key={i}
                  src={src}
                  alt={a.name}
                  className="size-24 cursor-zoom-in rounded-lg border border-border/60 object-cover"
                  onClick={() => src && onPreviewImage(src)}
                />
              );
            })}
          </div>
        )}
        {/* 非图片附件:文件名 chip */}
        {files.length > 0 && (
          <div className="flex max-w-[80%] flex-wrap justify-end gap-1.5">
            {files.map((a, i) => (
              <div
                key={i}
                className="flex items-center gap-1.5 rounded-lg border border-border/60 bg-muted/40 px-2.5 py-1.5 text-xs text-foreground"
              >
                <FileText className="size-4 shrink-0 text-muted-foreground" />
                <span className="max-w-[160px] truncate">{a.name}</span>
              </div>
            ))}
          </div>
        )}
        {/* 正文气泡:纯图片消息(空正文)不渲染空气泡 */}
        {message.content && (
          <div className="max-w-[80%] whitespace-pre-wrap break-words rounded-lg bg-primary/10 px-3.5 py-2 text-sm leading-relaxed text-foreground">
            {message.content}
          </div>
        )}
        {/* 时间 + 操作图标:都默认隐藏,悬浮在消息上才显示 */}
        <div className="flex items-center gap-1 text-muted-foreground opacity-0 transition-opacity group-hover:opacity-100">
          <span className="mr-0.5 text-[11px]">{time}</span>
          <IconBtn title="复制" onClick={() => onCopy(message.content)}>
            <Copy className="size-3.5" />
          </IconBtn>
          <IconBtn title="重新生成" onClick={() => onRegenerate(message.content)}>
            <RotateCcw className="size-3.5" />
          </IconBtn>
        </div>
      </div>
    );
  }

  return (
    <div className="group flex flex-col items-start gap-1">
      {/* 助手回复:宽度与输入框一致(占满 max-w-3xl 容器),不显示时间 */}
      <div className="w-full">
        <MarkdownMessage content={message.content} />
      </div>
      {/* 操作图标:悬浮在回复上才显示 */}
      <div className="flex items-center gap-1 text-muted-foreground opacity-0 transition-opacity group-hover:opacity-100">
        <IconBtn
          title="赞"
          active={feedback === "like"}
          onClick={() => onToggleFeedback(message.id, "like")}
        >
          <ThumbsUp
            className="size-3.5"
            fill={feedback === "like" ? "currentColor" : "none"}
          />
        </IconBtn>
        <IconBtn
          title="踩"
          active={feedback === "dislike"}
          onClick={() => onToggleFeedback(message.id, "dislike")}
        >
          <ThumbsDown
            className="size-3.5"
            fill={feedback === "dislike" ? "currentColor" : "none"}
          />
        </IconBtn>
        <IconBtn title="复制" onClick={() => onCopy(message.content)}>
          <Copy className="size-3.5" />
        </IconBtn>
      </div>
    </div>
  );
});
