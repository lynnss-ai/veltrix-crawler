// 资产库:展示采集落库的内容(contents 表)。全量库/内容库/图片库共用本组件。
// 筛选:左侧栏(行业 + 创建时间 + 发布时间)+ 顶部(平台 chip + 关键字搜索)。
// 关键字匹配 标题 / 采集关键词 / 文案;时间为预设范围。
import { useCallback, useDeferredValue, useEffect, useMemo, useRef, useState } from "react";
import { type ColumnDef } from "@tanstack/react-table";
import {
  ArrowLeft,
  Eye,
  FileSpreadsheet,
  LayoutGrid,
  List,
  Loader2,
  MessagesSquare,
  MoreHorizontal,
  NotebookPen,
  RefreshCw,
  Search,
  Trash2,
  X,
} from "lucide-react";
import { type DateRange } from "react-day-picker";
import { toast } from "sonner";
import * as XLSX from "xlsx";
import { save } from "@tauri-apps/plugin-dialog";
import { listen } from "@tauri-apps/api/event";

import { DataTable, type ServerTableState } from "@/components/DataTable";
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
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { useResponsiveCollapse } from "@/hooks/use-responsive-collapse";
import {
  api,
  type ContentLibraryStats,
  type ContentListQuery,
  type ContentView,
  type IndustryView,
  type PlatformConfig,
} from "@/lib/api";
import { platformChipClass, contentDetailUrl } from "@/lib/platforms";
import { formatTimestamp } from "@/lib/utils";
import { recordDownload } from "@/lib/download-history";
import type {
  CommentTimeRange,
  TaskContentFilter,
} from "./collect-meta";
import { COMMENT_LIMIT_OPTIONS, COMMENT_TIME_RANGE_META } from "./collect-meta";
import { ContentDetailDialog } from "@/components/content-detail-dialog";
import { EmptyState } from "@/components/EmptyState";
import {
  FilterChip,
  FilterSidebar,
  IndustryFilterToggle,
  DateRangeFilter,
} from "@/components/library-filters";
import { ImageWaterfall } from "@/components/ImageWaterfall";
import { ContentCard } from "@/components/ContentCard";
import { MediaStatusBadge } from "@/components/MediaStatusBadge";

// 瀑布流每次加载的卡片数:与后端 offset 步进一致,避免一次性挂载海量图片
const GRID_PAGE_SIZE = 48;

