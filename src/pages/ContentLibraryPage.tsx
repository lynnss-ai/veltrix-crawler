// 资产库:展示采集落库的内容(contents 表)。全量库/内容库/图片库共用本组件。
// 筛选:左侧栏(行业 + 创建时间 + 发布时间)+ 顶部(平台 chip + 关键字搜索)。
// 关键字匹配 标题 / 采集关键词 / 文案;时间为预设范围。
import { useCallback, useEffect, useMemo, useState } from "react";
import { type ColumnDef } from "@tanstack/react-table";
import {
  ArrowLeft,
  Eye,
  LayoutGrid,
  List,
  Loader2,
  MoreHorizontal,
  NotebookPen,
  RefreshCw,
  Search,
  Trash2,
  X,
} from "lucide-react";
import { type DateRange } from "react-day-picker";
import { toast } from "sonner";

import { DataTable } from "@/components/DataTable";
import { DataTableColumnHeader } from "@/components/DataTableColumnHeader";
import { FacetedFilter } from "@/components/FacetedFilter";
import { FORM_CONTROL_SIZING } from "@/lib/form-sizing";
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
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { Input } from "@/components/ui/input";
import { useResponsiveCollapse } from "@/hooks/use-responsive-collapse";
import {
  api,
  type ContentView,
  type IndustryView,
  type PlatformConfig,
} from "@/lib/api";
import { platformChipClass } from "@/lib/platforms";
import type { TaskContentFilter } from "./collect-meta";
import { ContentDetailDialog } from "@/components/content-detail-dialog";
import { EmptyState } from "@/components/EmptyState";
import {
  FilterChip,
  FilterSidebar,
  IndustryFilterToggle,
  DateRangeFilter,
  inDateRange,
} from "@/components/library-filters";
import { ImageWaterfall } from "@/components/ImageWaterfall";
import { ContentCard } from "@/components/ContentCard";
import { MediaStatusBadge } from "@/components/MediaStatusBadge";

// 瀑布流每次渲染/加载的卡片数:首屏与「加载更多」步长一致,避免一次性挂载海量图片
const GRID_PAGE_SIZE = 48;

// 图片库/内容库视图模式(瀑布流/表格)的 localStorage 持久化键(按库区分)
function viewModeStorageKey(kind: string): string {
  return `veltrix-library-view-${kind}`;
}

