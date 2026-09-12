// 评论库:展示采集落库的评论(comments 表)+ AI 意向标记。
// 筛选:左侧栏(行业 + 角标)+ 顶部(意向 / 平台 chip + 评论日期 + 关键字)。
// 视图:瀑布流(默认;按评论来源聚合卡片,卡片内 6 条,更多开右侧抽屉;
// 虚拟化分栏 + 滚动 append,与图片库瀑布流同加载方式)/ 表格(评论行)。
// 数据走后端分页:筛选/排序下沉 SQL;表格持当前页,瀑布流持已 append 的各批。
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { type ColumnDef } from "@tanstack/react-table";
import { Download, Heart, LayoutGrid, List, MessageCircle, Search, X } from "lucide-react";
import { type DateRange } from "react-day-picker";
import { toast } from "sonner";

import { DataTable, type ServerTableState } from "@/components/DataTable";
import { DataTableColumnHeader } from "@/components/DataTableColumnHeader";
import { FacetedFilter } from "@/components/FacetedFilter";
import { FORM_CONTROL_SIZING } from "@/lib/form-sizing";
import {
  DateRangeFilter,
  FilterChip,
  FilterSidebar,
  IndustryFilterToggle,
} from "@/components/library-filters";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { SimpleTooltip } from "@/components/SimpleTooltip";
import { useResponsiveCollapse } from "@/hooks/use-responsive-collapse";
import { useDebouncedValue } from "@/hooks/use-debounced-value";
import {
  api,
  type CommentListQuery,
  type CommentSourceGroup,
  type CommentView,
  type IndustryView,
  type PlatformConfig,
} from "@/lib/api";
import { formatTimestamp, formatDateTime } from "@/lib/utils";
import {
  platformClass,
  platformChipClass,
  contentDetailUrl,
  authorProfileUrl,
} from "@/lib/platforms";
import { useMediaFileUrl, mediaThumbPath } from "@/lib/media-file-url";
import { EmptyState } from "@/components/EmptyState";
import { CommentWaterfall } from "@/components/comment-waterfall";
import { save } from "@tauri-apps/plugin-dialog";
import { recordDownload } from "@/lib/download-history";

// 意向等级元数据(高=红、中=琥珀、低=灰、无=静默)
type IntentLevel = "high" | "medium" | "low" | "none";
const INTENT_META: Record<IntentLevel, { label: string; className: string }> = {
  high: {
    label: "高意向",
    className: "border-red-500/30 bg-red-500/10 text-red-600 dark:text-red-400",
  },
  medium: {
    label: "中意向",
    className:
      "border-amber-500/30 bg-amber-500/10 text-amber-600 dark:text-amber-400",
  },
  low: {
    label: "低意向",
    className:
      "border-slate-500/30 bg-slate-500/10 text-slate-600 dark:text-slate-400",
  },
  none: {
    label: "无意向",
    className: "border-border bg-muted text-muted-foreground",
  },
};

// 内容形态筛选项(评论所属内容的 kind)
const KIND_FILTERS: { value: string; label: string }[] = [
  { value: "video", label: "视频" },
  { value: "image", label: "图文" },
  { value: "article", label: "文章" },
];

// 意向筛选项:all=全部,unanalyzed=尚未分析(intentLevel 为 null);
// 每种意向独立配色(选中实色、未选同色系描边文字),与表格意向徽章色系一致
const INTENT_FILTERS: {
  value: string;
  label: string;
  chipActive: string;
  chipInactive: string;
}[] = [
  {
    value: "high",
    label: "高意向",
    chipActive: "border-red-500 bg-red-500 text-white",
    chipInactive:
      "border-red-500/40 text-red-600 hover:bg-red-500/10 dark:text-red-400",
  },
  {
    value: "medium",
    label: "中意向",
    chipActive: "border-amber-500 bg-amber-500 text-white",
    chipInactive:
      "border-amber-500/40 text-amber-600 hover:bg-amber-500/10 dark:text-amber-400",
  },
  {
    value: "low",
    label: "低意向",
    chipActive: "border-slate-500 bg-slate-500 text-white",
    chipInactive:
      "border-slate-500/40 text-slate-600 hover:bg-slate-500/10 dark:text-slate-400",
  },
  {
    value: "none",
    label: "无意向",
    chipActive: "border-zinc-400 bg-zinc-400 text-white",
    chipInactive:
      "border-zinc-400/50 text-zinc-500 hover:bg-zinc-400/10 dark:text-zinc-400",
  },
  {
    value: "unanalyzed",
    label: "未分析",
    chipActive: "border-violet-500 bg-violet-500 text-white",
    chipInactive:
      "border-violet-500/40 text-violet-600 hover:bg-violet-500/10 dark:text-violet-400",
  },
];

