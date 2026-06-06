// 资产库:展示采集落库的内容(contents 表)。全量库/内容库/图片库共用本组件。
// 筛选:左侧栏(行业 + 创建时间 + 发布时间)+ 顶部(平台 chip + 关键字搜索)。
// 关键字匹配 标题 / 采集关键词 / 文案;时间为预设范围。
import { useEffect, useMemo, useState } from "react";
import { type ColumnDef } from "@tanstack/react-table";
import {
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
  Loader2,
  MessageCircle,
  MoreHorizontal,
  Music2,
  RefreshCw,
  Search,
  Share2,
  Trash2,
  Video,
  XCircle,
  X,
} from "lucide-react";
import { openUrl } from "@tauri-apps/plugin-opener";
import { type DateRange } from "react-day-picker";
import { toast } from "sonner";

import { DataTable } from "@/components/DataTable";
import { DataTableColumnHeader } from "@/components/DataTableColumnHeader";
import { Button } from "@/components/ui/button";
import { Calendar } from "@/components/ui/calendar";
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
import { platformClass } from "@/lib/platforms";

const KIND_META: Record<
  ContentView["kind"],
  { label: string; icon: typeof Video }
> = {
  video: { label: "视频", icon: Video },
  image: { label: "图文", icon: ImageIcon },
  article: { label: "文章", icon: FileText },
  unknown: { label: "未知", icon: FileQuestion },
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
  onlyImages = false,
}: {
  // title 仅用于路由区分,页面内不再展示标题
  title?: string;
  onlyImages?: boolean;
}) {
  const [contents, setContents] = useState<ContentView[]>([]);
  const [platforms, setPlatforms] = useState<PlatformConfig[]>([]);
  const [industries, setIndustries] = useState<IndustryView[]>([]);
  const [search, setSearch] = useState("");
  const [platformFilter, setPlatformFilter] = useState(""); // ""=全部
  const [industryFilter, setIndustryFilter] = useState("__all");
  const [createdRange, setCreatedRange] = useState<DateRange | undefined>();
  const [publishedRange, setPublishedRange] = useState<DateRange | undefined>();
  const [sidebarCollapsed, setSidebarCollapsed] = useResponsiveCollapse();
  // 正在重试素材下载的内容 id 集合(行级 loading,避免重复点击)
  const [retrying, setRetrying] = useState<Set<string>>(new Set());

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

  // 图片库只看含图片的内容;其余库看全部
  const base = useMemo(
    () => (onlyImages ? contents.filter((c) => c.imageUrls.length > 0) : contents),
    [contents, onlyImages],
  );

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
  }, [base, platformFilter, industryFilter, createdRange, publishedRange, search]);

  // 是否有任意筛选生效(决定显示「重置」)
  const hasFilter =
    platformFilter !== "" ||
    industryFilter !== "__all" ||
    !!createdRange?.from ||
    !!publishedRange?.from ||
    search !== "";

  function resetFilters() {
    setPlatformFilter("");
    setIndustryFilter("__all");
    setCreatedRange(undefined);
    setPublishedRange(undefined);
    setSearch("");
  }

  // 删除一条内容:后端删库 + 本地列表移除
  async function handleDelete(id: string) {
    try {
      await api.removeContent(id);
      setContents((prev) => prev.filter((x) => x.id !== id));
      toast.success("已删除");
    } catch (e) {
      toast.error(`删除失败: ${e}`);
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

  // 详情:抖音打开视频详情页 /video/{aweme_id},其余退回视频直链/封面;无链接则提示
  function handleDetail(c: ContentView) {
    const url =
      c.platform === "douyin" && c.contentId
        ? `https://www.douyin.com/video/${c.contentId}`
        : c.videoUrl || c.coverUrl || c.imageUrls[0];
    if (!url) {
      toast.info("该内容暂无可打开的链接");
      return;
    }
    openUrl(url).catch((e) => toast.error(`打开失败: ${e}`));
  }

  // 依赖 platforms:平台异步加载后重建列,否则 platformName 闭包锁在空列表显示 id
  // eslint-disable-next-line react-hooks/exhaustive-deps
  const columns = useMemo<ColumnDef<ContentView>[]>(
    () => [
      {
        id: "content",
        header: "内容",
        enableSorting: false,
        // 卡片:左封面 + 右(作者头像/昵称/抖音号、关键词红标+标题、话题、互动数据)
        cell: ({ row }) => <ContentCard c={row.original} />,
      },
      {
        id: "kind",
        accessorKey: "kind",
        header: ({ column }) => (
          <DataTableColumnHeader column={column} title="内容形式" />
        ),
        cell: ({ row }) => {
          const meta = KIND_META[row.original.kind] ?? KIND_META.unknown;
          const Icon = meta.icon;
          return (
            <span className="inline-flex items-center gap-1 whitespace-nowrap rounded-md bg-secondary px-2 py-0.5 text-[11px] font-medium text-secondary-foreground">
              <Icon className="size-3" />
              {meta.label}
            </span>
          );
        },
      },
      {
        id: "industry",
        accessorKey: "industry",
        header: ({ column }) => (
          <DataTableColumnHeader column={column} title="所属行业" />
        ),
        cell: ({ row }) =>
          row.original.industry ? (
            <span className="inline-block whitespace-nowrap rounded-md bg-secondary px-2 py-0.5 text-[11px] font-medium text-secondary-foreground">
              {row.original.industry}
            </span>
          ) : (
            <span className="text-xs text-muted-foreground">—</span>
          ),
      },
      {
        id: "platform",
        accessorKey: "platform",
        header: ({ column }) => (
          <DataTableColumnHeader column={column} title="所属平台" />
        ),
        cell: ({ row }) => (
          <span
            className={`inline-block whitespace-nowrap rounded px-2 py-0.5 text-[11px] font-medium ${platformClass(row.original.platform)}`}
          >
            {platformName(row.original.platform)}
          </span>
        ),
      },
      {
        id: "publishedAt",
        accessorKey: "publishedAt",
        header: ({ column }) => (
          <DataTableColumnHeader column={column} title="发布时间" />
        ),
        cell: ({ row }) => (
          <span className="whitespace-nowrap text-xs text-muted-foreground">
            {formatTime(row.original.publishedAt)}
          </span>
        ),
      },
      {
        id: "collectedAt",
        accessorKey: "collectedAt",
        header: ({ column }) => (
          <DataTableColumnHeader column={column} title="创建时间" />
        ),
        cell: ({ row }) => (
          <span className="whitespace-nowrap text-xs text-muted-foreground">
            {formatTime(row.original.collectedAt)}
          </span>
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
                <DropdownMenuItem onClick={() => handleDetail(c)}>
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
        <div className="flex min-h-0 min-w-0 flex-1 flex-col gap-3">
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
            <div className="relative w-full sm:w-56">
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
          </div>
          {/* 平台筛选:不选即全部,点已选取消 */}
          <div className="flex flex-wrap items-center gap-2">
            {platformOptions.map((id) => (
              <FilterChip
                key={id}
                label={platformName(id)}
                active={platformFilter === id}
                onClick={() =>
                  setPlatformFilter((prev) => (prev === id ? "" : id))
                }
              />
            ))}
          </div>

          <DataTable
            columns={columns}
            data={filtered}
            itemLabel="内容"
            getRowId={(c) => c.id}
            defaultPageSize={50}
            emptyState={
              <div className="py-10 text-center text-sm text-muted-foreground">
                暂无符合条件的内容
              </div>
            }
          />
        </div>
      </div>
  );
}

// 单条内容卡片:左封面 + 右(作者头像/昵称/抖音ID、关键词红标+标题、话题、互动数据)。
// 平台与操作各自独立成列,卡片只承载内容主体。
function ContentCard({ c }: { c: ContentView }) {
  const meta = KIND_META[c.kind] ?? KIND_META.unknown;
  const Icon = meta.icon;
  const cover = c.coverUrl || c.imageUrls[0];
  // 话题已由后端从 desc 剥离到 topics 字段,这里直接用原文,不再正则切(避免误删 #1/C# 等合法井号)
  const titleText = c.title || c.desc || "(无文案)";
  // 作者主页链接(目前仅抖音:/user/{sec_uid});点头像跳转
  const homeUrl =
    c.platform === "douyin" && c.authorUid
      ? `https://www.douyin.com/user/${c.authorUid}`
      : null;
  // 视频详情链接(抖音:/video/{aweme_id});点封面跳转
  const detailUrl =
    c.platform === "douyin" && c.contentId
      ? `https://www.douyin.com/video/${c.contentId}`
      : c.videoUrl;
  return (
    // 固定列宽:不随标题长度撑宽,长标题在卡片内换行(窄屏 max-w-full 收缩,表格横向滚动兜底)
    <div className="flex w-[52rem] max-w-full gap-3 py-1">
      {/* 封面(竖图):点击跳转视频详情,hover 微透显示可点 */}
      {cover ? (
        <SimpleTooltip content={detailUrl ? "打开视频详情" : "暂无详情链接"}>
          <img
            src={cover}
            alt=""
            loading="lazy"
            className={`h-32 w-24 shrink-0 rounded-md object-cover transition ${
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
            onError={(e) => {
              e.currentTarget.style.display = "none";
            }}
          />
        </SimpleTooltip>
      ) : (
        <div className="flex h-32 w-24 shrink-0 items-center justify-center rounded-md bg-muted">
          <Icon className="size-6 text-muted-foreground" />
        </div>
      )}

      {/* 右侧信息 */}
      <div className="flex min-w-0 flex-1 flex-col gap-2">
        {/* 作者:头像(跳主页)+ 昵称/抖音号(点击复制) */}
        <div className="flex items-center gap-2">
          {/* 头像:点击跳转作者主页,hover 高亮环 */}
          {c.authorAvatar ? (
            <SimpleTooltip content={homeUrl ? "打开作者主页" : "暂无主页链接"}>
              <img
                src={c.authorAvatar}
                alt=""
                loading="lazy"
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
                onError={(e) => {
                  e.currentTarget.style.display = "none";
                }}
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

        {/* 标题:完整换行 */}
        <span className="whitespace-normal break-words text-sm text-foreground">
          {titleText}
        </span>

        {/* 关键词(置首,红标) + 话题(紫标),包裹换行 */}
        {(c.keyword || c.topics.length > 0) && (
          <div className="flex flex-wrap items-center gap-1">
            {c.keyword && (
              <span className="inline-flex items-center gap-1 rounded bg-red-500 px-1.5 py-0.5 text-[11px] font-medium text-white">
                <Search className="size-3" />
                {c.keyword}
              </span>
            )}
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
      </div>
    </div>
  );
}

// 素材状态徽章:展示下载/音频提取结果。失败态可点击重新拉取。
// 成功 + 视频已提取音频 → 额外音频标识;失败 → 红标 + tooltip 带原因。
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

  switch (c.mediaStatus) {
    case "success": {
      const hasAudio = c.kind === "video" && c.audioExtracted === true;
      return (
        <span className="inline-flex items-center gap-1 whitespace-nowrap rounded-md bg-emerald-100 px-2 py-0.5 text-[11px] font-medium text-emerald-700 dark:bg-emerald-950/60 dark:text-emerald-300">
          <CheckCircle2 className="size-3.5" />
          已下载
          {hasAudio && (
            <SimpleTooltip content="音频已成功提取">
              <Music2 className="size-3" />
            </SimpleTooltip>
          )}
        </span>
      );
    }
    case "failed":
      return (
        <SimpleTooltip
          content={
            c.mediaError
              ? `失败原因: ${c.mediaError} · 点击重试`
              : "点击重新拉取素材"
          }
        >
          <button
            type="button"
            onClick={onRetry}
            className="inline-flex cursor-pointer items-center gap-1 whitespace-nowrap rounded-md bg-rose-100 px-2 py-0.5 text-[11px] font-medium text-rose-700 transition-colors hover:bg-rose-200 dark:bg-rose-950/60 dark:text-rose-300 dark:hover:bg-rose-900/60"
          >
            <XCircle className="size-3.5" />
            失败
            <RefreshCw className="size-3" />
          </button>
        </SimpleTooltip>
      );
    case "pending":
      return (
        <span className="inline-flex items-center gap-1 whitespace-nowrap rounded-md bg-amber-100 px-2 py-0.5 text-[11px] font-medium text-amber-700 dark:bg-amber-950/60 dark:text-amber-300">
          <Clock className="size-3.5" />
          待处理
        </span>
      );
    default:
      return <span className="text-xs text-muted-foreground">—</span>;
  }
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
      <div className="flex h-8 items-center justify-between border-b px-3">
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
  const label = value?.from
    ? value.to
      ? `${fmtDate(value.from)} ~ ${fmtDate(value.to)}`
      : fmtDate(value.from)
    : "全部";
  return (
    <div className="flex items-center gap-1.5">
      <span className="text-xs text-muted-foreground">{title}</span>
      <Popover>
        <PopoverTrigger asChild>
          <Button variant="outline" className="cursor-pointer">
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
    </div>
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
