import { useEffect, useRef, useState } from "react";

// 数字补间动画时长;数据刷新时从旧值平滑增长/减少到新值,而非瞬变
const DURATION_MS = 600;

// 用 requestAnimationFrame 把数值从当前显示值补间到目标值(easeOutCubic)。
// 返回当前帧应显示的整数;value 为 undefined(数据未就绪)时返回 undefined。
export function useCountUp(value: number | undefined): number | undefined {
  const [display, setDisplay] = useState<number | undefined>(value);
  // 实时跟踪当前显示值:动画被新值打断时,从「当前帧值」续接,避免跳变
  const displayRef = useRef<number>(value ?? 0);

  useEffect(() => {
    if (value === undefined) {
      setDisplay(undefined);
      return;
    }
    const from = displayRef.current;
    const to = value;
    if (from === to) {
      setDisplay(to);
      return;
    }
    let raf = 0;
    let start = 0;
    const tick = (ts: number) => {
      if (!start) start = ts;
      const progress = Math.min((ts - start) / DURATION_MS, 1);
      const eased = 1 - Math.pow(1 - progress, 3); // easeOutCubic:先快后慢
      const current = Math.round(from + (to - from) * eased);
      displayRef.current = current;
      setDisplay(current);
      if (progress < 1) raf = requestAnimationFrame(tick);
    };
    raf = requestAnimationFrame(tick);
    return () => cancelAnimationFrame(raf);
  }, [value]);

  return display;
}

// 文本位置直接用的动画数字(已 toLocaleString);value 未就绪时显示 fallback。
export function AnimatedNumber({
  value,
  fallback = "—",
}: {
  value: number | undefined;
  fallback?: string;
}) {
  const display = useCountUp(value);
  return <>{display === undefined ? fallback : display.toLocaleString()}</>;
}
