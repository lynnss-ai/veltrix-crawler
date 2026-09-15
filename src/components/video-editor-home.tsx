// 视频剪辑首页:居中「打开」导入卡片 + 草稿列表(编辑器自动保存的剪辑进度,
// 点击草稿恢复继续剪辑)。只负责选片,选中后通过 onOpen / onOpenDraft 交给编辑器。
import { useEffect, useState } from "react";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { format } from "date-fns";
import {
  Clock3,
  FileVideo,
  Film,
  FolderInput,
  Layers3,
  Loader2,
  Play,
  Plus,
  RotateCw,
  Trash2,
} from "lucide-react";
import { toast } from "sonner";

import { api } from "@/lib/api";
import { deleteDraft, listDrafts, setDraftCover, setDraftCoverPath, type VideoDraft } from "@/lib/video-drafts";
import { captureVideoCover } from "@/lib/video-cover";
import { useMediaFileUrl } from "@/lib/media-file-url";
import { fmt } from "@/lib/timefmt";

function draftStats(draft: VideoDraft) {
  const clips = draft.tracks.flatMap((track) => track.clips);
  const duration = clips.reduce(
    (end, clip) => Math.max(end, (clip.position ?? clip.start) + Math.max(0, clip.end - clip.start)),
    0,
  );
  return { clips: clips.length, tracks: draft.tracks.length, duration };
}

