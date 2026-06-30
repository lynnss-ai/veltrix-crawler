// CollectPage 的纯元数据与辅助:状态/触发/筛选的标签映射、任务判定谓词、格式化函数。
// 从 CollectPage.tsx 拆出——皆为无 React 状态的纯逻辑 / 常量,缩减主组件文件体量。
import {
  CalendarClock,
  Infinity as InfinityIcon,
  LayoutGrid,
  MapPin,
  Target,
  Timer,
  Zap,
  type LucideIcon,
} from "lucide-react";
import type { PlatformConfig, TaskView } from "@/lib/api";

export type TaskStatus = TaskView["status"];
export type TaskTrigger = TaskView["trigger"];
export type SortMode = TaskView["sortMode"];
export type TimeRange = TaskView["timeRange"];
export type TaskItem = TaskView;

export const SORT_MODE_META: Record<SortMode, { label: string }> = {
  synthetic: { label: "综合" },
  hottest: { label: "最热" },
  latest: { label: "最新" },
  most_comment: { label: "最多评论" },
  most_collect: { label: "最多收藏" },
  most_danmaku: { label: "最大弹幕" },
};

export const TIME_RANGE_META: Record<TimeRange, { label: string }> = {
  any: { label: "不限" },
  "1d": { label: "一天内" },
  "1w": { label: "一周内" },
  "6m": { label: "半年内" },
};

// 各平台支持的排序/时间筛选项(渐进方案:沿用通用值 synthetic/hottest/latest + any/1d/1w/6m,
// 但每平台只列它真正支持的项,并用该平台的真实标签)。后端 legacy 路径对 URL 未覆盖的维度回退
// 结果页文案点击应用(快手/B站时间等),小红书走 RPA 文案点击。未列平台回退 DEFAULT_FILTER_META。
export type FilterOpt<V> = { value: V; label: string };
export const DEFAULT_FILTER_META: {
  sort: FilterOpt<SortMode>[];
  time: FilterOpt<TimeRange>[];
} = {
  sort: [
    { value: "synthetic", label: "综合" },
    { value: "hottest", label: "最热" },
    { value: "latest", label: "最新" },
  ],
  time: [
    { value: "any", label: "不限" },
    { value: "1d", label: "一天内" },
    { value: "1w", label: "一周内" },
    { value: "6m", label: "半年内" },
  ],
};
export const ALL_TIME: FilterOpt<TimeRange>[] = DEFAULT_FILTER_META.time;
export const PLATFORM_FILTER_META: Record<
  string,
  { sort: FilterOpt<SortMode>[]; time: FilterOpt<TimeRange>[] }
> = {
  douyin: {
    sort: [
      { value: "synthetic", label: "综合" },
      { value: "hottest", label: "最多点赞" },
      { value: "latest", label: "最新" },
    ],
    time: ALL_TIME,
  },
  xhs: {
    sort: [
      { value: "synthetic", label: "综合" },
      { value: "latest", label: "最新" },
      { value: "hottest", label: "最多点赞" },
      { value: "most_comment", label: "最多评论" },
      { value: "most_collect", label: "最多收藏" },
    ],
    time: ALL_TIME,
  },
  // 快手:无条件筛选(搜索结果页不提供排序 / 时间等筛选),任务表单不展示「采集筛选」区
  kuaishou: {
    sort: [],
    time: [],
  },
  bilibili: {
    sort: [
      { value: "synthetic", label: "综合排序" },
      { value: "hottest", label: "最多播放" },
      { value: "latest", label: "最新发布" },
      { value: "most_danmaku", label: "最大弹幕" },
      { value: "most_collect", label: "最多收藏" },
    ],
    time: ALL_TIME,
  },
  // TikTok:搜索结果靠顶部 tab 切「内容形式」(综合/视频/照片),无排序 / 时间筛选;
  // 内容形式见 PLATFORM_EXTRA_FILTERS.tiktok
  tiktok: {
    sort: [],
    time: [],
  },
  youtube: {
    sort: [
      { value: "synthetic", label: "综合" },
      { value: "latest", label: "最新" },
      { value: "hottest", label: "最多观看" },
    ],
    time: ALL_TIME,
  },
};

export function filterMetaFor(platform: string) {
  return PLATFORM_FILTER_META[platform] ?? DEFAULT_FILTER_META;
}

