// 视频剪辑首页:居中「打开」导入卡片 + 草稿列表(编辑器自动保存的剪辑进度,
// 点击草稿恢复继续剪辑)。只负责选片,选中后通过 onOpen / onOpenDraft 交给编辑器。
import { useEffect, useState } from "react";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { format } from "date-fns";
import {
  FileVideo,
  Film,
  FolderInput,
  Loader2,
  RotateCw,
  Trash2,
} from "lucide-react";
import { toast } from "sonner";

import { deleteDraft, listDrafts, setDraftCover, type VideoDraft } from "@/lib/video-drafts";
import { captureVideoCover } from "@/lib/video-cover";
import { useMediaFileUrl } from "@/lib/media-file-url";

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

  // 封面懒回填:存量草稿没有封面的,逐个后台抓帧并写回草稿库(串行,避免并发解码占资源)
  useEffect(() => {
    if (loading) return;
    const missing = drafts.filter((d) => !d.cover);
    if (!missing.length) return;
    let cancelled = false;
    void (async () => {
      for (const d of missing) {
        if (cancelled) return;
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
    <div className="flex min-h-0 flex-1 flex-col overflow-y-auto">
      <div className="flex w-full flex-1 flex-col px-6 py-6">
        {/* 打开:导入本地视频(靠左的横排卡片) */}
        <button
          type="button"
          onClick={() => void importVideo()}
          className="group flex w-80 items-center gap-4 rounded-xl border border-dashed border-border px-5 py-6 text-left transition-colors hover:border-primary/50 hover:bg-accent/20"
        >
          <span className="flex size-14 shrink-0 items-center justify-center rounded-full bg-primary/10 transition-colors group-hover:bg-primary/20">
            <FolderInput className="size-6 text-primary" />
          </span>
          <span className="flex flex-col gap-1">
            <span className="text-sm font-medium text-foreground">打开</span>
            <span className="text-xs text-muted-foreground">
              选择本地视频开始剪辑
            </span>
            <span className="text-[11px] text-muted-foreground/70">
              支持 mp4 / mov / mkv / webm / avi
            </span>
          </span>
        </button>

        {/* 草稿列表 */}
        <div className="mt-6 flex items-center justify-between">
          <span className="flex items-center gap-2 text-sm font-medium text-foreground">
            <FileVideo className="size-4 text-muted-foreground" />
            草稿
            {drafts.length > 0 && (
              <span className="rounded-full bg-muted px-1.5 py-0.5 text-[11px] text-muted-foreground">
                {drafts.length}
              </span>
            )}
          </span>
          <button
            type="button"
            title="刷新"
            onClick={() => setDrafts(listDrafts())}
            className="inline-flex size-7 items-center justify-center rounded-full text-muted-foreground transition-colors hover:bg-accent hover:text-foreground"
          >
            <RotateCw className="size-3.5" />
          </button>
        </div>
        {loading ? (
          <div className="flex flex-1 items-center justify-center text-muted-foreground">
            <Loader2 className="size-5 animate-spin" />
          </div>
        ) : drafts.length === 0 ? (
          <div className="mt-3 flex items-center gap-3 rounded-lg border border-dashed border-border px-4 py-6 text-muted-foreground">
            <Film className="size-5 opacity-40" />
            <span className="text-xs">暂无草稿,剪辑中添加的片段会自动保存到这里</span>
          </div>
        ) : (
          <div className="mt-3 grid grid-cols-2 gap-3 sm:grid-cols-3 lg:grid-cols-4 xl:grid-cols-5">
            {drafts.map((d) => (
              <div
                key={d.id}
                role="button"
                tabIndex={0}
                title="继续剪辑"
                onClick={() => onOpenDraft(d)}
                onKeyDown={(e) => e.key === "Enter" && onOpenDraft(d)}
                className="group flex cursor-pointer flex-col overflow-hidden rounded-lg border border-border transition-all hover:border-primary/40 hover:shadow-md hover:shadow-black/20"
              >
                <span className="flex h-28 items-center justify-center overflow-hidden bg-muted/30 transition-colors group-hover:bg-muted/50">
                  {d.cover ? (
                    <img
                      src={d.cover}
                      alt={d.name}
                      className="h-full w-full object-cover"
                      draggable={false}
                    />
                  ) : (
                    <Film className="size-7 text-muted-foreground" />
                  )}
                </span>
                <span className="flex items-center gap-1 px-2.5 py-2">
                  <span className="flex min-w-0 flex-1 flex-col gap-0.5">
                    <span className="truncate text-xs font-medium text-foreground">
                      {d.name}
                    </span>
                    <span className="text-[11px] text-muted-foreground">
                      {d.tracks.reduce((n, t) => n + t.clips.length, 0)} 个片段 ·{" "}
                      {format(new Date(d.updatedAt * 1000), "MM-dd HH:mm")}
                    </span>
                  </span>
                  <button
                    type="button"
                    title="删除草稿"
                    onClick={(e) => {
                      e.stopPropagation();
                      removeDraft(d.id);
                    }}
                    className="inline-flex size-7 shrink-0 items-center justify-center rounded-full text-muted-foreground opacity-0 transition-all hover:bg-accent hover:text-destructive group-hover:opacity-100"
                  >
                    <Trash2 className="size-3.5" />
                  </button>
                </span>
              </div>
            ))}
          </div>
        )}
      </div>
    </div>
  );
}
