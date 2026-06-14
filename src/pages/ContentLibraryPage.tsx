// 资产库:展示采集落库的内容(contents 表)。全量库/内容库/图片库共用本组件。
// 筛选:左侧栏(行业 + 创建时间 + 发布时间)+ 顶部(平台 chip + 关键字搜索)。
// 关键字匹配 标题 / 采集关键词 / 文案;时间为预设范围。
import { useEffect, useMemo, useRef, useState } from "react";
import { type ColumnDef } from "@tanstack/react-table";
import {
  AudioLines,
  Bookmark,
  CalendarDays,
  CheckCircle2,
  ChevronLeft,
  Clock,
  Eye,
  FileText,
  FileQuestion,
  Filter,
  Heart,
  Image as ImageIcon,
  LayoutGrid,
  List,
  Loader2,
  MessageCircle,
  MoreHorizontal,
  NotebookPen,
  RefreshCw,
  Search,
  Share2,
  Trash2,
  Video,
  XCircle,
  X,
} from "lucide-react";
import { openUrl } from "@tauri-apps/plugin-opener";
import { convertFileSrc } from "@tauri-apps/api/core";
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
import { Calendar } from "@/components/ui/calendar";
import { Checkbox } from "@/components/ui/checkbox";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { Input } from "@/components/ui/input";
import {
  Popover,
  PopoverContent,
  PopoverTrigger,
} from "@/components/ui/popover";
import { SimpleTooltip } from "@/components/SimpleTooltip";
import { useResponsiveCollapse } from "@/hooks/use-responsive-collapse";
import {
  api,
  type ContentView,
  type IndustryView,
  type PlatformConfig,
} from "@/lib/api";
import {
  authorProfileUrl,
  contentDetailUrl,
  labelBadgeClass,
  platformChipClass,
  platformClass,
  platformSolidClass,
} from "@/lib/platforms";
import { ContentDetailDialog } from "@/components/content-detail-dialog";
import { EmptyState } from "@/components/EmptyState";

// 瀑布流每次渲染/加载的卡片数:首屏与「加载更多」步长一致,避免一次性挂载海量图片
const GRID_PAGE_SIZE = 48;

// 图片库/内容库视图模式(瀑布流/表格)的 localStorage 持久化键(按库区分)
function viewModeStorageKey(kind: string): string {
  return `veltrix-library-view-${kind}`;
}

// 视频文案缺失时的演示数据:转写链路尚未跑通时让瀑布流有内容可看。
// 按 content_id 稳定取样(同一条永远同一段),卡片上带「示例」标记;真实转写落库后自动替换。
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

// 内容形式按语义固定配色:视频=蓝、图文=紫、文章=琥珀、未知=灰,跨页面一眼可辨
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

// 日期区间筛选:ts(Unix 秒)是否落在 [from 当天 0 点, to 当天 23:59] 内。未选起始=全部。
function inDateRange(
  ts: number | null | undefined,
  range: DateRange | undefined,
): boolean {
  if (!range?.from) return true;
  if (!ts) return false; // 选了日期区间时,没有时间的内容排除
  const ms = ts * 1000;
  const start = new Date(range.from).setHours(0, 0, 0, 0);
  const end = new Date(range.to ?? range.from).setHours(23, 59, 59, 999);
  return ms >= start && ms <= end;
}

// 日期格式 YYYY-MM-DD
function fmtDate(d: Date): string {
  const m = String(d.getMonth() + 1).padStart(2, "0");
  const day = String(d.getDate()).padStart(2, "0");
  return `${d.getFullYear()}-${m}-${day}`;
}

// 互动数:过万折算成「万」
function formatCount(n?: number | null): string {
  if (n == null) return "—";
  if (n >= 10000) return `${(n / 10000).toFixed(1)}万`;
  return String(n);
}

// 视频时长(秒)→ mm:ss;图文/无时长显示 —
function formatDuration(sec?: number | null): string {
  if (sec == null || sec <= 0) return "—";
  const m = Math.floor(sec / 60);
  const s = sec % 60;
  return `${m}:${String(s).padStart(2, "0")}`;
}

