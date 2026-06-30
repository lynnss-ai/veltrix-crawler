import { memo } from "react";
import {
  AudioLines,
  Bookmark,
  CalendarDays,
  Clock,
  FileQuestion,
  FileText,
  Heart,
  Image as ImageIcon,
  MessageCircle,
  Search,
  Share2,
  Video,
} from "lucide-react";
import { openUrl } from "@tauri-apps/plugin-opener";
import { toast } from "sonner";
import type { ContentView } from "@/lib/api";
import {
  authorProfileUrl,
  contentDetailUrl,
  labelBadgeClass,
  platformClass,
  platformUidLabel,
} from "@/lib/platforms";
import { LocalFirstImage } from "@/components/LocalFirstImage";
import { SimpleTooltip } from "@/components/SimpleTooltip";
import { formatTimestamp } from "@/lib/utils";

const KIND_META: Record<
  ContentView["kind"],
  { label: string; icon: typeof Video; cls: string }
> = {
  video: {
    label: "视频",
    icon: Video,
    cls: "bg-sky-500/10 text-sky-600 dark:text-sky-400",
  },
  image: {
    label: "图文",
    icon: ImageIcon,
    cls: "bg-violet-500/10 text-violet-600 dark:text-violet-400",
  },
  article: {
    label: "文章",
    icon: FileText,
    cls: "bg-amber-500/10 text-amber-600 dark:text-amber-400",
  },
  unknown: {
    label: "未知",
    icon: FileQuestion,
    cls: "bg-slate-500/10 text-slate-700 dark:text-slate-300",
  },
};

function formatCount(n?: number | null): string {
  if (n == null) return "—";
  if (n >= 10000) return `${(n / 10000).toFixed(1)}万`;
  return String(n);
}

function formatDuration(sec?: number | null): string {
  if (sec == null || sec <= 0) return "—";
  const m = Math.floor(sec / 60);
  const s = sec % 60;
  return `${m}:${String(s).padStart(2, "0")}`;
}

function copyText(text: string, label: string) {
  if (!text) return;
  if (!navigator.clipboard) {
    toast.error("当前环境不支持复制");
    return;
  }
  navigator.clipboard
    .writeText(text)
    .then(() => toast.success(`已复制${label}`))
    .catch(() => toast.error("复制失败"));
}