// 平台专属额外筛选维度(排序 / 发布时间见上方 PLATFORM_FILTER_META):每个平台单独声明自己
// 「筛选」面板里的维度,只在该平台的任务表单显示与生效。非「不限」选项的 value 即结果页浮层中
// 要点击的文案;"any" = 不限(默认、不点)。新增平台维度 = 在此声明 + 后端无需改(值即点击文案)。
export type FilterDimension = {
  id: string;
  label: string;
  icon: LucideIcon;
  /// 标签文字 + 图标的颜色(Tailwind 类,图标继承 currentColor 一并着色)
  color: string;
  options: FilterOpt<string>[];
};
export const PLATFORM_EXTRA_FILTERS: Record<string, FilterDimension[]> = {
  douyin: [
    {
      id: "videoDuration",
      label: "视频时长",
      icon: Timer,
      color: "text-emerald-600 dark:text-emerald-400",
      options: [
        { value: "any", label: "不限" },
        { value: "1分钟以下", label: "1分钟以下" },
        { value: "1-5分钟", label: "1-5分钟" },
        { value: "5分钟以上", label: "5分钟以上" },
      ],
    },
    {
      id: "searchScope",
      label: "搜索范围",
      icon: Target,
      color: "text-violet-600 dark:text-violet-400",
      options: [
        { value: "any", label: "不限" },
        { value: "关注的人", label: "关注的人" },
        { value: "最近看过", label: "最近看过" },
        { value: "还未看过", label: "还未看过" },
      ],
    },
    {
      id: "contentForm",
      label: "内容形式",
      icon: LayoutGrid,
      color: "text-rose-600 dark:text-rose-400",
      options: [
        { value: "any", label: "不限" },
        { value: "视频", label: "视频" },
        { value: "图文", label: "图文" },
      ],
    },
  ],
  xhs: [
    {
      id: "noteType",
      label: "笔记类型",
      icon: LayoutGrid,
      color: "text-rose-600 dark:text-rose-400",
      options: [
        { value: "any", label: "不限" },
        { value: "视频", label: "视频" },
        { value: "图文", label: "图文" },
      ],
    },
    {
      id: "searchScope",
      label: "搜索范围",
      icon: Target,
      color: "text-violet-600 dark:text-violet-400",
      options: [
        { value: "any", label: "不限" },
        { value: "已看过", label: "已看过" },
        { value: "未看过", label: "未看过" },
        { value: "已关注", label: "已关注" },
      ],
    },
    {
      id: "locationDistance",
      label: "位置距离",
      icon: MapPin,
      color: "text-cyan-600 dark:text-cyan-400",
      options: [
        { value: "any", label: "不限" },
        { value: "同城", label: "同城" },
        { value: "附近", label: "附近" },
      ],
    },
  ],
  // TikTok:内容形式是顶部 tab(#search-tabs 内 综合/用户/视频/直播/照片),非「筛选」浮层;
  // 故 filter_panel_labels 不含 tiktok(不展开面板),直接点 tab 文案「视频」/「照片」即可。
  // 内容只有视频与照片(无图文),"any"=综合 tab(默认不点)。
  tiktok: [
    {
      id: "contentForm",
      label: "内容形式",
      icon: LayoutGrid,
      color: "text-rose-600 dark:text-rose-400",
      options: [
        { value: "any", label: "不限" },
        { value: "视频", label: "视频" },
        { value: "照片", label: "照片" },
      ],
    },
  ],
  // B站:形式(综合/视频/专栏)是搜索结果页顶部 tab,非「筛选」浮层 → 直接点 tab 文案。
  // 基础搜索 URL 为 /video(视频),故「视频」=默认(any,不点);综合 / 专栏 点对应 tab 切换。
  bilibili: [
    {
      id: "contentForm",
      label: "形式",
      icon: LayoutGrid,
      color: "text-pink-600 dark:text-pink-400",
      options: [
        { value: "综合", label: "综合" },
        { value: "any", label: "视频" },
        { value: "专栏", label: "专栏" },
      ],
    },
  ],
};
export function extraFiltersFor(platform: string): FilterDimension[] {
  return PLATFORM_EXTRA_FILTERS[platform] ?? [];
}
// 列表/详情展示用:取平台标签,回退通用标签
export function sortLabelOf(platform: string, value: SortMode) {
  return (
    filterMetaFor(platform).sort.find((o) => o.value === value)?.label ??
    SORT_MODE_META[value]?.label ??
    value
  );
}
export function timeLabelOf(platform: string, value: TimeRange) {
  return (
    filterMetaFor(platform).time.find((o) => o.value === value)?.label ??
    TIME_RANGE_META[value]?.label ??
    value
  );
}
// 列表/详情展示用:把任务选中的平台专属额外筛选(noteType/searchScope/locationDistance 等)
// 转成 {维度标签, 选项标签} 列表,跳过"不限/空"。维度与选项标签按平台配置取,跨平台共用。
export function extraFilterChipsOf(
  platform: string,
  extraFilters: Record<string, string> | undefined,
): { label: string; value: string }[] {
  if (!extraFilters) return [];
  const chips: { label: string; value: string }[] = [];
  for (const dim of extraFiltersFor(platform)) {
    const v = extraFilters[dim.id];
    if (!v || v === "any") continue;
    const opt = dim.options.find((o) => o.value === v);
    chips.push({ label: dim.label, value: opt?.label ?? v });
  }
  return chips;
}

