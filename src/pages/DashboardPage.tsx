import {
  memo,
  useEffect,
  useRef,
  useState,
  type ComponentType,
} from "react";
import {
  Activity,
  CalendarDays,
  Inbox,
  CheckCircle2,
  Clock,
  Database,
  FileText,
  ListChecks,
  MessageCircle,
  Sparkles,
  X,
  XCircle,
} from "lucide-react";
import { type DateRange } from "react-day-picker";
import { listen } from "@tauri-apps/api/event";
import { platformColorHex } from "@/lib/platforms";
import {
  api,
  type DashboardOverview,
  type PlatformCount,
} from "@/lib/api";
import { ErrorBanner } from "@/components/ErrorBanner";
import { AnimatedNumber } from "@/components/animated-number";
import { Button } from "@/components/ui/button";
import { Calendar } from "@/components/ui/calendar";
import {
  Popover,
  PopoverContent,
  PopoverTrigger,
} from "@/components/ui/popover";
import { DonutChart } from "@/components/charts/DonutChart";
import { AutoScrollList } from "@/components/charts/AutoScrollList";
import { MultiTrendChart } from "@/components/charts/MultiTrendChart";

// 实时刷新节流间隔:采集中 task-progress / collect-log 事件触发很频繁,
// 而概览是多表聚合查询较重,最快每 3s 拉一次,事件风暴不会压垮后端
const REFRESH_THROTTLE_MS = 3000;
// 兜底轮询间隔:覆盖无事件推送的数据变化(删除内容、其他端写入等)
const POLL_INTERVAL_MS = 30_000;

// 平台配色:三大平台用官方品牌色(与全局平台标一致),其余平台按序号回退调色板
function platformColor(platform: string, index: number): string {
  return platformColorHex(platform) ?? PALETTE[index % PALETTE.length];
}

// 多平台趋势线 / 环形图配色(超出则循环复用)
const PALETTE = [
  "#0ea5e9",
  "#8b5cf6",
  "#10b981",
  "#f59e0b",
  "#ef4444",
  "#ec4899",
  "#14b8a6",
  "#f97316",
];

function toStartTs(d: Date): number {
  return Math.floor(new Date(d).setHours(0, 0, 0, 0) / 1000);
}
function toEndTs(d: Date): number {
  return Math.floor(new Date(d).setHours(23, 59, 59, 999) / 1000);
}
function fmtMd(d: Date): string {
  const p = (n: number) => String(n).padStart(2, "0");
  return `${p(d.getMonth() + 1)}-${p(d.getDate())}`;
}