export const ContentCard = memo(function ContentCard({
  c,
  platformName,
  showKind,
  onOpenDetail,
}: {
  c: ContentView;
  // 平台 id → 名称(展示用),由调用方注入平台配置映射
  platformName: (id: string) => string;
  // 是否展示「形式」徽标:全量库展示;内容库/图片库已按形态限定,徽标冗余故隐藏
  showKind: boolean;
  onOpenDetail: () => void;
}) {
  const meta = KIND_META[c.kind] ?? KIND_META.unknown;
  const Icon = meta.icon;
  const coverExternal = c.coverUrl || c.imageUrls[0] || "";
  const hasCover = Boolean(c.coverPath || coverExternal);
  const hasAvatar = Boolean(c.avatarPath || c.authorAvatar);
  const titleText = c.title || c.desc || "(无文案)";
  const homeUrl = authorProfileUrl(c.platform, c.authorUid);
  const detailUrl = contentDetailUrl(c.platform, c.contentId) || c.videoUrl;
  return (
    <div className="flex w-[64rem] max-w-full gap-3 py-1">
      {hasCover ? (
        <SimpleTooltip content={detailUrl ? "打开视频详情" : "暂无详情链接"}>
          {/* 固定 3:4 竖版缩略图(不拉伸不变形),顶端对齐;尺寸取 w-40 使高度≈文案截断后的行高 */}
          <LocalFirstImage
            localPath={c.coverPath}
            externalUrl={coverExternal}
            className={`aspect-[3/4] w-36 shrink-0 self-start rounded-md object-cover transition ${
              detailUrl ? "cursor-pointer hover:opacity-80" : ""
            }`}
            onClick={
              detailUrl
                ? () =>
                    openUrl(detailUrl).catch((e) =>
                      toast.error(`打开视频详情失败: ${e}`),
                    )
                : undefined
            }
          />
        </SimpleTooltip>
      ) : (
        <div className="flex aspect-[3/4] w-36 shrink-0 items-center justify-center self-start rounded-md bg-muted">
          <Icon className="size-6 text-muted-foreground" />
        </div>
      )}

      <div className="flex min-w-0 flex-1 flex-col gap-2">
        <div className="flex items-center gap-2">
          {hasAvatar ? (
            <SimpleTooltip content={homeUrl ? "打开作者主页" : "暂无主页链接"}>
              <LocalFirstImage
                localPath={c.avatarPath}
                externalUrl={c.authorAvatar ?? ""}
                className={`size-9 shrink-0 rounded-full object-cover transition ${
                  homeUrl
                    ? "cursor-pointer hover:ring-2 hover:ring-primary hover:ring-offset-1"
                    : ""
                }`}
                onClick={
                  homeUrl
                    ? () =>
                        openUrl(homeUrl).catch((e) =>
                          toast.error(`打开作者主页失败: ${e}`),
                        )
                    : undefined
                }
              />
            </SimpleTooltip>
          ) : (
            <div className="size-9 shrink-0 rounded-full bg-muted" />
          )}
          <div className="flex min-w-0 flex-col items-start">
            <SimpleTooltip content="点击复制昵称">
              <span
                className="max-w-full cursor-pointer truncate text-sm font-medium text-foreground hover:underline"
                onClick={() => copyText(c.authorNickname, "昵称")}
              >
                {c.authorNickname || "—"}
              </span>
            </SimpleTooltip>
            <SimpleTooltip content={`点击复制${platformUidLabel(c.platform)}`}>
              <span
                className="max-w-full cursor-pointer truncate text-xs text-muted-foreground hover:underline"
                onClick={() => copyText(c.authorUid, platformUidLabel(c.platform))}
              >
                {c.authorUid}
              </span>
            </SimpleTooltip>
          </div>
        </div>

        {/* 平台 / 形式 / 行业 徽标:原独立列合并进内容卡片,单独成行置于关键词行上方 */}
        <div className="flex flex-wrap items-center gap-1.5">
          <span
            className={`inline-block rounded px-2 py-0.5 text-[11px] font-medium ${platformClass(c.platform)}`}
          >
            {platformName(c.platform)}
          </span>
          {showKind && (
            <span
              className={`inline-flex items-center gap-1 rounded-md px-2 py-0.5 text-[11px] font-medium ${meta.cls}`}
            >
              <Icon className="size-3" />
              {meta.label}
            </span>
          )}
          {c.industry && (
            <span
              className={`inline-block rounded-md px-2 py-0.5 text-[11px] font-medium ${labelBadgeClass(c.industry)}`}
            >
              {c.industry}
            </span>
          )}
        </div>

        <div className="flex flex-wrap items-center gap-x-3 gap-y-1 text-xs text-muted-foreground">
          {c.keyword && (
            <span className="inline-flex items-center gap-1 rounded bg-red-500 px-1.5 py-0.5 text-[11px] font-medium text-white">
              <Search className="size-3" />
              {c.keyword}
            </span>
          )}
          <span className="inline-flex items-center gap-1 whitespace-nowrap">
            <CalendarDays className="size-3" />
            发布:{formatTimestamp(c.publishedAt)}
          </span>
          <span className="inline-flex items-center gap-1 whitespace-nowrap">
            <Clock className="size-3" />
            创建:{formatTimestamp(c.collectedAt)}
          </span>
        </div>

        <SimpleTooltip content="查看详情">
          {/* 文案最多展示 2 行,超出省略:收敛行高使各行高度趋于统一,全文点开详情查看 */}
          <p
            onClick={onOpenDetail}
            className="line-clamp-2 w-full cursor-pointer whitespace-normal break-words text-xs leading-relaxed text-muted-foreground transition-colors hover:text-primary hover:underline"
          >
            {titleText}
          </p>
        </SimpleTooltip>

        {c.topics.length > 0 && (
          <div className="flex flex-wrap items-center gap-1">
            {c.topics.map((topic, i) => (
              <span
                key={i}
                className="rounded bg-violet-100 px-1.5 py-0.5 text-[11px] text-violet-700 dark:bg-violet-950 dark:text-violet-300"
              >
                {topic}
              </span>
            ))}
          </div>
        )}

        <div className="flex flex-wrap items-center gap-2 text-xs">
          <span className="inline-flex items-center gap-1 rounded-md bg-rose-100 px-2 py-0.5 font-medium text-rose-700 dark:bg-rose-950/60 dark:text-rose-300">
            <Heart className="size-3.5" />
            {formatCount(c.likeCount)}
          </span>
          <span className="inline-flex items-center gap-1 rounded-md bg-amber-100 px-2 py-0.5 font-medium text-amber-700 dark:bg-amber-950/60 dark:text-amber-300">
            <Bookmark className="size-3.5" />
            {formatCount(c.collectCount)}
          </span>
          <span className="inline-flex items-center gap-1 rounded-md bg-sky-100 px-2 py-0.5 font-medium text-sky-700 dark:bg-sky-950/60 dark:text-sky-300">
            <MessageCircle className="size-3.5" />
            {formatCount(c.commentCount)}
          </span>
          <span className="inline-flex items-center gap-1 rounded-md bg-emerald-100 px-2 py-0.5 font-medium text-emerald-700 dark:bg-emerald-950/60 dark:text-emerald-300">
            <Share2 className="size-3.5" />
            {formatCount(c.shareCount)}
          </span>
          {c.duration != null && c.duration > 0 && (
            <span className="inline-flex items-center gap-1 rounded-md bg-secondary px-2 py-0.5 font-medium text-secondary-foreground">
              <Clock className="size-3.5" />
              {formatDuration(c.duration)}
            </span>
          )}
        </div>

        {c.kind === "video" && c.transcript && (
          <details className="rounded-md bg-muted/50 px-2 py-1.5">
            <summary className="flex cursor-pointer items-center gap-1 text-[11px] font-medium text-muted-foreground">
              <AudioLines className="size-3.5" />
              语音文案
            </summary>
            <p className="mt-1 whitespace-pre-wrap break-words text-xs text-foreground">
              {c.transcript}
            </p>
          </details>
        )}
        {c.kind === "video" && !c.transcript && c.transcriptError && (
          <SimpleTooltip content={c.transcriptError}>
            <span className="inline-flex w-fit cursor-help items-center gap-1 rounded bg-rose-100 px-1.5 py-0.5 text-[11px] text-rose-700 dark:bg-rose-950/60 dark:text-rose-300">
              <AudioLines className="size-3" />
              转写失败
            </span>
          </SimpleTooltip>
        )}
      </div>
    </div>
  );
});
