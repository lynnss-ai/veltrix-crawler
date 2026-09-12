// 创作工具页(创作工作区,由左侧菜单进入):tool 指定渲染哪个工具。
// 「视频剪辑」已可用:剪辑工作台铺满整页;文案撰写 / 素材管理前期占位,建设中。
import { Clapperboard, FileText, Images } from "lucide-react";

import { VideoEditorPanel } from "@/components/video-editor";

export type CreationTool = "video" | "copy" | "assets";

const TOOL_META: Record<CreationTool, { label: string; icon: typeof Clapperboard }> = {
  video: { label: "视频剪辑", icon: Clapperboard },
  copy: { label: "文案撰写", icon: FileText },
  assets: { label: "素材管理", icon: Images },
};

export function CreationPage({ tool }: { tool: CreationTool }) {
  if (tool === "video") {
    // 剪辑工作台铺满整页(上:左 片段列表 / 中 预览 / 右 播放控制三卡片,下:多轨道时间轴整卡)
    // 页面容器已是 p-2.5(10px),直接铺满即可,外层间距与卡片间距一致
    return (
      <div className="flex min-h-0 flex-1 overflow-hidden">
        <VideoEditorPanel />
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
