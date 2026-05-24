import { type LucideIcon } from "lucide-react";

// 统计卡:科技感卡片 + 可选图标徽章 + 进场动画,供看板/采集结果等复用。
interface StatCardProps {
  label: string;
  value: number | string;
  icon?: LucideIcon;
  hint?: string;
  delay?: number; // 进场延迟(ms),用于列表 stagger
}

export function StatCard({ label, value, icon: Icon, hint, delay = 0 }: StatCardProps) {
  return (
    <div
      className="veltrix-card veltrix-enter group p-6"
      style={{ animationDelay: `${delay}ms` }}
    >
      <div className="flex items-center justify-between">
        <div className="text-sm text-muted-foreground">{label}</div>
        {Icon && (
          <div className="rounded-lg bg-indigo-500/10 p-2 text-indigo-600 transition-colors group-hover:bg-indigo-500/20 dark:text-indigo-400">
            <Icon className="h-4 w-4" />
          </div>
        )}
      </div>
      <div className="mt-3 text-3xl font-semibold text-foreground">{value}</div>
      {hint && <div className="mt-1 text-xs text-muted-foreground">{hint}</div>}
    </div>
  );
}
