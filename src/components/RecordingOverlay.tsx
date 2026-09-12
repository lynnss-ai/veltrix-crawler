// 录屏悬浮控制条:无边框置顶小窗,悬浮在屏幕顶部。三种状态:
// ① 准备中(小条):「开始」+ 麦克风开关 + 设置齿轮 + 屏幕选择(多屏才显示)+ 取消;
//    单屏点「开始」直接开录,不经过预览确认;
//    设置齿轮在小条下方展开设置面板(窗口加高):含本程序开关 + 音频设备选择 + 音频测试;
// ② 预览确认(大窗):各屏缩略图平铺、统一长宽,点开时只截取一次(不做实时刷新,性能优先),
//    直接点选要录的屏;多屏打开入口时直接进入此态;设置面板在此态内联展开(窗口够大不缩放);
// ③ 录制中(小条):红点 + 计时 + 麦克风指示 + 暂停/继续 + 停止(屏幕指示不显示:启动时已固定)。
//    暂停 = 后端收尾当前分段并挂起计时,继续 = 起新分段;停止时各段拼接,暂停时段不进成片。
// 麦克风默认开(降噪/动态放大在后端滤镜链完成,无设备自动降级纯视频);
// 屏幕只能选具体某一块(全屏录制已移除),多屏默认主屏;两者都在启动时固定进 ffmpeg 命令,
// 录制中途不可改,录制态只做状态回显。「含本程序」默认开(开始时不最小化主窗口,本程序入镜);
// 关则开始时最小化主窗口,成品不含本程序。本窗口已被后端排除出屏幕捕获(Windows),不会被录进视频。
import { useEffect, useRef, useState } from "react";
import {
  AppWindow,
  Loader2,
  Mic,
  MicOff,
  Monitor,
  Pause,
  Play,
  Settings,
  Square,
  Volume2,
  X,
} from "lucide-react";

import { api } from "@/lib/api";
import type {
  AudioDeviceInfo,
  RecordingStatus,
  ScreenInfo,
  ScreenPreview,
} from "@/lib/api-types";

// 麦克风 / 屏幕选择 / 含本程序 / 音频设备的本地记忆键:下次开悬浮条记住上次选择
const MIC_STORAGE_KEY = "veltrix-rec-mic";
const SCREEN_STORAGE_KEY = "veltrix-rec-screen";
const INCLUDE_APP_STORAGE_KEY = "veltrix-rec-include-app";
const MIC_DEVICE_STORAGE_KEY = "veltrix-rec-mic-device";

// 秒数格式化为 mm:ss
function formatElapsed(totalSeconds: number): string {
  const safe = Math.max(0, Math.floor(totalSeconds));
  const minutes = Math.floor(safe / 60)
    .toString()
    .padStart(2, "0");
  const seconds = (safe % 60).toString().padStart(2, "0");
  return `${minutes}:${seconds}`;
}

// 把一组 0~1 的幅度值画成柱状波形(音频测试用;颜色取主题 primary)
function paintWave(canvas: HTMLCanvasElement, values: number[]) {
  const dpr = window.devicePixelRatio || 1;
  const w = canvas.clientWidth;
  const h = canvas.clientHeight;
  if (w === 0 || h === 0) return;
  canvas.width = Math.round(w * dpr);
  canvas.height = Math.round(h * dpr);
  const ctx = canvas.getContext("2d");
  if (!ctx) return;
  ctx.scale(dpr, dpr);
  ctx.clearRect(0, 0, w, h);
  const primary =
    getComputedStyle(document.documentElement)
      .getPropertyValue("--primary")
      .trim() || "#f43f5e";
  ctx.fillStyle = primary;
  const n = values.length;
  const gap = 2;
  const barW = Math.max(1, w / n - gap);
  for (let i = 0; i < n; i++) {
    const v = Math.min(1, values[i]);
    const bh = Math.max(2, v * (h - 4));
    ctx.fillRect((i * w) / n + gap / 2, (h - bh) / 2, barW, bh);
  }
}

