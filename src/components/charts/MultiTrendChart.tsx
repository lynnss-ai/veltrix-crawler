import { Fragment, memo, useState, type ReactNode, type MouseEvent as ReactMouseEvent } from "react";
import { platformColorHex } from "@/lib/platforms";
import type { PlatformSeries } from "@/lib/api";

// 多平台趋势线配色(超出则循环复用)
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

// 平台配色:三大平台用官方品牌色,其余平台按序号回退调色板
function platformColor(platform: string, index: number): string {
  return platformColorHex(platform) ?? PALETTE[index % PALETTE.length];
}

export const MultiTrendChart = memo(function MultiTrendChart({
  dates,
  series,
  platformName,
  metric = "contents",
  compact = false,
  hideAxisLabel = false,
  valueLabel,
  renderEmpty,
}: {
  dates: string[];
  series: PlatformSeries[];
  platformName: (id: string) => string;
  metric?: "contents" | "comments";
  compact?: boolean;
  hideAxisLabel?: boolean;
  valueLabel?: string;
  renderEmpty: (props: { text: string; className?: string }) => ReactNode;
}) {
  const [hover, setHover] = useState<number | null>(null);
  // 区间完全没有日期才退回纯占位;有日期则照常画坐标轴,仅缺曲线
  if (dates.length === 0) {
    return renderEmpty({ text: "该区间暂无采集数据", className: "h-52" });
  }

  // 主绘制字段:内容图(默认)或评论图(独立小图)。两图同款平滑折线,仅数据源 + 轴名不同
  const valuesOf = (s: PlatformSeries) =>
    metric === "comments" ? s.comments : s.contents;
  const axisLabel = metric === "comments" ? "评论" : "内容";

  const W = 760;
  const H = compact ? 176 : 240;
  const padL = 36;
  const padR = 16;
  const padT = 26; // 顶部留白:给轴名腾位,与最高刻度数字拉开
  const padB = 28;
  const innerW = W - padL - padR;
  const innerH = H - padT - padB;
  const n = dates.length;

  // 只画有数据的平台:全 0 平台会在底部叠成误导性的 0 基线。保留原始下标 si 用于配色,与图例一致
  const active = series
    .map((s, si) => ({ s, si }))
    .filter(({ s }) => valuesOf(s).some((c) => c > 0));
  const hasData = active.length > 0;

  const maxV = Math.max(1, ...active.flatMap(({ s }) => valuesOf(s)));
  const px = (i: number) =>
    padL + (n === 1 ? innerW / 2 : (i * innerW) / (n - 1));
  const py = (v: number) => padT + innerH * (1 - v / maxV);

  // Catmull-Rom→三次贝塞尔,但把控制点 y 钳在本段两端点之间:平滑且不过冲、峰落在数据点上,
  // 避免普通平滑把孤立高点鼓成偏移的钟形(评论那版钟形的根因)
  const smooth = (pts: [number, number][]): string => {
    if (pts.length === 0) return "";
    if (pts.length === 1) {
      return `M${pts[0][0].toFixed(1)},${pts[0][1].toFixed(1)}`;
    }
    const clampSeg = (y: number, a: number, b: number) =>
      Math.max(Math.min(a, b), Math.min(Math.max(a, b), y));
    let d = `M${pts[0][0].toFixed(1)},${pts[0][1].toFixed(1)}`;
    for (let i = 0; i < pts.length - 1; i++) {
      const p0 = pts[i - 1] ?? pts[i];
      const p1 = pts[i];
      const p2 = pts[i + 1];
      const p3 = pts[i + 2] ?? p2;
      const cp1x = p1[0] + (p2[0] - p0[0]) / 6;
      const cp2x = p2[0] - (p3[0] - p1[0]) / 6;
      const cp1y = clampSeg(p1[1] + (p2[1] - p0[1]) / 6, p1[1], p2[1]);
      const cp2y = clampSeg(p2[1] - (p3[1] - p1[1]) / 6, p1[1], p2[1]);
      d += ` C${cp1x.toFixed(1)},${cp1y.toFixed(1)} ${cp2x.toFixed(1)},${cp2y.toFixed(1)} ${p2[0].toFixed(1)},${p2[1].toFixed(1)}`;
    }
    return d;
  };
  const linePath = (counts: number[]) =>
    smooth(counts.map((v, i) => [px(i), py(v)] as [number, number]));
  // 面积:折线下沿沿底边闭合
  const areaPath = (counts: number[]) => {
    const yb = py(0).toFixed(1);
    return `${linePath(counts)} L${px(n - 1).toFixed(1)},${yb} L${px(0).toFixed(1)},${yb} Z`;
  };

  const fractions = [0, 0.25, 0.5, 0.75, 1];
  const ticks = fractions.map((f) => Math.round(maxV * f));
  const tickY = (f: number) => padT + innerH * (1 - f);
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
        <defs>
          {active.map(({ s, si }) => (
            <linearGradient
              key={`grad-${si}`}
              id={`trend-grad-${metric}-${s.platform}`}
              x1="0"
              y1="0"
              x2="0"
              y2="1"
            >
              <stop
                offset="0%"
                stopColor={platformColor(s.platform, si)}
                stopOpacity={0.3}
              />
              <stop
                offset="100%"
                stopColor={platformColor(s.platform, si)}
                stopOpacity={0}
              />
            </linearGradient>
          ))}
        </defs>

        {/* 横向网格 + 内容刻度(单轴) */}
        {fractions.map((f, i) => {
          const yy = tickY(f);
          return (
            <g key={`g${i}`}>
              <line
                x1={padL}
                y1={yy}
                x2={W - padR}
                y2={yy}
                stroke="currentColor"
                strokeOpacity={0.12}
              />
              <text
                x={padL - 6}
                y={yy + 3}
                textAnchor="end"
                className="fill-muted-foreground"
                fontSize={8}
              >
                {ticks[i]}
              </text>
            </g>
          );
        })}

        {/* 轴名:固定在顶部留白处,与最高刻度数字隔开 */}
        {!hideAxisLabel && (
          <text
            x={padL - 6}
            y={10}
            textAnchor="end"
            className="fill-muted-foreground"
            fontSize={8}
          >
            {axisLabel}
          </text>
        )}

        {dates.map((d, i) =>
          i % step === 0 || i === n - 1 ? (
            <text
              key={`x${i}`}
              x={px(i)}
              y={H - 8}
              textAnchor="middle"
              className="fill-muted-foreground"
              fontSize={8}
            >
              {d}
            </text>
          ) : null,
        )}

        {/* 内容面积(渐变填充,科技感) */}
        {active.map(({ s, si }) => (
          <path
            key={`a${si}`}
            d={areaPath(valuesOf(s))}
            fill={`url(#trend-grad-${metric}-${s.platform})`}
            stroke="none"
            style={{ transition: "d 0.6s ease-out" }}
          />
        ))}

        {/* 内容平滑折线(每平台品牌色) */}
        {active.map(({ s, si }) => (
          <path
            key={`l${si}`}
            d={linePath(valuesOf(s))}
            fill="none"
            stroke={platformColor(s.platform, si)}
            strokeWidth={1}
            strokeLinejoin="round"
            strokeLinecap="round"
            style={{ transition: "d 0.6s ease-out" }}
          />
        ))}

        {/* 数据点标记(非零;悬停当天放大) */}
        {active.map(({ s, si }) =>
          valuesOf(s).map((v, i) =>
            v > 0 ? (
              <circle
                key={`p${si}-${i}`}
                cx={px(i)}
                cy={py(v)}
                r={hover === i ? 3.5 : 2}
                fill={platformColor(s.platform, si)}
                className="stroke-background"
                strokeWidth={1.5}
                style={{ transition: "r 0.15s" }}
              />
            ) : null,
          ),
        )}

        {!hasData && (
          <text
            x={W / 2}
            y={padT + innerH / 2}
            textAnchor="middle"
            className="fill-muted-foreground"
            fontSize={8}
          >
            暂无采集数据
          </text>
        )}

        {/* 悬停竖线 */}
        {hover !== null && (
          <line
            x1={px(hover)}
            y1={padT}
            x2={px(hover)}
            y2={padT + innerH}
            stroke="currentColor"
            strokeOpacity={0.35}
            strokeDasharray="3 3"
          />
        )}
      </svg>

      {/* 悬停浮窗:当天各平台内容 + 评论明细(评论虽不画线,数据仍可见) */}
      {hover !== null && hasData && (
        <div
          className="pointer-events-none absolute top-1 z-10 w-max max-w-[260px] whitespace-nowrap rounded-md border bg-popover px-2.5 py-1.5 text-xs shadow-md"
          style={{
            left: `${(px(hover) / W) * 100}%`,
            // 靠左点左对齐、靠右点右对齐、中间居中,避免浮窗超出图表边缘被截断
            transform: `translateX(${
              px(hover) / W < 0.18
                ? "0%"
                : px(hover) / W > 0.82
                  ? "-100%"
                  : "-50%"
            })`,
          }}
        >
          <div className="mb-1 font-medium text-foreground">{dates[hover]}</div>
          <div className={`grid items-center gap-x-4 gap-y-1 ${valueLabel ? "grid-cols-[1fr_auto]" : "grid-cols-[1fr_auto_auto]"}`}>
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
                {valueLabel ? (
                  <span className="text-muted-foreground">
                    {valueLabel}{" "}
                    <span className="font-mono text-foreground">
                      {(s.contents[hover] ?? 0).toLocaleString()}
                    </span>
                  </span>
                ) : (
                  <>
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
                  </>
                )}
              </Fragment>
            ))}
          </div>
        </div>
      )}
    </div>
  );
});
