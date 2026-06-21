// 待发送录屏预览条:录屏停止后挂在输入区,提示视频将随下条消息一并加入对话,可点 × 移除。
// 对话 / 电脑操作 / RPA 三处输入区共用。
import { Video, X } from "lucide-react";

export function RecordingChip({ onRemove }: { onRemove: () => void }) {
  return (
    <div className="mb-1 flex items-center gap-2 rounded-lg border border-border/60 bg-muted/40 px-2.5 py-1.5 text-xs text-foreground">
      <Video className="size-4 shrink-0 text-muted-foreground" />
      <span className="min-w-0 flex-1 truncate">
        屏幕录制已就绪,将随消息一并加入对话
      </span>
      <button
        type="button"
        onClick={onRemove}
        className="inline-flex size-5 shrink-0 items-center justify-center rounded text-muted-foreground transition-colors hover:bg-accent hover:text-foreground"
        aria-label="移除录屏"
      >
        <X className="size-3.5" />
      </button>
    </div>
  );
}
