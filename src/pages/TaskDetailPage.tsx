// 任务详情独立页:顶部任务信息 + 双 tab(子任务列表 / 执行历史)
//
// 数据模型(MVP 前端 mock,后端接入时补 sub_tasks / executions 两张表):
// - 子任务 SubTask:一个关键词对应一个子任务,继承父任务的策略
// - 执行历史 Execution:子任务每次跑生成一条;watching/daily 类任务会有很多条

import { useEffect, useMemo, useState } from "react";
import {
  ArrowLeft,
  CalendarClock,
  Infinity as InfinityIcon,
  Zap,
} from "lucide-react";
import { type ColumnDef } from "@tanstack/react-table";

import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Progress } from "@/components/ui/progress";
import { DataTable } from "@/components/DataTable";
import { DataTableColumnHeader } from "@/components/DataTableColumnHeader";
import {
  Tabs,
  TabsContent,
  TabsList,
  TabsTrigger,
} from "@/components/ui/tabs";
import {
  Sheet,
  SheetContent,
  SheetHeader,
  SheetTitle,
} from "@/components/ui/sheet";
import {
  api,
  type CollectLogEntry,
  type TaskRunView,
  type TaskView,
} from "@/lib/api";
import { platformClass } from "@/lib/platforms";

// 采集日志条目类型见 @/lib/api 的 CollectLogEntry(含富条目 entry:头像/昵称/标题/序号)

const LOG_LEVEL_CLASS: Record<CollectLogEntry["level"], string> = {
  info: "text-foreground",
  warn: "text-amber-600 dark:text-amber-400",
  error: "text-destructive",
};

// 日志时间统一 HH:mm:ss(定宽,便于时间列对齐)
function fmtLogTime(ts: number): string {
  const d = new Date(ts * 1000);
  const p = (n: number) => String(n).padStart(2, "0");
  return `${p(d.getHours())}:${p(d.getMinutes())}:${p(d.getSeconds())}`;
}

// ---- 触发 / 状态 视图 meta(平台见 @/lib/platforms) ----

const TRIGGER_META: Record<
  TaskView["trigger"],
  { label: string; icon: typeof Zap }
> = {
  "once-now": { label: "立即一次", icon: Zap },
  daily: { label: "每日定时", icon: CalendarClock },
  watching: { label: "持续监听", icon: InfinityIcon },
};

