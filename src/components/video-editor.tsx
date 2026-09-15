// 视频剪辑编辑器(剪映式布局,铺满整个工作区):
// 左侧片段面板(头部带 打开 / 返回首页 入口);右侧预览列 = 预览区(黑场衬底、等比缩放居中,
// 点击播放/暂停)+ 播放器控制条;底部多轨道时间轴(对齐专业 NLE:视频轨 / 音频轨 / 字幕轨,
// 播放控制:空格 播放/暂停;工具行:编辑工具(选择/分割/左右全选/删除选中,快捷键 A/B/[/]/Delete)+ 撤销重做(Ctrl+Z/Shift+Z)、
// 增轨;轨道头点击设为激活轨,激活轨上框选后「添加片段」落到该轨;
// 刻度尺与轨道空白处按住拖动 = scrub,拖手柄设入点/出点,拖选区平移,点击片段选中并定位播放头)。
// 轨道与片段变化自动存草稿(localStorage,见 lib/video-drafts.ts),首页可恢复继续剪辑。
// 字幕轨支持时间线编辑、预览与导出烧录;视频片段支持裁剪、旋转、缩放、位置、透明度与画中画合成。
// VideoEditorPanel 是首页 / 编辑器的状态容器,首页见 video-editor-home.tsx。
import { memo, useEffect, useLayoutEffect, useMemo, useRef, useState, type CSSProperties, type ReactNode } from "react";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { listen } from "@tauri-apps/api/event";
import {
  ArrowDown,
  ArrowDownToLine,
  ArrowLeftToLine,
  ArrowRightToLine,
  ArrowUp,
  ArrowUpToLine,
  AlertTriangle,
  AudioLines,
  AudioWaveform,
  Captions,
  Clapperboard,
  ClipboardPaste,
  Combine,
  Copy,
  Crop,
  Download,
  Eraser,
  Eye,
  EyeOff,
  Flag,
  FastForward,
  FolderInput,
  House,
  ListChecks,
  ListCollapse,
  ListPlus,
  Link2,
  Loader2,
  Lock,
  LockOpen,
  MoreVertical,
  Magnet,
  MousePointer2,
  Move,
  Pause,
  PanelLeftDashed,
  PanelRightDashed,
  Pencil,
  Play,
  Redo2,
  Repeat2,
  Rewind,
  RotateCw,
  Scissors,
  SkipBack,
  SkipForward,
  ScanLine,
  SquarePlus,
  Trash2,
  Unplug,
  Undo2,
  Volume2,
  VolumeX,
  X,
  ZoomIn,
  ZoomOut,
} from "lucide-react";
import { toast } from "sonner";

import { api } from "@/lib/api";
import type { CreationExportProgress, EditorTrack, ExportQuality, ExportResolution, TrackClip, TrackType, TransitionInput, VideoInfo, VideoTransformInput } from "@/lib/api-types";
import { useMediaFileUrl } from "@/lib/media-file-url";
import { format } from "date-fns";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { SimpleTooltip } from "@/components/SimpleTooltip";
import { useTheme } from "@/components/theme-provider";
import { VideoEditorHome } from "@/components/video-editor-home";
import { saveDraft, setDraftCover, type TimelineMarker, type VideoDraft } from "@/lib/video-drafts";
import { captureVideoCover } from "@/lib/video-cover";
import { fmt, fmtTick } from "@/lib/timefmt";
import { MaterialPanel } from "@/components/video-editor-panel";

// 秒时间格式化统一走 @/lib/timefmt(fmt = 时:分:秒:厘秒,fmtTick = 刻度尺短格式)

// 从路径取文件名(兼容正反斜杠,草稿名用)
function baseName(path: string): string {
  return path.split(/[\\/]/).pop() ?? path;
}

// 浏览器媒体元素只读取元数据,避免为取得时长把整段外部音频解码进内存。
function readAudioDuration(url: string): Promise<number> {
  return new Promise((resolve, reject) => {
    const audio = new Audio();
    const finish = (error?: Error, duration?: number) => {
      window.clearTimeout(timer);
      audio.removeAttribute("src");
      audio.load();
      if (error) reject(error);
      else resolve(duration!);
    };
    const timer = window.setTimeout(() => finish(new Error("读取音频时长超时")), 15_000);
    audio.preload = "metadata";
    audio.onloadedmetadata = () => {
      if (Number.isFinite(audio.duration) && audio.duration > 0) finish(undefined, audio.duration);
      else finish(new Error("无法读取音频时长"));
    };
    audio.onerror = () => finish(new Error("音频格式无法读取"));
    audio.src = url;
  });
}

// 片段时间轴位置(序列时间):position 缺省 = 源起点(旧草稿迁移口径)
const clipPos = (c: TrackClip) => c.position ?? c.start;

function textCanvasPlacement(clip: TrackClip) {
  const legacyY = clip.textPosition === "top" ? 10 : clip.textPosition === "center" ? 50 : 90;
  return {
    x: Math.min(100, Math.max(0, clip.textX ?? 50)),
    y: Math.min(100, Math.max(0, clip.textY ?? legacyY)),
    rotation: clip.textRotation ?? 0,
  };
}
const clipSpeed = (c: TrackClip) => Math.max(0.25, Math.min(4, c.speed ?? 1));
function speedCurvePoints(c: TrackClip) {
  const sourceLength = Math.max(0, c.end - c.start);
  const points = [...(c.speedCurve ?? [])]
    .map((point) => ({ ...point, offset: Math.max(0, Math.min(sourceLength, point.offset)), speed: Math.max(0.25, Math.min(4, point.speed)) }))
    .sort((a, b) => a.offset - b.offset)
    .filter((point, index, values) => index === 0 || Math.abs(point.offset - values[index - 1].offset) > 0.001);
  if (!points.length) return [
    { id: "start", offset: 0, speed: clipSpeed(c) },
    { id: "end", offset: sourceLength, speed: clipSpeed(c) },
  ];
  if (points[0].offset > 0.001) points.unshift({ id: "start", offset: 0, speed: clipSpeed(c) });
  if (points[points.length - 1].offset < sourceLength - 0.001) points.push({ id: "end", offset: sourceLength, speed: clipSpeed(c) });
  return points;
}

function speedSegmentDuration(sourceLength: number, fromSpeed: number, toSpeed: number) {
  if (sourceLength <= 0) return 0;
  if (Math.abs(toSpeed - fromSpeed) < 0.0001) return sourceLength / fromSpeed;
  return sourceLength * Math.log(toSpeed / fromSpeed) / (toSpeed - fromSpeed);
}

function sourceOffsetToTimeline(c: TrackClip, sourceOffset: number) {
  const points = speedCurvePoints(c);
  const target = Math.max(0, Math.min(c.end - c.start, sourceOffset));
  let timeline = 0;
  for (let i = 0; i < points.length - 1; i++) {
    const from = points[i];
    const to = points[i + 1];
    if (target <= from.offset) break;
    const distance = Math.min(target, to.offset) - from.offset;
    const ratio = distance / Math.max(0.001, to.offset - from.offset);
    const speedAtTarget = from.speed + (to.speed - from.speed) * ratio;
    timeline += speedSegmentDuration(distance, from.speed, speedAtTarget);
    if (target <= to.offset) break;
  }
  return timeline;
}

function timelineOffsetToSource(c: TrackClip, timelineOffset: number) {
  const points = speedCurvePoints(c);
  let remaining = Math.max(0, timelineOffset);
  for (let i = 0; i < points.length - 1; i++) {
    const from = points[i];
    const to = points[i + 1];
    const sourceLength = to.offset - from.offset;
    const duration = speedSegmentDuration(sourceLength, from.speed, to.speed);
    if (remaining > duration) {
      remaining -= duration;
      continue;
    }
    if (Math.abs(to.speed - from.speed) < 0.0001) return from.offset + remaining * from.speed;
    const slope = (to.speed - from.speed) / sourceLength;
    return from.offset + from.speed * (Math.exp(slope * remaining) - 1) / slope;
  }
  return Math.max(0, c.end - c.start);
}

function clipSpeedAtSource(c: TrackClip, sourceOffset: number) {
  const points = speedCurvePoints(c);
  const target = Math.max(0, Math.min(c.end - c.start, sourceOffset));
  for (let i = 0; i < points.length - 1; i++) {
    const from = points[i];
    const to = points[i + 1];
    if (target <= to.offset + 0.001) {
      const ratio = (target - from.offset) / Math.max(0.001, to.offset - from.offset);
      return from.speed + (to.speed - from.speed) * Math.max(0, Math.min(1, ratio));
    }
  }
  return points[points.length - 1]?.speed ?? clipSpeed(c);
}

const clipLen = (c: TrackClip) => sourceOffsetToTimeline(c, Math.max(0, c.end - c.start));

function interpolatedTransform(clip: TrackClip, sequenceTime: number): VideoTransformInput {
  const base = clip.transform ?? {};
  const frames = [...(clip.keyframes ?? [])]
    .sort((a, b) => a.offset - b.offset);
  if (!frames.length) return base;
  const offset = Math.max(0, Math.min(clipLen(clip), sequenceTime - clipPos(clip)));
  const points = frames[0].offset > 0.001
    ? [{ id: "base", offset: 0, scale: base.scale ?? 1, positionX: base.positionX ?? 0, positionY: base.positionY ?? 0 }, ...frames]
    : frames;
  const right = points.findIndex((frame) => frame.offset >= offset);
  if (right <= 0) return { ...base, scale: points[0].scale, positionX: points[0].positionX, positionY: points[0].positionY };
  if (right < 0) {
    const last = points[points.length - 1];
    return { ...base, scale: last.scale, positionX: last.positionX, positionY: last.positionY };
  }
  const from = points[right - 1];
  const to = points[right];
  const linearRatio = Math.max(0, Math.min(1, (offset - from.offset) / Math.max(0.001, to.offset - from.offset)));
  const ratio = to.easing === "easeIn"
    ? linearRatio * linearRatio
    : to.easing === "easeOut"
      ? 1 - (1 - linearRatio) * (1 - linearRatio)
      : to.easing === "easeInOut"
        ? linearRatio * linearRatio * (3 - 2 * linearRatio)
        : linearRatio;
  const lerp = (a: number, b: number) => a + (b - a) * ratio;
  return {
    ...base,
    scale: lerp(from.scale, to.scale),
    positionX: lerp(from.positionX, to.positionX),
    positionY: lerp(from.positionY, to.positionY),
  };
}

// 编辑态保留单片段；导出时把连续速度曲线近似为少量恒速区间。
// 每个区间使用对数平均速度，区间总时长与曲线积分完全一致，避免成片累计漂移。
function expandSpeedCurveClip(clip: TrackClip): TrackClip[] {
  if ((clip.speedCurve?.length ?? 0) < 2) return [clip];
  const points = speedCurvePoints(clip);
  const pieces: TrackClip[] = [];
  for (let i = 0; i < points.length - 1; i++) {
    const from = points[i];
    const to = points[i + 1];
    const subdivisions = Math.max(1, Math.min(4, Math.ceil(Math.abs(to.speed - from.speed) / 0.55)));
    for (let part = 0; part < subdivisions; part++) {
      const sourceStart = from.offset + (to.offset - from.offset) * part / subdivisions;
      const sourceEnd = from.offset + (to.offset - from.offset) * (part + 1) / subdivisions;
      const timelineStart = sourceOffsetToTimeline(clip, sourceStart);
      const timelineEnd = sourceOffsetToTimeline(clip, sourceEnd);
      const timelineLength = Math.max(0.001, timelineEnd - timelineStart);
      const transform = interpolatedTransform(clip, clipPos(clip) + timelineStart);
      const innerFrames = (clip.keyframes ?? [])
        .filter((frame) => frame.offset > timelineStart + 0.001 && frame.offset < timelineEnd - 0.001)
        .map((frame) => ({ ...frame, offset: frame.offset - timelineStart }));
      const endTransform = interpolatedTransform(clip, clipPos(clip) + timelineEnd);
      pieces.push({
        ...clip,
        id: `${clip.id}-speed-${i}-${part}`,
        start: clip.start + sourceStart,
        end: clip.start + sourceEnd,
        position: clipPos(clip) + timelineStart,
        speed: (sourceEnd - sourceStart) / timelineLength,
        speedCurve: undefined,
        transform,
        keyframes: clip.keyframes?.length ? [
          ...innerFrames,
          {
            id: `${clip.id}-speed-end-${i}-${part}`,
            offset: timelineLength,
            scale: endTransform.scale ?? 1,
            positionX: endTransform.positionX ?? 0,
            positionY: endTransform.positionY ?? 0,
            easing: "linear",
          },
        ] : undefined,
      });
    }
  }
  return pieces.length
    ? pieces.map((piece, index) => ({
        ...piece,
        transitionIn: index === 0 ? clip.transitionIn : { kind: "none", durationSecs: 0 },
        fadeIn: index === 0 ? clip.fadeIn : 0,
        fadeOut: index === pieces.length - 1 ? clip.fadeOut : 0,
      }))
    : [clip];
}

function slicedSpeedCurve(clip: TrackClip, sourceStart: number, sourceEnd: number) {
  if ((clip.speedCurve?.length ?? 0) < 2) return undefined;
  const points = speedCurvePoints(clip);
  const at = (offset: number) => clipSpeedAtSource(clip, offset);
  return [
    { id: crypto.randomUUID(), offset: 0, speed: at(sourceStart) },
    ...points
      .filter((point) => point.offset > sourceStart + 0.001 && point.offset < sourceEnd - 0.001)
      .map((point) => ({ ...point, id: crypto.randomUUID(), offset: point.offset - sourceStart })),
    { id: crypto.randomUUID(), offset: sourceEnd - sourceStart, speed: at(sourceEnd) },
  ];
}

function previewColorFilter(transform?: VideoTransformInput): string {
  const preset = transform?.filter ?? "none";
  const presetValues = {
    none: [1, 1, 1, 0],
    vivid: [1.02, 1.08, 1.35, 0],
    cinema: [0.98, 1.14, 0.88, -4],
    warm: [1.01, 1.03, 1.08, -8],
    cool: [1, 1.04, 1.03, 8],
    mono: [1, 1.08, 0, 0],
  }[preset];
  const brightness = presetValues[0] * (1 + (transform?.brightness ?? 0));
  const contrast = presetValues[1] * (transform?.contrast ?? 1);
  const saturation = presetValues[2] * (transform?.saturation ?? 1);
  const hue = presetValues[3] + (transform?.hue ?? 0);
  const temperature = transform?.temperature ?? 0;
  return `brightness(${brightness}) contrast(${contrast}) saturate(${saturation}) hue-rotate(${hue}deg) sepia(${Math.abs(temperature) * 0.18})`;
}

// 紧凑排列(自动拼接):按 position 排序后逐段贴上前一段右缘,间隙与重叠一并消除,
// 首段位置保留;无变化返回 null(调用方据此跳过无效的撤销入栈)
function compactClips(clips: TrackClip[]): TrackClip[] | null {
  if (clips.length < 2) return null;
  const sorted = [...clips].sort((a, b) => clipPos(a) - clipPos(b) || a.start - b.start);
  let cursor = clipPos(sorted[0]);
  let changed = false;
  const out = sorted.map((c) => {
    const next =
      Math.abs(clipPos(c) - cursor) < 0.001 ? c : { ...c, position: cursor };
    if (next !== c) changed = true;
    cursor += clipLen(next);
    return next;
  });
  return changed ? out : null;
}

// 轨道类型 → 展示元数据(图标 / 中文名 / 片段头条与帧体配色,剪映式青色视频轨)。
// 帧体分 light / dark 两套底:light 用浅彩底, dark 用深彩底;头条保持饱和色(白字两主题均可读)。
const TRACK_META: Record<
  TrackType,
  { icon: typeof Clapperboard; label: string; headerClass: string; frameClass: string }
> = {
  video: { icon: Clapperboard, label: "视频", headerClass: "bg-teal-600/85 dark:bg-teal-700/80", frameClass: "border-teal-500/70 bg-teal-100 dark:border-teal-600/70 dark:bg-teal-950/60" },
  audio: { icon: AudioWaveform, label: "音频", headerClass: "bg-emerald-600/85 dark:bg-emerald-700/80", frameClass: "border-emerald-500/70 bg-emerald-100 dark:border-emerald-600/70 dark:bg-emerald-950/60" },
  text: { icon: Captions, label: "字幕", headerClass: "bg-amber-600/85 dark:bg-amber-700/80", frameClass: "border-amber-500/70 bg-amber-100 dark:border-amber-600/70 dark:bg-amber-950/60" },
};

// 片段主体纹理(纯 CSS 占位):视频 = 胶片分帧竖线,音频 = 双层竖条拟波形。
// 真实缩略图 / 波形需后端抽帧与音频峰值数据,接入前以此保持剪映式外观。
// waveLevel = 波形占比档位(0 小 / 1 中 / 2 大,⋮ 菜单可调),只影响音频纹理竖条带高度。
// 纹理色随主题:dark 用浅色线,light 用深色线,保证两种底色上都看得清。
function clipTexture(type: TrackType, waveLevel: number, isDark: boolean): CSSProperties {
  if (type === "audio") {
    const size = ["100% 30%, 100% 50%", "100% 55%, 100% 85%", "100% 80%, 100% 100%"][
      Math.min(2, Math.max(0, waveLevel))
    ];
    const bar = isDark ? "rgba(52,211,153,0.7)" : "rgba(5,150,105,0.55)";
    const barSoft = isDark ? "rgba(52,211,153,0.4)" : "rgba(5,150,105,0.3)";
    return {
      backgroundImage: `repeating-linear-gradient(90deg, ${bar} 0 2px, transparent 2px 5px), repeating-linear-gradient(90deg, ${barSoft} 0 2px, transparent 2px 9px)`,
      backgroundSize: size,
      backgroundPosition: "center, center",
    };
  }
  const line = isDark ? "rgba(255,255,255,0.13)" : "rgba(0,0,0,0.15)";
  return {
    backgroundImage: `repeating-linear-gradient(90deg, ${line} 0 1px, transparent 1px 30px)`,
  };
}

function newTrack(type: TrackType, existing: EditorTrack[]): EditorTrack {
  const n = existing.filter((t) => t.type === type).length + 1;
  return {
    id: crypto.randomUUID(),
    type,
    name: `${TRACK_META[type].label} ${n}`,
    clips: [],
  };
}

// 时间轴交互属性打包(组件 props 收敛,遵守参数 ≤4 约定 → 拆成状态 / 回调两组)
interface TimelineState {
  tracks: EditorTrack[];
  activeTrackId: string;
  duration: number;
  selStart: number;
  selEnd: number;
  // 片段头条展示用文件名(源视频名)
  fileName: string;
  // 选中的片段 id(点击选中,Ctrl+点击多选;选中片段高亮描边)
  selectedClipIds: string[];
  // 时间轴缩放:1 = 适配宽度,>1 放大(通道变宽、横向滚动)
  zoom: number;
  // 轨道高度(px,⋮ 菜单滑块可调)
  trackHeight: number;
  // 音频波形占比档位(0 小 / 1 中 / 2 大)
  waveLevel: number;
  // 胶片条缩略图 URL(后端抽帧;空数组 = 未就绪,视频片段回退 CSS 纹理)
  thumbUrls: string[];
  // 当前缩略图/主波形对应的素材;其他素材片段回退纹理,避免错贴画面。
  thumbSourcePath: string;
  defaultInputPath: string;
  // 音频波形峰值(前端 WebAudio 解码;null = 未就绪,音频片段回退 CSS 纹理)
  audioPeaks: Float32Array | null;
  // 外部音频按绝对路径缓存波形;不随草稿持久化,打开工程后按需重建。
  externalAudioPeaks: Map<string, Float32Array>;
  markers: TimelineMarker[];
  snapEnabled: boolean;
  fps: number;
  transitionKind: "none" | "dissolve" | "fade";
  transitionSecs: number;
}

// Timeline 的 DOM 句柄通道:current 高频变化不走 props(避免每帧重渲染整棵树),
// 播放头位置与跟随滚动由 VideoEditor 直接写 DOM;playheadPct 供挂载/重挂载时定位初始位置
interface TimelineRefs {
  scroll: HTMLDivElement | null;
  lanes: HTMLDivElement | null;
  playhead: HTMLDivElement | null;
  playheadPct: string;
  laneWidth: number;
  viewportWidth: number;
  playheadTime: number;
}

// 右键菜单目标:kind = track(轨道头)/ clip(片段)/ lane(轨道空白处)
interface CtxTarget {
  x: number;
  y: number;
  kind: "track" | "clip" | "lane";
  trackId: string;
  clipId?: string;
}

interface ClipTrimCommit {
  trackId: string;
  clipId: string;
  start: number;
  end: number;
  position: number;
}

interface TimelineActions {
  onSeek: (t: number) => void;
  onOpenClip: (clip: TrackClip) => void;
  onSelection: (start: number, end: number) => void;
  // scrub(按住拖播放头)开始 / 结束:父组件据此暂停播放、松手恢复
  onScrubChange?: (scrubbing: boolean) => void;
  onSelectTrack: (id: string) => void;
  onRemoveTrack: (id: string) => void;
  onRemoveClip: (trackId: string, clipId: string) => void;
  // 点击片段选中(additive = Ctrl+点击多选切换)
  onSelectClip: (clipId: string, additive: boolean) => void;
  onMoveTrack: (from: number, to: number) => void;
  // 轨道开关:锁定(禁增删)/ 可见(隐藏不参与导出)/ 静音(音频轨不混音)
  onToggleFlag: (id: string, flag: "locked" | "hidden" | "muted") => void;
  onZoomChange: (zoom: number) => void;
  // 右键菜单:坐标 + 目标(轨道头 / 片段 / 空白通道),由父组件弹出菜单
  onContextMenu: (target: CtxTarget) => void;
  // 双击轨道头重命名
  onRenameTrack: (id: string) => void;
  // 片段拖动落点(position):拖动结束提交一次(入撤销历史)
  onCommitClipMove: (trackId: string, clipId: string, position: number) => void;
  onCommitClipTrim: (trim: ClipTrimCommit) => void;
  onRemoveMarker: (id: string) => void;
}

// 音频片段帧体:真实波形画布(中线镜像;只画片段 [start,end] 对应的峰值切片;
// ResizeObserver 保证缩放 / 拖动列宽时按新宽度重绘;level = 波形占比档位)。
function WaveformBody({
  peaks,
  isDark,
  level,
  start,
  end,
  duration,
}: {
  peaks: Float32Array;
  isDark: boolean;
  level: number;
  start: number;
  end: number;
  duration: number;
}) {
  const ref = useRef<HTMLDivElement>(null);
  useEffect(() => {
    const host = ref.current;
    if (!host) return;
    let frame = 0;
    const draw = () => {
      frame = 0;
      const dpr = window.devicePixelRatio || 1;
      const w = host.clientWidth;
      const h = host.clientHeight;
      if (!w || !h) return;
      // 时间轴放大后片段可能有数万像素宽。单张 4K 画布再用 CSS 拉伸会把波形
      // 一同拉粗，因此按固定宽度切成多张画布，每块都保持真实 CSS 像素比例。
      const tileSize = 2048;
      const tileCount = Math.max(1, Math.ceil(w / tileSize));
      while (host.childElementCount > tileCount) host.lastElementChild?.remove();
      while (host.childElementCount < tileCount) host.appendChild(document.createElement("canvas"));
      const renderDpr = Math.min(1.5, dpr);
      const n = peaks.length;
      const from = Math.max(0, Math.floor((start / duration) * n));
      const to = Math.min(n, Math.max(from + 1, Math.ceil((end / duration) * n)));
      const span = to - from;
      const amp = (h / 2) * [0.35, 0.65, 0.95][Math.min(2, Math.max(0, level))];
      const mid = h / 2;
      for (let tileIndex = 0; tileIndex < tileCount; tileIndex++) {
        const canvas = host.children[tileIndex] as HTMLCanvasElement;
        const tileLeft = tileIndex * tileSize;
        const tileWidth = Math.min(tileSize, w - tileLeft);
        canvas.style.position = "absolute";
        canvas.style.left = `${tileLeft}px`;
        canvas.style.top = "0";
        canvas.style.width = `${tileWidth}px`;
        canvas.style.height = "100%";
        canvas.width = Math.max(1, Math.round(tileWidth * renderDpr));
        canvas.height = Math.max(1, Math.round(h * renderDpr));
        const ctx = canvas.getContext("2d");
        if (!ctx) continue;
        ctx.setTransform(renderDpr, 0, 0, renderDpr, 0, 0);
        ctx.clearRect(0, 0, tileWidth, h);
        ctx.fillStyle = isDark ? "rgba(52,211,153,0.85)" : "rgba(5,150,105,0.8)";
        ctx.fillRect(0, Math.floor(mid), tileWidth, 1);
        for (let x = 0; x < tileWidth; x += 2) {
          const globalX = tileLeft + x;
          const bucketStart = from + (globalX / w) * span;
          const bucketEnd = Math.min(to, from + ((globalX + 2) / w) * span);
          let v = 0;
          if (bucketEnd - bucketStart < 1) {
            // 放大到单个峰值跨越多个像素时做线性插值，避免出现重复的粗方块。
            const base = Math.min(to - 1, Math.max(from, Math.floor(bucketStart)));
            const next = Math.min(to - 1, base + 1);
            const mix = bucketStart - Math.floor(bucketStart);
            v = (peaks[base] ?? 0) * (1 - mix) + (peaks[next] ?? 0) * mix;
          } else {
            // 缩小时一列覆盖多个峰值桶，取最大值以免漏掉短促声音。
            const bucketFrom = Math.max(from, Math.floor(bucketStart));
            const bucketTo = Math.min(to, Math.max(bucketFrom + 1, Math.ceil(bucketEnd)));
            for (let bucket = bucketFrom; bucket < bucketTo; bucket++) {
              v = Math.max(v, peaks[bucket] ?? 0);
            }
          }
          const bh = Math.max(1, Math.pow(v, 0.72) * amp);
          ctx.fillRect(x, mid - bh, 1, bh * 2);
        }
      }
    };
    const scheduleDraw = () => {
      if (!frame) frame = requestAnimationFrame(draw);
    };
    scheduleDraw();
    const ro = new ResizeObserver(scheduleDraw);
    ro.observe(host);
    return () => {
      ro.disconnect();
      if (frame) cancelAnimationFrame(frame);
    };
  }, [peaks, isDark, level, start, end, duration]);
  return <div ref={ref} className="relative h-full w-full overflow-hidden" />;
}

// 抽帧固定 160x90(后端 crop),平铺 tile 宽高比恒为 16:9
const THUMB_ASPECT = 16 / 9;
// 单片段 tile 数上限:极端放大时同一帧重复平铺,限制 img 节点数量
const FILMSTRIP_MAX_TILES = 240;

// 视频片段帧体:胶片条平铺。每个 tile 恒为「高度 × 16:9」的固定像素宽,
// 缩放只增减 tile 数量(每 tile 取时间上最近的抽帧),杜绝 object-cover 拉伸 / 压扁
function Filmstrip({
  thumbs,
  start,
  end,
  duration,
  pxPerSec,
}: {
  thumbs: string[];
  start: number;
  end: number;
  duration: number;
  pxPerSec: number;
}) {
  const ref = useRef<HTMLDivElement>(null);
  const [h, setH] = useState(0);
  useEffect(() => {
    const el = ref.current;
    if (!el) return;
    const ro = new ResizeObserver(() => setH(el.clientHeight));
    ro.observe(el);
    setH(el.clientHeight);
    return () => ro.disconnect();
  }, []);
  const clipW = (end - start) * pxPerSec;
  const tileW = Math.max(8, h * THUMB_ASPECT);
  const tiles = Math.min(FILMSTRIP_MAX_TILES, Math.max(1, Math.round(clipW / tileW)));
  const seg = duration / thumbs.length;
  const items: ReactNode[] = [];
  for (let i = 0; i < tiles; i++) {
    // tile 中点对应的源时间 → 取最近一帧;放大超抽帧密度时同帧重复(与专业剪辑器一致)
    const tMid = start + ((i + 0.5) / tiles) * (end - start);
    const idx = Math.min(thumbs.length - 1, Math.max(0, Math.floor(tMid / seg)));
    items.push(
      <img
        key={i}
        src={thumbs[idx]}
        alt=""
        draggable={false}
        className="h-full shrink-0 object-cover"
        style={{ width: `${(100 / tiles).toFixed(3)}%` }}
      />,
    );
  }
  return (
    <div ref={ref} className="flex min-h-0 flex-1 overflow-hidden">
      {h > 0 && pxPerSec > 0 && items}
    </div>
  );
}

// 轨道头控制小按钮(锁定 / 可见 / 静音):on = 功能处于「生效侧」时的着色
function TrackFlagButton({
  on,
  activeClass,
  tooltip,
  onClick,
  children,
}: {
  on: boolean;
  activeClass: string;
  tooltip: string;
  onClick: () => void;
  children: ReactNode;
}) {
  return (
    <SimpleTooltip content={tooltip}>
      <button
        type="button"
        onClick={(e) => {
          e.stopPropagation();
          onClick();
        }}
        className={`inline-flex size-5 items-center justify-center rounded transition-colors hover:bg-accent ${
          on ? activeClass : "text-muted-foreground/60"
        }`}
      >
        {children}
      </button>
    </SimpleTooltip>
  );
}

// 右键菜单项(时间轴轨道 / 片段共用):danger = 破坏性操作红色
function CtxMenuItem({
  icon: Icon,
  label,
  onClick,
  disabled,
  danger,
}: {
  icon: typeof Clapperboard;
  label: string;
  onClick: () => void;
  disabled?: boolean;
  danger?: boolean;
}) {
  return (
    <button
      type="button"
      disabled={disabled}
      onClick={onClick}
      className={`flex w-full items-center gap-2 rounded px-2 py-1.5 text-left text-xs transition-colors hover:bg-accent ${
        disabled ? "pointer-events-none opacity-50" : ""
      } ${danger ? "text-destructive" : ""}`}
    >
      <Icon className="size-3.5 shrink-0" />
      {label}
    </button>
  );
}

function CtxSeparator() {
  return <div className="my-1 h-px bg-border" />;
}

