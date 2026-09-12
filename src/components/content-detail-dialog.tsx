// 全屏内容详情弹窗:顶部通栏(翻篇 + 链接操作;非视频另有「概览/文案与评论」页切换)。
// 视频:不分页,单屏三栏 —— 左转写文案+本地音频、中该内容评论(点赞倒序)、右作者卡+内容卡。
// 图文:「概览」= 素材瀑布全图 + 作者卡 + 内容卡;「文案与评论」= 文案 + 评论双栏。
// 素材点开看大图、方向键浏览,看完可续看下一个。
// 键盘:大图未开时 ←/→ 切上一篇/下一篇;大图打开时 ←/→ 翻图(可跨内容续看)。
import { useCallback, useEffect, useRef, useState, type ReactNode } from "react";
import { useMediaFileUrl, mediaThumbPath } from "@/lib/media-file-url";
import { openUrl } from "@tauri-apps/plugin-opener";
import {
  AudioLines,
  CheckCircle2,
  ChevronLeft,
  ChevronRight,
  Clock,
  Copy,
  ExternalLink,
  FileText,
  ImageOff,
  LayoutGrid,
  Loader2,
  MessagesSquare,
  ScanText,
  ThumbsUp,
  User,
  X,
  XCircle,
} from "lucide-react";
import { toast } from "sonner";

import { Dialog, DialogContent, DialogTitle } from "@/components/ui/dialog";
import { Button } from "@/components/ui/button";
import { Separator } from "@/components/ui/separator";
import { Switch } from "@/components/ui/switch";
import { SimpleTooltip } from "@/components/SimpleTooltip";
import { api, type CommentView, type ContentDetailView, type ContentListView, type ContentView } from "@/lib/api";
import {
  authorProfileUrl,
  contentDetailUrl,
  platformClass,
} from "@/lib/platforms";

// 数字格式:>=1w 用「x.xw」,否则千分位;null/缺失显示「—」
function fmtCount(n: number | null | undefined): string {
  if (n == null) return "—";
  if (n >= 10000) {
    const w = n / 10000;
    return `${Number.isInteger(w) ? w : w.toFixed(1)}w`;
  }
  return n.toLocaleString();
}

function fmtDate(ts: number | null | undefined): string {
  if (!ts) return "—";
  const d = new Date(ts * 1000);
  const mm = String(d.getMonth() + 1).padStart(2, "0");
  const dd = String(d.getDate()).padStart(2, "0");
  return `${d.getFullYear()}年${mm}月${dd}日`;
}

function fmtDuration(sec: number | null | undefined): string {
  if (!sec || sec <= 0) return "—";
  const m = Math.floor(sec / 60);
  const s = sec % 60;
  return `${m}:${String(s).padStart(2, "0")}`;
}

const KIND_LABEL: Record<string, string> = {
  video: "视频",
  image: "图文",
  article: "文章",
  unknown: "未知",
};

// 评论意向等级文案(与评论库 INTENT_META 口径一致);none/未分析不展示
const INTENT_LABEL: Record<string, string> = {
  high: "高意向",
  medium: "中意向",
  low: "低意向",
};

// 一张图的显示源:src 主源 + 可选 fallback(本地缺失时回退外链)
interface ImageEntry {
  src: string;
  fallback?: string;
}

// 取某条内容的全部图片显示源:优先 image_urls,为空回退 cover_url / 本地封面;
// 第一张本地优先(cover_path 下载成功),失败再回退原外链。
function imageEntries(c: ContentView, mediaFileUrl: (path: string) => string): ImageEntry[] {
  const ext = c.imageUrls.filter((u) => !!u);
  const list: ImageEntry[] =
    ext.length > 0
      ? c.imageUrls.flatMap((u, index) => {
          const path = c.imagePaths?.[index];
          return path ? [{ src: mediaFileUrl(path), fallback: u || undefined }] : u ? [{ src: u }] : [];
        })
      : c.coverUrl
        ? [{ src: c.coverUrl }]
        : c.coverPath
          ? [{ src: mediaFileUrl(c.coverPath) }]
          : [];
  if (list.length > 0 && c.coverPath && !c.imagePaths?.[0]) {
    list[0] = { src: mediaFileUrl(c.coverPath), fallback: list[0].src };
  }
  return list;
}

// 带本地→外链→隐藏三级回退的图片(详情左栏与大图共用)。
// 回退状态必须用 React state 而非直改 DOM:详情弹窗是单实例,切换内容时 React 复用
// 同一个 <img>,直改 style.display 会让一张失败封面把后续所有内容的封面永久藏住。
function FallbackImage({
  src,
  fallback,
  className,
  draggable,
  onClick,
}: {
  src: string;
  fallback?: string;
  className: string;
  draggable?: boolean;
  onClick?: (e: React.MouseEvent<HTMLImageElement>) => void;
}) {
  const [stage, setStage] = useState<"primary" | "fallback" | "hidden">(
    "primary",
  );
  // 换源(切换内容/翻图)即复位,上一张的失败状态不残留
  useEffect(() => {
    setStage("primary");
  }, [src, fallback]);
  if (stage === "hidden") return (
    <div className={`${className} flex min-h-32 items-center justify-center bg-muted text-sm text-muted-foreground`}>
      图片加载失败
    </div>
  );
  const current = stage === "primary" ? src : (fallback ?? src);
  return (
    <img
      src={current}
      referrerPolicy="no-referrer"
      alt=""
      loading="lazy"
      // 异步解码:大图(瀑布全图 / 大图浏览)在后台线程解码,不阻塞翻图与切换的主线程
      decoding="async"
      draggable={draggable}
      className={className}
      onClick={onClick}
      onError={() => {
        if (stage === "primary" && fallback) {
          setStage("fallback");
        } else {
          setStage("hidden");
        }
      }}
    />
  );
}

