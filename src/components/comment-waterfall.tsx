// 评论库瀑布流视图:数据由后端按来源分组返回(list_comment_sources_page,
// 每组含评论总数 + 点赞倒序前 N 条预览),前端只渲染不再分组/截断。
// 加载方式与图片库瀑布流一致:@tanstack/react-virtual 分 lane 虚拟化(只挂载可视区
// 附近卡片)+ IntersectionObserver 哨兵滚动 append;卡片 = 来源封面/标题/作者 + 预览评论,
// 「查看全部 N 条」开右侧抽屉(游标分页,可加载更多)。
import { useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";
import { useVirtualizer } from "@tanstack/react-virtual";
import { Heart, Loader2, MessageCircle } from "lucide-react";
import { toast } from "sonner";

import { api, type CommentSourceGroup, type CommentView } from "@/lib/api";
import { useMediaFileUrl, mediaThumbPath } from "@/lib/media-file-url";
import { formatTimestamp } from "@/lib/utils";
import { Button } from "@/components/ui/button";
import { EmptyState } from "@/components/EmptyState";
import {
  Sheet,
  SheetContent,
  SheetHeader,
  SheetTitle,
} from "@/components/ui/sheet";

// 抽屉每页拉取条数
const DRAWER_PAGE = 50;

// 卡片底色轮换(淡色底 + 同色系描边,相邻卡片好区分)
const CARD_TINTS = [
  "bg-blue-500/5 border-blue-500/25",
  "bg-emerald-500/5 border-emerald-500/25",
  "bg-amber-500/5 border-amber-500/25",
  "bg-violet-500/5 border-violet-500/25",
  "bg-rose-500/5 border-rose-500/25",
  "bg-cyan-500/5 border-cyan-500/25",
];

// 来源展示元信息:从组内首条评论的关联字段提取(后端 fill_comment_views 已关联 contents)
interface SourceMeta {
  title: string;
  kind: string;
  coverUrl: string;
  coverPath: string | null;
  authorNickname: string;
  authorAvatar: string | null;
}

function metaOf(g: CommentSourceGroup): SourceMeta {
  const c = g.comments[0];
  return {
    title: c?.contentTitle || "(无标题)",
    kind: c?.contentKind ?? "",
    coverUrl: c?.contentCoverUrl || "",
    coverPath: c?.contentCoverPath ?? null,
    authorNickname: c?.contentAuthorNickname || "",
    authorAvatar: c?.contentAuthorAvatar ?? null,
  };
}

// 单条评论行(卡片与抽屉共用)
function CommentRow({ c }: { c: CommentView }) {
  return (
    <div className="flex items-start gap-2">
      {c.authorAvatar ? (
        <img
          src={c.authorAvatar}
          referrerPolicy="no-referrer"
          onError={(e) => {
            e.currentTarget.style.display = "none";
          }}
          className="size-6 shrink-0 rounded-full object-cover"
          alt=""
        />
      ) : (
        <div className="flex size-6 shrink-0 items-center justify-center rounded-full bg-muted text-[10px] text-muted-foreground">
          {(c.authorNickname || "?").slice(0, 1)}
        </div>
      )}
      <div className="min-w-0 flex-1">
        <div className="flex items-baseline gap-2">
          <span className="truncate text-xs font-medium text-foreground">
            {c.authorNickname || "—"}
          </span>
          {c.createdAt != null && (
            <span className="shrink-0 text-[10px] text-muted-foreground">
              {formatTimestamp(c.createdAt)}
            </span>
          )}
        </div>
        <p className="mt-0.5 whitespace-pre-wrap break-words text-xs text-foreground/90">
          {c.text}
        </p>
        <div className="mt-0.5 flex items-center gap-3 text-[10px] tabular-nums text-muted-foreground">
          <span className="inline-flex items-center gap-0.5">
            <Heart className="size-2.5" />
            {c.likeCount ?? 0}
          </span>
          <span className="inline-flex items-center gap-0.5">
            <MessageCircle className="size-2.5" />
            {c.replyCount ?? 0}
          </span>
        </div>
      </div>
    </div>
  );
}

export function CommentWaterfall({
  groups,
  total,
  loading,
  platformName,
  onLoadMore,
}: {
  // 后端分组结果(已 append 的全部批次)
  groups: CommentSourceGroup[];
  // 来源总数(同筛选口径);hasMore = groups.length < total
  total: number;
  loading: boolean;
  platformName: (id: string) => string;
  onLoadMore: () => void;
}) {
  const mediaFileUrl = useMediaFileUrl();
  // 抽屉:当前查看的来源(null = 关闭)
  const [drawerSource, setDrawerSource] = useState<CommentSourceGroup | null>(null);
  const scrollRef = useRef<HTMLDivElement>(null);
  const sentinelRef = useRef<HTMLDivElement>(null);
  // 列数交给 JS(断点与旧 CSS columns 一致:sm=2 / xl=3 / 2xl=4),虚拟器才能分配 lane。
  // 首次数据未返回时容器尚未挂载,默认值本身必须是可用的桌面布局。
  const [layout, setLayout] = useState({ columns: 3, width: 0 });
  const hasMore = groups.length < total;
  const hasItems = groups.length > 0;

  useLayoutEffect(() => {
    const element = scrollRef.current;
    if (!element) return;
    const update = (width: number) => {
      if (width <= 0) return;
      const columns =
        width >= 1536 ? 4 : width >= 1280 ? 3 : width >= 640 ? 2 : 1;
      setLayout((prev) =>
        prev.columns === columns && prev.width === width
          ? prev
          : { columns, width },
      );
    };
    update(element.clientWidth);
    const observer = new ResizeObserver((entries) => {
      const width = Math.round(entries[0]?.contentRect.width ?? element.clientWidth);
      update(width);
    });
    observer.observe(element);
    return () => observer.disconnect();
    // 首次请求期间 scrollRef 尚不存在;数据回来、容器真正挂载后必须重新绑定。
  }, [hasItems]);

  const gap = 12;
  const virtualizer = useVirtualizer({
    count: groups.length,
    getScrollElement: () => scrollRef.current,
    getItemKey: (index) => {
      const g = groups[index];
      return g ? `${g.platform}-${g.contentId}` : index;
    },
    // 卡片 = 来源头 + 预览评论(评论文本行数不定)+ 查看全部钮;
    // 首次估算后由 measureElement 实测校正。
    estimateSize: () => 430,
    lanes: layout.columns,
    gap,
    overscan: layout.columns * 2,
    useAnimationFrameWithResizeObserver: true,
  });
  const virtualItems = virtualizer.getVirtualItems();

  // 列数或列宽变化后清掉旧测量值,否则窗口拉伸后沿用旧卡片高度会重叠/留白
  useEffect(() => {
    if (layout.width > 0) virtualizer.measure();
  }, [layout.columns, layout.width, virtualizer]);

  useEffect(() => {
    const sentinel = sentinelRef.current;
    if (!sentinel || !hasMore || loading) return;
    const observer = new IntersectionObserver(
      (entries) => {
        if (entries.some((e) => e.isIntersecting)) onLoadMore();
      },
      { root: scrollRef.current, rootMargin: "300px" },
    );
    observer.observe(sentinel);
    return () => observer.disconnect();
  }, [hasMore, groups.length, loading, onLoadMore]);

  if (groups.length === 0) {
    if (loading) {
      return (
        <div className="flex min-h-0 flex-1 items-center justify-center gap-2 text-sm text-muted-foreground">
          <Loader2 className="size-4 animate-spin" />
          加载中…
        </div>
      );
    }
    return (
      <div className="flex min-h-0 flex-1 items-center justify-center">
        <EmptyState
          title="暂无评论"
          description="开启任务的「评论采集」后,这里会按来源聚合展示评论"
        />
      </div>
    );
  }
  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <div
        ref={scrollRef}
        className="veltrix-thin-scrollbar min-h-0 flex-1 overflow-y-auto pr-1"
      >
        {/* 只挂载可视区附近的卡片;已加载批次再多,DOM/头像解码量保持稳定 */}
        <div
          className="relative w-full"
          style={{ height: virtualizer.getTotalSize() }}
        >
          {virtualItems.map((virtualItem) => {
            const g = groups[virtualItem.index];
            if (!g) return null;
            const meta = metaOf(g);
            // 列表小图用缩略图(缺失时文件服务惰性生成),原图留给详情
            const cover = meta.coverPath
              ? mediaFileUrl(mediaThumbPath(meta.coverPath))
              : meta.coverUrl;
            const kindLabel =
              meta.kind === "video"
                ? "视频"
                : meta.kind === "image"
                  ? "图文"
                  : meta.kind === "article"
                    ? "文章"
                    : "";
            const lane = virtualItem.lane;
            const widthPercent = 100 / layout.columns;
            const widthGap = (gap * (layout.columns - 1)) / layout.columns;
            return (
              <div
                key={virtualItem.key}
                ref={virtualizer.measureElement}
                data-index={virtualItem.index}
                className="absolute left-0 top-0 min-w-0"
                style={{
                  // 宽度由父容器百分比决定,首次测量为 0 时也能正常铺开。
                  width: `calc(${widthPercent}% - ${widthGap}px)`,
                  // translate 百分比基于卡片自身:每跨一列移动 100% 自身宽度 + 一个 gap
                  transform: `translate3d(calc(${lane * 100}% + ${lane * gap}px), ${virtualItem.start}px, 0)`,
                }}
              >
                <div
                  className={`rounded-lg border p-3 ${CARD_TINTS[virtualItem.index % CARD_TINTS.length]}`}
                >
                  {/* 来源:封面 + 标题 + 作者 + 平台 */}
                  <div className="flex items-center gap-2.5">
                    {cover ? (
                      <img
                        src={cover}
                        referrerPolicy="no-referrer"
                        onError={(e) => {
                          e.currentTarget.style.display = "none";
                        }}
                        className="h-14 w-11 shrink-0 rounded object-cover"
                        alt=""
                      />
                    ) : (
                      <div className="flex h-14 w-11 shrink-0 items-center justify-center rounded bg-muted text-[10px] text-muted-foreground">
                        无图
                      </div>
                    )}
                    <div className="min-w-0 flex-1">
                      <div className="flex items-center gap-1">
                        {kindLabel && (
                          <span className="shrink-0 rounded bg-muted px-1 text-[10px] text-muted-foreground">
                            {kindLabel}
                          </span>
                        )}
                        <span className="shrink-0 rounded bg-muted px-1 text-[10px] text-muted-foreground">
                          {platformName(g.platform)}
                        </span>
                      </div>
                      <div className="mt-0.5 line-clamp-2 text-xs font-medium text-foreground">
                        {meta.title}
                      </div>
                      {meta.authorNickname && (
                        <div className="mt-0.5 flex items-center gap-1 text-[11px] text-muted-foreground">
                          {meta.authorAvatar ? (
                            <img
                              src={meta.authorAvatar}
                              referrerPolicy="no-referrer"
                              onError={(e) => {
                                e.currentTarget.style.display = "none";
                              }}
                              className="size-4 shrink-0 rounded-full object-cover"
                              alt=""
                            />
                          ) : (
                            <span className="flex size-4 shrink-0 items-center justify-center rounded-full bg-muted text-[9px]">
                              {meta.authorNickname.slice(0, 1)}
                            </span>
                          )}
                          <span className="truncate">作者:{meta.authorNickname}</span>
                        </div>
                      )}
                    </div>
                  </div>
                  {/* 该来源的预览评论(后端截好,点赞倒序) */}
                  <div className="mt-2.5 flex flex-col gap-2.5 border-t border-border/60 pt-2.5">
                    {g.comments.map((c) => (
                      <CommentRow key={c.id} c={c} />
                    ))}
                  </div>
                  <button
                    type="button"
                    onClick={() => setDrawerSource(g)}
                    className="mt-2.5 w-full rounded-md py-1 text-center text-xs text-primary transition-colors hover:bg-primary/10"
                  >
                    查看全部 {g.commentCount} 条评论 ›
                  </button>
                </div>
              </div>
            );
          })}
        </div>
        {hasMore ? (
          <div
            ref={sentinelRef}
            className="flex items-center justify-center gap-1.5 py-3 text-xs text-muted-foreground"
          >
            {loading && <Loader2 className="size-3.5 animate-spin" />}
            {loading ? "正在加载" : "继续滚动加载"} · 已显示 {groups.length}/{total}
          </div>
        ) : (
          groups.length > 1 && (
            <div className="py-3 text-center text-xs text-muted-foreground">
              已全部加载 · 共 {groups.length} 个来源
            </div>
          )
        )}
      </div>

      {/* 右侧抽屉:该来源的全部评论(游标分页) */}
      <SourceCommentsDrawer
        source={drawerSource}
        onClose={() => setDrawerSource(null)}
      />
    </div>
  );
}

