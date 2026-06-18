// 任务调度页:任务列表 + 任务归档双 tab。MVP 阶段纯前端 mock,后续接 Tauri commands。
//
// 设计要点:
// - 任务支持三种触发:立即一次 / 定时一次 / 持续监听(增量)
// - 列表 tab 显示 pending/running/paused;归档 tab 显示 completed/failed/cancelled
// - 操作按钮按状态动态:running → 暂停/停止,paused → 启动/停止,pending → 启动/停止

import { useEffect, useMemo, useState } from "react";
import { useResponsiveCollapse } from "@/hooks/use-responsive-collapse";
import { api } from "@/lib/api";
import type { IndustryView, PlatformConfig, TaskInput, TaskView } from "@/lib/api";
import { sortLabelOf, timeLabelOf, COMMENT_TIME_RANGE_META, isTerminal, isInProgress, isInWatchingList, isInQuickList, isInScheduledQueue, isInWatchingTasks, nextRunTs, formatCountdown, STATUS_META, KEYWORD_STATE_META, keywordRowStates, keywordRowProgress, TRIGGER_META, formatTime } from "./collect-meta";
import type { TaskItem, PlatformOption } from "./collect-meta";
import { TaskDetailPage } from "@/pages/TaskDetailPage";
import { listen } from "@tauri-apps/api/event";
import { platformClass, platformChipClass } from "@/lib/platforms";
import { FORM_CONTROL_SIZING } from "@/lib/form-sizing";
import type { ColumnDef } from "@tanstack/react-table";
import { Archive, ChevronLeft, Eye, Filter, MoreHorizontal, SquarePen, Play, Plus, Radar, RotateCcw, Search, Square, Trash2, Wrench, X } from "lucide-react";
import { toast } from "sonner";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Progress } from "@/components/ui/progress";
import { DataTable } from "@/components/DataTable";
import { DataTableColumnHeader } from "@/components/DataTableColumnHeader";
import { AlertDialog, AlertDialogAction, AlertDialogCancel, AlertDialogContent, AlertDialogDescription, AlertDialogFooter, AlertDialogHeader, AlertDialogTitle } from "@/components/ui/alert-dialog";
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuSeparator, DropdownMenuTrigger } from "@/components/ui/dropdown-menu";
import { SimpleTooltip } from "@/components/SimpleTooltip";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { TaskFormSheet } from "./TaskFormSheet";

// ---- 数据模型(沿用后端 TaskView,本地 alias 为 TaskItem 方便引用) ----

function CountdownCell({ t }: { t: TaskItem }) {
  // 仅用于驱动每秒重渲染,值本身不消费
  const [, setTick] = useState(0);
  useEffect(() => {
    const id = setInterval(() => setTick((x) => x + 1), 1000);
    return () => clearInterval(id);
  }, []);

  const plan =
    t.trigger === "daily" ? (
      <>
        每日 <span className="font-mono">{t.scheduledAt}</span>
      </>
    ) : (
      <>
        每 <span className="font-mono">{t.watchIntervalMin}</span> 分
      </>
    );

  if (isInProgress(t)) {
    return (
      <span className="text-xs text-muted-foreground">
        {plan}
        <span className="ml-1.5 text-emerald-600 dark:text-emerald-400">
          运行中
        </span>
      </span>
    );
  }
  if (t.trigger === "watching" && t.status === "cancelled") {
    return (
      <span className="text-xs text-muted-foreground">
        {plan}
        <span className="ml-1.5">已停止 · 不再自动监听</span>
      </span>
    );
  }
  const next = nextRunTs(t);
  if (next == null) {
    return (
      <span className="text-xs text-muted-foreground">
        {plan}
        {t.trigger === "watching" && (
          <span className="ml-1.5">待首次运行</span>
        )}
      </span>
    );
  }
  const remain = next - Math.floor(Date.now() / 1000);
  // 计划与倒计时分两行:倒计时换行显示,状态列可更窄
  return (
    <span className="flex flex-col text-xs text-muted-foreground">
      <span>{plan}</span>
      <span
        className={`font-mono ${
          remain <= 0
            ? "text-emerald-600 dark:text-emerald-400"
            : "text-sky-600 dark:text-sky-400"
        }`}
      >
        {formatCountdown(remain)}
      </span>
    </span>
  );
}


