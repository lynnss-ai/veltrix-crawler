import { type ReactNode } from "react";
import { Badge } from "@/components/ui/badge";
import { cn } from "@/lib/utils";

// 通用状态徽章:按语义色 tone 渲染,供平台/账号/用户等各处状态复用。
export type StatusTone = "success" | "neutral" | "warning" | "danger" | "info";

const TONE_CLASS: Record<StatusTone, string> = {
  success:
    "border-transparent bg-emerald-500/15 text-emerald-600 dark:text-emerald-400",
  neutral: "border-transparent bg-muted text-muted-foreground",
  warning:
    "border-transparent bg-amber-500/15 text-amber-600 dark:text-amber-400",
  danger: "border-transparent bg-red-500/15 text-red-600 dark:text-red-400",
  info: "border-transparent bg-indigo-500/15 text-indigo-600 dark:text-indigo-300",
};

interface StatusBadgeProps {
  tone: StatusTone;
  children: ReactNode;
  className?: string;
}

export function StatusBadge({ tone, children, className }: StatusBadgeProps) {
  return (
    <Badge variant="outline" className={cn(TONE_CLASS[tone], className)}>
      {children}
    </Badge>
  );
}
