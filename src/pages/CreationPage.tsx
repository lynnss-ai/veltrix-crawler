// 创作工具页(创作工作区,由左侧菜单进入):tool 指定渲染哪个工具。
// 文案撰写 / 素材管理前期占位，视频剪辑已迁移到独立保存分支。
import { FileText, Images } from "lucide-react";

export type CreationTool = "copy" | "assets";

const TOOL_META: Record<CreationTool, { label: string; icon: typeof FileText }> = {
  copy: { label: "文案撰写", icon: FileText },
  assets: { label: "素材管理", icon: Images },
};

export function CreationPage({ tool }: { tool: CreationTool }) {
  const meta = TOOL_META[tool];
  return (
    <div className="flex flex-1 flex-col items-center justify-center gap-2 text-muted-foreground">
      <meta.icon className="size-8 opacity-40" />
      <span className="text-sm">{meta.label} · 建设中</span>
    </div>
  );
}
