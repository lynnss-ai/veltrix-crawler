// 内容生产工具页：默认入口是目标驱动的 AI 成片，专业时间线只在结果需要精修时出现。
import { FileText, Images, Sparkles } from "lucide-react";

import { AiVideoStudio } from "@/components/ai-video-studio";

export type CreationTool = "video" | "copy" | "assets";

const TOOL_META: Record<CreationTool, { label: string; icon: typeof Sparkles }> = {
  video: { label: "AI 成片", icon: Sparkles },
  copy: { label: "AI 文案", icon: FileText },
  assets: { label: "素材库", icon: Images },
};

export function CreationPage({ tool }: { tool: CreationTool }) {
  if (tool === "video") {
    return (
      <div className="flex min-h-0 flex-1 overflow-hidden">
        <AiVideoStudio />
      </div>
    );
  }
  const meta = TOOL_META[tool];
  return (
    <div className="flex flex-1 flex-col items-center justify-center gap-2 text-muted-foreground">
      <meta.icon className="size-8 opacity-40" />
      <span className="text-sm">{meta.label} · 建设中</span>
    </div>
  );
}