// 右侧抽屉:某来源下的全部评论(list_content_comments 按点赞倒序游标分页)
function SourceCommentsDrawer({
  source,
  onClose,
}: {
  source: CommentSourceGroup | null;
  onClose: () => void;
}) {
  const [items, setItems] = useState<CommentView[]>([]);
  const [total, setTotal] = useState(0);
  const [cursor, setCursor] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);

  const loadMore = useCallback(
    async (group: CommentSourceGroup, cur: string | null, append: boolean) => {
      setLoading(true);
      try {
        // 传平台原生 contentId + platform:后端先按行主键找,未命中按原生 id 兜底
        const res = await api.listContentComments(group.contentId, cur ?? undefined, DRAWER_PAGE, group.platform);
        setItems((prev) => (append ? [...prev, ...res.items] : res.items));
        setTotal(res.total);
        setCursor(res.nextCursor);
      } catch (e) {
        toast.error(`加载评论失败: ${e}`);
      } finally {
        setLoading(false);
      }
    },
    [],
  );

  // 打开抽屉 / 切换来源时重置并拉第一页
  useEffect(() => {
    if (!source) return;
    setItems([]);
    setTotal(0);
    setCursor(null);
    void loadMore(source, null, false);
  }, [source, loadMore]);

  const meta = source ? metaOf(source) : null;

  return (
    <Sheet open={source != null} onOpenChange={(o) => !o && onClose()}>
      <SheetContent side="right" className="flex w-full flex-col sm:max-w-xl">
        <SheetHeader>
          <SheetTitle className="line-clamp-1 pr-6 text-sm">
            {meta?.title ?? ""}
          </SheetTitle>
          <div className="flex items-center gap-1.5 text-xs text-muted-foreground">
            {meta?.authorAvatar && (
              <img
                src={meta.authorAvatar}
                referrerPolicy="no-referrer"
                onError={(e) => {
                  e.currentTarget.style.display = "none";
                }}
                className="size-4 rounded-full object-cover"
                alt=""
              />
            )}
            {meta?.authorNickname ? `作者:${meta.authorNickname} · ` : ""}
            共 {total} 条评论
          </div>
        </SheetHeader>
        <div className="flex min-h-0 flex-1 flex-col gap-3 overflow-y-auto px-4 pb-4">
          {items.map((c) => (
            <CommentRow key={c.id} c={c} />
          ))}
          {loading && (
            <div className="flex justify-center py-2 text-muted-foreground">
              <Loader2 className="size-4 animate-spin" />
            </div>
          )}
          {!loading && items.length === 0 && (
            <div className="py-8 text-center text-xs text-muted-foreground">
              该来源暂无评论
            </div>
          )}
          {cursor && !loading && (
            <Button
              variant="outline"
              size="sm"
              className="mx-auto"
              onClick={() => source && void loadMore(source, cursor, true)}
            >
              加载更多({items.length}/{total})
            </Button>
          )}
        </div>
      </SheetContent>
    </Sheet>
  );
}