// Float32 时域采样 → n 个 RMS 桶(0~1;语音 RMS 偏小,乘个系数让波形好看)
function bucketWave(samples: ArrayLike<number>, buckets: number): number[] {
  const out = new Array<number>(buckets).fill(0);
  const size = Math.max(1, Math.floor(samples.length / buckets));
  for (let i = 0; i < buckets; i++) {
    let sum = 0;
    const start = i * size;
    for (let j = 0; j < size && start + j < samples.length; j++) {
      const v = samples[start + j];
      sum += v * v;
    }
    out[i] = Math.min(1, Math.sqrt(sum / size) * 2.5);
  }
  return out;
}

// 从本地记忆 + 枚举结果解析初始屏幕下标:记忆失效(显示器被拔/历史上存过「全屏」)则回退主屏
function resolveInitialScreen(list: ScreenInfo[]): number | null {
  const raw = localStorage.getItem(SCREEN_STORAGE_KEY);
  const saved = raw === null || raw === "all" ? null : Number(raw);
  if (saved !== null && list.some((s) => s.index === saved)) return saved;
  return (list.find((s) => s.primary) ?? list[0])?.index ?? null;
}

export function RecordingOverlay() {
  const [recording, setRecording] = useState(false);
  const [elapsed, setElapsed] = useState(0);
  const [busy, setBusy] = useState(false);
  // 暂停态:红点变灰、计时挂起(以后端 elapsedSecs 为基准,本地只在非暂停时走秒)
  const [paused, setPaused] = useState(false);
  // 计时基准:secs = 后端回报的活跃秒数,at = 收到该回报的本机时刻
  const timerBase = useRef({ secs: 0, at: Date.now() });
  // 预览确认态
  const [previewOn, setPreviewOn] = useState(false);
  // 各屏缩略图:index → data URL(空串=该屏编码失败,占位显示);实时刷新时整表替换
  const [tiles, setTiles] = useState<Record<number, string>>({});
  const [previewLoading, setPreviewLoading] = useState(false);
  const [previewError, setPreviewError] = useState<string | null>(null);
  // 麦克风开关:默认开;选择持久化到 localStorage
  const [micOn, setMicOn] = useState(
    () => localStorage.getItem(MIC_STORAGE_KEY) !== "0",
  );
  // 含本程序:默认开(开始时不最小化主窗口,本程序一并入镜);关则成品不含本程序
  const [includeApp, setIncludeApp] = useState(
    () => localStorage.getItem(INCLUDE_APP_STORAGE_KEY) !== "0",
  );
  // 设置面板:展开时窗口加高(预览态内联);内含 含本程序 / 音频设备 / 音频测试
  const [settingsOpen, setSettingsOpen] = useState(false);
  // 音频设备选择器:空串 = 自动(后端挑评分最高的默认设备);选择持久化
  const [micDevice, setMicDevice] = useState(
    () => localStorage.getItem(MIC_DEVICE_STORAGE_KEY) ?? "",
  );
  const [audioDevices, setAudioDevices] = useState<AudioDeviceInfo[]>([]);
  const [audioLoading, setAudioLoading] = useState(false);
  const [testing, setTesting] = useState(false);
  const [testMsg, setTestMsg] = useState<string | null>(null);
  // 波形画布与上次成品波形(面板重开时回填;canvas 随面板卸载像素会丢)
  const waveCanvasRef = useRef<HTMLCanvasElement | null>(null);
  const lastWaveRef = useRef<number[] | null>(null);
  // 显示器列表与当前选择(只能选具体某块屏;null 仅表示尚未枚举完成)
  const [screens, setScreens] = useState<ScreenInfo[]>([]);
  const [screenIdx, setScreenIdx] = useState<number | null>(null);

  function toggleMic() {
    setMicOn((v) => {
      const next = !v;
      localStorage.setItem(MIC_STORAGE_KEY, next ? "1" : "0");
      return next;
    });
  }

  function toggleIncludeApp() {
    setIncludeApp((v) => {
      const next = !v;
      localStorage.setItem(INCLUDE_APP_STORAGE_KEY, next ? "1" : "0");
      return next;
    });
  }

  // 展开 / 收起设置面板:准备态窗口要加高才能容纳(悬浮窗无窗口控制权限,缩放走后端);
  // 预览态窗口本身够大,内联展开即可。展开时顺带加载音频设备清单
  async function toggleSettings() {
    const next = !settingsOpen;
    setSettingsOpen(next);
    setTestMsg(null);
    if (!previewOn) {
      await api
        .setRecordingOverlayPanel(next)
        .catch((e) => console.debug("调整悬浮窗尺寸失败:", e));
    }
    if (next) void loadAudioDevices();
  }

  async function loadAudioDevices() {
    setAudioLoading(true);
    try {
      setAudioDevices(await api.listAudioDevices());
    } catch (e) {
      console.debug("枚举音频设备失败:", e);
    } finally {
      setAudioLoading(false);
    }
  }

  // 选择音频设备:记忆到 localStorage,开始录制时传给后端
  function selectMicDevice(name: string) {
    setMicDevice(name);
    localStorage.setItem(MIC_DEVICE_STORAGE_KEY, name);
  }

  // 设置面板重开时回填上次波形
  useEffect(() => {
    if (settingsOpen && lastWaveRef.current && waveCanvasRef.current) {
      paintWave(waveCanvasRef.current, lastWaveRef.current);
    }
  }, [settingsOpen]);

  // 实时波形:测试期间用 getUserMedia + AnalyserNode 画输入波形;
  // 悬浮窗麦克风权限被拒 / 设备匹配不上时静默返回 null(结束后还有成品波形兜底)
  async function startLiveWave(): Promise<(() => void) | null> {
    try {
      let deviceId: string | undefined;
      if (micDevice) {
        // 浏览器设备名与 dshow 设备名口径不同,尽量模糊匹配;匹配不上就用系统默认输入
        const devs = await navigator.mediaDevices.enumerateDevices();
        const hit = devs.find(
          (d) =>
            d.kind === "audioinput" &&
            (d.label === micDevice ||
              d.label.includes(micDevice) ||
              micDevice.includes(d.label)),
        );
        deviceId = hit?.deviceId || undefined;
      }
      const stream = await navigator.mediaDevices.getUserMedia({
        audio: deviceId ? { deviceId: { ideal: deviceId } } : true,
      });
      const actx = new AudioContext();
      const src = actx.createMediaStreamSource(stream);
      const analyser = actx.createAnalyser();
      analyser.fftSize = 2048;
      src.connect(analyser);
      const bytes = new Uint8Array(analyser.fftSize);
      const floats = new Float32Array(analyser.fftSize);
      let raf = 0;
      const draw = () => {
        const canvas = waveCanvasRef.current;
        if (canvas) {
          analyser.getByteTimeDomainData(bytes);
          for (let i = 0; i < bytes.length; i++) floats[i] = (bytes[i] - 128) / 128;
          paintWave(canvas, bucketWave(floats, 64));
        }
        raf = requestAnimationFrame(draw);
      };
      raf = requestAnimationFrame(draw);
      return () => {
        cancelAnimationFrame(raf);
        stream.getTracks().forEach((t) => t.stop());
        void actx.close();
      };
    } catch {
      return null;
    }
  }

  // 把录好的 3 秒样本解成静态波形(含后端降噪/放大滤镜链的真实结果)
  async function drawRecordedWave(dataUrl: string) {
    try {
      const buf = await (await fetch(dataUrl)).arrayBuffer();
      const actx = new AudioContext();
      const audio = await actx.decodeAudioData(buf);
      void actx.close();
      const values = bucketWave(audio.getChannelData(0), 64);
      lastWaveRef.current = values;
      const canvas = waveCanvasRef.current;
      if (canvas) paintWave(canvas, values);
    } catch (e) {
      console.debug("绘制测试波形失败:", e);
    }
  }

  // 音频测试:后端用选中设备录 3 秒(与正式录制同一滤镜链),返回 data URL 直接回放;
  // 期间画布画实时输入波形,结束后换成成品静态波形
  async function testAudio() {
    if (testing) return;
    setTesting(true);
    setTestMsg(null);
    let stopLive: (() => void) | null = null;
    try {
      stopLive = await startLiveWave();
      const dataUrl = await api.testRecordingAudio(micDevice || null);
      stopLive?.();
      stopLive = null;
      await drawRecordedWave(dataUrl);
      await new Audio(dataUrl).play().catch(() => {});
      setTestMsg("已回放 3 秒试听");
    } catch (e) {
      stopLive?.();
      setTestMsg(String(e));
    } finally {
      setTesting(false);
    }
  }

  // 以后端状态为准同步计时:开始 / 暂停切换 / 录制中被重开 都走这里
  function syncTimer(s: RecordingStatus) {
    timerBase.current = { secs: s.elapsedSecs, at: Date.now() };
    setPaused(s.paused);
    setElapsed(s.elapsedSecs);
  }

  // 抓全部屏幕的缩略图:只在进预览时截一次(不做定时刷新,截图是重操作,省了持续 CPU 开销)
  async function loadTiles() {
    setPreviewLoading(true);
    setPreviewError(null);
    try {
      const list: ScreenPreview[] = await api.recordingPreviewAll();
      const map: Record<number, string> = {};
      for (const item of list) map[item.index] = item.dataUrl;
      setTiles(map);
    } catch (e) {
      setPreviewError(String(e));
    } finally {
      setPreviewLoading(false);
    }
  }

  // 选择某块屏:记忆 + 预览态缩略图高亮(平铺直选,无需重抓)
  function selectScreen(idx: number) {
    setScreenIdx(idx);
    localStorage.setItem(SCREEN_STORAGE_KEY, String(idx));
  }

  // 挂载时:同步后端录制状态(录制中被重开 → 直接进录制态)+ 枚举显示器;
  // 多屏且未在录制 → 直接进入预览确认态平铺选屏;单屏留准备态,点「开始」直接开录
  useEffect(() => {
    let disposed = false;
    (async () => {
      const status = await api
        .getRecordingStatus()
        .catch((e) => (console.debug("获取录制状态失败:", e), null));
      if (disposed) return;
      if (status?.recording) {
        setRecording(true);
        setMicOn(status.withMic);
        setScreenIdx(status.screenIndex);
        syncTimer(status);
        // 录制态不需要显示器清单(录制中不再做屏幕指示),不再自动进预览
        return;
      }
      const list = await api
        .listScreens()
        .catch((e) => (console.debug("枚举显示器失败:", e), [] as ScreenInfo[]));
      if (disposed) return;
      setScreens(list);
      setScreenIdx(resolveInitialScreen(list));
      if (list.length > 1) void enterPreview();
    })();
    return () => {
      disposed = true;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // 录制中每秒刷新计时(以后端回报的活跃秒数为基准;暂停时挂起不走秒)
  useEffect(() => {
    if (!recording) return;
    if (paused) {
      setElapsed(timerBase.current.secs);
      return;
    }
    const tick = () =>
      setElapsed(
        timerBase.current.secs + (Date.now() - timerBase.current.at) / 1000,
      );
    tick();
    const timer = window.setInterval(tick, 1000);
    return () => clearInterval(timer);
  }, [recording, paused]);

  // 进预览确认态:窗口放大 + 平铺各屏缩略图(不直接开录)
  async function enterPreview() {
    if (busy) return;
    setBusy(true);
    try {
      await api.setRecordingOverlayPreview(true);
      setPreviewOn(true);
      await loadTiles();
    } finally {
      setBusy(false);
    }
  }

  // 开始录制(预览态「开始录制」或单屏准备态「开始」直录):后端会把窗口缩回小条;
  // 未开「含本程序」时还会最小化主窗口(成品不含本程序)
  async function start() {
    // screenIdx 为 null(枚举失败)也放行:后端按 null 录整虚拟桌面,不至于点不动
    if (busy) return;
    setBusy(true);
    try {
      // 麦克风不可用 / 屏幕下标失效时后端自动降级(纯视频 / 重选屏幕),前端无需区分
      const s = await api.startScreenRecording(
        micOn,
        screenIdx,
        includeApp,
        micDevice,
      );
      setSettingsOpen(false); // 后端会把窗口缩回小条,本地面板态同步收起
      setPreviewOn(false);
      setTiles({});
      setRecording(true);
      syncTimer(s);
    } catch {
      // 失败留在当前态可重试(悬浮窗无 Toaster,无法弹提示)
    } finally {
      setBusy(false);
    }
  }

  // 暂停 / 继续:后端收尾当前分段或起新分段,返回的状态直接同步计时
  async function togglePause() {
    if (busy) return;
    setBusy(true);
    try {
      const s = await api.toggleRecordingPause();
      syncTimer(s);
    } catch {
      // 失败保持原状态可重试
    } finally {
      setBusy(false);
    }
  }

  // 准备态「开始」:多屏先进预览确认态选屏;单屏无需预览选屏,直接开录
  function handlePrimary() {
    if (screens.length > 1) void enterPreview();
    else void start();
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

  // 麦克风开关按钮(准备/预览两态共用)
  const micButton = (
    <button
      type="button"
      onClick={toggleMic}
      title={micOn ? "麦克风已开(点击关闭)" : "麦克风已关(点击开启)"}
      aria-pressed={micOn}
      className={`inline-flex size-7 shrink-0 items-center justify-center rounded-full transition-colors ${
        micOn
          ? "bg-primary/10 text-primary hover:bg-primary/15"
          : "text-muted-foreground hover:bg-accent"
      }`}
    >
      {micOn ? <Mic className="size-3.5" /> : <MicOff className="size-3.5" />}
    </button>
  );

  // 设置齿轮(准备/预览两态共用):展开 / 收起设置面板
  const settingsButton = (
    <button
      type="button"
      onClick={() => void toggleSettings()}
      title="录屏设置"
      aria-pressed={settingsOpen}
      className={`inline-flex size-7 shrink-0 items-center justify-center rounded-full transition-colors ${
        settingsOpen
          ? "bg-primary/10 text-primary hover:bg-primary/15"
          : "text-muted-foreground hover:bg-accent"
      }`}
    >
      <Settings className="size-3.5" />
    </button>
  );

  // 推荐设备名(「自动」选项的展示用)
  const recommendedMic = audioDevices.find((d) => d.recommended);

  // 设置面板:含本程序开关 + 音频设备选择 + 音频测试
  // 准备态在小条下方弹出(窗口已被后端加高);预览态内联在头部下方
  const settingsPanel = (
    <div className="flex w-full flex-col gap-2.5 rounded-2xl border border-border bg-background/95 p-3 shadow-2xl backdrop-blur">
      <button
        type="button"
        onClick={toggleIncludeApp}
        aria-pressed={includeApp}
        title={
          includeApp
            ? "开始时不最小化主窗口,本程序的操作一并入镜"
            : "开始时最小化主窗口,成品不含本程序"
        }
        className="flex items-center justify-between rounded-lg px-1 py-0.5 text-xs text-foreground transition-colors hover:bg-accent"
      >
        <span className="flex items-center gap-1.5">
          <AppWindow className="size-3.5 text-muted-foreground" />
          含本程序
        </span>
        <span
          className={`relative h-4 w-7 shrink-0 rounded-full transition-colors ${
            includeApp ? "bg-primary" : "bg-muted"
          }`}
        >
          <span
            className={`absolute top-0.5 size-3 rounded-full bg-white shadow transition-all ${
              includeApp ? "left-3.5" : "left-0.5"
            }`}
          />
        </span>
      </button>
      <div className="flex items-center gap-1.5 px-1 text-xs text-foreground">
        <Volume2 className="size-3.5 text-muted-foreground" />
        音频设备
      </div>
      <select
        value={micDevice}
        onChange={(e) => selectMicDevice(e.target.value)}
        disabled={audioLoading || !micOn}
        className="h-7 w-full rounded-md border border-border bg-background px-1.5 text-xs text-foreground outline-none disabled:opacity-50"
      >
        <option value="">
          自动{recommendedMic ? `(推荐:${recommendedMic.name})` : ""}
        </option>
        {audioDevices.map((d) => (
          <option key={d.name} value={d.name}>
            {d.name}
            {d.recommended ? "(推荐)" : ""}
          </option>
        ))}
      </select>
      {/* 波形:测试期间实时输入波形,结束后为成品(含降噪/放大)静态波形 */}
      <canvas
        ref={waveCanvasRef}
        className="h-12 w-full rounded-md bg-muted/40"
      />
      <div className="flex items-center gap-2">
        <button
          type="button"
          onClick={() => void testAudio()}
          disabled={testing || !micOn}
          title="用选中设备录 3 秒并回放(与正式录制同一降噪 / 放大链路)"
          className="inline-flex h-7 shrink-0 items-center gap-1.5 rounded-full border border-border px-3 text-xs text-muted-foreground transition-colors hover:bg-accent hover:text-foreground disabled:opacity-50"
        >
          {testing ? (
            <Loader2 className="size-3 animate-spin" />
          ) : (
            <Volume2 className="size-3" />
          )}
          {testing ? "录 3 秒试听…" : "音频测试"}
        </button>
        {testMsg && (
          <span
            className="min-w-0 flex-1 truncate text-[11px] text-muted-foreground"
            title={testMsg}
          >
            {testMsg}
          </span>
        )}
      </div>
      {!micOn && (
        <p className="text-[11px] leading-relaxed text-muted-foreground">
          麦克风已关:测试与录制均为纯视频
        </p>
      )}
    </div>
  );

  // 录制中:红点 + 计时 + 麦克风状态指示 + 暂停/继续 + 停止(居中)。外层透明只负责居中,内层自适应宽度 + 圆弧角
  // 麦克风指示仅展示状态:ffmpeg 输入在启动时已固定,录制中途无法更改;
  // 屏幕指示不显示——录制目标启动时已固定,展示出来也没有操作意义
  if (recording) {
    return (
      <div className="flex h-screen w-screen items-center justify-center">
        <div className="inline-flex items-center gap-2 rounded-2xl border border-border bg-background/95 px-3 py-1.5 shadow-2xl backdrop-blur">
          {paused ? (
            <span
              title="已暂停"
              className="inline-flex size-2.5 shrink-0 rounded-full bg-amber-500"
            />
          ) : (
            <span className="relative flex size-2.5 shrink-0">
              <span className="absolute inline-flex size-full animate-ping rounded-full bg-red-500 opacity-75" />
              <span className="relative inline-flex size-2.5 rounded-full bg-red-500" />
            </span>
          )}
          <span
            className={`font-mono text-sm tabular-nums ${
              paused ? "text-muted-foreground" : "text-foreground"
            }`}
          >
            {formatElapsed(elapsed)}
          </span>
          <span
            title={micOn ? "麦克风录制中" : "纯视频录制(麦克风关)"}
            className={`inline-flex size-5 shrink-0 items-center justify-center ${
              micOn ? "text-primary" : "text-muted-foreground"
            }`}
          >
            {micOn ? (
              <Mic className="size-3.5" />
            ) : (
              <MicOff className="size-3.5" />
            )}
          </span>
          <button
            type="button"
            onClick={() => void togglePause()}
            disabled={busy}
            title={paused ? "继续录制" : "暂停录制"}
            className="inline-flex size-6 shrink-0 items-center justify-center rounded-full bg-amber-500 text-white transition-colors hover:bg-amber-600 disabled:opacity-60"
          >
            {busy ? (
              <Loader2 className="size-3 animate-spin" />
            ) : paused ? (
              <Play className="size-3 fill-current" />
            ) : (
              <Pause className="size-3 fill-current" />
            )}
          </button>
          <button
            type="button"
            onClick={() => void stop()}
            disabled={busy}
            title={busy ? "正在处理…" : "停止录制"}
            className="inline-flex size-6 shrink-0 items-center justify-center rounded-full bg-red-500 text-white transition-colors hover:bg-red-600 disabled:opacity-60"
          >
            <Square className="size-3 fill-current" />
          </button>
        </div>
      </div>
    );
  }

  // 预览确认态:各屏缩略图平铺直选(实时刷新)+ 麦克风确认 + 底部 开始录制/返回
  if (previewOn) {
    return (
      <div className="flex h-screen w-screen items-center justify-center">
        <div className="flex w-[560px] flex-col gap-3 rounded-2xl border border-border bg-background/95 p-4 shadow-2xl backdrop-blur">
          <div className="flex items-center justify-between">
            <span className="text-sm font-medium text-foreground">
              选择要录制的屏幕{micOn ? "(含音频)" : "(纯视频)"}
            </span>
            <span className="flex items-center gap-1.5">
              {settingsButton}
              {micButton}
            </span>
          </div>
          {/* 设置面板:预览态窗口够大,内联展开(无需后端缩放) */}
          {settingsOpen && settingsPanel}
          {/* 屏幕平铺:实时缩略图,点击即选;选中高亮描边 */}
          <div className="grid max-h-[340px] grid-cols-2 gap-2 overflow-y-auto">
            {screens.map((s) => {
              const url = tiles[s.index];
              const selected = screenIdx === s.index;
              return (
                <button
                  key={s.index}
                  type="button"
                  onClick={() => selectScreen(s.index)}
                  className={`group flex flex-col overflow-hidden rounded-lg border text-left transition-colors ${
                    selected
                      ? "border-primary ring-2 ring-primary/40"
                      : "border-border hover:border-primary/40"
                  }`}
                >
                  <span className="flex aspect-video items-center justify-center bg-black/80">
                    {previewLoading && url === undefined ? (
                      <Loader2 className="size-5 animate-spin text-muted-foreground" />
                    ) : url ? (
                      <img
                        src={url}
                        alt={`屏幕 ${s.index + 1} 预览`}
                        className="h-full w-full object-contain"
                      />
                    ) : (
                      <span className="text-[11px] text-muted-foreground">
                        暂无预览
                      </span>
                    )}
                  </span>
                  <span
                    className={`flex items-center gap-1 px-2 py-1 text-[11px] ${
                      selected ? "text-primary" : "text-muted-foreground"
                    }`}
                  >
                    <Monitor className="size-3" />
                    屏{s.index + 1}
                    {s.primary ? "(主)" : ""} · {s.width}×{s.height}
                  </span>
                </button>
              );
            })}
          </div>
          {previewError && (
            <p className="text-[11px] text-destructive">{previewError}</p>
          )}
          <p className="text-[11px] leading-relaxed text-muted-foreground">
            {includeApp
              ? "「含本程序」已开:开始后不最小化主窗口,本程序的操作一并入镜。"
              : "点「开始录制」后主窗口会自动最小化,成品视频不含本程序。"}
          </p>
          {/* 底部按钮:退出在左(直接结束本次录屏操作,关悬浮窗 + 还原主窗口)、开始录制在右 */}
          <div className="flex shrink-0 items-center justify-between border-t border-border/60 pt-3">
            <button
              type="button"
              onClick={cancel}
              className="inline-flex h-8 items-center rounded-full border border-border px-4 text-xs text-muted-foreground transition-colors hover:bg-accent hover:text-foreground"
            >
              退出
            </button>
            <button
              type="button"
              onClick={() => void start()}
              disabled={busy || previewLoading || screenIdx === null}
              className="inline-flex h-8 shrink-0 items-center gap-1.5 rounded-full bg-red-500 px-4 text-xs font-medium text-white transition-colors hover:bg-red-600 disabled:opacity-60"
            >
              <span className="size-2 rounded-full bg-white" />
              开始录制
            </button>
          </div>
        </div>
      </div>
    );
  }

  // 准备中:开始 + 麦克风开关 + 设置齿轮 + 当前屏幕(多屏才显示)+ 取消。
  // 小条钉在窗口顶部,设置面板在其下方弹出(窗口已被后端加高)
  return (
    <div className="flex h-screen w-screen flex-col items-center pt-1.5">
      <div className="inline-flex items-center gap-1.5 rounded-2xl border border-border bg-background/95 px-2 py-1.5 shadow-2xl backdrop-blur">
        <button
          type="button"
          onClick={handlePrimary}
          disabled={busy}
          title={screens.length > 1 ? "开始录制(先预览选屏)" : "开始录制"}
          className="inline-flex shrink-0 items-center gap-1.5 rounded-full bg-red-500 py-1 pl-2.5 pr-3 text-xs font-medium text-white transition-colors hover:bg-red-600 disabled:opacity-60"
        >
          {busy ? (
            <Loader2 className="size-3 animate-spin" />
          ) : (
            <span className="size-2 rounded-full bg-white" />
          )}
          开始
        </button>
        {micButton}
        {settingsButton}
        {/* 当前屏幕:点击进预览态选屏;仅多屏时显示 */}
        {screens.length > 1 && screenIdx !== null && (
          <button
            type="button"
            onClick={() => void enterPreview()}
            title={`录制目标:屏幕 ${screenIdx + 1}(点击换屏)`}
            className="inline-flex h-7 shrink-0 items-center gap-1 rounded-full bg-primary/10 px-2 text-[11px] text-primary transition-colors hover:bg-primary/15"
          >
            <Monitor className="size-3.5" />
            屏{screenIdx + 1}
          </button>
        )}
        <button
          type="button"
          onClick={cancel}
          title="取消"
          className="inline-flex size-7 shrink-0 items-center justify-center rounded-full text-muted-foreground transition-colors hover:bg-accent"
        >
          <X className="size-3.5" />
        </button>
      </div>
      {/* 设置面板:小条下方弹出(窗口已被后端加高) */}
      {settingsOpen && (
        <div className="mt-1.5 w-full px-2">{settingsPanel}</div>
      )}
    </div>
  );
}
