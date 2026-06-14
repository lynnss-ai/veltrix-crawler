// 评论库:展示采集落库的评论(comments 表)+ AI 意向标记。
// 筛选:左侧栏(行业 + 角标)+ 顶部(意向 / 平台 chip + 评论日期 + 关键字)。
import { useEffect, useMemo, useState } from "react";
import { type ColumnDef } from "@tanstack/react-table";
import { Filter, Heart, MessageCircle, Search, X } from "lucide-react";
import { type DateRange } from "react-day-picker";
import { toast } from "sonner";

import { DataTable } from "@/components/DataTable";
import { DataTableColumnHeader } from "@/components/DataTableColumnHeader";
import { FacetedFilter } from "@/components/FacetedFilter";
import { FORM_CONTROL_SIZING } from "@/lib/form-sizing";
import {
  DateRangeFilter,
  FilterChip,
  FilterSidebar,
  inDateRange,
} from "@/components/library-filters";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { SimpleTooltip } from "@/components/SimpleTooltip";
import { useResponsiveCollapse } from "@/hooks/use-responsive-collapse";
import {
  api,
  type CommentView,
  type IndustryView,
  type PlatformConfig,
} from "@/lib/api";
import { platformClass, platformChipClass } from "@/lib/platforms";
import { convertFileSrc } from "@tauri-apps/api/core";
import { EmptyState } from "@/components/EmptyState";

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

// 意向筛选项:all=全部,unanalyzed=尚未分析(intentLevel 为 null)
const INTENT_FILTERS: { value: string; label: string }[] = [
  { value: "high", label: "高意向" },
  { value: "medium", label: "中意向" },
  { value: "low", label: "低意向" },
  { value: "none", label: "无意向" },
  { value: "unanalyzed", label: "未分析" },
];

function formatCount(n?: number | null): string {
  if (n == null) return "0";
  if (n >= 10000) return `${(n / 10000).toFixed(1)}万`;
  return String(n);
}

function formatTime(ts?: number | null): string {
  if (!ts) return "—";
  return new Date(ts * 1000).toLocaleString();
}

export function CommentLibraryPage() {
  const [comments, setComments] = useState<CommentView[]>([]);
  const [platforms, setPlatforms] = useState<PlatformConfig[]>([]);
  const [industries, setIndustries] = useState<IndustryView[]>([]);
  const [search, setSearch] = useState("");
  const [platformFilter, setPlatformFilter] = useState(""); // ""=全部
  const [intentFilter, setIntentFilter] = useState("all");
  const [industryFilter, setIndustryFilter] = useState("__all");
  const [commentRange, setCommentRange] = useState<DateRange | undefined>();
  const [kindFilter, setKindFilter] = useState<string[]>([]); // []=全部形态
  const [sidebarCollapsed, setSidebarCollapsed] = useResponsiveCollapse();

  useEffect(() => {
    api
      .listComments()
      .then(setComments)
      .catch((e) => toast.error(`加载评论失败: ${e}`));
    api.listPlatforms().then(setPlatforms).catch(() => {});
    api.listIndustries().then(setIndustries).catch(() => {});
  }, []);

  const platformName = (id: string) =>
    platforms.find((p) => p.id === id)?.name ?? id;

  // 各行业评论数(侧栏角标)
  const industryCounts = useMemo(() => {
    const map: Record<string, number> = { __all: comments.length };
    for (const c of comments) {
      if (c.industry) map[c.industry] = (map[c.industry] ?? 0) + 1;
    }
    return map;
  }, [comments]);

  const filtered = useMemo(() => {
    return comments.filter((c) => {
      if (platformFilter && c.platform !== platformFilter) return false;
      if (kindFilter.length > 0 && !kindFilter.includes(c.contentKind ?? ""))
        return false;
      if (industryFilter !== "__all" && c.industry !== industryFilter)
        return false;
      if (intentFilter !== "all") {
        if (intentFilter === "unanalyzed") {
          if (c.intentLevel != null) return false;
        } else if (c.intentLevel !== intentFilter) {
          return false;
        }
      }
      if (!inDateRange(c.createdAt, commentRange)) return false;
      if (search) {
        const q = search.toLowerCase();
        return (
          c.text.toLowerCase().includes(q) ||
          c.authorNickname.toLowerCase().includes(q)
        );
      }
      return true;
    });
  }, [
    comments,
    platformFilter,
    kindFilter,
    industryFilter,
    intentFilter,
    commentRange,
    search,
  ]);

  const hasFilter =
    platformFilter !== "" ||
    kindFilter.length > 0 ||
    industryFilter !== "__all" ||
    intentFilter !== "all" ||
    commentRange?.from != null ||
    search !== "";

  const resetFilters = () => {
    setPlatformFilter("");
    setKindFilter([]);
    setIndustryFilter("__all");
    setIntentFilter("all");
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
          const cover = c.contentCoverPath
            ? convertFileSrc(c.contentCoverPath)
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
        id: "industry",
        accessorKey: "industry",
        header: ({ column }) => (
          <DataTableColumnHeader column={column} title="所属行业" />
        ),
        cell: ({ row }) =>
          row.original.industry ? (
            <span className="text-xs text-foreground">
              {row.original.industry}
            </span>
          ) : (
            <span className="text-xs text-muted-foreground">—</span>
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
            {formatTime(row.original.createdAt)}
          </span>
        ),
      },
    ],
    // platformName 依赖 platforms,平台加载后重建列以正确显示名称
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [platforms],
  );

  return (
    <div className="flex min-h-0 min-w-0 flex-1 gap-4">
      {/* 左侧:行业筛选(可折叠,与图片库一致) */}
      {!sidebarCollapsed && (
        <FilterSidebar
          industries={industries}
          industryCounts={industryCounts}
          industryFilter={industryFilter}
          onIndustry={setIndustryFilter}
          onCollapse={() => setSidebarCollapsed(true)}
        />
      )}

      <div
        className={`flex min-h-0 min-w-0 flex-1 flex-col gap-3 ${FORM_CONTROL_SIZING}`}
      >
        {/* 行业按钮(收起态) + 评论日期 + 关键字搜索 + 重置 */}
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
              active={intentFilter === f.value}
              onClick={() =>
                setIntentFilter((prev) => (prev === f.value ? "all" : f.value))
              }
            />
          ))}
        </div>

        <DataTable
          columns={columns}
          data={filtered}
          itemLabel="评论"
          getRowId={(c) => c.id}
          defaultPageSize={50}
          emptyState={
            <EmptyState
              title="暂无评论"
              description="开启任务的「评论采集」后,这里会展示采集到的评论与意向标记"
            />
          }
        />
      </div>
    </div>
  );
}
