// 任务详情独立页:顶部任务信息 + 双 tab(子任务列表 / 执行历史)
//
// 数据模型(MVP 前端 mock,后端接入时补 sub_tasks / executions 两张表):
// - 子任务 SubTask:一个关键词对应一个子任务,继承父任务的策略
// - 执行历史 Execution:子任务每次跑生成一条;watching/daily 类任务会有很多条

import { useEffect, useMemo, useRef, useState } from "react";
import {
  ArrowLeft,
  CalendarClock,
  Filter,
  Infinity as InfinityIcon,
  Zap,
} from "lucide-react";
import { type ColumnDef } from "@tanstack/react-table";
import { listen } from "@tauri-apps/api/event";

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
import { type TaskView } from "@/lib/api";
import { platformClass } from "@/lib/platforms";

// 后端 collect-log 事件载荷(对应 src-tauri webview::CollectLog)
interface CollectLogEntry {
  taskId: string;
  ts: number;
  level: "info" | "warn" | "error";
  message: string;
}

// 单次采集会话日志上限,防止长任务把内存撑爆
const LOG_BUFFER_LIMIT = 500;

const LOG_LEVEL_CLASS: Record<CollectLogEntry["level"], string> = {
  info: "text-foreground",
  warn: "text-amber-600 dark:text-amber-400",
  error: "text-destructive",
};

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

interface Execution {
  id: string;
  subTaskId: string;
  keyword: string;
  startedAt: number;
  finishedAt: number | null;
  /// 本次新增内容数
  contentDelta: number;
  /// 本次新增评论数
  commentDelta: number;
  status: "running" | "success" | "failed" | "cancelled";
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

// 生成 mock 执行历史(每个子任务最近 3 次运行)
function deriveExecutions(subTasks: SubTask[]): Execution[] {
  const list: Execution[] = [];
  for (const sub of subTasks) {
    if (!sub.lastRunAt) continue;
    for (let i = 0; i < 3; i += 1) {
      const startedAt = sub.lastRunAt - i * 3600;
      const finishedAt = startedAt + 60 * (5 + Math.floor(Math.random() * 25));
      list.push({
        id: `${sub.id}-exec-${i}`,
        subTaskId: sub.id,
        keyword: sub.keyword,
        startedAt,
        finishedAt,
        contentDelta: Math.floor(Math.random() * 30),
        commentDelta: Math.floor(Math.random() * 80),
        status: i === 0 && sub.status === "failed" ? "failed" : "success",
        errorMessage: i === 0 ? sub.errorMessage : null,
      });
    }
  }
  // 最近的排前面
  return list.sort((a, b) => b.startedAt - a.startedAt);
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
  const [tab, setTab] = useState<"sub" | "history" | "logs">("sub");
  const [subFilterId, setSubFilterId] = useState<string | null>(null);
  const [logs, setLogs] = useState<CollectLogEntry[]>([]);
  const logEndRef = useRef<HTMLDivElement>(null);

  // 订阅后端采集日志,只收本任务的;卸载时取消监听避免泄漏
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    let cancelled = false;
    listen<CollectLogEntry>("collect-log", (event) => {
      if (event.payload.taskId !== task.id) return;
      setLogs((prev) => {
        const next = [...prev, event.payload];
        return next.length > LOG_BUFFER_LIMIT
          ? next.slice(-LOG_BUFFER_LIMIT)
          : next;
      });
    })
      .then((fn) => {
        // 订阅就绪前组件已卸载则立即退订
        if (cancelled) fn();
        else unlisten = fn;
      })
      .catch(() => {});
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, [task.id]);

  // 新日志到达时滚到底
  useEffect(() => {
    if (tab === "logs") logEndRef.current?.scrollIntoView({ behavior: "smooth" });
  }, [logs, tab]);

  const subTasks = useMemo(() => deriveSubTasks(task), [task]);
  const executions = useMemo(() => deriveExecutions(subTasks), [subTasks]);
  const filteredExecutions = useMemo(
    () =>
      subFilterId
        ? executions.filter((e) => e.subTaskId === subFilterId)
        : executions,
    [executions, subFilterId],
  );

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
        cell: ({ row }) => (
          <div className="flex justify-end">
            <Button
              variant="ghost"
              size="sm"
              className="h-7 cursor-pointer px-2"
              onClick={() => {
                setSubFilterId(row.original.id);
                setTab("history");
              }}
            >
              查看执行历史
            </Button>
          </div>
        ),
      },
    ],
    [],
  );

  const historyColumns = useMemo<ColumnDef<Execution>[]>(
    () => [
      {
        id: "keyword",
        accessorKey: "keyword",
        header: "关键词",
        enableSorting: false,
        cell: ({ row }) => row.original.keyword,
      },
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
        header: "本次结果",
        enableSorting: false,
        cell: ({ row }) => {
          const e = row.original;
          return (
            <div className="text-xs">
              <span className="text-muted-foreground">+内容</span>{" "}
              <span className="font-mono font-medium">{e.contentDelta}</span>
              {"  "}
              <span className="text-muted-foreground">+评论</span>{" "}
              <span className="font-mono font-medium">{e.commentDelta}</span>
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
            Execution["status"],
            { label: string; className: string }
          > = {
            running: {
              label: "运行中",
              className: "border-emerald-500/30 bg-emerald-500/10 text-emerald-600",
            },
            success: {
              label: "成功",
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
          const m = map[row.original.status];
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
        id: "error",
        header: "错误",
        enableSorting: false,
        cell: ({ row }) =>
          row.original.errorMessage ? (
            <span className="text-xs text-destructive">
              {row.original.errorMessage}
            </span>
          ) : (
            <span className="text-xs text-muted-foreground">—</span>
          ),
      },
    ],
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
                {filteredExecutions.length}
              </Badge>
            </TabsTrigger>
            <TabsTrigger value="logs">
              采集日志
              {logs.length > 0 && (
                <Badge variant="secondary" className="ml-1.5">
                  {logs.length}
                </Badge>
              )}
            </TabsTrigger>
          </TabsList>
          {tab === "history" && subFilterId && (
            <Button
              variant="outline"
              size="sm"
              className="cursor-pointer"
              onClick={() => setSubFilterId(null)}
            >
              <Filter className="size-3.5" />
              清除筛选
            </Button>
          )}
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
            data={filteredExecutions}
            itemLabel="执行记录"
            getRowId={(e) => e.id}
          />
        </TabsContent>
        <TabsContent value="logs" className="mt-2 flex min-h-0 flex-1 flex-col">
          <div className="min-h-0 flex-1 overflow-y-auto rounded-lg border bg-muted/30 p-3 font-mono text-xs">
            {logs.length === 0 ? (
              <div className="flex h-full items-center justify-center text-muted-foreground">
                暂无采集日志,启动采集后实时显示
              </div>
            ) : (
              <div className="space-y-0.5">
                {logs.map((log, i) => (
                  <div key={i} className="flex gap-2">
                    <span className="shrink-0 text-muted-foreground">
                      {new Date(log.ts * 1000).toLocaleTimeString()}
                    </span>
                    <span className={LOG_LEVEL_CLASS[log.level] ?? "text-foreground"}>
                      {log.message}
                    </span>
                  </div>
                ))}
                <div ref={logEndRef} />
              </div>
            )}
          </div>
        </TabsContent>
      </Tabs>
    </div>
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