export function VideoEditorHome({
  onOpen,
  onOpenDraft,
}: {
  onOpen: (path: string) => void;
  onOpenDraft: (draft: VideoDraft) => void;
}) {
  const [drafts, setDrafts] = useState<VideoDraft[]>([]);
  const [loading, setLoading] = useState(true);
  const mediaFileUrl = useMediaFileUrl();

  // 挂载时加载草稿(localStorage 读取是同步的,包一层只为统一加载态)
  useEffect(() => {
    setDrafts(listDrafts());
    setLoading(false);
  }, []);

  // 封面懒回填:优先 FFmpeg 已生成的胶片条首帧,浏览器抓帧只作为轻量回退。
  // 这样 WebView 编解码或 canvas 安全限制不会让草稿永久停留在占位图标。
  useEffect(() => {
    if (loading) return;
    const missing = drafts.filter((d) => !d.cover && !d.coverPath);
    if (!missing.length) return;
    let cancelled = false;
    void (async () => {
      for (const d of missing) {
        if (cancelled) return;
        const rels = await api.creationVideoThumbs(d.inputPath).catch(() => [] as string[]);
        if (rels[0] && !cancelled) {
          setDraftCoverPath(d.id, rels[0]);
          setDrafts((prev) => prev.map((x) => (x.id === d.id ? { ...x, coverPath: rels[0] } : x)));
          continue;
        }
        const cover = await captureVideoCover(mediaFileUrl(d.inputPath)).catch(() => null);
        if (!cover || cancelled) continue;
        setDraftCover(d.id, cover);
        setDrafts((prev) => prev.map((x) => (x.id === d.id ? { ...x, cover } : x)));
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [loading, drafts, mediaFileUrl]);

  async function importVideo() {
    const picked = await openDialog({
      multiple: false,
      filters: [{ name: "视频", extensions: ["mp4", "mov", "mkv", "webm", "avi"] }],
    });
    if (typeof picked === "string") onOpen(picked);
  }

  function removeDraft(id: string) {
    deleteDraft(id);
    setDrafts(listDrafts());
    toast.success("草稿已删除");
  }

  return (
    <div className="veltrix-editor-scrollbar min-h-0 flex-1 overflow-y-auto">
      <main className="mx-auto flex w-full max-w-[1480px] flex-col gap-8 px-6 py-7 lg:px-10 lg:py-10">
        <header className="flex flex-col gap-2">
          <span className="text-xs font-medium text-primary">创作工作台</span>
          <h1 className="text-2xl font-semibold tracking-tight text-foreground">视频剪辑</h1>
          <p className="max-w-2xl text-sm leading-relaxed text-muted-foreground">
            从本地视频开始新工程，或继续最近自动保存的剪辑草稿。
          </p>
        </header>

        <section className="grid gap-4 lg:grid-cols-[minmax(0,1fr)_320px]">
          <button
            type="button"
            onClick={() => void importVideo()}
            className="group relative flex min-h-44 overflow-hidden rounded-2xl border border-primary/25 bg-gradient-to-br from-primary/15 via-primary/5 to-transparent p-6 text-left transition-all hover:-translate-y-0.5 hover:border-primary/50 hover:shadow-lg hover:shadow-primary/5 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-primary"
          >
            <span className="absolute -right-10 -top-16 size-52 rounded-full bg-primary/10 blur-3xl transition-transform group-hover:scale-110" />
            <span className="relative flex max-w-lg flex-col justify-between gap-6">
              <span className="flex size-12 items-center justify-center rounded-xl bg-primary text-primary-foreground shadow-sm">
                <Plus className="size-5" />
              </span>
              <span className="flex flex-col gap-1.5">
                <span className="text-lg font-semibold text-foreground">新建剪辑工程</span>
                <span className="text-sm text-muted-foreground">选择一个主视频进入时间线，随后可继续导入更多视频和音频素材。</span>
                <span className="mt-1 flex items-center gap-1.5 text-xs font-medium text-primary">
                  <FolderInput className="size-3.5" />
                  选择本地视频
                </span>
              </span>
            </span>
          </button>

          <aside className="grid grid-cols-3 gap-2 rounded-2xl border border-border bg-card/60 p-4 lg:grid-cols-1">
            <div className="flex items-center gap-3 rounded-xl bg-muted/35 px-3 py-2.5">
              <FileVideo className="size-4 text-primary" />
              <span><strong className="block text-sm font-semibold">{drafts.length}</strong><span className="text-[11px] text-muted-foreground">剪辑草稿</span></span>
            </div>
            <div className="flex items-center gap-3 rounded-xl bg-muted/35 px-3 py-2.5">
              <Layers3 className="size-4 text-primary" />
              <span><strong className="block text-sm font-semibold">{drafts.reduce((n, d) => n + draftStats(d).clips, 0)}</strong><span className="text-[11px] text-muted-foreground">时间线片段</span></span>
            </div>
            <div className="flex items-center gap-3 rounded-xl bg-muted/35 px-3 py-2.5">
              <Clock3 className="size-4 text-primary" />
              <span><strong className="block text-sm font-semibold">自动</strong><span className="text-[11px] text-muted-foreground">本机保存</span></span>
            </div>
          </aside>
        </section>

        <section className="flex min-h-60 flex-col">
          <div className="flex items-end justify-between border-b border-border pb-3">
            <div>
              <h2 className="text-base font-semibold text-foreground">最近草稿</h2>
              <p className="mt-1 text-xs text-muted-foreground">点击封面继续上次编辑，草稿按更新时间排列。</p>
            </div>
            <button
              type="button"
              title="刷新草稿"
              aria-label="刷新草稿"
              onClick={() => setDrafts(listDrafts())}
              className="inline-flex size-8 items-center justify-center rounded-lg border border-border text-muted-foreground transition-colors hover:bg-accent hover:text-foreground"
            >
              <RotateCw className="size-3.5" />
            </button>
          </div>
          {loading ? (
            <div className="flex min-h-52 items-center justify-center text-muted-foreground"><Loader2 className="size-5 animate-spin" /></div>
          ) : drafts.length === 0 ? (
            <div className="mt-4 flex min-h-52 flex-col items-center justify-center gap-3 rounded-2xl border border-dashed border-border bg-muted/10 text-center">
              <span className="flex size-12 items-center justify-center rounded-full bg-muted"><Film className="size-5 text-muted-foreground" /></span>
              <span><strong className="block text-sm font-medium">还没有剪辑草稿</strong><span className="mt-1 block text-xs text-muted-foreground">创建工程后，编辑进度会自动出现在这里。</span></span>
            </div>
          ) : (
            <div className="mt-4 grid grid-cols-[repeat(auto-fill,minmax(260px,1fr))] gap-4">
              {drafts.map((draft) => {
                const stats = draftStats(draft);
                const cover = draft.cover ?? (draft.coverPath ? mediaFileUrl(draft.coverPath) : undefined);
                return (
                  <article
                    key={draft.id}
                    className="group relative overflow-hidden rounded-xl border border-border bg-card text-left transition-all hover:-translate-y-0.5 hover:border-primary/40 hover:shadow-lg hover:shadow-black/10"
                  >
                    <button
                      type="button"
                      aria-label={`继续剪辑 ${draft.name}`}
                      onClick={() => onOpenDraft(draft)}
                      className="block w-full text-left focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-primary"
                    >
                      <span className="relative block aspect-video overflow-hidden bg-muted/35">
                        {cover ? (
                          <img src={cover} alt={`${draft.name} 视频封面`} className="h-full w-full object-cover transition-transform duration-300 group-hover:scale-[1.025]" draggable={false} />
                        ) : (
                          <span className="flex h-full items-center justify-center"><Loader2 className="size-6 animate-spin text-muted-foreground/60" /></span>
                        )}
                        <span className="absolute inset-0 bg-gradient-to-t from-black/65 via-transparent to-black/10" />
                        <span className="absolute left-2.5 top-2.5 rounded-md bg-black/55 px-2 py-1 text-[10px] text-white/90 backdrop-blur-sm">自动保存</span>
                        <span className="absolute inset-0 flex items-center justify-center opacity-0 transition-opacity group-hover:opacity-100">
                          <span className="flex size-11 items-center justify-center rounded-full bg-white/90 text-black shadow-lg"><Play className="ml-0.5 size-4 fill-current" /></span>
                        </span>
                        <span className="absolute bottom-2.5 left-2.5 rounded bg-black/60 px-1.5 py-0.5 font-mono text-[10px] text-white">{fmt(stats.duration)}</span>
                      </span>
                      <span className="flex items-start gap-2 p-3">
                        <span className="min-w-0 flex-1">
                          <strong className="block truncate text-sm font-medium text-foreground" title={draft.name}>{draft.name}</strong>
                          <span className="mt-1 block text-[11px] text-muted-foreground">{stats.tracks} 条轨道 · {stats.clips} 个片段</span>
                        </span>
                        <time className="shrink-0 text-[10px] text-muted-foreground">{format(new Date(draft.updatedAt * 1000), "MM-dd HH:mm")}</time>
                      </span>
                    </button>
                    <button
                      type="button"
                      title="删除草稿"
                      aria-label={`删除草稿 ${draft.name}`}
                      onClick={() => removeDraft(draft.id)}
                      className="absolute right-2.5 top-2.5 z-10 inline-flex size-8 items-center justify-center rounded-lg bg-black/55 text-white/80 opacity-0 backdrop-blur-sm transition-all hover:bg-destructive hover:text-destructive-foreground group-hover:opacity-100 focus-visible:opacity-100"
                    ><Trash2 className="size-3.5" /></button>
                  </article>
                );
              })}
            </div>
          )}
        </section>
      </main>
    </div>
  );
}
