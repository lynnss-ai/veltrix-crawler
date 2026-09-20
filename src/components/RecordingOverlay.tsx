// 录屏悬浮控制条:无边框置顶小窗,悬浮在屏幕顶部。三种状态:
// ① 准备中(小条):「开始」+ 麦克风开关 + 设置齿轮 + 屏幕选择(多屏才显示)+ 取消;
//    单屏点「开始」直接开录;齿轮 / 屏幕选择进配置面板;
// ② 配置面板(大窗):左右结构——左列设备配置(麦克风开关 / 音频设备 / 音频测试 / 含本程序)
//    与屏幕选择列表,右列所选屏幕的实时预览(点开时只截取一次,不做实时刷新,性能优先);
//    小条 ↔ 面板由后端一次性整体切换窗口尺寸,不再在原窗口上拉高;多屏打开入口时直接进入此态;
// ③ 录制中(小条):红点 + 计时 + 麦克风指示 + 暂停/继续 + 停止(屏幕指示不显示:启动时已固定)。
//    暂停 = 后端收尾当前分段并挂起计时,继续 = 起新分段;停止时各段拼接,暂停时段不进成片。
// 麦克风默认开(降噪/动态放大在后端滤镜链完成,无设备自动降级纯视频);
// 录制中可随时开 / 关:音轨全程采集,关 = 记静音区间,分段收尾时应用到音轨(成片对应区间无声);
// 摄像头画中画(可选):开启且检测到设备时弹置顶预览窗(可拖拽摆放),
// 预览窗作为普通桌面窗口直接被录进视频,拖到哪视频里就在哪;
// 屏幕只能选具体某一块(全屏录制已移除),多屏默认主屏;屏幕在启动时固定进 ffmpeg 命令,录制中途不可改。
// 「含本程序」默认开(开始时不最小化主窗口,本程序入镜);
// 关则开始时最小化主窗口,成品不含本程序。本窗口已被后端排除出屏幕捕获(Windows),不会被录进视频。
import { useEffect, useRef, useState } from "react";
import { emit } from "@tauri-apps/api/event";
import {
  AppWindow,
  Layers,
  Loader2,
  Mic,
  MicOff,
  Monitor,
  Pause,
  PictureInPicture2,
  Play,
  Settings,
  Square,
  Video,
  Volume2,
  X,
} from "lucide-react";

import { api } from "@/lib/api";
import type {
  AudioDeviceInfo,
  CamPosition,
  CameraDeviceInfo,
  RecordingStatus,
  ScreenInfo,
  ScreenPreview,
} from "@/lib/api-types";

// 麦克风 / 屏幕选择 / 含本程序 / 音频设备 / 摄像头(开关、设备、落位)的本地记忆键:下次开悬浮条记住上次选择
const MIC_STORAGE_KEY = "veltrix-rec-mic";
const SCREEN_STORAGE_KEY = "veltrix-rec-screen";
const INCLUDE_APP_STORAGE_KEY = "veltrix-rec-include-app";
const MIC_DEVICE_STORAGE_KEY = "veltrix-rec-mic-device";
const CAM_STORAGE_KEY = "veltrix-rec-cam";
const CAM_DEVICE_STORAGE_KEY = "veltrix-rec-cam-device";
const CAM_POS_STORAGE_KEY = "veltrix-rec-cam-pos";
const GRABBER_STORAGE_KEY = "veltrix-rec-grabber";

// 摄像头画中画四角落位(dotClass = 示意点在迷你屏幕上的角)
const CAM_POSITIONS: Array<{
  value: CamPosition;
  label: string;
  dotClass: string;
}> = [
  { value: "topLeft", label: "左上", dotClass: "left-1 top-1" },
  { value: "topRight", label: "右上", dotClass: "right-1 top-1" },
  { value: "bottomLeft", label: "左下", dotClass: "left-1 bottom-1" },
  { value: "bottomRight", label: "右下", dotClass: "right-1 bottom-1" },
];

