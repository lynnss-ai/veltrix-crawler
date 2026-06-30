import { useEffect, useRef } from "react";
import { Loader2 } from "lucide-react";
import type { ContentView } from "@/lib/api";
import { EmptyState } from "@/components/EmptyState";
import { WaterfallCard } from "@/components/WaterfallCard";

const GRID_PAGE_SIZE = 48;

export function ImageWaterfall({
  items,
  visibleCount,
  onLoadMore,
  platformName,
  retrying,
  onOpenDetail,
  onRetry,
  onDelete,
}: {
  items: ContentView[];
  visibleCount: number;
  onLoadMore: () => void;
  platformName: (id: string) => string;
  retrying: Set<string>;
  onOpenDetail: (id: string) => void;
  onRetry: (c: ContentView) => void;
  onDelete: (id: string) => void;
}) {
  const scrollRef = useRef<HTMLDivElement>(null);
  const sentinelRef = useRef<HTMLDivElement>(null);
  const hasMore = visibleCount < items.length;

  useEffect(() => {
    const sentinel = sentinelRef.current;
    if (!sentinel || !hasMore) return;
    const observer = new IntersectionObserver(
      (entries) => {
        if (entries.some((e) => e.isIntersecting)) onLoadMore();
      },
      { root: scrollRef.current, rootMargin: "300px" },
    );
    observer.observe(sentinel);
    return () => observer.disconnect();
  }, [hasMore, visibleCount, items.length, onLoadMore]);

  if (items.length === 0) {
    return (
      <div className="flex min-h-0 flex-1 items-center justify-center">
        <EmptyState
          title="暂无素材"
          description="采集完成后,内容会以瀑布流展示在这里"
        />
      </div>
    );
  }
  const visible = items.slice(0, visibleCount);
  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <div
        ref={scrollRef}
        className="veltrix-thin-scrollbar min-h-0 flex-1 overflow-y-auto pr-1"
      >
        <div className="columns-2 gap-3 sm:columns-3 lg:columns-4 xl:columns-5 [&>*]:mb-3">
          {visible.map((c) => (
            <WaterfallCard
              key={c.id}
              c={c}
              platformName={platformName}
              retrying={retrying.has(c.id)}
              onOpenDetail={onOpenDetail}
              onRetry={onRetry}
              onDelete={onDelete}
            />
          ))}
        </div>
        {hasMore ? (
          <div
            ref={sentinelRef}
            className="flex items-center justify-center gap-1.5 py-3 text-xs text-muted-foreground"
          >
            <Loader2 className="size-3.5 animate-spin" />
            滚动自动加载 · 已显示 {visible.length}/{items.length}
          </div>
        ) : (
          items.length > GRID_PAGE_SIZE && (
            <div className="py-3 text-center text-xs text-muted-foreground">
              已全部加载 · 共 {items.length} 条
            </div>
          )
        )}
      </div>
    </div>
  );
}