// 数据穿透上下文:从任务列表/详情跳到全量库时携带,按任务过滤内容;
// 含 runStart/runEnd(Unix 秒)时进一步按 collectedAt 落在该次运行时间范围内过滤(单次任务穿透);
// 含 keyword 时再按该关键词过滤(采集明细里单关键词穿透)。
export type TaskContentFilter = {
  taskId: string;
  taskName?: string;
  keyword?: string;
  runStart?: number;
  runEnd?: number;
};

export type CommentTimeRange = NonNullable<TaskView["commentTimeRange"]>;

// 不限排首位:下拉默认/首选项,Select 按 Object.keys 插入顺序渲染
export const COMMENT_TIME_RANGE_META: Record<CommentTimeRange, { label: string }> = {
  any: { label: "不限" },
  "3d": { label: "3 天内" },
  "7d": { label: "7 天内" },
  "14d": { label: "14 天内" },
};

// 单视频评论抓取上限选项,value 为字符串(Select 需要),0 表示不限(排首位)
export const COMMENT_LIMIT_OPTIONS: { value: string; label: string }[] = [
  { value: "0", label: "不限" },
  { value: "100", label: "100 条" },
  { value: "500", label: "500 条" },
  { value: "1000", label: "1000 条" },
  { value: "2000", label: "2000 条" },
];

// 活跃 = 未归档;归档由用户手动操作(终止 / 失败都不自动归档,留在列表里)
export function isActive(t: TaskItem): boolean {
  return !t.archived;
}

// 终态:任务已结束(完成/失败/已停止),不再有暂停/停止等运行操作,可重跑或复制
export function isTerminal(t: TaskItem): boolean {
  return ["completed", "failed", "cancelled"].includes(t.status);
}
// 进行中:运行 / 评论采集 / 意向分析 / 素材下载,均可终止
export function isInProgress(t: TaskItem): boolean {
  return [
    "running",
    "collecting_comments",
    "analyzing_comments",
    "downloading_media",
  ].includes(t.status);
}
// 任务列表:所有未归档的任务(三种触发类型都纳入,作为总览)
export function isInWatchingList(t: TaskItem): boolean {
  return isActive(t);
}
// 快速任务:立即一次
export function isInQuickList(t: TaskItem): boolean {
  return t.trigger === "once-now" && isActive(t);
}
// 定时任务队列:仅每日定时(到点自动跑,带下次运行倒计时)
export function isInScheduledQueue(t: TaskItem): boolean {
  return t.trigger === "daily" && isActive(t);
}
// 持续监听任务:按间隔自动追新(带下次运行倒计时)
export function isInWatchingTasks(t: TaskItem): boolean {
  return t.trigger === "watching" && isActive(t);
}

// 下一次自动运行的时间点(Unix 秒);无法推算(未配置 / 监听未首启)返回 null
export function nextRunTs(t: TaskItem): number | null {
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
export function formatCountdown(sec: number): string {
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
export const STATUS_META: Record<
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
export type KeywordState = "done" | "running" | "pending" | "failed";

export const KEYWORD_STATE_META: Record<
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
export function keywordRowStates(t: TaskView): KeywordState[] {
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
export function keywordRowProgress(
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

export const TRIGGER_META: Record<
  TaskTrigger,
  { label: string; icon: typeof Zap }
> = {
  "once-now": { label: "立即一次", icon: Zap },
  daily: { label: "每日定时", icon: CalendarClock },
  watching: { label: "持续监听", icon: InfinityIcon },
};

// 采集策略默认值 — 创建采集任务表单初值
export const DEFAULT_STRATEGY = {
  sortMode: "synthetic" as SortMode,
  timeRange: "any" as TimeRange,
  perKeywordLimit: 50,
  minLikes: 0,
  aiExtract: false,
  collectComments: false,
  commentTimeRange: "any" as CommentTimeRange,
  commentLimit: 100,
  analyzeCommentIntent: false,
  autoSyncObsidian: false,
  // 平台专属额外筛选(默认全不限);实际维度由 PLATFORM_EXTRA_FILTERS 按平台决定
  extraFilters: {} as Record<string, string>,
};

// 平台颜色 / 名称统一从 @/lib/platforms 取(PlatformId 枚举);本文件内仅引用

export { formatTimestamp as formatTime } from "@/lib/utils";

// ---- 页面主体 ----

// 任务表单平台下拉选项:平台配置 + 绑定账号数 / 有效(登录)账号数
export type PlatformOption = PlatformConfig & { total: number; active: number };
