// 统一空状态占位:圆形图标底 + 标题 + 可选描述。各列表无数据时复用,样式一致。
// 默认图标与「任务调度」页空状态一致(Radar)。
import { Radar, type LucideIcon } from "lucide-react";

export function EmptyState({
  icon: Icon = Radar,
  title,
  description,
}: {
  icon?: LucideIcon;
  title: string;
  description?: string;
}) {
  return (
    <div className="flex flex-col items-center justify-center gap-3 py-16 text-center">
      <div className="flex size-14 items-center justify-center rounded-full bg-muted text-muted-foreground">
        <Icon className="size-7" />
      </div>
      <div className="space-y-1">
        <p className="text-sm font-medium text-foreground">{title}</p>
        {description && (
          <p className="text-xs text-muted-foreground">{description}</p>
        )}
      </div>
    </div>
  );
}
