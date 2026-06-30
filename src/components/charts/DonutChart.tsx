import { useState } from "react";
import { useCountUp } from "@/components/animated-number";

export function DonutChart({
  data,
  size = 128,
}: {
  data: { label: string; value: number; color: string }[];
  size?: number;
}) {
  const [hover, setHover] = useState<number | null>(null);
  const total = data.reduce((s, d) => s + d.value, 0);
  const animatedTotal = useCountUp(total) ?? total;
  const r = size / 2 - 12;
  const c = 2 * Math.PI * r;
  let acc = 0;

  const hoveredItem = hover !== null ? data[hover] : null;

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
            const isHovered = hover === i;
            const isFaded = hover !== null && !isHovered;
            const seg = (
              <circle
                key={i}
                cx={size / 2}
                cy={size / 2}
                r={r}
                fill="none"
                stroke={d.color}
                strokeWidth={isHovered ? 15 : 12}
                strokeOpacity={isFaded ? 0.4 : 1}
                strokeDasharray={`${(frac * c).toFixed(2)} ${c.toFixed(2)}`}
                strokeDashoffset={(-acc * c).toFixed(2)}
                style={{
                  cursor: "pointer",
                  transition:
                    "stroke-dasharray 0.6s ease-out, stroke-dashoffset 0.6s ease-out, stroke-width 0.15s, stroke-opacity 0.15s",
                }}
                onMouseEnter={() => setHover(i)}
                onMouseLeave={() => setHover(null)}
              />
            );
            acc += frac;
            return seg;
          })}
        </g>
      )}
      {hoveredItem ? (
        <>
          <text
            x={size / 2}
            y={size / 2 - 6}
            textAnchor="middle"
            className="fill-foreground"
            fontSize={13}
            fontWeight={600}
          >
            {hoveredItem.label}
          </text>
          <text
            x={size / 2}
            y={size / 2 + 12}
            textAnchor="middle"
            className="fill-muted-foreground"
            fontSize={11}
          >
            {hoveredItem.value.toLocaleString()}
          </text>
        </>
      ) : (
        <>
          <text
            x={size / 2}
            y={size / 2 - 2}
            textAnchor="middle"
            className="fill-foreground"
            fontSize={22}
            fontWeight={600}
          >
            {animatedTotal.toLocaleString()}
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
        </>
      )}
    </svg>
  );
}
