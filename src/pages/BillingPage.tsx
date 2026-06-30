import { useCallback, useEffect, useState } from "react";
import {
  Activity,
  Coins,
  MessageSquare,
  Sparkles,
} from "lucide-react";
import { type DateRange } from "react-day-picker";
import { api, type BillingOverview, type PlatformSeries, type ModelTrendSeries } from "@/lib/api";
import { DateRangePicker } from "@/components/DateRangePicker";
import { StatCard } from "@/components/StatCard";
import { DonutChart } from "@/components/charts/DonutChart";
import { MultiTrendChart } from "@/components/charts/MultiTrendChart";

const CHART_COLORS = [
  "#0ea5e9",
  "#8b5cf6",
  "#10b981",
  "#f59e0b",
  "#ef4444",
  "#ec4899",
  "#14b8a6",
  "#f97316",
];

function toPlatformSeries(list: ModelTrendSeries[]): PlatformSeries[] {
  return list.map((s) => ({
    platform: s.model,
    counts: s.values,
    contents: s.values,
    comments: [],
  }));
}

function EmptyLine({
  text,
  className,
}: {
  text: string;
  className?: string;
}) {
  return (
    <div
      className={`flex items-center justify-center text-sm text-muted-foreground ${className ?? "h-52"}`}
    >
      {text}
    </div>
  );
}

function toTimestamps(r?: DateRange) {
  const start = r?.from ? Math.floor(r.from.getTime() / 1000) : undefined;
  const end = r?.to ? Math.floor(r.to.getTime() / 1000) + 86400 : undefined;
  return { start, end };
}

