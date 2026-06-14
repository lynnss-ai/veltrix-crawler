import {
  Fragment,
  useEffect,
  useRef,
  useState,
  type ComponentType,
  type MouseEvent as ReactMouseEvent,
  type ReactNode,
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
  type PlatformSeries,
} from "@/lib/api";
import { ErrorBanner } from "@/components/ErrorBanner";
import { Button } from "@/components/ui/button";
import { Calendar } from "@/components/ui/calendar";
import {
  Popover,
  PopoverContent,
  PopoverTrigger,
} from "@/components/ui/popover";

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

// 用 Catmull-Rom 转三次贝塞尔生成平滑曲线路径(比折线更顺滑)。
// yMin/yMax 为绘图区上下边界:控制点 y 夹在区间内,避免平滑曲线在 0 值点上下越界(冲出 x 轴)
function smoothPath(
  pts: [number, number][],
  yMin: number,
  yMax: number,
): string {
  if (pts.length === 0) return "";
  if (pts.length < 3) {
    return pts
      .map(([x, y], i) => `${i === 0 ? "M" : "L"}${x.toFixed(1)},${y.toFixed(1)}`)
      .join(" ");
  }
  const clampY = (y: number) => Math.max(yMin, Math.min(yMax, y));
  let d = `M${pts[0][0].toFixed(1)},${pts[0][1].toFixed(1)}`;
  for (let i = 0; i < pts.length - 1; i++) {
    const p0 = pts[i - 1] ?? pts[i];
    const p1 = pts[i];
    const p2 = pts[i + 1];
    const p3 = pts[i + 2] ?? p2;
    const cp1x = p1[0] + (p2[0] - p0[0]) / 6;
    const cp1y = clampY(p1[1] + (p2[1] - p0[1]) / 6);
    const cp2x = p2[0] - (p3[0] - p1[0]) / 6;
    const cp2y = clampY(p2[1] - (p3[1] - p1[1]) / 6);
    d += ` C${cp1x.toFixed(1)},${cp1y.toFixed(1)} ${cp2x.toFixed(1)},${cp2y.toFixed(1)} ${p2[0].toFixed(1)},${p2[1].toFixed(1)}`;
  }
  return d;
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
    api.listPlatforms().then(setPlatforms).catch(() => {});
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
                        {(stat?.contents ?? 0).toLocaleString()}
                      </span>
                    </span>
                    <span className="flex w-16 justify-between">
                      <span className="text-muted-foreground">评论</span>
                      <span className="font-mono text-foreground">
                        {(stat?.comments ?? 0).toLocaleString()}
                      </span>
                    </span>
                  </div>
                );
              })}
            </div>
          )}
        </div>
        <div className="veltrix-card p-5">
          <h3 className="mb-4 flex items-center gap-2 text-sm font-semibold text-foreground">
            <ListChecks className="size-4 text-muted-foreground" />
            任务状态
          </h3>
          <div className="grid grid-cols-4 gap-2">
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
            数据采集趋势 · 各平台按天(内容 + 评论)
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
          <div className="mb-3 flex flex-wrap gap-4 text-xs text-muted-foreground">
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
function OverviewCard({
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
          {total?.toLocaleString() ?? "—"}
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
}

// 形态 / 平台共用键值项:标签左、数字右对齐,使两行数字成列对齐
function KV({ label, value }: { label: string; value: number }) {
  return (
    <div className="flex items-center justify-between gap-2">
      <span className="truncate text-muted-foreground">{label}</span>
      <span className="shrink-0 font-mono font-medium text-foreground">
        {value.toLocaleString()}
      </span>
    </div>
  );
}

// 今日采集指标:图标块 + 大数字 + 标签 + 环比
function TodayMetric({
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
          {value?.toLocaleString() ?? "—"}
        </div>
        <div className="mt-1 flex flex-wrap items-center gap-x-1.5 text-[11px] text-muted-foreground">
          <span>{label}</span>
          <DeltaBadge delta={delta} />
        </div>
      </div>
    </div>
  );
}

// 任务状态格子:状态色图标 + 数字 + 标签
function StatusTile({
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
    <div className="flex flex-col items-center gap-1.5 text-center">
      <div
        className={`flex size-11 items-center justify-center rounded-xl ${bg} ${color}`}
      >
        <Icon className="size-6" />
      </div>
      <div className="font-mono text-xl font-semibold leading-none text-foreground">
        {value?.toLocaleString() ?? "—"}
      </div>
      <div className="text-[11px] text-muted-foreground">{label}</div>
    </div>
  );
}

// 环比徽章(较昨日)
function DeltaBadge({ delta }: { delta: number | undefined }) {
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
}

// 环形图卡片(标题 + 甜甜圈 + 图例)
function DonutCard({
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
                  {d.value.toLocaleString()}
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
}

// 甜甜圈图(纯 SVG):各分段按占比绘制圆环弧
function DonutChart({
  data,
  size = 128,
}: {
  data: { label: string; value: number; color: string }[];
  size?: number;
}) {
  const total = data.reduce((s, d) => s + d.value, 0);
  const r = size / 2 - 12;
  const c = 2 * Math.PI * r;
  let acc = 0;
  return (
    <svg
      viewBox={`0 0 ${size} ${size}`}
      className="shrink-0 text-muted-foreground"
      style={{ width: size, height: size }}
    >
      <circle
        cx={size / 2}
        cy={size / 2}
        r={r}
        fill="none"
        stroke="currentColor"
        strokeOpacity={0.45}
        strokeWidth={12}
      />
      {total > 0 && (
        <g transform={`rotate(-90 ${size / 2} ${size / 2})`}>
          {data.map((d, i) => {
            if (d.value === 0) return null;
            const frac = d.value / total;
            const seg = (
              <circle
                key={i}
                cx={size / 2}
                cy={size / 2}
                r={r}
                fill="none"
                stroke={d.color}
                strokeWidth={12}
                strokeDasharray={`${(frac * c).toFixed(2)} ${c.toFixed(2)}`}
                strokeDashoffset={(-acc * c).toFixed(2)}
              />
            );
            acc += frac;
            return seg;
          })}
        </g>
      )}
      <text
        x={size / 2}
        y={size / 2 - 2}
        textAnchor="middle"
        className="fill-foreground"
        fontSize={22}
        fontWeight={600}
      >
        {total.toLocaleString()}
      </text>
      <text
        x={size / 2}
        y={size / 2 + 15}
        textAnchor="middle"
        className="fill-muted-foreground"
        fontSize={10}
      >
        合计
      </text>
    </svg>
  );
}

function EmptyLine({ text, className }: { text: string; className?: string }) {
  return (
    <div
      className={`flex flex-col items-center justify-center gap-2 text-muted-foreground ${className ?? "h-32"}`}
    >
      <Inbox className="size-7 opacity-40" />
      <span className="text-sm">{text}</span>
    </div>
  );
}

// 自动滚动列表:缓慢上滚,到底停顿后回到顶部循环(单份内容,不重复);鼠标悬停暂停。
function AutoScrollList({
  count,
  children,
}: {
  count: number;
  children: ReactNode;
}) {
  const ref = useRef<HTMLDivElement>(null);
  // 用 ref 存暂停态:onMouseEnter/Leave 改它,rAF 循环里读它,避免重建动画
  const pausedRef = useRef(false);
  useEffect(() => {
    const el = ref.current;
    if (!el) return;
    let raf = 0;
    let bottomFrames = 0; // 到底后停留的帧数,停一会再回顶,避免突兀
    const tick = () => {
      if (el && !pausedRef.current && el.scrollHeight > el.clientHeight) {
        if (el.scrollTop >= el.scrollHeight - el.clientHeight - 1) {
          bottomFrames += 1;
          if (bottomFrames > 90) {
            el.scrollTop = 0; // 到底停约 1.5s 后回到顶部
            bottomFrames = 0;
          }
        } else {
          el.scrollTop += 0.4;
        }
      }
      raf = requestAnimationFrame(tick);
    };
    raf = requestAnimationFrame(tick);
    return () => cancelAnimationFrame(raf);
  }, [count]);
  return (
    <div
      ref={ref}
      onMouseEnter={() => {
        pausedRef.current = true;
      }}
      onMouseLeave={() => {
        pausedRef.current = false;
      }}
      className="veltrix-no-scrollbar h-80 overflow-y-auto"
    >
      {children}
    </div>
  );
}

// 多平台采集趋势折线图(纯 SVG):每平台一条平滑曲线,支持鼠标悬停查看当天各平台数值。
function MultiTrendChart({
  dates,
  series,
  platformName,
}: {
  dates: string[];
  series: PlatformSeries[];
  platformName: (id: string) => string;
}) {
  const [hover, setHover] = useState<number | null>(null);
  const hasData =
    dates.length > 0 && series.some((s) => s.counts.some((c) => c > 0));
  // 区间完全没有日期才退回纯占位;有日期则照常画坐标轴,仅缺曲线
  if (dates.length === 0) {
    return <EmptyLine text="该区间暂无采集数据" className="h-52" />;
  }

  const W = 760;
  const H = 240;
  const padL = 32;
  const padR = 16;
  const padT = 16;
  const padB = 28;
  const innerW = W - padL - padR;
  const innerH = H - padT - padB;
  const n = dates.length;
  const maxV = Math.max(1, ...series.flatMap((s) => s.counts));

  const px = (i: number) =>
    padL + (n === 1 ? innerW / 2 : (i * innerW) / (n - 1));
  const py = (v: number) => padT + innerH * (1 - v / maxV);
  const linePath = (counts: number[]) =>
    smoothPath(
      counts.map((v, i) => [px(i), py(v)] as [number, number]),
      padT,
      padT + innerH,
    );

  const ticks = [0, 0.25, 0.5, 0.75, 1].map((f) => Math.round(maxV * f));
  const step = Math.max(1, Math.ceil(n / 8));

  // 鼠标移动 → 命中最近日期下标(viewBox 与渲染宽度按比例换算)
  const onMove = (e: ReactMouseEvent<SVGSVGElement>) => {
    const rect = e.currentTarget.getBoundingClientRect();
    if (rect.width === 0) return;
    const vbX = ((e.clientX - rect.left) / rect.width) * W;
    const idx = Math.round((vbX - padL) / (innerW / Math.max(1, n - 1)));
    setHover(Math.max(0, Math.min(n - 1, idx)));
  };

  return (
    <div className="relative">
      <svg
        viewBox={`0 0 ${W} ${H}`}
        className="h-auto w-full text-border"
        preserveAspectRatio="xMidYMid meet"
        onMouseMove={onMove}
        onMouseLeave={() => setHover(null)}
      >
        {ticks.map((t, i) => {
          const yy = py(t);
          return (
            <g key={`g${i}`}>
              <line
                x1={padL}
                y1={yy}
                x2={W - padR}
                y2={yy}
                stroke="currentColor"
                strokeOpacity={0.15}
              />
              <text
                x={padL - 6}
                y={yy + 3}
                textAnchor="end"
                className="fill-muted-foreground"
                fontSize={9}
              >
                {t}
              </text>
            </g>
          );
        })}

        {dates.map((d, i) =>
          i % step === 0 || i === n - 1 ? (
            <text
              key={`x${i}`}
              x={px(i)}
              y={H - 8}
              textAnchor="middle"
              className="fill-muted-foreground"
              fontSize={9}
            >
              {d}
            </text>
          ) : null,
        )}

        {series.map((s, si) => (
          <path
            key={s.platform}
            d={linePath(s.counts)}
            fill="none"
            stroke={platformColor(s.platform, si)}
            strokeWidth={1.5}
            strokeLinejoin="round"
            strokeLinecap="round"
          />
        ))}

        {/* 有坐标轴但区间内无任何采集数据时,中央叠加提示 */}
        {!hasData && (
          <text
            x={W / 2}
            y={padT + innerH / 2}
            textAnchor="middle"
            className="fill-muted-foreground"
            fontSize={9}
          >
            暂无采集数据
          </text>
        )}

        {/* 悬停:竖线 + 各平台数据点高亮 */}
        {hover !== null && (
          <>
            <line
              x1={px(hover)}
              y1={padT}
              x2={px(hover)}
              y2={H - padB}
              stroke="currentColor"
              strokeOpacity={0.4}
              strokeDasharray="3 3"
            />
            {series.map((s, si) => (
              <circle
                key={`h${si}`}
                cx={px(hover)}
                cy={py(s.counts[hover] ?? 0)}
                r={2.5}
                fill={platformColor(s.platform, si)}
                stroke="white"
                strokeWidth={1}
              />
            ))}
          </>
        )}
      </svg>

      {/* 悬停浮窗:当天各平台数值 */}
      {hover !== null && (
        <div
          className="pointer-events-none absolute top-1 z-10 w-max max-w-[260px] whitespace-nowrap rounded-md border bg-popover px-2.5 py-1.5 text-xs shadow-md"
          style={{
            left: `${(px(hover) / W) * 100}%`,
            // 靠左点左对齐、靠右点右对齐、中间居中,避免浮窗超出图表边缘被截断
            transform: `translateX(${
              hover / Math.max(1, n - 1) < 0.18
                ? "0%"
                : hover / Math.max(1, n - 1) > 0.82
                  ? "-100%"
                  : "-50%"
            })`,
          }}
        >
          <div className="mb-1 font-medium text-foreground">{dates[hover]}</div>
          <div className="grid grid-cols-[1fr_auto_auto] items-center gap-x-4 gap-y-1">
            {series.map((s, si) => (
              <Fragment key={`t${si}`}>
                <span className="flex items-center gap-1.5">
                  <span
                    className="size-2 shrink-0 rounded-full"
                    style={{ background: platformColor(s.platform, si) }}
                  />
                  <span className="text-muted-foreground">
                    {platformName(s.platform)}
                  </span>
                </span>
                <span className="text-muted-foreground">
                  内容{" "}
                  <span className="font-mono text-foreground">
                    {s.contents[hover] ?? 0}
                  </span>
                </span>
                <span className="text-muted-foreground">
                  评论{" "}
                  <span className="font-mono text-foreground">
                    {s.comments[hover] ?? 0}
                  </span>
                </span>
              </Fragment>
            ))}
          </div>
        </div>
      )}
    </div>
  );
}
