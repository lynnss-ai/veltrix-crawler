// 评论库瀑布流视图:数据由后端按来源分组返回(list_comment_sources_page,
// 每组含评论总数 + 点赞倒序前 N 条预览),前端只渲染不再分组/截断。
// 布局用 CSS 多列瀑布流(columns + break-inside-avoid):评论卡片是纯文本、高度方差大,
// 此前用 @tanstack/react-virtual 分 lane 虚拟化,测量高度与真实高度偏差会导致卡片相互
// 重叠;CSS columns 由浏览器自动排高,无测量环节。加载方式不变——IntersectionObserver
// 哨兵滚动 append;卡片 = 来源封面/标题/作者 + 预览评论,「查看全部 N 条」开右侧抽屉。
import { useCallback, useEffect, useRef, useState } from "react";
import { Heart, Loader2, MessageCircle } from "lucide-react";
import { toast } from "sonner";

import { api, type CommentSourceGroup, type CommentView } from "@/lib/api";
import { useMediaFileUrl, mediaThumbPath } from "@/lib/media-file-url";
import { formatTimestamp } from "@/lib/utils";
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
  const hasMore = groups.length < total;

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
        {/* CSS 多列瀑布流:卡片按列自动排高,高度再悬殊也不会重叠;断点 sm=2 / xl=3 / 2xl=4 */}
        <div className="columns-1 gap-3 sm:columns-2 xl:columns-3 2xl:columns-4">
          {groups.map((g, index) => {
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
            return (
              <div
                key={`${g.platform}-${g.contentId}`}
                className="mb-3 break-inside-avoid"
              >
                <div
                  className={`rounded-lg border p-3 ${CARD_TINTS[index % CARD_TINTS.length]}`}
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

  // 自动翻页:滚动到底部哨兵进入视口即续拉下一页(替代手动「加载更多」按钮)
  const sentinelRef = useRef<HTMLDivElement>(null);
  useEffect(() => {
    const el = sentinelRef.current;
    if (!el || !source || !cursor || loading) return;
    const observer = new IntersectionObserver(
      (entries) => {
        if (entries.some((e) => e.isIntersecting)) {
          void loadMore(source, cursor, true);
        }
      },
      // 提前 200px 触发,滚动到底前就开始拉,体感无断档
      { rootMargin: "0px 0px 200px 0px" },
    );
    observer.observe(el);
    return () => observer.disconnect();
  }, [source, cursor, loading, loadMore]);

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
          {/* 自动翻页哨兵:进入视口即续拉;到底后提示已全部加载 */}
          {cursor && <div ref={sentinelRef} className="h-px" />}
          {!cursor && items.length > 0 && (
            <div className="py-2 text-center text-xs text-muted-foreground">
              已全部加载 · 共 {items.length} 条
            </div>
          )}
        </div>
      </SheetContent>
    </Sheet>
  );
}