const STATUS_META: Record<
  TaskView["status"],
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
  collecting_comments: {
    label: "评论采集中",
    className:
      "border-violet-500/30 bg-violet-500/10 text-violet-600 dark:text-violet-400",
    dot: "bg-violet-500 animate-pulse",
  },
  analyzing_comments: {
    label: "意向分析中",
    className:
      "border-fuchsia-500/30 bg-fuchsia-500/10 text-fuchsia-600 dark:text-fuchsia-400",
    dot: "bg-fuchsia-500 animate-pulse",
  },
  downloading_media: {
    label: "素材下载中",
    className:
      "border-cyan-500/30 bg-cyan-500/10 text-cyan-600 dark:text-cyan-400",
    dot: "bg-cyan-500 animate-pulse",
  },
  completed: {
    label: "已完成",
    className: "border-sky-500/30 bg-sky-500/10 text-sky-600 dark:text-sky-400",
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

// ---- 子任务 / 执行历史 数据模型 ----

interface SubTask {
  id: string;
  keyword: string;
  status: TaskView["status"];
  contentCount: number;
  commentCount: number;
  lastRunAt: number | null;
  errorMessage: string | null;
}

// 从父任务派生子任务列表(每关键词一条);真实接入时替换为 api.listSubTasks
function deriveSubTasks(task: TaskView): SubTask[] {
  return task.keywords.map((kw, idx) => ({
    id: `${task.id}-sub-${idx}`,
    keyword: kw,
    // 简化:把父任务整体状态投射到每个子任务
    status: task.status,
    contentCount: Math.round(task.contentCount / Math.max(1, task.keywords.length)),
    commentCount: Math.round(task.commentCount / Math.max(1, task.keywords.length)),
    lastRunAt: task.startedAt ?? null,
    errorMessage: task.errorMessage,
  }));
}

// ---- 工具 ----

function formatTime(ts?: number | null): string {
  if (!ts) return "—";
  return new Date(ts * 1000).toLocaleString();
}

function formatDuration(start: number, end: number | null): string {
  if (!end) return "进行中";
  const sec = Math.max(0, end - start);
  if (sec < 60) return `${sec}s`;
  const m = Math.floor(sec / 60);
  const s = sec % 60;
  return s ? `${m}m ${s}s` : `${m}m`;
}

// ---- 页面 ----

export function TaskDetailPage({
  task,
  platformName,
  onBack,
}: {
  task: TaskView;
  platformName: (id: string) => string;
  onBack: () => void;
}) {
  const [tab, setTab] = useState<"sub" | "history">("sub");
  const [runs, setRuns] = useState<TaskRunView[]>([]);
  // 查看日志:当前查看的运行 + 其采集日志(右侧抽屉显示)
  const [viewRun, setViewRun] = useState<TaskRunView | null>(null);
  const [runLogs, setRunLogs] = useState<CollectLogEntry[]>([]);

  // 加载执行历史(每次运行一条);任务进行中时 2s 轮询刷新,拿最新运行状态 / 计数
  useEffect(() => {
    let timer: number | undefined;
    let cancelled = false;
    const load = () => {
      api
        .listTaskRuns(task.id)
        .then((rows) => {
          if (!cancelled) setRuns(rows);
        })
        .catch(() => {});
    };
    load();
    const inProgress =
      task.status === "running" ||
      task.status === "paused" ||
      task.status === "collecting_comments" ||
      task.status === "analyzing_comments" ||
      task.status === "downloading_media";
    if (inProgress) timer = window.setInterval(load, 2000);
    return () => {
      cancelled = true;
      if (timer) clearInterval(timer);
    };
  }, [task.id, task.status]);

  // 打开某次运行的采集日志(后端按该运行的时间范围从 collect_logs 切分)
  const openRunLogs = (run: TaskRunView) => {
    setViewRun(run);
    setRunLogs([]);
    api
      .listRunLogs(run.id)
      .then(setRunLogs)
      .catch(() => {});
  };

  const subTasks = useMemo(() => deriveSubTasks(task), [task]);

  const statusMeta = STATUS_META[task.status];
  const TriggerIcon = TRIGGER_META[task.trigger].icon;

  const subColumns = useMemo<ColumnDef<SubTask>[]>(
    () => [
      {
        id: "keyword",
        accessorKey: "keyword",
        header: ({ column }) => (
          <DataTableColumnHeader column={column} title="关键词" />
        ),
        cell: ({ row }) => (
          <span className="font-medium text-foreground">
            {row.original.keyword}
          </span>
        ),
      },
      {
        id: "status",
        accessorKey: "status",
        header: ({ column }) => (
          <DataTableColumnHeader column={column} title="状态" />
        ),
        cell: ({ row }) => {
          const m = STATUS_META[row.original.status];
          return (
            <span
              className={`inline-flex items-center gap-1.5 rounded-md border px-1.5 py-0.5 text-xs font-medium ${m.className}`}
            >
              <span className={`size-1.5 rounded-full ${m.dot}`} />
              {m.label}
            </span>
          );
        },
      },
      {
        id: "collected",
        header: "已采集",
        enableSorting: false,
        cell: ({ row }) => {
          const t = row.original;
          return (
            <div className="text-xs">
              <div>
                内容{" "}
                <span className="font-mono font-medium text-foreground">
                  {t.contentCount.toLocaleString()}
                </span>
              </div>
              <div className="text-muted-foreground">
                评论{" "}
                <span className="font-mono font-medium text-foreground">
                  {t.commentCount.toLocaleString()}
                </span>
              </div>
            </div>
          );
        },
      },
      {
        id: "lastRunAt",
        accessorKey: "lastRunAt",
        header: ({ column }) => (
          <DataTableColumnHeader column={column} title="最近运行" />
        ),
        cell: ({ row }) => (
          <span className="text-xs text-muted-foreground">
            {formatTime(row.original.lastRunAt)}
          </span>
        ),
      },
      {
        id: "actions",
        header: () => <div className="text-right">操作</div>,
        enableSorting: false,
        cell: () => (
          <div className="flex justify-end">
            <Button
              variant="ghost"
              size="sm"
              className="h-7 cursor-pointer px-2"
              onClick={() => setTab("history")}
            >
              查看执行历史
            </Button>
          </div>
        ),
      },
    ],
    [],
  );

  const historyColumns = useMemo<ColumnDef<TaskRunView>[]>(
    () => [
      {
        id: "startedAt",
        accessorKey: "startedAt",
        header: ({ column }) => (
          <DataTableColumnHeader column={column} title="开始时间" />
        ),
        cell: ({ row }) => (
          <span className="text-xs">{formatTime(row.original.startedAt)}</span>
        ),
      },
      {
        id: "duration",
        header: "耗时",
        enableSorting: false,
        cell: ({ row }) => (
          <span className="font-mono text-xs">
            {formatDuration(row.original.startedAt, row.original.finishedAt)}
          </span>
        ),
      },
      {
        id: "result",
        header: "本次新增",
        enableSorting: false,
        cell: ({ row }) => {
          const r = row.original;
          return (
            <div className="text-xs">
              <span className="text-muted-foreground">内容</span>{" "}
              <span className="font-mono font-medium">{r.contentDelta}</span>
              {"  "}
              <span className="text-muted-foreground">评论</span>{" "}
              <span className="font-mono font-medium">{r.commentDelta}</span>
            </div>
          );
        },
      },
      {
        id: "status",
        accessorKey: "status",
        header: "状态",
        enableSorting: false,
        cell: ({ row }) => {
          const map: Record<
            TaskRunView["status"],
            { label: string; className: string }
          > = {
            running: {
              label: "运行中",
              className: "border-emerald-500/30 bg-emerald-500/10 text-emerald-600",
            },
            completed: {
              label: "已完成",
              className: "border-sky-500/30 bg-sky-500/10 text-sky-600",
            },
            failed: {
              label: "失败",
              className: "border-destructive/30 bg-destructive/10 text-destructive",
            },
            cancelled: {
              label: "已停止",
              className: "border-slate-500/30 bg-slate-500/10 text-slate-500",
            },
          };
          const m = map[row.original.status] ?? map.completed;
          return (
            <span
              className={`inline-flex items-center rounded-md border px-1.5 py-0.5 text-xs font-medium ${m.className}`}
            >
              {m.label}
            </span>
          );
        },
      },
      {
        id: "actions",
        header: () => <div className="text-right">操作</div>,
        enableSorting: false,
        cell: ({ row }) => (
          <div className="flex justify-end">
            <Button
              variant="ghost"
              size="sm"
              className="h-7 cursor-pointer px-2"
              onClick={() => openRunLogs(row.original)}
            >
              查看日志
            </Button>
          </div>
        ),
      },
    ],
    // openRunLogs 仅用稳定的 setState/api,闭包捕获首次定义即可
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [],
  );

  return (
    <div className="flex min-h-0 min-w-0 flex-1 flex-col gap-4">
      {/* 返回 + 标题 */}
      <div className="flex items-center gap-2">
        <Button
          variant="ghost"
          size="sm"
          className="cursor-pointer"
          onClick={onBack}
        >
          <ArrowLeft />
          返回
        </Button>
        <h1 className="ml-1 text-xl font-semibold tracking-tight">任务详情</h1>
      </div>

      {/* 顶部任务信息卡 */}
      <div className="rounded-xl border bg-card p-4">
        <div className="flex flex-wrap items-start justify-between gap-3">
          <div className="min-w-0 flex-1 space-y-2">
            <div className="flex items-center gap-2">
              <span
                className={`shrink-0 rounded px-1.5 py-0.5 text-[11px] font-medium ${platformClass(task.platform)}`}
              >
                {platformName(task.platform)}
              </span>
              <h2 className="truncate text-lg font-semibold text-foreground">
                {task.name}
              </h2>
              <span
                className={`inline-flex items-center gap-1.5 rounded-md border px-1.5 py-0.5 text-xs font-medium ${statusMeta.className}`}
              >
                <span className={`size-1.5 rounded-full ${statusMeta.dot}`} />
                {statusMeta.label}
              </span>
            </div>
            <div className="flex flex-wrap items-center gap-x-4 gap-y-1.5 text-xs text-muted-foreground">
              <span>
                行业:
                <span className="ml-1 text-foreground">{task.industry}</span>
              </span>
              <span className="inline-flex items-center gap-1">
                <TriggerIcon className="size-3" />
                {TRIGGER_META[task.trigger].label}
                {task.trigger === "daily" && task.scheduledAt && (
                  <span className="ml-1 font-mono text-foreground">
                    {task.scheduledAt}
                  </span>
                )}
                {task.trigger === "watching" && task.watchIntervalMin && (
                  <span className="ml-1">
                    每 {task.watchIntervalMin} 分钟
                  </span>
                )}
              </span>
              <span>创建于 {formatTime(task.createdAt)}</span>
              {task.startedAt && <span>开始于 {formatTime(task.startedAt)}</span>}
            </div>
          </div>
          {/* 右侧数据卡 */}
          <div className="flex gap-3">
            <div className="rounded-lg border bg-background px-3 py-2 text-center">
              <div className="text-[10px] text-muted-foreground">内容</div>
              <div className="font-mono text-lg font-semibold">
                {task.contentCount.toLocaleString()}
              </div>
            </div>
            <div className="rounded-lg border bg-background px-3 py-2 text-center">
              <div className="text-[10px] text-muted-foreground">评论</div>
              <div className="font-mono text-lg font-semibold">
                {task.commentCount.toLocaleString()}
              </div>
            </div>
            <div className="rounded-lg border bg-background px-3 py-2 text-center">
              <div className="text-[10px] text-muted-foreground">关键词</div>
              <div className="font-mono text-lg font-semibold">
                {task.keywords.length}
              </div>
            </div>
          </div>
        </div>

        {(task.status === "running" || task.status === "paused") && (
          <div className="mt-3 flex items-center gap-2">
            <Progress value={task.progress} className="h-1.5 flex-1" />
            <span className="font-mono text-xs text-muted-foreground">
              {task.progress}%
            </span>
          </div>
        )}

        {task.status === "collecting_comments" && (
          <div className="mt-3 flex items-center gap-2">
            <Progress
              value={
                task.commentVideoTotal > 0
                  ? Math.round(
                      (task.commentVideoDone / task.commentVideoTotal) * 100,
                    )
                  : 0
              }
              className="h-1.5 flex-1"
            />
            <span className="font-mono text-xs text-muted-foreground">
              评论 {task.commentVideoDone}/{task.commentVideoTotal} 视频
            </span>
          </div>
        )}

        {task.status === "downloading_media" && (
          <div className="mt-3 flex items-center gap-2">
            <Progress
              value={
                task.mediaTotal > 0
                  ? Math.round((task.mediaDone / task.mediaTotal) * 100)
                  : 0
              }
              className="h-1.5 flex-1"
            />
            <span className="font-mono text-xs text-muted-foreground">
              素材 {task.mediaDone}/{task.mediaTotal}
            </span>
          </div>
        )}

        {/* 策略参数 */}
        <div className="mt-3 flex flex-wrap gap-1 border-t pt-3 text-[11px] text-muted-foreground">
          <ParamChip label="排序" value={task.sortMode === "synthetic" ? "综合" : task.sortMode === "hottest" ? "最热" : "最新"} />
          <ParamChip
            label="发布"
            value={
              task.timeRange === "any"
                ? "不限"
                : task.timeRange === "1d"
                  ? "一天内"
                  : task.timeRange === "1w"
                    ? "一周内"
                    : "半年内"
            }
          />
          <ParamChip label="每组" value={`≤ ${task.perKeywordLimit}`} />
          <ParamChip label="点赞" value={`≥ ${task.minLikes}`} />
          {task.aiExtract && (
            <span className="rounded bg-primary/10 px-1.5 py-0.5 text-primary">
              AI 文案提取
            </span>
          )}
          {task.collectComments && (
            <span className="rounded bg-violet-500/10 px-1.5 py-0.5 text-violet-600 dark:text-violet-400">
              评论采集
              {task.commentTimeRange && task.commentTimeRange !== "any"
                ? ` · ${
                    task.commentTimeRange === "3d"
                      ? "3天内"
                      : task.commentTimeRange === "7d"
                        ? "7天内"
                        : "14天内"
                  }`
                : ""}
              {task.commentLimit ? ` · ≤${task.commentLimit}` : " · 不限"}
            </span>
          )}
          {task.analyzeCommentIntent && (
            <span className="rounded bg-fuchsia-500/10 px-1.5 py-0.5 text-fuchsia-600 dark:text-fuchsia-400">
              意向分析
            </span>
          )}
          {task.autoSyncObsidian && (
            <span className="rounded bg-sky-500/10 px-1.5 py-0.5 text-sky-600 dark:text-sky-400">
              同步 Obsidian
            </span>
          )}
        </div>

        {task.status === "failed" && task.errorMessage && (
          <div className="mt-3 rounded-md border border-destructive/30 bg-destructive/5 px-3 py-2 text-xs text-destructive">
            {task.errorMessage}
          </div>
        )}
      </div>

      {/* Tabs */}
      <Tabs
        value={tab}
        onValueChange={(v) => setTab(v as typeof tab)}
        className="flex min-h-0 flex-1 flex-col"
      >
        <div className="flex items-center justify-between">
          <TabsList>
            <TabsTrigger value="sub">
              子任务列表
              <Badge variant="secondary" className="ml-1.5">
                {subTasks.length}
              </Badge>
            </TabsTrigger>
            <TabsTrigger value="history">
              执行历史
              <Badge variant="secondary" className="ml-1.5">
                {runs.length}
              </Badge>
            </TabsTrigger>
          </TabsList>
        </div>

        <TabsContent value="sub" className="mt-2 flex min-h-0 flex-1 flex-col">
          <DataTable
            columns={subColumns}
            data={subTasks}
            itemLabel="子任务"
            getRowId={(s) => s.id}
          />
        </TabsContent>
        <TabsContent
          value="history"
          className="mt-2 flex min-h-0 flex-1 flex-col"
        >
          <DataTable
            columns={historyColumns}
            data={runs}
            itemLabel="执行记录"
            getRowId={(r) => r.id}
          />
        </TabsContent>
      </Tabs>

      {/* 查看日志:某次运行的采集日志(后端按该运行时间范围切分) */}
      <Sheet
        open={viewRun !== null}
        onOpenChange={(open) => {
          if (!open) setViewRun(null);
        }}
      >
        <SheetContent className="flex w-full flex-col gap-0 p-0 sm:max-w-2xl">
          <SheetHeader className="border-b">
            <SheetTitle className="flex flex-wrap items-baseline gap-x-2">
              采集日志
              {viewRun && (
                <span className="text-xs font-normal text-muted-foreground">
                  {formatTime(viewRun.startedAt)} · 新增内容 {viewRun.contentDelta} / 评论{" "}
                  {viewRun.commentDelta}
                </span>
              )}
            </SheetTitle>
          </SheetHeader>
          <div className="min-h-0 flex-1 overflow-y-auto p-3 font-mono text-xs">
            {runLogs.length === 0 ? (
              <div className="flex h-full items-center justify-center text-muted-foreground">
                该次运行暂无日志
              </div>
            ) : (
              <div className="space-y-px">
                {runLogs.map((log, i) => (
                  <div
                    key={i}
                    className="flex items-start gap-2 rounded px-1.5 py-0.5 hover:bg-muted/50"
                  >
                    <span className="w-[58px] shrink-0 tabular-nums text-muted-foreground">
                      {fmtLogTime(log.ts)}
                    </span>
                    {log.entry ? (
                      <CollectEntryLine entry={log.entry} />
                    ) : (
                      <span
                        className={`min-w-0 flex-1 break-all ${LOG_LEVEL_CLASS[log.level] ?? "text-foreground"}`}
                      >
                        {log.message}
                      </span>
                    )}
                  </div>
                ))}
              </div>
            )}
          </div>
        </SheetContent>
      </Sheet>
    </div>
  );
}

// 富日志条目行:序号 + 类型徽章 + 头像 + 昵称 + 标题/评论内容(已由后端截断)
function CollectEntryLine({
  entry,
}: {
  entry: NonNullable<CollectLogEntry["entry"]>;
}) {
  const isComment = entry.kind === "comment";
  const typeLabel = isComment
    ? "评论"
    : entry.contentKind === "image"
      ? "图文"
      : entry.contentKind === "video"
        ? "视频"
        : "内容";
  const typeClass = isComment
    ? "bg-violet-500/15 text-violet-600 dark:text-violet-400"
    : "bg-sky-500/15 text-sky-600 dark:text-sky-400";
  return (
    <span className="flex min-w-0 flex-1 items-center gap-1.5">
      <span className="shrink-0 font-mono text-muted-foreground">
        #{entry.seq}
      </span>
      <span className={`shrink-0 rounded px-1 text-[10px] ${typeClass}`}>
        {typeLabel}
      </span>
      {entry.avatar ? (
        <img
          src={entry.avatar}
          alt=""
          referrerPolicy="no-referrer"
          className="size-4 shrink-0 rounded-full object-cover"
          onError={(e) => {
            (e.currentTarget as HTMLImageElement).style.display = "none";
          }}
        />
      ) : null}
      <span className="shrink-0 font-medium text-foreground">
        {entry.nickname || "匿名"}
      </span>
      <span className="truncate text-muted-foreground">
        {isComment ? `：${entry.title}` : `— ${entry.title}`}
      </span>
    </span>
  );
}

function ParamChip({ label, value }: { label: string; value: string }) {
  return (
    <span className="rounded bg-muted px-1.5 py-0.5">
      <span className="text-muted-foreground">{label}</span>
      <span className="ml-1 text-foreground">{value}</span>
    </span>
  );
}
