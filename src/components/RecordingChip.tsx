// 待发送录屏预览条:录屏停止后挂在输入区,提示视频将随下条消息一并加入对话,可点 × 移除。
// 点击条本身全屏预览视频(与历史消息图片预览同款 lightbox)。对话 / 电脑操作 / RPA 三处输入区共用。
import { useState } from "react";
import { convertFileSrc } from "@tauri-apps/api/core";
import { Play, Video, X } from "lucide-react";

export function RecordingChip({
  path,
  onRemove,
}: {
  path: string;
  onRemove: () => void;
}) {
  const [preview, setPreview] = useState(false);

  return (
    <>
      <div className="mb-1 flex items-center gap-2 rounded-lg border border-border/60 bg-muted/40 px-2.5 py-1.5 text-xs text-foreground">
        <button
          type="button"
          onClick={() => setPreview(true)}
          title="预览录屏"
          className="group flex min-w-0 flex-1 items-center gap-2 rounded text-left transition-colors hover:text-primary"
        >
          <Video className="size-4 shrink-0 text-muted-foreground" />
          <span className="min-w-0 flex-1 truncate">
            屏幕录制已就绪,将随消息一并加入对话
          </span>
          <span className="inline-flex shrink-0 items-center gap-1 rounded-full bg-primary/10 px-2 py-0.5 font-medium text-primary transition-colors group-hover:bg-primary/20 group-hover:shadow-sm">
            <Play className="size-3 fill-current" />
            点击预览
          </span>
        </button>
        <button
          type="button"
          onClick={onRemove}
          className="inline-flex size-5 shrink-0 items-center justify-center rounded text-muted-foreground transition-colors hover:bg-accent hover:text-foreground"
          aria-label="移除录屏"
        >
          <X className="size-3.5" />
        </button>
      </div>

      {/* 全屏预览:点背景 / 右上角 × 关闭 */}
      {preview && (
        <div
          className="fixed inset-0 z-[100] flex items-center justify-center bg-black/80 p-10"
          onClick={() => setPreview(false)}
        >
          <button
            type="button"
            className="absolute right-4 top-4 rounded-full bg-white/10 p-2 text-white transition-colors hover:bg-white/20"
            onClick={() => setPreview(false)}
          >
            <X className="size-5" />
          </button>
          <video
            src={convertFileSrc(path)}
            controls
            autoPlay
            className="max-h-full max-w-full rounded-lg"
            onClick={(e) => e.stopPropagation()}
          />
        </div>
      )}
    </>
  );
}