function useBillingData() {
  const [data, setData] = useState<BillingOverview | null>(null);
  const [range, setRange] = useState<DateRange | undefined>();
  const [loading, setLoading] = useState(false);

  const load = useCallback(async (r?: DateRange) => {
    setLoading(true);
    try {
      const { start, end } = toTimestamps(r);
      const result = await api.billingOverview(start, end);
      setData(result);
    } catch {
      // 静默处理,由调用方决定是否展示
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    load(range);
  }, []);

  const handleChange = useCallback(
    (r: DateRange | undefined) => {
      setRange(r);
      load(r);
    },
    [load],
  );

  const handleReset = useCallback(() => {
    setRange(undefined);
    load(undefined);
  }, [load]);

  return { data, range, loading, handleChange, handleReset };
}

export function BillingPage() {
  const summary = useBillingData();
  const tokenTrend = useBillingData();
  const reqTrend = useBillingData();
  const tokenDist = useBillingData();
  const reqDist = useBillingData();
  const detail = useBillingData();

  return (
    <div className="veltrix-no-scrollbar flex min-h-0 flex-1 flex-col gap-4 overflow-y-auto p-1">
      {summary.data && (
        <div
          className="veltrix-enter grid grid-cols-4 gap-4"
          style={{ animationDelay: "0ms" }}
        >
          <StatCard
            label="总 Token"
            value={summary.data.totalTokens.toLocaleString()}
            icon={Coins}
            delay={0}
          />
          <StatCard
            label="输入 Token"
            value={summary.data.totalPromptTokens.toLocaleString()}
            icon={Sparkles}
            delay={60}
          />
          <StatCard
            label="输出 Token"
            value={summary.data.totalCompletionTokens.toLocaleString()}
            icon={Activity}
            delay={120}
          />
          <StatCard
            label="总请求次数"
            value={summary.data.totalRequests.toLocaleString()}
            icon={MessageSquare}
            delay={180}
          />
        </div>
      )}

      <div className="grid gap-4 lg:grid-cols-2">
        <div
          className="veltrix-enter veltrix-card p-6"
          style={{ animationDelay: "60ms" }}
        >
          <div className="mb-4 flex flex-wrap items-center justify-between gap-2">
            <h2 className="text-sm font-semibold text-foreground">
              Token 消耗趋势 · 按模型
            </h2>
            <DateRangePicker
              range={tokenTrend.range}
              onChange={tokenTrend.handleChange}
              onReset={tokenTrend.handleReset}
            />
          </div>
          {tokenTrend.data && (
            <MultiTrendChart
              dates={tokenTrend.data.tokenTrendDates}
              series={toPlatformSeries(tokenTrend.data.tokenTrendSeries)}
              platformName={(id) => id}
              metric="contents"
              hideAxisLabel
              valueLabel="Token"
              renderEmpty={(props) => <EmptyLine {...props} />}
            />
          )}
        </div>
        <div
          className="veltrix-enter veltrix-card p-6"
          style={{ animationDelay: "100ms" }}
        >
          <div className="mb-4 flex flex-wrap items-center justify-between gap-2">
            <h2 className="text-sm font-semibold text-foreground">
              请求次数趋势 · 按模型
            </h2>
            <DateRangePicker
              range={reqTrend.range}
              onChange={reqTrend.handleChange}
              onReset={reqTrend.handleReset}
            />
          </div>
          {reqTrend.data && (
            <MultiTrendChart
              dates={reqTrend.data.requestTrendDates}
              series={toPlatformSeries(reqTrend.data.requestTrendSeries)}
              platformName={(id) => id}
              metric="contents"
              hideAxisLabel
              valueLabel="请求"
              renderEmpty={(props) => <EmptyLine {...props} />}
            />
          )}
        </div>
      </div>

      <div className="grid gap-4 lg:grid-cols-2">
        <div
          className="veltrix-enter veltrix-card p-6"
          style={{ animationDelay: "140ms" }}
        >
          <div className="mb-4 flex flex-wrap items-center justify-between gap-2">
            <h2 className="text-sm font-semibold text-foreground">
              Token 分布 · 按模型
            </h2>
            <DateRangePicker
              range={tokenDist.range}
              onChange={tokenDist.handleChange}
              onReset={tokenDist.handleReset}
            />
          </div>
          {tokenDist.data && (
            <div className="flex items-center gap-6">
              <DonutChart
                data={tokenDist.data.byModel.map((m, i) => ({
                  label: m.model,
                  value: m.totalTokens,
                  color: CHART_COLORS[i % CHART_COLORS.length],
                }))}
                size={140}
              />
              <div className="flex-1 space-y-1.5 text-xs">
                {tokenDist.data.byModel.map((m, i) => (
                  <div key={m.model} className="flex items-center gap-2">
                    <span
                      className="size-2.5 shrink-0 rounded-full"
                      style={{ background: CHART_COLORS[i % CHART_COLORS.length] }}
                    />
                    <span className="flex-1 truncate text-muted-foreground">
                      {m.model}
                    </span>
                    <span className="font-mono text-foreground">
                      {m.totalTokens.toLocaleString()}
                    </span>
                  </div>
                ))}
              </div>
            </div>
          )}
        </div>

        <div
          className="veltrix-enter veltrix-card p-6"
          style={{ animationDelay: "180ms" }}
        >
          <div className="mb-4 flex flex-wrap items-center justify-between gap-2">
            <h2 className="text-sm font-semibold text-foreground">
              请求次数分布 · 按模型
            </h2>
            <DateRangePicker
              range={reqDist.range}
              onChange={reqDist.handleChange}
              onReset={reqDist.handleReset}
            />
          </div>
          {reqDist.data && (
            <div className="flex items-center gap-6">
              <DonutChart
                data={reqDist.data.requestByModel.map((m, i) => ({
                  label: m.model,
                  value: m.count,
                  color: CHART_COLORS[i % CHART_COLORS.length],
                }))}
                size={140}
              />
              <div className="flex-1 space-y-1.5 text-xs">
                {reqDist.data.requestByModel.map((m, i) => (
                  <div key={m.model} className="flex items-center gap-2">
                    <span
                      className="size-2.5 shrink-0 rounded-full"
                      style={{ background: CHART_COLORS[i % CHART_COLORS.length] }}
                    />
                    <span className="flex-1 truncate text-muted-foreground">
                      {m.model}
                    </span>
                    <span className="font-mono text-foreground">
                      {m.count.toLocaleString()}
                    </span>
                  </div>
                ))}
              </div>
            </div>
          )}
        </div>
      </div>

      {/* 各模型明细:用 min-h 兜底而非 min-h-0,避免竖向空间不足时被 flex 压缩到 0 高度看不见
          (1920*1080 下上方卡片+图表占满时会触发);空间不足则外层 overflow-y-auto 出滚动条 */}
      <div
        className="veltrix-enter veltrix-card flex min-h-[20rem] flex-1 flex-col p-6"
        style={{ animationDelay: "220ms" }}
      >
        <div className="mb-4 flex flex-wrap items-center justify-between gap-2">
          <h2 className="text-sm font-semibold text-foreground">
            各模型明细
          </h2>
          <DateRangePicker
            range={detail.range}
            onChange={detail.handleChange}
            onReset={detail.handleReset}
          />
        </div>
        <div className="min-h-0 flex-1 overflow-auto veltrix-thin-scrollbar">
          {detail.data && (
            <table className="w-full text-sm">
              <thead className="sticky top-0 z-10 bg-card">
                <tr className="border-b text-left text-muted-foreground">
                  <th className="pb-2 pr-4 font-medium">模型</th>
                  <th className="pb-2 pr-4 text-right font-medium">输入 Token</th>
                  <th className="pb-2 pr-4 text-right font-medium">输出 Token</th>
                  <th className="pb-2 pr-4 text-right font-medium">合计 Token</th>
                  <th className="pb-2 pr-4 text-right font-medium">请求次数</th>
                  <th className="pb-2 text-right font-medium">最后请求时间</th>
                </tr>
              </thead>
              <tbody>
                {detail.data.byModel.map((m, i) => {
                  const req = detail.data!.requestByModel.find(
                    (r) => r.model === m.model,
                  );
                  const lastReq = m.lastRequestedAt
                    ? new Date(m.lastRequestedAt * 1000).toLocaleString()
                    : "--";
                  return (
                    <tr key={m.model} className="border-b last:border-0">
                      <td className="py-2 pr-4">
                        <span className="flex items-center gap-2">
                          <span
                            className="size-2.5 rounded-full"
                            style={{
                              background: CHART_COLORS[i % CHART_COLORS.length],
                            }}
                          />
                          {m.model}
                        </span>
                      </td>
                      <td className="py-2 pr-4 text-right font-mono">
                        {m.promptTokens.toLocaleString()}
                      </td>
                      <td className="py-2 pr-4 text-right font-mono">
                        {m.completionTokens.toLocaleString()}
                      </td>
                      <td className="py-2 pr-4 text-right font-mono">
                        {m.totalTokens.toLocaleString()}
                      </td>
                      <td className="py-2 pr-4 text-right font-mono">
                        {(req?.count ?? 0).toLocaleString()}
                      </td>
                      <td className="py-2 text-right text-muted-foreground">
                        {lastReq}
                      </td>
                    </tr>
                  );
                })}
                {detail.data.byModel.length === 0 && (
                  <tr>
                    <td
                      colSpan={6}
                      className="py-8 text-center text-muted-foreground"
                    >
                      该区间暂无用量数据
                    </td>
                  </tr>
                )}
              </tbody>
            </table>
          )}
        </div>
      </div>
    </div>
  );
}
