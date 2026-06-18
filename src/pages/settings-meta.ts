// SettingsPage 的共享基础:导航分组、厂商类型、清空确认词、字节格式化。
// 从 SettingsPage.tsx 拆出,作为后续 section 组件拆分的共享依赖(尤其 Provider 类型)。
import { AudioLines, Bot, Brain, Layers, NotebookPen, Settings2, Smartphone, Sparkles } from "lucide-react";

export const SECTION_GROUPS = [
  {
    title: "基础设置",
    items: [
      { key: "general", label: "常规", icon: Settings2 },
      { key: "remote-control", label: "远程控制", icon: Smartphone },
      { key: "obsidian", label: "Obsidian", icon: NotebookPen },
    ],
  },
  {
    title: "AI 配置",
    items: [
      { key: "providers", label: "模型厂商", icon: Bot },
      { key: "role-models", label: "角色模型", icon: Layers },
      { key: "transcription", label: "语音转写", icon: AudioLines },
      { key: "intent", label: "意向分析", icon: Sparkles },
      { key: "memory", label: "AI 记忆", icon: Brain },
    ],
  },
] as const;
export type SectionKey = (typeof SECTION_GROUPS)[number]["items"][number]["key"];


export interface Provider {
  id: string;
  code: string;
  name: string;
  apiUrl: string;
  apiKey: string;
  models: string; // 每行一个模型
}

// 模型厂商预设(code/name/apiUrl/asr)由后端 list_provider_capabilities 提供(单一真相源):
// 新增厂商下拉、不可重复添加、语音转写按 ASR 过滤都据此,前端不再硬编码厂商清单。
export type ProviderPreset = {
  code: string;
  name: string;
  apiUrl: string;
  asr: boolean;
};

// 清空数据需输入的确认词
export const CLEAR_CONFIRM_TEXT = "清空数据";

// 字节数格式化为可读大小
export function formatBytes(bytes: number): string {
  if (bytes <= 0) return "0 B";
  const units = ["B", "KB", "MB", "GB", "TB"];
  let value = bytes;
  let i = 0;
  while (value >= 1024 && i < units.length - 1) {
    value /= 1024;
    i += 1;
  }
  return `${value.toFixed(i === 0 ? 0 : 1)} ${units[i]}`;
}
