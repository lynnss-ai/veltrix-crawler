import { memo } from "react";
import {
  Clock,
  Eye,
  Heart,
  Image as ImageIcon,
  Loader2,
  MoreHorizontal,
  RefreshCw,
  Search,
  Trash2,
} from "lucide-react";
import type { ContentView } from "@/lib/api";
import { LocalFirstImage } from "@/components/LocalFirstImage";
import { Button } from "@/components/ui/button";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { platformClass, platformSolidClass } from "@/lib/platforms";

const MOCK_TRANSCRIPTS = [
  "今天给大家分享一个超实用的小技巧,学会之后能省下不少时间。整个过程只需要三步,先准备好材料,然后按照视频里的顺序操作,最后检查一遍就完成了。很多朋友反馈说效果特别好,记得点赞收藏,下次找不到就可惜了。",
  "很多人都在问这个问题,今天一次性讲清楚。其实关键就在于细节的把握,大部分人失败都是因为忽略了第二步。我把完整的流程整理出来了,跟着做基本不会出错。有问题可以在评论区留言,看到都会回复。",
  "这期内容准备了很久,把我这几年踩过的坑都总结进来了。如果你也遇到过类似的情况,一定要看到最后。前半部分讲原理,后半部分是实操演示,建议先收藏再慢慢看。",
  "开头先说结论:这个方法是目前亲测最有效的。视频里我会从零开始演示一遍,每个步骤都有讲解,新手也能跟得上。觉得有用的话帮忙点个关注,后续还会持续更新这个系列。",
  "最近后台收到特别多私信问这件事,干脆拍一期详细的。先讲大家最关心的三个误区,再给出我的建议。每个人情况不一样,评论区聊聊你的看法,说不定下期就翻牌你的问题。",
];
function mockTranscript(contentId: string): string {
  let h = 0;
  for (let i = 0; i < contentId.length; i += 1) {
    h = (h * 31 + contentId.charCodeAt(i)) >>> 0;
  }
  return MOCK_TRANSCRIPTS[h % MOCK_TRANSCRIPTS.length];
}

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