// Unix 秒 → 本地日期时间
function formatTime(ts?: number | null): string {
  if (!ts) return "—";
  return new Date(ts * 1000).toLocaleString();
}

// 复制文本到剪贴板 + 提示
function copyText(text: string, label: string) {
  if (!text) return;
  // navigator.clipboard 在非安全上下文/部分 WebView 下可能为 undefined,先判空避免同步 TypeError
  if (!navigator.clipboard) {
    toast.error("当前环境不支持复制");
    return;
  }
  navigator.clipboard
    .writeText(text)
    .then(() => toast.success(`已复制${label}`))
    .catch(() => toast.error("复制失败"));
}

export function ContentLibraryPage({
  kindFilter,
}: {
  // title 仅用于路由区分,页面内不再展示标题
  title?: string;
  // 限定内容形态:image=图文(图片库)/ video=视频(内容库);不传=全部(全量库)
  kindFilter?: ContentView["kind"];
}) {
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

  const platformName = (id: string) =>
    platforms.find((p) => p.id === id)?.name ?? id;

  useEffect(() => {
    api
      .listContents()
      .then(setContents)
      .catch((e) => toast.error(`加载内容失败: ${e}`));
    api.listPlatforms().then(setPlatforms).catch(() => {});
    api.listIndustries().then(setIndustries).catch(() => {});
  }, []);

  // 按内容形态过滤:图片库=图文 / 内容库=视频 / 全量库=全部。
  // 图片库切到「封面」图源时改为:全量库全部内容里有封面的(视频封面也作为图片素材浏览)
  const base = useMemo(() => {
    if (isImageLibrary && imageSource === "cover") {
      return contents.filter((c) => !!(c.coverPath || c.coverUrl));
    }
    return kindFilter
      ? contents.filter((c) => c.kind === kindFilter)
      : contents;
  }, [contents, kindFilter, isImageLibrary, imageSource]);

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

  // 依赖 platforms:平台异步加载后重建列,否则 platformName 闭包锁在空列表显示 id
  // eslint-disable-next-line react-hooks/exhaustive-deps
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
            onOpenDetail={() => setDetailId(row.original.id)}
          />
        ),
      },
      {
        // 平台 / 内容形式 / 所属行业合并一列:彩色徽标横排,省横向空间。
        // 内容库 / 图片库已按形态限定,形式徽标冗余,只在全量库显示
        id: "meta",
        accessorKey: "platform",
        header: ({ column }) => (
          <DataTableColumnHeader
            column={column}
            title={kindFilter ? "平台 | 行业" : "平台 | 形式 | 行业"}
          />
        ),
        // 列头已用竖线表意,单元格内只排彩色徽标、不再加分隔线
        cell: ({ row }) => {
          const c = row.original;
          const meta = KIND_META[c.kind] ?? KIND_META.unknown;
          const Icon = meta.icon;
          return (
            <div className="flex items-center gap-1.5 whitespace-nowrap">
              <span
                className={`inline-block rounded px-2 py-0.5 text-[11px] font-medium ${platformClass(c.platform)}`}
              >
                {platformName(c.platform)}
              </span>
              {!kindFilter && (
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
          );
        },
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
    [platforms, retrying],
  );

  return (
    <>
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
              <SimpleTooltip content="展开行业筛选">
                <Button
                  variant="outline"
                  className="cursor-pointer"
                  onClick={() => setSidebarCollapsed(false)}
                >
                  <Filter />
                  行业
                </Button>
              </SimpleTooltip>
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

// 本地优先图片:有本地路径走 asset 协议显示,加载失败回退平台外链,外链再失败隐藏。
// 封面与头像共用,避免两处重复 onError 回退逻辑。
function LocalFirstImage({
  localPath,
  externalUrl,
  className,
  onClick,
}: {
  localPath: string | null;
  externalUrl: string;
  className: string;
  onClick?: () => void;
}) {
  const src = localPath ? convertFileSrc(localPath) : externalUrl;
  return (
    <img
      src={src}
      alt=""
      loading="lazy"
      data-fallback={localPath ? externalUrl : ""}
      className={className}
      onClick={onClick}
      onError={(e) => {
        // 本地文件缺失则回退平台外链,外链再失败才隐藏
        const img = e.currentTarget;
        const fb = img.dataset.fallback;
        if (fb && img.src !== fb) {
          img.dataset.fallback = "";
          img.src = fb;
        } else {
          img.style.display = "none";
        }
      }}
    />
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

// 图片库瀑布流:CSS multi-column 让封面按原始比例错落排列;增量「加载更多」避免一次性挂载海量图片。
function ImageWaterfall({
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

  // 滚动自动加载:底部哨兵进入视口(提前 300px 预加载)即追加下一页,免手动点击。
  // 追加后哨兵仍可见时,visibleCount 变化触发 effect 重建 observer,会继续加载直到推出视口。
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
        {/* 列间距用 gap,卡片纵向间距用 mb;break-inside-avoid 防卡片跨列断裂 */}
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

// 瀑布流单卡:封面(本地优先、原始比例)+ 平台/张数/关键词浮标 + hover 操作菜单 + 标题/作者/点赞页脚。
function WaterfallCard({
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
      {/* 视频卡(内容库瀑布流)不放封面,以文案为主体;头部彩标行替代封面上的角标 */}
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
        /* 图文卡:封面 + 角标,点击打开内容详情抽屉 */
        <div
          className="relative cursor-pointer overflow-hidden bg-muted"
          onClick={() => onOpenDetail(c.id)}
        >
          {hasCover ? (
            <LocalFirstImage
              localPath={c.coverPath}
              externalUrl={coverExternal}
              className="w-full object-cover transition duration-300 group-hover:scale-[1.03]"
            />
          ) : (
            <div className="flex aspect-[3/4] items-center justify-center">
              <ImageIcon className="size-8 text-muted-foreground" />
            </div>
          )}
          {/* 顶部渐变压暗:平台标叠在亮色/花哨封面上也不会被吃掉 */}
          <div className="pointer-events-none absolute inset-x-0 top-0 h-14 bg-gradient-to-b from-black/45 to-transparent" />
          {/* 平台标(左上):实心彩底白字 + 白描边 + 重阴影,任何封面上都醒目 */}
          <span
            className={`absolute left-2 top-2 rounded-md px-2 py-0.5 text-xs font-bold shadow-lg ring-1 ring-white/40 ${platformSolidClass(c.platform)}`}
          >
            {platformName(c.platform)}
          </span>
          {/* 底部浮层:关键词红标(左)+ 多图张数(右),底端对齐;
              渐变压暗保证两个角标在任何封面上都清晰 */}
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

      {/* hover 操作菜单(右上):停止冒泡避免触发封面点击 */}
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

      {/* 页脚:标题(加大加粗、两行截断,点击打开详情)+ 视频文案 + 作者头像/昵称 + 点赞数 */}
      <div className="space-y-2 p-2.5">
        <p
          onClick={() => onOpenDetail(c.id)}
          className="line-clamp-2 cursor-pointer text-sm font-medium leading-snug text-foreground transition-colors hover:text-primary"
        >
          {titleText}
        </p>
        {/* 视频卡以文案为主体:真实转写优先;链路未跑通时用演示文案占位并打「示例」标 */}
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
}

// 单条内容卡片:左封面 + 右(作者头像/昵称/抖音ID、关键词红标+标题、话题、互动数据)。
// 平台与操作各自独立成列,卡片只承载内容主体。点标题打开应用内详情弹窗。
function ContentCard({
  c,
  onOpenDetail,
}: {
  c: ContentView;
  onOpenDetail: () => void;
}) {
  const meta = KIND_META[c.kind] ?? KIND_META.unknown;
  const Icon = meta.icon;
  // 本地优先:素材下载成功用本地文件(asset 协议),失败/未下载回退平台外链
  const coverExternal = c.coverUrl || c.imageUrls[0] || "";
  const hasCover = Boolean(c.coverPath || coverExternal);
  const hasAvatar = Boolean(c.avatarPath || c.authorAvatar);
  // 话题已由后端从 desc 剥离到 topics 字段,这里直接用原文,不再正则切(避免误删 #1/C# 等合法井号)
  const titleText = c.title || c.desc || "(无文案)";
  // 作者主页链接(抖音/快手);点头像跳转
  const homeUrl = authorProfileUrl(c.platform, c.authorUid);
  // 视频详情链接(抖音/快手);点封面跳转,其余回退视频直链
  const detailUrl = contentDetailUrl(c.platform, c.contentId) || c.videoUrl;
  return (
    // 固定列宽:不随标题长度撑宽,长标题在卡片内换行(窄屏 max-w-full 收缩,表格横向滚动兜底)。
    // 平台/形式/行业合并一列后横向空间富余,内容列加宽到 64rem 让标题/话题少折行
    <div className="flex w-[64rem] max-w-full gap-3 py-1">
      {/* 封面(竖图):点击跳转视频详情,hover 微透显示可点。
          min-h 保底 + self-stretch 随卡片行高拉伸(长标题撑高时 object-cover 裁切填满,不留底部空缺) */}
      {hasCover ? (
        <SimpleTooltip content={detailUrl ? "打开视频详情" : "暂无详情链接"}>
          <LocalFirstImage
            localPath={c.coverPath}
            externalUrl={coverExternal}
            className={`h-auto min-h-32 w-32 shrink-0 self-stretch rounded-md object-cover transition ${
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
        <div className="flex min-h-32 w-32 shrink-0 items-center justify-center self-stretch rounded-md bg-muted">
          <Icon className="size-6 text-muted-foreground" />
        </div>
      )}

      {/* 右侧信息 */}
      <div className="flex min-w-0 flex-1 flex-col gap-2">
        {/* 作者:头像(跳主页)+ 昵称/抖音号(点击复制) */}
        <div className="flex items-center gap-2">
          {/* 头像:点击跳转作者主页,hover 高亮环 */}
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
          {/* 昵称(上,点击复制)+ 抖音号(下,点击复制) */}
          <div className="flex min-w-0 flex-col items-start">
            <SimpleTooltip content="点击复制昵称">
              <span
                className="max-w-full cursor-pointer truncate text-sm font-medium text-foreground hover:underline"
                onClick={() => copyText(c.authorNickname, "昵称")}
              >
                {c.authorNickname || "—"}
              </span>
            </SimpleTooltip>
            <SimpleTooltip content="点击复制抖音号">
              <span
                className="max-w-full cursor-pointer truncate text-xs text-muted-foreground hover:underline"
                onClick={() => copyText(c.authorUid, "抖音号")}
              >
                {c.authorUid}
              </span>
            </SimpleTooltip>
          </div>
        </div>

        {/* 作者信息下:关键词红标 + 发布/创建时间(表格不再单列时间,信息聚拢到卡片) */}
        <div className="flex flex-wrap items-center gap-x-3 gap-y-1 text-xs text-muted-foreground">
          {c.keyword && (
            <span className="inline-flex items-center gap-1 rounded bg-red-500 px-1.5 py-0.5 text-[11px] font-medium text-white">
              <Search className="size-3" />
              {c.keyword}
            </span>
          )}
          <span className="inline-flex items-center gap-1 whitespace-nowrap">
            <CalendarDays className="size-3" />
            发布:{formatTime(c.publishedAt)}
          </span>
          <span className="inline-flex items-center gap-1 whitespace-nowrap">
            <Clock className="size-3" />
            创建:{formatTime(c.collectedAt)}
          </span>
        </div>

        {/* 标题:完整换行,点击打开应用内详情弹窗 */}
        <SimpleTooltip content="查看详情">
          <span
            onClick={onOpenDetail}
            className="w-fit cursor-pointer whitespace-normal break-words text-xs text-muted-foreground transition-colors hover:text-primary hover:underline"
          >
            {titleText}
          </span>
        </SimpleTooltip>

        {/* 话题(紫标),包裹换行 */}
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

        {/* 互动数据 + 时长:图标 + 着色胶囊(圆角矩形,深色) */}
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

        {/* 语音文案(仅视频):转写成功展示可展开文本,失败显示红标 */}
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
}

// 素材步骤成功态的按类型配色:一眼分清是哪一步完成了(失败统一红、待处理统一灰)
const STEP_SUCCESS_CLS: Record<string, string> = {
  视频: "bg-sky-100 text-sky-700 dark:bg-sky-950/60 dark:text-sky-300",
  音频: "bg-violet-100 text-violet-700 dark:bg-violet-950/60 dark:text-violet-300",
  文案: "bg-amber-100 text-amber-700 dark:bg-amber-950/60 dark:text-amber-300",
  图片: "bg-teal-100 text-teal-700 dark:bg-teal-950/60 dark:text-teal-300",
  评论: "bg-blue-100 text-blue-700 dark:bg-blue-950/60 dark:text-blue-300",
  意向: "bg-fuchsia-100 text-fuchsia-700 dark:bg-fuchsia-950/60 dark:text-fuchsia-300",
};

// 取步骤成功色:label 可能带进度(如「图片 3/5」),按前缀匹配;未知类型回退绿色
function stepSuccessCls(label: string): string {
  for (const [key, cls] of Object.entries(STEP_SUCCESS_CLS)) {
    if (label.startsWith(key)) return cls;
  }
  return "bg-emerald-100 text-emerald-700 dark:bg-emerald-950/60 dark:text-emerald-300";
}

// 素材状态徽章:展示下载/音频提取结果。失败态可点击重新拉取。
// 成功 + 视频已提取音频 → 额外音频标识;失败 → 红标 + tooltip 带原因。
// 单个步骤状态小标:true=按类型彩色✓ / false=红✗ / null=灰待处理
function StepBadge({
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
  const Icon = state === true ? CheckCircle2 : state === false ? XCircle : Clock;
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

// 细粒度素材状态:视频(视频↓/音频/文案)、图文(图片 done/total),并附评论/意向小标 + 失败重试
function MediaStatusBadge({
  c,
  retrying,
  onRetry,
}: {
  c: ContentView;
  retrying: boolean;
  onRetry: () => void;
}) {
  if (retrying) {
    return (
      <span className="inline-flex items-center gap-1 whitespace-nowrap rounded-md bg-secondary px-2 py-0.5 text-[11px] font-medium text-secondary-foreground">
        <Loader2 className="size-3.5 animate-spin" />
        拉取中
      </span>
    );
  }

  const isVideo = c.kind === "video";
  // 转写三态:有文本=成功,有错误=失败,否则未处理
  const transcriptState: boolean | null = c.transcript
    ? true
    : c.transcriptError
      ? false
      : null;
  // 图片三态:全部下完=成功,下了一部分=进行中(null),一张没下成功且有总数=失败
  const imageState: boolean | null =
    c.imageTotal == null || c.imageTotal === 0
      ? c.mediaStatus === "success"
        ? true
        : c.mediaStatus === "failed"
          ? false
          : null
      : c.imageDone === c.imageTotal
        ? true
        : (c.imageDone ?? 0) > 0
          ? null
          : false;

  return (
    <div className="flex flex-wrap items-center gap-1">
      {isVideo ? (
        <>
          <StepBadge label="视频" state={c.videoDownloaded} errorTip={c.mediaError} />
          <StepBadge label="音频" state={c.audioExtracted} errorTip={c.mediaError} />
          <StepBadge label="文案" state={transcriptState} errorTip={c.transcriptError} />
        </>
      ) : (
        <StepBadge
          label={
            c.imageTotal != null && c.imageTotal > 0
              ? `图片 ${c.imageDone ?? 0}/${c.imageTotal}`
              : "素材"
          }
          state={imageState}
          errorTip={c.mediaError}
        />
      )}
      {c.commentCollected === true && <StepBadge label="评论" state={true} />}
      {c.intentAnalyzed === true && <StepBadge label="意向" state={true} />}
      {c.mediaStatus === "failed" && (
        <SimpleTooltip content="点击重新拉取素材">
          <button
            type="button"
            onClick={onRetry}
            className="inline-flex cursor-pointer items-center gap-0.5 whitespace-nowrap rounded bg-rose-100 px-1.5 py-0.5 text-[10px] font-medium text-rose-700 transition-colors hover:bg-rose-200 dark:bg-rose-950/60 dark:text-rose-300 dark:hover:bg-rose-900/60"
          >
            <RefreshCw className="size-3" />
            重试
          </button>
        </SimpleTooltip>
      )}
    </div>
  );
}

// 筛选 chip:常规圆角矩形,选中高亮(与采集任务页平台筛选一致)
function FilterChip({
  label,
  active,
  onClick,
}: {
  label: string;
  active: boolean;
  onClick: () => void;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      className={`cursor-pointer rounded-md border px-3 py-1 text-xs transition-colors ${
        active
          ? "border-primary bg-primary text-primary-foreground"
          : "border-border text-muted-foreground hover:bg-accent hover:text-foreground"
      }`}
    >
      {label}
    </button>
  );
}

// 左侧筛选侧栏:只做行业筛选(带数量角标)
function FilterSidebar({
  industries,
  industryCounts,
  industryFilter,
  onIndustry,
  onCollapse,
}: {
  industries: IndustryView[];
  industryCounts: Record<string, number>;
  industryFilter: string;
  onIndustry: (v: string) => void;
  onCollapse: () => void;
}) {
  return (
    <div className="flex w-48 shrink-0 flex-col overflow-hidden rounded-xl border bg-card">
      <div className="flex h-10 items-center justify-between border-b px-3">
        <div className="flex items-center gap-1.5 text-sm font-medium">
          <Filter className="size-3.5 text-muted-foreground" />
          行业筛选
        </div>
        <SimpleTooltip content="收起">
          <Button
            variant="ghost"
            size="icon-xs"
            className="cursor-pointer"
            onClick={onCollapse}
          >
            <ChevronLeft />
          </Button>
        </SimpleTooltip>
      </div>
      <div className="flex-1 space-y-0.5 overflow-auto p-2">
        <IndustryFilterItem
          label="全部行业"
          count={industryCounts.__all ?? 0}
          active={industryFilter === "__all"}
          onClick={() => onIndustry("__all")}
        />
        {industries.map((ind) => (
          <IndustryFilterItem
            key={ind.id}
            label={ind.name}
            count={industryCounts[ind.name] ?? 0}
            active={industryFilter === ind.name}
            onClick={() => onIndustry(ind.name)}
          />
        ))}
      </div>
    </div>
  );
}

// 日期区间筛选:Popover + 双月日历,显示选中区间,可清除
function DateRangeFilter({
  title,
  value,
  onChange,
}: {
  title: string;
  value: DateRange | undefined;
  onChange: (range: DateRange | undefined) => void;
}) {
  // 未选区间时按钮直接显示字段名(创建日期 / 发布日期);选中后显示「字段名 区间」
  const range = value?.from
    ? value.to
      ? `${fmtDate(value.from)} ~ ${fmtDate(value.to)}`
      : fmtDate(value.from)
    : "";
  const label = range ? `${title} · ${range}` : title;
  return (
    <Popover>
      <PopoverTrigger asChild>
        <Button variant="outline" className="h-10 cursor-pointer">
          <CalendarDays className="size-3.5" />
          {label}
        </Button>
      </PopoverTrigger>
      <PopoverContent className="w-auto p-0" align="start">
        <Calendar
          mode="range"
          selected={value}
          onSelect={onChange}
          numberOfMonths={2}
        />
        {value?.from && (
          <div className="border-t p-2 text-right">
            <Button
              variant="ghost"
              size="sm"
              className="cursor-pointer"
              onClick={() => onChange(undefined)}
            >
              清除
            </Button>
          </div>
        )}
      </PopoverContent>
    </Popover>
  );
}

function IndustryFilterItem({
  label,
  count,
  active,
  onClick,
}: {
  label: string;
  count: number;
  active: boolean;
  onClick: () => void;
}) {
  return (
    <div
      onClick={onClick}
      className={`flex cursor-pointer items-center gap-2 rounded-md px-2 py-1.5 text-sm transition-colors ${
        active
          ? "bg-accent font-medium text-accent-foreground"
          : "hover:bg-accent/50"
      }`}
    >
      <span className="flex-1 truncate">{label}</span>
      <span className="text-xs text-muted-foreground">{count}</span>
    </div>
  );
}
