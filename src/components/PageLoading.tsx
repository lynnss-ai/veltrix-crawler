import { cn } from "@/lib/utils";
import type { ComponentProps } from "react";

type PageLoadingProps = {
  className?: string;
  /** 仪表盘首屏用卡片骨架,普通页面使用列表骨架。 */
  variant?: "page" | "dashboard" | "workspace";
};

function Bar({ className, ...props }: ComponentProps<"div">) {
  return (
    <div
      className={cn("animate-pulse rounded-md bg-muted", className)}
      aria-hidden="true"
      {...props}
    />
  );
}

/**
 * 页面级加载骨架。它只覆盖首次取数 / 模块下载,后台刷新保留旧内容,
 * 避免定时刷新时整页闪烁并让用户误以为数据被清空。
 */
export function PageLoading({
  className,
  variant = "page",
}: PageLoadingProps) {
  if (variant === "workspace") {
    return (
      <div
        className={cn("flex min-h-0 flex-1 flex-col gap-4 p-4", className)}
        role="status"
        aria-label="正在加载页面"
        aria-busy="true"
      >
        <div className="flex items-center gap-3 border-b pb-4">
          <Bar className="size-9 rounded-full" />
          <Bar className="h-5 w-40" />
        </div>
        <div className="flex min-h-0 flex-1 gap-4">
          <Bar className="hidden w-56 sm:block" />
          <div className="flex flex-1 flex-col justify-end gap-3 rounded-xl border p-4">
            <Bar className="h-4 w-2/3" />
            <Bar className="h-4 w-1/2" />
            <Bar className="mt-auto h-24 w-full" />
          </div>
        </div>
        <span className="sr-only">正在加载页面</span>
      </div>
    );
  }

  return (
    <div
      className={cn("flex min-h-0 flex-1 flex-col gap-4 p-1", className)}
      role="status"
      aria-label="正在加载页面"
      aria-busy="true"
    >
      {variant === "dashboard" && (
        <div className="grid grid-cols-1 gap-4 sm:grid-cols-3">
          {[0, 1, 2].map((item) => (
            <div key={item} className="space-y-4 rounded-xl border bg-card p-5">
              <div className="flex items-center gap-3">
                <Bar className="size-10 rounded-lg" />
                <Bar className="h-4 w-24" />
              </div>
              <Bar className="h-8 w-32" />
              <Bar className="h-3 w-full" />
            </div>
          ))}
        </div>
      )}
      <div className="min-h-0 flex-1 overflow-hidden rounded-xl border bg-card">
        <div className="flex items-center gap-3 border-b p-4">
          <Bar className="h-9 w-56 max-w-[45%]" />
          <Bar className="ml-auto h-9 w-24" />
        </div>
        <div className="space-y-5 p-5">
          {["78%", "92%", "65%", "86%", "72%", "89%"].map(
            (width, index) => (
              <div key={index} className="flex items-center gap-4">
                <Bar className="size-9 shrink-0 rounded-full" />
                <Bar className="h-4" style={{ width }} />
              </div>
            ),
          )}
        </div>
      </div>
      <span className="sr-only">正在加载页面</span>
    </div>
  );
}
