// 秒 → 时:分:秒:厘秒(秒的下一级 = 1/100 秒;<video> 不暴露帧率,厘秒是可靠的最细精度)
export function fmt(t: number): string {
  const safe = Math.max(0, t);
  const h = Math.floor(safe / 3600)
    .toString()
    .padStart(2, "0");
  const m = Math.floor((safe % 3600) / 60)
    .toString()
    .padStart(2, "0");
  const s = Math.floor(safe % 60)
    .toString()
    .padStart(2, "0");
  const cs = Math.floor((safe % 1) * 100)
    .toString()
    .padStart(2, "0");
  return `${h}:${m}:${s}:${cs}`;
}

// 刻度尺用短格式(分:秒):时:分:秒:厘秒在密集的刻度标签上太宽
export function fmtTick(t: number): string {
  const safe = Math.max(0, t);
  const m = Math.floor(safe / 60)
    .toString()
    .padStart(2, "0");
  const s = Math.floor(safe % 60)
    .toString()
    .padStart(2, "0");
  return `${m}:${s}`;
}