export function CollectPage() {
  const [tasks, setTasks] = useState<TaskItem[]>([]);
  const [industries, setIndustries] = useState<IndustryView[]>([]);
  const [platforms, setPlatforms] = useState<PlatformConfig[]>([]);
  // 任务表单用:启用平台 + 各自账号统计
  const [platformOptions, setPlatformOptions] = useState<PlatformOption[]>([]);

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
    api
      .listPlatforms()
      .then(async (list) => {
        setPlatforms(list);
        // 任务表单只用启用平台,并统计各平台绑定 / 有效(登录)账号数
        const enabled = list.filter((p) => p.enabled);
        const opts = await Promise.all(
          enabled.map((p) =>
            api
              .listAccounts(p.id)
              .then((accs) => ({
                ...p,
                total: accs.length,
                active: accs.filter((a) => a.status === "active").length,
              }))
              .catch(() => ({ ...p, total: 0, active: 0 })),
          ),
        );
        setPlatformOptions(opts);
      })
      .catch(() => {});
  }, []);

  // 采集 + 素材下载都在后端异步进行,任一处于进行态(running / downloading_media)就轮询刷新进度。
  // 注意 downloading_media 也要纳入,否则采集结束转入素材下载后轮询会停,进度卡住不动。
  const hasActiveTask = tasks.some(
    (t) =>
      t.status === "running" ||
      t.status === "collecting_comments" ||
      t.status === "analyzing_comments" ||
      t.status === "downloading_media",
  );
  useEffect(() => {
    if (!hasActiveTask) return;
    const timer = setInterval(reload, 2000);
    return () => clearInterval(timer);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [hasActiveTask]);

  // 进度事件:后端每次进度/状态变更即时推送最新任务视图,就地更新对应行(免等轮询)。
  // 上面的轮询保留作兜底,补偿偶发漏收的事件;两路都更新同一行,以事件为主、轮询纠偏。
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    let cancelled = false;
    listen<TaskView>("task-progress", (event) => {
      const view = event.payload;
      setTasks((prev) =>
        prev.map((t) =>
          t.id === view.id
            ? {
                ...view,
                // 事件推送不含关键词统计与累计总量,沿用上次轮询值,避免实时刷新把它们刷空 / 刷 0
                keywordStats: view.keywordStats?.length
                  ? view.keywordStats
                  : t.keywordStats,
                totalContents: view.totalContents || t.totalContents,
                totalComments: view.totalComments || t.totalComments,
              }
            : t,
        ),
      );
    })
      .then((fn) => {
        if (cancelled) fn();
        else unlisten = fn;
      })
      .catch(() => {});
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, []);
  const [tab, setTab] = useState<
    "active" | "quick" | "scheduled" | "watching" | "archive"
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
      if (tab === "watching") return isInWatchingTasks(t);
      return t.archived;
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
      if (tab === "watching") return isInWatchingTasks(t);
      return t.archived;
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
      watching: tasks.filter(isInWatchingTasks).length,
      archive: tasks.filter((t) => t.archived).length,
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
        archived: patch.archived ?? null,
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
        id: "run-control",
        header: "执行",
        enableSorting: false,
        cell: ({ row }) => {
          const t = row.original;
          // 已归档:显示「恢复」,点击移回任务列表
          if (t.archived) {
            return (
              <SimpleTooltip content="恢复到任务列表">
                <Button
                  variant="ghost"
                  size="icon"
                  className="cursor-pointer text-sky-600 hover:text-sky-600 dark:text-sky-400"
                  onClick={() =>
                    updateTask(t.id, { status: t.status, archived: false })
                  }
                >
                  <RotateCcw className="size-5" />
                </Button>
              </SimpleTooltip>
            );
          }
          // 进行中显示「终止」,其余显示「开始 / 重新运行」
          if (isInProgress(t)) {
            return (
              <SimpleTooltip content="终止">
                <Button
                  variant="ghost"
                  size="icon"
                  className="cursor-pointer text-amber-600 hover:text-amber-600 dark:text-amber-400"
                  onClick={() =>
                    updateTask(t.id, {
                      status: "cancelled",
                      finishedAt: Math.floor(Date.now() / 1000),
                    })
                  }
                >
                  <Square className="size-5" />
                </Button>
              </SimpleTooltip>
            );
          }
          return (
            <SimpleTooltip content={isTerminal(t) ? "重新运行" : "开始"}>
              <Button
                variant="ghost"
                size="icon"
                className="cursor-pointer text-emerald-600 hover:text-emerald-600 dark:text-emerald-400"
                onClick={() => runTask(t.id)}
              >
                <Play className="size-5" />
              </Button>
            </SimpleTooltip>
          );
        },
      },
      {
        id: "name",
        accessorKey: "name",
        header: ({ column }) => (
          <DataTableColumnHeader column={column} title="任务名" />
        ),
        cell: ({ row }) => (
          <span className="block min-w-[160px] max-w-[260px] truncate font-medium text-foreground">
            {row.original.name}
          </span>
        ),
      },
      {
        id: "collect-params",
        header: "采集参数",
        enableSorting: false,
        cell: ({ row }) => {
          const t = row.original;
          const TriggerIcon = TRIGGER_META[t.trigger].icon;
          // AI文案 / 评论 / 意向分析:涉及到(开启)时才单独占一行
          const hasSwitches =
            t.aiExtract || t.collectComments || t.analyzeCommentIntent;
          return (
            <div className="flex max-w-[240px] flex-col gap-1.5">
              {/* 第一行:平台 + 触发方式 */}
              <div className="flex flex-wrap items-center gap-1.5">
                <span
                  className={`inline-block w-20 truncate rounded px-1.5 py-0.5 text-center text-[11px] font-medium ${platformClass(t.platform)}`}
                >
                  {platformName(t.platform)}
                </span>
                <span className="inline-flex items-center gap-0.5 rounded bg-muted px-1.5 py-0.5 text-[11px] text-muted-foreground">
                  <TriggerIcon className="size-3" />
                  {TRIGGER_META[t.trigger].label}
                </span>
              </div>
              {/* 第二行:基础采集策略(排序 / 时间 / 目标数 / 最低赞) */}
              <div className="flex flex-wrap items-center gap-1 text-[11px] text-muted-foreground">
                <span className="rounded bg-muted px-1.5 py-0.5">
                  {sortLabelOf(t.platform, t.sortMode)}
                </span>
                <span className="rounded bg-muted px-1.5 py-0.5">
                  {timeLabelOf(t.platform, t.timeRange)}
                </span>
                <span className="rounded bg-muted px-1.5 py-0.5">
                  ≤ {t.perKeywordLimit}
                </span>
                {t.minLikes > 0 && (
                  <span className="rounded bg-muted px-1.5 py-0.5">
                    ≥{t.minLikes}赞
                  </span>
                )}
              </div>
              {/* 第三行:AI文案 / 评论 / 意向分析(开启时单独成行) */}
              {hasSwitches && (
                <div className="flex flex-wrap items-center gap-1 text-[11px]">
                  {t.aiExtract && (
                    <span className="rounded bg-primary/10 px-1.5 py-0.5 text-primary">
                      AI文案
                    </span>
                  )}
                  {t.collectComments && (
                    <span className="rounded bg-violet-500/10 px-1.5 py-0.5 text-violet-600 dark:text-violet-400">
                      评论
                      {t.commentTimeRange && t.commentTimeRange !== "any"
                        ? ` ${COMMENT_TIME_RANGE_META[t.commentTimeRange].label}`
                        : ""}
                      {t.commentLimit ? ` ≤${t.commentLimit}` : ""}
                    </span>
                  )}
                  {t.analyzeCommentIntent && (
                    <span className="rounded bg-fuchsia-500/10 px-1.5 py-0.5 text-fuchsia-600 dark:text-fuchsia-400">
                      意向分析
                    </span>
                  )}
                </div>
              )}
            </div>
          );
        },
      },
      {
        id: "collected",
        header: "采集结果",
        enableSorting: false,
        cell: ({ row }) => {
          const t = row.original;
          return (
            <div className="flex flex-col gap-0.5 text-xs">
              <div className="flex items-center gap-1">
                <span className="text-muted-foreground">内容</span>
                <span className="inline-block w-12 text-right font-mono font-medium text-foreground tabular-nums">
                  {t.totalContents.toLocaleString()}
                </span>
              </div>
              <div className="flex items-center gap-1">
                <span className="text-muted-foreground">评论</span>
                <span className="inline-block w-12 text-right font-mono font-medium text-foreground tabular-nums">
                  {t.totalComments.toLocaleString()}
                </span>
              </div>
            </div>
          );
        },
      },
      {
        id: "keyword-stats",
        header: "采集明细",
        enableSorting: false,
        cell: ({ row }) => {
          const t = row.original;
          const states = keywordRowStates(t);
          // keywordStats 与 keywords 同序(后端按任务 keywords 顺序生成);缺失退回 0
          const statOf = (i: number) =>
            t.keywordStats?.[i] ?? { contentCount: 0, commentCount: 0 };
          return (
            <div className="flex min-w-[300px] max-w-[420px] flex-col divide-y divide-dashed divide-border">
              {t.keywords.map((keyword, i) => {
                const state = states[i];
                const meta = KEYWORD_STATE_META[state];
                const { contentCount, commentCount } = statOf(i);
                const pct = keywordRowProgress(
                  state,
                  contentCount,
                  t.perKeywordLimit,
                );
                return (
                  <div
                    key={keyword}
                    className="flex flex-col gap-0.5 py-1.5 first:pt-0 last:pb-0"
                  >
                    <div className="flex items-center gap-1.5 text-xs">
                      <span
                        className={`size-1.5 shrink-0 rounded-full ${meta.dot}`}
                      />
                      <span
                        className="min-w-0 flex-1 truncate font-medium text-foreground"
                        title={keyword}
                      >
                        {keyword}
                      </span>
                      <span className={`shrink-0 text-[10px] ${meta.text}`}>
                        {meta.label}
                      </span>
                    </div>
                    <div className="flex items-center gap-2">
                      <Progress value={pct} className="h-1 flex-1" />
                      <span className="shrink-0 font-mono text-[10px] text-muted-foreground tabular-nums">
                        内容
                        <span className="ml-0.5 inline-block w-10 text-right text-foreground">
                          {contentCount.toLocaleString()}
                        </span>
                        <span className="mx-1">·</span>
                        评论
                        <span className="ml-0.5 inline-block w-10 text-right text-foreground">
                          {commentCount.toLocaleString()}
                        </span>
                      </span>
                    </div>
                  </div>
                );
              })}
            </div>
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
          // 从 dot 类串里取背景色(按 bg- 前缀找,不依赖排列顺序),供 ping 动画用
          const dotColor =
            meta.dot.split(" ").find((cls) => cls.startsWith("bg-")) ?? meta.dot;
          // 仅保留状态 + 倒计时:归档显示结束时间;定时/监听显示下次运行倒计时;其余不显示时间
          const timeNode = isArchive ? (
            formatTime(t.finishedAt)
          ) : (t.trigger === "daily" && t.scheduledAt) ||
            (t.trigger === "watching" && t.watchIntervalMin) ? (
            <CountdownCell t={t} />
          ) : null;
          return (
            <div className="flex max-w-[150px] flex-col gap-1 whitespace-normal">
              <div className="flex flex-wrap items-center gap-x-2 gap-y-1">
                <span
                  className={`inline-flex items-center gap-1.5 rounded-md border px-1.5 py-0.5 text-xs font-medium ${meta.className}`}
                >
                  {isInProgress(t) ? (
                    <span className="relative flex size-2">
                      <span
                        className={`absolute inline-flex size-full animate-ping rounded-full opacity-75 ${dotColor}`}
                      />
                      <span
                        className={`relative inline-flex size-2 rounded-full ${dotColor}`}
                      />
                    </span>
                  ) : (
                    <span className={`size-1.5 rounded-full ${meta.dot}`} />
                  )}
                  {meta.label}
                  {t.status === "failed" && t.errorMessage && (
                    <SimpleTooltip content={t.errorMessage}>
                      <span className="cursor-help text-[10px] underline">
                        详情
                      </span>
                    </SimpleTooltip>
                  )}
                </span>
                {/* 补偿执行:独立于状态徽章,放在状态信息之后 */}
                {t.status === "failed" && (
                  <SimpleTooltip content="按采集参数补做缺失步骤(意向 / 素材 / 转写);评论缺失请用「重新运行」">
                    <button
                      type="button"
                      onClick={() =>
                        api
                          .compensateTask(t.id)
                          .then(() => {
                            toast.success("已开始补偿");
                            reload();
                          })
                          .catch((e) => toast.error(`补偿失败: ${e}`))
                      }
                      className="inline-flex cursor-pointer items-center gap-0.5 rounded bg-violet-100 px-1.5 py-0.5 text-[10px] font-medium text-violet-700 transition-colors hover:bg-violet-200 dark:bg-violet-950/60 dark:text-violet-300 dark:hover:bg-violet-900/60"
                    >
                      <Wrench className="size-3" />
                      补偿执行
                    </button>
                  </SimpleTooltip>
                )}
              </div>
              {timeNode && (
                <span className="text-[11px] text-muted-foreground">
                  {timeNode}
                </span>
              )}
            </div>
          );
        },
      },
    ];

    cols.push({
      id: "actions",
      header: () => <div className="text-right">操作</div>,
      enableSorting: false,
      cell: ({ row }) => (
        <TaskActionsCell
          task={row.original}
          isArchive={isArchive}
          onUpdate={updateTask}
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
    <div className="flex min-h-0 min-w-0 flex-1 flex-col gap-4">
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
                <TabsList className="h-10 max-w-full overflow-x-auto">
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
                <TabsTrigger value="watching">
                  持续监听任务
                  <Badge variant="secondary" className="ml-1.5">
                    {counts.watching}
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

              <div className={`flex items-center gap-2 ${FORM_CONTROL_SIZING}`}>
                <div className="relative w-full sm:w-96">
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
                  创建采集任务
                </Button>
              </div>
            </div>

            {/* 平台筛选条:横向铺开所有平台;不选即全部,点击已选 chip 可取消 */}
            <div className="mt-2 flex flex-wrap items-center gap-1.5">
              {platforms.map((p) => (
                <PlatformChip
                  key={p.id}
                  id={p.id}
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
                  <EmptyHint text="暂无任务,点击右上角「创建采集任务」开始" />
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
                  <EmptyHint text="暂无快速任务,创建采集任务选「立即一次」即可" />
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
                  <EmptyHint text="暂无定时任务,创建采集任务选「每日定时」即可排入队列" />
                }
              />
            </TabsContent>
            <TabsContent value="watching" className="mt-2 flex min-h-0 flex-1 flex-col">
              <DataTable
                columns={activeColumns}
                data={filtered}
                itemLabel="任务"
                getRowId={(t) => t.id}
                emptyState={
                  <EmptyHint text="暂无持续监听任务,创建采集任务选「持续监听」即可" />
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
        platforms={platformOptions}
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

// 平台筛选 chip:全局统一品牌色(选中实色 / 未选浅色淡显)
function PlatformChip({
  id,
  label,
  active,
  onClick,
}: {
  id: string;
  label: string;
  active: boolean;
  onClick: () => void;
}) {
  return (
    <button type="button" onClick={onClick} className={platformChipClass(id, active)}>
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
  onDelete,
  onEdit,
  onDetail,
}: {
  task: TaskItem;
  isArchive: boolean;
  onUpdate: (id: string, patch: Partial<TaskItem>) => void;
  onDelete: (id: string) => void;
  onEdit: (t: TaskItem) => void;
  onDetail: (t: TaskItem) => void;
}) {
  const [archiveOpen, setArchiveOpen] = useState(false);
  const [deleteOpen, setDeleteOpen] = useState(false);

  return (
    <div className="flex items-center justify-end gap-1">
      <DropdownMenu>
        <DropdownMenuTrigger asChild>
          <Button variant="ghost" size="icon-xs" className="cursor-pointer">
            <MoreHorizontal />
          </Button>
        </DropdownMenuTrigger>
        <DropdownMenuContent align="end" className="w-32">
          <DropdownMenuItem
            className="cursor-pointer"
            onClick={() => onDetail(task)}
          >
            <Eye className="size-3.5" />
            详情
          </DropdownMenuItem>
          {!isArchive && (
            <DropdownMenuItem
              className="cursor-pointer"
              onClick={() => onEdit(task)}
            >
              <SquarePen className="size-3.5" />
              编辑
            </DropdownMenuItem>
          )}
          {!isArchive && (
            <DropdownMenuItem
              className="cursor-pointer"
              onClick={() => setArchiveOpen(true)}
            >
              <Archive className="size-3.5" />
              归档
            </DropdownMenuItem>
          )}
          <DropdownMenuSeparator />
          <DropdownMenuItem
            className="cursor-pointer text-destructive focus:text-destructive"
            onClick={() => setDeleteOpen(true)}
          >
            <Trash2 className="size-3.5" />
            删除
          </DropdownMenuItem>
        </DropdownMenuContent>
      </DropdownMenu>

      <AlertDialog open={archiveOpen} onOpenChange={setArchiveOpen}>
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>归档任务</AlertDialogTitle>
            <AlertDialogDescription>
              将任务「{task.name}」移至「任务归档」,任务保留(不删除),可在归档中查看或复制为新任务。
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel className="cursor-pointer">取消</AlertDialogCancel>
            <AlertDialogAction
              className="cursor-pointer"
              onClick={() => {
                onUpdate(task.id, {
                  status: task.status,
                  archived: true,
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
