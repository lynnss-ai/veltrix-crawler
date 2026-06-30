import { memo, useState, type MouseEvent as ReactMouseEvent } from "react";
import type { ModelTrendSeries } from "@/lib/api";

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

export const ModelTrendChart = memo(function ModelTrendChart({
  dates,
  series,
  unit,
}: {
  dates: string[];
  series: ModelTrendSeries[];
  unit?: string;
}) {
  const [hover, setHover] = useState<number | null>(null);

  if (dates.length === 0 || series.length === 0) {
    return (
      <div className="flex h-48 items-center justify-center text-sm text-muted-foreground">
        该区间暂无用量数据
      </div>
    );
  }

  const W = 760;
  const H = 200;
  const padL = 52;
  const padR = 16;
  const padT = 12;
  const padB = 28;
  const innerW = W - padL - padR;
  const innerH = H - padT - padB;
  const n = dates.length;

  const active = series
    .map((s, si) => ({ s, si }))
    .filter(({ s }) => s.values.some((v) => v > 0));
  const hasData = active.length > 0;

  const maxV = Math.max(1, ...active.flatMap(({ s }) => s.values));
  const px = (i: number) =>
    padL + (n === 1 ? innerW / 2 : (i * innerW) / (n - 1));
  const py = (v: number) => padT + innerH * (1 - v / maxV);

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
  const linePath = (values: number[]) =>
    smooth(values.map((v, i) => [px(i), py(v)] as [number, number]));
  const areaPath = (values: number[]) => {
    const yb = py(0).toFixed(1);
    return `${linePath(values)} L${px(n - 1).toFixed(1)},${yb} L${px(0).toFixed(1)},${yb} Z`;
  };

  const fractions = [0, 0.25, 0.5, 0.75, 1];
  const ticks = fractions.map((f) => Math.round(maxV * f));
  const tickY = (f: number) => padT + innerH * (1 - f);
  const step = Math.max(1, Math.ceil(n / 10));

  const onMove = (e: ReactMouseEvent<SVGSVGElement>) => {
    const rect = e.currentTarget.getBoundingClientRect();
    if (rect.width === 0) return;
    const vbX = ((e.clientX - rect.left) / rect.width) * W;
    const idx = Math.round((vbX - padL) / (innerW / Math.max(1, n - 1)));
    setHover(Math.max(0, Math.min(n - 1, idx)));
  };

  const colorOf = (si: number) => PALETTE[si % PALETTE.length];

  return (
    <div>
      <div className="relative">
        <svg
          viewBox={`0 0 ${W} ${H}`}
          className="h-auto w-full text-border"
          preserveAspectRatio="xMidYMid meet"
          onMouseMove={onMove}
          onMouseLeave={() => setHover(null)}
        >
          <defs>
            {active.map(({ si }) => (
              <linearGradient
                key={`grad-${si}`}
                id={`model-grad-${si}`}
                x1="0"
                y1="0"
                x2="0"
                y2="1"
              >
                <stop offset="0%" stopColor={colorOf(si)} stopOpacity={0.2} />
                <stop offset="100%" stopColor={colorOf(si)} stopOpacity={0} />
              </linearGradient>
            ))}
          </defs>

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
                  strokeOpacity={0.08}
                />
                <text
                  x={padL - 8}
                  y={yy + 3}
                  textAnchor="end"
                  className="fill-muted-foreground"
                  fontSize={9}
                >
                  {ticks[i].toLocaleString()}
                </text>
              </g>
            );
          })}

          {dates.map((d, i) =>
            i % step === 0 || i === n - 1 ? (
              <text
                key={`x${i}`}
                x={px(i)}
                y={H - 6}
                textAnchor="middle"
                className="fill-muted-foreground"
                fontSize={9}
              >
                {d}
              </text>
            ) : null,
          )}

          {active.map(({ s, si }) => (
            <path
              key={`a${si}`}
              d={areaPath(s.values)}
              fill={`url(#model-grad-${si})`}
              stroke="none"
            />
          ))}

          {active.map(({ s, si }) => (
            <path
              key={`l${si}`}
              d={linePath(s.values)}
              fill="none"
              stroke={colorOf(si)}
              strokeWidth={2}
              strokeLinejoin="round"
              strokeLinecap="round"
            />
          ))}

          {active.map(({ s, si }) =>
            s.values.map((v, i) =>
              v > 0 ? (
                <circle
                  key={`p${si}-${i}`}
                  cx={px(i)}
                  cy={py(v)}
                  r={hover === i ? 4 : 2.5}
                  fill={colorOf(si)}
                  className="stroke-background"
                  strokeWidth={hover === i ? 2 : 1.5}
                  style={{ transition: "r 0.15s, stroke-width 0.15s" }}
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
              fontSize={10}
            >
              暂无用量数据
            </text>
          )}

          {hover !== null && (
            <line
              x1={px(hover)}
              y1={padT}
              x2={px(hover)}
              y2={padT + innerH}
              stroke="currentColor"
              strokeOpacity={0.3}
              strokeDasharray="3 3"
            />
          )}
        </svg>

        {hover !== null && hasData && (
          <div
            className="pointer-events-none absolute top-0 z-10 w-max max-w-[300px] whitespace-nowrap rounded-lg border bg-popover/95 px-3 py-2 text-xs shadow-lg backdrop-blur-sm"
            style={{
              left: `${(px(hover) / W) * 100}%`,
              transform: `translateX(${
                px(hover) / W < 0.2
                  ? "0%"
                  : px(hover) / W > 0.8
                    ? "-100%"
                    : "-50%"
              })`,
            }}
          >
            <div className="mb-1.5 font-medium text-foreground">
              {dates[hover]}
            </div>
            <div className="space-y-1">
              {series.map((s, si) => (
                <div key={`t${si}`} className="flex items-center gap-2">
                  <span
                    className="size-2 shrink-0 rounded-full"
                    style={{ background: colorOf(si) }}
                  />
                  <span className="min-w-0 flex-1 truncate text-muted-foreground">
                    {s.model}
                  </span>
                  <span className="font-mono text-foreground">
                    {(s.values[hover] ?? 0).toLocaleString()}
                    {unit && (
                      <span className="ml-0.5 text-muted-foreground">
                        {unit}
                      </span>
                    )}
                  </span>
                </div>
              ))}
            </div>
          </div>
        )}
      </div>

      {/* 图例 + 汇总 */}
      <div className="mt-3 flex flex-wrap gap-x-5 gap-y-1.5 text-xs">
        {series.map((s, si) => {
          const total = s.values.reduce((a, b) => a + b, 0);
          return (
            <div key={s.model} className="flex items-center gap-1.5">
              <span
                className="size-2.5 rounded-full"
                style={{ background: colorOf(si) }}
              />
              <span className="text-muted-foreground">{s.model}</span>
              <span className="font-medium text-foreground">
                {total.toLocaleString()}
              </span>
            </div>
          );
        })}
      </div>
    </div>
  );
});
