import { memo } from "react";
import { Loader2, RefreshCw } from "lucide-react";
import type { ContentView } from "@/lib/api";
import { StepBadge } from "@/components/StepBadge";
import { SimpleTooltip } from "@/components/SimpleTooltip";

export const MediaStatusBadge = memo(function MediaStatusBadge({
  c,
  retrying,
  onRetry,
}: {
  c: ContentView;
  retrying: boolean;
  onRetry: () => void;
}) {
  if (retrying) {
    return (
      <span className="inline-flex items-center gap-1 whitespace-nowrap rounded-md bg-secondary px-2 py-0.5 text-[11px] font-medium text-secondary-foreground">
        <Loader2 className="size-3.5 animate-spin" />
        拉取中
      </span>
    );
  }

  const isVideo = c.kind === "video";
  const transcriptState: boolean | null = c.transcript
    ? true
    : c.transcriptError
      ? false
      : null;
  const imageState: boolean | null =
    c.imageTotal == null || c.imageTotal === 0
      ? c.mediaStatus === "success"
        ? true
        : c.mediaStatus === "failed"
          ? false
          : null
      : c.imageDone === c.imageTotal
        ? true
        : (c.imageDone ?? 0) > 0
          ? null
          : false;

  return (
    <div className="flex flex-wrap items-center gap-1">
      {isVideo ? (
        <>
          <StepBadge label="视频" state={c.videoDownloaded} errorTip={c.mediaError} />
          <StepBadge label="音频" state={c.audioExtracted} errorTip={c.mediaError} />
          <StepBadge label="文案" state={transcriptState} errorTip={c.transcriptError} />
        </>
      ) : (
        <StepBadge
          label={
            c.imageTotal != null && c.imageTotal > 0
              ? `图片 ${c.imageDone ?? 0}/${c.imageTotal}`
              : "素材"
          }
          state={imageState}
          errorTip={c.mediaError}
        />
      )}
      {c.commentCollected === true && <StepBadge label="评论" state={true} />}
      {c.intentAnalyzed === true && <StepBadge label="意向" state={true} />}
      {c.mediaStatus === "failed" && (
        <SimpleTooltip content="点击重新拉取素材">
          <button
            type="button"
            onClick={onRetry}
            className="inline-flex cursor-pointer items-center gap-0.5 whitespace-nowrap rounded bg-rose-100 px-1.5 py-0.5 text-[10px] font-medium text-rose-700 transition-colors hover:bg-rose-200 dark:bg-rose-950/60 dark:text-rose-300 dark:hover:bg-rose-900/60"
          >
            <RefreshCw className="size-3" />
            重试
          </button>
        </SimpleTooltip>
      )}
    </div>
  );
});
