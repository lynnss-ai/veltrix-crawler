// 录屏悬浮控制条:小巧无边框置顶小窗,悬浮在屏幕顶部。两种状态:
// ① 准备中:居中显示「开始」按钮 + 取消;② 录制中:居中显示红点 + 计时 + 停止。
// 仅录视频不录音频。录制由用户在此手动开始/停止;本窗口已被后端排除出屏幕捕获(Windows),不会被录进视频。
import { useEffect, useState } from "react";
import { Square, X } from "lucide-react";

import { api } from "@/lib/api";

// 秒数格式化为 mm:ss
function formatElapsed(totalSeconds: number): string {
  const safe = Math.max(0, Math.floor(totalSeconds));
  const minutes = Math.floor(safe / 60)
    .toString()
    .padStart(2, "0");
  const seconds = (safe % 60).toString().padStart(2, "0");
  return `${minutes}:${seconds}`;
}

export function RecordingOverlay() {
  const [recording, setRecording] = useState(false);
  const [startedAtMs, setStartedAtMs] = useState<number | null>(null);
  const [elapsed, setElapsed] = useState(0);
  const [busy, setBusy] = useState(false);

  // 挂载时同步后端状态(悬浮窗若在录制中被重开,直接进录制态)
  useEffect(() => {
    api
      .getRecordingStatus()
      .then((s) => {
        if (s.recording) {
          setRecording(true);
          setStartedAtMs(s.startedAt ? s.startedAt * 1000 : Date.now());
        }
      })
      .catch((e) => console.debug("获取录制状态失败:", e));
  }, []);

  // 录制中每秒刷新计时(以后端开始时间为基准)
  useEffect(() => {
    if (!recording || startedAtMs == null) return;
    const tick = () => setElapsed((Date.now() - startedAtMs) / 1000);
    tick();
    const timer = window.setInterval(tick, 1000);
    return () => clearInterval(timer);
  }, [recording, startedAtMs]);

  async function start() {
    if (busy) return;
    setBusy(true);
    try {
      const s = await api.startScreenRecording();
      setRecording(true);
      setStartedAtMs(s.startedAt ? s.startedAt * 1000 : Date.now());
    } catch {
      // 失败保持「准备中」可重试(悬浮窗无 Toaster,无法弹提示)
    } finally {
      setBusy(false);
    }
  }

  async function stop() {
    if (busy) return;
    setBusy(true);
    try {
      // 后端会结束 ffmpeg 并关闭本悬浮窗 + 还原主窗口
      await api.stopScreenRecording();
    } catch {
      setBusy(false);
    }
  }

  function cancel() {
    // 未开始时取消:后端关悬浮窗 + 还原主窗口,不产出文件
    api.cancelRecordingOverlay().catch((e) => console.debug("取消录制失败:", e));
  }

  // 录制中:红点 + 计时 + 停止(居中)。外层透明只负责居中,内层自适应宽度 + 圆弧角
  if (recording) {
    return (
      <div className="flex h-screen w-screen items-center justify-center">
        <div className="inline-flex items-center gap-2 rounded-2xl border border-border bg-background/95 px-3 py-1.5 shadow-2xl backdrop-blur">
          <span className="relative flex size-2.5 shrink-0">
            <span className="absolute inline-flex size-full animate-ping rounded-full bg-red-500 opacity-75" />
            <span className="relative inline-flex size-2.5 rounded-full bg-red-500" />
          </span>
          <span className="font-mono text-sm tabular-nums text-foreground">
            {formatElapsed(elapsed)}
          </span>
          <button
            type="button"
            onClick={() => void stop()}
            disabled={busy}
            title="停止录制"
            className="inline-flex size-6 shrink-0 items-center justify-center rounded-full bg-red-500 text-white transition-colors hover:bg-red-600 disabled:opacity-60"
          >
            <Square className="size-3 fill-current" />
          </button>
        </div>
      </div>
    );
  }

  // 准备中:开始 + 现场录音开关 + 取消(居中)。外层透明只负责居中,内层自适应宽度 + 圆弧角
  return (
    <div className="flex h-screen w-screen items-center justify-center">
      <div className="inline-flex items-center gap-1.5 rounded-2xl border border-border bg-background/95 px-2 py-1.5 shadow-2xl backdrop-blur">
        <button
          type="button"
          onClick={() => void start()}
          disabled={busy}
          title="开始录制"
          className="inline-flex shrink-0 items-center gap-1.5 rounded-full bg-red-500 py-1 pl-2.5 pr-3 text-xs font-medium text-white transition-colors hover:bg-red-600 disabled:opacity-60"
        >
          <span className="size-2 rounded-full bg-white" />
          开始
        </button>
        <button
          type="button"
          onClick={cancel}
          title="取消"
          className="inline-flex size-7 shrink-0 items-center justify-center rounded-full text-muted-foreground transition-colors hover:bg-accent"
        >
          <X className="size-3.5" />
        </button>
      </div>
    </div>
  );
}