export function ContentLibraryPage({
  kindFilter,
  taskFilter,
  onBack,
}: {
  // title 仅用于路由区分,页面内不再展示标题
  title?: string;
  // 限定内容形态:image=图文(图片库)/ video=视频(内容库);不传=全部(全量库)
  kindFilter?: ContentView["kind"];
  // 数据穿透:按任务(及可选单次运行时间范围)过滤;来自任务列表/详情的"查看内容"
  taskFilter?: TaskContentFilter;
  // 数据穿透返回:从任务列表/详情穿透进来时提供,点「返回」回到来源页
  onBack?: () => void;
}) {
  // 任务穿透过滤开关:进来默认开;用户点"清除"后看全部
  const [taskFilterOn, setTaskFilterOn] = useState(true);
  const [contents, setContents] = useState<ContentView[]>([]);
  const [platforms, setPlatforms] = useState<PlatformConfig[]>([]);
  const [industries, setIndustries] = useState<IndustryView[]>([]);
  const [search, setSearch] = useState("");
  const [platformFilter, setPlatformFilter] = useState(""); // ""=全部
  const [kindSearch, setKindSearch] = useState<string[]>([]); // 全量库内容形态筛选;[]=全部
  const [industryFilter, setIndustryFilter] = useState("__all");
  const [createdRange, setCreatedRange] = useState<DateRange | undefined>();
  const [publishedRange, setPublishedRange] = useState<DateRange | undefined>();
  // 内容详情弹窗:当前打开的内容 id(null=关闭)
  const [detailId, setDetailId] = useState<string | null>(null);
  const [sidebarCollapsed, setSidebarCollapsed] = useResponsiveCollapse();
  // 正在重试素材下载的内容 id 集合(行级 loading,避免重复点击)
  const [retrying, setRetrying] = useState<Set<string>>(new Set());
  // 批量导出 Obsidian 进行中(防重复点击)
  const [batchSyncing, setBatchSyncing] = useState(false);
  // 待确认的批量删除:ids + 确认后清空表格选择的回调;null=未弹确认框
  const [pendingDelete, setPendingDelete] = useState<{
    ids: string[];
    reset: () => void;
  } | null>(null);
  // 图片库/内容库:瀑布流(grid)/ 表格(list)双视图切换,默认瀑布流;
  // 选择按库持久化到 localStorage,下次进入保持上次的浏览习惯。
  // 图片库瀑布流以封面为主,内容库瀑布流以视频文案为主。
  const isImageLibrary = kindFilter === "image";
  const supportsWaterfall = kindFilter === "image" || kindFilter === "video";
  const [viewMode, setViewMode] = useState<"grid" | "list">(() => {
    if (!supportsWaterfall) return "list";
    try {
      return localStorage.getItem(viewModeStorageKey(kindFilter!)) === "list"
        ? "list"
        : "grid";
    } catch {
      return "grid";
    }
  });
  const changeViewMode = (mode: "grid" | "list") => {
    setViewMode(mode);
    try {
      localStorage.setItem(viewModeStorageKey(kindFilter ?? "all"), mode);
    } catch {
      // localStorage 不可用(隐私模式等)时仅本次生效
    }
  };
  // 图片库图源:image=图文内容的图片(默认)/ cover=全量库全部内容的封面
  const [imageSource, setImageSource] = useState<"image" | "cover">("image");
  // 瀑布流增量加载:当前可见卡片数,筛选/视图变化时重置回首屏
  const [visibleCount, setVisibleCount] = useState(GRID_PAGE_SIZE);

  const platformName = useCallback((id: string) =>
    platforms.find((p) => p.id === id)?.name ?? id, [platforms]);

  useEffect(() => {
    api
      .listContents()
      .then(setContents)
      .catch((e) => toast.error(`加载内容失败: ${e}`));
    api.listPlatforms().then(setPlatforms).catch((e) => console.warn("加载平台列表失败:", e));
    api.listIndustries().then(setIndustries).catch((e) => console.warn("加载行业列表失败:", e));
  }, []);

  // 按内容形态过滤:图片库=图文 / 内容库=视频 / 全量库=全部。
  // 图片库切到「封面」图源时改为:全量库全部内容里有封面的(视频封面也作为图片素材浏览)
  const base = useMemo(() => {
    if (isImageLibrary && imageSource === "cover") {
      return contents.filter((c) => !!(c.coverPath || c.coverUrl));
    }
    let list = kindFilter
      ? contents.filter((c) => c.kind === kindFilter)
      : contents;
    // 内容库(视频库)只展示已转写出文案的视频:转写(transcript)还没出来的视频
    // 在卡片里只能显示「示例文案 · 转写完成后自动替换」占位,无浏览价值,故不展示。
    //(全量库 / 图片库不受影响)
    if (kindFilter === "video") {
      list = list.filter((c) => (c.transcript ?? "").trim() !== "");
    }
    // 数据穿透:按任务过滤;带运行时间范围则进一步按 collectedAt 落在该次运行内(单次任务穿透)
    if (taskFilter && taskFilterOn) {
      list = list.filter((c) => c.taskId === taskFilter.taskId);
      if (taskFilter.keyword) {
        list = list.filter((c) => c.keyword === taskFilter.keyword);
      }
      if (taskFilter.runStart != null && taskFilter.runEnd != null) {
        list = list.filter(
          (c) =>
            c.collectedAt >= taskFilter.runStart! &&
            c.collectedAt <= taskFilter.runEnd!,
        );
      }
    }
    return list;
  }, [contents, kindFilter, isImageLibrary, imageSource, taskFilter, taskFilterOn]);

  // 平台筛选列出全部平台(与行业侧栏一致),不只展示已有数据的平台
  const platformOptions = useMemo(() => platforms.map((p) => p.id), [platforms]);

  // 各行业内容数(侧栏角标),只统计当前库(base)
  const industryCounts = useMemo(() => {
    const map: Record<string, number> = { __all: base.length };
    for (const c of base) {
      if (c.industry) map[c.industry] = (map[c.industry] ?? 0) + 1;
    }
    return map;
  }, [base]);

  const filtered = useMemo(() => {
    return base.filter((c) => {
      if (platformFilter && c.platform !== platformFilter) return false;
      if (kindSearch.length > 0 && !kindSearch.includes(c.kind)) return false;
      if (industryFilter !== "__all" && c.industry !== industryFilter)
        return false;
      if (!inDateRange(c.collectedAt, createdRange)) return false;
      if (!inDateRange(c.publishedAt, publishedRange)) return false;
      if (search) {
        const q = search.toLowerCase();
        return (
          (c.title ?? "").toLowerCase().includes(q) ||
          c.keyword.toLowerCase().includes(q) ||
          (c.desc ?? "").toLowerCase().includes(q)
        );
      }
      return true;
    });
  }, [
    base,
    platformFilter,
    kindSearch,
    industryFilter,
    createdRange,
    publishedRange,
    search,
  ]);

  // 筛选结果或视图模式变化时,瀑布流回到首屏(避免停留在已加载更多的页码)
  useEffect(() => {
    setVisibleCount(GRID_PAGE_SIZE);
  }, [filtered, viewMode]);

  // 是否有任意筛选生效(决定显示「重置」)
  const hasFilter =
    platformFilter !== "" ||
    kindSearch.length > 0 ||
    industryFilter !== "__all" ||
    !!createdRange?.from ||
    !!publishedRange?.from ||
    search !== "";

  function resetFilters() {
    setPlatformFilter("");
    setKindSearch([]);
    setIndustryFilter("__all");
    setCreatedRange(undefined);
    setPublishedRange(undefined);
    setSearch("");
  }

  // 删除一条内容:与批量删除共用红色确认弹窗(删除不可恢复,单条也必须确认)
  function handleDelete(id: string) {
    setPendingDelete({ ids: [id], reset: () => {} });
  }

  // 批量导出到当前用户的 Obsidian vault;成功后标记已同步并清空选择
  async function handleBatchSync(ids: string[], reset: () => void) {
    if (batchSyncing || ids.length === 0) return;
    setBatchSyncing(true);
    try {
      const n = await api.syncContentsToObsidian(ids);
      if (n > 0) {
        const idSet = new Set(ids);
        setContents((prev) =>
          prev.map((x) => (idSet.has(x.id) ? { ...x, syncedByMe: true } : x)),
        );
        toast.success(`已导出 ${n}/${ids.length} 条到 Obsidian`);
        reset();
      } else {
        toast.error("导出失败(无权限或内容不存在)");
      }
    } catch (e) {
      toast.error(`导出失败: ${e}`);
    } finally {
      setBatchSyncing(false);
    }
  }

  // 批量删除(确认弹窗点「删除」后执行):后端按 id 集合删,本地列表同步移除
  async function handleBatchDelete() {
    if (!pendingDelete) return;
    const { ids, reset } = pendingDelete;
    setPendingDelete(null);
    try {
      const n = await api.removeContents(ids);
      const idSet = new Set(ids);
      setContents((prev) => prev.filter((x) => !idSet.has(x.id)));
      toast.success(`已删除 ${n} 条`);
      reset();
    } catch (e) {
      toast.error(`批量删除失败: ${e}`);
    }
  }

  // 重新拉取素材:重跑下载并就地刷新该行状态。
  // 视频直链可能已过期(403),重试不一定成功——失败时提示需重新采集。
  async function handleRetry(c: ContentView) {
    if (retrying.has(c.id)) return;
    setRetrying((prev) => new Set(prev).add(c.id));
    try {
      const res = await api.retryContentMedia(c.id);
      setContents((prev) =>
        prev.map((x) =>
          x.id === res.id
            ? {
                ...x,
                mediaStatus: res.mediaStatus,
                audioExtracted: res.audioExtracted,
                mediaError: res.mediaError,
              }
            : x,
        ),
      );
      if (res.mediaStatus === "success") {
        toast.success("素材已重新拉取");
      } else {
        toast.error(
          `重试仍失败${res.mediaError ? `: ${res.mediaError}` : ""} · 链接可能已过期,建议重新采集`,
        );
      }
    } catch (e) {
      toast.error(`重试失败: ${e}`);
    } finally {
      setRetrying((prev) => {
        const next = new Set(prev);
        next.delete(c.id);
        return next;
      });
    }
  }

  const columns = useMemo<ColumnDef<ContentView>[]>(
    () => [
      {
        id: "select",
        enableSorting: false,
        header: ({ table }) => (
          <Checkbox
            checked={
              table.getIsAllPageRowsSelected() ||
              (table.getIsSomePageRowsSelected() && "indeterminate")
            }
            onCheckedChange={(v) => table.toggleAllPageRowsSelected(!!v)}
            aria-label="全选本页"
          />
        ),
        cell: ({ row }) => (
          <Checkbox
            checked={row.getIsSelected()}
            onCheckedChange={(v) => row.toggleSelected(!!v)}
            aria-label="选择本条"
          />
        ),
      },
      {
        id: "content",
        header: "内容",
        enableSorting: false,
        // 卡片:左封面 + 右(作者头像/昵称/抖音号、关键词红标+标题、话题、互动数据)
        cell: ({ row }) => (
          <ContentCard
            c={row.original}
            platformName={platformName}
            // 内容库/图片库已按形态限定,形式徽标冗余,只在全量库展示
            showKind={!kindFilter}
            onOpenDetail={() => setDetailId(row.original.id)}
          />
        ),
      },
      {
        id: "media",
        header: ({ column }) => (
          <DataTableColumnHeader column={column} title="素材" />
        ),
        accessorKey: "mediaStatus",
        cell: ({ row }) => (
          <MediaStatusBadge
            c={row.original}
            retrying={retrying.has(row.original.id)}
            onRetry={() => handleRetry(row.original)}
          />
        ),
      },
      {
        id: "actions",
        header: "操作",
        enableSorting: false,
        cell: ({ row }) => {
          const c = row.original;
          return (
            <DropdownMenu>
              <DropdownMenuTrigger asChild>
                <Button
                  variant="ghost"
                  size="icon"
                  className="size-7 cursor-pointer"
                >
                  <MoreHorizontal className="size-4" />
                </Button>
              </DropdownMenuTrigger>
              <DropdownMenuContent align="end">
                <DropdownMenuItem onClick={() => setDetailId(c.id)}>
                  <Eye className="size-4" />
                  详情
                </DropdownMenuItem>
                {c.mediaStatus === "failed" && (
                  <DropdownMenuItem
                    disabled={retrying.has(c.id)}
                    onClick={() => handleRetry(c)}
                  >
                    <RefreshCw className="size-4" />
                    重新拉取素材
                  </DropdownMenuItem>
                )}
                <DropdownMenuItem
                  variant="destructive"
                  onClick={() => handleDelete(c.id)}
                >
                  <Trash2 className="size-4" />
                  删除
                </DropdownMenuItem>
              </DropdownMenuContent>
            </DropdownMenu>
          );
        },
      },
    ],
    // 依赖 retrying:行级重试态变化时重建列,徽章 loading / 禁用态才能刷新
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [platforms, platformName, retrying],
  );

  return (
    <>
      {/* 数据穿透提示条:从任务列表/详情跳来时显示,可一键清除看全部 */}
      {taskFilter && taskFilterOn && (
        <div className="mb-3 flex shrink-0 items-center justify-between gap-2 rounded-md border border-primary/30 bg-primary/5 px-3 py-2 text-xs">
          <span className="text-foreground">
            正在查看任务
            <span className="mx-1 font-medium text-primary">
              「{taskFilter.taskName || taskFilter.taskId}」
            </span>
            {taskFilter.keyword ? (
              <>
                关键词
                <span className="mx-1 font-medium text-primary">
                  「{taskFilter.keyword}」
                </span>
                采集的内容
              </>
            ) : taskFilter.runStart != null && taskFilter.runEnd != null ? (
              "某次运行采集的内容"
            ) : (
              "采集的全部内容"
            )}
            <span className="ml-1 text-muted-foreground">
              · 共 {filtered.length} 条
            </span>
          </span>
          <div className="flex shrink-0 items-center gap-1">
            {/* 穿透返回:回到来源页(任务列表/详情) */}
            {onBack && (
              <Button
                variant="ghost"
                size="sm"
                className="h-7 cursor-pointer"
                onClick={onBack}
              >
                <ArrowLeft className="mr-1 size-3.5" />
                返回
              </Button>
            )}
            <Button
              variant="ghost"
              size="sm"
              className="h-7 cursor-pointer"
              onClick={() => setTaskFilterOn(false)}
            >
              <X className="mr-1 size-3.5" />
              清除筛选 · 看全部
            </Button>
          </div>
        </div>
      )}
      <div className="flex min-h-0 min-w-0 flex-1 gap-4">
      {/* 左侧:行业筛选(可折叠) */}
        {!sidebarCollapsed && (
          <FilterSidebar
            industries={industries}
            industryCounts={industryCounts}
            industryFilter={industryFilter}
            onIndustry={setIndustryFilter}
            onCollapse={() => setSidebarCollapsed(true)}
          />
        )}

        {/* 右侧:工具条 + 表格。min-h-0 让 DataTable 的 flex-1 正确约束高度,表格内部滚动 */}
        <div
          className={`flex min-h-0 min-w-0 flex-1 flex-col gap-3 ${FORM_CONTROL_SIZING}`}
        >
          {/* 行业按钮(收起态) + 日期区间 + 关键字搜索同排 */}
          <div className="flex flex-wrap items-center gap-x-4 gap-y-2">
            {sidebarCollapsed && (
              <IndustryFilterToggle
                onExpand={() => setSidebarCollapsed(false)}
              />
            )}
            <DateRangeFilter
              title="创建日期"
              value={createdRange}
              onChange={setCreatedRange}
            />
            <DateRangeFilter
              title="发布日期"
              value={publishedRange}
              onChange={setPublishedRange}
            />
            {!kindFilter && (
              <FacetedFilter
                title="内容形式"
                options={[
                  { value: "video", label: "视频" },
                  { value: "image", label: "图文" },
                  { value: "article", label: "文章" },
                ]}
                selected={kindSearch}
                onChange={setKindSearch}
              />
            )}
            <div className="relative w-full sm:w-72 lg:w-80">
              <Search className="pointer-events-none absolute left-3 top-1/2 size-4 -translate-y-1/2 text-muted-foreground" />
              <Input
                value={search}
                onChange={(e) => setSearch(e.target.value)}
                placeholder="标题 / 关键词 / 文案"
                className="pl-9"
              />
            </div>
            {hasFilter && (
              <Button
                variant="ghost"
                className="cursor-pointer px-2 lg:px-3"
                onClick={resetFilters}
              >
                重置
                <X />
              </Button>
            )}
            {supportsWaterfall && (
              <div className="ml-auto inline-flex h-10 items-center rounded-md border p-0.5">
                <ViewModeButton
                  active={viewMode === "grid"}
                  label="瀑布流"
                  icon={LayoutGrid}
                  onClick={() => changeViewMode("grid")}
                />
                <ViewModeButton
                  active={viewMode === "list"}
                  label="表格"
                  icon={List}
                  onClick={() => changeViewMode("list")}
                />
              </div>
            )}
          </div>
          {/* 平台筛选(不选即全部,点已选取消)+ 图片库图源切换,同一行展示 */}
          <div className="flex flex-wrap items-center gap-2">
            {/* 图片库:图文=图文内容的图片;封面=全量库全部内容的封面。与平台筛选同排,chip 同高。
                图源 / 平台 用纯文字标签分组,与 chip 等高对齐 */}
            {isImageLibrary && (
              <>
                <span className="text-xs font-medium text-muted-foreground">
                  图源
                </span>
                <FilterChip
                  label="图文"
                  active={imageSource === "image"}
                  onClick={() => setImageSource("image")}
                />
                <FilterChip
                  label="封面"
                  active={imageSource === "cover"}
                  onClick={() => setImageSource("cover")}
                />
                <span className="mx-1 h-4 w-px bg-border" />
                <span className="text-xs font-medium text-muted-foreground">
                  平台
                </span>
              </>
            )}
            {platformOptions.map((id) => (
              <button
                key={id}
                type="button"
                className={platformChipClass(id, platformFilter === id)}
                onClick={() =>
                  setPlatformFilter((prev) => (prev === id ? "" : id))
                }
              >
                {platformName(id)}
              </button>
            ))}
          </div>

          {supportsWaterfall && viewMode === "grid" ? (
            <ImageWaterfall
              items={filtered}
              visibleCount={visibleCount}
              onLoadMore={() =>
                setVisibleCount((prev) => prev + GRID_PAGE_SIZE)
              }
              platformName={platformName}
              retrying={retrying}
              onOpenDetail={setDetailId}
              onRetry={handleRetry}
              onDelete={handleDelete}
            />
          ) : (
            <DataTable
              columns={columns}
              data={filtered}
              itemLabel="内容"
              getRowId={(c) => c.id}
              defaultPageSize={50}
              renderToolbar={(table) => {
                const ids = table
                  .getSelectedRowModel()
                  .rows.map((r) => r.original.id);
                if (ids.length === 0) return null;
                const reset = () => table.resetRowSelection();
                return (
                  <div className="flex flex-wrap items-center gap-2 rounded-lg border bg-card px-3 py-2">
                    <span className="text-sm font-medium">
                      已选 {ids.length} 条
                    </span>
                    <Button
                      variant="outline"
                      size="sm"
                      className="cursor-pointer"
                      disabled={batchSyncing}
                      onClick={() => handleBatchSync(ids, reset)}
                    >
                      {batchSyncing ? (
                        <Loader2 className="animate-spin" />
                      ) : (
                        <NotebookPen />
                      )}
                      导出到 Obsidian
                    </Button>
                    <Button
                      variant="destructive"
                      size="sm"
                      className="cursor-pointer"
                      onClick={() => setPendingDelete({ ids, reset })}
                    >
                      <Trash2 />
                      批量删除
                    </Button>
                    <Button
                      variant="ghost"
                      size="sm"
                      className="cursor-pointer"
                      onClick={reset}
                    >
                      取消选择
                    </Button>
                  </div>
                );
              }}
              emptyState={
                <EmptyState
                  title="暂无内容"
                  description="采集任务完成后,内容会出现在这里"
                />
              }
            />
          )}
        </div>
      </div>
      <ContentDetailDialog
        items={filtered}
        activeId={detailId}
        onActiveIdChange={setDetailId}
      />
      {/* 批量删除确认:删除不可恢复,弹窗确认避免误触 */}
      <AlertDialog
        open={!!pendingDelete}
        onOpenChange={(o) => {
          if (!o) setPendingDelete(null);
        }}
      >
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>
              删除选中的 {pendingDelete?.ids.length ?? 0} 条内容?
            </AlertDialogTitle>
            <AlertDialogDescription>
              仅删除库中记录(已下载的本地素材文件不受影响),删除后不可恢复。
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel className="cursor-pointer">
              取消
            </AlertDialogCancel>
            <AlertDialogAction
              className="cursor-pointer bg-destructive text-white hover:bg-destructive/90"
              onClick={handleBatchDelete}
            >
              删除
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
    </>
  );
}

// 视图切换按钮:激活态高亮(图片库「瀑布流 / 列表」二选一)
function ViewModeButton({
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
