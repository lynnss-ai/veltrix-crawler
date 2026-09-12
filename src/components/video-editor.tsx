// 视频剪辑编辑器(剪映式布局,铺满整个工作区):
// 左侧片段面板(头部带 打开 / 返回首页 入口);右侧预览列 = 预览区(黑场衬底、等比缩放居中,
// 点击播放/暂停)+ 播放器控制条;底部多轨道时间轴(对齐专业 NLE:视频轨 / 音频轨 / 字幕轨,
// 工具行:编辑工具(选择/分割/左右全选/删除选中,快捷键 A/B/[/]/Delete)+ 撤销重做(Ctrl+Z/Shift+Z)、
// 增轨;轨道头点击设为激活轨,激活轨上框选后「添加片段」落到该轨;
// 刻度尺与轨道空白处按住拖动 = scrub,拖手柄设入点/出点,拖选区平移,点击片段选中并定位播放头)。
// 轨道与片段变化自动存草稿(localStorage,见 lib/video-drafts.ts),首页可恢复继续剪辑。
// 字幕轨暂为占位(后续接 burn-in)。不做转场 / 画中画合成。
// VideoEditorPanel 是首页 / 编辑器的状态容器,首页见 video-editor-home.tsx。
import { memo, useEffect, useMemo, useRef, useState, type CSSProperties, type ReactNode } from "react";
import { open as openDialog } from "@tauri-apps/plugin-dialog";import {
  ArrowLeftToLine,
  ArrowRightToLine,
  AudioLines,
  Download,
  Eye,
  EyeOff,
  FastForward,
  Film,
  FolderInput,
  ListPlus,
  Loader2,
  Lock,
  LockOpen,
  MoreVertical,
  MousePointer2,
  Music,
  Pause,
  Play,
  Plus,
  Redo2,
  Rewind,
  Scissors,
  SkipBack,
  SkipForward,
  Sparkles,
  Trash2,
  Type,
  Undo2,
  Volume2,
  VolumeX,
  ZoomIn,
  ZoomOut,
} from "lucide-react";
import { toast } from "sonner";

import { api } from "@/lib/api";
import type { EditorTrack, TrackType, TransitionInput, VideoInfo } from "@/lib/api-types";
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
import { saveDraft, setDraftCover, type VideoDraft } from "@/lib/video-drafts";
import { captureVideoCover } from "@/lib/video-cover";
import { getAudioPeaks } from "@/lib/audio-peaks";
import { fmt, fmtTick } from "@/lib/timefmt";
import { MaterialPanel } from "@/components/video-editor-panel";

// 秒时间格式化统一走 @/lib/timefmt(fmt = 时:分:秒:厘秒,fmtTick = 刻度尺短格式)

// 从路径取文件名(兼容正反斜杠,草稿名用)
function baseName(path: string): string {
  return path.split(/[\\/]/).pop() ?? path;
}

// 轨道类型 → 展示元数据(图标 / 中文名 / 片段头条与帧体配色,剪映式青色视频轨)。
// 帧体分 light / dark 两套底:light 用浅彩底, dark 用深彩底;头条保持饱和色(白字两主题均可读)。
const TRACK_META: Record<
  TrackType,
  { icon: typeof Film; label: string; headerClass: string; frameClass: string }
> = {
  video: { icon: Film, label: "视频", headerClass: "bg-teal-600/85 dark:bg-teal-700/80", frameClass: "border-teal-500/70 bg-teal-100 dark:border-teal-600/70 dark:bg-teal-950/60" },
  audio: { icon: Music, label: "音频", headerClass: "bg-emerald-600/85 dark:bg-emerald-700/80", frameClass: "border-emerald-500/70 bg-emerald-100 dark:border-emerald-600/70 dark:bg-emerald-950/60" },
  text: { icon: Type, label: "字幕", headerClass: "bg-amber-600/85 dark:bg-amber-700/80", frameClass: "border-amber-500/70 bg-amber-100 dark:border-amber-600/70 dark:bg-amber-950/60" },
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
  // 音频波形峰值(前端 WebAudio 解码;null = 未就绪,音频片段回退 CSS 纹理)
  audioPeaks: Float32Array | null;
}

// Timeline 的 DOM 句柄通道:current 高频变化不走 props(避免每帧重渲染整棵树),
// 播放头位置与跟随滚动由 VideoEditor 直接写 DOM;playheadPct 供挂载/重挂载时定位初始位置
interface TimelineRefs {
  scroll: HTMLDivElement | null;
  lanes: HTMLDivElement | null;
  playhead: HTMLDivElement | null;
  playheadPct: string;
}