// 可复制字段:label + 值 + 复制按钮
function CopyField({ label, value }: { label: string; value: string }) {
  if (!value) return null;
  return (
    <div className="flex items-center gap-1 text-xs text-muted-foreground">
      <span>
        {label}:
        <span className="ml-0.5 font-mono text-foreground">{value}</span>
      </span>
      <SimpleTooltip content="复制">
        <button
          type="button"
          className="cursor-pointer text-muted-foreground transition-colors hover:text-foreground"
          onClick={() => {
            navigator.clipboard
              .writeText(value)
              .then(() => toast.success("已复制"))
              .catch(() => toast.error("复制失败"));
          }}
        >
          <Copy className="size-3" />
        </button>
      </SimpleTooltip>
    </div>
  );
}

// 素材步骤成功态按类型配色,与全量库素材列一致(失败统一红、待处理统一灰)
const STEP_SUCCESS_CLS: Record<string, string> = {
  视频: "bg-sky-100 text-sky-700 dark:bg-sky-950/60 dark:text-sky-300",
  音频: "bg-violet-100 text-violet-700 dark:bg-violet-950/60 dark:text-violet-300",
  文案: "bg-amber-100 text-amber-700 dark:bg-amber-950/60 dark:text-amber-300",
  空文案: "bg-amber-100 text-amber-700 dark:bg-amber-950/60 dark:text-amber-300",
  图片: "bg-teal-100 text-teal-700 dark:bg-teal-950/60 dark:text-teal-300",
  评论: "bg-blue-100 text-blue-700 dark:bg-blue-950/60 dark:text-blue-300",
  意向: "bg-fuchsia-100 text-fuchsia-700 dark:bg-fuchsia-950/60 dark:text-fuchsia-300",
};

function stepSuccessCls(label: string): string {
  for (const [key, cls] of Object.entries(STEP_SUCCESS_CLS)) {
    if (label.startsWith(key)) return cls;
  }
  return "bg-emerald-100 text-emerald-700 dark:bg-emerald-950/60 dark:text-emerald-300";
}

// 素材处理状态小标:true=按类型彩色✓ / false=红✗(tooltip 带失败原因)/ null=灰待处理
function StateBadge({
  label,
  state,
  errorTip,
}: {
  label: string;
  state: boolean | null;
  errorTip?: string | null;
}) {
  const cls =
    state === true
      ? stepSuccessCls(label)
      : state === false
        ? "bg-rose-100 text-rose-700 dark:bg-rose-950/60 dark:text-rose-300"
        : "bg-muted text-muted-foreground";
  const Icon =
    state === true ? CheckCircle2 : state === false ? XCircle : Clock;
  const badge = (
    <span
      className={`inline-flex items-center gap-0.5 whitespace-nowrap rounded px-1.5 py-0.5 text-[10px] font-medium ${cls}`}
    >
      <Icon className="size-3" />
      {label}
    </span>
  );
  return state === false && errorTip ? (
    <SimpleTooltip content={errorTip}>
      <span className="cursor-help">{badge}</span>
    </SimpleTooltip>
  ) : (
    badge
  );
}

// 正文分页切换按钮(样式与全量库视图切换一致:选中=主色填充)
function PageButton({
  active,
  label,
  icon: Icon,
  onClick,
}: {
  active: boolean;
  label: string;
  icon: typeof LayoutGrid;
  onClick: () => void;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      className={`inline-flex h-full cursor-pointer items-center gap-1 rounded px-2.5 text-xs font-medium transition-colors ${
        active
          ? "bg-primary text-primary-foreground"
          : "text-muted-foreground hover:text-foreground"
      }`}
    >
      <Icon className="size-3.5" />
      {label}
    </button>
  );
}

// 统计格:边框卡片,上 label 下 value
function StatCell({  label,
  value,
  accent,
}: {
  label: string;
  value: ReactNode;
  accent?: "blue" | "red";
}) {
  const valueCls =
    accent === "blue"
      ? "text-sky-600 dark:text-sky-400"
      : accent === "red"
        ? "text-rose-600 dark:text-rose-400"
        : "text-foreground";
  return (
    <div className="rounded-md border bg-card px-3 py-2">
      <div className="text-[11px] text-muted-foreground">{label}</div>
      <div className={`mt-0.5 text-sm font-semibold ${valueCls}`}>{value}</div>
    </div>
  );
}

