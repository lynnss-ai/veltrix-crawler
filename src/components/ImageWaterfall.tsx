import { useEffect, useRef, useState } from "react";
import { useVirtualizer } from "@tanstack/react-virtual";
import { Loader2 } from "lucide-react";
import type { ContentView } from "@/lib/api";
import { EmptyState } from "@/components/EmptyState";
import { WaterfallCard } from "@/components/WaterfallCard";

// 服务端分页下 items 即「已加载的全部」(每批 append 进来),不再做渲染侧 slice;
// hasMore 由服务端总数 total 判定。
export function ImageWaterfall({
  items,
  total,
  loading,
  onLoadMore,
  platformName,
  retrying,
  onOpenDetail,
  onRetry,
  onDelete,
}: {
  items: ContentView[];
  /// 服务端总数(同筛选口径);不传则视为已全部加载
  total?: number;
  loading: boolean;
  onLoadMore: () => void;
  platformName: (id: string) => string;
  retrying: Set<string>;
  onOpenDetail: (id: string) => void;
  onRetry: (c: ContentView) => void;
  onDelete: (id: string) => void;
}) {
  const scrollRef = useRef<HTMLDivElement>(null);
  const sentinelRef = useRef<HTMLDivElement>(null);
  const [layout, setLayout] = useState({ columns: 2, width: 0 });
  const hasMore = total !== undefined && items.length < total;

  // 与原 CSS 断点保持一致;列数交给 JS 后,虚拟器才能准确分配瀑布流 lane。
  useEffect(() => {
    const element = scrollRef.current;
    if (!element) return;
    const update = (width: number) => {
      const columns = width >= 1280 ? 5 : width >= 1024 ? 4 : width >= 640 ? 3 : 2;
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
  }, []);

  const gap = 12;
  const estimatedCardWidth = Math.max(
    160,
    (layout.width - gap * (layout.columns - 1)) / layout.columns,
  );
  const virtualizer = useVirtualizer({
    count: items.length,
    getScrollElement: () => scrollRef.current,
    getItemKey: (index) => items[index]?.id ?? index,
    // 图片卡高度随列宽变化;视频卡主要是文字。首次估算后由 ResizeObserver 实测校正。
    estimateSize: (index) =>
      items[index]?.kind === "video" ? 250 : estimatedCardWidth * (4 / 3) + 135,
    lanes: layout.columns,
    gap,
    overscan: layout.columns * 2,
    useAnimationFrameWithResizeObserver: true,
  });
  const virtualItems = virtualizer.getVirtualItems();

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
  }, [hasMore, items.length, loading, onLoadMore]);

  if (items.length === 0) {
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
          title="暂无素材"
          description="采集完成后,内容会以瀑布流展示在这里"
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
        {/* 只挂载可视区附近的卡片。已加载数据可以很多,DOM/图片解码量仍保持稳定。 */}
        <div
          className="relative w-full"
          style={{ height: virtualizer.getTotalSize() }}
        >
          {virtualItems.map((virtualItem) => {
            const c = items[virtualItem.index];
            const lane = virtualItem.lane;
            const widthPercent = 100 / layout.columns;
            const widthGap = (gap * (layout.columns - 1)) / layout.columns;
            const leftGap = (lane * gap) / layout.columns;
            return (
              <div
                key={virtualItem.key}
                ref={virtualizer.measureElement}
                data-index={virtualItem.index}
                className="absolute left-0 top-0"
                style={{
                  width: `calc(${widthPercent}% - ${widthGap}px)`,
                  transform: `translate3d(calc(${lane * widthPercent}% + ${leftGap}px), ${virtualItem.start}px, 0)`,
                }}
              >
                <WaterfallCard
                  c={c}
                  platformName={platformName}
                  retrying={retrying.has(c.id)}
                  onOpenDetail={onOpenDetail}
                  onRetry={onRetry}
                  onDelete={onDelete}
                />
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
            {loading ? "正在加载" : "继续滚动加载"} · 已显示 {items.length}/{total}
          </div>
        ) : (
          items.length > 1 && (
            <div className="py-3 text-center text-xs text-muted-foreground">
              已全部加载 · 共 {items.length} 条
            </div>
          )
        )}
      </div>
    </div>
  );
}