// localStorage 回读的位置标识合法性校验
function isCamPos(v: string | null): v is CamPosition {
  return (
    v === "topLeft" ||
    v === "topRight" ||
    v === "bottomLeft" ||
    v === "bottomRight"
  );
}

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
  // 麦克风是否可用(= 本次录制存在音轨):决定录制中能否切换;无设备时按钮禁用
  const [micAvailable, setMicAvailable] = useState(false);
  // 含本程序:默认开(开始时不最小化主窗口,本程序一并入镜);关则成品不含本程序
  const [includeApp, setIncludeApp] = useState(
    () => localStorage.getItem(INCLUDE_APP_STORAGE_KEY) !== "0",
  );
  // 音频设备选择器:空串 = 自动(后端挑评分最高的默认设备);选择持久化
  const [micDevice, setMicDevice] = useState(
    () => localStorage.getItem(MIC_DEVICE_STORAGE_KEY) ?? "",
  );
  const [audioDevices, setAudioDevices] = useState<AudioDeviceInfo[]>([]);
  const [audioLoading, setAudioLoading] = useState(false);
  // 摄像头画中画:开关 / 设备 / 落位均持久化;设备列表进面板时枚举,空 = 未检测到摄像头
  const [camOn, setCamOn] = useState(
    () => localStorage.getItem(CAM_STORAGE_KEY) === "1",
  );
  const [camDevice, setCamDevice] = useState(
    () => localStorage.getItem(CAM_DEVICE_STORAGE_KEY) ?? "",
  );
  const [camPos, setCamPos] = useState<CamPosition>(() => {
    const v = localStorage.getItem(CAM_POS_STORAGE_KEY);
    return isCamPos(v) ? v : "topRight";
  });
  const [cameras, setCameras] = useState<CameraDeviceInfo[]>([]);
  const [camsLoading, setCamsLoading] = useState(false);
  // 抓屏方式:gdi = 兼容模式(DDA 会话建立 / 释放的驱动级闪黑屏可规避,CPU 略高)
  const [grabber, setGrabber] = useState(() =>
    localStorage.getItem(GRABBER_STORAGE_KEY) === "gdi" ? "gdi" : "auto",
  );
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

  // 展开 / 收起设置面板已移除:小条 ↔ 配置面板由后端整体切换窗口尺寸,见 enterPreview / collapse

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

  // 摄像头开关:未检测到设备时不允许开(录了也没有画面来源)
  function toggleCam() {
    if (cameras.length === 0) return;
    setCamOn((v) => {
      const next = !v;
      localStorage.setItem(CAM_STORAGE_KEY, next ? "1" : "0");
      return next;
    });
  }

  // 选择摄像头设备 / 画中画落位:记忆到 localStorage,开始录制时传给后端
  function selectCamDevice(name: string) {
    setCamDevice(name);
    localStorage.setItem(CAM_DEVICE_STORAGE_KEY, name);
  }

  function selectCamPos(pos: CamPosition) {
    setCamPos(pos);
    localStorage.setItem(CAM_POS_STORAGE_KEY, pos);
  }

  function selectGrabber(v: string) {
    setGrabber(v);
    localStorage.setItem(GRABBER_STORAGE_KEY, v);
  }

  // 枚举摄像头(进面板时与音频设备一起加载;空数组 = 未检测到,开关禁用)
  async function loadCameras() {
    setCamsLoading(true);
    try {
      setCameras(await api.listCameras());
    } catch (e) {
      console.debug("枚举摄像头失败:", e);
    } finally {
      setCamsLoading(false);
    }
  }

  // 配置面板(重)开时回填上次波形
  useEffect(() => {
    if (previewOn && lastWaveRef.current && waveCanvasRef.current) {
      paintWave(waveCanvasRef.current, lastWaveRef.current);
    }
  }, [previewOn]);

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
  // 多屏且未在录制 → 直接进入配置面板选屏;单屏留准备态,点「开始」直接开录
  useEffect(() => {
    let disposed = false;
    (async () => {
      const status = await api
        .getRecordingStatus()
        .catch((e) => (console.debug("获取录制状态失败:", e), null));
      if (disposed) return;
      if (status?.recording) {
        setRecording(true);
        setMicAvailable(status.withMic);
        setMicOn(status.micOn);
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

  // 进配置面板:小条 ↔ 面板整体切换窗口尺寸走后端(悬浮窗无窗口控制权限);
  // 首次进入截一次全部屏幕的缩略图(之后左列换屏、右列预览即时对应切换,无需重抓),
  // 顺带加载音频设备清单
  async function enterPreview() {
    if (busy) return;
    setBusy(true);
    try {
      await api.setRecordingOverlayPreview(true);
      setPreviewOn(true);
      setTestMsg(null);
      if (audioDevices.length === 0 && !audioLoading) void loadAudioDevices();
      if (cameras.length === 0 && !camsLoading) void loadCameras();
      if (Object.keys(tiles).length === 0) await loadTiles();
    } finally {
      setBusy(false);
    }
  }

  // 收起回小条:面板态关闭,配置项(localStorage 记忆)保留,随时再开
  function collapse() {
    setPreviewOn(false);
    api
      .setRecordingOverlayPreview(false)
      .catch((e) => console.debug("收起配置面板失败:", e));
  }

  // 开始录制(预览态「开始录制」或单屏准备态「开始」直录):后端会把窗口缩回小条;
  // 未开「含本程序」时还会最小化主窗口(成品不含本程序)
  async function start() {
    // screenIdx 为 null(枚举失败)也放行:后端按 null 录整虚拟桌面,不至于点不动
    if (busy) return;
    setBusy(true);
    try {
      // 麦克风不可用 / 屏幕下标失效时后端自动降级(纯视频 / 重选屏幕),前端无需区分
      const s = await api.startScreenRecording({
        micOn,
        screenIndex: screenIdx,
        includeApp,
        micDevice,
        camOn: camOn && cameras.length > 0,
        camDevice,
        camPosition: camPos,
        grabber: grabber === "gdi" ? "gdi" : null,
      });
      setPreviewOn(false); // 后端会把窗口缩回小条
      setTiles({});
      setRecording(true);
      setMicAvailable(s.withMic);
      setMicOn(s.micOn);
      syncTimer(s);
    } catch (e) {
      // 失败留在当前态可重试;悬浮窗没有 Toaster,把错误转发主窗口弹提示(避免「点了没反应」)
      void emit("recording-failed", {
        message: `开始录制失败: ${String(e)}`,
      }).catch(() => {});
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

  // 录制中随时开关麦克风:后端把开关时间记为静音区间(分段收尾时应用到音轨),
  // 界面立即翻转,录制不中断;成片声音与开关时间线一致
  async function toggleMicLive() {
    if (busy) return;
    setBusy(true);
    try {
      const s = await api.toggleRecordingMic();
      setMicOn(s.micOn);
    } catch {
      // 失败保持原状态可重试(悬浮窗无 Toaster,无法弹提示)
    } finally {
      setBusy(false);
    }
  }

  // 准备态「开始」:多屏先进配置面板选屏;单屏直接开录
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

  // 麦克风开关按钮(小条快捷开关;配置面板左列另有完整开关行)
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

  // 设置齿轮(小条):打开配置面板(左右结构:左设备配置 + 选屏,右所选屏预览)
  const settingsButton = (
    <button
      type="button"
      onClick={() => void enterPreview()}
      title="录屏设置"
      className="inline-flex size-7 shrink-0 items-center justify-center rounded-full text-muted-foreground transition-colors hover:bg-accent"
    >
      <Settings className="size-3.5" />
    </button>
  );

  // 推荐设备名(「自动」选项的展示用)
  const recommendedMic = audioDevices.find((d) => d.recommended);

  // 开关行右端的滑块(面板左列麦克风 / 含本程序共用)
  const toggleKnob = (on: boolean) => (
    <span
      className={`relative h-4 w-7 shrink-0 rounded-full transition-colors ${
        on ? "bg-primary" : "bg-muted"
      }`}
    >
      <span
        className={`absolute top-0.5 size-3 rounded-full bg-white shadow transition-all ${
          on ? "left-3.5" : "left-0.5"
        }`}
      />
    </span>
  );

  // 配置面板左列:设备配置(麦克风 / 音频设备 / 音频测试 / 含本程序)+ 屏幕选择列表;
  // 左列选中哪块屏,右列预览即时对应切换(缩略图只在进面板时截一次)
  const configColumn = (
    <div className="flex w-[212px] shrink-0 flex-col gap-2 overflow-y-auto border-r border-border/60 pr-3">
      <button
        type="button"
        onClick={toggleMic}
        aria-pressed={micOn}
        title={micOn ? "麦克风已开(点击关闭)" : "麦克风已关(点击开启)"}
        className="flex items-center justify-between rounded-lg px-1 py-0.5 text-xs text-foreground transition-colors hover:bg-accent"
      >
        <span className="flex items-center gap-1.5">
          {micOn ? (
            <Mic className="size-3.5" />
          ) : (
            <MicOff className="size-3.5" />
          )}
          麦克风
        </span>
        {toggleKnob(micOn)}
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
        className="h-10 w-full rounded-md bg-muted/40"
      />
      <div className="flex items-center gap-2">
        <button
          type="button"
          onClick={() => void testAudio()}
          disabled={testing || !micOn}
          title="用选中设备录 3 秒并回放(与正式录制同一降噪 / 放大链路)"
          className="inline-flex h-7 shrink-0 items-center gap-1.5 rounded-lg border border-border px-3 text-xs text-muted-foreground transition-colors hover:bg-accent hover:text-foreground disabled:opacity-50"
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
        <p className="px-1 text-[11px] leading-relaxed text-muted-foreground">
          麦克风已关:测试与录制均为纯视频
        </p>
      )}
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
        {toggleKnob(includeApp)}
      </button>
      <p className="px-1 text-[10px] leading-snug text-muted-foreground">
        {includeApp
          ? "开始后主窗口保持前台,本程序入镜"
          : "开始后主窗口最小化,不入镜"}
      </p>
      {/* 摄像头画中画:未检测到设备时开关禁用;开启后选设备与四角落位 */}
      <button
        type="button"
        onClick={toggleCam}
        aria-pressed={camOn}
        disabled={camsLoading || cameras.length === 0}
        title={
          cameras.length === 0
            ? camsLoading
              ? "正在检测摄像头…"
              : "未检测到摄像头"
            : camOn
              ? "摄像头已开(点击关闭,成片不含你的画面)"
              : "摄像头已关(点击开启,画面画中画合成进视频)"
        }
        className="flex items-center justify-between rounded-lg px-1 py-0.5 text-xs text-foreground transition-colors hover:bg-accent disabled:opacity-50"
      >
        <span className="flex items-center gap-1.5">
          <Video className="size-3.5 text-muted-foreground" />
          摄像头
        </span>
        {toggleKnob(camOn)}
      </button>
      {camOn && cameras.length > 0 && (
        <>
          <select
            value={camDevice}
            onChange={(e) => selectCamDevice(e.target.value)}
            className="h-7 w-full rounded-md border border-border bg-background px-1.5 text-xs text-foreground outline-none"
          >
            {cameras.map((c) => (
              <option key={c.name} value={c.name}>
                {c.name}
                {c.recommended ? "(推荐)" : ""}
              </option>
            ))}
          </select>
          <div className="flex items-center gap-1.5 px-1 text-xs text-foreground">
            <PictureInPicture2 className="size-3.5 text-muted-foreground" />
            画面位置
          </div>
          <div className="grid grid-cols-4 gap-1.5">
            {CAM_POSITIONS.map((p) => (
              <button
                key={p.value}
                type="button"
                onClick={() => selectCamPos(p.value)}
                title={p.label}
                aria-pressed={camPos === p.value}
                className={`relative h-7 rounded-lg border transition-colors ${
                  camPos === p.value
                    ? "border-primary bg-primary/10 text-primary"
                    : "border-border text-muted-foreground hover:bg-accent"
                }`}
              >
                <span
                  className={`absolute size-1.5 rounded-full bg-current ${p.dotClass}`}
                />
              </button>
            ))}
          </div>
        </>
      )}
      {/* 抓屏方式:GDI 兼容模式规避 DDA 会话建立 / 释放的驱动级闪黑屏 */}
      <div className="flex items-center gap-1.5 px-1 pt-1 text-xs text-foreground">
        <Layers className="size-3.5 text-muted-foreground" />
        抓屏方式
      </div>
      <select
        value={grabber}
        onChange={(e) => selectGrabber(e.target.value)}
        title={
          grabber === "gdi"
            ? "GDI 兼容模式:开始 / 停止不闪黑屏,CPU 占用略高"
            : "桌面复制 API:占用最低;若开始 / 停止时屏幕闪黑,选 GDI 兼容"
        }
        className="h-7 w-full rounded-md border border-border bg-background px-1.5 text-xs text-foreground outline-none"
      >
        <option value="auto">自动(推荐)</option>
        <option value="gdi">GDI 兼容(不闪黑)</option>
      </select>
      <div className="flex items-center gap-1.5 px-1 pt-1 text-xs text-foreground">
        <Monitor className="size-3.5 text-muted-foreground" />
        录制屏幕
      </div>
      <div className="flex flex-col gap-1.5">
        {screens.map((s) => {
          const active = screenIdx === s.index;
          return (
            <button
              key={s.index}
              type="button"
              onClick={() => selectScreen(s.index)}
              title={`录制屏幕 ${s.index + 1}`}
              className={`flex items-center gap-1.5 rounded-lg border px-2 py-1.5 text-xs transition-colors ${
                active
                  ? "border-primary bg-primary/10 text-primary"
                  : "border-border text-foreground hover:bg-accent"
              }`}
            >
              <Monitor className="size-3.5 shrink-0" />
              <span className="min-w-0 flex-1 truncate text-left">
                屏{s.index + 1}
                {s.primary ? "(主)" : ""}
              </span>
              <span className="shrink-0 text-[10px] text-muted-foreground">
                {s.width}×{s.height}
              </span>
            </button>
          );
        })}
      </div>
    </div>
  );

  // 录制中:红点 + 计时 + 麦克风开关 + 暂停/继续 + 停止(居中)。外层透明只负责居中,内层自适应宽度 + 圆弧角
  // 麦克风随时可切:后端记静音区间(分段收尾时应用到音轨),不重启进程;
  // 屏幕指示不显示——录制目标启动时已固定,展示出来也没有操作意义
  if (recording) {
    return (
      <div className="flex h-screen w-screen items-center justify-center">
        <div className="inline-flex items-center gap-2 rounded-2xl border border-border bg-background/95 px-3 py-1.5 backdrop-blur">
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
          {/* 麦克风开 / 关(随时可切):关 = 后端记静音区间,成片对应区间无声 */}
          <button
            type="button"
            onClick={() => void toggleMicLive()}
            disabled={busy || !micAvailable}
            title={
              !micAvailable
                ? "未接入麦克风设备,本次为纯视频录制"
                : micOn
                  ? "麦克风开启中(点击静音,成片对应区间无声)"
                  : "麦克风已静音(点击开启)"
            }
            className={`inline-flex size-5 shrink-0 items-center justify-center rounded-full transition-colors ${
              micOn
                ? "text-primary hover:bg-primary/10"
                : "text-muted-foreground hover:bg-accent"
            } disabled:opacity-60`}
          >
            {micOn ? (
              <Mic className="size-3.5" />
            ) : (
              <MicOff className="size-3.5" />
            )}
          </button>
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

  // 配置面板态:左右结构。左列设备配置(麦克风 / 音频设备 / 音频测试 / 含本程序)+ 屏幕选择,
  // 右列所选屏幕的实时预览;底部 退出 / 开始录制,右上 收起回小条。
  // 窗口尺寸由后端整体切换(set_recording_overlay_preview),卡片铺满窗口、留出圆角边距。
  if (previewOn) {
    const selected = screens.find((s) => s.index === screenIdx);
    const tile = screenIdx !== null ? tiles[screenIdx] : undefined;
    return (
      <div className="flex h-screen w-screen items-center justify-center p-1.5">
        <div className="flex h-full w-full flex-col gap-3 rounded-2xl border border-border bg-background/95 p-4 backdrop-blur">
          {/* 头部:标题 + 收起(回小条,不结束本次录屏操作) */}
          <div className="flex shrink-0 items-center justify-between">
            <span className="text-sm font-medium text-foreground">
              录屏设置{micOn ? "(含音频)" : "(纯视频)"}
            </span>
            <button
              type="button"
              onClick={collapse}
              title="收起回小条"
              className="inline-flex size-7 shrink-0 items-center justify-center rounded-full text-muted-foreground transition-colors hover:bg-accent"
            >
              <X className="size-3.5" />
            </button>
          </div>
          {/* 左配置 / 右预览 */}
          <div className="flex min-h-0 flex-1 gap-3">
            {configColumn}
            <div className="flex min-w-0 flex-1 flex-col gap-2">
              <div className="flex min-h-0 flex-1 items-center justify-center overflow-hidden rounded-lg border border-border bg-black/80">
                {previewLoading && !tile ? (
                  <Loader2 className="size-5 animate-spin text-muted-foreground" />
                ) : tile ? (
                  <img
                    src={tile}
                    alt={`屏幕 ${(screenIdx ?? 0) + 1} 预览`}
                    className="max-h-full max-w-full object-contain"
                  />
                ) : (
                  <span className="text-[11px] text-muted-foreground">
                    暂无预览
                  </span>
                )}
              </div>
              {previewError ? (
                <p className="text-[11px] text-destructive">{previewError}</p>
              ) : (
                <p className="flex items-center gap-1 text-[11px] text-muted-foreground">
                  <Monitor className="size-3" />
                  {selected
                    ? `屏${selected.index + 1}${selected.primary ? "(主)" : ""} · ${selected.width}×${selected.height}`
                    : "未选择屏幕"}
                </p>
              )}
            </div>
          </div>
          {/* 底部按钮:退出在左(直接结束本次录屏操作,关悬浮窗 + 还原主窗口)、开始录制在右 */}
          <div className="flex shrink-0 items-center justify-between border-t border-border/60 pt-3">
            <button
              type="button"
              onClick={cancel}
              className="inline-flex h-8 items-center rounded-lg border border-border px-4 text-xs text-muted-foreground transition-colors hover:bg-accent hover:text-foreground"
            >
              退出
            </button>
            <button
              type="button"
              onClick={() => void start()}
              disabled={busy || previewLoading || screenIdx === null}
              className="inline-flex h-8 shrink-0 items-center gap-1.5 rounded-lg bg-red-500 px-4 text-xs font-medium text-white transition-colors hover:bg-red-600 disabled:opacity-60"
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
  // 小条钉在窗口顶部;齿轮 / 屏幕选择进配置面板(窗口尺寸由后端整体切换)
  return (
    <div className="flex h-screen w-screen flex-col items-center pt-1.5">
      <div className="inline-flex items-center gap-1.5 rounded-2xl border border-border bg-background/95 px-2 py-1.5 backdrop-blur">
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
        {/* 当前屏幕:点击进配置面板换屏;仅多屏时显示 */}
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
    </div>
  );
}
