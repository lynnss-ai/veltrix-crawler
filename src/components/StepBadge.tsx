// 三态步骤状态小标:true=按类型彩色✓ / false=红✗ / null=灰待处理。
// 素材状态、任务详情等页面复用。
import { memo } from "react";
import { CheckCircle2, Clock, XCircle } from "lucide-react";
import { SimpleTooltip } from "@/components/SimpleTooltip";

const STEP_SUCCESS_CLS: Record<string, string> = {
  视频: "bg-sky-100 text-sky-700 dark:bg-sky-950/60 dark:text-sky-300",
  音频: "bg-violet-100 text-violet-700 dark:bg-violet-950/60 dark:text-violet-300",
  文案: "bg-amber-100 text-amber-700 dark:bg-amber-950/60 dark:text-amber-300",
  图片: "bg-teal-100 text-teal-700 dark:bg-teal-950/60 dark:text-teal-300",
  评论: "bg-blue-100 text-blue-700 dark:bg-blue-950/60 dark:text-blue-300",
  意向: "bg-fuchsia-100 text-fuchsia-700 dark:bg-fuchsia-950/60 dark:text-fuchsia-300",
};

function stepSuccessCls(label: string): string {
  for (const [key, cls] of Object.entries(STEP_SUCCESS_CLS)) {
    if (label.startsWith(key)) return cls;
  }
  return "bg-emerald-100 text-emerald-700 dark:bg-emerald-950/60 dark:text-emerald-300";
}

export const StepBadge = memo(function StepBadge({
  label,
  state,
  errorTip,
}: {
  label: string;
  state: boolean | null;
  errorTip?: string | null;
}) {
  const cls =
    state === true
      ? stepSuccessCls(label)
      : state === false
        ? "bg-rose-100 text-rose-700 dark:bg-rose-950/60 dark:text-rose-300"
        : "bg-muted text-muted-foreground";
  const Icon = state === true ? CheckCircle2 : state === false ? XCircle : Clock;
  const badge = (
    <span
      className={`inline-flex items-center gap-0.5 whitespace-nowrap rounded px-1.5 py-0.5 text-[10px] font-medium ${cls}`}
    >
      <Icon className="size-3" />
      {label}
    </span>
  );
  return state === false && errorTip ? (
    <SimpleTooltip content={errorTip}>
      <span className="cursor-help">{badge}</span>
    </SimpleTooltip>
  ) : (
    badge
  );
});