function formatCount(n?: number | null): string {
  if (n == null) return "0";
  if (n >= 10000) return `${(n / 10000).toFixed(1)}万`;
  return String(n);
}

// 瀑布流每批加载的来源数(滚动到底部哨兵 append 一批)
const GRID_PAGE_SIZE = 12;

// 表格列 id → 后端排序字段(白名单;其余列不可排序)
const SORT_BY_MAP: Record<string, CommentListQuery["sortBy"]> = {
  intent: "intent",
  likes: "likeCount",
  createdAt: "createdAt",
  collectedAt: "collectedAt",
};

// 日期区间 → Unix 秒闭区间(与旧 inDateRange 的本地日 00:00:00 ~ 23:59:59.999 口径一致)
const toDayStart = (d: Date) =>
  Math.floor(new Date(d).setHours(0, 0, 0, 0) / 1000);
const toDayEnd = (d: Date) =>
  Math.floor(new Date(d).setHours(23, 59, 59, 999) / 1000);

export function CommentLibraryPage() {
  const mediaFileUrl = useMediaFileUrl();
  // 当前页数据 + 总数 + 取数中(表格=评论行当前页;瀑布流=已 append 的来源批次)
  const [comments, setComments] = useState<CommentView[]>([]);
  const [total, setTotal] = useState(0);
  const [loading, setLoading] = useState(true);
  const [serverState, setServerState] = useState<ServerTableState>({
    pageIndex: 0,
    pageSize: 20,
    sorting: [{ id: "collectedAt", desc: true }],
  });
  const [platforms, setPlatforms] = useState<PlatformConfig[]>([]);
  const [industries, setIndustries] = useState<IndustryView[]>([]);
  const [search, setSearch] = useState("");
  const [platformFilter, setPlatformFilter] = useState(""); // ""=全部
  const [intentFilter, setIntentFilter] = useState<string[]>([]);
  const [industryFilter, setIndustryFilter] = useState("__all");
  const [commentRange, setCommentRange] = useState<DateRange | undefined>();
  const [kindFilter, setKindFilter] = useState<string[]>([]); // []=全部形态
  const [sidebarCollapsed, setSidebarCollapsed] = useResponsiveCollapse();
  // 视图:瀑布流(默认)/ 表格。瀑布流数据后端按来源分组(list_comment_sources_page,
  // 每组 6 条预览),offset 步进 append(与图片库瀑布流同加载方式);表格 = 评论行(20/页)
  const [viewMode, setViewMode] = useState<"table" | "waterfall">("waterfall");
  // 瀑布流分组数据(仅瀑布流视图使用;append 累积全部已加载批次)
  const [groups, setGroups] = useState<CommentSourceGroup[]>([]);
  const [groupTotal, setGroupTotal] = useState(0);
  // 瀑布流已加载偏移(批起点;append 式,不能从 groups.length 推导——去重会少计)
  const [gridOffset, setGridOffset] = useState(0);
  const switchView = (mode: "table" | "waterfall") => setViewMode(mode);
  // 输入即时回显,用户停顿后才触发列表与行业角标查询。
  const debouncedSearch = useDebouncedValue(search, 300);
  // 请求序号竞态守卫:筛选快速切换时,慢的旧响应不覆盖新响应
  const reqSeq = useRef(0);

  // 由当前筛选 + 分页/排序构造后端查询参数(导出翻页复用同一口径)
  const buildQuery = useCallback(
    (opts: {
      sorting?: ServerTableState["sorting"];
      limit: number;
      offset: number;
    }): CommentListQuery => {
      const sort = opts.sorting?.[0];
      return {
        search: debouncedSearch.trim() || null,
        platform: platformFilter || null,
        kinds: kindFilter,
        industry: industryFilter === "__all" ? null : industryFilter,
        intentLevels: intentFilter as CommentListQuery["intentLevels"],
        createdFrom: commentRange?.from ? toDayStart(commentRange.from) : null,
        createdTo: commentRange?.from
          ? toDayEnd(commentRange.to ?? commentRange.from)
          : null,
        sortBy: sort ? (SORT_BY_MAP[sort.id] ?? null) : null,
        sortDir: sort ? (sort.desc ? "desc" : "asc") : null,
        limit: opts.limit,
        offset: opts.offset,
      };
    },
    [debouncedSearch, platformFilter, kindFilter, industryFilter, intentFilter, commentRange],
  );

  useEffect(() => {
    api.listPlatforms().then(setPlatforms).catch((e) => console.warn("加载平台列表失败:", e));
    api.listIndustries().then(setIndustries).catch((e) => console.warn("加载行业列表失败:", e));
  }, []);

  // 筛选/视图变化:表格回第一页、瀑布流回首批(offset 页在列表变化后会漂移)
  useEffect(() => {
    setServerState((s) => (s.pageIndex === 0 ? s : { ...s, pageIndex: 0 }));
    setGridOffset((o) => (o === 0 ? o : 0));
  }, [viewMode, debouncedSearch, platformFilter, kindFilter, industryFilter, intentFilter, commentRange]);

  // IntersectionObserver 可能在重渲染时重复触发;稳定回调配合 loading 守卫避免跳批
  const loadMoreGrid = useCallback(() => {
    setGridOffset((offset) => offset + GRID_PAGE_SIZE);
  }, []);

  // 表格视图:服务端分页替换式拉取(评论行)
  useEffect(() => {
    if (viewMode !== "table") return;
    const query = buildQuery({
      sorting: serverState.sorting,
      limit: serverState.pageSize,
      offset: serverState.pageIndex * serverState.pageSize,
    });
    const seq = ++reqSeq.current;
    setLoading(true);
    api
      .listCommentsPage(query)
      .then((res) => {
        if (seq !== reqSeq.current) return; // 已有更新的请求发出,丢弃本次
        setComments(res.items);
        setTotal(res.total);
      })
      .catch((e) => {
        if (seq !== reqSeq.current) return;
        toast.error(`加载评论失败: ${e}`);
      })
      .finally(() => {
        if (seq === reqSeq.current) setLoading(false);
      });
  }, [buildQuery, serverState, viewMode]);

  // 瀑布流视图:offset 步进 append(加载更多);offset=0 时替换为首屏。
  // 后端按来源分组并截好每组 6 条预览,前端不再分组/截断。
  useEffect(() => {
    if (viewMode !== "waterfall") return;
    const query = buildQuery({ limit: GRID_PAGE_SIZE, offset: gridOffset });
    const seq = ++reqSeq.current;
    setLoading(true);
    api
      .listCommentSourcesPage(query, 6)
      .then((res) => {
        if (seq !== reqSeq.current) return;
        setGroups((prev) => {
          if (gridOffset === 0) return res.items;
          // append 可能因数据变动与已加载批次重叠,按来源键去重
          const seen = new Set(prev.map((g) => `${g.platform}-${g.contentId}`));
          return [
            ...prev,
            ...res.items.filter((g) => !seen.has(`${g.platform}-${g.contentId}`)),
          ];
        });
        setGroupTotal(res.total);
      })
      .catch((e) => {
        if (seq !== reqSeq.current) return;
        toast.error(`加载评论失败: ${e}`);
      })
      .finally(() => {
        if (seq === reqSeq.current) setLoading(false);
      });
  }, [buildQuery, gridOffset, viewMode]);

  const platformNames = useMemo(
    () => new Map(platforms.map((p) => [p.id, p.name])),
    [platforms],
  );
  const platformName = useCallback(
    (id: string) => platformNames.get(id) ?? id,
    [platformNames],
  );

  // 各行业评论数(侧栏角标):走后端聚合,跟随当前筛选(除行业自身——与列表口径一致)。
  // 「全部」角标用后端返回的 industryTotal(忽略行业筛选),不能复用列表 total——
  // 列表 total 含行业过滤,选中某行业后「全部」会被错误显示成该行业的数量
  const [industryCounts, setIndustryCounts] = useState<Record<string, number>>({});
  const [industryTotal, setIndustryTotal] = useState(0);
  const countsSeq = useRef(0);
  useEffect(() => {
    const query = buildQuery({ limit: 1, offset: 0 });
    const seq = ++countsSeq.current;
    api
      .commentIndustryCounts(query)
      .then((res) => {
        if (seq !== countsSeq.current) return; // 过期响应丢弃
        const map: Record<string, number> = {};
        for (const it of res.industries) map[it.industry] = it.count;
        setIndustryCounts(map);
        setIndustryTotal(res.total);
      })
      .catch((e) => console.warn("加载行业角标失败:", e));
  }, [buildQuery]);

  const hasFilter =
    platformFilter !== "" ||
    kindFilter.length > 0 ||
    industryFilter !== "__all" ||
    intentFilter.length > 0 ||
    commentRange?.from != null ||
    search !== "";

  const resetFilters = () => {
    setPlatformFilter("");
    setKindFilter([]);
    setIndustryFilter("__all");
    setIntentFilter([]);
    setCommentRange(undefined);
    setSearch("");
  };

  const columns: ColumnDef<CommentView>[] = useMemo(
    () => [
      {
        id: "author",
        accessorKey: "authorNickname",
        header: "评论者",
        enableSorting: false,
        cell: ({ row }) => {
          const c = row.original;
          return (
            <div className="flex w-44 items-center gap-2">
              {c.authorAvatar ? (
                <img
                  src={c.authorAvatar}
                  referrerPolicy="no-referrer"
                  onError={(e) => {
                    e.currentTarget.style.display = "none";
                  }}
                  className="size-8 shrink-0 rounded-full object-cover"
                  alt=""
                />
              ) : (
                <div className="flex size-8 shrink-0 items-center justify-center rounded-full bg-muted text-xs text-muted-foreground">
                  {(c.authorNickname || "?").slice(0, 1)}
                </div>
              )}
              <div className="min-w-0">
                <div className="truncate text-sm text-foreground">
                  {c.authorNickname || "—"}
                </div>
                {c.authorUniqueId && (
                  <div className="truncate text-xs text-muted-foreground">
                    @{c.authorUniqueId}
                  </div>
                )}
              </div>
            </div>
          );
        },
      },
      {
        id: "text",
        accessorKey: "text",
        header: "评论内容",
        enableSorting: false,
        cell: ({ row }) => (
          <span className="block max-w-md truncate text-foreground">
            {row.original.text}
          </span>
        ),
      },
      {
        id: "content",
        header: "所属内容",
        enableSorting: false,
        cell: ({ row }) => {
          const c = row.original;
          if (
            !c.contentTitle &&
            !c.contentCoverUrl &&
            !c.contentAuthorNickname
          ) {
            return <span className="text-xs text-muted-foreground">—</span>;
          }
          // 列表小图用缩略图(缺失时文件服务惰性生成),原图只留给详情大图
          const cover = c.contentCoverPath
            ? mediaFileUrl(mediaThumbPath(c.contentCoverPath))
            : c.contentCoverUrl || "";
          const kindLabel =
            c.contentKind === "video"
              ? "视频"
              : c.contentKind === "image"
                ? "图文"
                : c.contentKind === "article"
                  ? "文章"
                  : "";
          return (
            <div className="flex w-56 items-center gap-2">
              {cover ? (
                <img
                  src={cover}
                  referrerPolicy="no-referrer"
                  onError={(e) => {
                    e.currentTarget.style.display = "none";
                  }}
                  className="h-12 w-9 shrink-0 rounded object-cover"
                  alt=""
                />
              ) : (
                <div className="flex h-12 w-9 shrink-0 items-center justify-center rounded bg-muted text-[10px] text-muted-foreground">
                  无图
                </div>
              )}
              <div className="min-w-0">
                <div className="flex items-center gap-1">
                  {kindLabel && (
                    <span className="shrink-0 rounded bg-muted px-1 text-[10px] text-muted-foreground">
                      {kindLabel}
                    </span>
                  )}
                  <span className="truncate text-xs text-foreground">
                    {c.contentTitle || "(无标题)"}
                  </span>
                </div>
                {c.contentAuthorNickname && (
                  <div className="mt-0.5 flex items-center gap-1 text-xs text-muted-foreground">
                    {c.contentAuthorAvatar && (
                      <img
                        src={c.contentAuthorAvatar}
                        referrerPolicy="no-referrer"
                        onError={(e) => {
                          e.currentTarget.style.display = "none";
                        }}
                        className="size-4 rounded-full object-cover"
                        alt=""
                      />
                    )}
                    <span className="truncate">
                      作者:{c.contentAuthorNickname}
                    </span>
                  </div>
                )}
              </div>
            </div>
          );
        },
      },
      {
        id: "platform",
        accessorKey: "platform",
        header: "平台",
        enableSorting: false,
        cell: ({ row }) => (
          <span
            className={`inline-block w-16 truncate rounded px-1.5 py-0.5 text-center text-[11px] font-medium ${platformClass(row.original.platform)}`}
          >
            {platformName(row.original.platform)}
          </span>
        ),
      },
      {
        id: "intent",
        accessorFn: (c) => c.intentLevel ?? "unanalyzed",
        header: ({ column }) => (
          <DataTableColumnHeader column={column} title="意向" />
        ),
        cell: ({ row }) => {
          const c = row.original;
          if (!c.intentLevel) {
            return <span className="text-xs text-muted-foreground">未分析</span>;
          }
          const meta = INTENT_META[c.intentLevel];
          const badge = (
            <span
              className={`inline-flex rounded-md border px-1.5 py-0.5 text-xs font-medium ${meta.className}`}
            >
              {meta.label}
            </span>
          );
          return c.intentReason ? (
            <SimpleTooltip content={c.intentReason}>
              <span className="cursor-help">{badge}</span>
            </SimpleTooltip>
          ) : (
            badge
          );
        },
      },
      {
        id: "likes",
        accessorFn: (c) => c.likeCount ?? 0,
        header: ({ column }) => (
          <DataTableColumnHeader column={column} title="互动" />
        ),
        cell: ({ row }) => {
          const c = row.original;
          // 定宽 + tabular-nums 让点赞 / 回复两列对齐
          return (
            <div className="flex items-center gap-4 text-xs tabular-nums text-muted-foreground">
              <span className="inline-flex w-14 items-center gap-1">
                <Heart className="size-3 shrink-0" />
                {formatCount(c.likeCount)}
              </span>
              <span className="inline-flex w-14 items-center gap-1">
                <MessageCircle className="size-3 shrink-0" />
                {formatCount(c.replyCount)}
              </span>
            </div>
          );
        },
      },
      {
        id: "createdAt",
        accessorFn: (c) => c.createdAt ?? 0,
        header: ({ column }) => (
          <DataTableColumnHeader column={column} title="评论时间" />
        ),
        cell: ({ row }) => (
          <span className="text-xs text-muted-foreground">
            {formatTimestamp(row.original.createdAt)}
          </span>
        ),
      },
      {
        id: "collectedAt",
        accessorFn: (c) => c.collectedAt,
        header: ({ column }) => (
          <DataTableColumnHeader column={column} title="创建时间" />
        ),
        cell: ({ row }) => (
          <span className="text-xs text-muted-foreground">
            {formatDateTime(row.original.collectedAt)}
          </span>
        ),
      },
    ],
    [platforms, platformName, mediaFileUrl],
  );

  // 导出当前筛选 + 排序后的评论为 Excel(.xlsx);路径经系统保存对话框选定
  async function handleExport() {
    if (total === 0) {
      toast.error("当前没有可导出的评论");
      return;
    }
    try {
      // 样式版 Excel 库接近 1 MB,低频导出动作再按需加载。
      const xlsxPromise = import("xlsx-js-style");
      // 分页接口翻页拼全量(导出是低频动作,串行翻页可接受,不新增导出专用命令)
      const all: CommentView[] = [];
      const pageSize = 2000;
      let offset = 0;
      for (;;) {
        const res = await api.listCommentsPage(
          buildQuery({ sorting: serverState.sorting, limit: pageSize, offset }),
        );
        all.push(...res.items);
        if (all.length >= res.total || res.items.length === 0) break;
        offset += pageSize;
      }
      const XLSX = await xlsxPromise;
      const rows = all.map((c) => ({
        平台: platformName(c.platform),
        评论者: c.authorNickname,
        作者主页:
          authorProfileUrl(c.platform, c.authorUid, c.authorUniqueId) ?? "",
        评论内容: c.text,
        点赞数: c.likeCount ?? 0,
        回复数: c.replyCount ?? 0,
        意向: c.intentLevel ? INTENT_META[c.intentLevel].label : "未分析",
        意向理由: c.intentReason ?? "",
        采集关键词: c.keyword ?? "",
        视频ID: c.contentId,
        所属内容标题: c.contentTitle ?? "",
        内容链接: contentDetailUrl(c.platform, c.contentId) ?? "",
        评论时间: formatTimestamp(c.createdAt),
        创建时间: formatDateTime(c.collectedAt),
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
        { wch: 16 }, // 评论者
        { wch: 42 }, // 作者主页
        { wch: 50 }, // 评论内容
        { wch: 8 }, // 点赞数
        { wch: 8 }, // 回复数
        { wch: 8 }, // 意向
        { wch: 40 }, // 意向理由
        { wch: 20 }, // 采集关键词
        { wch: 24 }, // 视频ID
        { wch: 34 }, // 所属内容标题
        { wch: 46 }, // 内容链接
        { wch: 20 }, // 评论时间
        { wch: 20 }, // 创建时间
      ];
      const wb = XLSX.utils.book_new();
      XLSX.utils.book_append_sheet(wb, ws, "评论");
      const base64 = XLSX.write(wb, { type: "base64", bookType: "xlsx" });
      const now = new Date();
      const ymd = `${now.getFullYear()}${String(now.getMonth() + 1).padStart(2, "0")}${String(now.getDate()).padStart(2, "0")}`;
      // 当天导出流水号:每天从 001 起递增,导出成功才消耗(取消保存不计)
      const SEQ_KEY = "veltrix.comment-export-seq";
      let prevSeq: { date: string; seq: number } = { date: "", seq: 0 };
      try {
        const raw = localStorage.getItem(SEQ_KEY);
        if (raw) prevSeq = JSON.parse(raw);
      } catch {
        // 本地记录损坏则从头计
      }
      const seq = prevSeq.date === ymd ? prevSeq.seq + 1 : 1;
      const fileName = `意向评论-${ymd}-${String(seq).padStart(3, "0")}.xlsx`;
      const path = await save({
        defaultPath: fileName,
        filters: [{ name: "Excel 工作簿", extensions: ["xlsx"] }],
      });
      if (!path) return; // 用户取消保存
      await api.saveBinaryFile(path, base64);
      recordDownload({ path, name: fileName, kind: "评论导出" });
      localStorage.setItem(SEQ_KEY, JSON.stringify({ date: ymd, seq }));
      toast.success(`已导出 ${rows.length} 条评论`);
    } catch (e) {
      toast.error(`导出失败:${e instanceof Error ? e.message : String(e)}`);
    }
  }

  return (
    <div className="flex min-h-0 min-w-0 flex-1 gap-2.5">
      {/* 左侧:行业筛选(可折叠,与图片库一致) */}
      {!sidebarCollapsed && (
        <FilterSidebar
          industries={industries}
          industryCounts={{ ...industryCounts, __all: industryTotal }}
          industryFilter={industryFilter}
          onIndustry={setIndustryFilter}
          onCollapse={() => setSidebarCollapsed(true)}
        />
      )}

      <div
        className={`flex min-h-0 min-w-0 flex-1 flex-col gap-2.5 ${FORM_CONTROL_SIZING}`}
      >
        {/* 行业按钮(收起态) + 评论日期 + 关键字搜索 + 重置 */}
        <div className="flex flex-wrap items-center gap-x-4 gap-y-2">
          {sidebarCollapsed && (
            <IndustryFilterToggle onExpand={() => setSidebarCollapsed(false)} />
          )}
          <DateRangeFilter
            title="评论日期"
            value={commentRange}
            onChange={setCommentRange}
          />
          <FacetedFilter
            title="内容形式"
            options={KIND_FILTERS}
            selected={kindFilter}
            onChange={setKindFilter}
          />
          <div className="relative w-full sm:w-72 lg:w-80">
            <Search className="pointer-events-none absolute left-3 top-1/2 size-4 -translate-y-1/2 text-muted-foreground" />
            <Input
              value={search}
              onChange={(e) => setSearch(e.target.value)}
              placeholder="评论内容 / 作者"
              className="pl-9"
            />
          </div>
          <Button
            variant="outline"
            className="h-10 cursor-pointer px-2 lg:px-3"
            onClick={handleExport}
          >
            <Download className="size-4" />
            导出 Excel
          </Button>
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
          {/* 视图切换:瀑布流 / 表格(与图片库同款:靠右、激活实心高亮) */}
          <div className="ml-auto inline-flex h-10 items-center rounded-md border p-0.5">
            {(
              [
                { key: "waterfall", label: "瀑布流", icon: LayoutGrid },
                { key: "table", label: "表格", icon: List },
              ] as const
            ).map((v) => (
              <button
                key={v.key}
                type="button"
                onClick={() => switchView(v.key)}
                className={`inline-flex h-full cursor-pointer items-center gap-1 rounded px-2.5 text-xs font-medium transition-colors ${
                  viewMode === v.key
                    ? "bg-primary text-primary-foreground"
                    : "text-muted-foreground hover:text-foreground"
                }`}
              >
                <v.icon className="size-3.5" />
                {v.label}
              </button>
            ))}
          </div>
        </div>

        {/* 平台 + 意向筛选同一排:各带标签 + 竖线分隔 */}
        <div className="flex flex-wrap items-center gap-2">
          <span className="mr-1 text-xs font-medium text-muted-foreground">
            平台
          </span>
          {platforms.map((p) => (
            <button
              key={p.id}
              type="button"
              className={platformChipClass(p.id, platformFilter === p.id)}
              onClick={() =>
                setPlatformFilter((prev) => (prev === p.id ? "" : p.id))
              }
            >
              {p.name}
            </button>
          ))}
          <span className="mx-2 h-5 w-px shrink-0 bg-border" />
          <span className="mr-1 text-xs font-medium text-muted-foreground">
            意向
          </span>
          {INTENT_FILTERS.map((f) => (
            <FilterChip
              key={f.value}
              label={f.label}
              active={intentFilter.includes(f.value)}
              activeClassName={f.chipActive}
              inactiveClassName={f.chipInactive}
              onClick={() =>
                setIntentFilter((prev) =>
                  prev.includes(f.value)
                    ? prev.filter((v) => v !== f.value)
                    : [...prev, f.value],
                )
              }
            />
          ))}
        </div>

        {viewMode === "table" ? (
          <DataTable
            columns={columns}
            data={comments}
            itemLabel="评论"
            getRowId={(c) => c.id}
            defaultPageSize={20}
            serverControl={{
              total,
              state: serverState,
              onStateChange: setServerState,
              loading,
            }}
            emptyState={
              <EmptyState
                title="暂无评论"
                description="开启任务的「评论采集」后,这里会展示采集到的评论与意向标记"
              />
            }
          />
        ) : (
          <CommentWaterfall
            groups={groups}
            loading={loading}
            total={groupTotal}
            platformName={platformName}
            onLoadMore={loadMoreGrid}
          />
        )}
      </div>
    </div>
  );
}