interface TimelineActions {
  onSeek: (t: number) => void;
  // 精确落点(点击片段;scrub 走 onSeek 的节流预览)
  onSeekExact: (t: number) => void;
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
  const ref = useRef<HTMLCanvasElement>(null);
  useEffect(() => {
    const canvas = ref.current;
    if (!canvas) return;
    const draw = () => {
      const dpr = window.devicePixelRatio || 1;
      const w = canvas.clientWidth;
      const h = canvas.clientHeight;
      if (!w || !h) return;
      canvas.width = Math.round(w * dpr);
      canvas.height = Math.round(h * dpr);
      const ctx = canvas.getContext("2d");
      if (!ctx) return;
      ctx.scale(dpr, dpr);
      const n = peaks.length;
      const from = Math.max(0, Math.floor((start / duration) * n));
      const to = Math.min(n, Math.max(from + 1, Math.ceil((end / duration) * n)));
      const span = to - from;
      const amp = (h / 2) * [0.35, 0.65, 0.95][Math.min(2, Math.max(0, level))];
      ctx.fillStyle = isDark ? "rgba(52,211,153,0.85)" : "rgba(5,150,105,0.8)";
      const mid = h / 2;
      for (let x = 0; x < w; x++) {
        const v = peaks[from + Math.floor((x / w) * span)] ?? 0;
        const bh = Math.max(1, v * amp);
        ctx.fillRect(x, mid - bh, 1, bh * 2);
      }
    };
    draw();
    const ro = new ResizeObserver(draw);
    ro.observe(canvas);
    return () => ro.disconnect();
  }, [peaks, isDark, level, start, end, duration]);
  return <canvas ref={ref} className="h-full w-full" />;
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

// 多轨道时间轴(剪映式):左侧轨道头列(名称 + 锁定/可见/静音控制,点击激活,可拖拽排序),
// 右侧刻度尺(|mm:ss 主刻度 + 次级小刻度)+ 每条轨道一条通道;片段 = 头条(文件名 + 时长)+
// 帧体纹理(视频胶片分帧 / 音频拟波形);选区(激活轨上,两端手柄拖动 / 中部平移)+
// 贯通刻度尺与全轨道的白色播放头;刻度尺与通道空白处按住拖动 = scrub
function TimelineInner({ state, actions, refs }: { state: TimelineState; actions: TimelineActions; refs: TimelineRefs }) {
  const { tracks, activeTrackId, duration, selStart, selEnd, fileName, selectedClipIds, zoom, trackHeight, waveLevel, thumbUrls, audioPeaks } = state;
  const { onSeek, onSeekExact, onSelection, onScrubChange, onSelectTrack, onRemoveTrack, onRemoveClip, onSelectClip, onMoveTrack, onToggleFlag, onZoomChange } = actions;
  // 纹理 / 片段配色随主题(system 时读系统偏好)
  const { theme } = useTheme();
  const isDark =
    theme === "dark" ||
    (theme === "system" && window.matchMedia("(prefers-color-scheme: dark)").matches);
  const lanesRef = useRef<HTMLDivElement>(null);
  // 横向滚动容器(zoom>1 时通道区变宽出滚动条)
  const scrollRef = useRef<HTMLDivElement>(null);
  const [viewWidth, setViewWidth] = useState(0);
  const dragMode = useRef<"seek" | "start" | "end" | "move" | null>(null);
  // move 模式:按下点相对选区起点的偏移,平移时保持选区长度不变
  const moveAnchor = useRef(0);

  // 测量可视宽度(刻度密度随缩放 / 容器宽自适应)。依赖 duration:元数据就绪前
  // Timeline 早退不渲染、scrollRef 为 null,必须在时长就位后重新测量,否则刻度恒为兜底值
  useEffect(() => {
    const el = scrollRef.current;
    if (!el) return;
    const ro = new ResizeObserver(() => setViewWidth(el.clientWidth));
    ro.observe(el);
    setViewWidth(el.clientWidth);
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

  if (duration <= 0) return null;
  const pct = (t: number) => `${(t / duration) * 100}%`;
  const timeAt = (clientX: number) => {
    const el = lanesRef.current;
    if (!el) return 0;
    const r = el.getBoundingClientRect();
    const ratio = Math.min(1, Math.max(0, (clientX - r.left) / r.width));
    return ratio * duration;
  };

  function beginDrag(mode: NonNullable<typeof dragMode.current>, e: React.PointerEvent) {
    e.stopPropagation();
    // 指针捕获挂到通道区:手柄按下后的 move/up 也路由到统一处理器
    lanesRef.current?.setPointerCapture(e.pointerId);
    dragMode.current = mode;
    if (mode === "seek") {
      setScrubbing(true);
      onScrubChange?.(true);
      onSeek(timeAt(e.clientX));
    }
    if (mode === "move") moveAnchor.current = timeAt(e.clientX) - selStart;
  }

  function endDrag() {
    if (dragMode.current === "seek") {
      setScrubbing(false);
      onScrubChange?.(false);
    }
    dragMode.current = null;
  }

  function onPointerMove(e: React.PointerEvent) {
    const mode = dragMode.current;
    if (!mode) return;
    const t = timeAt(e.clientX);
    if (mode === "seek") onSeek(t);
    else if (mode === "start") onSelection(Math.min(t, selEnd - 0.1), selEnd);
    else if (mode === "end") onSelection(selStart, Math.max(t, selStart + 0.1));
    else {
      const len = selEnd - selStart;
      const s = Math.min(Math.max(0, t - moveAnchor.current), duration - len);
      onSelection(s, s + len);
    }
  }

  // 刻度密度随缩放自适应:保证相邻刻度 ≥50px,素材长 / 缩得小则逐级放疏
  const pxPerSec = viewWidth > 0 ? (viewWidth * zoom) / duration : 0;
  const TICK_CANDIDATES = [0.5, 1, 2, 5, 10, 15, 30, 60, 120, 300];
  let tickEvery = TICK_CANDIDATES.find((c) => pxPerSec * c >= 50) ?? 300;
  while (duration / tickEvery > 400) tickEvery *= 2; // 超长线素材防刻度爆炸
  const ticks: number[] = [];
  for (let t = 0; t <= duration; t += tickEvery) ticks.push(t);

  return (
    // 撑满父容器全高:右侧滚动容器的横向滚动条才能落在时间轴区最底部
    <div className="flex h-full select-none">
      {/* 左:轨道头列(名称 + 锁定/可见/静音控制,点击激活,可拖拽排序;与右侧通道行高对齐) */}
      <div className="w-32 shrink-0 border-r border-border bg-muted/30">
        <div className="h-6 border-b border-border bg-muted/20" />
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
              onKeyDown={(e) => e.key === "Enter" && onSelectTrack(t.id)}
              style={{ height: trackHeight }}
              className={`group relative flex cursor-grab items-center gap-1 border-b border-border/60 px-2 active:cursor-grabbing ${
                active ? "bg-accent/50" : ""
              } ${dragIdx === idx ? "opacity-40" : ""} ${
                dropAt === idx && dragIdx !== null && dragIdx !== idx
                  ? "bg-primary/10 shadow-[inset_0_2px_0_0] shadow-primary"
                  : ""
              }`}
            >
              <meta.icon
                className={`size-3.5 shrink-0 ${active ? "text-foreground" : "text-muted-foreground"}`}
              />
              {/* 控制:锁定(禁增删)/ 可见(隐藏不参与导出)/ 静音(仅音频轨,不混音),与轨道图标同行 */}
              <div className="flex items-center gap-0.5">
                <TrackFlagButton
                  on={!!t.locked}
                  activeClass="text-amber-500"
                  tooltip={t.locked ? "解锁轨道" : "锁定轨道(禁止增删片段)"}
                  onClick={() => onToggleFlag(t.id, "locked")}
                >
                  {t.locked ? <Lock className="size-3" /> : <LockOpen className="size-3" />}
                </TrackFlagButton>
                <TrackFlagButton
                  on={!t.hidden}
                  activeClass="text-foreground"
                  tooltip={t.hidden ? "显示轨道(参与导出)" : "隐藏轨道(不参与导出)"}
                  onClick={() => onToggleFlag(t.id, "hidden")}
                >
                  {t.hidden ? <EyeOff className="size-3" /> : <Eye className="size-3" />}
                </TrackFlagButton>
                {t.type === "audio" && (
                  <TrackFlagButton
                    on={!t.muted}
                    activeClass="text-foreground"
                    tooltip={t.muted ? "取消静音" : "静音(导出不混入该轨音频)"}
                    onClick={() => onToggleFlag(t.id, "muted")}
                  >
                    {t.muted ? <VolumeX className="size-3" /> : <Volume2 className="size-3" />}
                  </TrackFlagButton>
                )}
              </div>
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
          );
        })}
      </div>