// 表格列 id → 后端排序字段(白名单;本页仅「素材」列可排序)
const CONTENT_SORT_BY_MAP: Record<string, ContentListQuery["sortBy"]> = {
  media: "mediaStatus",
};

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
  // 服务端分页:contents 持有「当前已加载页/批」(表格=当前页,瀑布流=已 append 的各批)
  const [contents, setContents] = useState<ContentView[]>([]);
  const [total, setTotal] = useState(0);
  const [loading, setLoading] = useState(false);
  // 表格视图的服务端分页/排序状态
  const [serverState, setServerState] = useState<ServerTableState>({
    pageIndex: 0,
    pageSize: 50,
    sorting: [],
  });
  // 瀑布流已加载偏移(批起点;append 式,事件删行后不能从 contents.length 推导)
  const [gridOffset, setGridOffset] = useState(0);
  const [platforms, setPlatforms] = useState<PlatformConfig[]>([]);
  const [industries, setIndustries] = useState<IndustryView[]>([]);
  const [search, setSearch] = useState("");
  const [platformFilter, setPlatformFilter] = useState(""); // ""=全部
  const [kindSearch, setKindSearch] = useState<string[]>([]); // 全量库内容形态筛选;[]=全部
  const [industryFilter, setIndustryFilter] = useState("__all");
  const [createdRange, setCreatedRange] = useState<DateRange | undefined>();
  const [publishedRange, setPublishedRange] = useState<DateRange | undefined>();
  // 请求序号竞态守卫:筛选快速切换时,慢的旧响应不覆盖新响应
  const reqSeq = useRef(0);
  // 内容详情弹窗:当前打开的内容 id(null=关闭)
  const [detailId, setDetailId] = useState<string | null>(null);
  const [sidebarCollapsed, setSidebarCollapsed] = useResponsiveCollapse();
  // 正在重试素材下载的内容 id 集合(行级 loading,避免重复点击)
  const [retrying, setRetrying] = useState<Set<string>>(new Set());
  // 文案转写重试中的内容 id 集合(与素材重试分开,互不影响)
  const [retryingTranscript, setRetryingTranscript] = useState<Set<string>>(
    new Set(),
  );
  // 批量导出 Obsidian 进行中(防重复点击)
  const [batchSyncing, setBatchSyncing] = useState(false);
  // 批量导出 Excel 进行中(防重复点击)
  const [batchExporting, setBatchExporting] = useState(false);
  // 批量重试转写失败进行中(防重复点击)
  const [batchRetryingTranscripts, setBatchRetryingTranscripts] = useState(false);
  // 补采评论弹窗:目标 ids + 确认后清空表格选择的回调;null=未打开
  const [commentDialog, setCommentDialog] = useState<{
    ids: string[];
    reset: () => void;
  } | null>(null);
  // 补采评论参数(与新建任务表单评论采集项一致):时间范围 / 单视频上限 / 意向分析
  const [cmTimeRange, setCmTimeRange] = useState<CommentTimeRange>("any");
  const [cmLimit, setCmLimit] = useState("0");
  const [cmIntent, setCmIntent] = useState(false);
  // 补采评论进行中(防重复点击;逐视频开详情页采集,耗时较长)
  const [collectingComments, setCollectingComments] = useState(false);
  // 提取文案/评论进行中:列表只显示本批待处理条目,成功一条移除一条;null=未在提取
  const [processing, setProcessing] = useState<{
    kind: "transcript" | "comments";
    ids: Set<string>;
  } | null>(null);
  // 事件监听回调里读最新 processing(监听器只注册一次,不能闭包旧值)
  const processingRef = useRef<typeof processing>(null);
  useEffect(() => {
    processingRef.current = processing;
  }, [processing]);
  // 取消请求已发出、等后端在批间/下一条生效(防重复点击)
  const [cancelling, setCancelling] = useState(false);
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
  // 搜索词延迟值:输入即时回显,筛选/大列表重渲染延后到空闲帧,避免逐键卡顿
  const deferredSearch = useDeferredValue(search);

  const platformName = useCallback((id: string) =>
    platforms.find((p) => p.id === id)?.name ?? id, [platforms]);

  // 任务穿透时按任务拉取(服务端过滤,旧任务内容不被全量上限截断);开关/目标任务变化时重取
  const penetratedTaskId =
    taskFilter && taskFilterOn ? taskFilter.taskId : undefined;

  // 日期区间 → Unix 秒闭区间(与旧 inDateRange 的本地日 00:00:00 ~ 23:59:59.999 口径一致)
  const toDayStart = (d: Date) =>
    Math.floor(new Date(d).setHours(0, 0, 0, 0) / 1000);
  const toDayEnd = (d: Date) =>
    Math.floor(new Date(d).setHours(23, 59, 59, 999) / 1000);

  // 由当前筛选 + 分页/排序构造后端查询参数(统计 / 批量快照 / 导出复用同一口径)
  const buildQuery = useCallback(
    (opts: {
      sorting?: ServerTableState["sorting"];
      limit: number;
      offset: number;
    }): ContentListQuery => {
      const sort = opts.sorting?.[0];
      return {
        taskId: penetratedTaskId ?? null,
        keyword: penetratedTaskId ? (taskFilter?.keyword ?? null) : null,
        runStart: penetratedTaskId ? (taskFilter?.runStart ?? null) : null,
        runEnd: penetratedTaskId ? (taskFilter?.runEnd ?? null) : null,
        search: deferredSearch.trim() || null,
        platform: platformFilter || null,
        // kindFilter(库级形态)与 kindSearch(全量库内多选)UI 互斥
        kinds: kindFilter ? [kindFilter] : kindSearch,
        industry: industryFilter === "__all" ? null : industryFilter,
        createdFrom: createdRange?.from ? toDayStart(createdRange.from) : null,
        createdTo: createdRange?.from
          ? toDayEnd(createdRange.to ?? createdRange.from)
          : null,
        publishedFrom: publishedRange?.from
          ? toDayStart(publishedRange.from)
          : null,
        publishedTo: publishedRange?.from
          ? toDayEnd(publishedRange.to ?? publishedRange.from)
          : null,
        // 图源仅图片库传:「封面」要全形态 + 有封面;「图文」不加封面条件(旧 base 口径)
        imageSource: isImageLibrary && imageSource === "cover" ? "cover" : null,
        // 内容库(视频 tab)只展示已转写文案的视频
        requireTranscript: kindFilter === "video" || null,
        sortBy: sort ? (CONTENT_SORT_BY_MAP[sort.id] ?? null) : null,
        sortDir: sort ? (sort.desc ? "desc" : "asc") : null,
        limit: opts.limit,
        offset: opts.offset,
        // 提取文案/评论进行中:ids 模式不分页一次取回本批(「列表只剩本批」视图)
        ids: processing ? [...processing.ids] : null,
      };
    },
    [
      penetratedTaskId,
      taskFilter,
      deferredSearch,
      platformFilter,
      kindFilter,
      kindSearch,
      industryFilter,
      createdRange,
      publishedRange,
      isImageLibrary,
      imageSource,
      processing,
    ],
  );

  // 筛选/视图/库形态变化:表格回第一页、瀑布流回首批(offset 页在列表变化后会漂移)
  useEffect(() => {
    setServerState((s) => (s.pageIndex === 0 ? s : { ...s, pageIndex: 0 }));
    setGridOffset((o) => (o === 0 ? o : 0));
  }, [
    viewMode,
    deferredSearch,
    platformFilter,
    kindSearch,
    industryFilter,
    createdRange,
    publishedRange,
    kindFilter,
    imageSource,
    taskFilter,
    taskFilterOn,
  ]);

  // 表格视图:服务端分页替换式拉取
  useEffect(() => {
    if (supportsWaterfall && viewMode === "grid") return;
    const query = buildQuery({
      sorting: serverState.sorting,
      limit: serverState.pageSize,
      offset: serverState.pageIndex * serverState.pageSize,
    });
    const seq = ++reqSeq.current;
    setLoading(true);
    api
      .listContentsPage(query)
      .then((res) => {
        if (seq !== reqSeq.current) return;
        setContents(res.items);
        setTotal(res.total);
      })
      .catch((e) => {
        if (seq !== reqSeq.current) return;
        toast.error(`加载内容失败: ${e}`);
      })
      .finally(() => {
        if (seq === reqSeq.current) setLoading(false);
      });
  }, [viewMode, supportsWaterfall, serverState, buildQuery]);

  // 瀑布流视图:offset 步进 append(加载更多);offset=0 时替换为首屏
  useEffect(() => {
    if (!(supportsWaterfall && viewMode === "grid")) return;
    const query = buildQuery({ limit: GRID_PAGE_SIZE, offset: gridOffset });
    const seq = ++reqSeq.current;
    setLoading(true);
    api
      .listContentsPage(query)
      .then((res) => {
        if (seq !== reqSeq.current) return;
        setContents((prev) => {
          if (gridOffset === 0) return res.items;
          // 事件删行后 append 可能重叠,按 id 去重
          const seen = new Set(prev.map((c) => c.id));
          return [...prev, ...res.items.filter((c) => !seen.has(c.id))];
        });
        setTotal(res.total);
      })
      .catch((e) => {
        if (seq !== reqSeq.current) return;
        toast.error(`加载内容失败: ${e}`);
      })
      .finally(() => {
        if (seq === reqSeq.current) setLoading(false);
      });
  }, [viewMode, supportsWaterfall, gridOffset, buildQuery]);

  useEffect(() => {
    api.listPlatforms().then(setPlatforms).catch((e) => console.warn("加载平台列表失败:", e));
    api.listIndustries().then(setIndustries).catch((e) => console.warn("加载行业列表失败:", e));
  }, []);

  // 转写完成实时刷新:后端每写完一条 transcript 发事件,就地更新该行,
  // 「未转写 N 条」计数随批量/采集后转写进度实时递减(无需等整批结束或手动刷新)。
  // 批量转写期间事件很密:积攒 300ms 合帧一次 setContents——否则每条都整表重算
  // (筛选/行模型/50 张卡片重渲染),列表会明显卡顿。
  useEffect(() => {
    type Payload = {
      id: string;
      transcript: string | null;
      transcriptError: string | null;
    };
    const pending = new Map<string, Payload>();
    let timer: ReturnType<typeof setTimeout> | null = null;
    const flush = () => {
      timer = null;
      if (pending.size === 0) return;
      const batch = new Map(pending);
      pending.clear();
      const proc = processingRef.current;
      setContents((prev) =>
        prev.flatMap((x) => {
          const p = batch.get(x.id);
          if (!p) return [x];
          // 提取文案进行中且本条转写成功:从列表移除(「成功一条消失一条」的渐进效果);
          // 失败的保留在列表里,转写错误就地显示
          if (
            proc?.kind === "transcript" &&
            proc.ids.has(x.id) &&
            (p.transcript ?? "").trim()
          ) {
            return [];
          }
          return [
            { ...x, transcript: p.transcript, transcriptError: p.transcriptError },
          ];
        }),
      );
    };
    const unlisten = listen<Payload>("content-transcript-updated", (e) => {
      pending.set(e.payload.id, e.payload);
      if (!timer) timer = setTimeout(flush, 300);
    });
    return () => {
      if (timer) clearTimeout(timer);
      unlisten.then((f) => f());
    };
  }, []);

  // 评论补采成功实时移除:后端每采成功一条内容发事件,提取评论进行中
  // 对应行从列表消失(与转写同一套「成功一条移除一条」交互);失败的保留
  useEffect(() => {
    const unlisten = listen<{ id: string; comments: number }>(
      "content-comments-collected",
      (e) => {
        const proc = processingRef.current;
        if (proc?.kind !== "comments" || !proc.ids.has(e.payload.id)) return;
        setContents((prev) => prev.filter((x) => x.id !== e.payload.id));
      },
    );
    return () => {
      unlisten.then((f) => f());
    };
  }, []);

  // 平台筛选列出全部平台(与行业侧栏一致),不只展示已有数据的平台
  const platformOptions = useMemo(() => platforms.map((p) => p.id), [platforms]);

  // 各行业内容数(侧栏角标):走后端聚合,跟随当前筛选(除行业自身——与列表口径一致)。
  // 「全部」角标用列表 total,渲染时合并传入
  const [industryCounts, setIndustryCounts] = useState<Record<string, number>>({});
  const countsSeq = useRef(0);
  useEffect(() => {
    const query = buildQuery({ limit: 1, offset: 0 });
    const seq = ++countsSeq.current;
    api
      .contentIndustryCounts(query)
      .then((list) => {
        if (seq !== countsSeq.current) return; // 过期响应丢弃
        const map: Record<string, number> = {};
        for (const it of list) map[it.industry] = it.count;
        setIndustryCounts(map);
      })
      .catch((e) => console.warn("加载行业角标失败:", e));
  }, [buildQuery]);

  // 待转写 / 待提取评论计数(按钮显示 + 批量目标判定):走后端统计,与当前筛选口径一致。
  // buildQuery 含 processing,批量进行中(ids 模式)统计即本批剩余
  const [libStats, setLibStats] = useState<ContentLibraryStats>({
    untranscribed: 0,
    pendingComment: 0,
  });
  const statsSeq = useRef(0);
  useEffect(() => {
    const query = buildQuery({ limit: 1, offset: 0 });
    const seq = ++statsSeq.current;
    api
      .contentLibraryStats(query)
      .then((s) => {
        if (seq !== statsSeq.current) return;
        setLibStats(s);
      })
      .catch((e) => console.warn("加载处理状态统计失败:", e));
  }, [buildQuery]);

  // 提取进行中剩余待处理条数:成功条目已从 contents 移除,直接数这批还剩多少
  const processingRemaining = useMemo(
    () =>
      processing
        ? contents.filter((c) => processing.ids.has(c.id)).length
        : 0,
    [processing, contents],
  );

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

  // 删除一条内容:与批量删除共用红色确认弹窗(删除不可恢复,单条也必须确认)。
  // useCallback 稳定引用:作为 prop 传给 memo 的瀑布流卡片,避免父组件重渲染时整列跟着重渲染
  const handleDelete = useCallback((id: string) => {
    setPendingDelete({ ids: [id], reset: () => {} });
  }, []);

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

  // 批量导出 Excel:仅导出选中里「有文案」的内容;路径经系统保存对话框选定
  async function handleBatchExportExcel(selected: ContentView[]) {
    if (batchExporting) return;
    const withTranscript = selected.filter((c) => c.transcript?.trim());
    if (withTranscript.length === 0) {
      toast.error("所选内容均无文案,没有可导出的数据");
      return;
    }
    setBatchExporting(true);
    try {
      const rows = withTranscript.map((c) => ({
        平台: platformName(c.platform),
        行业: c.industry,
        // 抖音等平台无独立标题(正文在 desc):标题列回退用 desc,简介列此时留空不重复
        标题: c.title ?? c.desc ?? "",
        简介: c.title ? (c.desc ?? "") : "",
        作者: c.authorNickname,
        点赞数: c.likeCount ?? 0,
        评论数: c.commentCount ?? 0,
        收藏数: c.collectCount ?? 0,
        分享数: c.shareCount ?? 0,
        采集关键词: c.keyword,
        文案: c.transcript ?? "",
        内容链接: contentDetailUrl(c.platform, c.contentId) ?? "",
        发布时间: formatTimestamp(c.publishedAt),
        采集时间: formatTimestamp(c.collectedAt),
      }));
      const ws = XLSX.utils.json_to_sheet(rows);
      // 表头样式:居中 + 靛蓝背景 + 加粗白字
      const headerStyle = {
        font: { bold: true, color: { rgb: "FFFFFF" } },
        fill: { fgColor: { rgb: "4F46E5" } },
        alignment: { horizontal: "center" as const, vertical: "center" as const },
      };
      if (ws["!ref"]) {
        const range = XLSX.utils.decode_range(ws["!ref"]);
        for (let col = range.s.c; col <= range.e.c; col++) {
          const addr = XLSX.utils.encode_cell({ r: 0, c: col });
          const cell = ws[addr];
          if (cell) (cell as Record<string, unknown>).s = headerStyle;
        }
      }
      // 列宽(字符数),与导出字段顺序对应
      ws["!cols"] = [
        { wch: 8 }, // 平台
        { wch: 12 }, // 行业
        { wch: 30 }, // 标题
        { wch: 36 }, // 简介
        { wch: 16 }, // 作者
        { wch: 8 }, // 点赞数
        { wch: 8 }, // 评论数
        { wch: 8 }, // 收藏数
        { wch: 8 }, // 分享数
        { wch: 18 }, // 采集关键词
        { wch: 60 }, // 文案
        { wch: 46 }, // 内容链接
        { wch: 20 }, // 发布时间
        { wch: 20 }, // 采集时间
      ];
      const wb = XLSX.utils.book_new();
      XLSX.utils.book_append_sheet(wb, ws, "文案内容");
      const base64 = XLSX.write(wb, { type: "base64", bookType: "xlsx" });
      const now = new Date();
      const ymd = `${now.getFullYear()}${String(now.getMonth() + 1).padStart(2, "0")}${String(now.getDate()).padStart(2, "0")}`;
      // 当天导出流水号:每天从 001 起递增,导出成功才消耗(取消保存不计)
      const SEQ_KEY = "veltrix.content-export-seq";
      let prevSeq: { date: string; seq: number } = { date: "", seq: 0 };
      try {
        const raw = localStorage.getItem(SEQ_KEY);
        if (raw) prevSeq = JSON.parse(raw);
      } catch {
        // 本地记录损坏则从头计
      }
      const seq = prevSeq.date === ymd ? prevSeq.seq + 1 : 1;
      const fileName = `文案内容-${ymd}-${String(seq).padStart(3, "0")}.xlsx`;
      const path = await save({
        defaultPath: fileName,
        filters: [{ name: "Excel 工作簿", extensions: ["xlsx"] }],
      });
      if (!path) return; // 用户取消保存
      await api.saveBinaryFile(path, base64);
      recordDownload({ path, name: fileName, kind: "内容导出" });
      localStorage.setItem(SEQ_KEY, JSON.stringify({ date: ymd, seq }));
      // 部分选中无文案被跳过时在结果里说明
      const skipped = selected.length - withTranscript.length;
      toast.success(
        skipped > 0
          ? `已导出 ${withTranscript.length} 条(跳过 ${skipped} 条无文案)`
          : `已导出 ${withTranscript.length} 条`,
      );
    } catch (e) {
      toast.error(`导出失败:${e instanceof Error ? e.message : String(e)}`);
    } finally {
      setBatchExporting(false);
    }
  }

  // 重新拉取素材:重跑下载并就地刷新该行状态。
  // 视频直链可能已过期(403),重试不一定成功——失败时提示需重新采集。
  // useCallback 稳定引用:作为 prop 传给 memo 的瀑布流卡片;仅重试集合变化时重建
  const handleRetry = useCallback(async (c: ContentView) => {
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
                transcript: res.transcript,
                transcriptError: res.transcriptError,
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
  }, [retrying]);

  // 重新转写文案:仅重跑语音转写(素材/音频不动),就地刷新该行文案状态
  async function handleRetryTranscript(c: ContentView) {
    if (retryingTranscript.has(c.id)) return;
    setRetryingTranscript((prev) => new Set(prev).add(c.id));
    try {
      const res = await api.retryContentTranscript(c.id);
      setContents((prev) =>
        prev.map((x) =>
          x.id === res.id
            ? { ...x, transcript: res.transcript, transcriptError: res.transcriptError }
            : x,
        ),
      );
      if (res.transcript) {
        toast.success("文案已重新转写");
      } else {
        toast.error(
          `转写仍失败${res.transcriptError ? `: ${res.transcriptError}` : ""}`,
        );
      }
    } catch (e) {
      toast.error(`转写重试失败: ${e}`);
    } finally {
      setRetryingTranscript((prev) => {
        const next = new Set(prev);
        next.delete(c.id);
        return next;
      });
    }
  }

  // 取消批量提取:登记后端停止标记;转写在批间、评论在「下一个视频」前生效,
  // 运行中的 invoke 收尾后 handler 的 finally 会退出处理视图并刷新列表
  async function handleCancelProcessing() {
    if (cancelling) return;
    setCancelling(true);
    try {
      await api.cancelLibraryExtract();
      toast.info("已请求取消 · 当前条目完成后停止");
    } catch (e) {
      toast.error(`取消失败: ${e}`);
      setCancelling(false);
    }
  }

  // 批量转写:后端对当前筛选中「有音频无文案」的条目按任务分组重跑语音转写
  // (并发遵循系统设置「语音转写」)。点击后列表先筛出本批,转写成功一条移除一条;
  // 完成后清 processing(列表自动回到当前页视图),成功数用服务端剩余待转写统计倒推
  async function handleBatchRetryTranscripts() {
    if (batchRetryingTranscripts || libStats.untranscribed === 0) return;
    setBatchRetryingTranscripts(true);
    try {
      // 点击瞬间按当前筛选口径取目标快照(后续成功移除不改变集合)
      const ids = await api.listBatchContentIds(
        buildQuery({ limit: 1, offset: 0 }),
        "transcript",
      );
      if (ids.length === 0) return;
      setProcessing({ kind: "transcript", ids: new Set(ids) });
      const retried = await api.retryFailedTranscripts(ids);
      // 不再整表重拉:本批剩余 = 同口径服务端待转写数(等价旧「重拉后过滤本批」)
      const stats = await api.contentLibraryStats(
        buildQuery({ limit: 1, offset: 0 }),
      );
      const remain = stats.untranscribed;
      const ok = Math.max(0, retried - remain);
      toast.success(
        remain > 0
          ? `批量转写完成 · 共处理 ${retried} 条,成功 ${ok} 条,仍有 ${remain} 条未转写`
          : `批量转写完成 · ${retried} 条全部成功`,
      );
    } catch (e) {
      toast.error(`批量转写失败: ${e}`);
    } finally {
      setProcessing(null);
      setCancelling(false);
      setBatchRetryingTranscripts(false);
    }
  }

  // 补采评论:对选中内容按弹窗参数重采一级评论(后端逐视频开详情页采集,耗时较长)。
  // 点击后先关弹窗、列表筛出本批,采成功一条移除一条;完成后退出筛选(列表自动回当前页视图)。
  // 跳过/失败明细量多,toast 只给汇总,逐条打控制台
  async function handleRecollectComments() {
    if (!commentDialog || collectingComments) return;
    setCollectingComments(true);
    const { ids, reset } = commentDialog;
    // 立即关弹窗并清空选择:列表进入「仅本批」视图,成功条目会渐进消失
    setCommentDialog(null);
    reset();
    setProcessing({ kind: "comments", ids: new Set(ids) });
    const toastId = toast.loading(
      `正在提取 ${ids.length} 条内容的评论 · 会逐个打开详情页,请稍候…`,
    );
    try {
      const s = await api.recollectComments(ids, {
        commentTimeRange: cmTimeRange,
        commentLimit: Number(cmLimit) || 0,
        analyzeIntent: cmIntent,
      });
      toast.success(
        `提取评论完成 · 成功 ${s.succeeded} 条内容 / 入库评论 ${s.comments} 条` +
          (s.failed > 0 ? ` · 失败 ${s.failed}` : "") +
          (s.skipped > 0 ? ` · 跳过 ${s.skipped}` : ""),
        { id: toastId },
      );
      if (s.messages.length) console.warn("提取评论明细:", s.messages);
    } catch (e) {
      toast.error(`提取评论失败: ${e}`, { id: toastId });
    } finally {
      setProcessing(null);
      setCancelling(false);
      setCollectingComments(false);
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
            retryingTranscript={retryingTranscript.has(row.original.id)}
            onRetryTranscript={() => handleRetryTranscript(row.original)}
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
                {/* 文案未转写(含当时缺 API Key 被跳过的)或转写失败,且有音频:只重跑语音转写 */}
                {!c.transcript && c.audioPath && (
                  <DropdownMenuItem
                    disabled={retryingTranscript.has(c.id)}
                    onClick={() => handleRetryTranscript(c)}
                  >
                    <RefreshCw className="size-4" />
                    {c.transcriptError ? "重新转写文案" : "转写文案"}
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
    // 依赖 retrying/retryingTranscript:行级重试态变化时重建列,徽章 loading / 禁用态才能刷新
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [platforms, platformName, retrying, retryingTranscript],
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
              · 共 {total} 条
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
            industryCounts={{ ...industryCounts, __all: total }}
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
            {/* 右侧操作区:视图切换(图片库/内容库),整体靠右;提取评论/文案按钮在下方平台行右侧 */}
            <div className="ml-auto flex items-center gap-2">
              {supportsWaterfall && (
                <div className="inline-flex h-10 items-center rounded-md border p-0.5">
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
            {/* 全量库批量提取入口:与平台行同排靠右(不与上方筛选行对齐)。
                提取评论:待提计数 + 弹窗设评论参数,无选择上下文 reset 传空操作 */}
            {!kindFilter && libStats.pendingComment > 0 && (
              <Button
                variant="outline"
                className="ml-auto h-7 cursor-pointer border-violet-500/40 px-3 text-xs text-violet-600 hover:bg-violet-500/10 dark:text-violet-400"
                disabled={collectingComments}
                onClick={async () => {
                  // 点击瞬间按当前筛选口径取目标快照(与「提取文案」一致)
                  const ids = await api.listBatchContentIds(
                    buildQuery({ limit: 1, offset: 0 }),
                    "comments",
                  );
                  if (ids.length === 0) {
                    toast.info("当前没有待提取评论的内容");
                    return;
                  }
                  setCommentDialog({ ids, reset: () => {} });
                }}
              >
                {collectingComments ? (
                  <Loader2 className="animate-spin" />
                ) : (
                  <MessagesSquare />
                )}
                提取评论 · {libStats.pendingComment} 条
              </Button>
            )}
            {/* 提取文案:待提(有音频无文案)计数 + 一键批量提取;
                评论按钮不在(已提完)时用 ml-auto 保持靠右 */}
            {!kindFilter && libStats.untranscribed > 0 && (
              <Button
                variant="outline"
                className={`h-7 cursor-pointer border-amber-500/40 px-3 text-xs text-amber-600 hover:bg-amber-500/10 dark:text-amber-400 ${
                  libStats.pendingComment > 0 ? "" : "ml-auto"
                }`}
                disabled={batchRetryingTranscripts}
                onClick={handleBatchRetryTranscripts}
              >
                {batchRetryingTranscripts ? (
                  <Loader2 className="animate-spin" />
                ) : (
                  <RefreshCw />
                )}
                提取文案 · {libStats.untranscribed} 条
              </Button>
            )}
          </div>

          {/* 提取文案/评论进行中:仅显示本批,成功一条移除一条;提示条给剩余计数 + 取消 */}
          {processing && (
            <div className="flex items-center gap-2 rounded-md border border-primary/30 bg-primary/5 px-3 py-1.5 text-xs">
              <Loader2 className="size-3.5 animate-spin text-primary" />
              <span>
                {processing.kind === "transcript"
                  ? "正在提取文案"
                  : "正在提取评论"}{" "}
                · 剩余{" "}
                <span className="font-mono font-medium">
                  {processingRemaining}
                </span>{" "}
                条(成功一条自动移除一条)
              </span>
              {/* 取消:转写在批间、评论在「下一个视频」前生效,已完成条目保留 */}
              <Button
                variant="ghost"
                size="sm"
                className="ml-auto h-6 cursor-pointer px-2 text-xs"
                disabled={cancelling}
                onClick={handleCancelProcessing}
              >
                {cancelling ? <Loader2 className="animate-spin" /> : <X />}
                {cancelling ? "取消中…" : "取消"}
              </Button>
            </div>
          )}

          {supportsWaterfall && viewMode === "grid" ? (
            <ImageWaterfall
              items={contents}
              total={total}
              onLoadMore={() => setGridOffset((o) => o + GRID_PAGE_SIZE)}
              platformName={platformName}
              retrying={retrying}
              onOpenDetail={setDetailId}
              onRetry={handleRetry}
              onDelete={handleDelete}
            />
          ) : (
            <DataTable
              columns={columns}
              data={contents}
              itemLabel="内容"
              getRowId={(c) => c.id}
              defaultPageSize={50}
              pageSizeOptions={[50, 100, 200, 500, 1000]}
              serverControl={{
                total,
                state: serverState,
                onStateChange: setServerState,
                loading,
              }}
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
                    {/* 导出 Excel:仅导出有文案的;无文案的在结果提示里说明跳过数 */}
                    <Button
                      variant="outline"
                      size="sm"
                      className="cursor-pointer"
                      disabled={batchExporting}
                      onClick={() =>
                        handleBatchExportExcel(
                          table
                            .getSelectedRowModel()
                            .rows.map((r) => r.original),
                        )
                      }
                    >
                      {batchExporting ? (
                        <Loader2 className="animate-spin" />
                      ) : (
                        <FileSpreadsheet />
                      )}
                      导出 Excel
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
        items={contents}
        activeId={detailId}
        onActiveIdChange={setDetailId}
      />
      {/* 提取评论参数弹窗:三项与新建任务表单的评论采集项一致(评论时间 / 单视频上限 / 意图分析);
          仅顶部「提取评论」一个入口(当前筛选待提批) */}
      <Dialog
        open={!!commentDialog}
        onOpenChange={(o) => {
          if (!o && !collectingComments) setCommentDialog(null);
        }}
      >
        <DialogContent>
          <DialogHeader>
            <DialogTitle>
              提取评论 · 共 {commentDialog?.ids.length ?? 0} 条内容
            </DialogTitle>
            <DialogDescription>
              按下列参数采集这批内容的一级评论;评论数为 0 的内容会自动跳过,已采过的评论按评论
              ID 去重入库。
            </DialogDescription>
          </DialogHeader>
          <div className="grid grid-cols-3 gap-3">
            <div className="space-y-1.5">
              <Label htmlFor="recollect-comment-time">评论时间</Label>
              <Select
                value={cmTimeRange}
                onValueChange={(v) => setCmTimeRange(v as CommentTimeRange)}
              >
                <SelectTrigger id="recollect-comment-time" className="w-full">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  {(Object.keys(COMMENT_TIME_RANGE_META) as CommentTimeRange[]).map(
                    (k) => (
                      <SelectItem key={k} value={k}>
                        {COMMENT_TIME_RANGE_META[k].label}
                      </SelectItem>
                    ),
                  )}
                </SelectContent>
              </Select>
            </div>
            <div className="space-y-1.5">
              <Label htmlFor="recollect-comment-limit">单视频上限</Label>
              <Select value={cmLimit} onValueChange={setCmLimit}>
                <SelectTrigger id="recollect-comment-limit" className="w-full">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  {COMMENT_LIMIT_OPTIONS.map((o) => (
                    <SelectItem key={o.value} value={o.value}>
                      {o.label}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
            </div>
            <div className="space-y-1.5">
              <Label htmlFor="recollect-comment-intent">评论意图分析</Label>
              <Select
                value={cmIntent ? "1" : "0"}
                onValueChange={(v) => setCmIntent(v === "1")}
              >
                <SelectTrigger id="recollect-comment-intent" className="w-full">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="0">否</SelectItem>
                  <SelectItem value="1">是</SelectItem>
                </SelectContent>
              </Select>
            </div>
          </div>
          <DialogFooter>
            <Button
              variant="outline"
              className="cursor-pointer"
              disabled={collectingComments}
              onClick={() => setCommentDialog(null)}
            >
              取消
            </Button>
            <Button
              className="cursor-pointer"
              disabled={collectingComments}
              onClick={handleRecollectComments}
            >
              {collectingComments && <Loader2 className="animate-spin" />}
              开始提取
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
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