// 多轨道时间轴(剪映式):左侧轨道头列(名称 + 锁定/可见/静音控制,点击激活,可拖拽排序),
// 右侧刻度尺(|mm:ss 主刻度 + 次级小刻度)+ 每条轨道一条通道;片段 = 头条(文件名 + 时长)+
// 帧体纹理(视频胶片分帧 / 音频拟波形);选区(激活轨上,两端手柄拖动 / 中部平移)+
// 贯通刻度尺与全轨道的白色播放头;刻度尺与通道空白处按住拖动 = scrub
function TimelineInner({ state, actions, refs }: { state: TimelineState; actions: TimelineActions; refs: TimelineRefs }) {
  const { tracks, activeTrackId, duration, selStart, selEnd, fileName, selectedClipIds, zoom, trackHeight, waveLevel, thumbUrls, thumbSourcePath, defaultInputPath, audioPeaks, externalAudioPeaks, markers, snapEnabled, fps, transitionKind, transitionSecs } = state;
  const { onSeek, onOpenClip, onSelection, onScrubChange, onSelectTrack, onRemoveTrack, onRemoveClip, onSelectClip, onMoveTrack, onToggleFlag, onZoomChange, onContextMenu: onCtx, onRenameTrack, onCommitClipMove, onCommitClipTrim, onRemoveMarker } = actions;
  // 纹理 / 片段配色随主题(system 时读系统偏好)
  const { theme } = useTheme();
  const isDark =
    theme === "dark" ||
    (theme === "system" && window.matchMedia("(prefers-color-scheme: dark)").matches);
  const firstVideoClipId = tracks
    .filter((track) => track.type === "video" && !track.hidden)
    .flatMap((track) => track.clips)
    .sort((a, b) => clipPos(a) - clipPos(b))[0]?.id;
  const displayedTransition = (clip: TrackClip): TransitionInput | undefined => {
    if (clip.id === firstVideoClipId) return undefined;
    if (clip.transitionIn) return clip.transitionIn.kind === "none" ? undefined : clip.transitionIn;
    return transitionKind === "none"
      ? undefined
      : { kind: transitionKind, durationSecs: transitionSecs };
  };
  const lanesRef = useRef<HTMLDivElement>(null);
  // 横向滚动容器(zoom>1 时通道区变宽出滚动条)
  const scrollRef = useRef<HTMLDivElement>(null);
  const trackHeadsRef = useRef<HTMLDivElement>(null);
  const [viewWidth, setViewWidth] = useState(0);
  const dragMode = useRef<"seek" | "start" | "end" | "move" | null>(null);
  // move 模式:按下点相对选区起点的偏移,平移时保持选区长度不变
  const moveAnchor = useRef(0);
  // 片段拖动(水平移动 position):ref 持有拖动中状态,state 仅驱动预览渲染;
  // 松手才经 onCommitClipMove 提交一次,撤销历史只有一步
  const [clipDrag, setClipDrag] = useState<{ trackId: string; clipId: string; pos: number } | null>(null);
  const clipDragRef = useRef<{
    trackId: string;
    clipId: string;
    startX: number;
    origPos: number;
    len: number;
    curPos: number;
    moved: boolean;
  } | null>(null);
  const [clipTrim, setClipTrim] = useState<ClipTrimCommit | null>(null);
  const clipTrimRef = useRef<(
    ClipTrimCommit & {
      edge: "start" | "end";
      startX: number;
      originalStart: number;
      originalEnd: number;
      originalPosition: number;
      sourceDuration: number;
      moved: boolean;
    }
  ) | null>(null);
  // 拖动结束后抑制紧随而来的 click(否则拖完会误触发选中+定位)
  const suppressClipClick = useRef(false);

  // 测量可视宽度(刻度密度随缩放 / 容器宽自适应)。依赖 duration:元数据就绪前
  // Timeline 早退不渲染、scrollRef 为 null,必须在时长就位后重新测量,否则刻度恒为兜底值
  useEffect(() => {
    const el = scrollRef.current;
    if (!el) return;
    const updateDimensions = () => {
      const viewport = el.clientWidth;
      refs.viewportWidth = viewport;
      refs.laneWidth = lanesRef.current?.clientWidth ?? viewport;
      setViewWidth((previous) => previous === viewport ? previous : viewport);
    };
    const ro = new ResizeObserver(updateDimensions);
    ro.observe(el);
    if (lanesRef.current) ro.observe(lanesRef.current);
    updateDimensions();
    return () => ro.disconnect();
  }, [duration]);

  // Ctrl/Cmd + 滚轮缩放(React 的 onWheel 是 passive,需原生监听才能 preventDefault)
  // 同样依赖 duration:时长就绪后 scrollRef 才存在
  useEffect(() => {
    const el = scrollRef.current;
    if (!el) return;
    const onWheel = (e: WheelEvent) => {
      if (!e.ctrlKey && !e.metaKey) return;
      e.preventDefault();
      onZoomChange(zoom * (e.deltaY < 0 ? 1.25 : 0.8));
    };
    el.addEventListener("wheel", onWheel, { passive: false });
    return () => el.removeEventListener("wheel", onWheel);
  }, [zoom, onZoomChange, duration]);

  // 播放头自动跟随 + 位置更新不在此处:current 不走 props(防每帧重渲染),
  // 由 VideoEditor 经 refs 直接写 DOM(见 TimelineRefs 注释)

  // 轨道头拖拽排序(HTML5 DnD;时间刻度尺固定在顶部,不参与排序)
  // 注意:钩子必须在上方早退(duration<=0 return null)之前声明,保证渲染间钩子数量一致
  const [dragIdx, setDragIdx] = useState<number | null>(null);
  const [dropAt, setDropAt] = useState<number | null>(null);
  // 播放头 scrub 中:抓手放大 + 出时间气泡(仅开始 / 结束两次渲染,拖动过程仍走 DOM 直写)
  const [scrubbing, setScrubbing] = useState(false);
  const pendingSeek = useRef<number | null>(null);
  const seekFrame = useRef(0);
  const previousLaneWidth = useRef(0);

  useEffect(() => () => {
    if (seekFrame.current) cancelAnimationFrame(seekFrame.current);
  }, []);

  // 通道宽度因缩放变化时,让播放头保持在视口内原来的位置,避免先跳走再被播放循环拉回。
  useLayoutEffect(() => {
    const lanes = lanesRef.current;
    const scroll = scrollRef.current;
    const playhead = refs.playhead;
    if (!lanes || !scroll || !playhead) return;
    const oldWidth = previousLaneWidth.current || refs.laneWidth || lanes.offsetWidth;
    const match = playhead.style.transform.match(/translate(?:X|3d)\(([-\d.]+)px/);
    const oldX = match ? Number(match[1]) : 0;
    const ratio = oldWidth > 0 ? Math.min(1, Math.max(0, oldX / oldWidth)) : 0;
    const viewportX = oldX - scroll.scrollLeft;
    const newWidth = lanes.offsetWidth;
    const newX = ratio * newWidth;
    refs.laneWidth = newWidth;
    refs.viewportWidth = scroll.clientWidth;
    previousLaneWidth.current = newWidth;
    playhead.style.transform = `translate3d(${newX.toFixed(3)}px,0,0)`;
    scroll.scrollLeft = Math.max(0, Math.min(newWidth - scroll.clientWidth, newX - viewportX));
  }, [zoom, duration, viewWidth, refs]);

  if (duration <= 0) return null;
  const pct = (t: number) => `${(t / duration) * 100}%`;
  const timeAt = (clientX: number) => {
    const el = lanesRef.current;
    if (!el) return 0;
    const r = el.getBoundingClientRect();
    const ratio = Math.min(1, Math.max(0, (clientX - r.left) / r.width));
    return ratio * duration;
  };

  function updateScrubVisual(t: number) {
    const lanes = lanesRef.current;
    const playhead = refs.playhead;
    if (!lanes || !playhead || duration <= 0) return;
    const rawX = (t / duration) * lanes.clientWidth;
    const x = rawX;
    playhead.style.transform = `translate3d(${x.toFixed(3)}px,0,0)`;
    const bubble = playhead.querySelector<HTMLElement>("[data-ph-time]");
    if (bubble) bubble.textContent = fmt(t);
    const scroll = scrollRef.current;
    if (scroll && (x < scroll.scrollLeft + 24 || x > scroll.scrollLeft + scroll.clientWidth - 24)) {
      scroll.scrollLeft = Math.max(0, x - scroll.clientWidth / 2);
    }
  }

  // 高频 pointermove 合并到每帧一次:播放头立即直写 DOM,视频 seek 与 React 状态最多 60Hz。
  function queueSeek(t: number) {
    pendingSeek.current = t;
    updateScrubVisual(t);
    if (seekFrame.current) return;
    seekFrame.current = requestAnimationFrame(() => {
      seekFrame.current = 0;
      const target = pendingSeek.current;
      pendingSeek.current = null;
      if (target !== null) onSeek(target);
    });
  }

  function flushSeek() {
    if (seekFrame.current) cancelAnimationFrame(seekFrame.current);
    seekFrame.current = 0;
    const target = pendingSeek.current;
    pendingSeek.current = null;
    if (target !== null) onSeek(target);
  }

  function beginDrag(mode: NonNullable<typeof dragMode.current>, e: React.PointerEvent) {
    if (e.button !== 0) return;
    e.preventDefault();
    e.stopPropagation();
    // 指针捕获挂到通道区:手柄按下后的 move/up 也路由到统一处理器
    lanesRef.current?.setPointerCapture(e.pointerId);
    dragMode.current = mode;
    if (mode === "seek") {
      setScrubbing(true);
      onScrubChange?.(true);
      queueSeek(timeAt(e.clientX));
    }
    if (mode === "move") moveAnchor.current = timeAt(e.clientX) - selStart;
  }

  function endDrag() {
    if (dragMode.current === "seek") {
      flushSeek();
      setScrubbing(false);
      onScrubChange?.(false);
    }
    // 片段拖动收尾:拖动过才提交落点(一次撤销步),点击(未拖动)交给 onClick
    const d = clipDragRef.current;
    if (d) {
      if (d.moved) {
        suppressClipClick.current = true;
        onCommitClipMove(d.trackId, d.clipId, d.curPos);
      }
      clipDragRef.current = null;
      setClipDrag(null);
    }
    const trim = clipTrimRef.current;
    if (trim) {
      if (trim.moved) {
        suppressClipClick.current = true;
        onCommitClipTrim({
          trackId: trim.trackId,
          clipId: trim.clipId,
          start: trim.start,
          end: trim.end,
          position: trim.position,
        });
      }
      clipTrimRef.current = null;
      setClipTrim(null);
    }
    dragMode.current = null;
  }

  // 帧网格负责精确落点，磁性候选覆盖播放头、标记、选区和所有轨道片段边缘。
  function snappedPosition(value: number, movingLength: number, excludeClipId?: string) {
    if (!snapEnabled || pxPerSec <= 0) return value;
    const frame = 1 / Math.max(1, fps || 30);
    const frameAligned = Math.round(value / frame) * frame;
    const candidates = [0, duration, refs.playheadTime, selStart, selEnd, ...markers.map((marker) => marker.time)];
    for (const track of tracks) {
      for (const clip of track.clips) {
        if (clip.id === excludeClipId) continue;
        const left = clipPos(clip);
        const right = left + clipLen(clip);
        candidates.push(left, right, left - movingLength, right - movingLength);
      }
    }
    const threshold = 10 / pxPerSec;
    let best: number | undefined;
    let distance = threshold;
    for (const candidate of candidates) {
      const nextDistance = Math.abs(value - candidate);
      if (nextDistance <= distance) {
        best = candidate;
        distance = nextDistance;
      }
    }
    return Math.max(0, Math.min(Math.max(0, duration - movingLength), best ?? frameAligned));
  }

  function onPointerMove(e: React.PointerEvent) {
    const trim = clipTrimRef.current;
    if (trim) {
      if (pxPerSec <= 0) return;
      const dt = (e.clientX - trim.startX) / pxPerSec;
      if (!trim.moved && Math.abs(e.clientX - trim.startX) < 3) return;
      trim.moved = true;
      const speed = clipSpeed(tracks.flatMap((track) => track.clips).find((clip) => clip.id === trim.clipId) ?? { start: 0, end: 0, id: "" });
      if (trim.edge === "start") {
        trim.start = Math.min(
          trim.originalEnd - 0.1,
          Math.max(0, trim.originalStart - trim.originalPosition * speed, trim.originalStart + dt * speed),
        );
        trim.position = trim.originalPosition + (trim.start - trim.originalStart) / speed;
        const snapped = snappedPosition(trim.position, 0, trim.clipId);
        trim.start = Math.min(trim.originalEnd - 0.1, trim.originalStart + (snapped - trim.originalPosition) * speed);
        trim.position = trim.originalPosition + (trim.start - trim.originalStart) / speed;
      } else {
        trim.end = Math.max(
          trim.originalStart + 0.1,
          Math.min(trim.sourceDuration, trim.originalEnd + dt * speed),
        );
        const right = snappedPosition(
          trim.originalPosition + (trim.end - trim.originalStart) / speed,
          0,
          trim.clipId,
        );
        trim.end = Math.max(trim.originalStart + 0.1, trim.originalStart + (right - trim.originalPosition) * speed);
      }
      setClipTrim({
        trackId: trim.trackId,
        clipId: trim.clipId,
        start: trim.start,
        end: trim.end,
        position: trim.position,
      });
      return;
    }
    // 片段拖动优先(指针捕获在通道区,move 事件路由到这里)
    const d = clipDragRef.current;
    if (d) {
      if (pxPerSec <= 0) return;
      const dx = e.clientX - d.startX;
      if (!d.moved && Math.abs(dx) < 4) return;
      d.moved = true;
      const dt = dx / pxPerSec;
      const pos = snappedPosition(d.origPos + dt, d.len, d.clipId);
      d.curPos = pos;
      setClipDrag({ trackId: d.trackId, clipId: d.clipId, pos });
      return;
    }
    const mode = dragMode.current;
    if (!mode) return;
    const t = timeAt(e.clientX);
    if (mode === "seek") queueSeek(t);
    else if (mode === "start") onSelection(Math.min(t, selEnd - 0.1), selEnd);
    else if (mode === "end") onSelection(selStart, Math.max(t, selStart + 0.1));
    else {
      const len = selEnd - selStart;
      const s = Math.min(Math.max(0, t - moveAnchor.current), duration - len);
      onSelection(s, s + len);
    }
  }

  // 片段按下:记录起点等拖动就绪状态;移动 ≥4px 才算拖动(不影响点击选中/定位)
  function beginClipDrag(e: React.PointerEvent, track: EditorTrack, c: TrackClip) {
    if (e.button !== 0 || track.locked || pxPerSec <= 0) return;
    e.stopPropagation();
    lanesRef.current?.setPointerCapture(e.pointerId);
    clipDragRef.current = {
      trackId: track.id,
      clipId: c.id,
      startX: e.clientX,
      origPos: clipPos(c),
      len: clipLen(c),
      curPos: clipPos(c),
      moved: false,
    };
  }

  function beginClipTrim(
    e: React.PointerEvent,
    track: EditorTrack,
    clip: TrackClip,
    edge: "start" | "end",
  ) {
    if (e.button !== 0 || track.locked || pxPerSec <= 0) return;
    if ((clip.speedCurve?.length ?? 0) > 0) {
      toast.error("速度曲线片段请先清除曲线，再拖动源片裁边");
      return;
    }
    e.preventDefault();
    e.stopPropagation();
    lanesRef.current?.setPointerCapture(e.pointerId);
    clipTrimRef.current = {
      trackId: track.id,
      clipId: clip.id,
      edge,
      startX: e.clientX,
      originalStart: clip.start,
      originalEnd: clip.end,
      originalPosition: clipPos(clip),
      sourceDuration: clip.sourceDuration ?? duration,
      moved: false,
      start: clip.start,
      end: clip.end,
      position: clipPos(clip),
    };
  }

  // 刻度密度随缩放自适应:保证相邻刻度 ≥50px,素材长 / 缩得小则逐级放疏
  const pxPerSec = viewWidth > 0 ? (viewWidth * zoom) / duration : 0;
  // 视频轨更紧凑以减少胶片条占高；独立音频轨增高，方便观察波形和精确下刀。
  const trackPixelHeight = (type: TrackType) => type === "video"
    ? Math.max(48, Math.round(trackHeight * 0.72))
    : type === "audio"
      ? Math.max(72, Math.round(trackHeight * 1.2))
      : Math.max(44, Math.round(trackHeight * 0.68));
  const TICK_CANDIDATES = [0.5, 1, 2, 5, 10, 15, 30, 60, 120, 300];
  let tickEvery = TICK_CANDIDATES.find((c) => pxPerSec * c >= 50) ?? 300;
  while (duration / tickEvery > 400) tickEvery *= 2; // 超长线素材防刻度爆炸
  const ticks: number[] = [];
  for (let t = 0; t <= duration; t += tickEvery) ticks.push(t);
  const draggedAnchor = clipDrag
    ? tracks.flatMap((track) => track.clips).find((clip) => clip.id === clipDrag.clipId)
    : undefined;
  const dragPreviewPosition = (clip: TrackClip) => {
    if (!clipDrag || !draggedAnchor) return clipPos(clip);
    const grouped = draggedAnchor.groupId
      ? tracks.flatMap((track) => track.clips).filter((item) => item.groupId === draggedAnchor.groupId)
      : [draggedAnchor];
    const delta = Math.max(
      clipDrag.pos - clipPos(draggedAnchor),
      -Math.min(...grouped.map(clipPos)),
    );
    if (clip.id === draggedAnchor.id) return clipPos(clip) + delta;
    if (clip.groupId && clip.groupId === draggedAnchor.groupId) {
      return clipPos(clip) + delta;
    }
    return clipPos(clip);
  };
  const isDraggedGroupClip = (clip: TrackClip) => !!clipDrag && (
    clip.id === clipDrag.clipId || !!(clip.groupId && clip.groupId === draggedAnchor?.groupId)
  );

  return (
    // 撑满父容器全高:右侧滚动容器的横向滚动条才能落在时间轴区最底部
    <div className="flex h-full select-none">
      {/* 左:轨道头列(名称 + 锁定/可见/静音控制,点击激活,可拖拽排序;与右侧通道行高对齐) */}
      <div className="flex w-32 shrink-0 flex-col overflow-hidden border-r border-border bg-muted/30">
        <div className="h-6 bg-muted/20" />
        <div
          ref={trackHeadsRef}
          className="min-h-0 flex-1 overflow-hidden"
          onWheel={(e) => {
            if (scrollRef.current && Math.abs(e.deltaY) > 0) {
              scrollRef.current.scrollTop += e.deltaY;
            }
          }}
        >
        {tracks.map((t, idx) => {
          const meta = TRACK_META[t.type];
          const active = t.id === activeTrackId;
          return (
            <div
              key={t.id}
              role="button"
              tabIndex={0}
              draggable={!t.locked}
              onDragStart={(e) => {
                setDragIdx(idx);
                e.dataTransfer.effectAllowed = "move";
              }}
              onDragEnd={() => {
                setDragIdx(null);
                setDropAt(null);
              }}
              onDragOver={(e) => {
                e.preventDefault();
                e.dataTransfer.dropEffect = "move";
                setDropAt(idx);
              }}
              onDragLeave={() => setDropAt((p) => (p === idx ? null : p))}
              onDrop={(e) => {
                e.preventDefault();
                if (dragIdx !== null && dragIdx !== idx) onMoveTrack(dragIdx, idx);
                setDragIdx(null);
                setDropAt(null);
              }}
              onClick={() => onSelectTrack(t.id)}
              onDoubleClick={() => onRenameTrack(t.id)}
              onKeyDown={(e) => e.key === "Enter" && onSelectTrack(t.id)}
              onContextMenu={(e) => {
                e.preventDefault();
                e.stopPropagation();
                onSelectTrack(t.id);
                onCtx({ x: e.clientX, y: e.clientY, kind: "track", trackId: t.id });
              }}
              style={{ height: trackPixelHeight(t.type) }}
              className={`group relative flex cursor-grab flex-col justify-center gap-1 border-b border-border/60 px-2 active:cursor-grabbing ${
                active ? "bg-accent/50" : ""
              } ${t.hidden ? "opacity-50" : ""} ${dragIdx === idx ? "opacity-40" : ""} ${
                dropAt === idx && dragIdx !== null && dragIdx !== idx
                  ? "bg-primary/10 shadow-[inset_0_2px_0_0] shadow-primary"
                  : ""
              }`}
            >
              {/* 第一行:类型图标 + 轨道名(重命名后此处可见;双击亦可重命名) */}
              <div className="flex min-w-0 items-center gap-1.5">
                <meta.icon
                  className={`size-3.5 shrink-0 ${active ? "text-foreground" : "text-muted-foreground"}`}
                />
                <span
                  className="min-w-0 flex-1 truncate text-[11px] leading-tight"
                  title={`${t.name}(双击重命名)`}
                >
                  {t.name}
                </span>
              </div>
              {/* 第二行:锁定 / 可见 / 静音(音频轨)或关闭原声(视频轨) */}
              <div className="flex items-center gap-0.5">
                <TrackFlagButton
                  on={!t.locked}
                  activeClass="text-amber-500"
                  tooltip={t.locked ? "解锁轨道" : "锁定轨道"}
                  onClick={() => onToggleFlag(t.id, "locked")}
                >
                  {t.locked ? <Lock className="size-3" /> : <LockOpen className="size-3" />}
                </TrackFlagButton>
                <TrackFlagButton
                  on={!t.hidden}
                  activeClass="text-foreground"
                  tooltip={t.hidden ? "显示轨道" : "隐藏轨道"}
                  onClick={() => onToggleFlag(t.id, "hidden")}
                >
                  {t.hidden ? <EyeOff className="size-3" /> : <Eye className="size-3" />}
                </TrackFlagButton>
                {(t.type === "audio" || t.type === "video") && (
                  <TrackFlagButton
                    on={!t.muted}
                    activeClass="text-foreground"
                    tooltip={
                      t.muted
                        ? t.type === "audio"
                          ? "取消静音"
                          : "恢复原声"
                        : t.type === "audio"
                          ? "静音"
                          : "关闭原声"
                    }
                    onClick={() => onToggleFlag(t.id, "muted")}
                  >
                    {t.muted ? <VolumeX className="size-3" /> : <Volume2 className="size-3" />}
                  </TrackFlagButton>
                )}
                {tracks.length > 1 && !t.locked && (
                  <button
                    type="button"
                    onClick={(e) => {
                      e.stopPropagation();
                      onRemoveTrack(t.id);
                    }}
                    className="ml-auto inline-flex size-4 shrink-0 items-center justify-center rounded-full text-muted-foreground opacity-0 transition-all hover:bg-accent hover:text-destructive group-hover:opacity-100"
                  >
                    <Trash2 className="size-2.5" />
                  </button>
                )}
              </div>
            </div>
          );
        })}
          {/* 与右侧刻度尺占位保持一致,使两侧纵向最大滚动距离完全相同。 */}
          <div className="h-6" />
        </div>
      </div>

      {/* 右:刻度尺 + 通道(横向滚动容器;指针事件挂内层,捕获后拖出区域仍能收到 move/up) */}
      {/* y-auto:轨道高度可调,总高超出可视区时竖向滚动(对齐剪映);横向滚动条落在时间轴区最底部 */}
      <div
        ref={(el) => {
          scrollRef.current = el;
          refs.scroll = el;
          if (el) refs.viewportWidth = el.clientWidth;
        }}
        onScroll={(e) => {
          if (trackHeadsRef.current) trackHeadsRef.current.scrollTop = e.currentTarget.scrollTop;
        }}
        className="veltrix-editor-scrollbar min-w-0 flex-1 overflow-x-auto overflow-y-auto"
      >
        <div
          ref={(el) => {
            lanesRef.current = el;
            refs.lanes = el;
            if (el) refs.laneWidth = el.clientWidth;
          }}
          className="relative flex min-h-full flex-col"
          style={{ width: zoom > 1 ? `${zoom * 100}%` : "100%" }}
          onPointerDown={(e) => beginDrag("seek", e)}
          onPointerMove={onPointerMove}
          onPointerUp={endDrag}
          onPointerCancel={endDrag}
          onLostPointerCapture={endDrag}
        >
        {/* 刻度尺(剪映式):主刻度「竖线 + 时间码」与次级小刻度均顶对齐,无底部分隔线,
            与轨道区融为一体 */}
        <div className="sticky top-0 z-30 h-6 bg-muted/95 backdrop-blur-sm">
          {ticks.map((t) => (
            <span key={t} className="absolute top-0 flex items-start gap-1" style={{ left: pct(t) }}>
              <span className="h-2.5 w-px bg-muted-foreground/50" />
              <span className="text-[9px] leading-none tabular-nums text-muted-foreground/80">
                {fmtTick(t)}
              </span>
            </span>
          ))}
          {pxPerSec * (tickEvery / 5) >= 6 &&
            ticks.slice(0, -1).flatMap((t) =>
              [1, 2, 3, 4].map((k) => {
                const mt = t + (tickEvery / 5) * k;
                if (mt > duration) return null;
                return (
                  <span
                    key={mt}
                    className="absolute top-0 h-1.5 w-px bg-border"
                    style={{ left: pct(mt) }}
                  />
                );
              }),
            )}
          {markers.filter((marker) => marker.time <= duration).map((marker) => (
            <button
              key={marker.id}
              type="button"
              title={`${marker.label} · ${fmt(marker.time)}（Shift+点击删除）`}
              className="absolute top-0 z-20 -translate-x-1/2 text-amber-400 drop-shadow"
              style={{ left: pct(marker.time) }}
              onPointerDown={(event) => event.stopPropagation()}
              onClick={(event) => {
                event.stopPropagation();
                if (event.shiftKey) onRemoveMarker(marker.id);
                else onSeek(marker.time);
              }}
            >
              <Flag className="size-3 fill-current" />
            </button>
          ))}
        </div>
        {/* 通道区:flex-1 撑满可视高度,轨道不足一屏时播放头也能到底 */}
        <div className="relative flex-1">
          {tracks.map((t) => {
            const meta = TRACK_META[t.type];
            const active = t.id === activeTrackId;
            return (
              <div
                key={t.id}
                style={{ height: trackPixelHeight(t.type) }}
                className={`relative cursor-grab overflow-hidden border-b border-border active:cursor-grabbing ${
                  active ? "bg-muted/40" : "bg-muted/20"
                } ${t.hidden ? "opacity-45" : ""}`}
                onContextMenu={(e) => {
                  // 空白通道右键(片段自己的 handler 会 stopPropagation)
                  e.preventDefault();
                  onCtx({ x: e.clientX, y: e.clientY, kind: "lane", trackId: t.id });
                }}
              >
                {t.clips.length === 0 && (
                  <span className="pointer-events-none absolute inset-0 flex items-center px-3 text-[10px] text-muted-foreground/55">
                    {t.type === "text"
                      ? "空字幕轨 · 从左侧“文本/字幕”添加"
                      : "空轨道 · 选择源片区间后点击“+”添加"}
                  </span>
                )}
                {/* 该轨已有片段(点击选中并定位播放头,Ctrl+点击多选;按住水平拖动改 position,
                    磁吸对齐相邻片段;头条 hover 出删除,锁定轨不可拖) */}
                {t.clips.map((c) => (
                  <div
                    key={c.id}
                    role="button"
                    tabIndex={0}
                    onPointerDown={(e) => beginClipDrag(e, t, c)}
                    onClick={(e) => {
                      if (suppressClipClick.current) {
                        suppressClipClick.current = false;
                        return;
                      }
                      if (t.type === "text") onSeek(clipPos(c));
                      else onOpenClip(c);
                      onSelectClip(c.id, e.ctrlKey || e.metaKey);
                    }}
                    onContextMenu={(e) => {
                      // 右键 = 选中该片段(未选中时)并弹菜单(对齐专业 NLE 行为)
                      e.preventDefault();
                      e.stopPropagation();
                      if (!selectedClipIds.includes(c.id)) onSelectClip(c.id, false);
                      onCtx({ x: e.clientX, y: e.clientY, kind: "clip", trackId: t.id, clipId: c.id });
                    }}
                    onKeyDown={(e) => {
                      if (e.key !== "Enter") return;
                      if (t.type === "text") onSeek(clipPos(c));
                      else onOpenClip(c);
                    }}
                    className={`group absolute inset-y-1 z-10 flex cursor-pointer flex-col overflow-hidden rounded-[4px] border ${meta.frameClass} ${
                      selectedClipIds.includes(c.id) ? "ring-2 ring-primary" : ""
                    } ${isDraggedGroupClip(c) ? "z-20 opacity-80 shadow-lg" : ""}`}
                    style={{
                      left: pct(
                        clipTrim?.clipId === c.id
                          ? clipTrim.position
                          : dragPreviewPosition(c),
                      ),
                      width: pct(
                        clipTrim?.clipId === c.id
                          ? (clipTrim.end - clipTrim.start) / clipSpeed(c)
                          : clipLen(c),
                      ),
                    }}
                  >
                    {selectedClipIds.includes(c.id) && !t.locked && (
                      <>
                        <button
                          type="button"
                          aria-label="拖动片段入点"
                          className="absolute inset-y-0 left-0 z-30 w-1.5 cursor-ew-resize bg-primary/80 opacity-0 transition-opacity group-hover:opacity-100"
                          onPointerDown={(e) => beginClipTrim(e, t, c, "start")}
                        />
                        <button
                          type="button"
                          aria-label="拖动片段出点"
                          className="absolute inset-y-0 right-0 z-30 w-1.5 cursor-ew-resize bg-primary/80 opacity-0 transition-opacity group-hover:opacity-100"
                          onPointerDown={(e) => beginClipTrim(e, t, c, "end")}
                        />
                      </>
                    )}
                    {/* 头条:文件名 + 片段时长 */}
                    <div className={`flex h-4 shrink-0 items-center gap-1 px-1.5 ${meta.headerClass}`}>
                      <span className="truncate text-[9px] text-white/90">
                        {c.text ?? (c.inputPath ? baseName(c.inputPath) : fileName)}
                      </span>
                      <span className="ml-auto shrink-0 font-mono text-[9px] tabular-nums text-white/75">
                        {fmt(clipLen(c))}
                      </span>
                      {(c.speed ?? 1) !== 1 && <span className="shrink-0 font-mono text-[8px] text-white/80">{c.speed}×</span>}
                      {c.groupId && <Link2 className="size-2.5 shrink-0 text-white/80" />}
                      {!t.locked && (
                        <button
                          type="button"
                          onClick={(e) => {
                            e.stopPropagation();
                            onRemoveClip(t.id, c.id);
                          }}
                          className="inline-flex size-3.5 shrink-0 items-center justify-center rounded-full text-white/80 opacity-0 transition-all hover:text-destructive-foreground group-hover:opacity-100"
                        >
                          <Trash2 className="size-2.5" />
                        </button>
                      )}
                    </div>
                    {/* 帧体:视频 = 胶片条 + 内嵌音频波形(剪映式);音频 = 真实波形(均未就绪时回退 CSS 纹理) */}
                    {t.type === "video" ? (
                      <div className="flex min-h-0 flex-1 flex-col">
                        {(c.inputPath ?? defaultInputPath) === thumbSourcePath && thumbUrls.length > 0 ? (
                          <Filmstrip
                            thumbs={thumbUrls}
                            start={c.start}
                            end={c.end}
                            duration={c.sourceDuration ?? duration}
                            pxPerSec={pxPerSec}
                          />
                        ) : (
                          <div
                            className="min-h-0 flex-1"
                            style={clipTexture(t.type, waveLevel, isDark)}
                          />
                        )}
                        {/* 源视频带声音时,在胶片条下方压一条矮波形,方便对着人声下刀;
                            高度按片段体 38% 自适应(随轨道高度增减,最小 12px) */}
                        {!t.muted
                          && !c.muteOriginal
                          && (c.inputPath ?? defaultInputPath) === thumbSourcePath
                          && audioPeaks && (
                          <div className="h-[38%] min-h-3 shrink-0">
                            <WaveformBody
                              peaks={audioPeaks}
                              isDark={isDark}
                              level={2}
                              start={c.start}
                              end={c.end}
                              duration={c.sourceDuration ?? duration}
                            />
                          </div>
                        )}
                      </div>
                    ) : t.type === "text" ? (
                      <div className="flex min-h-0 flex-1 items-center justify-center bg-amber-100/80 px-2 text-center text-[11px] font-medium text-amber-950 dark:bg-amber-950/70 dark:text-amber-100">
                        <span className="line-clamp-2">{c.text || "空字幕"}</span>
                      </div>
                    ) : t.type === "audio" && (c.inputPath ? externalAudioPeaks.get(c.inputPath) : audioPeaks) ? (
                      <div className="min-h-0 flex-1">
                        <WaveformBody
                          peaks={(c.inputPath ? externalAudioPeaks.get(c.inputPath) : audioPeaks)!}
                          isDark={isDark}
                          level={waveLevel}
                          start={c.start}
                          end={c.end}
                          duration={c.sourceDuration ?? duration}
                        />
                      </div>
                    ) : (
                      <div
                        className="min-h-0 flex-1"
                        style={clipTexture(t.type, waveLevel, isDark)}
                      />
                    )}
                    {t.type === "audio" && (
                      <>
                        {(c.fadeIn ?? 0) > 0 && (
                          <span
                            className="pointer-events-none absolute inset-y-4 left-0 z-10 border-t border-primary/80 bg-gradient-to-br from-background/80 to-transparent"
                            style={{ width: `${Math.min(100, (c.fadeIn ?? 0) / Math.max(0.001, clipLen(c)) * 100)}%`, clipPath: "polygon(0 0, 100% 100%, 0 100%)" }}
                          />
                        )}
                        {(c.fadeOut ?? 0) > 0 && (
                          <span
                            className="pointer-events-none absolute inset-y-4 right-0 z-10 border-t border-primary/80 bg-gradient-to-bl from-background/80 to-transparent"
                            style={{ width: `${Math.min(100, (c.fadeOut ?? 0) / Math.max(0.001, clipLen(c)) * 100)}%`, clipPath: "polygon(100% 0, 100% 100%, 0 100%)" }}
                          />
                        )}
                        {(c.volume ?? 1) !== 1 && (
                          <span className="pointer-events-none absolute bottom-1 right-1 z-20 rounded bg-black/55 px-1 font-mono text-[8px] text-white">{Math.round((c.volume ?? 1) * 100)}%</span>
                        )}
                      </>
                    )}
                    {(c.speedCurve?.length ?? 0) > 0 && (
                      <svg className="pointer-events-none absolute inset-x-0 bottom-0 z-20 h-5 w-full overflow-visible" viewBox="0 0 100 20" preserveAspectRatio="none">
                        <polyline
                          fill="none"
                          stroke="rgba(250,204,21,0.95)"
                          strokeWidth="1.5"
                          vectorEffect="non-scaling-stroke"
                          points={speedCurvePoints(c).map((point) => {
                            const x = sourceOffsetToTimeline(c, point.offset) / Math.max(0.001, clipLen(c)) * 100;
                            const y = 18 - (point.speed - 0.25) / 3.75 * 16;
                            return `${x.toFixed(2)},${y.toFixed(2)}`;
                          }).join(" ")}
                        />
                      </svg>
                    )}
                    {displayedTransition(c) && (
                      <span
                        className="pointer-events-none absolute inset-y-4 left-0 z-20 flex items-center justify-center overflow-hidden border-r border-violet-300/80 bg-gradient-to-r from-violet-500/55 to-violet-500/5 text-[8px] font-medium text-white"
                        style={{ width: `${Math.min(50, displayedTransition(c)!.durationSecs / Math.max(0.001, clipLen(c)) * 100)}%` }}
                      >{displayedTransition(c)!.kind === "fade" ? "淡黑" : "叠化"}</span>
                    )}
                    {t.type === "video" && (c.keyframes ?? []).map((frame) => (
                      <span
                        key={frame.id}
                        title={`关键帧 ${fmt(frame.offset)}`}
                        className="pointer-events-none absolute bottom-1 z-20 size-2 -translate-x-1/2 rotate-45 border border-white/90 bg-primary shadow-sm"
                        style={{ left: `${Math.max(0, Math.min(100, frame.offset / Math.max(0.001, clipLen(c)) * 100))}%` }}
                      />
                    ))}
                  </div>
                ))}
                {/* 选区只画在激活轨上(拖动中部平移) */}
                {active && (
                  <>
                    <div
                      className="absolute inset-y-0 cursor-grab border-x-2 border-primary bg-primary/15 active:cursor-grabbing"
                      style={{ left: pct(selStart), width: pct(selEnd - selStart) }}
                      onPointerDown={(e) => beginDrag("move", e)}
                    />
                    {/* 入点 / 出点手柄 */}
                    <div
                      className="absolute inset-y-0 z-20 w-2 -translate-x-1/2 cursor-ew-resize rounded-sm bg-primary"
                      style={{ left: pct(selStart) }}
                      onPointerDown={(e) => beginDrag("start", e)}
                    />
                    <div
                      className="absolute inset-y-0 z-20 w-2 -translate-x-1/2 cursor-ew-resize rounded-sm bg-primary"
                      style={{ left: pct(selEnd) }}
                      onPointerDown={(e) => beginDrag("end", e)}
                    />
                  </>
                )}
              </div>
            );
          })}
        </div>
        {/* 播放头(剪映式):掏空圆角矩形抓手 + 从其底边垂下的贯通竖线(flex 同列,天然相连)。
            颜色走 foreground 跟随主题(深色主题=白,浅色=黑);hover 时抓手放大、竖线提亮,
            scrub 时旁侧跟随时间气泡(文本由 VideoEditor 直写,不走渲染)。
            移动走 translate3d(非 left),由合成层执行子像素位移;缩到十分钟全览时每帧
            仅移动约 0.05px,不能取整,否则会退化成每 0.3 秒跳 1px。
            位置不走 React 渲染:挂载时按 refs.playheadPct 换算初始 px,之后直写 transform。 */}
        <div
          ref={(el) => {
            refs.playhead = el;
            if (el) {
              const ratio = parseFloat(refs.playheadPct) / 100;
              const w = el.parentElement?.offsetWidth ?? 0;
              el.style.transform = `translate3d(${(ratio * w).toFixed(3)}px,0,0)`;
            }
          }}
          className="absolute inset-y-0 z-30 w-0 will-change-transform"
        >
          <div
            className="group flex h-full w-3 -translate-x-1/2 cursor-ew-resize touch-none flex-col items-center"
            onPointerDown={(e) => beginDrag("seek", e)}
          >
            {/* 抓手:掏空圆角矩形(描边透出刻度);暗黑下用纯白 */}
            <div className="mt-1 flex h-4 shrink-0 items-start justify-center">
              <div className="h-4 w-3 rounded-[4px] border-2 border-foreground bg-transparent transition-all group-hover:h-[18px] group-hover:w-3.5 dark:border-white" />
            </div>
            {/* 贯通竖线:从矩形底边出发直插底;恒定 1.5px,scrub 发光;
                颜色跟随主题(深色=纯白) */}
            <div
              className={`flex-1 bg-foreground/80 transition-colors group-hover:bg-foreground dark:bg-white/80 dark:group-hover:bg-white ${
                scrubbing
                  ? "w-[1.5px] bg-foreground dark:bg-white shadow-[0_0_4px_rgba(0,0,0,0.35)] dark:shadow-[0_0_4px_rgba(255,255,255,0.6)]"
                  : "w-[1.5px]"
              }`}
            />
            {/* 时间气泡:仅 scrub 时显示,跟随播放头 */}
            {scrubbing && (
              <span
                data-ph-time
                className="absolute left-4 top-2 whitespace-nowrap rounded bg-foreground px-1 py-0.5 text-[9px] leading-none tabular-nums text-background shadow dark:bg-white dark:text-black"
              />
            )}
          </div>
        </div>
        </div>
      </div>
    </div>
  );
}

// memo 化:props(state / actions / refs)在 VideoEditor 侧已用 useMemo / ref 稳定化,
// 播放 / scrub 期间 current 高频变化不再触发整棵时间轴树重渲染
const Timeline = memo(TimelineInner);

// 编辑器:输入为已选视频路径(+ 可选草稿);onBack 返回首页,onOpen 换片(容器用 key 重挂载重置状态)
function VideoEditor({
  inputPath,
  draft,
  onBack,
  onOpen,
}: {
  inputPath: string;
  draft?: VideoDraft;
  onBack: () => void;
  onOpen: (path: string) => void;
}) {
  const mediaFileUrl = useMediaFileUrl();
  const videoRef = useRef<HTMLVideoElement>(null);
  const previewCanvasRef = useRef<HTMLDivElement>(null);
  const layerVideoRefs = useRef<Map<string, HTMLVideoElement>>(new Map());
  const [previewPath, setPreviewPath] = useState(inputPath);
  const previewPathRef = useRef(previewPath);
  previewPathRef.current = previewPath;
  const [proxyBySource, setProxyBySource] = useState<Record<string, string>>({});
  const [preparingProxyPath, setPreparingProxyPath] = useState<string | null>(null);
  const proxyRequested = useRef(new Set<string>());
  const activePreviewMediaPath = proxyBySource[previewPath] ?? previewPath;
  const pendingMediaSeek = useRef<{
    time: number;
    play: boolean;
    preserveSelection?: boolean;
  } | null>(null);
  const timelineClipIdRef = useRef<string | null>(null);
  // 时间轴空白区没有对应的媒体源,单独保存序列坐标;不能继续沿用上一个视频的 currentTime。
  const [timelineGapTime, setTimelineGapTimeState] = useState<number | null>(null);
  const timelineGapTimeRef = useRef<number | null>(null);
  const [duration, setDuration] = useState(0);
  const [current, setCurrent] = useState(0);
  const [playing, setPlaying] = useState(false);
  // 播放倍速(应用到 <video>.playbackRate,换片不重置)
  const [rate, setRate] = useState(draft?.settings?.playbackRate ?? 1);
  const [selStart, setSelStart] = useState(0);
  const [selEnd, setSelEnd] = useState(0);
  // 多轨道:默认一条视频轨;草稿恢复时带上保存的轨道(旧草稿片段补 position=源起点)
  const [tracks, setTracks] = useState<EditorTrack[]>(() => {
    const base = draft?.tracks ?? [newTrack("video", [])];
    return base.map((t) => ({
      ...t,
      clips: t.clips.map((c) => ({ ...c, position: c.position ?? c.start })),
    }));
  });
  // 只有草稿内确实存在片段才视为已初始化。元数据返回前自动保存出来的空草稿
  // 仍应自动补入主视频，否则重新打开后会永久停留在空轨道。
  const initialClipSeeded = useRef(
    draft?.tracks.some((track) => track.clips.length > 0) ?? false,
  );
  const [mediaInitState, setMediaInitState] = useState<"loading" | "ready" | "error">(
    initialClipSeeded.current ? "ready" : "loading",
  );
  const [showMediaInitProgress, setShowMediaInitProgress] = useState(false);
  const [activeTrackId, setActiveTrackId] = useState(
    () => (draft?.tracks ?? [])[0]?.id ?? "",
  );
  // 时间轴缩放(1 = 适配宽度,上限 20x)
  const [zoom, setZoom] = useState(1);
  const changeZoom = (z: number) => setZoom(Math.min(20, Math.max(1, z)));
  // 片段选择:点击片段选中(Ctrl+点击多选),工具栏/快捷键做分割、左右全选、删除
  const [selectedClipIds, setSelectedClipIds] = useState<string[]>([]);
  // 链接选择默认开启：分离出的音频与原视频仍作为一组选择，可随时关闭后单独精修。
  const [linkedSelection, setLinkedSelection] = useState(true);
  const [snapEnabled, setSnapEnabled] = useState(true);
  const [editMode, setEditMode] = useState<"append" | "insert" | "overwrite">(
    draft?.settings?.editMode ?? "append",
  );
  const [markers, setMarkers] = useState<TimelineMarker[]>(draft?.markers ?? []);
  const [loopRange, setLoopRange] = useState<{ start: number; end: number } | null>(null);
  const loopRangeRef = useRef(loopRange);
  loopRangeRef.current = loopRange;
  // 撤销/重做:tracks 快照历史(上限 50 步);histCounts 仅驱动按钮禁用态渲染
  const history = useRef<{ past: EditorTrack[][]; future: EditorTrack[][] }>({
    past: [],
    future: [],
  });
  const [histCounts, setHistCounts] = useState({ past: 0, future: 0 });
  // 时间轴显示设置(⋮ 菜单):轨道高度(px)/ 音频波形占比档位(0 小 / 1 中 / 2 大)
  // 轨道高度(px,⋮ 菜单滑块可调):统一 90,胶片条 16:9 tile 宽约 160px,细节清晰
  const [trackHeight, setTrackHeight] = useState(draft?.settings?.trackHeight ?? 90);
  const [waveLevel, setWaveLevel] = useState(draft?.settings?.waveLevel ?? 1);
  // 导出:转场(kind=none 不加,流拷贝快路径)/ 转场时长 / 导出中状态
  const [transitionKind, setTransitionKind] = useState<"none" | "dissolve" | "fade">(
    draft?.settings?.transitionKind ?? "none",
  );
  const [transitionSecs, setTransitionSecs] = useState(draft?.settings?.transitionSecs ?? 0.5);
  const [exportError, setExportError] = useState<string | null>(null);
  const [exportJobs, setExportJobs] = useState<CreationExportProgress[]>([]);
  const exporting = exportJobs.some((job) => job.status === "queued" || job.status === "running" || job.status === "cancelling");
  // 导出参数弹窗:分辨率(短边档位,不放大)/ 画质(CRF 档位);转场沿用上方状态,弹窗内选择
  const [exportOpen, setExportOpen] = useState(false);
  const [exportRes, setExportRes] = useState<ExportResolution>(
    draft?.settings?.exportResolution ?? "original",
  );
  const [exportQuality, setExportQuality] = useState<ExportQuality>(
    draft?.settings?.exportQuality ?? "medium",
  );
  // 预览模式:source = 源片直放;timeline = 按可见视频轨片段序列播放(自动跳过非片段区间)
  const [previewMode, setPreviewMode] = useState<"source" | "timeline">("source");
  // 草稿 id 稳定化:无草稿打开时生成一个,片段落进来即以此 id 保存
  const draftId = useRef(draft?.id ?? crypto.randomUUID());
  // 封面(dataURL):草稿已带则复用;没有则元数据就绪后后台抓帧,自动保存时一并写入
  const coverRef = useRef(draft?.cover);
  // 草稿名称(「草稿设置」对话框可改,随草稿持久化到首页草稿库)
  const [draftName, setDraftName] = useState(() => draft?.name ?? baseName(inputPath));
  // 视频分辨率(loadedmetadata 时读取);lastSavedAt = 草稿最近保存时间
  const [videoMeta, setVideoMeta] = useState<{ w: number; h: number } | null>(null);
  const [lastSavedAt, setLastSavedAt] = useState<number | null>(draft?.updatedAt ?? null);
  // 视频元信息(帧率/码率/编码,后端 ffmpeg -i 解析)与时间轴胶片条(media_root 相对路径)
  const [videoInfo, setVideoInfo] = useState<VideoInfo | null>(null);
  const [mainVideoInfo, setMainVideoInfo] = useState<VideoInfo | null>(null);
  const [thumbRels, setThumbRels] = useState<string[]>([]);
  // 音频波形峰值(出现音频轨片段时按需解码一次)
  const [audioPeaks, setAudioPeaks] = useState<Float32Array | null>(null);
  const [externalAudioPeaks, setExternalAudioPeaks] = useState<Map<string, Float32Array>>(
    () => new Map(),
  );
  const [settingsOpen, setSettingsOpen] = useState(false);
  // 对话框内编辑中的名称(取消不落地)
  const [nameDraft, setNameDraft] = useState("");
  // 时间轴右键菜单(轨道头 / 片段 / 空白通道);关闭 = 点菜单外 / Escape / 执行动作后
  const [ctxMenu, setCtxMenu] = useState<CtxTarget | null>(null);
  const ctxMenuRef = useRef<HTMLDivElement | null>(null);
  // 轨道重命名对话框(id 为 null 时关闭)
  const [renameTarget, setRenameTarget] = useState<string | null>(null);
  const [renameName, setRenameName] = useState("");
  const [textTransformPreview, setTextTransformPreview] = useState<{
    clipId: string;
    x: number;
    y: number;
    rotation: number;
  } | null>(null);
  const textGestureRef = useRef<{
    clipId: string;
    mode: "move" | "rotate";
    startClientX: number;
    startClientY: number;
    startPointerAngle: number;
    x: number;
    y: number;
    rotation: number;
    canvasRect: DOMRect;
  } | null>(null);

  function seedInitialVideoClip(sourceDuration: number) {
    if (initialClipSeeded.current || !Number.isFinite(sourceDuration) || sourceDuration <= 0) return;
    initialClipSeeded.current = true;
    const clipId = crypto.randomUUID();
    const targetTrackId = tracks.find((track) => track.type === "video")?.id ?? crypto.randomUUID();
    setTracks((currentTracks) => {
      if (currentTracks.some((track) => track.type === "video" && track.clips.length > 0)) {
        return currentTracks;
      }
      const clip: TrackClip = {
        id: clipId,
        start: 0,
        end: sourceDuration,
        position: 0,
        inputPath,
        sourceDuration,
      };
      const videoTrack = currentTracks.find((track) => track.type === "video");
      if (videoTrack) {
        return currentTracks.map((track) =>
          track.id === videoTrack.id ? { ...track, clips: [clip] } : track,
        );
      }
      return [{ id: targetTrackId, type: "video", name: "视频 1", clips: [clip] }, ...currentTracks];
    });
    setActiveTrackId(targetTrackId);
    setSelectedClipIds([clipId]);
    timelineClipIdRef.current = clipId;
    setMediaInitState("ready");
  }

  // 快速视频不闪烁转圈；超过短暂阈值才显示进度说明。初始化层本身立即遮住编辑区，
  // 因而用户不会先看到播放器就绪、时间轴却仍为空的中间状态。
  useEffect(() => {
    if (mediaInitState !== "loading") {
      setShowMediaInitProgress(false);
      return;
    }
    const progressTimer = window.setTimeout(() => setShowMediaInitProgress(true), 180);
    const errorTimer = window.setTimeout(() => setMediaInitState("error"), 30_000);
    return () => {
      window.clearTimeout(progressTimer);
      window.clearTimeout(errorTimer);
    };
  }, [mediaInitState]);

  // 后台队列事件：组件重新进入时先恢复活跃任务，随后持续合并进度与最终结果。
  useEffect(() => {
    let active = true;
    let dispose: (() => void) | undefined;
    void api.creationListActiveExports().then((jobs) => {
      if (active) setExportJobs(jobs);
    });
    void listen<CreationExportProgress>("creation-export-progress", (event) => {
      const job = event.payload;
      setExportJobs((currentJobs) => {
        const exists = currentJobs.some((value) => value.jobId === job.jobId);
        return exists
          ? currentJobs.map((value) => (value.jobId === job.jobId ? job : value))
          : [job, ...currentJobs];
      });
      if (job.status === "completed" && job.outputPath) {
        toast.success("后台视频导出完成", {
          action: {
            label: "打开文件夹",
            onClick: () => void api.revealPath(job.outputPath!),
          },
        });
      } else if (job.status === "failed") {
        setExportError(job.error ?? "后台导出失败");
      } else if (job.status === "cancelled") {
        toast.info("已取消导出");
      }
    }).then((unlisten) => {
      if (active) dispose = unlisten;
      else unlisten();
    });
    return () => {
      active = false;
      dispose?.();
    };
  }, []);

  // 右键菜单关闭:菜单外按下 / Escape
  useEffect(() => {
    if (!ctxMenu) return;
    const onDown = (e: MouseEvent) => {
      if (ctxMenuRef.current && !ctxMenuRef.current.contains(e.target as Node)) setCtxMenu(null);
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") setCtxMenu(null);
    };
    document.addEventListener("mousedown", onDown, true);
    window.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("mousedown", onDown, true);
      window.removeEventListener("keydown", onKey);
    };
  }, [ctxMenu]);

  // 右键菜单视口夹紧:按渲染后的实际尺寸把菜单整体收进窗口(固定估算会漏,
  // 底部靠边时最后一项「删除轨道」被窗口边缘裁掉)
  useLayoutEffect(() => {
    const el = ctxMenuRef.current;
    if (!el || !ctxMenu) return;
    const r = el.getBoundingClientRect();
    el.style.left = `${Math.max(8, Math.min(ctxMenu.x, window.innerWidth - r.width - 8))}px`;
    el.style.top = `${Math.max(8, Math.min(ctxMenu.y, window.innerHeight - r.height - 8))}px`;
  }, [ctxMenu]);

  // 无草稿新开会话:activeTrackId 在 tracks 初始化后补上(初始 tracks[0])
  useEffect(() => {
    if (!activeTrackId && tracks.length > 0) setActiveTrackId(tracks[0].id);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // 工程变化自动存草稿(防抖 500ms)。空时间轴也保存,这样刚导入的素材与导出参数不会丢失。
  useEffect(() => {
    // 新视频尚未创建主轨片段时不落空草稿，避免下次恢复出“1 条轨道、0 个片段”。
    if (mediaInitState !== "ready") return;
    const t = setTimeout(() => {
      const now = Math.floor(Date.now() / 1000);
      saveDraft({
        id: draftId.current,
        name: draftName,
        inputPath,
        tracks,
        markers,
        settings: {
          transitionKind,
          transitionSecs,
          exportResolution: exportRes,
          exportQuality,
          playbackRate: rate,
          trackHeight,
          waveLevel,
          editMode,
        },
        updatedAt: now,
        cover: coverRef.current,
        coverPath: draft?.coverPath,
      });
      setLastSavedAt(now);
    }, 500);
    return () => clearTimeout(t);
  }, [
    tracks,
    markers,
    inputPath,
    draftName,
    transitionKind,
    transitionSecs,
    exportRes,
    exportQuality,
    rate,
    trackHeight,
    waveLevel,
    editMode,
    draft?.coverPath,
    mediaInitState,
  ]);

  // 倍速变化实时应用到视频元素与音频轨预览元素(保持音画同速)
  useEffect(() => {
    const active = tracks.flatMap((track) => track.clips).find((clip) => clip.id === timelineClipIdRef.current);
    if (videoRef.current) videoRef.current.playbackRate = rate * (previewMode === "timeline" && active ? clipSpeedAtSource(active, current - active.start) : 1);
    for (const track of tracks.filter((item) => item.type === "audio")) {
      for (const clip of track.clips) {
        const el = audioElsRef.current.get(clip.id);
        if (el) el.playbackRate = rate * clipSpeed(clip);
      }
    }
  }, [rate, tracks, previewMode]);

  // 后台生成短 GOP 代理。完成后仅替换播放器媒体,工程片段和导出路径仍引用原文件;
  // 切换时保存播放位置与状态,避免代理就绪造成预览跳回开头。
  useEffect(() => {
    if (proxyBySource[previewPath] || proxyRequested.current.has(previewPath)) return;
    proxyRequested.current.add(previewPath);
    setPreparingProxyPath(previewPath);
    void api.creationVideoProxy(previewPath)
      .then((proxyPath) => {
        if (!proxyPath) return;
        if (previewPathRef.current === previewPath) {
          const video = videoRef.current;
          pendingMediaSeek.current = {
            time: video?.currentTime ?? 0,
            play: video ? !video.paused : false,
            preserveSelection: true,
          };
        }
        setProxyBySource((prev) => ({ ...prev, [previewPath]: proxyPath }));
      })
      .catch(() => {
        // 代理失败不阻断编辑,播放器继续使用原素材。
      })
      .finally(() => {
        setPreparingProxyPath((currentPath) =>
          currentPath === previewPath ? null : currentPath,
        );
      });
  }, [previewPath, proxyBySource]);

  // 元信息与胶片条跟随当前预览素材切换;未激活素材使用纹理占位,避免错贴缩略图。
  useEffect(() => {
    let cancelled = false;
    setVideoInfo(null);
    setThumbRels([]);
    api.creationVideoInfo(previewPath).then((info) => {
      if (!cancelled) {
        setVideoInfo(info);
        if (previewPath === inputPath) seedInitialVideoClip(info.durationSecs);
      }
    }).catch(() => {});
    api.creationVideoThumbs(previewPath).then((rels) => {
      if (!cancelled) setThumbRels(rels);
    }).catch(() => {});
    return () => {
      cancelled = true;
    };
  }, [previewPath]);

  // 工程画布始终以主视频为基准;切去查看其他素材不应改变导出分辨率。
  useEffect(() => {
    let cancelled = false;
    api.creationVideoInfo(inputPath).then((info) => {
      if (!cancelled) setMainVideoInfo(info);
    }).catch(() => {});
    return () => {
      cancelled = true;
    };
  }, [inputPath]);

  // 音频波形由后端 FFmpeg 流式解码并缓存；大视频不会再被 WebView 整文件读入内存。
  // 视频片段底部也需要波形，方便对着人声下刀。
  const wantPeaks = tracks.some((t) => t.clips.length > 0);
  useEffect(() => {
    setAudioPeaks(null);
    if (!wantPeaks) return;
    let cancelled = false;
    void api.creationAudioPeaks(previewPath, 32768)
      .then((peaks) => {
        if (!cancelled && peaks.length) setAudioPeaks(Float32Array.from(peaks));
      })
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  }, [wantPeaks, previewPath]);

  // 外部音乐/音效也走后端缓存，恢复草稿时不会重复计算。
  useEffect(() => {
    let cancelled = false;
    const paths = [
      ...new Set(
        tracks.flatMap((t) =>
          t.type === "audio"
            ? t.clips.map((clip) => clip.inputPath).filter((path): path is string => !!path)
            : [],
        ),
      ),
    ];
    for (const path of paths) {
      if (externalAudioPeaks.has(path)) continue;
      void api.creationAudioPeaks(path, 32768)
        .then((peaks) => {
          if (!peaks.length || cancelled) return;
          setExternalAudioPeaks((prev) => {
            if (prev.has(path)) return prev;
            const next = new Map(prev);
            next.set(path, Float32Array.from(peaks));
            return next;
          });
        })
        .catch(() => {});
    }
    return () => {
      cancelled = true;
    };
  }, [tracks, externalAudioPeaks]);

  // 封面抓取:草稿没有封面时,元数据就绪后后台抓一帧写入草稿库(首页卡片展示)。
  // 抓到即写 setDraftCover(不依赖片段入档);后续自动保存也会经 coverRef 带上。
  useEffect(() => {
    if (coverRef.current || duration <= 0) return;
    let cancelled = false;
    void captureVideoCover(mediaFileUrl(inputPath)).then((cover) => {
      if (!cover || cancelled) return;
      coverRef.current = cover;
      setDraftCover(draftId.current, cover);
    });
    return () => {
      cancelled = true;
    };
  }, [duration, inputPath, mediaFileUrl]);

  async function importVideo() {
    const picked = await openDialog({
      multiple: false,
      filters: [{ name: "视频", extensions: ["mp4", "mov", "mkv", "webm", "avi"] }],
    });
    if (typeof picked === "string") onOpen(picked);
  }

  async function importVideoAssets() {
    const picked = await openDialog({
      multiple: true,
      filters: [{ name: "视频", extensions: ["mp4", "mov", "mkv", "webm", "avi"] }],
    });
    const paths = typeof picked === "string" ? [picked] : picked;
    if (!paths?.length) return;

    const additions: EditorTrack[] = [];
    let position = tracks
      .filter((track) => track.type === "video" && !track.hidden)
      .flatMap((track) => track.clips)
      .reduce((end, clip) => Math.max(end, clipPos(clip) + clipLen(clip)), 0);
    for (const path of paths) {
      try {
        const info = await api.creationVideoInfo(path);
        const track = newTrack("video", [...tracks, ...additions]);
        track.name = baseName(path);
        track.clips = [
          {
            id: crypto.randomUUID(),
            start: 0,
            end: info.durationSecs,
            position,
            inputPath: path,
            sourceDuration: info.durationSecs,
          },
        ];
        additions.push(track);
        position += info.durationSecs;
      } catch (e) {
        toast.error(`${baseName(path)} 导入失败: ${e instanceof Error ? e.message : String(e)}`);
      }
    }
    if (!additions.length) return;
    commitTracks((prev) => [...prev, ...additions]);
    setActiveTrackId(additions[additions.length - 1].id);
    toast.success(`已导入 ${additions.length} 个视频素材`);
  }

  async function importAudio() {
    const picked = await openDialog({
      multiple: true,
      filters: [{ name: "音频", extensions: ["mp3", "wav", "m4a", "aac", "flac", "ogg"] }],
    });
    const paths = typeof picked === "string" ? [picked] : picked;
    if (!paths?.length) return;

    const additions: EditorTrack[] = [];
    for (const path of paths) {
      try {
        const sourceDuration = await readAudioDuration(mediaFileUrl(path));
        const track = newTrack("audio", [...tracks, ...additions]);
        track.name = baseName(path);
        track.clips = [
          {
            id: crypto.randomUUID(),
            start: 0,
            end: duration > 0 ? Math.min(sourceDuration, duration) : sourceDuration,
            position: 0,
            inputPath: path,
            sourceDuration,
          },
        ];
        additions.push(track);
      } catch (e) {
        toast.error(`${baseName(path)} 导入失败: ${e instanceof Error ? e.message : String(e)}`);
      }
    }
    if (!additions.length) return;
    commitTracks((prev) => [...prev, ...additions]);
    setActiveTrackId(additions[additions.length - 1].id);
    setPreviewMode("timeline");
    toast.success(`已导入 ${additions.length} 个音频素材`);
  }

  function seek(t: number) {
    // scrub 过程不推 React 状态:Timeline 已直接移动播放头,视频实际帧由 seek 泵更新;
    // 松手时 seekExact 一次提交,避免高频拖动让属性面板和预览覆盖层整棵重渲染。
    scrubTarget.current = t;
  }

  // scrub 两阶段定位:拖动中约 12fps 快速跳关键帧,只用于跟手预览;松手后覆盖旧 seek,
  // 仅做一次精确定位。长 GOP 视频不再积压几十次精确解码,最终落点可以优先执行。
  const scrubTarget = useRef<number | null>(null);
  const pumpRaf = useRef(0);
  const lastPreviewSeekAt = useRef(0);
  function startSeekPump() {
    if (pumpRaf.current) return;
    const tick = () => {
      pumpRaf.current = requestAnimationFrame(tick);
      const v = videoRef.current;
      const now = performance.now();
      if (!v || v.seeking || now - lastPreviewSeekAt.current < 80) return;
      const target = scrubTarget.current;
      if (target === null || Math.abs(target - v.currentTime) < 0.04) return;
      lastPreviewSeekAt.current = now;
      const fastVideo = v as HTMLVideoElement & { fastSeek?: (time: number) => void };
      if (typeof fastVideo.fastSeek === "function") fastVideo.fastSeek(target);
      else v.currentTime = target;
    };
    pumpRaf.current = requestAnimationFrame(tick);
  }
  function stopSeekPump() {
    if (pumpRaf.current) {
      cancelAnimationFrame(pumpRaf.current);
      pumpRaf.current = 0;
    }
    scrubTarget.current = null;
  }

  // 精确落点(scrub 结束 / 点击片段):丢弃泵的挂起目标后直接设 currentTime
  function seekExact(t: number) {
    scrubTarget.current = null;
    const v = videoRef.current;
    if (v && Math.abs(v.currentTime - t) >= 0.005) {
      // 新 currentTime 会按媒体规范中止/覆盖尚未完成的快速 seek,最终只解码这个目标。
      v.currentTime = t;
    }
    setCurrent(t);
  }

  function clipSourcePath(clip: TrackClip): string {
    return clip.inputPath ?? inputPath;
  }

  function setTimelineGapTime(time: number | null) {
    timelineGapTimeRef.current = time;
    setTimelineGapTimeState(time);
  }

  // 切换素材后必须等 loadedmetadata 才能可靠 seek;同素材则直接定位。
  function openTimelineClip(clip: TrackClip, play = false, time = clip.start) {
    const path = clipSourcePath(clip);
    setTimelineGapTime(null);
    timelineClipIdRef.current = clip.id;
    if (videoRef.current) videoRef.current.playbackRate = rate * clipSpeedAtSource(clip, time - clip.start);
    if (path !== previewPathRef.current) {
      videoRef.current?.pause();
      pendingMediaSeek.current = { time, play };
      setPreviewPath(path);
      return;
    }
    seekExact(time);
    if (play) void videoRef.current?.play();
  }

  function seekSequencePosition(sequenceTime: number, exact: boolean) {
    const clip = playSeq.find(
      (candidate) =>
        sequenceTime >= clipPos(candidate) - 0.01 &&
        sequenceTime <= clipPos(candidate) + clipLen(candidate) + 0.01,
    );
    if (!clip) {
      // 空白区必须立即停掉旧视频并显示黑场,否则视觉上会误以为空轨仍有素材。
      scrubTarget.current = null;
      timelineClipIdRef.current = null;
      videoRef.current?.pause();
      pauseAllAudio();
      setTimelineGapTime(sequenceTime);
      return;
    }
    setTimelineGapTime(null);
    const sourceTime = Math.min(
      clip.end,
      clip.start + timelineOffsetToSource(clip, Math.max(0, sequenceTime - clipPos(clip))),
    );
    if (clipSourcePath(clip) !== previewPathRef.current) {
      openTimelineClip(clip, false, sourceTime);
    } else if (exact) {
      timelineClipIdRef.current = clip.id;
      seekExact(sourceTime);
    } else {
      timelineClipIdRef.current = clip.id;
      seek(sourceTime);
    }
  }

  function togglePlay() {
    const v = videoRef.current;
    if (!v) return;
    if (v.paused) {
      // 时间轴模式:当前不在任何片段内时,先跳到序列开头再播
      if (previewMode === "timeline" && playSeq.length) {
        if (timelineGapTimeRef.current !== null) {
          const next = playSeq.find(
            (clip) => clipPos(clip) >= timelineGapTimeRef.current! - 0.01,
          );
          if (next) openTimelineClip(next, true);
          return;
        }
        const active = playSeq.find(
          (c) =>
            c.id === timelineClipIdRef.current &&
            clipSourcePath(c) === previewPathRef.current &&
            current >= c.start &&
            current < c.end,
        );
        if (!active) {
          openTimelineClip(playSeq[0], true);
          return;
        }
      }
      void v.play();
    } else {
      v.pause();
    }
  }

  // 序列预览:播到片段尾自动跳下一段开头;序列外(手动 seek 到空隙)跳最近下一段,没有则停
  function handleTimeUpdate(t: number) {
    if (scrubTarget.current === null) setCurrent(t);
    if (previewMode !== "timeline" || !playing || !playSeq.length) return;
    const idx = playSeq.findIndex(
      (c) =>
        c.id === timelineClipIdRef.current &&
        clipSourcePath(c) === previewPathRef.current &&
        t >= c.start - 0.05 &&
        t < c.end,
    );
    if (idx >= 0) {
      if (t >= playSeq[idx].end - 0.04) {
        const next = playSeq[idx + 1];
        const clipRight = clipPos(playSeq[idx]) + clipLen(playSeq[idx]);
        if (next && clipPos(next) <= clipRight + 0.05) {
          openTimelineClip(next, true);
        } else {
          videoRef.current?.pause();
          setTimelineGapTime(clipRight);
        }
      }
      return;
    }
    videoRef.current?.pause();
  }

  // 切换 源片 / 时间轴 预览:无可见视频轨片段时不让进时间轴模式
  function switchPreviewMode(mode: "source" | "timeline") {
    if (mode === previewMode) return;
    videoRef.current?.pause();
    if (mode === "timeline") {
      if (!playSeq.length) {
        toast.error("可见视频轨没有片段,先在时间轴添加片段");
        return;
      }
      setPreviewMode(mode);
      openTimelineClip(playSeq[0]);
      return;
    }
    timelineClipIdRef.current = null;
    setTimelineGapTime(null);
    setPreviewMode(mode);
    if (previewPathRef.current !== inputPath) {
      pendingMediaSeek.current = { time: 0, play: false };
      setPreviewPath(inputPath);
    }
  }

  // 拖播放头(scrub)时暂停播放并启动 seek 泵,松手后精确落点、视情况恢复播放
  const scrubWasPlaying = useRef(false);
  function handleScrubChange(scrubbing: boolean) {
    const v = videoRef.current;
    if (!v) return;
    if (scrubbing) {
      scrubWasPlaying.current = !v.paused;
      v.pause();
      lastPreviewSeekAt.current = 0;
      startSeekPump();
    } else {
      if (previewMode === "timeline" && timelineGapTimeRef.current !== null) {
        stopSeekPump();
        scrubWasPlaying.current = false;
        return;
      }
      // 松手落点:优先取拖动中的最新目标(scrubTarget 是 ref,不受 actions 记忆化
      // 闭包影响);不能读 current 状态——timelineActions 被 memo 冻结在旧渲染,
      // 闭包里的 current 是旧值(≈0),会把播放头拉回原点
      const finalT = scrubTarget.current ?? v.currentTime;
      stopSeekPump();
      seekExact(finalT);
      if (scrubWasPlaying.current) {
        scrubWasPlaying.current = false;
        void v.play();
      }
    }
  }

  // 入点不能越过出点(反之亦然),留 0.1s 最小时长
  const setIn = () => setSelStart(Math.min(current, selEnd - 0.1));
  const setOut = () => setSelEnd(Math.max(current, selStart + 0.1));

  function syncHist() {
    setHistCounts({ past: history.current.past.length, future: history.current.future.length });
  }

  // 轨道变更统一入口:入撤销历史 + 清空重做栈
  function commitTracks(next: (prev: EditorTrack[]) => EditorTrack[]) {
    history.current.past.push(tracks);
    if (history.current.past.length > 50) history.current.past.shift();
    history.current.future = [];
    setTracks(next);
    syncHist();
  }

  function undo() {
    const snapshot = history.current.past.pop();
    if (!snapshot) return;
    history.current.future.push(tracks);
    setTracks(snapshot);
    syncHist();
  }

  function redo() {
    const snapshot = history.current.future.pop();
    if (!snapshot) return;
    history.current.past.push(tracks);
    setTracks(snapshot);
    syncHist();
  }

  function selectClip(clipId: string, additive: boolean) {
    const linkedIds = new Set([clipId]);
    const allClips = tracks.flatMap((track) => track.clips);
    const clicked = allClips.find((clip) => clip.id === clipId);
    if (clicked?.groupId) {
      for (const clip of allClips) if (clip.groupId === clicked.groupId) linkedIds.add(clip.id);
    }
    if (linkedSelection) {
      if (clicked?.detachedFrom) linkedIds.add(clicked.detachedFrom);
      for (const clip of allClips) {
        if (clip.detachedFrom === clipId || (clicked?.detachedFrom && clip.detachedFrom === clicked.detachedFrom)) {
          linkedIds.add(clip.id);
        }
      }
    }
    setSelectedClipIds((prev) =>
      additive
        ? prev.includes(clipId)
          ? prev.filter((x) => !linkedIds.has(x))
          : [...new Set([...prev, ...linkedIds])]
        : [...linkedIds],
    );
  }

  function groupSelectedClips() {
    if (selectedClipIds.length < 2) {
      toast.error("至少选择两个片段才能编组");
      return;
    }
    const groupId = crypto.randomUUID();
    commitTracks((prev) => prev.map((track) => ({
      ...track,
      clips: track.clips.map((clip) => selectedClipIds.includes(clip.id) ? { ...clip, groupId } : clip),
    })));
    toast.success(`已将 ${selectedClipIds.length} 个片段编组`);
  }

  function ungroupSelectedClips() {
    const groupIds = new Set(tracks.flatMap((track) => track.clips)
      .filter((clip) => selectedClipIds.includes(clip.id) && clip.groupId)
      .map((clip) => clip.groupId!));
    if (!groupIds.size) {
      toast.info("所选片段没有编组");
      return;
    }
    commitTracks((prev) => prev.map((track) => ({
      ...track,
      clips: track.clips.map((clip) => clip.groupId && groupIds.has(clip.groupId)
        ? { ...clip, groupId: undefined }
        : clip),
    })));
    toast.success("已解除片段编组");
  }

  // 向左/向右全选([ / ]):选中播放头一侧的全部片段
  function selectClipsSide(side: "left" | "right") {
    // 左右关系必须按时间轴 position 判断;start/end 是素材源时间,多素材时不可直接比较。
    const playheadPosition = sourceToSeq(current) ?? current;
    const ids = tracks.flatMap((t) =>
      t.clips
        .filter((c) =>
          side === "left"
            ? clipPos(c) + clipLen(c) <= playheadPosition + 0.01
            : clipPos(c) >= playheadPosition - 0.01,
        )
        .map((c) => c.id),
    );
    if (!ids.length) {
      toast.error(side === "left" ? "播放头左侧没有片段" : "播放头右侧没有片段");
      return;
    }
    setSelectedClipIds(ids);
  }

  // 分割(B):播放头处切开片段;有选中片段只切选中的,否则切所有未锁定轨上被播放头穿过的
  function splitAtPlayhead() {
    const t = current;
    const anyHit = tracks.some(
      (track) =>
        !track.locked &&
        track.clips.some((c) => {
          const hit = selectedClipIds.length ? selectedClipIds.includes(c.id) : true;
          return hit && clipSourcePath(c) === previewPath && c.start < t - 0.01 && c.end > t + 0.01;
        }),
    );
    if (!anyHit) {
      toast.error("播放头处没有可分割的片段");
      return;
    }
    splitAtTimes([t], previewPath);
  }

  // 按切点分割所有未锁定轨片段(智能切分 / 播放头分割共用;场景切点音频轨一并切开,保持音画对齐)。
  // 新片段 position 按源内偏移换算,保证重排后的轨上分割不跳位
  function splitAtTimes(times: number[], sourcePath?: string) {
    const sorted = [...times].sort((a, b) => a - b);
    commitTracks((prev) =>
      prev.map((track) => {
        if (track.locked || track.type === "text") return track;
        return {
          ...track,
          clips: track.clips.flatMap((c) => {
            if (sourcePath && clipSourcePath(c) !== sourcePath) return [c];
            const points = sorted.filter((t) => t > c.start + 0.01 && t < c.end - 0.01);
            if (!points.length) return [c];
            const basePos = clipPos(c);
            const boundaries = [c.start, ...points, c.end];
            return boundaries.slice(0, -1).map((start, index) => {
              const end = boundaries[index + 1];
              const timelineStart = sourceOffsetToTimeline(c, start - c.start);
              const timelineEnd = sourceOffsetToTimeline(c, end - c.start);
              return {
                ...c,
                id: index === 0 ? c.id : crypto.randomUUID(),
                start,
                end,
                position: basePos + timelineStart,
                speedCurve: slicedSpeedCurve(c, start - c.start, end - c.start),
                transform: interpolatedTransform(c, basePos + timelineStart),
                keyframes: c.keyframes
                  ?.filter((frame) => frame.offset >= timelineStart - 0.001 && frame.offset <= timelineEnd + 0.001)
                  .map((frame) => ({ ...frame, offset: frame.offset - timelineStart })),
              };
            });
          }),
        };
      }),
    );
  }

  // 智能切分:OpenCV 场景检测 → 切点处分割所有未锁定视频轨
  const [detecting, setDetecting] = useState(false);
  async function smartSplit() {
    if (detecting) return;
    setDetecting(true);
    try {
      const cuts = await api.creationDetectScenes(previewPath);
      const inRange = cuts.filter((t) => t > 0.1 && t < duration - 0.1);
      if (!inRange.length) {
        toast.info("未检测到场景切换点");
        return;
      }
      splitAtTimes(inRange, previewPath);
      toast.success(`智能切分完成:${inRange.length} 个切点`);
    } catch (e) {
      toast.error(`智能切分失败: ${e}`);
    } finally {
      setDetecting(false);
    }
  }

  // 普通删除保留空隙；波纹删除只把同轨后续片段左移被删时长，不破坏原有间距。
  function deleteSelectedClips(ripple = false) {
    if (!selectedClipIds.length) {
      toast.error("先点击选中要删除的片段");
      return;
    }
    commitTracks((prev) =>
      prev.map((t) => {
        if (t.locked) return t;
        const removed = t.clips
          .filter((c) => selectedClipIds.includes(c.id))
          .map((c) => ({ start: clipPos(c), end: clipPos(c) + clipLen(c), length: clipLen(c) }))
          .sort((a, b) => a.start - b.start);
        const kept = t.clips.filter((c) => !selectedClipIds.includes(c.id));
        if (kept.length === t.clips.length) return t;
        if (!ripple) return { ...t, clips: kept };
        return {
          ...t,
          clips: kept.map((clip) => {
            const shift = removed
              .filter((range) => range.end <= clipPos(clip) + 0.001)
              .reduce((sum, range) => sum + range.length, 0);
            return shift > 0 ? { ...clip, position: Math.max(0, clipPos(clip) - shift) } : clip;
          }),
        };
      }),
    );
    setSelectedClipIds([]);
  }

  // 快捷键:空格 播放/暂停 / A 选择(清除选择)/ B 分割 / [ 向左全选 / ] 向右全选 /
  // Delete 删除选中 / Ctrl+Z(Shift) 撤销重做。
  // 不设依赖数组:每渲染重挂一次,保证闭包拿到最新 tracks / current / 选择态。
  useEffect(() => {
    function onKey(e: KeyboardEvent) {
      const el = e.target as HTMLElement;
      if (el.tagName === "INPUT" || el.tagName === "TEXTAREA" || el.isContentEditable) return;
      // 右键菜单打开时不响应编辑快捷键(Escape 关菜单由菜单自己的监听处理)
      if (ctxMenu) return;
      if (e.ctrlKey || e.metaKey) {
        const k = e.key.toLowerCase();
        if (k === "z") {
          e.preventDefault();
          if (e.shiftKey) redo();
          else undo();
        } else if (k === "y") {
          e.preventDefault();
          redo();
        } else if (k === "g") {
          e.preventDefault();
          if (e.shiftKey) ungroupSelectedClips();
          else groupSelectedClips();
        }
        return;
      }
      if (e.key === " " || e.code === "Space") {
        // 播放/暂停;preventDefault 阻止页面滚动与焦点按钮被空格激活
        e.preventDefault();
        if (duration > 0) togglePlay();
      } else if (e.key === "Delete" || e.key === "Backspace") {
        if (selectedClipIds.length) deleteSelectedClips(e.shiftKey);
      } else if (e.key.toLowerCase() === "b") splitAtPlayhead();
      else if (e.key.toLowerCase() === "m") addMarker();
      else if (e.key.toLowerCase() === "a") setSelectedClipIds([]);
      else if (e.key === "[") selectClipsSide("left");
      else if (e.key === "]") selectClipsSide("right");
    }
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  });

  // 新轨道插到激活轨正下方(无激活轨时追加到末尾)
  function addTrack(type: TrackType) {
    const t = newTrack(type, tracks);
    commitTracks((prev) => {
      const idx = prev.findIndex((x) => x.id === activeTrackId);
      const at = idx >= 0 ? idx + 1 : prev.length;
      return [...prev.slice(0, at), t, ...prev.slice(at)];
    });
    setActiveTrackId(t.id);
  }

  // 在指定下标插入新轨道(右键菜单「上方 / 下方插入」),并激活
  function insertTrackAt(type: TrackType, index: number) {
    const t = newTrack(type, tracks);
    commitTracks((prev) => {
      const arr = [...prev];
      arr.splice(Math.max(0, Math.min(arr.length, index)), 0, t);
      return arr;
    });
    setActiveTrackId(t.id);
  }

  // 轨道拖拽排序(from → to 位置)
  function moveTrack(from: number, to: number) {
    commitTracks((prev) => {
      const next = [...prev];
      const [moved] = next.splice(from, 1);
      next.splice(to, 0, moved);
      return next;
    });
  }

  // 上移 / 下移一条轨道(右键菜单;delta = -1 / 1),越界静默
  function moveTrackBy(id: string, delta: -1 | 1) {
    commitTracks((prev) => {
      const i = prev.findIndex((t) => t.id === id);
      const j = i + delta;
      if (i < 0 || j < 0 || j >= prev.length) return prev;
      const next = [...prev];
      [next[i], next[j]] = [next[j], next[i]];
      return next;
    });
  }

  // 复制轨道:克隆轨道及全部片段(新片段 id),插到该轨下方
  function duplicateTrack(id: string) {
    const src = tracks.find((t) => t.id === id);
    if (!src) return;
    const copy: EditorTrack = {
      ...src,
      id: crypto.randomUUID(),
      name: `${src.name} 副本`,
      locked: false,
      clips: src.clips.map((c) => ({ ...c, id: crypto.randomUUID() })),
    };
    commitTracks((prev) => {
      const at = prev.findIndex((t) => t.id === id) + 1;
      return [...prev.slice(0, at), copy, ...prev.slice(at)];
    });
    setActiveTrackId(copy.id);
    toast.success(`已复制「${src.name}」`);
  }

  // 清空轨道全部片段(锁定轨拒绝)
  function clearTrackClips(id: string) {
    if (tracks.find((t) => t.id === id)?.locked) {
      toast.error("轨道已锁定,先解锁再清空");
      return;
    }
    commitTracks((prev) => prev.map((t) => (t.id === id ? { ...t, clips: [] } : t)));
    setSelectedClipIds([]);
  }

  // 重命名轨道(对话框确认后调用)
  function renameTrack(id: string, name: string) {
    const trimmed = name.trim();
    if (!trimmed) return;
    commitTracks((prev) => prev.map((t) => (t.id === id ? { ...t, name: trimmed } : t)));
  }

  // 全选某条轨道的全部片段
  function selectAllClipsInTrack(id: string) {
    const ids = tracks.find((t) => t.id === id)?.clips.map((c) => c.id) ?? [];
    if (!ids.length) {
      toast.error("该轨道没有片段");
      return;
    }
    setSelectedClipIds(ids);
  }

  // 片段剪贴板(会话级,不入草稿):复制的源区间集合,粘贴时克隆新 id 落到目标轨
  const clipClipboard = useRef<{ type: TrackType; clips: TrackClip[] } | null>(null);
  function copySelectedClips() {
    const selectedTracks = tracks
      .map((track) => ({
        type: track.type,
        clips: track.clips.filter((clip) => selectedClipIds.includes(clip.id)),
      }))
      .filter((entry) => entry.clips.length > 0);
    const copied = selectedTracks.flatMap((entry) => entry.clips);
    if (!copied.length) {
      toast.error("先选中要复制的片段");
      return;
    }
    const types = new Set(selectedTracks.map((entry) => entry.type));
    if (types.size !== 1) {
      toast.error("视频与音频片段不能混合复制,请只选择一种轨道类型");
      return;
    }
    clipClipboard.current = { type: selectedTracks[0].type, clips: copied };
    toast.success(`已复制 ${copied.length} 个片段`);
  }

  // 粘贴到目标轨(默认激活轨):跳过与目标轨已有片段完全同区间的(防连点重复粘贴)
  function pasteClips(targetTrackId?: string) {
    const trackId = targetTrackId ?? activeTrackId;
    const track = tracks.find((t) => t.id === trackId);
    if (!track) return;
    if (track.locked) {
      toast.error("轨道已锁定,先解锁再粘贴");
      return;
    }
    if (!clipClipboard.current) {
      toast.error("剪贴板没有片段");
      return;
    }
    if (clipClipboard.current.type !== track.type) {
      toast.error(`不能把${TRACK_META[clipClipboard.current.type].label}片段粘贴到${TRACK_META[track.type].label}轨`);
      return;
    }
    // 先按目标轨现有片段去重(同 position + 同源区间才算重复),全部重复时如实提示
    const exist = new Set(
      track.clips.map((c) => `${clipPos(c).toFixed(3)}~${c.start.toFixed(3)}~${c.end.toFixed(3)}`),
    );
    const additions = clipClipboard.current.clips
      .filter(
        (c) => !exist.has(`${clipPos(c).toFixed(3)}~${c.start.toFixed(3)}~${c.end.toFixed(3)}`),
      )
      .map((c) => ({ ...c, id: crypto.randomUUID() }));
    if (!additions.length) {
      toast.info("片段与该轨现有内容相同,未重复添加");
      return;
    }
    commitTracks((prev) =>
      prev.map((t) =>
        t.id === trackId
          ? { ...t, clips: [...t.clips, ...additions].sort((a, b) => clipPos(a) - clipPos(b) || a.start - b.start) }
          : t,
      ),
    );
    toast.success(
      additions.length === 1
        ? `已粘贴 1 个片段到「${track.name}」`
        : `已粘贴 ${additions.length} 个片段到「${track.name}」`,
    );
  }

  // 轨道开关:锁定(禁增删片段/删轨)/ 可见(隐藏轨不参与导出)/ 静音(音频轨不混音)
  function toggleTrackFlag(id: string, flag: "locked" | "hidden" | "muted") {
    commitTracks((prev) =>
      prev.map((t) => {
        if (t.id !== id) return t;
        if (flag === "locked") return { ...t, locked: !t.locked };
        if (flag === "hidden") return { ...t, hidden: !t.hidden };
        return { ...t, muted: !t.muted };
      }),
    );
  }

  function removeTrack(id: string) {
    if (tracks.find((t) => t.id === id)?.locked) {
      toast.error("轨道已锁定,先解锁再删除");
      return;
    }
    const removedClipIds = new Set(tracks.find((t) => t.id === id)?.clips.map((c) => c.id) ?? []);
    commitTracks((prev) => {
      const next = prev.filter((t) => t.id !== id);
      // 保底:至少留一条轨道
      return next.length ? next : [newTrack("video", [])];
    });
    if (activeTrackId === id) {
      setActiveTrackId((tracks.find((t) => t.id !== id)?.id) ?? "");
    }
    setSelectedClipIds((prev) => prev.filter((clipId) => !removedClipIds.has(clipId)));
  }

  function addTextClip(text: string) {
    const requestedPosition = previewMode === "timeline"
      ? sourceToSeq(current) ?? 0
      : sourceToSeq(current) ?? current;
    const canvasDuration = Math.max(0.1, seqSpan || duration);
    const position = Math.min(Math.max(0, requestedPosition), canvasDuration - 0.1);
    const textDuration = Math.min(3, Math.max(0.1, canvasDuration - position));
    const clip: TrackClip = {
      id: crypto.randomUUID(),
      start: 0,
      end: textDuration,
      position,
      text,
      fontSize: 42,
      textColor: "#ffffff",
      textPosition: "bottom",
      textX: 50,
      textY: 90,
      textRotation: 0,
    };
    const existing = tracks.find((track) => track.type === "text" && !track.locked);
    const target = existing ?? newTrack("text", tracks);
    commitTracks((prev) => {
      if (!existing) return [...prev, { ...target, clips: [clip] }];
      return prev.map((track) =>
        track.id === target.id
          ? { ...track, clips: [...track.clips, clip].sort((a, b) => clipPos(a) - clipPos(b)) }
          : track,
      );
    });
    setActiveTrackId(target.id);
    setSelectedClipIds([clip.id]);
    setPreviewMode("timeline");
    toast.success("文字已添加到时间线");
  }

  function updateClip(clipId: string, patch: Partial<TrackClip>) {
    commitTracks((prev) =>
      prev.map((track) => ({
        ...track,
        clips: track.clips.map((clip) => clip.id === clipId ? { ...clip, ...patch } : clip),
      })),
    );
  }

  function updateVideoTransform(clipId: string, patch: Partial<VideoTransformInput>) {
    const clip = tracks.flatMap((track) => track.clips).find((item) => item.id === clipId);
    if (!clip) return;
    const nextTransform = { ...clip.transform, ...patch };
    const animatable = "scale" in patch || "positionX" in patch || "positionY" in patch;
    if (animatable && (clip.keyframes?.length ?? 0) > 0) {
      const offset = previewMode === "timeline"
        ? Math.max(0, Math.min(clipLen(clip), (previewSequenceTime ?? clipPos(clip)) - clipPos(clip)))
        : Math.max(0, Math.min(clipLen(clip), sourceOffsetToTimeline(clip, current - clip.start)));
      const currentTransform = interpolatedTransform(clip, clipPos(clip) + offset);
      const nearby = clip.keyframes?.find((frame) => Math.abs(frame.offset - offset) <= 0.04);
      const frame = {
        id: nearby?.id ?? crypto.randomUUID(),
        offset,
        scale: patch.scale ?? currentTransform.scale ?? 1,
        positionX: patch.positionX ?? currentTransform.positionX ?? 0,
        positionY: patch.positionY ?? currentTransform.positionY ?? 0,
        easing: nearby?.easing ?? "linear" as const,
      };
      const keyframes = [...(clip.keyframes ?? []).filter((item) => item.id !== nearby?.id), frame]
        .sort((a, b) => a.offset - b.offset);
      updateClip(clipId, { transform: nextTransform, keyframes });
      return;
    }
    updateClip(clipId, { transform: nextTransform });
  }

  function addVideoKeyframe(clip: TrackClip) {
    if (transitionKind !== "none") {
      setTransitionKind("none");
      toast.info("关键帧动画已启用，全局转场已关闭");
    }
    const offset = previewMode === "timeline"
      ? Math.max(0, Math.min(clipLen(clip), (previewSequenceTime ?? clipPos(clip)) - clipPos(clip)))
      : Math.max(0, Math.min(clipLen(clip), sourceOffsetToTimeline(clip, current - clip.start)));
    const transform = interpolatedTransform(clip, clipPos(clip) + offset);
    const frame = {
      id: crypto.randomUUID(),
      offset,
      scale: transform.scale ?? 1,
      positionX: transform.positionX ?? 0,
      positionY: transform.positionY ?? 0,
      easing: "linear" as const,
    };
    const frames = [...(clip.keyframes ?? []).filter((item) => Math.abs(item.offset - offset) > 0.04), frame]
      .sort((a, b) => a.offset - b.offset);
    updateClip(clip.id, { keyframes: frames });
    toast.success(`已在 ${fmt(offset)} 添加关键帧`);
  }

  function removeVideoKeyframe(clip: TrackClip, id: string) {
    updateClip(clip.id, { keyframes: (clip.keyframes ?? []).filter((frame) => frame.id !== id) });
  }

  function applySpeedCurvePreset(clip: TrackClip, preset: "montage" | "bullet" | "speedUp" | "slowDown") {
    const length = Math.max(0.1, clip.end - clip.start);
    const values = preset === "montage"
      ? [[0, 1], [0.2, 2.6], [0.48, 0.65], [0.76, 2.1], [1, 1]]
      : preset === "bullet"
        ? [[0, 1.5], [0.32, 0.35], [0.68, 0.35], [1, 1.5]]
        : preset === "speedUp"
          ? [[0, 0.5], [1, 3]]
          : [[0, 3], [1, 0.5]];
    updateClip(clip.id, {
      speed: 1,
      speedCurve: values.map(([ratio, speed]) => ({ id: crypto.randomUUID(), offset: ratio * length, speed })),
    });
  }

  function addSpeedPoint(clip: TrackClip) {
    if ((clip.speedCurve?.length ?? 0) >= 8) {
      toast.error("单个片段最多添加 8 个速度点");
      return;
    }
    const sourceOffset = Math.max(0, Math.min(clip.end - clip.start, current - clip.start));
    const speed = clipSpeedAtSource(clip, sourceOffset);
    const point = { id: crypto.randomUUID(), offset: sourceOffset, speed };
    const existing = (clip.speedCurve?.length ?? 0) >= 2
      ? clip.speedCurve!
      : speedCurvePoints(clip).map((item) => ({ ...item, id: crypto.randomUUID() }));
    updateClip(clip.id, {
      speedCurve: [...existing.filter((item) => Math.abs(item.offset - sourceOffset) > 0.02), point].sort((a, b) => a.offset - b.offset),
    });
  }

  function addClip() {
    if (selEnd - selStart < 0.1) {
      toast.error("选区太短,先在时间轴上框选一段");
      return;
    }
    const track = tracks.find((t) => t.id === activeTrackId);
    if (!track) return;
    if (track.locked) {
      toast.error("轨道已锁定,先解锁再添加片段");
      return;
    }
    if (track.type === "text") {
      toast.error("请从左侧“文本”或“字幕”面板添加文字");
      return;
    }
    const length = selEnd - selStart;
    const position = editMode === "append"
      ? track.clips.reduce((end, clip) => Math.max(end, clipPos(clip) + clipLen(clip)), 0)
      : Math.max(0, previewMode === "timeline" ? displayCurrent : sourceToSeq(current) ?? current);
    const added: TrackClip = {
      id: crypto.randomUUID(),
      start: selStart,
      end: selEnd,
      position,
      inputPath: previewPath === inputPath ? undefined : previewPath,
      sourceDuration: duration,
    };
    commitTracks((prev) =>
      prev.map((t) =>
        t.id === track.id
          ? {
              ...t,
              clips: (() => {
                let existing = t.clips;
                if (editMode === "insert") {
                  existing = t.clips.flatMap((clip) => {
                    const left = clipPos(clip);
                    const right = left + clipLen(clip);
                    if (left >= position - 0.001) return [{ ...clip, position: left + length }];
                    if (right <= position + 0.001) return [clip];
                    const sourceOffset = timelineOffsetToSource(clip, position - left);
                    const sourceCut = clip.start + sourceOffset;
                    return [
                      { ...clip, end: sourceCut, speedCurve: slicedSpeedCurve(clip, 0, sourceOffset), keyframes: clip.keyframes?.filter((frame) => frame.offset <= position - left) },
                      {
                        ...clip,
                        id: crypto.randomUUID(),
                        start: sourceCut,
                        position: position + length,
                        speedCurve: slicedSpeedCurve(clip, sourceOffset, clip.end - clip.start),
                        keyframes: clip.keyframes
                          ?.filter((frame) => frame.offset > position - left)
                          .map((frame) => ({ ...frame, offset: frame.offset - (position - left) })),
                      },
                    ];
                  });
                } else if (editMode === "overwrite") {
                  const overwriteEnd = position + length;
                  existing = t.clips.flatMap((clip) => {
                    const left = clipPos(clip);
                    const right = left + clipLen(clip);
                    if (right <= position + 0.001 || left >= overwriteEnd - 0.001) return [clip];
                    const pieces: TrackClip[] = [];
                    if (left < position - 0.001) {
                      const sourceOffset = timelineOffsetToSource(clip, position - left);
                      pieces.push({
                        ...clip,
                        end: clip.start + sourceOffset,
                        speedCurve: slicedSpeedCurve(clip, 0, sourceOffset),
                        keyframes: clip.keyframes?.filter((frame) => frame.offset <= position - left),
                      });
                    }
                    if (right > overwriteEnd + 0.001) {
                      const consumed = overwriteEnd - left;
                      const sourceOffset = timelineOffsetToSource(clip, consumed);
                      pieces.push({
                        ...clip,
                        id: crypto.randomUUID(),
                        start: clip.start + sourceOffset,
                        position: overwriteEnd,
                        speedCurve: slicedSpeedCurve(clip, sourceOffset, clip.end - clip.start),
                        keyframes: clip.keyframes
                          ?.filter((frame) => frame.offset >= consumed)
                          .map((frame) => ({ ...frame, offset: frame.offset - consumed })),
                      });
                    }
                    return pieces;
                  });
                }
                return [...existing, added].sort((a, b) => clipPos(a) - clipPos(b) || a.start - b.start);
              })(),
            }
          : t,
      ),
    );
    setSelectedClipIds([added.id]);
    if (editMode !== "append") setPreviewMode("timeline");
    toast.success(editMode === "insert" ? "已插入片段并后移后续内容" : editMode === "overwrite" ? "已覆盖播放头后的同轨内容" : "片段已添加到轨道末尾");
  }

  function removeClip(trackId: string, clipId: string) {
    if (tracks.find((t) => t.id === trackId)?.locked) {
      toast.error("轨道已锁定,先解锁再删片段");
      return;
    }
    commitTracks((prev) =>
      prev.map((t) => t.id === trackId
        ? { ...t, clips: t.clips.filter((c) => c.id !== clipId) }
        : t),
    );
  }

  // 片段拖动落点提交(时间轴 position;一次拖动一步撤销)
  function moveClip(_trackId: string, clipId: string, position: number) {
    const clicked = tracks.flatMap((track) => track.clips).find((clip) => clip.id === clipId);
    if (!clicked) return;
    const moving = clicked.groupId
      ? tracks.flatMap((track) => track.clips).filter((clip) => clip.groupId === clicked.groupId)
      : [clicked];
    const requestedDelta = position - clipPos(clicked);
    const delta = Math.max(requestedDelta, -Math.min(...moving.map(clipPos)));
    const movingIds = new Set(moving.map((clip) => clip.id));
    commitTracks((prev) => prev.map((track) => track.locked ? track : ({
      ...track,
      clips: track.clips.map((clip) => movingIds.has(clip.id)
        ? { ...clip, position: clipPos(clip) + delta }
        : clip),
    })));
  }

  // 拖动片段左右边缘裁切源区间;左裁同时移动时间轴起点,保持片段内容与位置直觉一致。
  function trimClip(trim: ClipTrimCommit) {
    commitTracks((prev) =>
      prev.map((track) =>
        track.id === trim.trackId
          ? {
              ...track,
              clips: track.clips.map((clip) =>
                clip.id === trim.clipId
                  ? {
                      ...clip,
                      start: trim.start,
                      end: trim.end,
                      position: trim.position,
                    }
                  : clip,
              ),
            }
          : track,
      ),
    );
  }

  // 自动拼接(右键菜单):该轨紧凑排列,消除片段间间隙与重叠
  function compactTrackClips(id: string) {
    if (tracks.find((t) => t.id === id)?.locked) {
      toast.error("轨道已锁定,先解锁再拼接");
      return;
    }
    commitTracks((prev) =>
      prev.map((t) => {
        if (t.id !== id) return t;
        const compacted = compactClips(t.clips);
        return compacted ? { ...t, clips: compacted } : t;
      }),
    );
  }

  // 音轨分离(剪映式):把视频片段的原声以等长片段落到音频轨(同源区间、同时间轴位置),
  // 并只关闭对应视频片段的原声——其他未分离片段不受影响;预览期由隐藏 <audio> 按轨同步播放。
  // 右键单个片段生效;该片段在多选中时,分离多选的全部片段(同一轨)
  function detachAudio(trackId: string, clipId: string) {
    const track = tracks.find((t) => t.id === trackId);
    if (!track || track.type !== "video") return;
    if (track.locked) {
      toast.error("轨道已锁定,先解锁再分离音频");
      return;
    }
    const targets =
      selectedClipIds.includes(clipId)
        ? track.clips.filter((c) => selectedClipIds.includes(c.id))
        : track.clips.filter((c) => c.id === clipId);
    if (!targets.length) return;

    // 音频轨宿主:第一个未锁定音频轨;没有则在最后一条视频轨下方新建
    let audioHost = tracks.find((t) => t.type === "audio" && !t.locked);
    const needNewTrack = !audioHost;
    const newAudio = newTrack("audio", tracks);

    commitTracks((prev) => {
      let next = prev.map((t) => {
        if (t.id === trackId) {
          const targetIds = new Set(targets.map((c) => c.id));
          return {
            ...t,
            clips: t.clips.map((c) =>
              targetIds.has(c.id) ? { ...c, muteOriginal: true } : c,
            ),
          };
        }
        if (needNewTrack) return t;
        if (t.id !== audioHost!.id) return t;
        const exist = new Set(
          t.clips.map((c) => `${clipPos(c).toFixed(3)}~${c.start.toFixed(3)}~${c.end.toFixed(3)}`),
        );
        const additions = targets
          .filter((c) => !exist.has(`${clipPos(c).toFixed(3)}~${c.start.toFixed(3)}~${c.end.toFixed(3)}`))
          .map<TrackClip>((c) => ({
            id: crypto.randomUUID(),
            start: c.start,
            end: c.end,
            position: clipPos(c),
            detachedFrom: c.id,
            inputPath: c.inputPath,
            sourceDuration: c.sourceDuration,
            speed: c.speed,
            speedCurve: c.speedCurve,
          }));
        return additions.length ? { ...t, clips: [...t.clips, ...additions] } : t;
      });
      if (needNewTrack) {
        // 新音频轨:填充分离片段后插到最后一条视频轨下方
        newAudio.clips = targets.map<TrackClip>((c) => ({
          id: crypto.randomUUID(),
          start: c.start,
          end: c.end,
          position: clipPos(c),
          detachedFrom: c.id,
          inputPath: c.inputPath,
          sourceDuration: c.sourceDuration,
          speed: c.speed,
          speedCurve: c.speedCurve,
        }));
        next.push(newAudio);
        const lastVideoIdx = next.reduce((m, t, i) => (t.type === "video" ? i : m), -1);
        next.splice(lastVideoIdx + 1, 0, next.splice(next.indexOf(newAudio), 1)[0]);
      }
      return next;
    });
    setPreviewMode("timeline");
    toast.success(
      targets.length === 1 ? "音频已分离到音频轨" : `已分离 ${targets.length} 个片段的音频`,
    );
  }

  // 合并音频(分离的逆操作):删除该视频轨派生的音频片段,并恢复对应视频片段原声。
  function mergeDetachedAudio(trackId: string) {
    const track = tracks.find((t) => t.id === trackId);
    if (!track || track.type !== "video") return;
    const clipIds = new Set(track.clips.map((c) => c.id));
    const hasDetached = tracks.some(
      (t) => t.type === "audio" && t.clips.some((c) => c.detachedFrom && clipIds.has(c.detachedFrom)),
    );
    if (!hasDetached) {
      toast.info("没有找到该轨道的分离音频");
      return;
    }
    // 兼容旧草稿:旧版分离会把整轨 muted=true,且没有片段级 muteOriginal 标记。
    const legacyTrackMute = track.muted && !track.clips.some((c) => c.muteOriginal);
    commitTracks((prev) =>
      prev.map((t) => {
        if (t.type === "audio") {
          const kept = t.clips.filter((c) => !(c.detachedFrom && clipIds.has(c.detachedFrom)));
          return kept.length === t.clips.length ? t : { ...t, clips: kept };
        }
        if (t.id === trackId) {
          return {
            ...t,
            muted: legacyTrackMute ? false : t.muted,
            clips: t.clips.map((c) =>
              clipIds.has(c.id) && c.muteOriginal
                ? { ...c, muteOriginal: false }
                : c,
            ),
          };
        }
        return t;
      }),
    );
    toast.success("分离音频已合并回视频轨,原声已恢复");
  }

  // 源分辨率(探测失败回退 <video> 元数据)与档位换算:目标 = 短边对齐档位,
  // 不放大(源短边 ≤ 目标时不缩放),宽高取偶(h264 要求)
  const srcDims = mainVideoInfo
    ? { w: mainVideoInfo.width, h: mainVideoInfo.height }
    : videoMeta;
  const RES_SHORT: Record<Exclude<ExportResolution, "original">, number> = {
    "1080p": 1080,
    "720p": 720,
    "480p": 480,
  };
  function resTarget(res: ExportResolution): { width: number; height: number } | null {
    if (res === "original" || !srcDims) return null;
    const n = RES_SHORT[res];
    const short = Math.min(srcDims.w, srcDims.h);
    if (short <= n) return null;
    const k = n / short;
    const even = (x: number) => Math.max(2, Math.round(x / 2) * 2);
    return { width: even(srcDims.w * k), height: even(srcDims.h * k) };
  }
  // 档位副标题:目标实际分辨率 / 无需缩放提示
  function resLabel(res: ExportResolution): string {
    if (res === "original") return srcDims ? `${srcDims.w}×${srcDims.h}` : "—";
    if (!srcDims) return "—";
    const t = resTarget(res);
    return t ? `${t.width}×${t.height}` : `源已 ≤ ${RES_SHORT[res]}P,无需缩放`;
  }

  async function exportVideo() {
    // 视频轨片段按时间轴位置(序列顺序)排序后导出;没有则退回当前选区作为单段。
    // 隐藏轨不参与导出;静音音频轨不混入(隐藏音频轨一并排除)
    const byPos = (a: TrackClip, b: TrackClip) => clipPos(a) - clipPos(b) || a.start - b.start;
    const videoClips = visibleVideoTracks
      .flatMap((track, layer) =>
        track.clips.flatMap((clip) => expandSpeedCurveClip(clip).map((piece) => ({
          ...piece,
          muteOriginal: !!track.muted || !!piece.muteOriginal,
          layer,
        }))),
      )
      .sort(byPos);
    const audioClips = tracks
      .filter((t) => t.type === "audio" && !t.hidden && !t.muted)
      .flatMap((t) => t.clips.flatMap(expandSpeedCurveClip))
      .sort(byPos);
    const textOverlays = tracks
      .filter((track) => track.type === "text" && !track.hidden)
      .flatMap((track) => track.clips)
      .filter((clip) => !!clip.text?.trim())
      .map((clip) => ({
        start: clipPos(clip),
        end: clipPos(clip) + clipLen(clip),
        text: clip.text!.trim(),
        fontSize: clip.fontSize ?? 42,
        color: clip.textColor ?? "#ffffff",
        position: clip.textPosition ?? "bottom",
        x: clip.textX,
        y: clip.textY,
        rotation: clip.textRotation ?? 0,
      }));
    const segs = videoClips.length
      ? videoClips.map(({ start, end, position, inputPath: clipInputPath, transform, muteOriginal: clipMuted, layer, keyframes, speed, transitionIn }) => ({
          start,
          end,
          position,
          inputPath: clipInputPath,
          transform,
          muteOriginal: clipMuted,
          layer,
          keyframes,
          speed,
          transitionIn,
        }))
      : [{ start: selStart, end: selEnd, inputPath: previewPath }];
    if (!videoClips.length && selEnd - selStart >= duration - 0.05) {
      toast.error("选区就是完整视频,先框选要保留的片段");
      return;
    }
    const transition: TransitionInput | undefined =
      transitionKind === "none"
        ? undefined
        : { kind: transitionKind, durationSecs: transitionSecs };
    const jobId = crypto.randomUUID();
    setExportError(null);
    setExportOpen(false);
    try {
      const queuedJob: CreationExportProgress = {
        jobId,
        percent: 0,
        stage: "正在加入导出队列",
        status: "queued",
      };
      setExportJobs((jobs) => [queuedJob, ...jobs]);
      await api.creationStartExport({
        inputPath,
        segments: segs,
        audioSegments: audioClips.map(({ start, end, position, inputPath: clipInputPath, volume, pan, fadeIn, fadeOut, speed }) => ({
          start,
          end,
          position,
          inputPath: clipInputPath,
          volume,
          pan,
          fadeIn,
          fadeOut,
          speed,
        })),
        transition,
        scale: resTarget(exportRes),
        quality: exportQuality,
        muteOriginal,
        textOverlays,
        jobId,
      });
      toast.success("已加入后台导出队列，可以继续编辑");
    } catch (e) {
      const message = String(e);
      setExportJobs((jobs) => jobs.filter((job) => job.jobId !== jobId));
      setExportError(message);
      toast.error("加入导出队列失败");
    }
  }

  async function cancelExport(jobId: string) {
    setExportJobs((jobs) => jobs.map((job) => job.jobId === jobId
      ? { ...job, status: "cancelling", stage: "正在取消…" }
      : job));
    try {
      await api.creationCancelExport(jobId);
    } catch (error) {
      toast.error(`取消失败: ${error}`);
    }
  }

  function dismissExportJob(jobId: string) {
    setExportJobs((jobs) => jobs.filter((job) => job.jobId !== jobId));
    void api.creationDismissExportJob(jobId);
  }

  function dismissFinishedExportJobs() {
    const finished = exportJobs.filter((job) => !["queued", "running", "cancelling"].includes(job.status));
    setExportJobs((jobs) => jobs.filter((job) => ["queued", "running", "cancelling"].includes(job.status)));
    for (const job of finished) void api.creationDismissExportJob(job.jobId);
  }

  const totalClips = tracks.reduce((n, t) => n + t.clips.length, 0);
  const activeTrack = tracks.find((t) => t.id === activeTrackId);
  // 可见视频轨与其原声开关:所有可见视频轨都关闭原声时,导出不带源音频
  const visibleVideoTracks = tracks.filter((t) => t.type === "video" && !t.hidden);
  const muteOriginal =
    visibleVideoTracks.length > 0 && visibleVideoTracks.every((t) => !!t.muted);
  // 序列预览播放序列:可见视频轨片段按时间轴位置排序(视觉顺序即播放顺序)
  const playSeq = visibleVideoTracks
    .flatMap((t) => t.clips)
    .sort((a, b) => clipPos(a) - clipPos(b) || a.start - b.start);
  // 序列总跨度(时间轴模式下播放头归一化基准 = 最靠右片段的右缘)
  const seqSpan = playSeq.reduce((m, c) => Math.max(m, clipPos(c) + clipLen(c)), 0);
  // 编辑画布保留源视频长度,允许把片段向右移动并形成空白区。
  const timelineSpan = Math.max(duration, seqSpan);
  // 源时间 → 序列时间(找包含该源时刻的片段,按其 position 换算);不在任何片段内返回 null
  function sourceToSeq(t: number): number | null {
    const active = playSeq.find((c) => c.id === timelineClipIdRef.current);
    if (
      active &&
      clipSourcePath(active) === previewPathRef.current &&
      t >= active.start - 0.05 &&
      t < active.end
    ) {
      return clipPos(active) + (t - active.start);
    }
    for (const c of playSeq) {
      if (clipSourcePath(c) === previewPathRef.current && t >= c.start - 0.05 && t < c.end) {
        timelineClipIdRef.current = c.id;
        return clipPos(c) + sourceOffsetToTimeline(c, t - c.start);
      }
    }
    return null;
  }
  const previewSequenceTime = previewMode === "timeline"
    ? timelineGapTime ?? sourceToSeq(current)
    : current;
  const displayCurrent = previewMode === "timeline"
    ? (timelineGapTime ?? sourceToSeq(current) ?? 0)
    : current;
  const displayDuration = previewMode === "timeline" ? timelineSpan : duration;
  function addMarker() {
    const time = Math.max(0, Math.min(displayDuration, displayCurrent));
    setMarkers((values) => [...values, {
      id: crypto.randomUUID(),
      time,
      label: `标记 ${values.length + 1}`,
    }].sort((a, b) => a.time - b.time));
  }
  function toggleLoopRange() {
    if (loopRange) {
      setLoopRange(null);
      return;
    }
    const selected = tracks.flatMap((track) => track.clips).filter((clip) => selectedClipIds.includes(clip.id));
    const start = selected.length ? Math.min(...selected.map(clipPos)) : selStart;
    const end = selected.length ? Math.max(...selected.map((clip) => clipPos(clip) + clipLen(clip))) : selEnd;
    if (end - start < 0.05) {
      toast.error("先选择片段或设置有效入点、出点");
      return;
    }
    setLoopRange({ start, end });
    if (previewMode !== "timeline") setPreviewMode("timeline");
  }
  const activeTextClips = previewSequenceTime == null
    ? []
    : tracks
        .filter((track) => track.type === "text" && !track.hidden)
        .flatMap((track) => track.clips)
        .filter(
          (clip) =>
            previewSequenceTime >= clipPos(clip) &&
            previewSequenceTime < clipPos(clip) + clipLen(clip),
        );
  const selectedClip = tracks
    .flatMap((track) => track.clips)
    .find((clip) => selectedClipIds.includes(clip.id));
  const selectedVideoClip = tracks
    .filter((track) => track.type === "video")
    .flatMap((track) => track.clips)
    .find((clip) => selectedClipIds.includes(clip.id));
  const selectedAudioClip = tracks
    .filter((track) => track.type === "audio")
    .flatMap((track) => track.clips)
    .find((clip) => selectedClipIds.includes(clip.id));
  const selectedTextClip = selectedClip?.text !== undefined ? selectedClip : undefined;

  function beginTextTransform(
    event: React.PointerEvent<HTMLElement>,
    clip: TrackClip,
    mode: "move" | "rotate",
  ) {
    if (event.button !== 0) return;
    const track = tracks.find((item) => item.clips.some((candidate) => candidate.id === clip.id));
    if (track?.locked) {
      toast.error("字幕轨已锁定，先解锁再调整文字");
      return;
    }
    const canvasRect = previewCanvasRef.current?.getBoundingClientRect();
    if (!canvasRect?.width || !canvasRect.height) return;
    event.preventDefault();
    event.stopPropagation();
    event.currentTarget.setPointerCapture(event.pointerId);
    const placement = textTransformPreview?.clipId === clip.id
      ? textTransformPreview
      : { clipId: clip.id, ...textCanvasPlacement(clip) };
    const centerX = canvasRect.left + canvasRect.width * placement.x / 100;
    const centerY = canvasRect.top + canvasRect.height * placement.y / 100;
    textGestureRef.current = {
      clipId: clip.id,
      mode,
      startClientX: event.clientX,
      startClientY: event.clientY,
      startPointerAngle: Math.atan2(event.clientY - centerY, event.clientX - centerX) * 180 / Math.PI,
      x: placement.x,
      y: placement.y,
      rotation: placement.rotation,
      canvasRect,
    };
    setSelectedClipIds([clip.id]);
    if (track) setActiveTrackId(track.id);
    setTextTransformPreview(placement);
  }

  function moveTextTransform(event: React.PointerEvent<HTMLElement>) {
    const gesture = textGestureRef.current;
    if (!gesture) return;
    event.preventDefault();
    event.stopPropagation();
    if (gesture.mode === "move") {
      gesture.x = Math.min(100, Math.max(0,
        gesture.x + (event.clientX - gesture.startClientX) / gesture.canvasRect.width * 100,
      ));
      gesture.y = Math.min(100, Math.max(0,
        gesture.y + (event.clientY - gesture.startClientY) / gesture.canvasRect.height * 100,
      ));
      gesture.startClientX = event.clientX;
      gesture.startClientY = event.clientY;
    } else {
      const centerX = gesture.canvasRect.left + gesture.canvasRect.width * gesture.x / 100;
      const centerY = gesture.canvasRect.top + gesture.canvasRect.height * gesture.y / 100;
      const angle = Math.atan2(event.clientY - centerY, event.clientX - centerX) * 180 / Math.PI;
      gesture.rotation = Math.round(gesture.rotation + angle - gesture.startPointerAngle);
      gesture.startPointerAngle = angle;
    }
    setTextTransformPreview({
      clipId: gesture.clipId,
      x: gesture.x,
      y: gesture.y,
      rotation: gesture.rotation,
    });
  }

  function endTextTransform(event: React.PointerEvent<HTMLElement>) {
    const gesture = textGestureRef.current;
    if (!gesture) return;
    event.preventDefault();
    event.stopPropagation();
    textGestureRef.current = null;
    updateClip(gesture.clipId, {
      textX: Number(gesture.x.toFixed(2)),
      textY: Number(gesture.y.toFixed(2)),
      textRotation: ((gesture.rotation % 360) + 360) % 360,
    });
    setTextTransformPreview(null);
  }

  const previewVideoClip = previewMode === "timeline"
    ? timelineGapTime === null
      ? playSeq.find(
          (clip) =>
            clip.id === timelineClipIdRef.current
            && clipSourcePath(clip) === previewPath
            && current >= clip.start - 0.05
            && current < clip.end,
        )
        // 从源片模式刚切入时间轴时 active id 可能尚未提交,按当前源时间补找片段,
        // 保证分离动作后的首帧就关闭视频原声,不会短暂与分离音轨重复播放。
        ?? playSeq.find(
          (clip) =>
            clipSourcePath(clip) === previewPath
            && current >= clip.start - 0.05
            && current < clip.end,
        )
      : undefined
    : selectedVideoClip && clipSourcePath(selectedVideoClip) === previewPath
      ? selectedVideoClip
      : undefined;
  const previewTransform = previewVideoClip
    ? interpolatedTransform(
        previewVideoClip,
        previewMode === "timeline"
          ? (previewSequenceTime ?? clipPos(previewVideoClip))
          : clipPos(previewVideoClip) + Math.max(0, current - previewVideoClip.start),
      )
    : undefined;
  const previewOriginalMuted = previewVideoClip
    ? !!previewVideoClip.muteOriginal
      || !!tracks.find(
        (track) => track.type === "video" && track.clips.some((clip) => clip.id === previewVideoClip.id),
      )?.muted
    : muteOriginal;
  const layeredPreviewClips = visibleVideoTracks.flatMap((track, layer) =>
    track.clips.map((clip) => ({
      clip,
      layer,
      muted: !!track.muted || !!clip.muteOriginal,
    })),
  );
  const previewVideoLayer = layeredPreviewClips.find(
    (item) => item.clip.id === previewVideoClip?.id,
  )?.layer ?? 0;
  const layeredPreviewClipsRef = useRef(layeredPreviewClips);
  layeredPreviewClipsRef.current = layeredPreviewClips;
  const playbackRateRef = useRef(rate);
  playbackRateRef.current = rate;
  function seekFromControls(time: number) {
    if (previewMode === "timeline") seekSequencePosition(time, true);
    else seekExact(time);
  }
  // 预览原声按当前片段所属视频轨控制,多轨时不会因其他轨未静音而重复播放分离音频。
  useEffect(() => {
    if (videoRef.current) videoRef.current.muted = previewOriginalMuted;
  }, [previewOriginalMuted]);

  // 画中画预览元素以主播放器的序列时间为时钟;非活动片段立即隐藏并暂停。
  function syncLayerVideos(seqTime: number | null, shouldPlay: boolean) {
    for (const item of layeredPreviewClipsRef.current) {
      const el = layerVideoRefs.current.get(item.clip.id);
      if (!el) continue;
      const active = previewModeRef.current === "timeline"
        && seqTime !== null
        && item.clip.id !== timelineClipIdRef.current
        && seqTime >= clipPos(item.clip) - 0.03
        && seqTime < clipPos(item.clip) + clipLen(item.clip);
      el.style.opacity = active
        ? String(item.clip.transform?.opacity ?? 1)
        : "0";
      if (!active) {
        if (!el.paused) el.pause();
        continue;
      }
      const sourceOffset = timelineOffsetToSource(item.clip, seqTime - clipPos(item.clip));
      const sourceTime = item.clip.start + sourceOffset;
      el.muted = item.muted;
      el.playbackRate = playbackRateRef.current * clipSpeedAtSource(item.clip, sourceOffset);
      if (Math.abs(el.currentTime - sourceTime) > 0.12) {
        try {
          el.currentTime = sourceTime;
        } catch {
          /* 元数据尚未就绪时下一次同步重试 */
        }
      }
      if (shouldPlay && el.paused) void el.play().catch(() => {});
      if (!shouldPlay && !el.paused) el.pause();
    }
  }

  useEffect(() => {
    syncLayerVideos(previewSequenceTime, playing);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [previewSequenceTime, playing, tracks, rate, previewMode]);

  // 分栏拖动:块间距本身即隐形拖动区(无可见拖动条),指针捕获保证拖出热区不丢事件。
  // 默认上区 58%;三列约 20% / 58% / 22%,左右栏设置可读性下限,中央预览获取主要空间。
  const containerRef = useRef<HTMLDivElement>(null);
  const [topRatio, setTopRatio] = useState(0.58);
  const [leftWidth, setLeftWidth] = useState(260);
  const [rightWidth, setRightWidth] = useState(300);
  const columnWidthsRef = useRef({ left: leftWidth, right: rightWidth });
  columnWidthsRef.current = { left: leftWidth, right: rightWidth };
  const resizing = useRef<"row" | "left" | "right" | null>(null);
  const colDrag = useRef({ startX: 0, startW: 0 });
  // 首次渲染期间父级会短暂出现极小宽度,不能在那一帧锁定百分比,否则左右栏会被压成几十像素。
  // 宽度稳定后初始化;后续窗口明显缩放时按原比例调整,手动拖出的比例也会保留。
  const widthInit = useRef(false);
  const lastContainerWidth = useRef(0);
  useEffect(() => {
    const el = containerRef.current;
    if (!el) return;
    const apply = () => {
      const w = el.getBoundingClientRect().width;
      // 桌面工作台低于 720px 通常只是路由切换/侧栏动画的过渡宽度,等待下一次观察值。
      if (w < 720) return;
      const clampLeft = (value: number) => Math.min(360, Math.max(220, value));
      const clampRight = (value: number) => Math.min(400, Math.max(260, value));
      if (!widthInit.current) {
        widthInit.current = true;
        lastContainerWidth.current = w;
        setLeftWidth(clampLeft(w * 0.2));
        setRightWidth(clampRight(w * 0.22));
        return;
      }
      const previousWidth = lastContainerWidth.current;
      if (previousWidth > 0 && Math.abs(w - previousWidth) >= 24) {
        const scale = w / previousWidth;
        setLeftWidth(clampLeft(columnWidthsRef.current.left * scale));
        setRightWidth(clampRight(columnWidthsRef.current.right * scale));
      }
      lastContainerWidth.current = w;
    };
    apply();
    const ro = new ResizeObserver(apply);
    ro.observe(el);
    return () => ro.disconnect();
  }, []);

  function beginRowDrag(e: React.PointerEvent) {
    e.preventDefault();
    resizing.current = "row";
    (e.target as HTMLElement).setPointerCapture(e.pointerId);
  }

  function beginColDrag(side: "left" | "right") {
    return (e: React.PointerEvent) => {
      e.preventDefault();
      resizing.current = side;
      colDrag.current = {
        startX: e.clientX,
        startW: side === "left" ? leftWidth : rightWidth,
      };
      (e.target as HTMLElement).setPointerCapture(e.pointerId);
    };
  }

  function onSplitDragMove(e: React.PointerEvent) {
    const mode = resizing.current;
    if (!mode) return;
    if (mode === "row") {
      const el = containerRef.current;
      if (!el) return;
      const r = el.getBoundingClientRect();
      setTopRatio(Math.min(0.75, Math.max(0.4, (e.clientY - r.top) / r.height)));
      return;
    }
    const dx = e.clientX - colDrag.current.startX;
    const el = containerRef.current;
    const cw = el ? el.getBoundingClientRect().width : 0;
    if (mode === "left") {
      const maxW = cw > 0 ? Math.min(420, cw * 0.32) : 420;
      setLeftWidth(Math.min(maxW, Math.max(220, colDrag.current.startW + dx)));
    } else {
      const maxW = cw > 0 ? Math.min(460, cw * 0.35) : 460;
      setRightWidth(Math.min(maxW, Math.max(260, colDrag.current.startW - dx)));
    }
  }

  function endSplitDrag() {
    resizing.current = null;
  }

  // Timeline props 稳定化 + DOM 句柄通道:配合 Timeline 的 memo,播放期间不重渲染时间轴
  const timelineRefs = useRef<TimelineRefs>({
    scroll: null,
    lanes: null,
    playhead: null,
    playheadPct: "0%",
    laneWidth: 0,
    viewportWidth: 0,
    playheadTime: 0,
  });
  const playheadTime = previewMode === "timeline"
    ? timelineGapTime ?? sourceToSeq(current) ?? 0
    : current;
  const playheadSpan = previewMode === "timeline" ? timelineSpan : duration;
  timelineRefs.current.playheadPct =
    playheadSpan > 0 ? `${((playheadTime / playheadSpan) * 100).toFixed(6)}%` : "0%";
  timelineRefs.current.playheadTime = playheadTime;

  // 播放头位置 + 跟随滚动:current 高频变化只写 DOM,不走 Timeline 渲染
  useEffect(() => {
    // 播放中由 rAF 循环逐帧驱动:timeupdate 仅 ~4Hz,若在这里写入旧位置会与 rAF
    // 打架,播放头表现为周期性回跳闪烁;本 effect 只负责暂停态 / scrub 落定后的定位。
    // 位置写 transform(合成层移动,零重绘;left 每帧重绘会引发亚像素闪烁)
    if (playing || scrubTarget.current !== null) return;
    const { scroll, lanes, playhead } = timelineRefs.current;
    if (!playhead || duration <= 0) return;
    // 暂停态:音频轨预览元素一并停住
    pauseAllAudio();
    const lanesW = timelineRefs.current.laneWidth || lanes?.offsetWidth || 0;
    // 时间轴预览模式:播放头走序列时间(位置空间);源片模式:走源时间
    const x =
      previewMode === "timeline" && timelineSpan > 0
        ? ((timelineGapTime ?? sourceToSeq(current) ?? 0) / timelineSpan) * lanesW
        : (current / duration) * lanesW;
    playhead.style.transform = `translate3d(${x.toFixed(3)}px,0,0)`;
    // scrub 时间气泡(存在才写,不触发渲染)
    const bubble = playhead.querySelector<HTMLElement>("[data-ph-time]");
    if (bubble) bubble.textContent = fmt(current);
    if (scroll && lanes) {
      if (x < scroll.scrollLeft + 40 || x > scroll.scrollLeft + scroll.clientWidth - 60) {
        scroll.scrollLeft = Math.max(0, x - scroll.clientWidth / 3);
      }
    }
    // zoom 变化会改变通道宽度,px 定位需随之重算
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [current, duration, playing, zoom, previewMode, timelineSpan, timelineGapTime]);

  // 播放中改用 rAF 每帧驱动播放头与跟随滚动:<video> 的 timeupdate 事件只有 ~4Hz,
  // 只靠它移动播放头会一卡一卡。rAF 直写 DOM 不触发 React 渲染(Timeline 已 memo 化),
  // current 状态仍由 timeupdate 低频更新,只喂时间文本等轻量消费者。
  // previewMode / playSeq 经 ref 读取,避免播放中 tracks 微变导致循环反复重建。
  const playSeqRef = useRef(playSeq);
  playSeqRef.current = playSeq;
  const previewModeRef = useRef(previewMode);
  previewModeRef.current = previewMode;
  const tracksRef = useRef(tracks);
  tracksRef.current = tracks;

  // ===== 音轨预览引擎:每个片段一个隐藏 <audio>,因此同一轨可引用不同素材。 =====
  const audioElsRef = useRef<Map<string, HTMLAudioElement>>(new Map());
  const audioContextRef = useRef<AudioContext | null>(null);
  const audioNodesRef = useRef<Map<string, {
    element: HTMLAudioElement;
    source: MediaElementAudioSourceNode;
    gain: GainNode;
    panner: StereoPannerNode;
  }>>(new Map());
  const lastAudioSyncAt = useRef(0);
  function audioPreviewNodes(clipId: string, element: HTMLAudioElement) {
    const cached = audioNodesRef.current.get(clipId);
    if (cached?.element === element) return cached;
    cached?.source.disconnect();
    cached?.gain.disconnect();
    cached?.panner.disconnect();
    const context = audioContextRef.current ?? new AudioContext();
    audioContextRef.current = context;
    const source = context.createMediaElementSource(element);
    const gain = context.createGain();
    const panner = context.createStereoPanner();
    source.connect(gain).connect(panner).connect(context.destination);
    const next = { element, source, gain, panner };
    audioNodesRef.current.set(clipId, next);
    return next;
  }
  // 按序列时间同步各音频片段;同轨重叠片段也按真实混音结果同时播放。
  function syncAudioPlayback(seqT: number | null, isPlaying: boolean) {
    for (const t of tracksRef.current) {
      if (t.type !== "audio") continue;
      for (const clip of t.clips) {
        const el = audioElsRef.current.get(clip.id);
        if (!el) continue;
        const active = isPlaying
          && seqT != null
          && !t.hidden
          && !t.muted
          && seqT >= clipPos(clip) - 0.05
          && seqT < clipPos(clip) + clipLen(clip);
        if (!active) {
          if (!el.paused) el.pause();
          continue;
        }
        const sourceOffset = timelineOffsetToSource(clip, seqT - clipPos(clip));
        const srcT = Math.max(0, clip.start + sourceOffset);
        const elapsed = Math.max(0, seqT - clipPos(clip));
        const remaining = Math.max(0, clipLen(clip) - elapsed);
        const fadeInGain = (clip.fadeIn ?? 0) > 0 ? Math.min(1, elapsed / (clip.fadeIn ?? 0)) : 1;
        const fadeOutGain = (clip.fadeOut ?? 0) > 0 ? Math.min(1, remaining / (clip.fadeOut ?? 0)) : 1;
        const nodes = audioPreviewNodes(clip.id, el);
        nodes.gain.gain.value = Math.max(0, Math.min(2, clip.volume ?? 1)) * fadeInGain * fadeOutGain;
        nodes.panner.pan.value = Math.max(-1, Math.min(1, clip.pan ?? 0));
        if (audioContextRef.current?.state === "suspended") void audioContextRef.current.resume();
        el.muted = false;
        el.volume = 1;
        el.playbackRate = rate * clipSpeedAtSource(clip, sourceOffset);
        if (el.paused) {
          try {
            el.currentTime = srcT;
          } catch {
            /* 未就绪时忽略,下一帧重试 */
          }
          void el.play().catch(() => {});
        } else if (Math.abs(el.currentTime - srcT) > 0.3) {
          try {
            el.currentTime = srcT;
          } catch {
            /* 同上 */
          }
        }
      }
    }
  }

  function pauseAllAudio() {
    for (const el of audioElsRef.current.values()) {
      if (!el.paused) el.pause();
    }
  }
  useEffect(() => () => {
    for (const nodes of audioNodesRef.current.values()) {
      nodes.source.disconnect();
      nodes.gain.disconnect();
      nodes.panner.disconnect();
    }
    audioNodesRef.current.clear();
    void audioContextRef.current?.close();
    audioContextRef.current = null;
  }, []);
  useEffect(() => {
    if (!playing) return;
    let raf = 0;
    const tick = () => {
      raf = requestAnimationFrame(tick);
      const v = videoRef.current;
      if (!v || duration <= 0) return;
      const t = v.currentTime;
      const { playhead, scroll, lanes } = timelineRefs.current;
      if (!lanes) return;
      // 时间轴模式:播放头走序列时间(位置空间);源片模式:走源时间。
      // 跳段瞬间(源时刻不在任何片段内)本帧跳过写入,下一帧落在新片段内
      const inTimeline = previewModeRef.current === "timeline";
      const span = inTimeline
        ? Math.max(
            duration,
            playSeqRef.current.reduce((m, c) => Math.max(m, clipPos(c) + clipLen(c)), 0),
          )
        : duration;
      const seqT = inTimeline ? sourceToSeq(t) : sourceToSeq(t) ?? t;
      if (inTimeline) {
        const active = playSeqRef.current.find((clip) => clip.id === timelineClipIdRef.current);
        if (active) {
          const desiredRate = playbackRateRef.current * clipSpeedAtSource(active, t - active.start);
          if (Math.abs(v.playbackRate - desiredRate) > 0.01) v.playbackRate = desiredRate;
        }
      }
      const loop = loopRangeRef.current;
      if (loop && seqT != null && seqT >= loop.end - 0.02) {
        if (inTimeline) {
          const first = playSeqRef.current.find((clip) => loop.start >= clipPos(clip) - 0.01 && loop.start < clipPos(clip) + clipLen(clip));
          if (first) openTimelineClip(first, true, first.start + Math.max(0, loop.start - clipPos(first)));
        } else {
          seekExact(loop.start);
        }
        return;
      }
      // 源片和时间轴预览都按可推导出的序列位置播放音频,与导出混音口径一致。
      const now = performance.now();
      if (now - lastAudioSyncAt.current >= 50) {
        lastAudioSyncAt.current = now;
        syncAudioPlayback(seqT, true);
        syncLayerVideos(seqT, true);
      }
      if (seqT == null || span <= 0) return;
      // GPU 子像素定位:全览长视频时每帧位移远小于 1px,保留小数才能连续移动。
      const laneWidth = timelineRefs.current.laneWidth || lanes.offsetWidth;
      const x = (seqT / span) * laneWidth;
      if (playhead) {
        playhead.style.transform = `translate3d(${x.toFixed(3)}px,0,0)`;
        const bubble = playhead.querySelector<HTMLElement>("[data-ph-time]");
        if (bubble) bubble.textContent = fmt(t);
      }
      if (scroll) {
        // 进入右缘跟拍区后,每帧把播放头锚回 1/3 处 = 时间轴连续滑动(丝滑跟随)
        const viewportWidth = timelineRefs.current.viewportWidth || scroll.clientWidth;
        if (x < scroll.scrollLeft + 40 || x > scroll.scrollLeft + viewportWidth - 60) {
          scroll.scrollLeft = Math.max(0, x - viewportWidth / 3);
        }
      }
      // 时间轴序列预览:跳段规则与 handleTimeUpdate 一致,但逐帧执行,段边界不拖沓
      if (inTimeline) {
        const seq = playSeqRef.current;
        const idx = seq.findIndex(
          (c) =>
            c.id === timelineClipIdRef.current &&
            clipSourcePath(c) === previewPathRef.current &&
            t >= c.start - 0.05 &&
            t < c.end,
        );
        if (idx >= 0) {
          if (t >= seq[idx].end - 0.04) {
            const next = seq[idx + 1];
            const clipRight = clipPos(seq[idx]) + clipLen(seq[idx]);
            if (next && clipPos(next) <= clipRight + 0.05) {
              openTimelineClip(next, true);
            } else {
              v.pause();
              setTimelineGapTime(clipRight);
            }
          }
        } else {
          v.pause();
        }
      }
    };
    raf = requestAnimationFrame(tick);
    return () => cancelAnimationFrame(raf);
  }, [playing, duration]);

  const timelineState = useMemo<TimelineState>(
    () => ({
      tracks,
      activeTrackId,
      duration: timelineSpan,
      selStart,
      selEnd,
      fileName: baseName(inputPath),
      selectedClipIds,
      zoom,
      trackHeight,
      waveLevel,
      thumbUrls: thumbRels.map(mediaFileUrl),
      thumbSourcePath: previewPath,
      defaultInputPath: inputPath,
      audioPeaks,
      externalAudioPeaks,
      markers,
      snapEnabled,
      fps: videoInfo?.fps ?? 30,
      transitionKind,
      transitionSecs,
    }),
    [tracks, activeTrackId, timelineSpan, selStart, selEnd, inputPath, previewPath, selectedClipIds, zoom, trackHeight, waveLevel, thumbRels, mediaFileUrl, audioPeaks, externalAudioPeaks, markers, snapEnabled, videoInfo?.fps, transitionKind, transitionSecs],
  );
  // actions 引用的函数内部均基于 commitTracks(prev) 或 ref / setState 工作,
  // tracks 不变时冻结引用是安全的;tracks 变化即重新生成
  const timelineActions = useMemo<TimelineActions>(
    () => ({
      onSeek: (t) => previewMode === "timeline" ? seekSequencePosition(t, false) : seek(t),
      onOpenClip: (clip) => openTimelineClip(clip),
      onSelection: (s, e) => {
        setSelStart(s);
        setSelEnd(e);
      },
      onScrubChange: handleScrubChange,
      onSelectTrack: setActiveTrackId,
      onRemoveTrack: removeTrack,
      onRemoveClip: removeClip,
      onSelectClip: selectClip,
      onMoveTrack: moveTrack,
      onToggleFlag: toggleTrackFlag,
      onZoomChange: changeZoom,
      onContextMenu: setCtxMenu,
      onCommitClipMove: moveClip,
      onCommitClipTrim: trimClip,
      onRemoveMarker: (id) => setMarkers((values) => values.filter((marker) => marker.id !== id)),
      onRenameTrack: (id) => {
        const t = tracks.find((x) => x.id === id);
        if (!t) return;
        setRenameName(t.name);
        setRenameTarget(id);
      },
    }),
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [tracks, activeTrackId, previewMode],
  );

  return (
    <div
      ref={containerRef}
      className="relative flex h-full min-h-0 w-full flex-col gap-2.5"
      onContextMenuCapture={(e) => e.preventDefault()}
    >
      {mediaInitState !== "ready" && (
        <div className="absolute inset-0 z-[100] flex items-center justify-center rounded-lg bg-background">
          <div className="flex max-w-sm flex-col items-center px-6 text-center">
            {mediaInitState === "loading" ? (
              <>
                <div className="flex size-12 items-center justify-center rounded-2xl border border-primary/20 bg-primary/10 text-primary">
                  {showMediaInitProgress
                    ? <Loader2 className="size-5 animate-spin" />
                    : <Clapperboard className="size-5" />}
                </div>
                <h2 className="mt-4 text-sm font-medium text-foreground">正在打开视频</h2>
                <p className="mt-1.5 text-xs leading-5 text-muted-foreground">
                  正在读取视频信息并创建主轨道，大文件可能需要稍等片刻。
                </p>
                <span className="mt-3 max-w-full truncate font-mono text-[10px] text-muted-foreground/70">
                  {baseName(inputPath)}
                </span>
              </>
            ) : (
              <>
                <div className="flex size-12 items-center justify-center rounded-2xl border border-destructive/20 bg-destructive/10 text-destructive">
                  <AlertTriangle className="size-5" />
                </div>
                <h2 className="mt-4 text-sm font-medium text-foreground">视频打开超时</h2>
                <p className="mt-1.5 text-xs leading-5 text-muted-foreground">
                  视频可能过大、文件已损坏，或当前编码暂不受支持。
                </p>
                <div className="mt-4 flex items-center gap-2">
                  <Button type="button" variant="outline" size="sm" onClick={onBack}>返回</Button>
                  <Button type="button" size="sm" onClick={() => void importVideo()}>重新选择</Button>
                </div>
              </>
            )}
          </div>
        </div>
      )}
      {/* 上:左 片段列表 / 中 预览 / 右 属性面板,三块各自独立成卡片(高度 = topRatio;块间距即隐形拖动区) */}
      <div className="flex min-h-0 shrink-0 gap-2.5" style={{ height: `${topRatio * 100}%` }}>
        {/* 左:片段列表(头部带 打开 / 返回首页 入口) */}
        <div
          className="flex shrink-0 flex-col rounded-lg border border-border"
          style={{ width: leftWidth }}
        >
          <div className="flex items-center border-b border-border px-2.5 py-1.5">
            <span className="text-[11px] text-muted-foreground">
              素材面板
            </span>
            <span className="ml-auto flex items-center gap-1">
              <SimpleTooltip content="打开视频">
                <button
                  type="button"
                  onClick={() => void importVideo()}
                  className="inline-flex size-6 items-center justify-center rounded-full text-muted-foreground transition-colors hover:bg-accent hover:text-foreground"
                >
                  <FolderInput className="size-3.5" />
                </button>
              </SimpleTooltip>
              <SimpleTooltip content="返回首页">
                <button
                  type="button"
                  onClick={onBack}
                  className="inline-flex size-6 items-center justify-center rounded-full text-muted-foreground transition-colors hover:bg-accent hover:text-foreground"
                >
                  <House className="size-3.5" />
                </button>
              </SimpleTooltip>
            </span>
          </div>
          <MaterialPanel
            tracks={tracks}
            onRemoveClip={removeClip}
            onImport={() => void importVideoAssets()}
            onImportAudio={() => void importAudio()}
            onAddText={addTextClip}
            transitionKind={transitionKind}
            onTransitionKind={setTransitionKind}
            onExportAgain={() => setExportOpen(true)}
            onApplyVideoStyle={(patch) => {
              if (selectedVideoClip) updateVideoTransform(selectedVideoClip.id, patch);
              else toast.info("请先在时间轴选中一个视频片段");
            }}
          />
        </div>
        {/* 左/中 间距:隐形拖动区(负 margin 让热区正好覆盖 10px 间距,不占额外布局宽度) */}
        <div
          className="-mx-2.5 w-2.5 shrink-0 cursor-col-resize touch-none"
          onPointerDown={beginColDrag("left")}
          onPointerMove={onSplitDragMove}
          onPointerUp={endSplitDrag}
          onPointerCancel={endSplitDrag}
        />
        {/* 中:预览(黑场衬底 + 播放控制条) */}
        <div className="flex min-w-0 flex-1 flex-col overflow-hidden rounded-lg border border-border">
          <div className="flex items-center justify-between border-b border-border px-3 py-2 text-xs font-medium text-foreground">
            <span className="flex items-center gap-2">
              播放器
              <span className="font-normal text-muted-foreground">
                {preparingProxyPath === previewPath
                  ? "正在准备流畅预览…"
                  : proxyBySource[previewPath]
                    ? "流畅预览"
                    : "原始素材"}
              </span>
            </span>
            {/* 源片 / 时间轴序列预览切换(时间轴模式按可见视频轨片段连播,跳过非片段区间) */}
            <span className="flex gap-1">
              {(
                [
                  ["source", "源片"],
                  ["timeline", "时间轴"],
                ] as const
              ).map(([mode, label]) => (
                <button
                  key={mode}
                  type="button"
                  onClick={() => switchPreviewMode(mode)}
                  className={`rounded-md border px-2 py-0.5 text-[11px] font-normal transition-colors ${
                    previewMode === mode
                      ? "border-primary bg-primary/10 text-primary"
                      : "border-border text-muted-foreground hover:bg-accent hover:text-foreground"
                  }`}
                >
                  {label}
                </button>
              ))}
            </span>
          </div>
          {/* 预览区:黑场衬底,视频按预览区可用空间等比缩放铺满(保持宽高比、居中;点击播放 / 暂停) */}
          <div
            ref={previewCanvasRef}
            className="relative flex min-h-0 flex-1 cursor-pointer items-center justify-center overflow-hidden bg-black/40 p-3"
            onClick={togglePlay}
          >
            <video
              ref={videoRef}
              src={mediaFileUrl(activePreviewMediaPath)}
              preload="auto"
              className={`max-h-full max-w-full rounded-md bg-black transition-[transform,opacity] duration-150 ${
                previewMode === "timeline" && timelineGapTime !== null
                  ? "pointer-events-none opacity-0"
                  : "opacity-100"
              }`}
              style={{
                position: "relative",
                zIndex: 10 + previewVideoLayer,
                clipPath: `inset(${previewTransform?.cropTop ?? 0}% ${previewTransform?.cropRight ?? 0}% ${previewTransform?.cropBottom ?? 0}% ${previewTransform?.cropLeft ?? 0}%)`,
                transform: `translate(${(previewTransform?.positionX ?? 0) / 2}%, ${(previewTransform?.positionY ?? 0) / 2}%) rotate(${previewTransform?.rotation ?? 0}deg) scale(${previewTransform?.scale ?? 1})`,
                filter: previewColorFilter(previewTransform),
              }}
              onPlay={() => setPlaying(true)}
              onPause={() => setPlaying(false)}
              onTimeUpdate={(e) => handleTimeUpdate(e.currentTarget.currentTime)}
              onLoadedMetadata={(e) => {
                const d = e.currentTarget.duration;
                const pending = pendingMediaSeek.current;
                const active = tracks.flatMap((track) => track.clips).find((clip) => clip.id === timelineClipIdRef.current);
                e.currentTarget.playbackRate = rate * (previewMode === "timeline" && active ? clipSpeedAtSource(active, (pending?.time ?? current) - active.start) : 1);
                setDuration(d);
                if (previewPath === inputPath) seedInitialVideoClip(d);
                if (!pending?.preserveSelection) {
                  setSelStart(0);
                  setSelEnd(d);
                }
                if (previewPath === inputPath && activePreviewMediaPath === inputPath) {
                  setVideoMeta({
                    w: e.currentTarget.videoWidth,
                    h: e.currentTarget.videoHeight,
                  });
                }
                if (pending) {
                  pendingMediaSeek.current = null;
                  e.currentTarget.currentTime = Math.min(Math.max(0, pending.time), d);
                  setCurrent(e.currentTarget.currentTime);
                  if (pending.play) void e.currentTarget.play();
                }
              }}
            />
            {/* 其余同时段视频按轨道顺序叠加在主播放器上,构成画中画预览。 */}
            {layeredPreviewClips.map(({ clip, layer, muted }) => {
              const active = previewMode === "timeline"
                && previewSequenceTime !== null
                && clip.id !== timelineClipIdRef.current
                && previewSequenceTime >= clipPos(clip) - 0.03
                && previewSequenceTime < clipPos(clip) + clipLen(clip);
              const animated = interpolatedTransform(clip, previewSequenceTime ?? clipPos(clip));
              return (
                <video
                  key={`layer-${clip.id}`}
                  ref={(el) => {
                    if (el) layerVideoRefs.current.set(clip.id, el);
                    else layerVideoRefs.current.delete(clip.id);
                  }}
                  src={mediaFileUrl(clip.inputPath ?? inputPath)}
                  preload="metadata"
                  muted={muted}
                  className="pointer-events-none absolute max-h-[calc(100%-1.5rem)] max-w-[calc(100%-1.5rem)] rounded-md bg-transparent transition-opacity duration-100"
                  style={{
                    zIndex: 10 + layer,
                    opacity: active ? (animated.opacity ?? 1) : 0,
                    clipPath: `inset(${animated.cropTop ?? 0}% ${animated.cropRight ?? 0}% ${animated.cropBottom ?? 0}% ${animated.cropLeft ?? 0}%)`,
                    transform: `translate(${(animated.positionX ?? 0) / 2}%, ${(animated.positionY ?? 0) / 2}%) rotate(${animated.rotation ?? 0}deg) scale(${animated.scale ?? 1})`,
                    filter: previewColorFilter(animated),
                  }}
                  onLoadedMetadata={() => syncLayerVideos(previewSequenceTime, playing)}
                />
              );
            })}
            {activeTextClips.map((clip) => {
              const selected = selectedClipIds.includes(clip.id);
              const placement = textTransformPreview?.clipId === clip.id
                ? textTransformPreview
                : { clipId: clip.id, ...textCanvasPlacement(clip) };
              return (
                <div
                  key={clip.id}
                  className={`absolute z-[1000] max-w-[90%] touch-none select-none text-center font-semibold drop-shadow-[0_2px_3px_rgba(0,0,0,0.9)] ${
                    selected ? "cursor-move" : "cursor-pointer"
                  }`}
                  style={{
                    left: `${placement.x}%`,
                    top: `${placement.y}%`,
                    color: clip.textColor ?? "#ffffff",
                    fontSize: `${Math.max(14, (clip.fontSize ?? 42) * 0.55)}px`,
                    transform: `translate(-50%, -50%) rotate(${placement.rotation}deg)`,
                  }}
                  onClick={(event) => event.stopPropagation()}
                  onPointerDown={(event) => beginTextTransform(event, clip, "move")}
                  onPointerMove={moveTextTransform}
                  onPointerUp={endTextTransform}
                  onPointerCancel={endTextTransform}
                >
                  {selected && (
                    <>
                      <span className="pointer-events-none absolute -inset-1.5 rounded border border-primary" />
                      <span className="pointer-events-none absolute bottom-full left-1/2 h-6 border-l border-primary" />
                      <button
                        type="button"
                        aria-label="旋转文字"
                        title="拖动旋转文字"
                        className="absolute bottom-[calc(100%+1.25rem)] left-1/2 flex size-5 -translate-x-1/2 items-center justify-center rounded-full border border-primary bg-background text-primary shadow-sm"
                        onPointerDown={(event) => beginTextTransform(event, clip, "rotate")}
                        onPointerMove={moveTextTransform}
                        onPointerUp={endTextTransform}
                        onPointerCancel={endTextTransform}
                        onClick={(event) => event.stopPropagation()}
                      >
                        <RotateCw className="size-3" />
                      </button>
                    </>
                  )}
                  <span className="whitespace-pre-wrap rounded bg-black/20 px-2 py-1">{clip.text}</span>
                </div>
              );
            })}
            {/* 每个音频片段持有独立媒体元素,同一工程可以混用主视频原声与外部音乐。 */}
            {tracks
              .filter((t) => t.type === "audio")
              .flatMap((t) =>
                t.clips.map((clip) => (
                  <audio
                    key={clip.id}
                    ref={(el) => {
                      if (el) audioElsRef.current.set(clip.id, el);
                      else {
                        audioElsRef.current.delete(clip.id);
                        const nodes = audioNodesRef.current.get(clip.id);
                        if (nodes) {
                          nodes.element.pause();
                          nodes.gain.gain.value = 0;
                        }
                      }
                    }}
                    src={mediaFileUrl(clip.inputPath ?? inputPath)}
                    preload="auto"
                    className="hidden"
                  />
                )),
              )}
            {!playing && (
              <span className="pointer-events-none absolute z-[1100] inline-flex size-14 items-center justify-center rounded-full bg-black/50 text-white">
                <Play className="size-6 fill-current" />
              </span>
            )}
          </div>
          {/* 播放器控制条:回到开头 / 快退 5s / 播放暂停 / 快进 5s / 到结尾 + 时间 */}
          <div className="flex h-10 shrink-0 items-center justify-center gap-1 border-t border-border">
            <SimpleTooltip content="回到开头">
              <Button
                variant="ghost"
                size="icon"
                className="size-7"
                onClick={() => seekFromControls(0)}
              >
                <SkipBack className="size-4" />
              </Button>
            </SimpleTooltip>
            <SimpleTooltip content="快退 5 秒">
              <Button
                variant="ghost"
                size="icon"
                className="size-7"
                onClick={() => seekFromControls(Math.max(0, displayCurrent - 5))}
              >
                <Rewind className="size-4" />
              </Button>
            </SimpleTooltip>
            <SimpleTooltip content={playing ? "暂停" : "播放"}>
              <Button variant="ghost" size="icon" className="size-8" onClick={togglePlay}>
                {playing ? <Pause className="size-4" /> : <Play className="size-4" />}
              </Button>
            </SimpleTooltip>
            <SimpleTooltip content="快进 5 秒">
              <Button
                variant="ghost"
                size="icon"
                className="size-7"
                onClick={() => seekFromControls(Math.min(displayDuration, displayCurrent + 5))}
              >
                <FastForward className="size-4" />
              </Button>
            </SimpleTooltip>
            <SimpleTooltip content="到结尾">
              <Button
                variant="ghost"
                size="icon"
                className="size-7"
                onClick={() => seekFromControls(displayDuration)}
              >
                <SkipForward className="size-4" />
              </Button>
            </SimpleTooltip>
            <span className="mx-2 font-mono text-xs tabular-nums text-muted-foreground">
              {fmt(displayCurrent)} / {fmt(displayDuration)}
            </span>
          </div>
        </div>
        {/* 中/右 间距:隐形拖动区 */}
        <div
          className="-mx-2.5 w-2.5 shrink-0 cursor-col-resize touch-none"
          onPointerDown={beginColDrag("right")}
          onPointerMove={onSplitDragMove}
          onPointerUp={endSplitDrag}
          onPointerCancel={endSplitDrag}
        />
        {/* 右:属性面板(视频信息 / 选区 / 播放倍速 / 工程) */}
        <div
          className="flex shrink-0 flex-col overflow-hidden rounded-lg border border-border"
          style={{ width: rightWidth }}
        >
          <div className="border-b border-border px-3 py-2 text-xs font-medium text-foreground">
            属性
          </div>
          <div className="veltrix-editor-scrollbar flex min-h-0 flex-1 flex-col gap-4 overflow-y-auto p-3 text-xs">
            <section className="flex flex-col gap-1.5">
              <div className="flex items-center justify-between">
                <span className="text-[11px] text-muted-foreground">草稿</span>
                <button
                  type="button"
                  onClick={() => {
                    setNameDraft(draftName);
                    setSettingsOpen(true);
                  }}
                  className="rounded px-1.5 py-0.5 text-[11px] text-muted-foreground transition-colors hover:bg-accent hover:text-foreground"
                >
                  修改
                </button>
              </div>
              <div className="flex items-center justify-between gap-2">
                <span className="shrink-0 text-muted-foreground">名称</span>
                <span className="truncate" title={draftName}>
                  {draftName}
                </span>
              </div>
              <div className="flex items-center justify-between gap-2">
                <span className="shrink-0 text-muted-foreground">源文件</span>
                <span className="truncate" title={inputPath}>
                  {baseName(inputPath)}
                </span>
              </div>
              <div className="flex items-center justify-between">
                <span className="text-muted-foreground">分辨率</span>
                <span className="font-mono tabular-nums">
                  {videoInfo
                    ? `${videoInfo.width} × ${videoInfo.height}`
                    : videoMeta
                      ? `${videoMeta.w} × ${videoMeta.h}`
                      : "—"}
                </span>
              </div>
              <div className="flex items-center justify-between">
                <span className="text-muted-foreground">更新于</span>
                <span className="font-mono tabular-nums">
                  {lastSavedAt ? format(lastSavedAt * 1000, "yyyy-MM-dd HH:mm") : "—"}
                </span>
              </div>
            </section>
            <section className="flex flex-col gap-1.5">
              <span className="text-[11px] text-muted-foreground">视频</span>
              <div className="flex items-center justify-between">
                <span className="text-muted-foreground">时长</span>
                <span className="font-mono tabular-nums">{fmt(duration)}</span>
              </div>
              <div className="flex items-center justify-between">
                <span className="text-muted-foreground">当前</span>
                <span className="font-mono tabular-nums">{fmt(current)}</span>
              </div>
              <div className="flex items-center justify-between">
                <span className="text-muted-foreground">帧率</span>
                <span className="font-mono tabular-nums">
                  {videoInfo ? `${videoInfo.fps.toFixed(2)} fps` : "—"}
                </span>
              </div>
              <div className="flex items-center justify-between">
                <span className="text-muted-foreground">码率</span>
                <span className="font-mono tabular-nums">
                  {videoInfo?.bitrateKbps ? `${videoInfo.bitrateKbps} kb/s` : "—"}
                </span>
              </div>
              <div className="flex items-center justify-between">
                <span className="text-muted-foreground">编码</span>
                <span className="font-mono">
                  {videoInfo
                    ? [videoInfo.videoCodec, videoInfo.audioCodec]
                        .filter(Boolean)
                        .join(" / ") || "—"
                    : "—"}
                </span>
              </div>
            </section>
            {selectedAudioClip && (
              <section className="flex flex-col gap-2 border-t border-border pt-3">
                <div className="flex items-center justify-between">
                  <span className="inline-flex items-center gap-1.5 text-[11px] text-muted-foreground"><AudioLines className="size-3.5" />音频混音</span>
                  <button
                    type="button"
                    onClick={() => updateClip(selectedAudioClip.id, { volume: 1, pan: 0, fadeIn: 0, fadeOut: 0 })}
                    className="text-[10px] text-muted-foreground hover:text-foreground"
                  >重置</button>
                </div>
                <label className="flex items-center gap-2">
                  <span className="w-12 shrink-0 text-muted-foreground">音量</span>
                  <input type="range" min={0} max={200} step={1} value={Math.round((selectedAudioClip.volume ?? 1) * 100)} onChange={(event) => updateClip(selectedAudioClip.id, { volume: Number(event.target.value) / 100 })} className="h-1 min-w-0 flex-1 cursor-pointer accent-primary" />
                  <span className="w-10 text-right font-mono text-[10px] tabular-nums">{Math.round((selectedAudioClip.volume ?? 1) * 100)}%</span>
                </label>
                <label className="flex items-center gap-2">
                  <span className="w-12 shrink-0 text-muted-foreground">声像</span>
                  <input type="range" min={-100} max={100} step={1} value={Math.round((selectedAudioClip.pan ?? 0) * 100)} onChange={(event) => updateClip(selectedAudioClip.id, { pan: Number(event.target.value) / 100 })} className="h-1 min-w-0 flex-1 cursor-pointer accent-primary" />
                  <span className="w-10 text-right font-mono text-[10px] tabular-nums">{(selectedAudioClip.pan ?? 0) === 0 ? "居中" : (selectedAudioClip.pan ?? 0) < 0 ? `左${Math.abs(Math.round((selectedAudioClip.pan ?? 0) * 100))}` : `右${Math.round((selectedAudioClip.pan ?? 0) * 100)}`}</span>
                </label>
                {(["fadeIn", "fadeOut"] as const).map((key) => {
                  const maximum = Math.max(0, clipLen(selectedAudioClip));
                  const value = Math.min(maximum, selectedAudioClip[key] ?? 0);
                  return (
                    <label key={key} className="flex items-center gap-2">
                      <span className="w-12 shrink-0 text-muted-foreground">{key === "fadeIn" ? "淡入" : "淡出"}</span>
                      <input type="range" min={0} max={maximum} step={0.05} value={value} onChange={(event) => updateClip(selectedAudioClip.id, { [key]: Number(event.target.value) })} className="h-1 min-w-0 flex-1 cursor-pointer accent-primary" />
                      <span className="w-10 text-right font-mono text-[10px] tabular-nums">{value.toFixed(1)}s</span>
                    </label>
                  );
                })}
                <p className="text-[10px] leading-relaxed text-muted-foreground">音量、左右声像与淡入淡出会同步用于预览和最终导出。</p>
              </section>
            )}
            {selectedVideoClip && (
              <section className="flex flex-col gap-2 border-t border-border pt-3">
                <div className="flex items-center justify-between">
                  <span className="inline-flex items-center gap-1.5 text-[11px] text-muted-foreground"><Crop className="size-3.5" />画面变换</span>
                  <button
                    type="button"
                    onClick={() => updateClip(selectedVideoClip.id, { transform: undefined, keyframes: [] })}
                    className="rounded px-1.5 py-0.5 text-[10px] text-muted-foreground hover:bg-accent hover:text-foreground"
                  >
                    重置
                  </button>
                </div>
                <div className="flex items-center gap-2">
                  <span className="w-12 shrink-0 text-muted-foreground">速度</span>
                  <div className="grid min-w-0 flex-1 grid-cols-6 gap-1">
                    {([0.25, 0.5, 1, 1.5, 2, 4] as const).map((speed) => (
                      <button
                        key={speed}
                        type="button"
                        onClick={() => {
                          const timelineLength = (selectedVideoClip.end - selectedVideoClip.start) / speed;
                          updateClip(selectedVideoClip.id, {
                            speed,
                            speedCurve: undefined,
                            keyframes: selectedVideoClip.keyframes?.map((frame) => ({ ...frame, offset: Math.min(frame.offset, timelineLength) })),
                          });
                        }}
                        className={`rounded border py-1 font-mono text-[9px] ${(selectedVideoClip.speed ?? 1) === speed ? "border-primary bg-primary/10 text-primary" : "border-border text-muted-foreground hover:bg-accent"}`}
                      >{speed}×</button>
                    ))}
                  </div>
                </div>
                <div className="mt-1 flex items-center justify-between border-t border-border pt-3">
                  <span className="text-[11px] text-muted-foreground">速度曲线</span>
                  <span className="flex items-center gap-1">
                    <button type="button" onClick={() => addSpeedPoint(selectedVideoClip)} className="rounded bg-primary/10 px-1.5 py-0.5 text-[10px] text-primary hover:bg-primary/20">当前位置加点</button>
                    <button type="button" onClick={() => updateClip(selectedVideoClip.id, { speedCurve: undefined, speed: 1 })} className="text-[10px] text-muted-foreground hover:text-foreground">清除</button>
                  </span>
                </div>
                <div className="grid grid-cols-4 gap-1">
                  {([[
                    "montage", "蒙太奇",
                  ], ["bullet", "子弹时间"], ["speedUp", "渐快"], ["slowDown", "渐慢"]] as const).map(([preset, label]) => (
                    <button key={preset} type="button" onClick={() => applySpeedCurvePreset(selectedVideoClip, preset)} className="rounded border border-border py-1 text-[9px] text-muted-foreground hover:border-primary hover:text-primary">{label}</button>
                  ))}
                </div>
                {(selectedVideoClip.speedCurve?.length ?? 0) > 0 && (
                  <div className="flex flex-col gap-1 rounded border border-border p-1.5">
                    {[...(selectedVideoClip.speedCurve ?? [])].sort((a, b) => a.offset - b.offset).map((point) => (
                      <div key={point.id} className="flex items-center gap-1.5">
                        <input
                          type="number"
                          min={0}
                          max={selectedVideoClip.end - selectedVideoClip.start}
                          step={0.1}
                          value={Number(point.offset.toFixed(2))}
                          title="速度点在源片段中的时间"
                          onChange={(event) => updateClip(selectedVideoClip.id, {
                            speedCurve: selectedVideoClip.speedCurve
                              ?.map((item) => item.id === point.id ? { ...item, offset: Math.max(0, Math.min(selectedVideoClip.end - selectedVideoClip.start, Number(event.target.value))) } : item)
                              .sort((a, b) => a.offset - b.offset),
                          })}
                          className="h-5 w-12 rounded border border-input bg-background px-1 font-mono text-[9px] outline-none"
                        />
                        <input type="range" min={25} max={400} step={5} value={Math.round(point.speed * 100)} onChange={(event) => updateClip(selectedVideoClip.id, {
                          speedCurve: selectedVideoClip.speedCurve?.map((item) => item.id === point.id ? { ...item, speed: Number(event.target.value) / 100 } : item),
                        })} className="h-1 min-w-0 flex-1 cursor-pointer accent-primary" />
                        <span className="w-8 text-right font-mono text-[9px]">{point.speed.toFixed(2)}×</span>
                        <button type="button" aria-label="删除速度点" onClick={() => {
                          const next = selectedVideoClip.speedCurve?.filter((item) => item.id !== point.id) ?? [];
                          updateClip(selectedVideoClip.id, { speedCurve: next.length >= 2 ? next : undefined });
                        }} className="text-muted-foreground hover:text-destructive"><Trash2 className="size-3" /></button>
                      </div>
                    ))}
                  </div>
                )}
                <div className="mt-1 flex items-center justify-between border-t border-border pt-3">
                  <span className="text-[11px] text-muted-foreground">与前一片段</span>
                  <button type="button" onClick={() => updateClip(selectedVideoClip.id, { transitionIn: undefined })} className="text-[10px] text-muted-foreground hover:text-foreground">跟随全局</button>
                </div>
                <div className="grid grid-cols-4 gap-1">
                  {([[
                    "inherit", "全局",
                  ], ["none", "硬切"], ["dissolve", "叠化"], ["fade", "淡黑"]] as const).map(([kind, label]) => {
                    const active = kind === "inherit"
                      ? !selectedVideoClip.transitionIn
                      : selectedVideoClip.transitionIn?.kind === kind;
                    return (
                      <button
                        key={kind}
                        type="button"
                        onClick={() => updateClip(selectedVideoClip.id, {
                          transitionIn: kind === "inherit" ? undefined : { kind, durationSecs: kind === "none" ? 0 : selectedVideoClip.transitionIn?.durationSecs || 0.5 },
                        })}
                        className={`rounded border py-1 text-[9px] ${active ? "border-primary bg-primary/10 text-primary" : "border-border text-muted-foreground hover:bg-accent"}`}
                      >{label}</button>
                    );
                  })}
                </div>
                {selectedVideoClip.transitionIn && selectedVideoClip.transitionIn.kind !== "none" && (
                  <label className="flex items-center gap-2">
                    <span className="w-12 shrink-0 text-muted-foreground">时长</span>
                    <input type="range" min={0.1} max={Math.min(2, Math.max(0.1, clipLen(selectedVideoClip) / 2))} step={0.05} value={selectedVideoClip.transitionIn.durationSecs} onChange={(event) => updateClip(selectedVideoClip.id, { transitionIn: { ...selectedVideoClip.transitionIn!, durationSecs: Number(event.target.value) } })} className="h-1 min-w-0 flex-1 cursor-pointer accent-primary" />
                    <span className="w-10 text-right font-mono text-[10px]">{selectedVideoClip.transitionIn.durationSecs.toFixed(2)}s</span>
                  </label>
                )}
                <div className="flex items-center gap-2">
                  <span className="w-12 shrink-0 text-muted-foreground">旋转</span>
                  <div className="grid min-w-0 flex-1 grid-cols-4 gap-1">
                    {([0, 90, 180, 270] as const).map((rotation) => (
                      <button
                        key={rotation}
                        type="button"
                        title={`旋转 ${rotation}°`}
                        onClick={() => updateVideoTransform(selectedVideoClip.id, { rotation })}
                        className={`rounded border py-1 font-mono text-[10px] ${
                          (selectedVideoClip.transform?.rotation ?? 0) === rotation
                            ? "border-primary bg-primary/10 text-primary"
                            : "border-border text-muted-foreground hover:bg-accent"
                        }`}
                      >
                        {rotation === 0 ? "原始" : `${rotation}°`}
                      </button>
                    ))}
                  </div>
                  <RotateCw className="size-3.5 text-muted-foreground" />
                </div>
                <label className="flex items-center gap-2">
                  <span className="w-12 shrink-0 text-muted-foreground">缩放</span>
                  <input
                    type="range"
                    min={25}
                    max={300}
                    step={1}
                    value={Math.round((selectedVideoClip.transform?.scale ?? 1) * 100)}
                    onChange={(e) => updateVideoTransform(selectedVideoClip.id, { scale: Number(e.target.value) / 100 })}
                    className="h-1 min-w-0 flex-1 cursor-pointer accent-primary"
                  />
                  <span className="w-9 text-right font-mono tabular-nums">{Math.round((selectedVideoClip.transform?.scale ?? 1) * 100)}%</span>
                </label>
                <label className="flex items-center gap-2">
                  <span className="w-12 shrink-0 text-muted-foreground">透明度</span>
                  <input
                    type="range"
                    min={0}
                    max={100}
                    step={1}
                    value={Math.round((selectedVideoClip.transform?.opacity ?? 1) * 100)}
                    onChange={(e) => updateVideoTransform(selectedVideoClip.id, { opacity: Number(e.target.value) / 100 })}
                    className="h-1 min-w-0 flex-1 cursor-pointer accent-primary"
                  />
                  <span className="w-9 text-right font-mono tabular-nums">{Math.round((selectedVideoClip.transform?.opacity ?? 1) * 100)}%</span>
                </label>
                {(["positionX", "positionY"] as const).map((axis) => (
                  <label key={axis} className="flex items-center gap-2">
                    <span className="w-12 shrink-0 text-muted-foreground">{axis === "positionX" ? "水平" : "垂直"}</span>
                    <input
                      type="range"
                      min={-100}
                      max={100}
                      step={1}
                      value={selectedVideoClip.transform?.[axis] ?? 0}
                      onChange={(e) => updateVideoTransform(selectedVideoClip.id, { [axis]: Number(e.target.value) })}
                      className="h-1 min-w-0 flex-1 cursor-pointer accent-primary"
                    />
                    <span className="w-9 text-right font-mono tabular-nums">{selectedVideoClip.transform?.[axis] ?? 0}</span>
                  </label>
                ))}
                <div className="flex items-center gap-1.5 text-[11px] text-muted-foreground"><Move className="size-3.5" />位置范围按画布百分比调整</div>
                <div className="grid grid-cols-2 gap-2">
                  {([
                    ["cropTop", "上裁剪", "cropBottom"],
                    ["cropRight", "右裁剪", "cropLeft"],
                    ["cropBottom", "下裁剪", "cropTop"],
                    ["cropLeft", "左裁剪", "cropRight"],
                  ] as const).map(([side, label, opposite]) => (
                    <label key={side} className="flex flex-col gap-1">
                      <span className="flex justify-between text-[10px] text-muted-foreground"><span>{label}</span><span className="font-mono">{selectedVideoClip.transform?.[side] ?? 0}%</span></span>
                      <input
                        type="range"
                        min={0}
                        max={Math.max(0, 90 - (selectedVideoClip.transform?.[opposite] ?? 0))}
                        step={1}
                        value={selectedVideoClip.transform?.[side] ?? 0}
                        onChange={(e) => updateVideoTransform(selectedVideoClip.id, { [side]: Number(e.target.value) })}
                        className="h-1 cursor-pointer accent-primary"
                      />
                    </label>
                  ))}
                </div>
                <div className="mt-1 flex items-center justify-between border-t border-border pt-3">
                  <span className="text-[11px] text-muted-foreground">滤镜</span>
                  <button type="button" onClick={() => updateVideoTransform(selectedVideoClip.id, { filter: "none" })} className="text-[10px] text-muted-foreground hover:text-foreground">清除</button>
                </div>
                <div className="grid grid-cols-3 gap-1">
                  {([
                    ["none", "原片"], ["vivid", "鲜明"], ["cinema", "电影"],
                    ["warm", "暖阳"], ["cool", "清冷"], ["mono", "黑白"],
                  ] as const).map(([filter, label]) => (
                    <button
                      key={filter}
                      type="button"
                      onClick={() => updateVideoTransform(selectedVideoClip.id, { filter })}
                      className={`rounded border py-1.5 text-[10px] transition-colors ${(selectedVideoClip.transform?.filter ?? "none") === filter ? "border-primary bg-primary/10 text-primary" : "border-border text-muted-foreground hover:bg-accent"}`}
                    >{label}</button>
                  ))}
                </div>
                <div className="mt-1 flex items-center justify-between border-t border-border pt-3">
                  <span className="text-[11px] text-muted-foreground">色彩调节</span>
                  <button type="button" onClick={() => updateVideoTransform(selectedVideoClip.id, { brightness: 0, contrast: 1, saturation: 1, temperature: 0, hue: 0 })} className="text-[10px] text-muted-foreground hover:text-foreground">重置</button>
                </div>
                {([
                  ["brightness", "亮度", -100, 100, Math.round((selectedVideoClip.transform?.brightness ?? 0) * 100), 100],
                  ["contrast", "对比度", 0, 200, Math.round((selectedVideoClip.transform?.contrast ?? 1) * 100), 100],
                  ["saturation", "饱和度", 0, 200, Math.round((selectedVideoClip.transform?.saturation ?? 1) * 100), 100],
                  ["temperature", "色温", -100, 100, Math.round((selectedVideoClip.transform?.temperature ?? 0) * 100), 100],
                  ["hue", "色相", -180, 180, Math.round(selectedVideoClip.transform?.hue ?? 0), 1],
                ] as const).map(([key, label, min, max, value, divisor]) => (
                  <label key={key} className="flex items-center gap-2">
                    <span className="w-12 shrink-0 text-muted-foreground">{label}</span>
                    <input type="range" min={min} max={max} step={1} value={value} onChange={(event) => updateVideoTransform(selectedVideoClip.id, { [key]: Number(event.target.value) / divisor })} className="h-1 min-w-0 flex-1 cursor-pointer accent-primary" />
                    <span className="w-9 text-right font-mono text-[10px] tabular-nums">{value}</span>
                  </label>
                ))}
                <div className="mt-1 flex items-center justify-between border-t border-border pt-3">
                  <span className="text-[11px] text-muted-foreground">关键帧动画</span>
                  <button type="button" onClick={() => addVideoKeyframe(selectedVideoClip)} className="inline-flex items-center gap-1 rounded bg-primary/10 px-1.5 py-1 text-[10px] text-primary hover:bg-primary/20"><SquarePlus className="size-3" />当前位置</button>
                </div>
                {(selectedVideoClip.keyframes ?? []).length === 0 ? (
                  <p className="text-[10px] leading-relaxed text-muted-foreground">先在当前位置添加首个关键帧；之后移动播放头再调整缩放或位置，会自动生成新关键帧并平滑过渡。</p>
                ) : (
                  <div className="flex flex-col gap-1">
                    {(selectedVideoClip.keyframes ?? []).map((frame) => (
                      <div key={frame.id} className="flex items-center gap-1.5 rounded border border-border px-2 py-1 text-[10px]">
                        <button type="button" onClick={() => { setPreviewMode("timeline"); seekSequencePosition(clipPos(selectedVideoClip) + frame.offset, true); }} className="font-mono text-primary hover:underline">{fmt(frame.offset)}</button>
                        <span className="truncate text-muted-foreground">缩放 {Math.round(frame.scale * 100)}% · X {Math.round(frame.positionX)} · Y {Math.round(frame.positionY)}</span>
                        <select
                          value={frame.easing ?? "linear"}
                          title="到达此关键帧前的缓动"
                          onChange={(event) => updateClip(selectedVideoClip.id, {
                            keyframes: (selectedVideoClip.keyframes ?? []).map((item) => item.id === frame.id
                              ? { ...item, easing: event.target.value as NonNullable<typeof item.easing> }
                              : item),
                          })}
                          className="h-5 rounded border border-input bg-background px-1 text-[9px] outline-none"
                        >
                          <option value="linear">线性</option>
                          <option value="easeIn">缓入</option>
                          <option value="easeOut">缓出</option>
                          <option value="easeInOut">缓入缓出</option>
                        </select>
                        <button type="button" aria-label="删除关键帧" onClick={() => removeVideoKeyframe(selectedVideoClip, frame.id)} className="ml-auto text-muted-foreground hover:text-destructive"><Trash2 className="size-3" /></button>
                      </div>
                    ))}
                  </div>
                )}
              </section>
            )}
            {selectedTextClip && (
              <section className="flex flex-col gap-2 border-t border-border pt-3">
                <span className="text-[11px] text-muted-foreground">字幕属性</span>
                <textarea
                  value={selectedTextClip.text ?? ""}
                  rows={3}
                  maxLength={500}
                  onChange={(e) => updateClip(selectedTextClip.id, { text: e.target.value })}
                  className="resize-y rounded-md border border-input bg-transparent px-2 py-1.5 text-xs outline-none focus:border-ring"
                />
                <label className="flex items-center gap-2">
                  <span className="w-12 shrink-0 text-muted-foreground">字号</span>
                  <input
                    type="range"
                    min={18}
                    max={96}
                    step={1}
                    value={selectedTextClip.fontSize ?? 42}
                    onChange={(e) => updateClip(selectedTextClip.id, { fontSize: Number(e.target.value) })}
                    className="h-1 min-w-0 flex-1 cursor-pointer accent-primary"
                  />
                  <span className="w-7 text-right font-mono tabular-nums">{selectedTextClip.fontSize ?? 42}</span>
                </label>
                <label className="flex items-center gap-2">
                  <span className="w-12 shrink-0 text-muted-foreground">颜色</span>
                  <input
                    type="color"
                    value={selectedTextClip.textColor ?? "#ffffff"}
                    onChange={(e) => updateClip(selectedTextClip.id, { textColor: e.target.value })}
                    className="h-7 w-10 cursor-pointer rounded border border-input bg-transparent p-0.5"
                  />
                  <span className="font-mono text-[11px] uppercase text-muted-foreground">
                    {selectedTextClip.textColor ?? "#ffffff"}
                  </span>
                </label>
                <div className="flex items-center gap-2">
                  <span className="w-12 shrink-0 text-muted-foreground">位置</span>
                  <div className="flex min-w-0 flex-1 gap-1">
                    {(["top", "center", "bottom"] as const).map((position) => (
                      <button
                        key={position}
                        type="button"
                        onClick={() => updateClip(selectedTextClip.id, {
                          textPosition: position,
                          textX: 50,
                          textY: position === "top" ? 10 : position === "center" ? 50 : 90,
                        })}
                        className={`flex-1 rounded border px-1 py-1 text-[10px] ${
                          (selectedTextClip.textPosition ?? "bottom") === position
                            ? "border-primary bg-primary/10 text-primary"
                            : "border-border text-muted-foreground hover:bg-accent"
                        }`}
                      >
                        {position === "top" ? "顶部" : position === "center" ? "居中" : "底部"}
                      </button>
                    ))}
                  </div>
                </div>
                <label className="flex items-center gap-2">
                  <span className="w-12 shrink-0 text-muted-foreground">水平</span>
                  <input
                    type="range"
                    min={0}
                    max={100}
                    step={1}
                    value={Math.round(textCanvasPlacement(selectedTextClip).x)}
                    onChange={(event) => updateClip(selectedTextClip.id, { textX: Number(event.target.value) })}
                    className="h-1 min-w-0 flex-1 cursor-pointer accent-primary"
                  />
                  <span className="w-8 text-right font-mono tabular-nums">{Math.round(textCanvasPlacement(selectedTextClip).x)}%</span>
                </label>
                <label className="flex items-center gap-2">
                  <span className="w-12 shrink-0 text-muted-foreground">垂直</span>
                  <input
                    type="range"
                    min={0}
                    max={100}
                    step={1}
                    value={Math.round(textCanvasPlacement(selectedTextClip).y)}
                    onChange={(event) => updateClip(selectedTextClip.id, { textY: Number(event.target.value) })}
                    className="h-1 min-w-0 flex-1 cursor-pointer accent-primary"
                  />
                  <span className="w-8 text-right font-mono tabular-nums">{Math.round(textCanvasPlacement(selectedTextClip).y)}%</span>
                </label>
                <label className="flex items-center gap-2">
                  <span className="w-12 shrink-0 text-muted-foreground">旋转</span>
                  <input
                    type="range"
                    min={-180}
                    max={180}
                    step={1}
                    value={Math.round((((selectedTextClip.textRotation ?? 0) + 180) % 360 + 360) % 360 - 180)}
                    onChange={(event) => updateClip(selectedTextClip.id, { textRotation: Number(event.target.value) })}
                    className="h-1 min-w-0 flex-1 cursor-pointer accent-primary"
                  />
                  <span className="w-9 text-right font-mono tabular-nums">{Math.round(selectedTextClip.textRotation ?? 0)}°</span>
                </label>
                <label className="flex items-center gap-2">
                  <span className="w-12 shrink-0 text-muted-foreground">时长</span>
                  <Input
                    type="number"
                    min={0.1}
                    max={3600}
                    step={0.1}
                    value={Number(clipLen(selectedTextClip).toFixed(1))}
                    onChange={(e) => {
                      const length = Math.max(0.1, Number(e.target.value) || 0.1);
                      updateClip(selectedTextClip.id, { end: selectedTextClip.start + length });
                    }}
                    className="h-7 flex-1 text-xs"
                  />
                  <span className="text-muted-foreground">秒</span>
                </label>
              </section>
            )}
            <section className="flex flex-col gap-1.5">
              <span className="text-[11px] text-muted-foreground">选区</span>
              <div className="flex items-center justify-between">
                <span className="text-muted-foreground">入点</span>
                <span className="font-mono tabular-nums">{fmt(selStart)}</span>
              </div>
              <div className="flex items-center justify-between">
                <span className="text-muted-foreground">出点</span>
                <span className="font-mono tabular-nums">{fmt(selEnd)}</span>
              </div>
              <div className="flex items-center justify-between">
                <span className="text-muted-foreground">时长</span>
                <span className="font-mono tabular-nums">
                  {fmt(Math.max(0, selEnd - selStart))}
                </span>
              </div>
            </section>
            <section className="flex flex-col gap-1.5">
              <span className="text-[11px] text-muted-foreground">播放倍速</span>
              <div className="flex flex-wrap gap-1">
                {[0.5, 0.75, 1, 1.25, 1.5, 2, 3].map((r) => (
                  <button
                    key={r}
                    type="button"
                    onClick={() => setRate(r)}
                    className={`rounded-md border px-2 py-1 font-mono text-[11px] tabular-nums transition-colors ${
                      r === rate
                        ? "border-primary bg-primary/10 text-primary"
                        : "border-border text-muted-foreground hover:bg-accent hover:text-foreground"
                    }`}
                  >
                    {r}x
                  </button>
                ))}
              </div>
            </section>
            <section className="flex flex-col gap-1.5">
              <span className="text-[11px] text-muted-foreground">工程</span>
              <div className="flex items-center justify-between">
                <span className="text-muted-foreground">激活轨道</span>
                <span>{activeTrack?.name ?? "—"}</span>
              </div>
              <div className="flex items-center justify-between">
                <span className="text-muted-foreground">轨道 / 片段</span>
                <span className="font-mono tabular-nums">
                  {tracks.length} / {totalClips}
                </span>
              </div>
            </section>
            <section className="flex flex-col gap-1.5">
              <span className="text-[11px] text-muted-foreground">导出</span>
              <div className="flex items-center justify-between gap-2">
                <span className="shrink-0 text-muted-foreground">参数</span>
                <button
                  type="button"
                  onClick={() => setExportOpen(true)}
                  className="truncate rounded px-1.5 py-0.5 text-[11px] text-muted-foreground transition-colors hover:bg-accent hover:text-foreground"
                >
                  {exportRes === "original" ? "原画" : `短边 ${RES_SHORT[exportRes]}P`} ·{" "}
                  {exportQuality === "high" ? "高" : exportQuality === "low" ? "低" : "中"}画质 ·{" "}
                  {transitionKind === "none"
                    ? "无转场"
                    : transitionKind === "dissolve"
                      ? "叠化"
                      : "淡黑"}
                </button>
              </div>
              <Button
                size="sm"
                className="mt-1 h-7 text-xs"
                disabled={duration <= 0}
                onClick={() => setExportOpen(true)}
              >
                <Download className="size-3.5" />
                {totalClips > 0 ? `导出视频(${totalClips} 段)` : "导出视频"}
                {exporting && <span className="rounded-full bg-primary-foreground/15 px-1.5 text-[10px]">后台</span>}
              </Button>
            </section>
          </div>
        </div>
      </div>

      {/* 上/下 间距:隐形拖动区(无可见拖动条;负 margin 让热区正好覆盖 10px 间距) */}
      <div
        className="-my-2.5 h-2.5 w-full shrink-0 cursor-row-resize touch-none"
        onPointerDown={beginRowDrag}
        onPointerMove={onSplitDragMove}
        onPointerUp={endSplitDrag}
        onPointerCancel={endSplitDrag}
      />

      {/* 下:时间轴区(图标工具行 + 多轨道时间轴)一整张卡片,占剩余高度;纵向 flex 让滚动容器撑满全高,
          横向滚动条落在整个时间轴区的最底部(而不是紧跟通道内容) */}
      <div className="flex min-h-0 flex-1 flex-col overflow-hidden rounded-lg border border-border">
        <div className="flex items-center gap-1 border-b border-border px-3 py-1">
          {/* 编辑工具(剪映式):选择下拉(选择 A / 分割 B / 左右全选 [ ])+ 撤销重做 + 分割 + 删除选中 */}
          <DropdownMenu>
            <SimpleTooltip content="编辑工具(选择 A / 分割 B / 向左全选 [ / 向右全选 ])">
              <DropdownMenuTrigger asChild>
                <Button variant="ghost" size="icon" className="size-7">
                  <MousePointer2 className="size-3.5" />
                </Button>
              </DropdownMenuTrigger>
            </SimpleTooltip>
            <DropdownMenuContent align="start">
              <DropdownMenuItem onClick={() => setSelectedClipIds([])}>
                <MousePointer2 className="size-3.5" />
                选择
                <span className="ml-auto pl-4 text-xs text-muted-foreground">A</span>
              </DropdownMenuItem>
              <DropdownMenuItem onClick={splitAtPlayhead}>
                <Scissors className="size-3.5" />
                分割
                <span className="ml-auto pl-4 text-xs text-muted-foreground">B</span>
              </DropdownMenuItem>
              <DropdownMenuItem onClick={() => selectClipsSide("left")}>
                <ArrowLeftToLine className="size-3.5" />
                向左全选
                <span className="ml-auto pl-4 text-xs text-muted-foreground">[</span>
              </DropdownMenuItem>
              <DropdownMenuItem onClick={() => selectClipsSide("right")}>
                <ArrowRightToLine className="size-3.5" />
                向右全选
                <span className="ml-auto pl-4 text-xs text-muted-foreground">]</span>
              </DropdownMenuItem>
            </DropdownMenuContent>
          </DropdownMenu>
          <SimpleTooltip content="撤销(Ctrl+Z)">
            <Button
              variant="ghost"
              size="icon"
              className="size-7"
              disabled={histCounts.past === 0}
              onClick={undo}
            >
              <Undo2 className="size-3.5" />
            </Button>
          </SimpleTooltip>
          <SimpleTooltip content="重做(Ctrl+Shift+Z)">
            <Button
              variant="ghost"
              size="icon"
              className="size-7"
              disabled={histCounts.future === 0}
              onClick={redo}
            >
              <Redo2 className="size-3.5" />
            </Button>
          </SimpleTooltip>
          <SimpleTooltip content="分割播放头处片段(B)">
            <Button variant="ghost" size="icon" className="size-7" onClick={splitAtPlayhead}>
              <Scissors className="size-3.5" />
            </Button>
          </SimpleTooltip>
          <SimpleTooltip content="智能切分(OpenCV 场景检测,仅切分当前素材片段)">
            <Button
              variant="ghost"
              size="icon"
              className="size-7"
              disabled={detecting}
              onClick={() => void smartSplit()}
            >
              {detecting ? (
                <Loader2 className="size-3.5 animate-spin" />
              ) : (
                <ScanLine className="size-3.5" />
              )}
            </Button>
          </SimpleTooltip>
          <SimpleTooltip content="删除选中片段并保留空隙(Delete)">
            <Button
              variant="ghost"
              size="icon"
              className="size-7"
              disabled={selectedClipIds.length === 0}
              onClick={() => deleteSelectedClips(false)}
            >
              <Trash2 className="size-3.5" />
            </Button>
          </SimpleTooltip>
          <SimpleTooltip content="波纹删除并左移后续片段(Shift+Delete)">
            <Button
              variant="ghost"
              size="icon"
              className="size-7"
              disabled={selectedClipIds.length === 0}
              onClick={() => deleteSelectedClips(true)}
            >
              <ListCollapse className="size-3.5" />
            </Button>
          </SimpleTooltip>
          <SimpleTooltip content={linkedSelection ? "链接选择已开启：视频与分离音频一起选中" : "链接选择已关闭：可单独选择视频或音频"}>
            <Button
              variant={linkedSelection ? "secondary" : "ghost"}
              size="icon"
              className="size-7"
              onClick={() => setLinkedSelection((value) => !value)}
            >
              {linkedSelection ? <Link2 className="size-3.5" /> : <Unplug className="size-3.5" />}
            </Button>
          </SimpleTooltip>
          <SimpleTooltip content={snapEnabled ? "磁性吸附已开启：对齐帧、播放头、标记和片段边缘" : "磁性吸附已关闭"}>
            <Button variant={snapEnabled ? "secondary" : "ghost"} size="icon" className="size-7" onClick={() => setSnapEnabled((value) => !value)}>
              <Magnet className="size-3.5" />
            </Button>
          </SimpleTooltip>
          <DropdownMenu>
            <SimpleTooltip content="片段编组">
              <DropdownMenuTrigger asChild>
                <Button variant="ghost" size="icon" className="size-7" disabled={selectedClipIds.length === 0}>
                  <Combine className="size-3.5" />
                </Button>
              </DropdownMenuTrigger>
            </SimpleTooltip>
            <DropdownMenuContent align="start">
              <DropdownMenuItem disabled={selectedClipIds.length < 2} onClick={groupSelectedClips}>
                <Combine className="size-3.5" />编组选中片段
              </DropdownMenuItem>
              <DropdownMenuItem onClick={ungroupSelectedClips}>
                <Unplug className="size-3.5" />解除编组
              </DropdownMenuItem>
            </DropdownMenuContent>
          </DropdownMenu>
          <SimpleTooltip content="在播放头添加标记(M；Shift+点击标记删除)">
            <Button variant="ghost" size="icon" className="size-7" onClick={addMarker}>
              <Flag className="size-3.5" />
            </Button>
          </SimpleTooltip>
          <SimpleTooltip content={loopRange ? `循环 ${fmt(loopRange.start)} - ${fmt(loopRange.end)}` : "循环所选片段或入出点"}>
            <Button variant={loopRange ? "secondary" : "ghost"} size="icon" className="size-7" onClick={toggleLoopRange}>
              <Repeat2 className="size-3.5" />
            </Button>
          </SimpleTooltip>
          <SimpleTooltip content="设为入点">
            <Button variant="ghost" size="icon" className="size-7" onClick={setIn}>
              <PanelLeftDashed className="size-3.5" />
            </Button>
          </SimpleTooltip>
          <SimpleTooltip content="设为出点">
            <Button variant="ghost" size="icon" className="size-7" onClick={setOut}>
              <PanelRightDashed className="size-3.5" />
            </Button>
          </SimpleTooltip>
          <SimpleTooltip content={`添加片段到「${activeTrack?.name ?? "—"}」(当前选区)`}>
            <Button variant="ghost" size="icon" className="size-7" onClick={addClip}>
              <SquarePlus className="size-3.5" />
            </Button>
          </SimpleTooltip>
          <div className="ml-1 inline-flex h-7 items-center rounded-md border border-border bg-muted/30 p-0.5">
            {([[
              "append", "追加",
            ], ["insert", "插入"], ["overwrite", "覆盖"]] as const).map(([mode, label]) => (
              <button
                key={mode}
                type="button"
                onClick={() => setEditMode(mode)}
                className={`h-5 rounded px-1.5 text-[9px] transition-colors ${editMode === mode ? "bg-background text-foreground shadow-sm" : "text-muted-foreground hover:text-foreground"}`}
              >{label}</button>
            ))}
          </div>
          <DropdownMenu>
            <SimpleTooltip content="添加轨道">
              <DropdownMenuTrigger asChild>
                <Button variant="ghost" size="icon" className="size-7">
                  <ListPlus className="size-3.5" />
                </Button>
              </DropdownMenuTrigger>
            </SimpleTooltip>
            <DropdownMenuContent align="start">
              <DropdownMenuItem onClick={() => addTrack("video")}>
                <Clapperboard className="size-3.5" />
                视频轨
              </DropdownMenuItem>
              <DropdownMenuItem onClick={() => addTrack("audio")}>
                <AudioWaveform className="size-3.5" />
                音频轨
              </DropdownMenuItem>
              <DropdownMenuItem onClick={() => addTrack("text")}>
                <Captions className="size-3.5" />
                字幕轨
              </DropdownMenuItem>
            </DropdownMenuContent>
          </DropdownMenu>
          {/* 时间轴缩放(靠右):− / 滑块 / +(Ctrl+滚轮同效);⋮ = 轨道高度 / 波形占比 */}
          <div className="ml-auto flex items-center gap-1">
          <SimpleTooltip content="缩小时间轴">
            <Button
              variant="ghost"
              size="icon"
              className="size-7"
              disabled={zoom <= 1}
              onClick={() => changeZoom(zoom / 1.25)}
            >
              <ZoomOut className="size-3.5" />
            </Button>
          </SimpleTooltip>
          <input
            type="range"
            min={1}
            max={20}
            step={0.25}
            value={zoom}
            onChange={(e) => changeZoom(Number(e.target.value))}
            className="h-1 w-20 cursor-pointer accent-primary"
          />
          <SimpleTooltip content="放大时间轴">
            <Button
              variant="ghost"
              size="icon"
              className="size-7"
              disabled={zoom >= 20}
              onClick={() => changeZoom(zoom * 1.25)}
            >
              <ZoomIn className="size-3.5" />
            </Button>
          </SimpleTooltip>
          <DropdownMenu>
            <SimpleTooltip content="时间轴显示设置(轨道高度 / 波形占比)">
              <DropdownMenuTrigger asChild>
                <Button variant="ghost" size="icon" className="size-7">
                  <MoreVertical className="size-3.5" />
                </Button>
              </DropdownMenuTrigger>
            </SimpleTooltip>
            <DropdownMenuContent align="end" className="w-64 p-3">
              <div className="flex items-center gap-2">
                <span className="shrink-0 text-xs text-muted-foreground">整体高度</span>
                <input
                  type="range"
                  min={40}
                  max={128}
                  step={4}
                  value={trackHeight}
                  onChange={(e) => setTrackHeight(Number(e.target.value))}
                  className="h-1 min-w-0 flex-1 cursor-pointer accent-primary"
                />
              </div>
              <div className="mt-3 flex items-center gap-2">
                <span className="shrink-0 text-xs text-muted-foreground">波形占比</span>
                <div className="flex gap-1">
                  {([0, 1, 2] as const).map((lv) => (
                    <button
                      key={lv}
                      type="button"
                      onClick={() => setWaveLevel(lv)}
                      className={`inline-flex size-7 items-center justify-center rounded-md border transition-colors ${
                        waveLevel === lv
                          ? "border-primary bg-primary/10 text-primary"
                          : "border-border text-muted-foreground hover:bg-accent hover:text-foreground"
                      }`}
                    >
                      <AudioLines
                        className={lv === 0 ? "size-3" : lv === 1 ? "size-3.5" : "size-4"}
                      />
                    </button>
                  ))}
                </div>
              </div>
            </DropdownMenuContent>
          </DropdownMenu>
          </div>
        </div>
        {/* 时间轴与工具栏留 2px 间距(pt-0.5),刻度尺不贴死分隔线 */}
        <div className="min-h-0 flex-1 px-3 pt-0.5 pb-3">
          <Timeline state={timelineState} actions={timelineActions} refs={timelineRefs.current} />
        </div>
      </div>

      {/* 草稿设置对话框:名称可改;源文件 / 分辨率 / 时长 / 保存位置只读 */}
      <Dialog open={settingsOpen} onOpenChange={setSettingsOpen}>
        <DialogContent className="sm:max-w-md">
          <DialogHeader>
            <DialogTitle>草稿设置</DialogTitle>
          </DialogHeader>
          <div className="flex flex-col gap-3 text-xs">
            <div className="flex items-center gap-3">
              <span className="w-16 shrink-0 text-muted-foreground">草稿名称</span>
              <Input
                value={nameDraft}
                onChange={(e) => setNameDraft(e.target.value)}
                className="h-8 flex-1 text-xs"
                maxLength={50}
              />
            </div>
            <div className="flex items-start gap-3">
              <span className="w-16 shrink-0 pt-1 text-muted-foreground">源文件</span>
              <span className="break-all pt-1 font-mono text-[11px] text-muted-foreground">
                {inputPath}
              </span>
            </div>
            <div className="flex items-center gap-3">
              <span className="w-16 shrink-0 text-muted-foreground">分辨率</span>
              <span className="font-mono tabular-nums">
                {videoInfo
                  ? `${videoInfo.width} × ${videoInfo.height}`
                  : videoMeta
                    ? `${videoMeta.w} × ${videoMeta.h}`
                    : "—"}
              </span>
            </div>
            <div className="flex items-center gap-3">
              <span className="w-16 shrink-0 text-muted-foreground">帧率</span>
              <span className="font-mono tabular-nums">
                {videoInfo ? `${videoInfo.fps.toFixed(2)} fps` : "—"}
              </span>
            </div>
            <div className="flex items-center gap-3">
              <span className="w-16 shrink-0 text-muted-foreground">码率</span>
              <span className="font-mono tabular-nums">
                {videoInfo?.bitrateKbps ? `${videoInfo.bitrateKbps} kb/s` : "—"}
              </span>
            </div>
            <div className="flex items-center gap-3">
              <span className="w-16 shrink-0 text-muted-foreground">时长</span>
              <span className="font-mono tabular-nums">{fmt(duration)}</span>
            </div>
            <div className="flex items-center gap-3">
              <span className="w-16 shrink-0 text-muted-foreground">保存位置</span>
              <span className="text-muted-foreground">本机草稿库(首页可恢复继续剪辑)</span>
            </div>
          </div>
          <DialogFooter>
            <Button variant="outline" size="sm" onClick={() => setSettingsOpen(false)}>
              取消
            </Button>
            <Button
              size="sm"
              onClick={() => {
                const name = nameDraft.trim();
                if (name) {
                  setDraftName(name);
                  toast.success("草稿名称已更新");
                }
                setSettingsOpen(false);
              }}
            >
              保存
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      {/* 导出参数对话框:分辨率 / 画质 / 转场,确认后开始导出 */}
      <Dialog open={exportOpen} onOpenChange={setExportOpen}>
        <DialogContent className="sm:max-w-md">
          <DialogHeader>
            <DialogTitle>导出视频</DialogTitle>
          </DialogHeader>
          <div className="flex flex-col gap-4 text-xs">
            {/* 分辨率:短边档位,不放大(源短边 ≤ 目标时显示「无需缩放」) */}
            <section className="flex flex-col gap-2">
              <span className="text-muted-foreground">分辨率</span>
              <div className="flex flex-col gap-1.5">
                {(
                  [
                    ["original", "原画"],
                    ["1080p", "1080P"],
                    ["720p", "720P"],
                    ["480p", "480P"],
                  ] as const
                ).map(([value, label]) => (
                  <button
                    key={value}
                    type="button"
                    onClick={() => setExportRes(value)}
                    className={`flex items-center justify-between rounded-md border px-3 py-2 transition-colors ${
                      exportRes === value
                        ? "border-primary bg-primary/10"
                        : "border-border hover:border-primary/40 hover:bg-accent/40"
                    }`}
                  >
                    <span>{label}</span>
                    <span className="font-mono tabular-nums text-[11px] text-muted-foreground">
                      {resLabel(value)}
                    </span>
                  </button>
                ))}
              </div>
            </section>
            {/* 画质:CRF 档位;流拷贝路径(原画 + 无转场 + 无音频叠加)不受影响 */}
            <section className="flex flex-col gap-2">
              <span className="text-muted-foreground">画质</span>
              <div className="flex gap-1">
                {(
                  [
                    ["high", "高"],
                    ["medium", "中"],
                    ["low", "低"],
                  ] as const
                ).map(([value, label]) => (
                  <button
                    key={value}
                    type="button"
                    onClick={() => setExportQuality(value)}
                    className={`flex-1 rounded-md border px-2 py-1.5 text-center transition-colors ${
                      exportQuality === value
                        ? "border-primary bg-primary/10 text-primary"
                        : "border-border text-muted-foreground hover:bg-accent hover:text-foreground"
                    }`}
                  >
                    {label}
                    <span className="ml-1 text-[10px] text-muted-foreground">
                      {value === "high" ? "体积大" : value === "low" ? "体积小" : "推荐"}
                    </span>
                  </button>
                ))}
              </div>
              <span className="text-[11px] leading-relaxed text-muted-foreground">
                原画 + 无转场 + 无音频叠加时按流拷贝直出(最快,画质与源一致);改分辨率、
                加转场或有音频叠加时会重新编码
              </span>
            </section>
            {/* 转场仅在多片段时生效;原声渐变与音频轨叠加由同一渲染链完成 */}
            <section className="flex flex-col gap-2">
              <span className="text-muted-foreground">转场</span>
              <div className="flex gap-1">
                {(
                  [
                    ["none", "无"],
                    ["dissolve", "叠化"],
                    ["fade", "淡黑"],
                  ] as const
                ).map(([kind, label]) => (
                  <button
                    key={kind}
                    type="button"
                    onClick={() => setTransitionKind(kind)}
                    className={`flex-1 rounded-md border px-2 py-1.5 text-center transition-colors ${
                      transitionKind === kind
                        ? "border-primary bg-primary/10 text-primary"
                        : "border-border text-muted-foreground hover:bg-accent hover:text-foreground"
                    }`}
                  >
                    {label}
                  </button>
                ))}
              </div>
              {transitionKind !== "none" && (
                <div className="flex items-center gap-2">
                  <span className="shrink-0 text-muted-foreground">时长</span>
                  <div className="flex gap-1">
                    {[0.3, 0.5, 1, 2].map((s) => (
                      <button
                        key={s}
                        type="button"
                        onClick={() => setTransitionSecs(s)}
                        className={`rounded-md border px-2.5 py-1 font-mono text-[11px] tabular-nums transition-colors ${
                          transitionSecs === s
                            ? "border-primary bg-primary/10 text-primary"
                            : "border-border text-muted-foreground hover:bg-accent hover:text-foreground"
                        }`}
                      >
                        {s}s
                      </button>
                    ))}
                  </div>
                </div>
              )}
            </section>
          </div>
          <DialogFooter>
            <Button variant="outline" size="sm" onClick={() => setExportOpen(false)}>
              取消
            </Button>
            <Button
              size="sm"
              disabled={duration <= 0}
              onClick={() => void exportVideo()}
            >
              <Download className="size-3.5" />
              {totalClips > 0 ? `加入后台队列(${totalClips} 段)` : "加入后台队列"}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      {/* 非模态后台任务坞：不遮挡编辑器，可连续提交多个版本并逐项取消。 */}
      {exportJobs.length > 0 && (
        <aside className="fixed bottom-5 right-5 z-50 flex w-[340px] max-w-[calc(100vw-2rem)] flex-col overflow-hidden rounded-xl border border-border bg-popover/95 shadow-2xl backdrop-blur">
          <header className="flex items-center border-b border-border px-3 py-2.5">
            <span className="text-xs font-medium">导出任务</span>
            <span className="ml-2 rounded-full bg-muted px-1.5 py-0.5 text-[10px] text-muted-foreground">
              {exportJobs.filter((job) => ["queued", "running", "cancelling"].includes(job.status)).length} 个处理中
            </span>
            <button
              type="button"
              title="收起已结束任务"
              onClick={dismissFinishedExportJobs}
              className="ml-auto inline-flex size-6 items-center justify-center rounded-md text-muted-foreground hover:bg-accent hover:text-foreground"
            >
              <X className="size-3.5" />
            </button>
          </header>
          <div className="veltrix-editor-scrollbar flex max-h-80 flex-col gap-2 overflow-y-auto p-2">
            {exportJobs.map((job, index) => {
              const active = ["queued", "running", "cancelling"].includes(job.status);
              return (
                <article key={job.jobId} className="flex flex-col gap-2 rounded-lg border border-border bg-background/65 p-2.5">
                  <div className="flex items-center gap-2 text-xs">
                    {active ? <Loader2 className="size-3.5 shrink-0 animate-spin text-primary" /> : job.status === "completed" ? <Download className="size-3.5 shrink-0 text-emerald-500" /> : <AlertTriangle className="size-3.5 shrink-0 text-destructive" />}
                    <span className="min-w-0 flex-1 truncate">导出任务 {exportJobs.length - index}</span>
                    <span className="font-mono text-[10px] tabular-nums text-muted-foreground">{Math.round(job.percent)}%</span>
                  </div>
                  <div className="h-1.5 overflow-hidden rounded-full bg-muted" role="progressbar" aria-valuenow={Math.round(job.percent)} aria-valuemin={0} aria-valuemax={100}>
                    <div className={`h-full rounded-full transition-[width] duration-150 ${job.status === "failed" ? "bg-destructive" : job.status === "completed" ? "bg-emerald-500" : "bg-primary"}`} style={{ width: `${Math.max(0, Math.min(100, job.percent))}%` }} />
                  </div>
                  <div className="flex items-center gap-2">
                    <span className="min-w-0 flex-1 truncate text-[10px] text-muted-foreground">{job.stage}</span>
                    {active ? (
                      <button type="button" disabled={job.status === "cancelling"} onClick={() => void cancelExport(job.jobId)} className="text-[10px] text-muted-foreground hover:text-destructive disabled:opacity-50">
                        {job.status === "cancelling" ? "取消中" : "取消"}
                      </button>
                    ) : job.outputPath ? (
                      <button type="button" onClick={() => void api.revealPath(job.outputPath!)} className="text-[10px] text-primary hover:underline">打开位置</button>
                    ) : null}
                    {!active && (
                      <button type="button" aria-label="移除任务" onClick={() => dismissExportJob(job.jobId)} className="text-muted-foreground hover:text-foreground"><X className="size-3" /></button>
                    )}
                  </div>
                </article>
              );
            })}
          </div>
        </aside>
      )}

      {/* 失败原因保留在页面中，用户可阅读完整信息并回到参数页重试。 */}
      <Dialog open={exportError !== null} onOpenChange={(open) => !open && setExportError(null)}>
        <DialogContent className="sm:max-w-md">
          <DialogHeader>
            <DialogTitle className="flex items-center gap-2">
              <AlertTriangle className="size-4 text-destructive" />
              导出失败
            </DialogTitle>
          </DialogHeader>
          <div className="max-h-40 overflow-y-auto rounded-lg border border-destructive/20 bg-destructive/5 p-3 text-xs leading-relaxed text-muted-foreground veltrix-editor-scrollbar">
            {exportError}
          </div>
          <p className="text-[11px] leading-relaxed text-muted-foreground">
            当前工程和导出参数都已保留，可以调整参数后重新尝试。
          </p>
          <DialogFooter>
            <Button variant="outline" size="sm" onClick={() => setExportError(null)}>
              关闭
            </Button>
            <Button
              size="sm"
              onClick={() => {
                setExportError(null);
                setExportOpen(true);
              }}
            >
              重新导出
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      {/* 轨道重命名对话框(右键菜单「重命名」) */}
      <Dialog open={renameTarget !== null} onOpenChange={(o) => !o && setRenameTarget(null)}>
        <DialogContent className="sm:max-w-xs">
          <DialogHeader>
            <DialogTitle>重命名轨道</DialogTitle>
          </DialogHeader>
          <Input
            value={renameName}
            autoFocus
            maxLength={30}
            className="h-8 text-xs"
            onChange={(e) => setRenameName(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter" && renameTarget !== null) {
                renameTrack(renameTarget, renameName);
                setRenameTarget(null);
              }
            }}
          />
          <DialogFooter>
            <Button variant="outline" size="sm" onClick={() => setRenameTarget(null)}>
              取消
            </Button>
            <Button
              size="sm"
              onClick={() => {
                if (renameTarget !== null) renameTrack(renameTarget, renameName);
                setRenameTarget(null);
              }}
            >
              保存
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      {/* 时间轴右键菜单浮层:轨道头 / 片段 / 空白通道 三种形态;动作执行后即关闭 */}
      {ctxMenu && (
        <div
          ref={ctxMenuRef}
          className="fixed z-[60] min-w-44 rounded-md border border-border bg-popover p-1 text-foreground shadow-lg"
          style={{
            left: Math.min(ctxMenu.x, window.innerWidth - 200),
            top: Math.min(ctxMenu.y, window.innerHeight - 320),
          }}
          onContextMenu={(e) => e.preventDefault()}
        >
          {(() => {
            const track = tracks.find((t) => t.id === ctxMenu.trackId);
            if (!track) return null;
            const idx = tracks.findIndex((t) => t.id === ctxMenu.trackId);
            const close = () => setCtxMenu(null);
            const act = (fn: () => void) => () => {
              fn();
              close();
            };
            if (ctxMenu.kind === "track") {
              const typeMeta = TRACK_META[track.type];
              return (
                <>
                  <CtxMenuItem
                    icon={Pencil}
                    label="重命名"
                    onClick={act(() => {
                      setRenameName(track.name);
                      setRenameTarget(track.id);
                    })}
                  />
                  <CtxSeparator />
                  <CtxMenuItem
                    icon={ArrowUpToLine}
                    label={`上方插入${typeMeta.label}轨`}
                    onClick={act(() => insertTrackAt(track.type, idx))}
                  />
                  <CtxMenuItem
                    icon={ArrowDownToLine}
                    label={`下方插入${typeMeta.label}轨`}
                    onClick={act(() => insertTrackAt(track.type, idx + 1))}
                  />
                  <CtxMenuItem
                    icon={ArrowUp}
                    label="上移轨道"
                    disabled={idx === 0}
                    onClick={act(() => moveTrackBy(track.id, -1))}
                  />
                  <CtxMenuItem
                    icon={ArrowDown}
                    label="下移轨道"
                    disabled={idx === tracks.length - 1}
                    onClick={act(() => moveTrackBy(track.id, 1))}
                  />
                  <CtxSeparator />
                  <CtxMenuItem icon={Copy} label="复制轨道" onClick={act(() => duplicateTrack(track.id))} />
                  <CtxMenuItem
                    icon={ListCollapse}
                    label="自动拼接"
                    disabled={track.clips.length < 2}
                    onClick={act(() => compactTrackClips(track.id))}
                  />
                  <CtxMenuItem
                    icon={Eraser}
                    label="清空片段"
                    disabled={track.locked || track.clips.length === 0}
                    onClick={act(() => clearTrackClips(track.id))}
                  />
                  {track.type === "video" && (
                    <CtxMenuItem
                      icon={Combine}
                      label="合并音频"
                      onClick={act(() => mergeDetachedAudio(track.id))}
                    />
                  )}
                  <CtxSeparator />
                  <CtxMenuItem
                    icon={track.locked ? LockOpen : Lock}
                    label={track.locked ? "解锁轨道" : "锁定轨道"}
                    onClick={act(() => toggleTrackFlag(track.id, "locked"))}
                  />
                  <CtxMenuItem
                    icon={track.hidden ? Eye : EyeOff}
                    label={track.hidden ? "显示轨道" : "隐藏轨道"}
                    onClick={act(() => toggleTrackFlag(track.id, "hidden"))}
                  />
                  {(track.type === "video" || track.type === "audio") && (
                    <CtxMenuItem
                      icon={track.muted ? Volume2 : VolumeX}
                      label={track.muted ? (track.type === "audio" ? "取消静音" : "恢复原声") : track.type === "audio" ? "静音" : "关闭原声"}
                      onClick={act(() => toggleTrackFlag(track.id, "muted"))}
                    />
                  )}
                  <CtxSeparator />
                  <CtxMenuItem
                    icon={Trash2}
                    label="删除轨道"
                    danger
                    disabled={track.locked || tracks.length <= 1}
                    onClick={act(() => removeTrack(track.id))}
                  />
                </>
              );
            }
            if (ctxMenu.kind === "clip") {
              const clip = track.clips.find((c) => c.id === ctxMenu.clipId);
              const crossing = clip
                ? clip.start < current - 0.01 && clip.end > current + 0.01
                : false;
              const multi = selectedClipIds.length > 1 && ctxMenu.clipId && selectedClipIds.includes(ctxMenu.clipId);
              return (
                <>
                  <CtxMenuItem
                    icon={Scissors}
                    label="在播放头处分割"
                    disabled={track.locked || !crossing}
                    onClick={act(() => splitAtPlayhead())}
                  />
                  {track.type === "video" && (
                    <CtxMenuItem
                      icon={Unplug}
                      label={multi ? `分离所选音频(${selectedClipIds.length})` : "分离音频"}
                      disabled={track.locked}
                      onClick={act(() => ctxMenu.clipId && detachAudio(track.id, ctxMenu.clipId))}
                    />
                  )}
                  <CtxMenuItem
                    icon={ListChecks}
                    label="全选本轨片段"
                    disabled={track.clips.length === 0}
                    onClick={act(() => selectAllClipsInTrack(track.id))}
                  />
                  {multi && (
                    <CtxMenuItem
                      icon={Combine}
                      label="编组选中片段"
                      onClick={act(groupSelectedClips)}
                    />
                  )}
                  {clip?.groupId && (
                    <CtxMenuItem
                      icon={Unplug}
                      label="解除片段编组"
                      onClick={act(ungroupSelectedClips)}
                    />
                  )}
                  <CtxSeparator />
                  <CtxMenuItem
                    icon={Copy}
                    label={multi ? `复制所选(${selectedClipIds.length})` : "复制此片段"}
                    onClick={act(() => copySelectedClips())}
                  />
                  <CtxMenuItem
                    icon={ClipboardPaste}
                    label="粘贴到本轨"
                    disabled={track.locked || !clipClipboard.current}
                    onClick={act(() => pasteClips(track.id))}
                  />
                  <CtxSeparator />
                  <CtxMenuItem
                    icon={Trash2}
                    label={multi ? `删除所选(${selectedClipIds.length})` : "删除此片段"}
                    danger
                    disabled={track.locked}
                    onClick={act(() => {
                      if (multi) deleteSelectedClips(false);
                      else if (ctxMenu.clipId) removeClip(track.id, ctxMenu.clipId);
                    })}
                  />
                  <CtxMenuItem
                    icon={ListCollapse}
                    label={multi ? `波纹删除所选(${selectedClipIds.length})` : "波纹删除此片段"}
                    danger
                    disabled={track.locked}
                    onClick={act(() => deleteSelectedClips(true))}
                  />
                </>
              );
            }
            // 空白通道:增轨 + 粘贴 + 全选本轨
            return (
              <>
                <CtxMenuItem icon={Clapperboard} label="添加视频轨" onClick={act(() => insertTrackAt("video", idx + 1))} />
                <CtxMenuItem icon={AudioWaveform} label="添加音频轨" onClick={act(() => insertTrackAt("audio", idx + 1))} />
                <CtxMenuItem icon={Captions} label="添加字幕轨" onClick={act(() => insertTrackAt("text", idx + 1))} />
                <CtxSeparator />
                <CtxMenuItem
                  icon={ClipboardPaste}
                  label="粘贴片段"
                  disabled={track.locked || !clipClipboard.current}
                  onClick={act(() => pasteClips(track.id))}
                />
                <CtxMenuItem
                  icon={ListChecks}
                  label="全选本轨片段"
                  disabled={track.clips.length === 0}
                  onClick={act(() => selectAllClipsInTrack(track.id))}
                />
              </>
            );
          })()}
        </div>
      )}
    </div>
  );
}

// 状态容器:未选片显示首页,选片 / 恢复草稿后进入编辑器
// (key 重挂载保证换片、换草稿时编辑状态清零)
export function VideoEditorPanel({
  initialDraft,
  onExit,
}: {
  initialDraft?: VideoDraft;
  onExit?: () => void;
} = {}) {
  const [editing, setEditing] = useState<{
    path: string;
    draft?: VideoDraft;
  } | null>(() => initialDraft ? { path: initialDraft.inputPath, draft: initialDraft } : null);
  if (!editing) {
    return (
      <div className="flex min-h-0 flex-1 flex-col">
        {onExit && (
          <div className="flex justify-end px-6 pt-4 lg:px-10">
            <button type="button" onClick={onExit} className="text-xs text-muted-foreground hover:text-foreground">
              返回 AI 成片
            </button>
          </div>
        )}
        <VideoEditorHome
          onOpen={(path) => setEditing({ path })}
          onOpenDraft={(draft) => setEditing({ path: draft.inputPath, draft })}
        />
      </div>
    );
  }
  return (
    <VideoEditor
      key={editing.draft?.id ?? editing.path}
      inputPath={editing.path}
      draft={editing.draft}
      onBack={() => {
        if (onExit) onExit();
        else setEditing(null);
      }}
      onOpen={(path) => setEditing({ path })}
    />
  );
}