export function ContentDetailDialog({
  items,
  activeId,
  onActiveIdChange,
}: {
  // 当前筛选后的内容列表:用于「上一篇/下一篇」与大图跨内容续看
  items: ContentListView[];
  // 当前打开的内容 id(null=关闭)
  activeId: string | null;
  // 切换内容 / 关闭(传 null)
  onActiveIdChange: (id: string | null) => void;
}) {
  const mediaFileUrl = useMediaFileUrl();
  const [detail, setDetail] = useState<ContentDetailView | null>(null);
  const [loading, setLoading] = useState(false);
  const [monitoring, setMonitoring] = useState(false);
  // 右侧评论栏:当前内容的评论列表(点赞倒序,游标分页「加载更多」)
  const [comments, setComments] = useState<CommentView[]>([]);
  // 该内容评论总数(标题展示;首屏响应才带)
  const [commentsTotal, setCommentsTotal] = useState<number | null>(null);
  // 下一页游标;null = 已到底,隐藏「加载更多」
  const [nextCursor, setNextCursor] = useState<string | null>(null);
  const [commentsLoading, setCommentsLoading] = useState(false);
  const [commentsLoadingMore, setCommentsLoadingMore] = useState(false);
  // 请求竞态保护:快速切上一篇/下一篇时,过期响应(序号不匹配)直接丢弃
  const commentsReqSeq = useRef(0);
  // 正文分页(仅图文等非视频内容):overview=素材 + 作者/内容卡;text=文案与评论通栏双栏。
  // 视频不分页(单屏三栏),该状态不影响视频;切上一篇/下一篇时保持当前页
  const [page, setPage] = useState<"overview" | "text">("overview");
  // 大图浏览态:定位到某条内容的第几张图(null=未打开大图)
  const [lightbox, setLightbox] = useState<{
    itemIndex: number;
    imgIndex: number;
  } | null>(null);

  const open = !!activeId;
  const currentIndex = items.findIndex((x) => x.id === activeId);

  useEffect(() => {
    if (!activeId) {
      setDetail(null);
      setPage("overview"); // 关闭后复位,下次打开回到概览页
      return;
    }
    setLoading(true);
    api
      .getContentDetail(activeId)
      .then(setDetail)
      .catch((e) => toast.error(`加载详情失败: ${e}`))
      .finally(() => setLoading(false));
  }, [activeId]);

  // 评论栏:随内容切换加载首页评论(与详情并行,互不阻塞);分页由「加载更多」续拉
  useEffect(() => {
    if (!activeId) {
      setComments([]);
      setCommentsTotal(null);
      setNextCursor(null);
      return;
    }
    const seq = ++commentsReqSeq.current;
    // 切换内容先清空旧列表,避免上一篇的评论在新内容加载期间闪显
    setComments([]);
    setCommentsTotal(null);
    setNextCursor(null);
    setCommentsLoading(true);
    api
      .listContentComments(activeId)
      .then((page) => {
        if (seq !== commentsReqSeq.current) return; // 已切到其他内容,丢弃过期响应
        setComments(page.items);
        setCommentsTotal(page.total);
        setNextCursor(page.nextCursor);
      })
      .catch((e) => {
        if (seq !== commentsReqSeq.current) return;
        console.warn("加载评论失败:", e);
        setComments([]);
        setCommentsTotal(null);
      })
      .finally(() => {
        if (seq === commentsReqSeq.current) setCommentsLoading(false);
      });
  }, [activeId]);

  // 「加载更多」:按游标续拉下一页并追加到列表末尾
  const loadMoreComments = () => {
    if (!activeId || !nextCursor || commentsLoadingMore) return;
    const seq = commentsReqSeq.current; // 沿用当前内容的序号,内容切换后旧页响应被丢弃
    setCommentsLoadingMore(true);
    api
      .listContentComments(activeId, nextCursor)
      .then((page) => {
        if (seq !== commentsReqSeq.current) return;
        setComments((prev) => [...prev, ...page.items]);
        setCommentsTotal(page.total);
        setNextCursor(page.nextCursor);
      })
      .catch((e) => {
        if (seq !== commentsReqSeq.current) return;
        console.warn("加载更多评论失败:", e);
        toast.error(`加载更多评论失败: ${e}`);
      })
      .finally(() => {
        if (seq === commentsReqSeq.current) setCommentsLoadingMore(false);
      });
  };

  const content = detail?.content;
  const author = detail?.author;
  // 大图跨内容续看需要完整图集,但列表行已是瘦身视图(无 imageUrls/imagePaths):
  // 按内容 id 缓存详情接口的完整视图,首次跨入某内容时拉取一次
  const detailCache = useRef(new Map<string, ContentView>());
  const detailForItem = useCallback(
    async (id: string): Promise<ContentView | null> => {
      if (content?.id === id) return content;
      const cached = detailCache.current.get(id);
      if (cached) return cached;
      try {
        const d = await api.getContentDetail(id);
        detailCache.current.set(id, d.content);
        return d.content;
      } catch (e) {
        console.warn("加载大图内容详情失败:", e);
        return null;
      }
    },
    [content],
  );
  // 作者头像:本地优先(下载成功用 asset 协议),失败回退外链
  const avatarSrc = author?.avatarPath
    ? mediaFileUrl(mediaThumbPath(author.avatarPath))
    : (author?.avatar ?? "");
  const avatarFallback = author?.avatarPath ? (author.avatar ?? "") : undefined;

  // 是否视频:视频单屏三栏、不分页。用列表项 kind 兜底,详情未加载完也能定版式
  const isVideo =
    (content ?? (currentIndex >= 0 ? items[currentIndex] : undefined))?.kind ===
    "video";

  // 列表顺序的上一篇/下一篇(不跳过无图内容,视频也能看信息)
  const hasPrev = currentIndex > 0;
  const hasNext = currentIndex >= 0 && currentIndex < items.length - 1;
  const goPrev = useCallback(() => {
    if (hasPrev) onActiveIdChange(items[currentIndex - 1].id);
  }, [hasPrev, items, currentIndex, onActiveIdChange]);
  const goNext = useCallback(() => {
    if (hasNext) onActiveIdChange(items[currentIndex + 1].id);
  }, [hasNext, items, currentIndex, onActiveIdChange]);

  // 当前内容的大图必须使用详情接口返回的完整图集。列表项只是搜索摘要,小红书通常
  // 只有一张封面;此前瀑布流虽显示详情图,点开后又退回列表项,导致预览只剩第一张。
  const entriesForItem = useCallback(
    (itemIndex: number) => {
      const item = items[itemIndex];
      if (!item) return [];
      // 列表行是瘦身视图(无完整图集):优先用详情缓存,未缓存时封面兜底一帧;
      // stepLightbox 跨入前会 await 详情落缓存,正常浏览不会停在这一帧
      const view = content?.id === item.id ? content : detailCache.current.get(item.id);
      if (!view) {
        const cover = item.coverPath
          ? mediaFileUrl(item.coverPath)
          : item.coverUrl || item.firstImageUrl || "";
        return cover ? [{ src: cover }] : [];
      }
      return imageEntries(view, mediaFileUrl);
    },
    [content, items, mediaFileUrl],
  );

  // 大图翻页:在当前内容图片间走;越过边界则顺延到上/下一个内容并同步右侧详情。
  const stepLightbox = useCallback(
    async (dir: 1 | -1) => {
      if (!lightbox) return;
      let it = lightbox.itemIndex;
      let im = lightbox.imgIndex + dir;
      while (it >= 0 && it < items.length) {
        // 跨内容续看需要完整图集:瘦身列表行只有封面,先拉详情(带缓存)再判定落点
        const view = await detailForItem(items[it].id);
        const imgs = view ? imageEntries(view, mediaFileUrl) : entriesForItem(it);
        if (im < 0) im = imgs.length - 1; // 从右端进入上一内容
        if (im >= 0 && im < imgs.length) {
          setLightbox({ itemIndex: it, imgIndex: im });
          if (items[it].id !== activeId) onActiveIdChange(items[it].id);
          return;
        }
        it += dir;
        if (it < 0 || it >= items.length) return; // 到整个序列两端,停住
        im = dir === 1 ? 0 : -1;
      }
    },
    [lightbox, items, activeId, onActiveIdChange, entriesForItem, detailForItem, mediaFileUrl],
  );

  // 大图打开时:← → 翻图(可跨内容续看)。Esc 由嵌套 Dialog(Radix)自行关闭,不在此处理。
  useEffect(() => {
    if (!lightbox) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "ArrowRight") {
        e.preventDefault();
        stepLightbox(1);
      } else if (e.key === "ArrowLeft") {
        e.preventDefault();
        stepLightbox(-1);
      }
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [lightbox, stepLightbox]);

  // 大图未开时:← → 直接切上一篇/下一篇,连续浏览不用回鼠标点按钮
  useEffect(() => {
    if (!open || lightbox) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "ArrowRight") {
        e.preventDefault();
        goNext();
      } else if (e.key === "ArrowLeft") {
        e.preventDefault();
        goPrev();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [open, lightbox, goPrev, goNext]);

  const profileUrl = content
    ? authorProfileUrl(content.platform, author?.uid, author?.platformId)
    : null;
  const originUrl = content
    ? contentDetailUrl(content.platform, content.contentId, content.xsecToken) || content.videoUrl
    : null;

  // 左栏图片:以已加载详情为准
  const entries = content ? imageEntries(content, mediaFileUrl) : [];
  const openLightbox = (imgIndex: number) => {
    if (currentIndex < 0) return;
    setLightbox({ itemIndex: currentIndex, imgIndex });
  };

  // 大图当前帧
  const lbItem = lightbox ? items[lightbox.itemIndex] : null;
  const lbEntries = lightbox ? entriesForItem(lightbox.itemIndex) : [];
  const lbEntry = lightbox ? lbEntries[lightbox.imgIndex] : null;

  function toggleMonitor(next: boolean) {
    if (!activeId || !author) return;
    setMonitoring(true);
    api
      .setAuthorMonitored(activeId, next)
      .then(() => {
        setDetail((prev) =>
          prev
            ? { ...prev, author: { ...prev.author, isMonitored: next } }
            : prev,
        );
        toast.success(next ? "已开启作者监控" : "已关闭作者监控");
      })
      .catch((e) => toast.error(`操作失败: ${e}`))
      .finally(() => setMonitoring(false));
  }

  // 文案栏:按内容形式显示对应文案——视频=语音转写文案,图文/文章=封面 OCR 文字。
  // withAudio(视频三栏左栏)时不套卡片、文案直接铺开,底部钉本地音频
  // 播放器(旧数据已提取但未记录路径时给出指引);图文「文案与评论」页保留通栏卡片
  const transcriptColumn = (withAudio: boolean) => {
    const isVideo = content?.kind === "video";
    // 图文没有语音文案,主文案位显示封面 OCR 文字;三态口径同转写
    const text = isVideo ? content?.transcript : content?.coverOcrText;
    const textError = isVideo ? content?.transcriptError : content?.coverOcrError;
    const emptyText =
      loading && !content
        ? "加载中…"
        : textError
          ? `${isVideo ? "转写" : "识别"}失败:${textError}`
          : isVideo
            ? "暂无文案(未开启 AI 文案提取,或转写尚未完成)"
            : "暂无封面文字(未开启封面 OCR,或识别尚未完成)";
    return (
    <div className="flex min-h-0 flex-1 flex-col bg-muted/20 p-4">
      <div
        className={
          withAudio
            ? "flex min-h-0 w-full flex-1 flex-col"
            : "mx-auto flex min-h-0 w-full max-w-4xl flex-1 flex-col rounded-lg border bg-card shadow-sm"
        }
      >
        <div
          className={`flex items-center gap-1.5 px-3 py-2 text-sm font-medium text-muted-foreground ${withAudio ? "" : "border-b"}`}
        >
          {isVideo ? <AudioLines className="size-4" /> : <ScanText className="size-4" />}
          {isVideo ? "视频文案" : "封面文字"}
          {text && (
            <SimpleTooltip content="复制全文">
              <button
                type="button"
                className="ml-auto cursor-pointer text-muted-foreground transition-colors hover:text-foreground"
                onClick={() => {
                  navigator.clipboard
                    ?.writeText(text ?? "")
                    .then(() => toast.success(isVideo ? "已复制视频文案" : "已复制封面文字"))
                    .catch(() => toast.error("复制失败"));
                }}
              >
                <Copy className="size-4" />
              </button>
            </SimpleTooltip>
          )}
        </div>
        <div
          className={`veltrix-thin-scrollbar min-h-0 flex-1 overflow-y-auto whitespace-pre-wrap text-sm leading-relaxed ${withAudio ? "px-3 pb-4" : "p-4"}`}
        >
          {text ? (
            text
          ) : text === "" ? (
            <span className="text-muted-foreground">
              {isVideo
                ? "转写完成,未识别到语音(空文案)"
                : "识别完成,封面上没有可识别的文字"}
            </span>
          ) : (
            <span className="text-muted-foreground">{emptyText}</span>
          )}
        </div>
      </div>
      {/* 封面文字补充块仅视频显示(图文的封面文字即主文案,避免重复);
          OCR 三态:非空=识别文本 / 空串=封面无文字 / null+error=识别失败;未跑过识别(全 null)不占位 */}
      {isVideo && (content?.coverOcrText != null || content?.coverOcrError) && (
        <div className="mt-3 w-full shrink-0 rounded-md border bg-muted/40 px-3 py-2 text-xs">
          <div className="flex items-center gap-1 font-medium text-muted-foreground">
            <ScanText className="size-3.5" />
            封面文字
          </div>
          <p className="mt-1 whitespace-pre-wrap break-words text-foreground">
            {content?.coverOcrText
              ? content.coverOcrText
              : content?.coverOcrText === ""
                ? "识别完成,封面上没有可识别的文字"
                : `识别失败:${content?.coverOcrError ?? ""}`}
          </p>
        </div>
      )}
      {withAudio &&
        (content?.audioPath ? (
          <audio
            controls
            preload="metadata"
            src={mediaFileUrl(content.audioPath)}
            className="mt-3 w-full shrink-0"
          />
        ) : content?.audioExtracted ? (
          <div className="mt-3 w-full shrink-0 rounded-md border bg-muted/40 px-3 py-2 text-xs text-muted-foreground">
            音频已提取,但该条为旧数据未记录文件路径;在列表「重新拉取素材」后即可在此播放。
          </div>
        ) : null)}
    </div>
    );
  };

  // 评论栏:该内容的评论列表(点赞倒序,热评在前)
  const commentsColumn = (widthCls: string) => (
    <div className={`flex ${widthCls} shrink-0 flex-col border-l`}>
      <div className="flex items-center gap-1.5 border-b px-3 py-2 text-sm font-medium text-muted-foreground">
        <MessagesSquare className="size-4" />
        评论 · {commentsTotal ?? comments.length}
        {commentsLoading && <Loader2 className="size-3.5 animate-spin" />}
      </div>
      <div className="veltrix-thin-scrollbar min-h-0 flex-1 overflow-y-auto">
        {comments.length === 0 && !commentsLoading ? (
          <div className="flex h-full items-center justify-center p-4 text-center text-xs text-muted-foreground">
            暂无评论(未采集评论,或该内容没有评论)
          </div>
        ) : (
          comments.map((cm) => (
            <div
              key={cm.id}
              className="flex gap-2 border-b px-3 py-2.5 last:border-0"
            >
              {cm.authorAvatar ? (
                <img
                  src={cm.authorAvatar}
                  alt=""
                  loading="lazy"
                  referrerPolicy="no-referrer"
                  className="size-7 shrink-0 rounded-full border object-cover"
                  onError={(e) => {
                    (e.currentTarget as HTMLImageElement).style.display =
                      "none";
                  }}
                />
              ) : (
                <div className="flex size-7 shrink-0 items-center justify-center rounded-full border bg-muted text-muted-foreground">
                  <User className="size-3.5" />
                </div>
              )}
              <div className="min-w-0 flex-1">
                <div className="flex items-center gap-1.5 text-xs">
                  <span className="truncate font-medium">
                    {cm.authorNickname || "匿名"}
                  </span>
                  {cm.intentLevel && INTENT_LABEL[cm.intentLevel] && (
                    <span className="shrink-0 rounded bg-fuchsia-500/10 px-1 text-[10px] text-fuchsia-600 dark:text-fuchsia-400">
                      {INTENT_LABEL[cm.intentLevel]}
                    </span>
                  )}
                  <span className="ml-auto shrink-0 text-muted-foreground">
                    {fmtDate(cm.createdAt)}
                  </span>
                </div>
                <p className="mt-0.5 whitespace-pre-wrap break-words text-sm leading-snug">
                  {cm.text}
                </p>
                <div className="mt-0.5 flex items-center gap-1 text-[11px] text-muted-foreground">
                  <ThumbsUp className="size-3" />
                  {fmtCount(cm.likeCount)}
                </div>
              </div>
            </div>
          ))
        )}
        {/* 游标分页:还有下一页时显示「加载更多」,已到底不显示 */}
        {nextCursor && (
          <div className="flex justify-center border-t px-3 py-2">
            <Button
              variant="ghost"
              size="sm"
              className="cursor-pointer text-xs text-muted-foreground"
              disabled={commentsLoadingMore}
              onClick={loadMoreComments}
            >
              {commentsLoadingMore && (
                <Loader2 className="size-3.5 animate-spin" />
              )}
              加载更多(已显示 {comments.length} / {commentsTotal ?? "…"})
            </Button>
          </div>
        )}
      </div>
    </div>
  );

  return (
    <>
      <Dialog
        open={open}
        onOpenChange={(o) => {
          if (!o) {
            setLightbox(null);
            onActiveIdChange(null);
          }
        }}
      >
        <DialogContent className="flex h-[92vh] w-[96vw] max-w-[96vw] flex-col gap-0 overflow-hidden p-0 sm:max-w-[96vw]">
          <DialogTitle className="sr-only">内容详情</DialogTitle>

          {/* 顶部通栏:上一篇/下一篇 + 页面切换 + 链接操作(右上留白给关闭按钮) */}
          <div className="flex items-center gap-2 border-b px-4 py-2.5 pr-12">
            <SimpleTooltip content="上一篇">
              <Button
                variant="outline"
                size="icon-sm"
                className="cursor-pointer"
                disabled={!hasPrev}
                onClick={goPrev}
              >
                <ChevronLeft />
              </Button>
            </SimpleTooltip>
            <SimpleTooltip content="下一篇">
              <Button
                variant="outline"
                size="icon-sm"
                className="cursor-pointer"
                disabled={!hasNext}
                onClick={goNext}
              >
                <ChevronRight />
              </Button>
            </SimpleTooltip>
            {currentIndex >= 0 && (
              <span className="text-xs text-muted-foreground">
                第 {currentIndex + 1} / {items.length} 篇
              </span>
            )}
            {/* 切换中:保留上一条内容展示,仅以小菊花提示在刷新,避免整屏闪空 */}
            {loading && detail && (
              <Loader2 className="size-3.5 animate-spin text-muted-foreground" />
            )}
            {/* 页面切换:概览(素材+作者/内容卡)/ 文案与评论(通栏双栏);视频单屏三栏,无需切换 */}
            {!isVideo && (
              <div className="mx-auto inline-flex h-8 items-center rounded-md border p-0.5">
                <PageButton
                  active={page === "overview"}
                  label="概览"
                  icon={LayoutGrid}
                  onClick={() => setPage("overview")}
                />
                <PageButton
                  active={page === "text"}
                  label="文案与评论"
                  icon={FileText}
                  onClick={() => setPage("text")}
                />
              </div>
            )}
            <span className="ml-auto inline-flex items-center gap-1">
              <SimpleTooltip content="复制原文链接">
                <Button
                  variant="outline"
                  size="icon-sm"
                  className="cursor-pointer"
                  disabled={!originUrl}
                  onClick={() => {
                    if (!originUrl) return;
                    navigator.clipboard
                      ?.writeText(originUrl)
                      .then(() => toast.success("已复制原文链接"))
                      .catch(() => toast.error("复制失败"));
                  }}
                >
                  <Copy />
                </Button>
              </SimpleTooltip>
              <SimpleTooltip content="打开原文">
                <Button
                  variant="outline"
                  size="icon-sm"
                  className="cursor-pointer"
                  disabled={!originUrl}
                  onClick={() => {
                    if (!originUrl) return;
                    openUrl(originUrl).catch((e) =>
                      toast.error(`打开失败: ${e}`),
                    );
                  }}
                >
                  <ExternalLink />
                </Button>
              </SimpleTooltip>
            </span>
          </div>

          {/* 左侧区域按内容形式/分页切换:视频=文案+音频、评论两栏(不分页);
              图文概览=素材区;图文「文案与评论」=文案+评论。
              右栏作者卡+内容卡各页共用,始终钉在最右 */}
          <div className="flex min-h-0 flex-1">
          {isVideo ? (
          <>
            {transcriptColumn(true)}
            {commentsColumn("w-[520px]")}
          </>
          ) : page === "overview" ? (
          <>
          {/* 素材区:图文瀑布流全图(点开看大图) */}
          <div className="flex min-h-0 flex-1 flex-col bg-muted/20">
          <div className="veltrix-thin-scrollbar min-h-0 flex-1 overflow-y-auto p-4">
            {entries.length > 0 ? (
              // 瀑布流:按图片原始比例错落排列,break-inside-avoid 防跨列断裂
              <div className="columns-2 gap-3 sm:columns-3 xl:columns-4 [&>*]:mb-3">
                {entries.map((it, i) => (
                  <div key={i} className="relative break-inside-avoid">
                    <FallbackImage
                      src={it.src}
                      fallback={it.fallback}
                      onClick={() => openLightbox(i)}
                      className="w-full cursor-zoom-in rounded-lg border bg-card shadow-sm transition hover:opacity-95"
                    />
                    {entries.length > 1 && (
                      <span className="pointer-events-none absolute left-2 top-2 rounded bg-black/55 px-1.5 py-0.5 text-[11px] font-medium text-white backdrop-blur-sm">
                        {i + 1} / {entries.length}
                      </span>
                    )}
                  </div>
                ))}
              </div>
            ) : (
              <div className="flex h-full items-center justify-center text-sm text-muted-foreground">
                <div className="text-center">
                  <ImageOff className="mx-auto mb-2 size-8 opacity-40" />
                  {loading && !content ? "加载中…" : "暂无图片素材"}
                </div>
              </div>
            )}
          </div>
          </div>
          </>
          ) : (
          /* 文案与评论页(图文):左文案、右该内容评论列表(点赞倒序) */
          <>
            {transcriptColumn(false)}
            {commentsColumn("w-[480px]")}
          </>
          )}
          {/* 右栏:作者卡 + 内容卡(各页共用,始终钉在最右) */}
          <div
            className={`flex ${isVideo ? "w-[440px]" : "w-[520px]"} shrink-0 flex-col border-l`}
          >
            <div className="veltrix-thin-scrollbar min-h-0 flex-1 overflow-y-auto p-5">
              {loading && !detail ? (
                <div className="py-16 text-center text-sm text-muted-foreground">
                  加载中…
                </div>
              ) : content && author ? (
                <div className="space-y-4">
                  {/* 作者区 */}
                  <section className="space-y-3">
                    {/* 头像 + 昵称:点头像跳作者主页(无需单独「去主页」按钮) */}
                    <div className="flex items-center gap-3">
                      {avatarSrc ? (
                        <SimpleTooltip
                          content={profileUrl ? "打开作者主页" : "暂无主页链接"}
                        >
                          <FallbackImage
                            src={avatarSrc}
                            fallback={avatarFallback}
                            onClick={
                              profileUrl
                                ? () =>
                                    openUrl(profileUrl).catch((e) =>
                                      toast.error(`打开失败: ${e}`),
                                    )
                                : undefined
                            }
                            className={`size-12 shrink-0 rounded-full border object-cover transition ${
                              profileUrl
                                ? "cursor-pointer hover:ring-2 hover:ring-primary hover:ring-offset-1"
                                : ""
                            }`}
                          />
                        </SimpleTooltip>
                      ) : (
                        <div className="flex size-12 shrink-0 items-center justify-center rounded-full border bg-muted text-muted-foreground">
                          <User className="size-5" />
                        </div>
                      )}
                      <h2 className="min-w-0 flex-1 truncate text-lg font-bold">
                        {author.nickname || "未知作者"}
                      </h2>
                    </div>
                    <div className="flex flex-wrap gap-x-4 gap-y-1">
                      <CopyField
                        label="ID"
                        value={author.shortId || author.uid}
                      />
                      <CopyField label="sec" value={author.uid} />
                      {author.platformId && (
                        <CopyField label="@" value={author.platformId} />
                      )}
                    </div>
                    <div className="grid grid-cols-4 gap-2">
                      <StatCell
                        label="粉丝"
                        value={fmtCount(author.followerCount)}
                      />
                      <StatCell
                        label="关注"
                        value={fmtCount(author.followingCount)}
                      />
                      <StatCell
                        label="获赞"
                        value={fmtCount(author.totalFavorited)}
                      />
                      <StatCell label="属地" value={author.location ?? "—"} />
                      <StatCell
                        label="已采视频"
                        value={author.videoCount}
                        accent="blue"
                      />
                      <StatCell
                        label="已采评论"
                        value={author.commentCount}
                        accent="blue"
                      />
                      <StatCell
                        label="作品总点赞"
                        value={fmtCount(content.likeCount)}
                        accent="red"
                      />
                      <StatCell
                        label="监控状态"
                        value={
                          <span className="flex items-center gap-2">
                            <span>
                              {author.isMonitored ? "监控中" : "未监控"}
                            </span>
                            <Switch
                              checked={author.isMonitored}
                              disabled={monitoring}
                              onCheckedChange={toggleMonitor}
                            />
                          </span>
                        }
                      />
                    </div>
                    {author.signature && (
                      <div className="rounded-md border bg-muted/30 px-3 py-2 text-sm">
                        <span className="mr-2 text-xs text-muted-foreground">
                          简介
                        </span>
                        {author.signature}
                      </div>
                    )}
                    <div className="grid grid-cols-3 gap-2 text-xs">
                      <div>
                        <span className="text-muted-foreground">首次采集</span>
                        <div>{fmtDate(author.firstCollectedAt)}</div>
                      </div>
                      <div>
                        <span className="text-muted-foreground">最近发布</span>
                        <div>{fmtDate(author.lastPublishedAt)}</div>
                      </div>
                      <div>
                        <span className="text-muted-foreground">最近采集</span>
                        <div>{fmtDate(author.lastCollectedAt)}</div>
                      </div>
                    </div>
                  </section>

                  <Separator />

                  {/* 内容区 */}
                  <section className="space-y-3">
                    {/* 关键词单独成行,不挤占标题宽度 */}
                    {content.keyword && (
                      <span className="inline-block rounded bg-rose-500 px-1.5 py-0.5 text-[11px] font-medium text-white">
                        {content.keyword}
                      </span>
                    )}
                    {/* 标题:点击即跳转原文(无需单独「去原文」按钮) */}
                    {originUrl ? (
                      <SimpleTooltip content="点击打开原文">
                        <h3
                          onClick={() =>
                            openUrl(originUrl).catch((e) =>
                              toast.error(`打开失败: ${e}`),
                            )
                          }
                          className="cursor-pointer font-semibold leading-snug transition-colors hover:text-primary hover:underline"
                        >
                          {content.title || content.desc || "(无标题)"}
                        </h3>
                      </SimpleTooltip>
                    ) : (
                      <h3 className="font-semibold leading-snug">
                        {content.title || content.desc || "(无标题)"}
                      </h3>
                    )}
                    {content.desc && content.title && (
                      <p className="whitespace-pre-wrap break-words text-sm text-muted-foreground">
                        {content.desc}
                      </p>
                    )}
                    {content.topics.length > 0 && (
                      <div className="flex flex-wrap gap-1">
                        {content.topics.map((t) => (
                          <span
                            key={t}
                            className="rounded bg-violet-100 px-1.5 py-0.5 text-[11px] text-violet-700 dark:bg-violet-950/60 dark:text-violet-300"
                          >
                            {t}
                          </span>
                        ))}
                      </div>
                    )}
                    <div className="flex flex-wrap gap-x-6 gap-y-1 text-sm">
                      <span>
                        <span className="text-muted-foreground">点赞 </span>
                        <span className="font-semibold text-rose-600 dark:text-rose-400">
                          {fmtCount(content.likeCount)}
                        </span>
                      </span>
                      <span>
                        <span className="text-muted-foreground">收藏 </span>
                        <span className="font-semibold text-amber-600 dark:text-amber-400">
                          {fmtCount(content.collectCount)}
                        </span>
                      </span>
                      <span>
                        <span className="text-muted-foreground">评论 </span>
                        <span className="font-semibold text-sky-600 dark:text-sky-400">
                          {fmtCount(content.commentCount)}
                        </span>
                      </span>
                      <span>
                        <span className="text-muted-foreground">分享 </span>
                        <span className="font-semibold text-emerald-600 dark:text-emerald-400">
                          {fmtCount(content.shareCount)}
                        </span>
                      </span>
                      {content.kind === "video" && (
                        <span>
                          <span className="text-muted-foreground">时长 </span>
                          <span className="font-semibold">
                            {fmtDuration(content.duration)}
                          </span>
                        </span>
                      )}
                    </div>
                    <div className="flex flex-wrap gap-x-4 gap-y-1 text-xs text-muted-foreground">
                      <span>
                        平台:
                        <span
                          className={`ml-1 rounded px-1.5 py-0.5 ${platformClass(content.platform)}`}
                        >
                          {content.platform}
                        </span>
                      </span>
                      <span>
                        内容形式:{KIND_LABEL[content.kind] ?? content.kind}
                      </span>
                      {content.industry && <span>所属行业:{content.industry}</span>}
                      <span>发布时间:{fmtDate(content.publishedAt)}</span>
                      <span>采集时间:{fmtDate(content.collectedAt)}</span>
                    </div>
                    {/* 素材处理状态:视频(视频↓/音频/文案)、图文(图片 done/total)+ 评论/意向 */}
                    <div className="flex flex-wrap items-center gap-1">
                      {content.kind === "video" ? (
                        <>
                          <StateBadge
                            label="视频"
                            state={content.videoDownloaded}
                            errorTip={content.mediaError}
                          />
                          <StateBadge
                            label="音频"
                            state={content.audioExtracted}
                            errorTip={content.mediaError}
                          />
                          <StateBadge
                            label={
                              content.transcript === "" ? "空文案" : "文案"
                            }
                            state={
                              content.transcript != null
                                ? true
                                : content.transcriptError
                                  ? false
                                  : null
                            }
                            errorTip={content.transcriptError}
                          />
                        </>
                      ) : (
                        <StateBadge
                          label={
                            content.imageTotal != null && content.imageTotal > 0
                              ? `图片 ${content.imageDone ?? 0}/${content.imageTotal}`
                              : "素材"
                          }
                          state={
                            content.imageTotal == null ||
                            content.imageTotal === 0
                              ? content.mediaStatus === "success"
                                ? true
                                : content.mediaStatus === "failed"
                                  ? false
                                  : null
                              : content.imageDone === content.imageTotal
                                ? true
                                : (content.imageDone ?? 0) > 0
                                  ? null
                                  : false
                          }
                          errorTip={content.mediaError}
                        />
                      )}
                      {content.commentCollected === true && (
                        <StateBadge label="评论" state={true} />
                      )}
                      {content.intentAnalyzed === true && (
                        <StateBadge label="意向" state={true} />
                      )}
                    </div>
                  </section>
                </div>
              ) : (
                <div className="py-16 text-center text-sm text-muted-foreground">
                  未找到内容详情
                </div>
              )}
            </div>
          </div>
          </div>
        </DialogContent>
      </Dialog>

      {/* 大图浏览层:独立嵌套 Dialog(顶层 modal),由 Radix 接管点击/Esc/焦点,
          避免覆盖层点击穿透到下层详情内容。点黑背景关闭,箭头/方向键翻页可跨内容续看。 */}
      <Dialog
        open={!!lightbox}
        onOpenChange={(o) => {
          if (!o) setLightbox(null);
        }}
      >
        <DialogContent
          showCloseButton={false}
          onClick={() => setLightbox(null)}
          className="left-0 top-0 flex h-screen w-screen max-w-none translate-x-0 translate-y-0 items-center justify-center gap-0 rounded-none border-0 bg-black/90 p-0 ring-0 sm:max-w-none"
        >
          <DialogTitle className="sr-only">图片预览</DialogTitle>
          {lightbox && lbEntry && (
            <>
              <FallbackImage
                src={lbEntry.src}
                fallback={lbEntry.fallback}
                draggable={false}
                onClick={(e) => e.stopPropagation()}
                className="max-h-[92vh] max-w-[92vw] select-none object-contain"
              />

              {/* 关闭 */}
              <button
                type="button"
                onClick={(e) => {
                  e.stopPropagation();
                  setLightbox(null);
                }}
                className="absolute right-4 top-4 inline-flex size-10 cursor-pointer items-center justify-center rounded-full bg-white/10 text-white transition-colors hover:bg-white/20"
              >
                <X className="size-5" />
              </button>

              {/* 左右翻页 */}
              <button
                type="button"
                onClick={(e) => {
                  e.stopPropagation();
                  stepLightbox(-1);
                }}
                className="absolute left-4 top-1/2 inline-flex size-12 -translate-y-1/2 cursor-pointer items-center justify-center rounded-full bg-white/10 text-white transition-colors hover:bg-white/20"
              >
                <ChevronLeft className="size-7" />
              </button>
              <button
                type="button"
                onClick={(e) => {
                  e.stopPropagation();
                  stepLightbox(1);
                }}
                className="absolute right-4 top-1/2 inline-flex size-12 -translate-y-1/2 cursor-pointer items-center justify-center rounded-full bg-white/10 text-white transition-colors hover:bg-white/20"
              >
                <ChevronRight className="size-7" />
              </button>

              {/* 顶部计数 + 作者(跨内容续看时可见当前来源) */}
              <div className="absolute top-4 left-1/2 -translate-x-1/2 rounded-full bg-black/55 px-4 py-1.5 text-sm text-white backdrop-blur-sm">
                {lbEntries.length > 1 && (
                  <span className="font-medium">
                    {lightbox.imgIndex + 1} / {lbEntries.length}
                  </span>
                )}
                {lbItem?.authorNickname && (
                  <span className="ml-2 text-white/70">
                    {lbItem.authorNickname}
                  </span>
                )}
              </div>

              {/* 底部缩略图条:多图时点选直达某张,免逐张翻 */}
              {lbEntries.length > 1 && (
                <div
                  onClick={(e) => e.stopPropagation()}
                  className="veltrix-thin-scrollbar absolute bottom-4 left-1/2 flex max-w-[90vw] -translate-x-1/2 gap-1.5 overflow-x-auto rounded-lg bg-black/55 p-1.5 backdrop-blur-sm"
                >
                  {lbEntries.map((en, i) => (
                    <FallbackImage
                      key={i}
                      src={en.src}
                      fallback={en.fallback}
                      draggable={false}
                      onClick={() =>
                        setLightbox({
                          itemIndex: lightbox.itemIndex,
                          imgIndex: i,
                        })
                      }
                      className={`h-14 w-10 shrink-0 cursor-pointer rounded object-cover transition ${
                        i === lightbox.imgIndex
                          ? "ring-2 ring-white"
                          : "opacity-55 hover:opacity-100"
                      }`}
                    />
                  ))}
                </div>
              )}
            </>
          )}
        </DialogContent>
      </Dialog>
    </>
  );
}