      {/* 右:刻度尺 + 通道(横向滚动容器;指针事件挂内层,捕获后拖出区域仍能收到 move/up) */}
      {/* y-auto:轨道高度可调,总高超出可视区时竖向滚动(对齐剪映);横向滚动条落在时间轴区最底部 */}
      <div
        ref={(el) => {
          scrollRef.current = el;
          refs.scroll = el;
        }}
        className="min-w-0 flex-1 overflow-x-auto overflow-y-auto"
      >
        <div
          ref={(el) => {
            lanesRef.current = el;
            refs.lanes = el;
          }}
          className="relative flex min-h-full flex-col"
          style={{ width: zoom > 1 ? `${zoom * 100}%` : "100%" }}
          onPointerDown={(e) => beginDrag("seek", e)}
          onPointerMove={onPointerMove}
          onPointerUp={endDrag}
          onPointerCancel={endDrag}
        >
        {/* 刻度尺(剪映式:竖线 + 右侧时间码;刻度间距足够时补次级小刻度) */}
        <div className="relative h-6 border-b border-border bg-muted/20">
          {ticks.map((t) => (
            <span key={t} className="absolute bottom-0 flex items-end gap-1" style={{ left: pct(t) }}>
              <span className="h-2.5 w-px bg-muted-foreground/60" />
              <span className="pb-0.5 text-[9px] leading-none tabular-nums text-muted-foreground">
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
                    className="absolute bottom-0 h-1 w-px bg-border"
                    style={{ left: pct(mt) }}
                  />
                );
              }),
            )}
        </div>
        {/* 通道区:flex-1 撑满可视高度,轨道不足一屏时播放头也能到底 */}
        <div className="relative flex-1">
          {tracks.map((t) => {
            const meta = TRACK_META[t.type];
            const active = t.id === activeTrackId;
            return (
              <div
                key={t.id}
                style={{ height: trackHeight }}
                className={`relative cursor-grab overflow-hidden border-b border-border active:cursor-grabbing ${
                  active ? "bg-muted/40" : "bg-muted/20"
                }`}
              >
                {/* 该轨已有片段(点击选中并定位播放头,Ctrl+点击多选;头条 hover 出删除,锁定轨不给出) */}
                {t.clips.map((c) => (
                  <div
                    key={c.id}
                    role="button"
                    tabIndex={0}
                    onPointerDown={(e) => e.stopPropagation()}
                    onClick={(e) => {
                      onSeekExact(c.start);
                      onSelectClip(c.id, e.ctrlKey || e.metaKey);
                    }}
                    onKeyDown={(e) => e.key === "Enter" && onSeekExact(c.start)}
                    className={`group absolute inset-y-1 z-10 flex cursor-pointer flex-col overflow-hidden rounded-[4px] border ${meta.frameClass} ${
                      selectedClipIds.includes(c.id) ? "ring-2 ring-primary" : ""
                    }`}
                    style={{ left: pct(c.start), width: pct(c.end - c.start) }}
                  >
                    {/* 头条:文件名 + 片段时长 */}
                    <div className={`flex h-4 shrink-0 items-center gap-1 px-1.5 ${meta.headerClass}`}>
                      <span className="truncate text-[9px] text-white/90">{fileName}</span>
                      <span className="ml-auto shrink-0 font-mono text-[9px] tabular-nums text-white/75">
                        {fmt(c.end - c.start)}
                      </span>
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
                        {thumbUrls.length > 0 ? (
                          <Filmstrip
                            thumbs={thumbUrls}
                            start={c.start}
                            end={c.end}
                            duration={duration}
                            pxPerSec={pxPerSec}
                          />
                        ) : (
                          <div
                            className="min-h-0 flex-1"
                            style={clipTexture(t.type, waveLevel, isDark)}
                          />
                        )}
                        {/* 源视频带声音时,在胶片条下方压一条矮波形,方便对着人声下刀 */}
                        {audioPeaks && (
                          <div className="h-3.5 shrink-0">
                            <WaveformBody
                              peaks={audioPeaks}
                              isDark={isDark}
                              level={2}
                              start={c.start}
                              end={c.end}
                              duration={duration}
                            />
                          </div>
                        )}
                      </div>
                    ) : t.type === "audio" && audioPeaks ? (
                      <div className="min-h-0 flex-1">
                        <WaveformBody
                          peaks={audioPeaks}
                          isDark={isDark}
                          level={waveLevel}
                          start={c.start}
                          end={c.end}
                          duration={duration}
                        />
                      </div>
                    ) : (
                      <div
                        className="min-h-0 flex-1"
                        style={clipTexture(t.type, waveLevel, isDark)}
                      />
                    )}
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
        {/* 播放头(剪映式):刻度尺区一枚醒目的红色抓手 + 贯通到底的竖线;hover / 拖动中抓手放大、
            竖线加粗并发光,scrub 时抓手旁跟随时间气泡(文本由 VideoEditor 直写,不走渲染)。
            位置不走 React 渲染:挂载时取 refs.playheadPct,之后由 VideoEditor 直接写 style.left */}
        <div
          ref={(el) => {
            refs.playhead = el;
            if (el) el.style.left = refs.playheadPct;
          }}
          className="absolute inset-y-0 z-30 -translate-x-1/2"
        >
          <div
            className="group flex h-full w-3 cursor-ew-resize touch-none flex-col items-center"
            onPointerDown={(e) => beginDrag("seek", e)}
          >
            {/* 刻度尺区抓手 */}
            <div className="relative flex h-6 w-full items-center justify-center">
              <div
                className={`rounded-full bg-red-500 shadow transition-all ${
                  scrubbing ? "h-[18px] w-3" : "h-4 w-2.5 group-hover:h-[18px] group-hover:w-3"
                }`}
              />
              {/* 时间气泡:仅 scrub 时显示,跟随播放头 */}
              {scrubbing && (
                <span
                  data-ph-time
                  className="absolute left-3 top-1/2 -translate-y-1/2 whitespace-nowrap rounded bg-red-500 px-1 py-0.5 text-[9px] leading-none tabular-nums text-white shadow"
                />
              )}
            </div>
            {/* 贯通竖线 */}
            <div
              className={`flex-1 bg-red-500/90 transition-all group-hover:w-[2px] group-hover:bg-red-500 group-hover:shadow-[0_0_4px_rgba(239,68,68,0.7)] ${
                scrubbing ? "w-[2px] bg-red-500 shadow-[0_0_4px_rgba(239,68,68,0.7)]" : "w-px"
              }`}
            />
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
  const [duration, setDuration] = useState(0);
  const [current, setCurrent] = useState(0);
  const [playing, setPlaying] = useState(false);
  // 播放倍速(应用到 <video>.playbackRate,换片不重置)
  const [rate, setRate] = useState(1);
  const [selStart, setSelStart] = useState(0);
  const [selEnd, setSelEnd] = useState(0);
  // 多轨道:默认一条视频轨;草稿恢复时带上保存的轨道
  const [tracks, setTracks] = useState<EditorTrack[]>(
    () => draft?.tracks ?? [newTrack("video", [])],
  );
  const [activeTrackId, setActiveTrackId] = useState(
    () => (draft?.tracks ?? [])[0]?.id ?? "",
  );
  // 时间轴缩放(1 = 适配宽度,上限 20x)
  const [zoom, setZoom] = useState(1);
  const changeZoom = (z: number) => setZoom(Math.min(20, Math.max(1, z)));
  // 片段选择:点击片段选中(Ctrl+点击多选),工具栏/快捷键做分割、左右全选、删除
  const [selectedClipIds, setSelectedClipIds] = useState<string[]>([]);
  // 撤销/重做:tracks 快照历史(上限 50 步);histCounts 仅驱动按钮禁用态渲染
  const history = useRef<{ past: EditorTrack[][]; future: EditorTrack[][] }>({
    past: [],
    future: [],
  });
  const [histCounts, setHistCounts] = useState({ past: 0, future: 0 });
  // 时间轴显示设置(⋮ 菜单):轨道高度(px)/ 音频波形占比档位(0 小 / 1 中 / 2 大)
  const [trackHeight, setTrackHeight] = useState(56);
  const [waveLevel, setWaveLevel] = useState(1);
  // 导出:转场(kind=none 不加,流拷贝快路径)/ 转场时长 / 导出中状态
  const [transitionKind, setTransitionKind] = useState<"none" | "dissolve" | "fade">("none");
  const [transitionSecs, setTransitionSecs] = useState(0.5);
  const [exporting, setExporting] = useState(false);
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
  const [thumbRels, setThumbRels] = useState<string[]>([]);
  // 音频波形峰值(出现音频轨片段时按需解码一次)
  const [audioPeaks, setAudioPeaks] = useState<Float32Array | null>(null);
  const [settingsOpen, setSettingsOpen] = useState(false);
  // 对话框内编辑中的名称(取消不落地)
  const [nameDraft, setNameDraft] = useState("");

  // 无草稿新开会话:activeTrackId 在 tracks 初始化后补上(初始 tracks[0])
  useEffect(() => {
    if (!activeTrackId && tracks.length > 0) setActiveTrackId(tracks[0].id);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // 轨道 / 草稿名变化自动存草稿(防抖 500ms;所有轨道都空时不建档)
  useEffect(() => {
    if (tracks.every((t) => t.clips.length === 0)) return;
    const t = setTimeout(() => {
      const now = Math.floor(Date.now() / 1000);
      saveDraft({
        id: draftId.current,
        name: draftName,
        inputPath,
        tracks,
        updatedAt: now,
        cover: coverRef.current,
      });
      setLastSavedAt(now);
    }, 500);
    return () => clearTimeout(t);
  }, [tracks, inputPath, draftName]);

  // 倍速变化实时应用到视频元素
  useEffect(() => {
    if (videoRef.current) videoRef.current.playbackRate = rate;
  }, [rate]);

  // 元信息(帧率/码率/编码)与胶片条缩略图:打开视频后后台拉取,失败静默(展示回退)
  useEffect(() => {
    let cancelled = false;
    api.creationVideoInfo(inputPath).then((info) => {
      if (!cancelled) setVideoInfo(info);
    }).catch(() => {});
    api.creationVideoThumbs(inputPath).then((rels) => {
      if (!cancelled) setThumbRels(rels);
    }).catch(() => {});
    return () => {
      cancelled = true;
    };
  }, [inputPath]);

  // 音频波形:出现音频轨片段时解码一次(getAudioPeaks 按 URL 缓存,重复触发无成本)
  const hasAudioClips = tracks.some((t) => t.type === "audio" && t.clips.length > 0);
  useEffect(() => {
    if (!hasAudioClips || audioPeaks) return;
    let cancelled = false;
    void getAudioPeaks(mediaFileUrl(inputPath), 1600).then((peaks) => {
      if (!cancelled && peaks) setAudioPeaks(peaks);
    });
    return () => {
      cancelled = true;
    };
  }, [hasAudioClips, audioPeaks, inputPath, mediaFileUrl]);

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

  function seek(t: number) {
    setCurrent(t);
    throttledPreviewSeek(t);
  }

  // 拖动(scrub)期间的预览定位:rAF 节流 + fastSeek(只跳关键帧,解码开销小),
  // 避免每次 pointermove 都精确 seek 导致卡顿;松手后由 handleScrubChange 精确落点
  const seekRaf = useRef(0);
  const seekPending = useRef(0);
  function throttledPreviewSeek(t: number) {
    seekPending.current = t;
    if (seekRaf.current) return;
    seekRaf.current = requestAnimationFrame(() => {
      seekRaf.current = 0;
      const v = videoRef.current;
      if (!v) return;
      const target = seekPending.current;
      if (typeof v.fastSeek === "function") v.fastSeek(target);
      else v.currentTime = target;
    });
  }

  // 精确落点(scrub 结束 / 点击片段):直接设 currentTime
  function seekExact(t: number) {
    if (seekRaf.current) {
      cancelAnimationFrame(seekRaf.current);
      seekRaf.current = 0;
    }
    if (videoRef.current) videoRef.current.currentTime = t;
    setCurrent(t);
  }

  function togglePlay() {
    const v = videoRef.current;
    if (!v) return;
    if (v.paused) {
      // 时间轴模式:当前不在任何片段内时,先跳到序列开头再播
      if (previewMode === "timeline" && playSeq.length) {
        const inside = playSeq.some((c) => current >= c.start && current < c.end);
        if (!inside) seekExact(playSeq[0].start);
      }
      void v.play();
    } else {
      v.pause();
    }
  }

  // 序列预览:播到片段尾自动跳下一段开头;序列外(手动 seek 到空隙)跳最近下一段,没有则停
  function handleTimeUpdate(t: number) {
    setCurrent(t);
    if (previewMode !== "timeline" || !playing || !playSeq.length) return;
    const idx = playSeq.findIndex((c) => t >= c.start - 0.05 && t < c.end);
    if (idx >= 0) {
      if (t >= playSeq[idx].end - 0.04) {
        const next = playSeq[idx + 1];
        if (next) seekExact(next.start);
        else videoRef.current?.pause();
      }
      return;
    }
    const next = playSeq.find((c) => c.start > t);
    if (next) seekExact(next.start);
    else videoRef.current?.pause();
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
      seekExact(playSeq[0].start);
      return;
    }
    setPreviewMode(mode);
  }

  // 拖播放头(scrub)时暂停播放,松手后若之前在播则恢复
  const scrubWasPlaying = useRef(false);
  function handleScrubChange(scrubbing: boolean) {
    const v = videoRef.current;
    if (!v) return;
    if (scrubbing) {
      scrubWasPlaying.current = !v.paused;
      v.pause();
    } else {
      // 松手:取消节流的预览 seek,精确落到最终位置,再视情况恢复播放
      seekExact(seekPending.current);
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
    setSelectedClipIds((prev) =>
      additive
        ? prev.includes(clipId)
          ? prev.filter((x) => x !== clipId)
          : [...prev, clipId]
        : [clipId],
    );
  }

  // 向左/向右全选([ / ]):选中播放头一侧的全部片段
  function selectClipsSide(side: "left" | "right") {
    const ids = tracks.flatMap((t) =>
      t.clips
        .filter((c) => (side === "left" ? c.end <= current + 0.01 : c.start >= current - 0.01))
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
          return hit && c.start < t - 0.01 && c.end > t + 0.01;
        }),
    );
    if (!anyHit) {
      toast.error("播放头处没有可分割的片段");
      return;
    }
    splitAtTimes([t]);
  }

  // 按切点分割所有未锁定轨片段(智能切分 / 播放头分割共用;场景切点音频轨一并切开,保持音画对齐)
  function splitAtTimes(times: number[]) {
    const sorted = [...times].sort((a, b) => a - b);
    commitTracks((prev) =>
      prev.map((track) => {
        if (track.locked) return track;
        return {
          ...track,
          clips: track.clips.flatMap((c) => {
            const points = sorted.filter((t) => t > c.start + 0.01 && t < c.end - 0.01);
            if (!points.length) return [c];
            const parts = [{ ...c, end: points[0] }];
            for (let i = 0; i < points.length; i++) {
              parts.push({
                id: crypto.randomUUID(),
                start: points[i],
                end: i + 1 < points.length ? points[i + 1] : c.end,
              });
            }
            return parts;
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
      const cuts = await api.creationDetectScenes(inputPath);
      const inRange = cuts.filter((t) => t > 0.1 && t < duration - 0.1);
      if (!inRange.length) {
        toast.info("未检测到场景切换点");
        return;
      }
      splitAtTimes(inRange);
      toast.success(`智能切分完成:${inRange.length} 个切点`);
    } catch (e) {
      toast.error(`智能切分失败: ${e}`);
    } finally {
      setDetecting(false);
    }
  }

  // 删除选中片段(Delete;锁定轨跳过)
  function deleteSelectedClips() {
    if (!selectedClipIds.length) {
      toast.error("先点击选中要删除的片段");
      return;
    }
    commitTracks((prev) =>
      prev.map((t) =>
        t.locked ? t : { ...t, clips: t.clips.filter((c) => !selectedClipIds.includes(c.id)) },
      ),
    );
    setSelectedClipIds([]);
  }

  // 快捷键:A 选择(清除选择)/ B 分割 / [ 向左全选 / ] 向右全选 / Delete 删除选中 / Ctrl+Z(Shift) 撤销重做。
  // 不设依赖数组:每渲染重挂一次,保证闭包拿到最新 tracks / current / 选择态。
  useEffect(() => {
    function onKey(e: KeyboardEvent) {
      const el = e.target as HTMLElement;
      if (el.tagName === "INPUT" || el.tagName === "TEXTAREA" || el.isContentEditable) return;
      if (e.ctrlKey || e.metaKey) {
        const k = e.key.toLowerCase();
        if (k === "z") {
          e.preventDefault();
          if (e.shiftKey) redo();
          else undo();
        } else if (k === "y") {
          e.preventDefault();
          redo();
        }
        return;
      }
      if (e.key === "Delete" || e.key === "Backspace") {
        if (selectedClipIds.length) deleteSelectedClips();
      } else if (e.key.toLowerCase() === "b") splitAtPlayhead();
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

  // 轨道拖拽排序(from → to 位置)
  function moveTrack(from: number, to: number) {
    commitTracks((prev) => {
      const next = [...prev];
      const [moved] = next.splice(from, 1);
      next.splice(to, 0, moved);
      return next;
    });
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
    commitTracks((prev) => {
      const next = prev.filter((t) => t.id !== id);
      // 保底:至少留一条轨道
      return next.length ? next : [newTrack("video", [])];
    });
    if (activeTrackId === id) {
      setActiveTrackId((tracks.find((t) => t.id !== id)?.id) ?? "");
    }
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
      toast.error("字幕轨暂不支持添加片段(建设中)");
      return;
    }
    commitTracks((prev) =>
      prev.map((t) =>
        t.id === track.id
          ? {
              ...t,
              clips: [
                ...t.clips,
                { id: crypto.randomUUID(), start: selStart, end: selEnd },
              ].sort((a, b) => a.start - b.start),
            }
          : t,
      ),
    );
  }

  function removeClip(trackId: string, clipId: string) {
    if (tracks.find((t) => t.id === trackId)?.locked) {
      toast.error("轨道已锁定,先解锁再删片段");
      return;
    }
    commitTracks((prev) =>
      prev.map((t) =>
        t.id === trackId
          ? { ...t, clips: t.clips.filter((c) => c.id !== clipId) }
          : t,
      ),
    );
  }

  async function exportVideo() {
    // 视频轨片段(按轨道顺序、轨内按时间排序);没有则退回当前选区作为单段。
    // 隐藏轨不参与导出;静音音频轨不混入(隐藏音频轨一并排除)
    const videoClips = tracks
      .filter((t) => t.type === "video" && !t.hidden)
      .flatMap((t) => t.clips)
      .sort((a, b) => a.start - b.start);
    const audioClips = tracks
      .filter((t) => t.type === "audio" && !t.hidden && !t.muted)
      .flatMap((t) => t.clips)
      .sort((a, b) => a.start - b.start);
    const segs = videoClips.length
      ? videoClips.map(({ start, end }) => ({ start, end }))
      : [{ start: selStart, end: selEnd }];
    if (!videoClips.length && selEnd - selStart >= duration - 0.05) {
      toast.error("选区就是完整视频,先框选要保留的片段");
      return;
    }
    const transition: TransitionInput | undefined =
      transitionKind === "none"
        ? undefined
        : { kind: transitionKind, durationSecs: transitionSecs };
    setExporting(true);
    try {
      const out = await api.creationExportVideo(
        inputPath,
        segs,
        audioClips.map(({ start, end }) => ({ start, end })),
        transition,
      );
      toast.success("视频导出完成", {
        action: {
          label: "打开文件夹",
          onClick: () => {
            api.revealPath(out).catch((err) => toast.error(`打开失败: ${err}`));
          },
        },
      });
    } catch (e) {
      toast.error(`导出失败: ${e}`);
    } finally {
      setExporting(false);
    }
  }

  const totalClips = tracks.reduce((n, t) => n + t.clips.length, 0);
  const activeTrack = tracks.find((t) => t.id === activeTrackId);
  // 序列预览播放序列:可见视频轨片段(轨道顺序,轨内按时间)
  const playSeq = tracks
    .filter((t) => t.type === "video" && !t.hidden)
    .flatMap((t) => t.clips)
    .sort((a, b) => a.start - b.start);

  // 分栏拖动:块间距本身即隐形拖动区(无可见拖动条),指针捕获保证拖出热区不丢事件。
  // 上下比例默认 65%(钳制 25%~80%);三列默认 左40% / 中40% / 右20%(整数比 2:2:1,按容器宽换算,
  // 拖动后左列钳制 180px~50%、右列钳制 200px~40%)。
  const containerRef = useRef<HTMLDivElement>(null);
  const [topRatio, setTopRatio] = useState(0.65);
  const [leftWidth, setLeftWidth] = useState(260);
  const [rightWidth, setRightWidth] = useState(240);
  const resizing = useRef<"row" | "left" | "right" | null>(null);
  const colDrag = useRef({ startX: 0, startW: 0 });
  // 三列默认宽只初始化一次:容器宽未就绪时挂 ResizeObserver 等首帧
  const widthInit = useRef(false);
  useEffect(() => {
    const el = containerRef.current;
    if (!el) return;
    const apply = () => {
      const w = el.getBoundingClientRect().width;
      if (w <= 0 || widthInit.current) return;
      widthInit.current = true;
      setLeftWidth(Math.min(w * 0.5, Math.max(180, w * 0.4)));
      setRightWidth(Math.min(w * 0.4, Math.max(200, w * 0.2)));
    };
    apply();
    if (widthInit.current) return;
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
      setTopRatio(Math.min(0.8, Math.max(0.25, (e.clientY - r.top) / r.height)));
      return;
    }
    const dx = e.clientX - colDrag.current.startX;
    const el = containerRef.current;
    const cw = el ? el.getBoundingClientRect().width : 0;
    if (mode === "left") {
      const maxW = cw > 0 ? cw * 0.5 : 480;
      setLeftWidth(Math.min(maxW, Math.max(180, colDrag.current.startW + dx)));
    } else {
      const maxW = cw > 0 ? cw * 0.4 : 420;
      setRightWidth(Math.min(maxW, Math.max(200, colDrag.current.startW - dx)));
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
  });
  timelineRefs.current.playheadPct =
    duration > 0 ? `${((current / duration) * 100).toFixed(4)}%` : "0%";

  // 播放头位置 + 跟随滚动:current 高频变化只写 DOM,不走 Timeline 渲染
  useEffect(() => {
    const { scroll, lanes, playhead, playheadPct } = timelineRefs.current;
    if (!playhead || duration <= 0) return;
    playhead.style.left = playheadPct;
    // scrub 时间气泡(存在才写,不触发渲染)
    const bubble = playhead.querySelector<HTMLElement>("[data-ph-time]");
    if (bubble) bubble.textContent = fmt(current);
    if (scroll && lanes) {
      const x = (current / duration) * lanes.offsetWidth;
      if (x < scroll.scrollLeft + 40 || x > scroll.scrollLeft + scroll.clientWidth - 60) {
        scroll.scrollLeft = Math.max(0, x - scroll.clientWidth / 3);
      }
    }
  }, [current, duration]);

  const timelineState = useMemo<TimelineState>(
    () => ({
      tracks,
      activeTrackId,
      duration,
      selStart,
      selEnd,
      fileName: baseName(inputPath),
      selectedClipIds,
      zoom,
      trackHeight,
      waveLevel,
      thumbUrls: thumbRels.map(mediaFileUrl),
      audioPeaks,
    }),
    [tracks, activeTrackId, duration, selStart, selEnd, inputPath, selectedClipIds, zoom, trackHeight, waveLevel, thumbRels, mediaFileUrl, audioPeaks],
  );
  // actions 引用的函数内部均基于 commitTracks(prev) 或 ref / setState 工作,
  // tracks 不变时冻结引用是安全的;tracks 变化即重新生成
  const timelineActions = useMemo<TimelineActions>(
    () => ({
      onSeek: seek,
      onSeekExact: seekExact,
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
    }),
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [tracks, activeTrackId],
  );

  return (
    <div ref={containerRef} className="flex h-full min-h-0 w-full flex-col gap-2.5">
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
                  <Undo2 className="size-3.5" />
                </button>
              </SimpleTooltip>
            </span>
          </div>
          <MaterialPanel
            tracks={tracks}
            onRemoveClip={removeClip}
            onImport={() => void importVideo()}
            transitionKind={transitionKind}
            onTransitionKind={setTransitionKind}
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
            播放器
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
            className="relative flex min-h-0 flex-1 cursor-pointer items-center justify-center bg-black/40 p-3"
            onClick={togglePlay}
          >
            <video
              ref={videoRef}
              src={mediaFileUrl(inputPath)}
              preload="metadata"
              className="max-h-full max-w-full rounded-md bg-black"
              onPlay={() => setPlaying(true)}
              onPause={() => setPlaying(false)}
              onTimeUpdate={(e) => handleTimeUpdate(e.currentTarget.currentTime)}
              onLoadedMetadata={(e) => {
                const d = e.currentTarget.duration;
                e.currentTarget.playbackRate = rate;
                setDuration(d);
                setSelStart(0);
                setSelEnd(d);
                setVideoMeta({
                  w: e.currentTarget.videoWidth,
                  h: e.currentTarget.videoHeight,
                });
              }}
            />
            {!playing && (
              <span className="pointer-events-none absolute inline-flex size-14 items-center justify-center rounded-full bg-black/50 text-white">
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
                onClick={() => seekExact(0)}
              >
                <SkipBack className="size-4" />
              </Button>
            </SimpleTooltip>
            <SimpleTooltip content="快退 5 秒">
              <Button
                variant="ghost"
                size="icon"
                className="size-7"
                onClick={() => seekExact(Math.max(0, current - 5))}
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
                onClick={() => seekExact(Math.min(duration, current + 5))}
              >
                <FastForward className="size-4" />
              </Button>
            </SimpleTooltip>
            <SimpleTooltip content="到结尾">
              <Button
                variant="ghost"
                size="icon"
                className="size-7"
                onClick={() => seekExact(duration)}
              >
                <SkipForward className="size-4" />
              </Button>
            </SimpleTooltip>
            <span className="mx-2 font-mono text-xs tabular-nums text-muted-foreground">
              {fmt(current)} / {fmt(duration)}
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
          <div className="flex min-h-0 flex-1 flex-col gap-4 overflow-y-auto p-3 text-xs">
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
              <div className="flex items-center justify-between">
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
                      className={`rounded-md border px-2 py-1 text-[11px] transition-colors ${
                        transitionKind === kind
                          ? "border-primary bg-primary/10 text-primary"
                          : "border-border text-muted-foreground hover:bg-accent hover:text-foreground"
                      }`}
                    >
                      {label}
                    </button>
                  ))}
                </div>
              </div>
              {transitionKind !== "none" && (
                <div className="flex items-center justify-between">
                  <span className="text-muted-foreground">转场时长</span>
                  <div className="flex gap-1">
                    {[0.3, 0.5, 1, 2].map((s) => (
                      <button
                        key={s}
                        type="button"
                        onClick={() => setTransitionSecs(s)}
                        className={`rounded-md border px-2 py-1 font-mono text-[11px] tabular-nums transition-colors ${
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
              <Button
                size="sm"
                className="mt-1 h-7 text-xs"
                disabled={exporting || duration <= 0}
                onClick={() => void exportVideo()}
              >
                {exporting ? (
                  <Loader2 className="size-3.5 animate-spin" />
                ) : (
                  <Download className="size-3.5" />
                )}
                {exporting ? "导出中…" : totalClips > 0 ? `导出视频(${totalClips} 段)` : "导出视频"}
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
          <SimpleTooltip content="智能切分(OpenCV 场景检测,在切点处分割所有未锁定轨)">
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
                <Sparkles className="size-3.5" />
              )}
            </Button>
          </SimpleTooltip>
          <SimpleTooltip content="删除选中片段(Delete)">
            <Button
              variant="ghost"
              size="icon"
              className="size-7"
              disabled={selectedClipIds.length === 0}
              onClick={deleteSelectedClips}
            >
              <Trash2 className="size-3.5" />
            </Button>
          </SimpleTooltip>
          <SimpleTooltip content="设为入点">
            <Button variant="ghost" size="icon" className="size-7" onClick={setIn}>
              <ArrowRightToLine className="size-3.5" />
            </Button>
          </SimpleTooltip>
          <SimpleTooltip content="设为出点">
            <Button variant="ghost" size="icon" className="size-7" onClick={setOut}>
              <ArrowLeftToLine className="size-3.5" />
            </Button>
          </SimpleTooltip>
          <SimpleTooltip content={`添加片段到「${activeTrack?.name ?? "—"}」(当前选区)`}>
            <Button variant="ghost" size="icon" className="size-7" onClick={addClip}>
              <Plus className="size-3.5" />
            </Button>
          </SimpleTooltip>
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
                <Film className="size-3.5" />
                视频轨
              </DropdownMenuItem>
              <DropdownMenuItem onClick={() => addTrack("audio")}>
                <Music className="size-3.5" />
                音频轨
              </DropdownMenuItem>
              <DropdownMenuItem onClick={() => addTrack("text")}>
                <Type className="size-3.5" />
                字幕轨(占位)
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
                <span className="shrink-0 text-xs text-muted-foreground">轨道高度</span>
                <input
                  type="range"
                  min={32}
                  max={96}
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
        {/* 时间轴贴工具栏无顶部留白,刻度尺直接顶到分隔线 */}
        <div className="min-h-0 flex-1 px-3 pb-3">
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
    </div>
  );
}

// 状态容器:未选片显示首页,选片 / 恢复草稿后进入编辑器
// (key 重挂载保证换片、换草稿时编辑状态清零)
export function VideoEditorPanel() {
  const [editing, setEditing] = useState<{
    path: string;
    draft?: VideoDraft;
  } | null>(null);
  if (!editing) {
    return (
      <VideoEditorHome
        onOpen={(path) => setEditing({ path })}
        onOpenDraft={(draft) => setEditing({ path: draft.inputPath, draft })}
      />
    );
  }
  return (
    <VideoEditor
      key={editing.draft?.id ?? editing.path}
      inputPath={editing.path}
      draft={editing.draft}
      onBack={() => setEditing(null)}
      onOpen={(path) => setEditing({ path })}
    />
  );
}