export const WaterfallCard = memo(function WaterfallCard({
  c,
  platformName,
  retrying,
  onOpenDetail,
  onRetry,
  onDelete,
}: {
  c: ContentView;
  platformName: (id: string) => string;
  retrying: boolean;
  onOpenDetail: (id: string) => void;
  onRetry: (c: ContentView) => void;
  onDelete: (id: string) => void;
}) {
  const coverExternal = c.coverUrl || c.imageUrls[0] || "";
  const hasCover = Boolean(c.coverPath || coverExternal);
  const hasAvatar = Boolean(c.avatarPath || c.authorAvatar);
  const titleText = c.title || c.desc || "(无文案)";
  const imageCount = c.imageUrls.length;
  const isVideo = c.kind === "video";
  return (
    <div className="group relative break-inside-avoid overflow-hidden rounded-xl border border-border bg-card transition duration-200 hover:-translate-y-0.5 hover:shadow-md">
      {isVideo ? (
        <div className="flex flex-wrap items-center gap-1.5 px-2.5 pt-2.5">
          <span
            className={`rounded px-2 py-0.5 text-[11px] font-medium ${platformClass(c.platform)}`}
          >
            {platformName(c.platform)}
          </span>
          {c.keyword && (
            <span className="inline-flex min-w-0 items-center gap-0.5 rounded bg-red-500 px-1.5 py-0.5 text-[10px] font-medium text-white">
              <Search className="size-2.5 shrink-0" />
              <span className="truncate">{c.keyword}</span>
            </span>
          )}
          {c.duration != null && c.duration > 0 && (
            <span className="inline-flex items-center gap-0.5 rounded bg-secondary px-1.5 py-0.5 text-[10px] font-medium text-secondary-foreground">
              <Clock className="size-2.5" />
              {formatDuration(c.duration)}
            </span>
          )}
        </div>
      ) : (
        <div
          className="relative aspect-[3/4] cursor-pointer overflow-hidden bg-muted"
          onClick={() => onOpenDetail(c.id)}
        >
          {hasCover ? (
            <LocalFirstImage
              localPath={c.coverPath}
              externalUrl={coverExternal}
              className="size-full object-cover transition duration-300 group-hover:scale-[1.03]"
            />
          ) : (
            <div className="flex size-full items-center justify-center">
              <ImageIcon className="size-8 text-muted-foreground" />
            </div>
          )}
          <div className="pointer-events-none absolute inset-x-0 top-0 h-14 bg-gradient-to-b from-black/45 to-transparent" />
          <span
            className={`absolute left-2 top-2 rounded-md px-2 py-0.5 text-xs font-bold shadow-lg ring-1 ring-white/40 ${platformSolidClass(c.platform)}`}
          >
            {platformName(c.platform)}
          </span>
          {(c.keyword || imageCount > 1) && (
            <>
              <div className="pointer-events-none absolute inset-x-0 bottom-0 h-14 bg-gradient-to-t from-black/45 to-transparent" />
              <div className="pointer-events-none absolute inset-x-0 bottom-0 flex items-end justify-between gap-1.5 p-2">
                {c.keyword ? (
                  <span className="inline-flex min-w-0 items-center gap-0.5 rounded-md bg-red-500 px-1.5 py-0.5 text-[11px] font-semibold text-white shadow-lg ring-1 ring-white/40">
                    <Search className="size-3 shrink-0" />
                    <span className="truncate">{c.keyword}</span>
                  </span>
                ) : (
                  <span />
                )}
                {imageCount > 1 && (
                  <span className="inline-flex shrink-0 items-center gap-1 rounded-md bg-black/70 px-2 py-0.5 text-xs font-bold text-white shadow-lg ring-1 ring-white/40 backdrop-blur-sm">
                    <ImageIcon className="size-3.5" />
                    {imageCount} 图
                  </span>
                )}
              </div>
            </>
          )}
        </div>
      )}

      <div className="absolute right-1.5 top-1.5 opacity-0 transition-opacity group-hover:opacity-100">
        <DropdownMenu>
          <DropdownMenuTrigger asChild>
            <Button
              variant="secondary"
              size="icon"
              className="size-7 cursor-pointer shadow-sm"
              onClick={(e) => e.stopPropagation()}
            >
              {retrying ? (
                <Loader2 className="size-4 animate-spin" />
              ) : (
                <MoreHorizontal className="size-4" />
              )}
            </Button>
          </DropdownMenuTrigger>
          <DropdownMenuContent align="end">
            <DropdownMenuItem onClick={() => onOpenDetail(c.id)}>
              <Eye className="size-4" />
              详情
            </DropdownMenuItem>
            {c.mediaStatus === "failed" && (
              <DropdownMenuItem disabled={retrying} onClick={() => onRetry(c)}>
                <RefreshCw className="size-4" />
                重新拉取素材
              </DropdownMenuItem>
            )}
            <DropdownMenuItem
              variant="destructive"
              onClick={() => onDelete(c.id)}
            >
              <Trash2 className="size-4" />
              删除
            </DropdownMenuItem>
          </DropdownMenuContent>
        </DropdownMenu>
      </div>

      <div className="space-y-2 p-2.5">
        <p
          onClick={() => onOpenDetail(c.id)}
          className="line-clamp-2 cursor-pointer text-sm font-medium leading-snug text-foreground transition-colors hover:text-primary"
        >
          {titleText}
        </p>
        {c.kind === "video" && (
          <div className="space-y-1">
            <p className="line-clamp-5 text-xs leading-relaxed text-muted-foreground">
              {c.transcript || mockTranscript(c.contentId)}
            </p>
            {!c.transcript && (
              <span className="inline-block rounded bg-amber-100 px-1.5 py-0.5 text-[10px] font-medium text-amber-700 dark:bg-amber-950/60 dark:text-amber-300">
                示例文案 · 转写完成后自动替换
              </span>
            )}
          </div>
        )}
        <div className="flex items-center justify-between gap-2">
          <div className="flex min-w-0 items-center gap-1.5">
            {hasAvatar ? (
              <LocalFirstImage
                localPath={c.avatarPath}
                externalUrl={c.authorAvatar ?? ""}
                className="size-5 shrink-0 rounded-full object-cover"
              />
            ) : (
              <div className="size-5 shrink-0 rounded-full bg-muted" />
            )}
            <span className="truncate text-[11px] text-muted-foreground">
              {c.authorNickname || "—"}
            </span>
          </div>
          <span className="inline-flex shrink-0 items-center gap-0.5 text-[11px] text-muted-foreground">
            <Heart className="size-3" />
            {formatCount(c.likeCount)}
          </span>
        </div>
      </div>
    </div>
  );
});
