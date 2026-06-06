// 任务调度页:任务列表 + 任务归档双 tab。MVP 阶段纯前端 mock,后续接 Tauri commands。
//
// 设计要点:
// - 任务支持三种触发:立即一次 / 定时一次 / 持续监听(增量)
// - 列表 tab 显示 pending/running/paused;归档 tab 显示 completed/failed/cancelled
// - 操作按钮按状态动态:running → 暂停/停止,paused → 启动/停止,pending → 启动/停止

import { useEffect, useMemo, useState, type FormEvent } from "react";
import { useResponsiveCollapse } from "@/hooks/use-responsive-collapse";
import {
  api,
  type IndustryView,
  type PlatformConfig,
  type TaskInput,
  type TaskView,
} from "@/lib/api";
import { TaskDetailPage } from "@/pages/TaskDetailPage";
import { platformClass } from "@/lib/platforms";
import { type ColumnDef } from "@tanstack/react-table";
import {
  CalendarClock,
  ChevronLeft,
  CircleSlash2,
  Filter,
  Infinity as InfinityIcon,
  MoreHorizontal,
  Plus,
  Radar,
  Search,
  X,
  Zap,
} from "lucide-react";
import { toast } from "sonner";

import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Progress } from "@/components/ui/progress";
import { DataTable } from "@/components/DataTable";
import { DataTableColumnHeader } from "@/components/DataTableColumnHeader";
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
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { Switch } from "@/components/ui/switch";
import { Textarea } from "@/components/ui/textarea";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import {
  Sheet,
  SheetContent,
  SheetDescription,
  SheetFooter,
  SheetHeader,
  SheetTitle,
} from "@/components/ui/sheet";
import { SimpleTooltip } from "@/components/SimpleTooltip";
import {
  Tabs,
  TabsContent,
  TabsList,
  TabsTrigger,
} from "@/components/ui/tabs";

// ---- 数据模型(沿用后端 TaskView,本地 alias 为 TaskItem 方便引用) ----

type TaskStatus = TaskView["status"];
type TaskTrigger = TaskView["trigger"];
type SortMode = TaskView["sortMode"];
type TimeRange = TaskView["timeRange"];
type TaskItem = TaskView;

const SORT_MODE_META: Record<SortMode, { label: string }> = {
  synthetic: { label: "综合" },
  hottest: { label: "最热" },
  latest: { label: "最新" },
};

const TIME_RANGE_META: Record<TimeRange, { label: string }> = {
  any: { label: "不限" },
  "1d": { label: "一天内" },
  "1w": { label: "一周内" },
  "6m": { label: "半年内" },
};

// 完成的任务保留在原列表(用户偏好:已完成≠归档),归档 tab 只收失败/手动停止
const ACTIVE_STATUSES: TaskStatus[] = [
  "pending",
  "running",
  "paused",
  "completed",
];
const ARCHIVED_STATUSES: TaskStatus[] = ["failed", "cancelled"];

// 三个未归档 tab:按触发类型切分
function isActive(t: TaskItem): boolean {
  return ACTIVE_STATUSES.includes(t.status);
}

// 终态:任务已结束(完成/失败/已停止),不再有暂停/停止等运行操作,可重跑或复制
function isTerminal(t: TaskItem): boolean {
  return ["completed", "failed", "cancelled"].includes(t.status);
}
// 任务列表:所有未归档的任务(三种触发类型都纳入,作为总览)
function isInWatchingList(t: TaskItem): boolean {
  return isActive(t);
}
// 快速任务:立即一次
function isInQuickList(t: TaskItem): boolean {
  return t.trigger === "once-now" && isActive(t);
}
// 定时任务队列:每日定时
function isInScheduledQueue(t: TaskItem): boolean {
  return t.trigger === "daily" && isActive(t);
}

const STATUS_META: Record<
  TaskStatus,
  { label: string; className: string; dot: string }
> = {
  pending: {
    label: "等待运行",
    className:
      "border-amber-500/30 bg-amber-500/10 text-amber-600 dark:text-amber-400",
    dot: "bg-amber-500",
  },
  running: {
    label: "运行中",
    className:
      "border-emerald-500/30 bg-emerald-500/10 text-emerald-600 dark:text-emerald-400",
    dot: "bg-emerald-500 animate-pulse",
  },
  paused: {
    label: "已暂停",
    className:
      "border-slate-500/30 bg-slate-500/10 text-slate-600 dark:text-slate-400",
    dot: "bg-slate-500",
  },
  completed: {
    label: "已完成",
    className:
      "border-sky-500/30 bg-sky-500/10 text-sky-600 dark:text-sky-400",
    dot: "bg-sky-500",
  },
  failed: {
    label: "失败",
    className: "border-destructive/30 bg-destructive/10 text-destructive",
    dot: "bg-destructive",
  },
  cancelled: {
    label: "已停止",
    className: "border-slate-500/30 bg-slate-500/10 text-slate-500",
    dot: "bg-slate-400",
  },
};

const TRIGGER_META: Record<
  TaskTrigger,
  { label: string; icon: typeof Zap }
> = {
  "once-now": { label: "立即一次", icon: Zap },
  daily: { label: "每日定时", icon: CalendarClock },
  watching: { label: "持续监听", icon: InfinityIcon },
};

