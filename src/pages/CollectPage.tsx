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
import { listen } from "@tauri-apps/api/event";
import { openUrl } from "@tauri-apps/plugin-opener";
import { platformClass, platformChipClass } from "@/lib/platforms";
import { FORM_CONTROL_SIZING } from "@/lib/form-sizing";
import { type ColumnDef } from "@tanstack/react-table";
import {
  Archive,
  CalendarClock,
  ChevronLeft,
  CircleSlash2,
  Eye,
  Filter,
  Infinity as InfinityIcon,
  MoreHorizontal,
  SquarePen,
  Play,
  Plus,
  Radar,
  RotateCcw,
  Search,
  Square,
  Trash2,
  Wrench,
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

type CommentTimeRange = NonNullable<TaskView["commentTimeRange"]>;

const COMMENT_TIME_RANGE_META: Record<CommentTimeRange, { label: string }> = {
  "3d": { label: "3 天内" },
  "7d": { label: "7 天内" },
  "14d": { label: "14 天内" },
  any: { label: "不限" },
};

// 单视频评论抓取上限选项,value 为字符串(Select 需要),0 表示不限
const COMMENT_LIMIT_OPTIONS: { value: string; label: string }[] = [
  { value: "100", label: "100 条" },
  { value: "500", label: "500 条" },
  { value: "1000", label: "1000 条" },
  { value: "0", label: "不限" },
];

// 活跃 = 未归档;归档由用户手动操作(终止 / 失败都不自动归档,留在列表里)
function isActive(t: TaskItem): boolean {
  return !t.archived;
}

// 终态:任务已结束(完成/失败/已停止),不再有暂停/停止等运行操作,可重跑或复制
function isTerminal(t: TaskItem): boolean {
  return ["completed", "failed", "cancelled"].includes(t.status);
}
// 进行中:运行 / 评论采集 / 意向分析 / 素材下载,均可终止
function isInProgress(t: TaskItem): boolean {
  return [
    "running",
    "collecting_comments",
    "analyzing_comments",
    "downloading_media",
  ].includes(t.status);
}
// 任务列表:所有未归档的任务(三种触发类型都纳入,作为总览)
function isInWatchingList(t: TaskItem): boolean {
  return isActive(t);
}
// 快速任务:立即一次
function isInQuickList(t: TaskItem): boolean {
  return t.trigger === "once-now" && isActive(t);
}
// 定时任务队列:仅每日定时(到点自动跑,带下次运行倒计时)
function isInScheduledQueue(t: TaskItem): boolean {
  return t.trigger === "daily" && isActive(t);
}
// 持续监听任务:按间隔自动追新(带下次运行倒计时)
function isInWatchingTasks(t: TaskItem): boolean {
  return t.trigger === "watching" && isActive(t);
}

// 下一次自动运行的时间点(Unix 秒);无法推算(未配置 / 监听未首启)返回 null
function nextRunTs(t: TaskItem): number | null {
  if (t.trigger === "daily" && t.scheduledAt) {
    const [hh, mm] = t.scheduledAt.split(":").map(Number);
    if (Number.isNaN(hh)) return null;
    const target = new Date();
    target.setHours(hh, Number.isNaN(mm) ? 0 : mm, 0, 0);
    let ts = Math.floor(target.getTime() / 1000);
    const nowSec = Math.floor(Date.now() / 1000);
    // 今日目标点已过:今天跑过了 → 看明天;还没跑 → 即将由调度器启动(返回已过点)
    if (ts <= nowSec) {
      const ranToday = t.startedAt != null && t.startedAt >= ts;
      if (ranToday) ts += 86400;
    }
    return ts;
  }
  if (t.trigger === "watching" && t.watchIntervalMin) {
    const last = t.finishedAt ?? t.startedAt;
    if (!last) return null; // 从未运行:需手动首启,启动后按间隔自动追新
    return last + t.watchIntervalMin * 60;
  }
  return null;
}

// 倒计时文案:到点显示「即将运行」(后台调度器 30s 内拉起)
function formatCountdown(sec: number): string {
  if (sec <= 0) return "即将运行";
  const h = Math.floor(sec / 3600);
  const m = Math.floor((sec % 3600) / 60);
  const s = sec % 60;
  if (h > 0)
    return `${h}时${String(m).padStart(2, "0")}分${String(s).padStart(2, "0")}秒后`;
  if (m > 0) return `${m}分${String(s).padStart(2, "0")}秒后`;
  return `${s}秒后`;
}

// 定时/监听任务的「时间」单元格:计划描述 + 下次运行倒计时(每秒自刷)
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
  return (
    <span className="text-xs text-muted-foreground">
      {plan}
      <span
        className={`ml-1.5 font-mono ${
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

// 「采集明细」列里单个关键词的采集态。任务整体只有一个状态/进度,这里据此估算每个关键词的
// 采集态,非后端逐词实时记录(运行中按整体进度反推当前在采的词)。
type KeywordState = "done" | "running" | "pending" | "failed";

const KEYWORD_STATE_META: Record<
  KeywordState,
  { label: string; dot: string; text: string }
> = {
  done: {
    label: "完成",
    dot: "bg-emerald-500",
    text: "text-emerald-600 dark:text-emerald-400",
  },
  running: {
    label: "采集中",
    dot: "bg-emerald-500 animate-pulse",
    text: "text-emerald-600 dark:text-emerald-400",
  },
  pending: {
    label: "等待",
    dot: "bg-muted-foreground/40",
    text: "text-muted-foreground",
  },
  failed: { label: "未完成", dot: "bg-destructive", text: "text-destructive" },
};

// 按任务整体状态/进度推断每个关键词(按 keywords 顺序)的采集态:
// 关键词采集串行执行 → completed / 后处理阶段(评论/意向/素材)= 全部完成;pending = 全部等待;
// running / paused 按整体进度反推当前在采的词索引(progress = ((idx+1)/n)*100);
// failed / cancelled:已采到内容的算完成,其余算未完成。
function keywordRowStates(t: TaskView): KeywordState[] {
  const n = t.keywords.length;
  const hasContent = (i: number) =>
    (t.keywordStats?.[i]?.contentCount ?? 0) > 0;
  switch (t.status) {
    case "pending":
      return Array.from({ length: n }, (): KeywordState => "pending");
    case "completed":
    case "collecting_comments":
    case "analyzing_comments":
    case "downloading_media":
      return Array.from({ length: n }, (): KeywordState => "done");
    case "failed":
    case "cancelled":
      return Array.from({ length: n }, (_, i): KeywordState =>
        hasContent(i) ? "done" : "failed",
      );
    default: {
      // running / paused
      const currentIdx = Math.min(
        n - 1,
        Math.max(0, Math.round((t.progress / 100) * n) - 1),
      );
      return Array.from({ length: n }, (_, i): KeywordState =>
        i < currentIdx ? "done" : i === currentIdx ? "running" : "pending",
      );
    }
  }
}

// 单关键词进度百分比:完成=100,等待=0;采集中/未完成按已采内容数 ÷ 目标数估算
// (目标数为「不限」时无法按比例,采到内容给非零示意值,否则 0)。
function keywordRowProgress(
  state: KeywordState,
  contentCount: number,
  perKeywordLimit: number,
): number {
  if (state === "done") return 100;
  if (state === "pending") return 0;
  if (perKeywordLimit > 0) {
    return Math.min(100, Math.round((contentCount / perKeywordLimit) * 100));
  }
  return contentCount > 0 ? 60 : 0;
}

const TRIGGER_META: Record<
  TaskTrigger,
  { label: string; icon: typeof Zap }
> = {
  "once-now": { label: "立即一次", icon: Zap },
  daily: { label: "每日定时", icon: CalendarClock },
  watching: { label: "持续监听", icon: InfinityIcon },
};

// 采集策略默认值 — 创建采集任务表单初值
const DEFAULT_STRATEGY = {
  sortMode: "synthetic" as SortMode,
  timeRange: "any" as TimeRange,
  perKeywordLimit: 50,
  minLikes: 0,
  aiExtract: false,
  collectComments: false,
  commentTimeRange: "any" as CommentTimeRange,
  commentLimit: 500,
  analyzeCommentIntent: false,
  autoSyncObsidian: false,
};

// 平台颜色 / 名称统一从 @/lib/platforms 取(PlatformId 枚举);本文件内仅引用

function formatTime(ts?: number | null): string {
  if (!ts) return "—";
  return new Date(ts * 1000).toLocaleString();
}

// ---- 页面主体 ----

// 任务表单平台下拉选项:平台配置 + 绑定账号数 / 有效(登录)账号数
type PlatformOption = PlatformConfig & { total: number; active: number };

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
            <div className="flex max-w-[200px] flex-col gap-1 whitespace-normal">
              <div className="flex items-center gap-2">
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
  platforms: PlatformOption[];
  onOpenChange: (v: boolean) => void;
  onSubmit: (input: TaskInput) => void;
}) {
  const [name, setName] = useState(initial?.name ?? "");
  const [industry, setIndustry] = useState(initial?.industry ?? "");
  const [platform, setPlatform] = useState(initial?.platform ?? "");
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
  const [autoSyncObsidian, setAutoSyncObsidian] = useState(
    initial?.autoSyncObsidian ?? DEFAULT_STRATEGY.autoSyncObsidian,
  );
  // ffmpeg 安装检测:null=检测中,true/false=结果;已装则隐藏「点此下载」引导
  const [ffmpegAvailable, setFfmpegAvailable] = useState<boolean | null>(null);
  // Obsidian vault 是否已配置:未配置则禁用「自动同步」开关并强制关闭
  const [obsidianConfigured, setObsidianConfigured] = useState(false);

  // 表单打开时检测 ffmpeg 安装情况与 Obsidian 配置:决定下载引导是否显示 / 同步开关是否可用
  useEffect(() => {
    if (!open) return;
    api
      .checkFfmpeg()
      .then((s) => setFfmpegAvailable(s.available))
      .catch(() => setFfmpegAvailable(false));
    api
      .getObsidianVault()
      .then((vault) => {
        const configured = vault.trim().length > 0;
        setObsidianConfigured(configured);
        // 未配置却已勾选(如编辑旧任务后 vault 被清空)→ 关掉,避免存下无法生效的自动同步
        if (!configured) setAutoSyncObsidian(false);
      })
      .catch(() => setObsidianConfigured(false));
    // 仅在打开时检测一次;setState 恒稳定,无需进依赖
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open]);

  // 评论采集
  const [collectComments, setCollectComments] = useState(
    initial?.collectComments ?? DEFAULT_STRATEGY.collectComments,
  );
  const [commentTimeRange, setCommentTimeRange] = useState<CommentTimeRange>(
    initial?.commentTimeRange ?? DEFAULT_STRATEGY.commentTimeRange,
  );
  const [commentLimit, setCommentLimit] = useState(
    String(initial?.commentLimit ?? DEFAULT_STRATEGY.commentLimit),
  );
  const [analyzeCommentIntent, setAnalyzeCommentIntent] = useState(
    initial?.analyzeCommentIntent ?? DEFAULT_STRATEGY.analyzeCommentIntent,
  );

  function handleSubmit(e: FormEvent) {
    e.preventDefault();
    const trimmedName = name.trim();
    if (!trimmedName) {
      toast.error("请输入任务名");
      return;
    }
    if (!industry) {
      toast.error("请选择所属行业");
      return;
    }
    if (!platform) {
      toast.error("请选择所属平台");
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
          ? Math.max(30, Number(watchIntervalMin) || 30)
          : null,
      sortMode,
      timeRange,
      perKeywordLimit: Math.max(1, Number(perKeywordLimit) || 50),
      minLikes: Math.max(0, Number(minLikes) || 0),
      aiExtract,
      autoSyncObsidian,
      collectComments,
      // 评论参数仅在开启评论采集时有意义;意图分析依赖评论采集,关闭时强制 false
      commentTimeRange,
      commentLimit: collectComments ? Math.max(0, Number(commentLimit) || 0) : 0,
      analyzeCommentIntent: collectComments && analyzeCommentIntent,
    };
    onSubmit(input);
  }

  return (
    <Sheet open={open} onOpenChange={onOpenChange}>
      <SheetContent className="flex w-full flex-col gap-0 sm:max-w-[600px]">
        <SheetHeader>
          <SheetTitle>{initial ? "编辑采集任务" : "创建采集任务"}</SheetTitle>
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
            <Label htmlFor="task-name">
              任务名 <span className="text-destructive">*</span>
            </Label>
            <Input
              id="task-name"
              value={name}
              onChange={(e) => setName(e.target.value)}
              placeholder="例:小红书 · 母婴关键词监控"
            />
          </div>
          <div className="space-y-1.5">
            <Label htmlFor="task-industry">
              所属行业 <span className="text-destructive">*</span>
            </Label>
            <Select value={industry} onValueChange={setIndustry}>
              <SelectTrigger id="task-industry" className="w-full">
                <SelectValue placeholder="请选择所属行业" />
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
            <Label htmlFor="task-platform">
              所属平台 <span className="text-destructive">*</span>
            </Label>
            <Select value={platform} onValueChange={setPlatform}>
              <SelectTrigger id="task-platform" className="w-full">
                <SelectValue placeholder="请选择所属平台" />
              </SelectTrigger>
              <SelectContent>
                {platforms.length === 0 ? (
                  <div className="px-2 py-1.5 text-xs text-muted-foreground">
                    暂无启用平台
                  </div>
                ) : (
                  platforms.map((p) => (
                    <SelectItem
                      key={p.id}
                      value={p.id}
                      disabled={p.active === 0}
                      className="[&>span:last-child]:w-full"
                    >
                      <span className="flex w-full items-center gap-1.5">
                        <span>{p.name}</span>
                        <span className="ml-auto text-xs text-muted-foreground">
                          有效账号
                        </span>
                        <span className="rounded bg-muted px-1.5 py-0.5 font-mono text-[11px] text-muted-foreground">
                          {p.active}/{p.total}
                        </span>
                      </span>
                    </SelectItem>
                  ))
                )}
              </SelectContent>
            </Select>
          </div>
          <div className="space-y-1.5">
            <div className="flex items-center justify-between">
              <Label htmlFor="task-keywords">
                关键词 <span className="text-destructive">*</span>
              </Label>
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
                对采集到的视频自动提取文案(转音频后做语音转写);依赖 ffmpeg
                {ffmpegAvailable === true && (
                  <span className="ml-1 text-emerald-600 dark:text-emerald-400">
                    · 已检测到,可正常转写
                  </span>
                )}
                {ffmpegAvailable === false && (
                  <>
                    ,未安装可
                    <button
                      type="button"
                      className="ml-1 cursor-pointer text-primary underline underline-offset-2 hover:text-primary/80"
                      onClick={() =>
                        openUrl("https://ffmpeg.org/download.html").catch((e) =>
                          toast.error(`打开下载页失败: ${e}`),
                        )
                      }
                    >
                      点此下载
                    </button>
                  </>
                )}
              </p>
            </div>
            <Switch
              id="task-ai-extract"
              checked={aiExtract}
              onCheckedChange={setAiExtract}
            />
          </div>
          <div className="flex items-center justify-between rounded-md border px-3 py-2.5">
            <div className="space-y-0.5">
              <Label htmlFor="task-auto-sync" className="cursor-pointer">
                采集完成后自动同步到 Obsidian
              </Label>
              <p className="text-xs text-muted-foreground">
                {obsidianConfigured
                  ? "采完自动把内容写入你的 Obsidian vault;未开启也可在内容库手动同步"
                  : "未配置 Obsidian,请先到「系统设置 → Obsidian」配置 vault 路径后再开启"}
              </p>
            </div>
            <Switch
              id="task-auto-sync"
              checked={obsidianConfigured && autoSyncObsidian}
              onCheckedChange={setAutoSyncObsidian}
              disabled={!obsidianConfigured}
            />
          </div>

          {/* 评论采集:开启后展开评论抓取规则 + 意图分析 */}
          <div className="rounded-md border">
            <div className="flex items-center justify-between px-3 py-2.5">
              <div className="space-y-0.5">
                <Label
                  htmlFor="task-collect-comments"
                  className="cursor-pointer"
                >
                  评论采集
                </Label>
                <p className="text-xs text-muted-foreground">
                  抓取内容下的评论,用于后续意向客户分析
                </p>
              </div>
              <Switch
                id="task-collect-comments"
                checked={collectComments}
                onCheckedChange={(v) => {
                  setCollectComments(v);
                  // 意图分析依赖评论采集,关闭评论采集时同步关闭意图分析
                  if (!v) setAnalyzeCommentIntent(false);
                }}
              />
            </div>

            {collectComments && (
              <div className="space-y-3 border-t px-3 py-3">
                <div className="grid grid-cols-2 gap-3">
                  <div className="space-y-1.5">
                    <Label htmlFor="task-comment-time">评论时间范围</Label>
                    <Select
                      value={commentTimeRange}
                      onValueChange={(v) =>
                        setCommentTimeRange(v as CommentTimeRange)
                      }
                    >
                      <SelectTrigger id="task-comment-time" className="w-full">
                        <SelectValue />
                      </SelectTrigger>
                      <SelectContent>
                        {(
                          Object.keys(
                            COMMENT_TIME_RANGE_META,
                          ) as CommentTimeRange[]
                        ).map((k) => (
                          <SelectItem key={k} value={k}>
                            {COMMENT_TIME_RANGE_META[k].label}
                          </SelectItem>
                        ))}
                      </SelectContent>
                    </Select>
                  </div>
                  <div className="space-y-1.5">
                    <Label htmlFor="task-comment-limit">单视频上限</Label>
                    <Select value={commentLimit} onValueChange={setCommentLimit}>
                      <SelectTrigger id="task-comment-limit" className="w-full">
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
                </div>

                <div className="flex items-center justify-between rounded-md border px-3 py-2.5">
                  <div className="space-y-0.5">
                    <Label
                      htmlFor="task-comment-intent"
                      className="cursor-pointer"
                    >
                      评论意图分析
                    </Label>
                    <p className="text-xs text-muted-foreground">
                      采集完成后用 AI 分析评论,提取有意向的客户
                    </p>
                  </div>
                  <Switch
                    id="task-comment-intent"
                    checked={analyzeCommentIntent}
                    onCheckedChange={setAnalyzeCommentIntent}
                  />
                </div>
              </div>
            )}
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
                      placeholder="时"
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
                      placeholder="分"
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
                  min={30}
                  value={watchIntervalMin}
                  onChange={(e) => setWatchIntervalMin(e.target.value)}
                  onBlur={() => {
                    // 失焦纠偏:低于 30 自动抬到 30(最小轮询间隔)
                    const n = Number(watchIntervalMin);
                    if (!Number.isFinite(n) || n < 30) {
                      setWatchIntervalMin("30");
                    }
                  }}
                  className="w-40"
                />
                <p className="text-xs text-muted-foreground">
                  每过 {watchIntervalMin || "?"} 分钟检查一次是否有新内容(最少 30
                  分钟)
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
          <Button type="submit" form="task-form" className="cursor-pointer">
            {initial ? "保存修改" : "创建采集任务"}
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