// 数据概览:累计计数(平台细分)+ 今日/任务概况 + 多平台趋势 + 意向/平台/素材占比 + 热门榜。
export function DashboardPage() {
  const [data, setData] = useState<DashboardOverview | null>(null);
  const [platforms, setPlatforms] = useState<{ id: string; name: string }[]>(
    [],
  );
  const [range, setRange] = useState<DateRange | undefined>();
  const [error, setError] = useState<string | null>(null);

  const platformName = (id: string) =>
    platforms.find((p) => p.id === id)?.name ?? id;

  // 事件回调里读到的 range 状态会过期(闭包捕获),用 ref 保存当前区间供自动刷新使用
  const rangeRef = useRef<DateRange | undefined>(undefined);
  const lastLoadAt = useRef(0);
  const pendingTimer = useRef<number | null>(null);

  const load = (r?: DateRange) => {
    rangeRef.current = r;
    lastLoadAt.current = Date.now();
    const start = r?.from ? toStartTs(r.from) : undefined;
    const end = r?.to ? toEndTs(r.to) : r?.from ? toEndTs(r.from) : undefined;
    api
      .dashboardOverview(start, end)
      .then(setData)
      .catch((e) => setError(String(e)));
  };

  // 节流刷新:距上次加载不足节流间隔时,挂一个尾随定时器补一次,保证最终状态不丢
  const scheduleReload = () => {
    const since = Date.now() - lastLoadAt.current;
    if (since >= REFRESH_THROTTLE_MS) {
      load(rangeRef.current);
    } else if (pendingTimer.current === null) {
      pendingTimer.current = window.setTimeout(() => {
        pendingTimer.current = null;
        load(rangeRef.current);
      }, REFRESH_THROTTLE_MS - since);
    }
  };

  useEffect(() => {
    api.listPlatforms().then(setPlatforms).catch((e) => console.warn("加载平台列表失败:", e));
    load();

    // 实时刷新:订阅采集进度与采集日志事件,数据一落库概览随之更新,无需手动刷新;
    // 再加慢轮询兜底,覆盖无事件推送的变化
    let disposed = false;
    const unlistens: (() => void)[] = [];
    for (const name of ["task-progress", "collect-log"]) {
      listen(name, scheduleReload).then(
        (fn) => {
          if (disposed) fn();
          else unlistens.push(fn);
        },
        () => {}, // 浏览器调试(非 Tauri)环境订阅失败,仅靠轮询
      );
    }
    const poll = window.setInterval(
      () => load(rangeRef.current),
      POLL_INTERVAL_MS,
    );
    return () => {
      disposed = true;
      unlistens.forEach((fn) => fn());
      window.clearInterval(poll);
      if (pendingTimer.current !== null) {
        window.clearTimeout(pendingTimer.current);
        pendingTimer.current = null;
      }
    };
    // 仅首次挂载订阅;区间变化由 onSelect 主动触发并经 rangeRef 透传给自动刷新
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const rangeLabel =
    range?.from && range?.to
      ? `${fmtMd(range.from)} ~ ${fmtMd(range.to)}`
      : range?.from
        ? fmtMd(range.from)
        : "近 14 天";

  // 环形图数据
  const intentDonut = data
    ? [
        { label: "高意向", value: data.intentDistribution.high, color: "#10b981" },
        { label: "中意向", value: data.intentDistribution.medium, color: "#f59e0b" },
        { label: "低意向", value: data.intentDistribution.low, color: "#94a3b8" },
        { label: "无意向", value: data.intentDistribution.none, color: "#cbd5e1" },
      ]
    : [];
  const platformDonut = (data?.contentByPlatform ?? [])
    .filter((p) => p.count > 0)
    .map((p, i) => ({
      label: platformName(p.platform),
      value: p.count,
      color: platformColor(p.platform, i),
    }));
  const mediaDonut = data
    ? [
        { label: "成功", value: data.mediaStats.success, color: "#10b981" },
        { label: "待处理", value: data.mediaStats.pending, color: "#f59e0b" },
        { label: "失败", value: data.mediaStats.failed, color: "#ef4444" },
      ]
    : [];

  return (
    <div className="veltrix-no-scrollbar min-h-0 flex-1 space-y-4 overflow-y-auto p-1">
      <ErrorBanner message={error} onClose={() => setError(null)} />

      {/* 累计计数 + 平台细分 */}
      <div
        className="veltrix-enter grid grid-cols-1 gap-4 sm:grid-cols-3"
        style={{ animationDelay: "0ms" }}
      >
        <OverviewCard
          icon={Database}
          label="全量库"
          hint="采集内容总数"
          total={data?.contentTotal}
          byPlatform={data?.contentByPlatform}
          platformName={platformName}
          color="text-sky-600 dark:text-sky-400"
          bg="bg-sky-500/10"
          kindStats={
            data
              ? { video: data.contentVideo, image: data.contentImage }
              : undefined
          }
        />
        <OverviewCard
          icon={MessageCircle}
          label="评论库"
          hint="采集评论总数"
          total={data?.commentTotal}
          byPlatform={data?.commentByPlatform}
          platformName={platformName}
          color="text-violet-600 dark:text-violet-400"
          bg="bg-violet-500/10"
          kindStats={
            data
              ? { video: data.commentVideo, image: data.commentImage }
              : undefined
          }
        />
        <OverviewCard
          icon={Sparkles}
          label="意向客资"
          hint="高意向评论(AI 标注)"
          total={data?.intentTotal}
          byPlatform={data?.intentByPlatform}
          platformName={platformName}
          color="text-emerald-600 dark:text-emerald-400"
          bg="bg-emerald-500/10"
          kindStats={
            data
              ? { video: data.intentVideo, image: data.intentImage }
              : undefined
          }
        />
      </div>

      {/* 今日采集 + 任务状态 */}
      <div
        className="veltrix-enter grid grid-cols-1 gap-4 lg:grid-cols-2"
        style={{ animationDelay: "70ms" }}
      >
        <div className="veltrix-card p-5">
          <h3 className="mb-4 flex items-center gap-2 text-sm font-semibold text-foreground">
            <CalendarDays className="size-4 text-muted-foreground" />
            今日采集
          </h3>
          <div className="grid grid-cols-2 gap-4">
            <TodayMetric
              icon={FileText}
              label="内容"
              value={data?.today.contents}
              delta={data?.today.contentsDelta}
              color="text-sky-600 dark:text-sky-400"
              bg="bg-sky-500/10"
            />
            <TodayMetric
              icon={MessageCircle}
              label="评论"
              value={data?.today.comments}
              delta={data?.today.commentsDelta}
              color="text-violet-600 dark:text-violet-400"
              bg="bg-violet-500/10"
            />
          </div>
          {/* 今日各平台采集细分:每行两个平台,平台名左,内容 / 评论定宽右对齐成列 */}
          {data && platforms.length > 0 && (
            <div className="mt-4 grid grid-cols-2 gap-x-6 gap-y-1.5 border-t pt-3 text-xs">
              {platforms.map((platform) => {
                // byPlatform 只含今日采到数据的平台;未采到的补 0 也列出,平台一览更完整
                const stat = data.today.byPlatform.find(
                  (p) => p.platform === platform.id,
                );
                return (
                  <div key={platform.id} className="flex items-center gap-2">
                    <span className="flex-1 truncate text-muted-foreground">
                      {platformName(platform.id)}
                    </span>
                    <span className="flex w-16 justify-between">
                      <span className="text-muted-foreground">内容</span>
                      <span className="font-mono text-foreground">
                        <AnimatedNumber value={stat?.contents ?? 0} />
                      </span>
                    </span>
                    <span className="flex w-16 justify-between">
                      <span className="text-muted-foreground">评论</span>
                      <span className="font-mono text-foreground">
                        <AnimatedNumber value={stat?.comments ?? 0} />
                      </span>
                    </span>
                  </div>
                );
              })}
            </div>
          )}
        </div>
        <div className="veltrix-card flex flex-col p-5">
          <h3 className="mb-4 flex items-center gap-2 text-sm font-semibold text-foreground">
            <ListChecks className="size-4 text-muted-foreground" />
            任务状态
          </h3>
          <div className="grid flex-1 grid-cols-4 grid-rows-1 gap-2">
            <StatusTile
              icon={Activity}
              label="进行中"
              value={data?.taskStatus.running}
              color="text-sky-600 dark:text-sky-400"
              bg="bg-sky-500/10"
            />
            <StatusTile
              icon={Clock}
              label="排队"
              value={data?.taskStatus.pending}
              color="text-slate-600 dark:text-slate-300"
              bg="bg-slate-500/10"
            />
            <StatusTile
              icon={CheckCircle2}
              label="今日完成"
              value={data?.taskStatus.completedToday}
              color="text-emerald-600 dark:text-emerald-400"
              bg="bg-emerald-500/10"
            />
            <StatusTile
              icon={XCircle}
              label="失败"
              value={data?.taskStatus.failed}
              color="text-rose-600 dark:text-rose-400"
              bg="bg-rose-500/10"
            />
          </div>
        </div>
      </div>

      {/* 采集趋势(多平台 + 区间) */}
      <div
        className="veltrix-enter veltrix-card p-6"
        style={{ animationDelay: "140ms" }}
      >
        <div className="mb-4 flex flex-wrap items-center justify-between gap-2">
          <h2 className="text-sm font-semibold text-foreground">
            数据采集趋势 · 各平台内容按天(平滑折线)
          </h2>
          <Popover>
            <PopoverTrigger asChild>
              <Button variant="outline" size="sm" className="cursor-pointer">
                <CalendarDays className="size-4" />
                {rangeLabel}
                {range?.from && (
                  <span
                    role="button"
                    tabIndex={-1}
                    onClick={(e) => {
                      e.stopPropagation();
                      setRange(undefined);
                      load(undefined);
                    }}
                    className="-mr-1 ml-1 inline-flex items-center rounded p-0.5 text-muted-foreground hover:bg-muted hover:text-foreground"
                  >
                    <X className="size-3.5" />
                  </span>
                )}
              </Button>
            </PopoverTrigger>
            <PopoverContent className="w-auto p-0" align="end">
              <Calendar
                mode="range"
                numberOfMonths={2}
                selected={range}
                onSelect={(r) => {
                  setRange(r);
                  load(r);
                }}
              />
              <div className="flex justify-end border-t p-2">
                <Button
                  variant="ghost"
                  size="sm"
                  className="cursor-pointer"
                  onClick={() => {
                    setRange(undefined);
                    load(undefined);
                  }}
                >
                  重置为近 14 天
                </Button>
              </div>
            </PopoverContent>
          </Popover>
        </div>

        {data && data.trendSeries.length > 0 && (
          <div className="mb-3 flex flex-wrap items-center gap-4 text-xs text-muted-foreground">
            {data.trendSeries.map((s, i) => (
              <span key={s.platform} className="inline-flex items-center gap-1.5">
                <span
                  className="size-2 rounded-full"
                  style={{ background: platformColor(s.platform, i) }}
                />
                {platformName(s.platform)}
              </span>
            ))}
          </div>
        )}

        <MultiTrendChart
          dates={data?.trendDates ?? []}
          series={data?.trendSeries ?? []}
          platformName={platformName}
          metric="contents"
          renderEmpty={(props) => <EmptyLine {...props} />}
        />
      </div>

      {/* 意向分布 / 平台占比 / 素材下载 */}
      <div
        className="veltrix-enter grid grid-cols-1 gap-4 lg:grid-cols-3"
        style={{ animationDelay: "210ms" }}
      >
        <DonutCard title="意向分布" data={intentDonut} />
        <DonutCard title="平台内容占比" data={platformDonut} />
        <DonutCard title="素材下载概况" data={mediaDonut} />
      </div>

      {/* 热门内容 / 热门关键词 */}
      <div
        className="veltrix-enter grid grid-cols-1 gap-4 lg:grid-cols-2"
        style={{ animationDelay: "280ms" }}
      >
        <div className="veltrix-card p-5">
          <h3 className="mb-3 text-sm font-semibold text-foreground">
            热门内容 Top {data?.hotContents.length ?? 0}
          </h3>
          {data && data.hotContents.length > 0 ? (
            <AutoScrollList count={data.hotContents.length}>
              <ol className="space-y-2">
              {data.hotContents.map((c, i) => (
                <li key={i} className="flex items-center gap-2 text-sm">
                  <span className="w-5 shrink-0 text-center font-mono text-xs text-muted-foreground">
                    {i + 1}
                  </span>
                  <span className="shrink-0 rounded bg-muted px-1 py-0.5 text-[11px] text-muted-foreground">
                    {platformName(c.platform)}
                  </span>
                  <span className="min-w-0 flex-1 truncate text-foreground">
                    {c.title || "(无标题)"}
                  </span>
                  <span className="shrink-0 font-mono text-xs text-muted-foreground">
                    {c.likeCount.toLocaleString()} 赞
                  </span>
                </li>
              ))}
              </ol>
            </AutoScrollList>
          ) : (
            <EmptyLine text="暂无内容" />
          )}
        </div>

        <div className="veltrix-card p-5">
          <h3 className="mb-3 text-sm font-semibold text-foreground">
            热门话题 Top {data?.topKeywords.length ?? 0}
          </h3>
          {data && data.topKeywords.length > 0 ? (
            <AutoScrollList count={data.topKeywords.length}>
              <ul className="space-y-2.5">
              {data.topKeywords.map((k, i) => {
                const max = data.topKeywords[0]?.count || 1;
                return (
                  <li key={k.keyword} className="text-sm">
                    <div className="flex items-center justify-between gap-2">
                      <span className="min-w-0 truncate text-foreground">
                        {i + 1}. {k.keyword}
                      </span>
                      <span className="shrink-0 font-mono text-xs text-muted-foreground">
                        {k.count.toLocaleString()}
                      </span>
                    </div>
                    <div className="mt-1 h-1.5 overflow-hidden rounded-full bg-muted">
                      <div
                        className="h-full rounded-full bg-primary"
                        style={{ width: `${(k.count / max) * 100}%` }}
                      />
                    </div>
                  </li>
                );
              })}
              </ul>
            </AutoScrollList>
          ) : (
            <EmptyLine text="暂无话题" />
          )}
        </div>
      </div>
    </div>
  );
}

// 概览卡片:彩色图标块 + 大数字 + 平台细分 chips
const OverviewCard = memo(function OverviewCard({
  icon: Icon,
  label,
  hint,
  total,
  byPlatform,
  platformName,
  color,
  bg,
  kindStats,
}: {
  icon: ComponentType<{ className?: string }>;
  label: string;
  hint: string;
  total: number | undefined;
  byPlatform: PlatformCount[] | undefined;
  platformName: (id: string) => string;
  color: string;
  bg: string;
  kindStats?: { video: number; image: number };
}) {
  return (
    <div className="veltrix-card p-5">
      <div className="flex items-center gap-3">
        <div
          className={`flex size-10 shrink-0 items-center justify-center rounded-xl ${bg} ${color}`}
        >
          <Icon className="size-5" />
        </div>
        <span className="text-sm font-medium text-muted-foreground">
          {label}
        </span>
        <span className="ml-auto font-mono text-2xl font-semibold text-foreground">
          <AnimatedNumber value={total} />
        </span>
      </div>
      <div className="mt-3 border-t pt-3 text-xs">
        {/* 内容形态 */}
        {kindStats && (
          <div className="grid grid-cols-2 gap-x-4 gap-y-1">
            <KV label="视频" value={kindStats.video} />
            <KV label="图文" value={kindStats.image} />
          </div>
        )}
        {/* 分割线:形态与平台分隔 */}
        {kindStats && byPlatform && byPlatform.length > 0 && (
          <div className="my-2 border-t" />
        )}
        {/* 平台分布 */}
        {byPlatform && byPlatform.length > 0 ? (
          <div className="grid grid-cols-2 gap-x-4 gap-y-1">
            {byPlatform.map((p) => (
              <KV
                key={p.platform}
                label={platformName(p.platform)}
                value={p.count}
              />
            ))}
          </div>
        ) : kindStats ? null : (
          <span className="text-muted-foreground">{hint}</span>
        )}
      </div>
    </div>
  );
});

// 形态 / 平台共用键值项:标签左、数字右对齐,使两行数字成列对齐
const KV = memo(function KV({ label, value }: { label: string; value: number }) {
  return (
    <div className="flex items-center justify-between gap-2">
      <span className="truncate text-muted-foreground">{label}</span>
      <span className="shrink-0 font-mono font-medium text-foreground">
        <AnimatedNumber value={value} />
      </span>
    </div>
  );
});

// 今日采集指标:图标块 + 大数字 + 标签 + 环比
const TodayMetric = memo(function TodayMetric({
  icon: Icon,
  label,
  value,
  delta,
  color,
  bg,
}: {
  icon: ComponentType<{ className?: string }>;
  label: string;
  value: number | undefined;
  delta: number | undefined;
  color: string;
  bg: string;
}) {
  return (
    <div className="flex items-center gap-3">
      <div
        className={`flex size-10 shrink-0 items-center justify-center rounded-xl ${bg} ${color}`}
      >
        <Icon className="size-5" />
      </div>
      <div className="min-w-0">
        <div className="font-mono text-xl font-semibold leading-none text-foreground">
          <AnimatedNumber value={value} />
        </div>
        <div className="mt-1 flex flex-wrap items-center gap-x-1.5 text-[11px] text-muted-foreground">
          <span>{label}</span>
          <DeltaBadge delta={delta} />
        </div>
      </div>
    </div>
  );
});

// 任务状态格子:状态色图标 + 数字 + 标签
const StatusTile = memo(function StatusTile({
  icon: Icon,
  label,
  value,
  color,
  bg,
}: {
  icon: ComponentType<{ className?: string }>;
  label: string;
  value: number | undefined;
  color: string;
  bg: string;
}) {
  return (
    <div className="flex h-full flex-col items-center justify-center gap-1.5 text-center">
      <div
        className={`flex size-14 items-center justify-center rounded-xl ${bg} ${color}`}
      >
        <Icon className="size-7" />
      </div>
      <div className="font-mono text-xl font-semibold leading-none text-foreground">
        <AnimatedNumber value={value} />
      </div>
      <div className="text-[11px] text-muted-foreground">{label}</div>
    </div>
  );
});

// 环比徽章(较昨日)
const DeltaBadge = memo(function DeltaBadge({ delta }: { delta: number | undefined }) {
  if (delta === undefined) return null;
  if (delta === 0)
    return <span className="text-muted-foreground">较昨日持平</span>;
  const up = delta > 0;
  return (
    <span
      className={
        up
          ? "text-emerald-600 dark:text-emerald-400"
          : "text-rose-600 dark:text-rose-400"
      }
    >
      {up ? "↑" : "↓"} {Math.abs(delta).toLocaleString()} 较昨日
    </span>
  );
});

// 环形图卡片(标题 + 甜甜圈 + 图例)
const DonutCard = memo(function DonutCard({
  title,
  data,
}: {
  title: string;
  data: { label: string; value: number; color: string }[];
}) {
  const total = data.reduce((s, d) => s + d.value, 0);
  return (
    <div className="veltrix-card p-5">
      <h3 className="mb-3 text-sm font-semibold text-foreground">{title}</h3>
      <div className="flex items-center gap-4">
        <DonutChart data={data} />
        <div className="flex flex-1 flex-col gap-2 text-xs">
          {data.map((d) => {
            const pct = total > 0 ? Math.round((d.value / total) * 100) : 0;
            return (
              <div key={d.label} className="flex items-center gap-2">
                <span
                  className="size-2.5 shrink-0 rounded-sm"
                  style={{ background: d.color }}
                />
                <span className="truncate text-muted-foreground">
                  {d.label}
                </span>
                <span className="ml-auto shrink-0 font-mono text-foreground">
                  <AnimatedNumber value={d.value} />
                </span>
                <span className="w-9 shrink-0 text-right font-mono text-muted-foreground">
                  {pct}%
                </span>
              </div>
            );
          })}
          {data.length === 0 && (
            <span className="text-muted-foreground">暂无数据</span>
          )}
        </div>
      </div>
    </div>
  );
});

const EmptyLine = memo(function EmptyLine({ text, className }: { text: string; className?: string }) {
  return (
    <div
      className={`flex flex-col items-center justify-center gap-2 text-muted-foreground ${className ?? "h-32"}`}
    >
      <Inbox className="size-7 opacity-40" />
      <span className="text-sm">{text}</span>
    </div>
  );
});