// 采集策略默认值 — 新建任务表单初值
const DEFAULT_STRATEGY = {
  sortMode: "synthetic" as SortMode,
  timeRange: "any" as TimeRange,
  perKeywordLimit: 50,
  minLikes: 0,
  aiExtract: false,
};

// 平台颜色 / 名称统一从 @/lib/platforms 取(PlatformId 枚举);本文件内仅引用

function formatTime(ts?: number | null): string {
  if (!ts) return "—";
  return new Date(ts * 1000).toLocaleString();
}

function formatRelative(ts: number): string {
  const diff = Math.max(0, Math.floor(Date.now() / 1000) - ts);
  if (diff < 60) return `${diff} 秒前`;
  if (diff < 3600) return `${Math.floor(diff / 60)} 分钟前`;
  if (diff < 86400) return `${Math.floor(diff / 3600)} 小时前`;
  return `${Math.floor(diff / 86400)} 天前`;
}

// ---- 页面主体 ----

export function CollectPage() {
  const [tasks, setTasks] = useState<TaskItem[]>([]);
  const [industries, setIndustries] = useState<IndustryView[]>([]);
  const [platforms, setPlatforms] = useState<PlatformConfig[]>([]);

  // 平台 id → 名称,首列任务名前的标签和 toolbar 都用这个查表
  const platformName = (id: string) =>
    platforms.find((p) => p.id === id)?.name ?? id;

  // 加载任务列表;每次 mutate 后重拉(简单粗暴,数据量不大时 OK)
  const reload = () => {
    api
      .listTasks()
      .then(setTasks)
      .catch((e) => toast.error(`加载任务失败: ${e}`));
  };
  useEffect(() => {
    reload();
    api.listIndustries().then(setIndustries).catch(() => {});
    api.listPlatforms().then(setPlatforms).catch(() => {});
  }, []);

  // 采集在后端后台异步进行,有任务 running 时轮询刷新进度
  const hasRunningTask = tasks.some((t) => t.status === "running");
  useEffect(() => {
    if (!hasRunningTask) return;
    const timer = setInterval(reload, 3000);
    return () => clearInterval(timer);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [hasRunningTask]);
  const [tab, setTab] = useState<
    "active" | "quick" | "scheduled" | "archive"
  >("active");
  const [search, setSearch] = useState("");
  // 空字符串视为「不筛选,展示全部」;点击已选 chip 可取消
  const [platformFilter, setPlatformFilter] = useState<string>("");
  const [industryFilter, setIndustryFilter] = useState<string>("__all");
  const [sidebarCollapsed, setSidebarCollapsed] = useResponsiveCollapse();
  const [formOpen, setFormOpen] = useState(false);
  // 详情页 task id;非空时整页切到 TaskDetailPage
  const [detailId, setDetailId] = useState<string | null>(null);
  const [editingTask, setEditingTask] = useState<TaskItem | null>(null);

  const filtered = useMemo(() => {
    const inTab = (t: TaskItem) => {
      if (tab === "active") return isInWatchingList(t);
      if (tab === "quick") return isInQuickList(t);
      if (tab === "scheduled") return isInScheduledQueue(t);
      return ARCHIVED_STATUSES.includes(t.status);
    };
    return tasks.filter((t) => {
      if (!inTab(t)) return false;
      if (platformFilter && t.platform !== platformFilter) return false;
      if (industryFilter !== "__all" && t.industry !== industryFilter)
        return false;
      if (search) {
        const q = search.toLowerCase();
        return (
          t.name.toLowerCase().includes(q) ||
          t.keywords.some((k) => k.toLowerCase().includes(q))
        );
      }
      return true;
    });
  }, [tasks, tab, search, platformFilter, industryFilter]);

  // 各行业任务数(角标),只统计当前 tab 下的
  const industryCounts = useMemo(() => {
    const inTab = (t: TaskItem) => {
      if (tab === "active") return isInWatchingList(t);
      if (tab === "quick") return isInQuickList(t);
      if (tab === "scheduled") return isInScheduledQueue(t);
      return ARCHIVED_STATUSES.includes(t.status);
    };
    const map: Record<string, number> = { __all: 0 };
    for (const t of tasks) {
      if (!inTab(t)) continue;
      map.__all += 1;
      map[t.industry] = (map[t.industry] ?? 0) + 1;
    }
    return map;
  }, [tasks, tab]);

  const counts = useMemo(
    () => ({
      active: tasks.filter(isInWatchingList).length,
      quick: tasks.filter(isInQuickList).length,
      scheduled: tasks.filter(isInScheduledQueue).length,
      archive: tasks.filter((t) => ARCHIVED_STATUSES.includes(t.status)).length,
    }),
    [tasks],
  );

  // 是否有任意筛选生效(决定显示「重置」)
  const hasFilter =
    platformFilter !== "" || industryFilter !== "__all" || search !== "";
  function resetFilters() {
    setPlatformFilter("");
    setIndustryFilter("__all");
    setSearch("");
  }

  // 只允许变更 status + started/finished 时间,其他字段保留(后端 update_task_status)
  function updateTask(id: string, patch: Partial<TaskItem>) {
    if (!patch.status) return;
    api
      .updateTaskStatus({
        id,
        status: patch.status,
        startedAt: patch.startedAt ?? null,
        finishedAt: patch.finishedAt ?? null,
      })
      .then(reload)
      .catch((e) => toast.error(`更新失败: ${e}`));
  }

  // 启动采集:接后端 run_task(选账号 → 后台开窗 + 拟人 RPA 采集),启动后轮询看进度
  function runTask(id: string) {
    api
      .runTask(id)
      .then(() => {
        toast.success("已启动采集");
        reload();
      })
      .catch((e) => toast.error(`启动失败: ${e}`));
  }

  function deleteTask(id: string) {
    api
      .removeTask(id)
      .then(() => {
        toast.success("任务已删除");
        reload();
      })
      .catch((e) => toast.error(`删除失败: ${e}`));
  }

  function handleSaveTask(input: TaskInput) {
    api
      .upsertTask(input)
      .then(() => {
        setFormOpen(false);
        setEditingTask(null);
        toast.success(editingTask ? "任务已更新" : "任务已创建");
        reload();
      })
      .catch((e) => toast.error(`保存失败: ${e}`));
  }

  function onEdit(t: TaskItem) {
    setEditingTask(t);
    setFormOpen(true);
  }
  function onDetail(t: TaskItem) {
    setDetailId(t.id);
  }

  // 列工厂:active / archive 共用大部分列,进度列仅 active 显示
  const buildColumns = (isArchive: boolean): ColumnDef<TaskItem>[] => {
    const cols: ColumnDef<TaskItem>[] = [
      {
        id: "name",
        accessorKey: "name",
        header: ({ column }) => (
          <DataTableColumnHeader column={column} title="任务名" />
        ),
        cell: ({ row }) => (
          <span className="block truncate font-medium text-foreground">
            {row.original.name}
          </span>
        ),
      },
      {
        id: "platform",
        accessorKey: "platform",
        header: ({ column }) => (
          <DataTableColumnHeader column={column} title="所属平台" />
        ),
        // 库里存的是 platform id,展示时转换为平台名称
        cell: ({ row }) => {
          const t = row.original;
          return (
            <span
              className={`inline-block w-20 truncate rounded px-1.5 py-0.5 text-center text-[11px] font-medium ${platformClass(t.platform)}`}
            >
              {platformName(t.platform)}
            </span>
          );
        },
      },
      {
        id: "collected",
        accessorFn: (t) => t.contentCount,
        header: ({ column }) => (
          <DataTableColumnHeader column={column} title="采集结果" />
        ),
        cell: ({ row }) => {
          const t = row.original;
          return (
            <div className="flex flex-col gap-0.5 text-xs">
              <div>
                <span className="text-muted-foreground">内容</span>
                <span className="ml-1 font-mono font-medium text-foreground">
                  {t.contentCount.toLocaleString()}
                </span>
              </div>
              <div>
                <span className="text-muted-foreground">评论</span>
                <span className="ml-1 font-mono font-medium text-foreground">
                  {t.commentCount.toLocaleString()}
                </span>
              </div>
            </div>
          );
        },
      },
      {
        id: "params",
        header: "采集参数",
        enableSorting: false,
        cell: ({ row }) => {
          const t = row.original;
          return (
            <div className="space-y-1">
              <div className="flex flex-wrap gap-1">
                {t.keywords.slice(0, 4).map((k) => (
                  <span
                    key={k}
                    className="rounded border bg-background px-1.5 py-0.5 text-[11px] text-foreground"
                  >
                    {k}
                  </span>
                ))}
                {t.keywords.length > 4 && (
                  <SimpleTooltip content={t.keywords.slice(4).join(" · ")}>
                    <span className="cursor-help text-[11px] text-muted-foreground">
                      +{t.keywords.length - 4}
                    </span>
                  </SimpleTooltip>
                )}
              </div>
              <div className="flex flex-wrap gap-1 text-[11px] text-muted-foreground">
                <span className="rounded bg-muted px-1.5 py-0.5">
                  {SORT_MODE_META[t.sortMode].label}
                </span>
                <span className="rounded bg-muted px-1.5 py-0.5">
                  {TIME_RANGE_META[t.timeRange].label}
                </span>
                <span className="rounded bg-muted px-1.5 py-0.5">
                  ≤ {t.perKeywordLimit}
                </span>
                {t.minLikes > 0 && (
                  <span className="rounded bg-muted px-1.5 py-0.5">
                    ≥{t.minLikes}赞
                  </span>
                )}
                {t.aiExtract && (
                  <span className="rounded bg-primary/10 px-1.5 py-0.5 text-primary">
                    AI
                  </span>
                )}
              </div>
            </div>
          );
        },
      },
      {
        id: "trigger",
        accessorKey: "trigger",
        header: "触发",
        enableSorting: false,
        cell: ({ row }) => {
          const t = row.original;
          const Icon = TRIGGER_META[t.trigger].icon;
          return (
            <span className="inline-flex items-center gap-1 rounded-md bg-muted px-1.5 py-0.5 text-xs text-muted-foreground">
              <Icon className="size-3" />
              {TRIGGER_META[t.trigger].label}
            </span>
          );
        },
      },
      {
        id: "status",
        accessorKey: "status",
        header: ({ column }) => (
          <DataTableColumnHeader column={column} title="状态" />
        ),
        cell: ({ row }) => {
          const t = row.original;
          const meta = STATUS_META[t.status];
          return (
            <span
              className={`inline-flex items-center gap-1.5 rounded-md border px-1.5 py-0.5 text-xs font-medium ${meta.className}`}
            >
              <span className={`size-1.5 rounded-full ${meta.dot}`} />
              {meta.label}
              {t.status === "failed" && t.errorMessage && (
                <SimpleTooltip content={t.errorMessage}>
                  <span className="cursor-help text-[10px] underline">
                    详情
                  </span>
                </SimpleTooltip>
              )}
            </span>
          );
        },
      },
    ];

    if (!isArchive) {
      cols.push({
        id: "progress",
        accessorKey: "progress",
        header: ({ column }) => (
          <DataTableColumnHeader column={column} title="进度" />
        ),
        cell: ({ row }) => {
          const t = row.original;
          if (t.status === "running" || t.status === "paused") {
            return (
              <div className="flex w-32 items-center gap-2">
                <Progress value={t.progress} className="h-1.5 flex-1" />
                <span className="font-mono text-xs text-muted-foreground">
                  {t.progress}%
                </span>
              </div>
            );
          }
          return <span className="text-xs text-muted-foreground">—</span>;
        },
      });
    }

    cols.push({
      id: "time",
      header: isArchive ? "结束时间" : "时间",
      enableSorting: false,
      cell: ({ row }) => {
        const t = row.original;
        return (
          <span className="text-xs text-muted-foreground">
            {isArchive ? (
              formatTime(t.finishedAt)
            ) : t.trigger === "daily" && t.scheduledAt ? (
              <>
                每日 <span className="font-mono">{t.scheduledAt}</span>
              </>
            ) : t.trigger === "watching" && t.watchIntervalMin ? (
              <>
                每 <span className="font-mono">{t.watchIntervalMin}</span> 分
              </>
            ) : t.startedAt ? (
              `开始 ${formatRelative(t.startedAt)}`
            ) : (
              "—"
            )}
          </span>
        );
      },
    });

    cols.push({
      id: "actions",
      header: () => <div className="text-right">操作</div>,
      enableSorting: false,
      cell: ({ row }) => (
        <TaskActionsCell
          task={row.original}
          isArchive={isArchive}
          onUpdate={updateTask}
          onRun={runTask}
          onDelete={deleteTask}
          onEdit={onEdit}
          onDetail={onDetail}
        />
      ),
    });

    return cols;
  };

  // 依赖 platforms:平台列表异步加载后重建列,否则「所属平台」cell 的 platformName
  // 闭包锁在初始空列表上,一直回退显示 id(拼音)
  // eslint-disable-next-line react-hooks/exhaustive-deps
  const activeColumns = useMemo(() => buildColumns(false), [platforms]);
  // eslint-disable-next-line react-hooks/exhaustive-deps
  const archiveColumns = useMemo(() => buildColumns(true), [platforms]);

  // 详情页:在所有 hooks 之后再做条件 return,保持 hooks 顺序稳定
  const detailTask = detailId ? tasks.find((x) => x.id === detailId) : null;
  if (detailTask) {
    return (
      <TaskDetailPage
        task={detailTask}
        platformName={platformName}
        onBack={() => setDetailId(null)}
      />
    );
  }

  return (
    <div className="flex h-full min-w-0 flex-col gap-4">
      <div className="flex min-h-0 min-w-0 flex-1 gap-4">
        {/* 左侧:行业筛选(展开态显示完整面板) */}
        {!sidebarCollapsed && (
          <IndustryFilterSidebar
            industries={industries}
            onCollapse={() => setSidebarCollapsed(true)}
            selected={industryFilter}
            onSelect={setIndustryFilter}
            counts={industryCounts}
          />
        )}

        {/* 右侧:Tabs + 表格 */}
        <div className="flex min-w-0 flex-1 flex-col">
          <Tabs
            value={tab}
            onValueChange={(v) => setTab(v as typeof tab)}
            className="flex h-full flex-col"
          >
            <div className="flex flex-wrap items-center justify-between gap-2">
              <div className="flex flex-wrap items-center gap-2">
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
                <TabsList className="max-w-full overflow-x-auto">
                <TabsTrigger value="active">
                  任务列表
                  <Badge variant="secondary" className="ml-1.5">
                    {counts.active}
                  </Badge>
                </TabsTrigger>
                <TabsTrigger value="quick">
                  快速任务
                  <Badge variant="secondary" className="ml-1.5">
                    {counts.quick}
                  </Badge>
                </TabsTrigger>
                <TabsTrigger value="scheduled">
                  定时任务队列
                  <Badge variant="secondary" className="ml-1.5">
                    {counts.scheduled}
                  </Badge>
                </TabsTrigger>
                <TabsTrigger value="archive">
                  任务归档
                  <Badge variant="secondary" className="ml-1.5">
                    {counts.archive}
                  </Badge>
                </TabsTrigger>
              </TabsList>
              </div>

              <div className="flex items-center gap-2">
                <div className="relative w-full sm:w-56">
                  <Search className="pointer-events-none absolute left-2.5 top-1/2 size-4 -translate-y-1/2 text-muted-foreground" />
                  <Input
                    value={search}
                    onChange={(e) => setSearch(e.target.value)}
                    placeholder="名称 / 关键词"
                    className="w-full pl-8"
                  />
                </div>
                {hasFilter && (
                  <Button
                    variant="ghost"
                    className="shrink-0 cursor-pointer px-2 lg:px-3"
                    onClick={resetFilters}
                  >
                    重置
                    <X />
                  </Button>
                )}
                <Button
                  onClick={() => {
                    setEditingTask(null);
                    setFormOpen(true);
                  }}
                  className="shrink-0 cursor-pointer"
                >
                  <Plus />
                  新建任务
                </Button>
              </div>
            </div>

            {/* 平台筛选条:横向铺开所有平台;不选即全部,点击已选 chip 可取消 */}
            <div className="mt-2 flex flex-wrap items-center gap-1.5">
              {platforms.map((p) => (
                <PlatformChip
                  key={p.id}
                  label={p.name}
                  active={platformFilter === p.id}
                  onClick={() =>
                    setPlatformFilter((prev) => (prev === p.id ? "" : p.id))
                  }
                />
              ))}
            </div>

            <TabsContent value="active" className="mt-2 flex min-h-0 flex-1 flex-col">
              <DataTable
                columns={activeColumns}
                data={filtered}
                itemLabel="任务"
                getRowId={(t) => t.id}
                emptyState={
                  <EmptyHint text="暂无任务,点击右上角「新建任务」开始" />
                }
              />
            </TabsContent>
            <TabsContent value="quick" className="mt-2 flex min-h-0 flex-1 flex-col">
              <DataTable
                columns={activeColumns}
                data={filtered}
                itemLabel="任务"
                getRowId={(t) => t.id}
                emptyState={
                  <EmptyHint text="暂无快速任务,新建任务选「立即一次」即可" />
                }
              />
            </TabsContent>
            <TabsContent value="scheduled" className="mt-2 flex min-h-0 flex-1 flex-col">
              <DataTable
                columns={activeColumns}
                data={filtered}
                itemLabel="任务"
                getRowId={(t) => t.id}
                emptyState={
                  <EmptyHint text="暂无定时任务,新建任务选「每日定时」即可排入队列" />
                }
              />
            </TabsContent>
            <TabsContent value="archive" className="mt-2 flex min-h-0 flex-1 flex-col">
              <DataTable
                columns={archiveColumns}
                data={filtered}
                itemLabel="归档任务"
                getRowId={(t) => t.id}
                emptyState={<EmptyHint text="暂无归档任务" />}
              />
            </TabsContent>
          </Tabs>
        </div>
      </div>

      <TaskFormSheet
        key={formOpen ? (editingTask?.id ?? "new") : "idle"}
        open={formOpen}
        initial={editingTask}
        industries={industries}
        platforms={platforms}
        onOpenChange={(v) => {
          setFormOpen(v);
          if (!v) setEditingTask(null);
        }}
        onSubmit={handleSaveTask}
      />
    </div>
  );
}

// ---- 左侧 行业筛选(可折叠) ----

function IndustryFilterSidebar({
  industries,
  onCollapse,
  selected,
  onSelect,
  counts,
}: {
  industries: IndustryView[];
  onCollapse: () => void;
  selected: string;
  onSelect: (v: string) => void;
  counts: Record<string, number>;
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
          count={counts.__all ?? 0}
          active={selected === "__all"}
          onClick={() => onSelect("__all")}
        />
        {industries.map((ind) => (
          <IndustryFilterItem
            key={ind.id}
            label={ind.name}
            count={counts[ind.name] ?? 0}
            active={selected === ind.name}
            onClick={() => onSelect(ind.name)}
          />
        ))}
      </div>
    </div>
  );
}

// 平台筛选 chip:扁平按钮,active 用主题色填充
function PlatformChip({
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


// ---- 新建 / 编辑 任务 Sheet ----

function TaskFormSheet({
  open,
  initial,
  industries,
  platforms,
  onOpenChange,
  onSubmit,
}: {
  open: boolean;
  initial: TaskItem | null;
  industries: IndustryView[];
  platforms: PlatformConfig[];
  onOpenChange: (v: boolean) => void;
  onSubmit: (input: TaskInput) => void;
}) {
  const [name, setName] = useState(initial?.name ?? "");
  const [industry, setIndustry] = useState(
    initial?.industry ?? industries[0]?.name ?? "",
  );
  const [platform, setPlatform] = useState(
    initial?.platform ?? platforms[0]?.id ?? "",
  );
  const [keywordsRaw, setKeywordsRaw] = useState(
    initial?.keywords.join("\n") ?? "",
  );

  // 行业变更时自动从该行业拉关键词填充(可编辑);编辑模式下首次保持原值不覆盖
  const initialIndustryName = initial?.industry ?? "";
  useEffect(() => {
    // 编辑态首次进入 = 用户没主动切换行业,不动 textarea
    if (industry === initialIndustryName && keywordsRaw) return;
    const ind = industries.find((i) => i.name === industry);
    if (!ind) return;
    api
      .listKeywords(ind.id)
      .then((rows) => setKeywordsRaw(rows.map((r) => r.word).join("\n")))
      .catch(() => {});
    // 这里有意只跟 industry 变化,不依赖 keywordsRaw,避免循环
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [industry, industries]);
  const [trigger, setTrigger] = useState<TaskTrigger>(
    initial?.trigger ?? "once-now",
  );
  const [scheduledAt, setScheduledAt] = useState<string>(
    initial?.scheduledAt ?? "",
  );
  const [watchIntervalMin, setWatchIntervalMin] = useState<string>(
    String(initial?.watchIntervalMin ?? 30),
  );

  // 采集策略字段
  const [sortMode, setSortMode] = useState<SortMode>(
    initial?.sortMode ?? DEFAULT_STRATEGY.sortMode,
  );
  const [timeRange, setTimeRange] = useState<TimeRange>(
    initial?.timeRange ?? DEFAULT_STRATEGY.timeRange,
  );
  const [perKeywordLimit, setPerKeywordLimit] = useState(
    String(initial?.perKeywordLimit ?? DEFAULT_STRATEGY.perKeywordLimit),
  );
  const [minLikes, setMinLikes] = useState(
    String(initial?.minLikes ?? DEFAULT_STRATEGY.minLikes),
  );
  const [aiExtract, setAiExtract] = useState(
    initial?.aiExtract ?? DEFAULT_STRATEGY.aiExtract,
  );

  function handleSubmit(e: FormEvent) {
    e.preventDefault();
    const trimmedName = name.trim();
    if (!trimmedName) {
      toast.error("请输入任务名");
      return;
    }
    // 多行关键词:严格按行分隔,每行 trim 后取非空
    const keywords = keywordsRaw
      .split(/\r?\n/)
      .map((k) => k.trim())
      .filter(Boolean);
    if (keywords.length === 0) {
      toast.error("请至少输入一个关键词");
      return;
    }
    const input: TaskInput = {
      id: initial?.id ?? crypto.randomUUID(),
      name: trimmedName,
      industry,
      platform,
      keywords,
      trigger,
      scheduledAt: trigger === "daily" && scheduledAt ? scheduledAt : null,
      watchIntervalMin:
        trigger === "watching"
          ? Math.max(1, Number(watchIntervalMin) || 30)
          : null,
      sortMode,
      timeRange,
      perKeywordLimit: Math.max(1, Number(perKeywordLimit) || 50),
      minLikes: Math.max(0, Number(minLikes) || 0),
      aiExtract,
    };
    onSubmit(input);
  }

  return (
    <Sheet open={open} onOpenChange={onOpenChange}>
      <SheetContent className="flex w-full flex-col gap-0 sm:max-w-md">
        <SheetHeader>
          <SheetTitle>{initial ? "编辑任务" : "新建采集任务"}</SheetTitle>
          <SheetDescription>
            配置采集目标 + 触发方式;保存后任务进入「等待运行」,可手动启动。
          </SheetDescription>
        </SheetHeader>

        <form
          id="task-form"
          onSubmit={handleSubmit}
          className="flex-1 space-y-4 overflow-y-auto px-4 py-2"
        >
          <div className="space-y-1.5">
            <Label htmlFor="task-name">任务名</Label>
            <Input
              id="task-name"
              value={name}
              onChange={(e) => setName(e.target.value)}
              placeholder="例:小红书 · 母婴关键词监控"
            />
          </div>
          <div className="space-y-1.5">
            <Label htmlFor="task-industry">所属行业</Label>
            <Select value={industry} onValueChange={setIndustry}>
              <SelectTrigger id="task-industry" className="w-full">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                {industries.map((ind) => (
                  <SelectItem key={ind.id} value={ind.name}>
                    {ind.name}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          </div>
          <div className="space-y-1.5">
            <Label htmlFor="task-platform">所属平台</Label>
            <Select value={platform} onValueChange={setPlatform}>
              <SelectTrigger id="task-platform" className="w-full">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                {platforms.map((p) => (
                  <SelectItem key={p.id} value={p.id}>
                    {p.name}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          </div>
          <div className="space-y-1.5">
            <div className="flex items-center justify-between">
              <Label htmlFor="task-keywords">关键词</Label>
              <span className="text-xs text-muted-foreground">
                多个关键词每行一个
              </span>
            </div>
            <Textarea
              id="task-keywords"
              rows={10}
              value={keywordsRaw}
              onChange={(e) => setKeywordsRaw(e.target.value)}
              placeholder={"婴儿辅食\n宝宝湿疹\n孕妇维生素"}
              className="min-h-48 font-mono text-sm"
            />
          </div>
          {/* 采集策略 */}
          <div className="grid grid-cols-2 gap-3">
            <div className="space-y-1.5">
              <Label htmlFor="task-sort">排序方式</Label>
              <Select
                value={sortMode}
                onValueChange={(v) => setSortMode(v as SortMode)}
              >
                <SelectTrigger id="task-sort" className="w-full">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  {(Object.keys(SORT_MODE_META) as SortMode[]).map((k) => (
                    <SelectItem key={k} value={k}>
                      {SORT_MODE_META[k].label}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
            </div>
            <div className="space-y-1.5">
              <Label htmlFor="task-time">发布时间</Label>
              <Select
                value={timeRange}
                onValueChange={(v) => setTimeRange(v as TimeRange)}
              >
                <SelectTrigger id="task-time" className="w-full">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  {(Object.keys(TIME_RANGE_META) as TimeRange[]).map((k) => (
                    <SelectItem key={k} value={k}>
                      {TIME_RANGE_META[k].label}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
            </div>
          </div>
          <div className="grid grid-cols-2 gap-3">
            <div className="space-y-1.5">
              <Label htmlFor="task-limit">每组返回数量</Label>
              <Input
                id="task-limit"
                type="number"
                min={1}
                value={perKeywordLimit}
                onChange={(e) => setPerKeywordLimit(e.target.value)}
              />
              <p className="text-xs text-muted-foreground">
                每个关键词最多返回的条数
              </p>
            </div>
            <div className="space-y-1.5">
              <Label htmlFor="task-min-likes">最低点赞数</Label>
              <Input
                id="task-min-likes"
                type="number"
                min={0}
                value={minLikes}
                onChange={(e) => setMinLikes(e.target.value)}
              />
              <p className="text-xs text-muted-foreground">
                低于该值的内容直接抛弃
              </p>
            </div>
          </div>
          <div className="flex items-center justify-between rounded-md border px-3 py-2.5">
            <div className="space-y-0.5">
              <Label htmlFor="task-ai-extract" className="cursor-pointer">
                AI 文案提取
              </Label>
              <p className="text-xs text-muted-foreground">
                对采集到的图片 / 视频自动提取文案
              </p>
            </div>
            <Switch
              id="task-ai-extract"
              checked={aiExtract}
              onCheckedChange={setAiExtract}
            />
          </div>

          <div className="space-y-1.5">
            <Label>触发方式</Label>
            <div className="grid grid-cols-3 gap-2">
              {(Object.keys(TRIGGER_META) as TaskTrigger[]).map((k) => {
                const m = TRIGGER_META[k];
                const Icon = m.icon;
                const active = trigger === k;
                return (
                  <button
                    type="button"
                    key={k}
                    onClick={() => setTrigger(k)}
                    className={`cursor-pointer rounded-md border px-2 py-2 text-xs transition-colors ${
                      active
                        ? "border-primary bg-primary/10 text-primary"
                        : "hover:bg-accent"
                    }`}
                  >
                    <Icon className="mx-auto mb-1 size-4" />
                    {m.label}
                  </button>
                );
              })}
            </div>
          </div>
          {trigger === "daily" && (
            <div className="space-y-1.5">
              <Label>每日执行时间</Label>
              {(() => {
                const [hh = "", mm = ""] = scheduledAt.split(":");
                const pad = (n: number) =>
                  String(Math.max(0, Math.min(59, n))).padStart(2, "0");
                return (
                  <div className="flex items-center gap-2">
                    <Input
                      type="number"
                      min={0}
                      max={23}
                      placeholder="HH"
                      value={hh}
                      onChange={(e) => {
                        const n = Number(e.target.value);
                        const next = Number.isNaN(n)
                          ? ""
                          : String(Math.max(0, Math.min(23, n))).padStart(
                              2,
                              "0",
                            );
                        setScheduledAt(`${next}:${mm || "00"}`);
                      }}
                      className="w-20 text-center"
                    />
                    <span className="text-muted-foreground">:</span>
                    <Input
                      type="number"
                      min={0}
                      max={59}
                      placeholder="MM"
                      value={mm}
                      onChange={(e) => {
                        const n = Number(e.target.value);
                        const next = Number.isNaN(n) ? "" : pad(n);
                        setScheduledAt(`${hh || "00"}:${next}`);
                      }}
                      className="w-20 text-center"
                    />
                  </div>
                );
              })()}
              <p className="text-xs text-muted-foreground">
                每天到达该时间点时自动启动一次(24 小时制)
              </p>
            </div>
          )}

          {trigger === "watching" && (
            <>
              <div className="space-y-1.5">
                <Label htmlFor="task-watch-interval">轮询间隔(分钟)</Label>
                <Input
                  id="task-watch-interval"
                  type="number"
                  min={1}
                  value={watchIntervalMin}
                  onChange={(e) => setWatchIntervalMin(e.target.value)}
                  className="w-40"
                />
                <p className="text-xs text-muted-foreground">
                  每过 {watchIntervalMin || "?"} 分钟检查一次是否有新内容
                </p>
              </div>
              <div className="rounded-md border border-sky-500/30 bg-sky-500/10 px-3 py-2 text-xs text-sky-700 dark:text-sky-400">
                <CircleSlash2 className="-mt-0.5 mr-1 inline size-3" />
                持续监听任务运行后不会主动结束,需要手动停止。
              </div>
            </>
          )}
        </form>

        <SheetFooter>
          <Button
            type="button"
            variant="outline"
            onClick={() => onOpenChange(false)}
            className="cursor-pointer"
          >
            取消
          </Button>
          <Button type="submit" form="task-form" className="cursor-pointer">
            {initial ? "保存修改" : "创建任务"}
          </Button>
        </SheetFooter>
      </SheetContent>
    </Sheet>
  );
}

// ---- 空状态提示 + 单行操作按钮组(含归档/删除确认) ----

function EmptyHint({ text }: { text: string }) {
  return (
    <div className="flex flex-col items-center justify-center gap-2 py-12 text-center">
      <Radar className="size-8 opacity-30" />
      <p className="text-sm text-muted-foreground">{text}</p>
    </div>
  );
}

function TaskActionsCell({
  task,
  isArchive,
  onUpdate,
  onRun,
  onDelete,
  onEdit,
  onDetail,
}: {
  task: TaskItem;
  isArchive: boolean;
  onUpdate: (id: string, patch: Partial<TaskItem>) => void;
  onRun: (id: string) => void;
  onDelete: (id: string) => void;
  onEdit: (t: TaskItem) => void;
  onDetail: (t: TaskItem) => void;
}) {
  const [archiveOpen, setArchiveOpen] = useState(false);
  const [deleteOpen, setDeleteOpen] = useState(false);

  return (
    <div className="flex items-center justify-end gap-1">
      {/* 详情:任何状态都在最前,跳转到独立详情页 */}
      <Button
        variant="ghost"
        size="sm"
        className="h-7 cursor-pointer px-2"
        onClick={() => onDetail(task)}
      >
        详情
      </Button>

      {!isArchive && task.trigger !== "watching" && (
        <>
          {task.status === "running" ? (
            <Button
              variant="ghost"
              size="sm"
              className="h-7 cursor-pointer px-2"
              onClick={() => onUpdate(task.id, { status: "paused" })}
            >
              暂停
            </Button>
          ) : (
            <Button
              variant="ghost"
              size="sm"
              className="h-7 cursor-pointer px-2"
              onClick={() => onRun(task.id)}
            >
              启动
            </Button>
          )}
          {/* 已完成任务无「停止」:点停止会改成 cancelled,反而被归档移出列表 */}
          {!isTerminal(task) && (
            <Button
              variant="ghost"
              size="sm"
              className="h-7 cursor-pointer px-2"
              onClick={() =>
                onUpdate(task.id, {
                  status: "cancelled",
                  finishedAt: Math.floor(Date.now() / 1000),
                })
              }
            >
              停止
            </Button>
          )}
        </>
      )}

      {/* 终态任务(完成/失败/已停止)均可复制为新任务,不限归档 tab */}
      {isTerminal(task) && (
        <Button
          variant="ghost"
          size="sm"
          className="h-7 cursor-pointer px-2"
          onClick={() =>
            onEdit({
              ...task,
              id: `t-${Date.now()}`,
              name: `${task.name} (副本)`,
              status: "pending",
              progress: 0,
              contentCount: 0,
              commentCount: 0,
              startedAt: null,
              finishedAt: null,
              errorMessage: null,
              createdAt: Math.floor(Date.now() / 1000),
              updatedAt: Math.floor(Date.now() / 1000),
            })
          }
        >
          复制
        </Button>
      )}

      <DropdownMenu>
        <DropdownMenuTrigger asChild>
          <Button variant="ghost" size="icon-xs" className="cursor-pointer">
            <MoreHorizontal />
          </Button>
        </DropdownMenuTrigger>
        <DropdownMenuContent align="end" className="w-32">
          {!isArchive && (
            <DropdownMenuItem
              className="cursor-pointer"
              onClick={() => onEdit(task)}
            >
              编辑
            </DropdownMenuItem>
          )}
          {!isArchive && !isTerminal(task) && task.trigger === "watching" && (
            <>
              <DropdownMenuItem
                className="cursor-pointer text-amber-600 focus:text-amber-600 dark:text-amber-400"
                onClick={() =>
                  onUpdate(task.id, {
                    status: "cancelled",
                    finishedAt: Math.floor(Date.now() / 1000),
                  })
                }
              >
                终止
              </DropdownMenuItem>
              <DropdownMenuItem
                className="cursor-pointer"
                onClick={() => setArchiveOpen(true)}
              >
                归档
              </DropdownMenuItem>
            </>
          )}
          <DropdownMenuSeparator />
          <DropdownMenuItem
            className="cursor-pointer text-destructive focus:text-destructive"
            onClick={() => setDeleteOpen(true)}
          >
            删除
          </DropdownMenuItem>
        </DropdownMenuContent>
      </DropdownMenu>

      <AlertDialog open={archiveOpen} onOpenChange={setArchiveOpen}>
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>归档任务</AlertDialogTitle>
            <AlertDialogDescription>
              将任务「{task.name}」标记为已完成并移至归档,此操作可在归档中重新复制为新任务。
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel className="cursor-pointer">取消</AlertDialogCancel>
            <AlertDialogAction
              className="cursor-pointer"
              onClick={() => {
                onUpdate(task.id, {
                  status: "completed",
                  finishedAt: Math.floor(Date.now() / 1000),
                });
                setArchiveOpen(false);
              }}
            >
              确认归档
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>

      <AlertDialog open={deleteOpen} onOpenChange={setDeleteOpen}>
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>删除任务</AlertDialogTitle>
            <AlertDialogDescription>
              将永久删除任务「{task.name}」及其采集记录,此操作不可恢复。
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel className="cursor-pointer">取消</AlertDialogCancel>
            <AlertDialogAction
              className="cursor-pointer bg-destructive text-white hover:bg-destructive/90"
              onClick={() => {
                onDelete(task.id);
                setDeleteOpen(false);
              }}
            >
              确认删除
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
    </div>
  );
}
