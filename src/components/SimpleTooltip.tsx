import { type ReactNode } from "react";
import {
  Tooltip,
  TooltipContent,
  TooltipTrigger,
} from "@/components/ui/tooltip";

// 全站统一的 Tooltip 封装:一行包裹任意元素,免去每处重复 Trigger/Content。
// 约定:本项目所有提示气泡一律用此组件(基于 shadcn Tooltip),不再用原生 title。
// 全局 TooltipProvider 已在入口挂载,故此处无需再包 Provider。
export function SimpleTooltip({
  content,
  children,
  side = "top",
}: {
  content: ReactNode;
  children: ReactNode;
  side?: "top" | "right" | "bottom" | "left";
}) {
  // 无内容时直接透传,避免渲染空气泡
  if (!content) return <>{children}</>;
  return (
    <Tooltip>
      <TooltipTrigger asChild>{children}</TooltipTrigger>
      <TooltipContent side={side}>{content}</TooltipContent>
    </Tooltip>
  );
}
