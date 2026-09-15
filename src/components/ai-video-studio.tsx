// AI 成片工作台第一期：以“目标 → 自动分析 → 直接成片”为主路径。
// DeepSeek-V4.1-Flash 只选择真实镜头编号；口播语义定位仍需带时间戳转写，不能由截图臆测。
// 时间线只作为 AI 初剪结果的修正器，不提供从空白项目开始的专业剪辑入口。
import { useEffect, useMemo, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { openPath } from "@tauri-apps/plugin-opener";
import {
  ArrowRight,
  CheckCircle2,
  Eye,
  FileVideo2,
  Loader2,
  Play,
  RefreshCw,
  Sparkles,
  WandSparkles,
} from "lucide-react";
import { toast } from "sonner";

import { Button } from "@/components/ui/button";
import { VideoEditorPanel } from "@/components/video-editor";
import { api } from "@/lib/api";
import { useMediaFileUrl } from "@/lib/media-file-url";
import type {
  ClipSegment,
  CreationExportProgress,
  AiVideoPlanView,
  TrackClip,
  VideoInfo,
} from "@/lib/api-types";
import type { VideoDraft } from "@/lib/video-drafts";
import { fmt } from "@/lib/timefmt";

type Pace = "fast" | "balanced" | "story";
type Goal = "lead" | "reach" | "knowledge" | "repurpose";

const PLATFORM_OPTIONS = [
  { value: "douyin", label: "抖音" },
  { value: "xhs", label: "小红书" },
  { value: "kuaishou", label: "快手" },
  { value: "tiktok", label: "TikTok" },
  { value: "shorts", label: "YouTube Shorts" },
] as const;

const GOAL_OPTIONS: Array<{ value: Goal; label: string; description: string }> = [
  { value: "lead", label: "获得线索", description: "突出痛点、结果与行动引导" },
  { value: "reach", label: "扩大曝光", description: "更快节奏与更强开场" },
  { value: "knowledge", label: "知识表达", description: "优先保证观点和叙事完整" },
  { value: "repurpose", label: "内容复用", description: "压缩原片并保留主要结构" },
];

const PACE_OPTIONS: Array<{ value: Pace; label: string; maxScene: number }> = [
  { value: "fast", label: "紧凑", maxScene: 2.8 },
  { value: "balanced", label: "均衡", maxScene: 4.5 },
  { value: "story", label: "叙事", maxScene: 7.5 },
];

function fileName(path: string) {
  return path.split(/[\\/]/).pop() || path;
}

function buildSceneCandidates(duration: number, cuts: number[], maxScene: number) {
  const points = [0, ...cuts, duration]
    .filter((point) => Number.isFinite(point) && point >= 0 && point <= duration)
    .sort((a, b) => a - b)
    .filter((point, index, all) => index === 0 || point - all[index - 1] > 0.2);
  const candidates: Array<{ start: number; end: number }> = [];
  for (let index = 0; index < points.length - 1; index += 1) {
    let cursor = points[index];
    const end = points[index + 1];
    while (end - cursor > maxScene + 0.25) {
      candidates.push({ start: cursor, end: cursor + maxScene });
      cursor += maxScene;
    }
    if (end - cursor >= 0.35) candidates.push({ start: cursor, end });
  }
  return candidates;
}

function sampleSceneCandidates(duration: number, cuts: number[], pace: Pace) {
  const maxScene = PACE_OPTIONS.find((option) => option.value === pace)?.maxScene ?? 4.5;
  const all = buildSceneCandidates(duration, cuts, maxScene);
  if (all.length <= 20) return all;
  const indices = new Set<number>([0]);
  for (let index = 1; index < 20; index += 1) {
    indices.add(Math.round((index * (all.length - 1)) / 19));
  }
  return [...indices].sort((a, b) => a - b).map((index) => all[index]);
}

function segmentsFromAiPlan(
  candidates: Array<{ start: number; end: number }>,
  selectedIndices: number[],
  targetDuration: number,
): ClipSegment[] {
  let remaining = targetDuration;
  let position = 0;
  const result: ClipSegment[] = [];
  for (const index of [...selectedIndices].sort((a, b) => a - b)) {
    const scene = candidates[index];
    if (!scene || remaining < 0.35) break;
    const length = Math.min(scene.end - scene.start, remaining);
    if (length < 0.35) break;
    result.push({ start: scene.start, end: scene.start + length, position });
    position += length;
    remaining -= length;
  }
  return result;
}

function makeDraft(path: string, segments: ClipSegment[], duration: number): VideoDraft {
  const clips: TrackClip[] = segments.map((segment) => ({
    ...segment,
    id: crypto.randomUUID(),
    inputPath: path,
    sourceDuration: duration,
  }));
  return {
    id: crypto.randomUUID(),
    name: `AI 初剪 · ${fileName(path)}`,
    inputPath: path,
    tracks: [{ id: crypto.randomUUID(), type: "video", name: "AI 初剪", clips }],
    settings: {
      transitionKind: "none",
      transitionSecs: 0.35,
      exportResolution: "1080p",
      exportQuality: "medium",
      playbackRate: 1,
      trackHeight: 56,
      waveLevel: 1,
      editMode: "append",
    },
    updatedAt: Math.floor(Date.now() / 1000),
  };
}

export function AiVideoStudio() {
  const [sourcePath, setSourcePath] = useState("");
  const [videoInfo, setVideoInfo] = useState<VideoInfo | null>(null);
  const [platform, setPlatform] = useState("douyin");
  const [goal, setGoal] = useState<Goal>("repurpose");
  const [pace, setPace] = useState<Pace>("balanced");
  const [targetDuration, setTargetDuration] = useState(30);
  const [brief, setBrief] = useState("");
  const [segments, setSegments] = useState<ClipSegment[]>([]);
  const [aiPlan, setAiPlan] = useState<AiVideoPlanView | null>(null);
  const [lastCuts, setLastCuts] = useState<number[]>([]);
  const [candidates, setCandidates] = useState<Array<{ start: number; end: number }>>([]);
  const [thumbs, setThumbs] = useState<string[]>([]);
  const [analyzing, setAnalyzing] = useState(false);
  const [advancedDraft, setAdvancedDraft] = useState<VideoDraft | null>(null);
  const [job, setJob] = useState<CreationExportProgress | null>(null);
  const previewRef = useRef<HTMLVideoElement>(null);
  const mediaFileUrl = useMediaFileUrl();

  // 目标变化后旧方案不再代表当前要求；必须重新策划才能导出。
  useEffect(() => {
    setSegments([]);
    setAiPlan(null);
  }, [platform, goal, pace, targetDuration, brief]);

  useEffect(() => {
    let dispose: (() => void) | undefined;
    void listen<CreationExportProgress>("creation-export-progress", (event) => {
      setJob((current) => current?.jobId === event.payload.jobId ? event.payload : current);
    }).then((unlisten) => {
      dispose = unlisten;
    });
    return () => dispose?.();
  }, []);

  const selectedDuration = useMemo(
    () => segments.reduce((sum, segment) => sum + segment.end - segment.start, 0),
    [segments],
  );
  const outputReady = segments.length > 0 && videoInfo;

  async function chooseSource() {
    const picked = await openDialog({
      multiple: false,
      filters: [{ name: "视频", extensions: ["mp4", "mov", "mkv", "webm", "avi"] }],
    });
    if (typeof picked !== "string") return;
    setSourcePath(picked);
    setVideoInfo(null);
    setSegments([]);
    setAiPlan(null);
    setLastCuts([]);
    setCandidates([]);
    setThumbs([]);
    setJob(null);
  }

  async function analyze() {
    if (!sourcePath) {
      toast.error("请先选择一条视频");
      return;
    }
    setAnalyzing(true);
    setSegments([]);
    setAiPlan(null);
    setJob(null);
    try {
      const info = await api.creationVideoInfo(sourcePath);
      const [cuts, thumbPaths] = await Promise.all([
        api.creationDetectScenes(sourcePath),
        api.creationVideoThumbs(sourcePath).catch(() => [] as string[]),
      ]);
      setVideoInfo(info);
      setLastCuts(cuts);
      setThumbs(thumbPaths.map(mediaFileUrl));
      const sceneCandidates = sampleSceneCandidates(info.durationSecs, cuts, pace);
      setCandidates(sceneCandidates);
      if (!sceneCandidates.length) throw new Error("视频太短，无法生成初剪方案");
      const plan = await api.creationAiPlan({
        inputPath: sourcePath,
        scenes: sceneCandidates,
        platform,
        goal,
        brief,
        targetDuration,
      });
      const next = segmentsFromAiPlan(sceneCandidates, plan.selectedIndices, targetDuration);
      if (!next.length) throw new Error("DeepSeek 未选出可用镜头");
      setAiPlan(plan);
      setSegments(next);
      toast.success(`DeepSeek-V4.1-Flash 已从 ${sceneCandidates.length} 个候选镜头中选择 ${next.length} 段`);
    } catch (error) {
      toast.error(`AI 策划失败：${error}`);
    } finally {
      setAnalyzing(false);
    }
  }

  function useLocalDraft() {
    if (!videoInfo) {
      toast.error("请先完成本地场景检测");
      return;
    }
    const visible = candidates.length ? candidates : sampleSceneCandidates(videoInfo.durationSecs, lastCuts, pace);
    if (!visible.length) {
      toast.error("视频过短，无法生成可用镜头");
      return;
    }
    const wanted = Math.min(visible.length, Math.max(1, Math.ceil(targetDuration / 4.5)));
    const picks = new Set<number>();
    for (let slot = 0; slot < wanted; slot += 1) {
      picks.add(Math.round(slot * (visible.length - 1) / Math.max(1, wanted - 1)));
    }
    let next = segmentsFromAiPlan(visible, [...picks], targetDuration);
    for (let index = 0; index < visible.length && next.reduce((sum, clip) => sum + clip.end - clip.start, 0) < targetDuration - 0.35; index += 1) {
      if (picks.has(index)) continue;
      picks.add(index);
      next = segmentsFromAiPlan(visible, [...picks], targetDuration);
    }
    setAiPlan(null);
    setSegments(next);
    toast.success("已生成不依赖模型的本地粗剪");
  }

  function previewScene(scene: { start: number; end: number }) {
    const video = previewRef.current;
    if (!video) return;
    video.currentTime = Math.min(scene.start + 0.1, Math.max(scene.start, scene.end - 0.1));
    void video.play().catch(() => toast.error("视频预览暂不可用"));
  }

  function toggleScene(index: number) {
    if (analyzing || !candidates[index]) return;
    const selected = candidates
      .map((scene, sceneIndex) => segments.some((segment) => Math.abs(segment.start - scene.start) < 0.05) ? sceneIndex : -1)
      .filter((sceneIndex) => sceneIndex >= 0);
    let nextIndices = selected.includes(index)
      ? selected.filter((sceneIndex) => sceneIndex !== index)
      : [...selected, index];
    let next = segmentsFromAiPlan(candidates, nextIndices, targetDuration);
    // 后段镜头加入时，目标时长可能已被前段占满；让用户刚选的画面真正进入成片。
    if (!selected.includes(index)) {
      while (nextIndices.length > 1 && !next.some((segment) => Math.abs(segment.start - candidates[index].start) < 0.05)) {
        const earliest = [...nextIndices].sort((a, b) => a - b)[0];
        nextIndices = nextIndices.filter((sceneIndex) => sceneIndex !== earliest);
        next = segmentsFromAiPlan(candidates, nextIndices, targetDuration);
      }
    }
    setSegments(next);
    setAiPlan(null);
  }

  function sceneThumb(scene: { start: number; end: number }) {
    if (!thumbs.length || !videoInfo?.durationSecs) return undefined;
    const middle = (scene.start + scene.end) / 2;
    const index = Math.min(thumbs.length - 1, Math.max(0, Math.round(middle / videoInfo.durationSecs * (thumbs.length - 1))));
    return thumbs[index];
  }

  async function startExport() {
    if (!sourcePath || !videoInfo || !segments.length) return;
    const jobId = crypto.randomUUID();
    const vertical = { width: 1080, height: 1920 };
    const sourceWidth = Math.max(1, videoInfo.width);
    const sourceHeight = Math.max(1, videoInfo.height);
    const fit = Math.min(vertical.width / sourceWidth, vertical.height / sourceHeight);
    const fill = Math.max(vertical.width / sourceWidth, vertical.height / sourceHeight);
    const fillScale = Math.min(3, Math.max(1, fill / Math.max(fit, 0.0001)));
    const exportSegments = segments.map((segment) => ({
      ...segment,
      inputPath: sourcePath,
      transform: { scale: fillScale },
    }));
    setJob({ jobId, percent: 0, stage: "正在加入队列", status: "queued" });
    try {
      await api.creationStartExport({
        inputPath: sourcePath,
        segments: exportSegments,
        scale: vertical,
        quality: "medium",
        jobId,
      });
      toast.success("AI 成片任务已加入后台队列");
    } catch (error) {
      setJob({ jobId, percent: 0, stage: "提交失败", status: "failed", error: String(error) });
      toast.error(`提交失败：${error}`);
    }
  }

  if (advancedDraft) {
    return (
      <VideoEditorPanel
        initialDraft={advancedDraft}
        onExit={() => setAdvancedDraft(null)}
      />
    );
  }

  return (
    <div className="veltrix-editor-scrollbar min-h-0 flex-1 overflow-y-auto">
      <main className="mx-auto flex w-full max-w-[1380px] flex-col gap-6 px-6 py-7 lg:px-10 lg:py-9">
        <header className="flex flex-col gap-4 lg:flex-row lg:items-start lg:justify-between">
          <div className="max-w-3xl">
            <div className="mb-2 flex items-center gap-2 text-xs font-medium text-primary">
              <Sparkles className="size-3.5" />
              内容生产 · AI 成片
            </div>
            <h1 className="text-2xl font-semibold tracking-tight">把视频变成一张可调整的镜头故事板</h1>
            <p className="mt-2 text-sm leading-6 text-muted-foreground">
              先看原片，再让 AI 挑镜头。点击画面就能预览或调整入选片段，确认后直接成片。
            </p>
          </div>
        </header>

        <section className="grid gap-4 xl:grid-cols-[1fr_1.15fr_0.9fr]">
          <article className="rounded-2xl border border-border bg-card p-5 shadow-sm">
            <div className="mb-5 flex items-center gap-3">
              <span className="flex size-8 items-center justify-center rounded-full bg-primary text-sm font-semibold text-primary-foreground">1</span>
              <div><h2 className="text-sm font-semibold">选择源视频</h2><p className="text-xs text-muted-foreground">第一期支持本地长视频</p></div>
            </div>
            {!sourcePath ? <button
              type="button"
              disabled={analyzing}
              onClick={() => void chooseSource()}
              className="flex min-h-48 w-full flex-col items-center justify-center gap-3 rounded-xl border border-dashed border-primary/35 bg-primary/[0.035] p-5 text-center transition-colors hover:border-primary hover:bg-primary/[0.07]"
            >
              <span className="flex size-12 items-center justify-center rounded-xl bg-primary/10 text-primary"><FileVideo2 className="size-5" /></span>
              <><strong className="text-sm">选择本地视频</strong><span className="max-w-56 text-xs leading-5 text-muted-foreground">支持 MP4、MOV、MKV、WebM 和 AVI</span></>
            </button> : (
              <div className="space-y-2">
                <video ref={previewRef} controls playsInline preload="metadata" src={mediaFileUrl(sourcePath)} className="aspect-video w-full rounded-xl bg-black object-contain" />
                <div className="flex items-center justify-between gap-2 text-xs"><strong className="truncate">{fileName(sourcePath)}</strong><button type="button" disabled={analyzing} onClick={() => void chooseSource()} className="shrink-0 text-primary hover:underline">更换视频</button></div>
              </div>
            )}
            {videoInfo && (
              <div className="mt-3 grid grid-cols-3 gap-2 text-center text-xs">
                <span className="rounded-lg bg-muted/50 px-2 py-2"><strong className="block">{fmt(videoInfo.durationSecs)}</strong><small className="text-muted-foreground">原片时长</small></span>
                <span className="rounded-lg bg-muted/50 px-2 py-2"><strong className="block">{videoInfo.width}×{videoInfo.height}</strong><small className="text-muted-foreground">分辨率</small></span>
                <span className="rounded-lg bg-muted/50 px-2 py-2"><strong className="block">{Math.round(videoInfo.fps)} fps</strong><small className="text-muted-foreground">帧率</small></span>
              </div>
            )}
          </article>

          <article className="rounded-2xl border border-border bg-card p-5 shadow-sm">
            <div className="mb-5 flex items-center gap-3">
              <span className="flex size-8 items-center justify-center rounded-full bg-primary text-sm font-semibold text-primary-foreground">2</span>
              <div><h2 className="text-sm font-semibold">一句话告诉 AI</h2><p className="text-xs text-muted-foreground">不填也可以，直接让 AI 看视频</p></div>
            </div>
            <div className="space-y-4">
              <label className="block text-xs font-medium">希望成片突出什么？
                <textarea value={brief} disabled={analyzing} onChange={(event) => setBrief(event.target.value)} placeholder="比如：保留最精彩的操作过程，开头快速展示结果。也可以留空。" className="mt-2 min-h-28 w-full resize-none rounded-xl border border-input bg-background px-3 py-3 text-sm leading-6 outline-none focus:border-primary" />
              </label>
              <div className="rounded-xl bg-muted/40 p-3">
                <div className="flex items-center justify-between text-xs"><span className="font-medium">想要多长？</span><strong className="text-primary">约 {targetDuration} 秒</strong></div>
                <input type="range" min="15" max="90" step="15" value={targetDuration} disabled={analyzing} onChange={(event) => setTargetDuration(Number(event.target.value))} aria-label="成片目标时长" className="mt-3 w-full accent-primary" />
                <div className="flex justify-between text-[10px] text-muted-foreground"><span>更短</span><span>更完整</span></div>
              </div>
              <details className="text-xs text-muted-foreground">
                <summary className="cursor-pointer py-1 hover:text-foreground">需要指定平台或风格？展开偏好</summary>
                <div className="mt-3 space-y-3 rounded-xl border border-border p-3">
                  <label className="block font-medium">发布平台
                    <select value={platform} disabled={analyzing} onChange={(event) => setPlatform(event.target.value)} className="mt-1.5 h-9 w-full rounded-md border border-input bg-background px-3 text-sm">
                      {PLATFORM_OPTIONS.map((option) => <option key={option.value} value={option.value}>{option.label}</option>)}
                    </select>
                  </label>
                  <label className="block font-medium">内容方向
                    <select value={goal} disabled={analyzing} onChange={(event) => setGoal(event.target.value as Goal)} className="mt-1.5 h-9 w-full rounded-md border border-input bg-background px-3 text-sm">
                      {GOAL_OPTIONS.map((option) => <option key={option.value} value={option.value}>{option.label}</option>)}
                    </select>
                  </label>
                  <label className="block font-medium">镜头节奏
                  <select value={pace} disabled={analyzing} onChange={(event) => setPace(event.target.value as Pace)} className="mt-1.5 h-9 w-full rounded-md border border-input bg-background px-3 text-sm">
                    {PACE_OPTIONS.map((option) => <option key={option.value} value={option.value}>{option.label}</option>)}
                  </select>
                  </label>
                </div>
              </details>
              <p className="text-[10px] leading-4 text-muted-foreground">默认按抖音竖版、内容复用和均衡节奏策划；不需要逐项设置。</p>
            </div>
          </article>

          <article className="flex flex-col rounded-2xl border border-border bg-card p-5 shadow-sm">
            <div className="mb-5 flex items-center gap-3">
              <span className="flex size-8 items-center justify-center rounded-full bg-primary text-sm font-semibold text-primary-foreground">3</span>
              <div><h2 className="text-sm font-semibold">生成初剪</h2><p className="text-xs text-muted-foreground">AI 选完后在下方画面里审核</p></div>
            </div>
            {!segments.length ? (
              <div className="flex flex-1 flex-col items-center justify-center rounded-xl bg-muted/25 p-5 text-center">
                <WandSparkles className="mb-3 size-8 text-primary/70" />
                <strong className="text-sm">让 AI 看一遍视频</strong>
                <p className="mt-1 max-w-56 text-xs leading-5 text-muted-foreground">识别镜头并给出可见的初剪故事板。模型看画面截图，不理解口播时间轴。</p>
                <Button className="mt-5 w-full" disabled={!sourcePath || analyzing} onClick={() => void analyze()}>
                  {analyzing ? <Loader2 className="size-4 animate-spin" /> : <Sparkles className="size-4" />}
                  {analyzing ? "正在分析与策划" : "生成镜头故事板"}
                </Button>
                {videoInfo && !analyzing && (
                  <button type="button" onClick={useLocalDraft} className="mt-3 text-xs text-muted-foreground hover:text-foreground">模型不可用？使用本地粗剪</button>
                )}
              </div>
            ) : (
              <div className="flex flex-1 flex-col">
                <div className="rounded-xl border border-emerald-500/25 bg-emerald-500/[0.06] p-4">
                  <div className="flex items-center gap-2 text-emerald-600 dark:text-emerald-400"><CheckCircle2 className="size-4" /><strong className="text-sm">初剪方案已就绪</strong></div>
                  {aiPlan && (
                    <div className="mt-2 text-xs">
                      <strong className="block">{aiPlan.title}</strong>
                      <p className="mt-1 leading-5 text-muted-foreground">{aiPlan.rationale}</p>
                      <p className="mt-1 text-[10px] text-muted-foreground">模型仅看镜头截图，观点、商品宣称与口播连贯性请人工复核。</p>
                    </div>
                  )}
                  <div className="mt-3 grid grid-cols-3 gap-2 text-center">
                    <span><strong className="block text-base">{segments.length}</strong><small className="text-[10px] text-muted-foreground">选中镜头</small></span>
                    <span><strong className="block text-base">{fmt(selectedDuration)}</strong><small className="text-[10px] text-muted-foreground">成片时长</small></span>
                    <span><strong className="block text-base">{aiPlan ? "AI" : "本地"}</strong><small className="text-[10px] text-muted-foreground">选段方式</small></span>
                  </div>
                </div>
                {job && (
                  <div className="mb-3 rounded-lg border border-border p-3 text-xs">
                    <div className="flex items-center justify-between"><span>{job.stage}</span><span>{Math.round(job.percent)}%</span></div>
                    <div className="mt-2 h-1.5 overflow-hidden rounded-full bg-muted"><div className="h-full bg-primary transition-all" style={{ width: `${job.percent}%` }} /></div>
                    {job.error && <p className="mt-2 text-destructive">{job.error}</p>}
                    {job.outputPath && (
                      <div className="mt-2 space-y-2">
                        <video controls playsInline preload="metadata" src={mediaFileUrl(job.outputPath)} className="aspect-[9/16] max-h-64 w-full rounded-lg bg-black object-contain" />
                        <button type="button" onClick={() => void openPath(job.outputPath!)} className="inline-flex items-center gap-1 text-primary hover:underline"><Play className="size-3" />打开成片文件</button>
                      </div>
                    )}
                  </div>
                )}
                <div className="mt-auto space-y-2">
                  <Button className="w-full" disabled={!outputReady || job?.status === "queued" || job?.status === "running"} onClick={() => void startExport()}>
                    <WandSparkles className="size-4" />一键生成竖版成片
                  </Button>
                  <Button variant="outline" className="w-full" onClick={() => setAdvancedDraft(makeDraft(sourcePath, segments, videoInfo!.durationSecs))}>
                    高级调整<ArrowRight className="size-4" />
                  </Button>
                  <button type="button" onClick={() => void analyze()} className="flex w-full items-center justify-center gap-1.5 py-1 text-xs text-muted-foreground hover:text-foreground"><RefreshCw className="size-3" />重新生成方案</button>
                </div>
              </div>
            )}
          </article>
        </section>

        {candidates.length > 0 && (
          <section className="rounded-2xl border border-border bg-card p-5 shadow-sm">
            <div className="mb-4 flex flex-wrap items-end justify-between gap-2">
              <div><h2 className="text-base font-semibold">镜头故事板</h2><p className="mt-1 text-xs text-muted-foreground">点击镜头加入或移出成片；点播放可在左侧原片中复核。缩略图是源视频抽帧，不是最终竖版预览。</p></div>
              <span className="rounded-full bg-primary/10 px-3 py-1 text-xs font-medium text-primary">{segments.length} / {candidates.length} 个镜头入选</span>
            </div>
            <div className="grid gap-3 sm:grid-cols-2 lg:grid-cols-4 xl:grid-cols-5">
              {candidates.map((scene, index) => {
                const selected = segments.some((segment) => Math.abs(segment.start - scene.start) < 0.05);
                const thumb = sceneThumb(scene);
                return (
                  <div key={`${scene.start}-${scene.end}`} className={`overflow-hidden rounded-xl border transition-colors ${selected ? "border-primary bg-primary/[0.05]" : "border-border bg-background"}`}>
                    {thumb && <img src={thumb} alt={`镜头 ${index + 1} 的视频缩略图`} loading="lazy" className="aspect-video w-full object-cover" />}
                    <div className="flex items-center justify-between gap-1 p-2">
                      <button type="button" disabled={analyzing} onClick={() => toggleScene(index)} aria-pressed={selected} className="min-w-0 flex-1 text-left text-xs">
                        <strong className="block truncate">{selected ? "✓ " : ""}镜头 {index + 1}</strong>
                        <span className="text-[10px] text-muted-foreground">{fmt(scene.start)} – {fmt(scene.end)}</span>
                      </button>
                      <button type="button" onClick={() => previewScene(scene)} aria-label={`预览镜头 ${index + 1}`} title="在原片中预览" className="rounded-md p-1.5 text-muted-foreground hover:bg-accent hover:text-foreground"><Eye className="size-4" /></button>
                    </div>
                  </div>
                );
              })}
            </div>
          </section>
        )}
      </main>
    </div>
  );
}
