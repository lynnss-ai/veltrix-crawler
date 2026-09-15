// 视频剪辑草稿:localStorage 持久化(前期轻量方案,不入库)。
// 草稿 = 源视频路径 + 多轨道(视频/音频/字幕,每条轨道若干片段),
// 编辑器里轨道变化后自动保存,首页草稿卡片点击恢复继续剪辑。上限 50 条,超出淘汰最旧的。
// 旧格式(只有 clips 字段)读取时自动迁移为单条视频轨。
import type {
  EditorTrack,
  ExportQuality,
  ExportResolution,
  TrackClip,
} from "@/lib/api-types";

// 工程级设置必须跟草稿一起恢复,否则重新打开后预览/导出参数会悄悄回到默认值。
export interface VideoDraftSettings {
  transitionKind: "none" | "dissolve" | "fade";
  transitionSecs: number;
  exportResolution: ExportResolution;
  exportQuality: ExportQuality;
  playbackRate: number;
  trackHeight: number;
  waveLevel: number;
  editMode?: "append" | "insert" | "overwrite";
}

export interface TimelineMarker {
  id: string;
  time: number;
  label: string;
}

export interface VideoDraft {
  id: string;
  name: string; // 源视频文件名(展示用)
  inputPath: string;
  tracks: EditorTrack[];
  markers?: TimelineMarker[];
  settings?: VideoDraftSettings;
  updatedAt: number; // 秒级时间戳
  // 封面(JPEG dataURL,宽 480):编辑器抓帧 / 首页懒回填,卡片展示用
  cover?: string;
  // 浏览器 canvas 抓帧失败时,使用后端 FFmpeg 胶片条首帧(media_root 相对路径)。
  coverPath?: string;
}

const KEY = "veltrix-video-drafts";
const MAX_DRAFTS = 50;

// 旧格式草稿(单组片段,无轨道概念)
interface LegacyDraft {
  id: string;
  name: string;
  inputPath: string;
  clips?: { start: number; end: number }[];
  tracks?: EditorTrack[];
  markers?: TimelineMarker[];
  settings?: VideoDraftSettings;
  updatedAt: number;
  cover?: string;
  coverPath?: string;
}

function migrate(d: LegacyDraft): VideoDraft {
  if (Array.isArray(d.tracks)) {
    return {
      id: d.id,
      name: d.name,
      inputPath: d.inputPath,
      tracks: d.tracks,
      markers: d.markers,
      settings: d.settings,
      updatedAt: d.updatedAt,
      cover: d.cover,
      coverPath: d.coverPath,
    };
  }
  const clips: TrackClip[] = (d.clips ?? []).map((c) => ({
    ...c,
    id: crypto.randomUUID(),
  }));
  return {
    id: d.id,
    name: d.name,
    inputPath: d.inputPath,
    tracks: [{ id: crypto.randomUUID(), type: "video", name: "视频 1", clips }],
    markers: d.markers,
    settings: d.settings,
    updatedAt: d.updatedAt,
    cover: d.cover,
    coverPath: d.coverPath,
  };
}

export function listDrafts(): VideoDraft[] {
  try {
    const raw = localStorage.getItem(KEY);
    if (!raw) return [];
    const list = (JSON.parse(raw) as LegacyDraft[]).map(migrate);
    return list.sort((a, b) => b.updatedAt - a.updatedAt);
  } catch {
    return [];
  }
}

export function saveDraft(draft: VideoDraft): void {
  const rest = listDrafts().filter((d) => d.id !== draft.id);
  const next = [draft, ...rest].slice(0, MAX_DRAFTS);
  localStorage.setItem(KEY, JSON.stringify(next));
}

export function deleteDraft(id: string): void {
  localStorage.setItem(
    KEY,
    JSON.stringify(listDrafts().filter((d) => d.id !== id)),
  );
}

// 只更新封面字段(草稿可能尚未因片段入档,此时静默跳过,等自动保存时带上)
export function setDraftCover(id: string, cover: string): void {
  const list = listDrafts();
  if (!list.some((d) => d.id === id)) return;
  localStorage.setItem(
    KEY,
    JSON.stringify(list.map((d) => (d.id === id ? { ...d, cover } : d))),
  );
}

export function setDraftCoverPath(id: string, coverPath: string): void {
  const list = listDrafts();
  if (!list.some((draft) => draft.id === id)) return;
  localStorage.setItem(
    KEY,
    JSON.stringify(list.map((draft) => draft.id === id ? { ...draft, coverPath } : draft)),
  );
}
