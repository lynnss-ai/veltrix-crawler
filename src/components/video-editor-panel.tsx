// 素材面板(剪映式):大类 tab 栏 + 中类/小类菜单 + 内容区。
// 真实内容:素材-片段(当前工程片段列表)/ 素材-导入(打开视频)/ 素材-导出记录(后端导出目录扫描);
// 转场-预设(点击应用到属性面板的导出转场设置)。其余大类暂为占位,接入时往 GROUPS 挂节点、
// 在 renderContent 加分支即可。智能包装 / 数字人不上(需求明确移除)。
import { useEffect, useState } from "react";
import { format } from "date-fns";
import {
  Captions,
  ChevronDown,
  ChevronRight,
  Download,
  Film,
  FolderInput,
  LayoutTemplate,
  Music,
  Palette,
  Scissors,
  SlidersHorizontal,
  Sticker,
  Trash2,
  Type,
  Wand2,
} from "lucide-react";
import { toast } from "sonner";

import { api } from "@/lib/api";
import type { EditorTrack, ExportItem } from "@/lib/api-types";
import { fmt } from "@/lib/timefmt";

type TransitionKind = "none" | "dissolve" | "fade";

const TABS = [
  { key: "media", label: "素材", icon: Film },
  { key: "audio", label: "音频", icon: Music },
  { key: "text", label: "文本", icon: Type },
  { key: "sticker", label: "贴纸", icon: Sticker },
  { key: "effect", label: "特效", icon: Wand2 },
  { key: "transition", label: "转场", icon: Scissors },
  { key: "subtitle", label: "字幕", icon: Captions },
  { key: "filter", label: "滤镜", icon: Palette },
  { key: "adjust", label: "调节", icon: SlidersHorizontal },
  { key: "template", label: "模板", icon: LayoutTemplate },
] as const;

type TabKey = (typeof TABS)[number]["key"];

interface GroupNode {
  key: string;
  label: string;
  children?: { key: string; label: string }[];
}

const GROUPS: Record<TabKey, GroupNode[]> = {
  media: [
    { key: "clips", label: "片段" },
    { key: "import", label: "导入", children: [{ key: "local", label: "本地视频" }] },
    { key: "exports", label: "导出记录" },
  ],
  transition: [
    {
      key: "preset",
      label: "预设",
      children: [
        { key: "none", label: "无" },
        { key: "dissolve", label: "叠化" },
        { key: "fade", label: "淡黑" },
      ],
    },
  ],
  audio: [{ key: "default", label: "默认" }],
  text: [{ key: "default", label: "默认" }],
  sticker: [{ key: "default", label: "默认" }],
  effect: [{ key: "default", label: "默认" }],
  subtitle: [{ key: "default", label: "默认" }],
  filter: [{ key: "default", label: "默认" }],
  adjust: [{ key: "default", label: "默认" }],
  template: [{ key: "default", label: "默认" }],
};

// 字节数 → 可读大小
function fmtSize(bytes: number): string {
  if (bytes >= 1 << 20) return `${(bytes / (1 << 20)).toFixed(1)} MB`;
  if (bytes >= 1 << 10) return `${(bytes / (1 << 10)).toFixed(0)} KB`;
  return `${bytes} B`;
}

// 导出记录内容(首次展开时拉取;点击条目打开所在文件夹)
function ExportsContent() {
  const [items, setItems] = useState<ExportItem[] | null>(null);
  useEffect(() => {
    api
      .creationListExports()
      .then(setItems)
      .catch((e) => toast.error(`读取导出记录失败: ${e}`));
  }, []);
  if (items === null) {
    return <p className="py-6 text-center text-[11px] text-muted-foreground">读取中…</p>;
  }
  if (!items.length) {
    return (
      <p className="py-6 text-center text-[11px] leading-relaxed text-muted-foreground">
        暂无导出记录
        <br />
        属性面板「导出」生成的成片会出现在这里
      </p>
    );
  }
  return (
    <div className="flex flex-col gap-1.5">
      {items.map((it) => (
        <button
          key={it.name}
          type="button"
          title="打开所在文件夹"
          onClick={() => void api.revealPath(it.path).catch((e) => toast.error(`打开失败: ${e}`))}
          className="flex items-center gap-2 rounded-md border border-border px-2 py-1.5 text-left text-xs transition-colors hover:border-primary/40 hover:bg-accent/40"
        >
          <Download className="size-3.5 shrink-0 text-muted-foreground" />
          <span className="flex min-w-0 flex-1 flex-col gap-0.5">
            <span className="truncate">{it.name}</span>
            <span className="text-[10px] text-muted-foreground">
              {fmtSize(it.size)} · {format(new Date(it.createdAt * 1000), "MM-dd HH:mm")}
            </span>
          </span>
        </button>
      ))}
    </div>
  );
}

export function MaterialPanel({
  tracks,
  onRemoveClip,
  onImport,
  transitionKind,
  onTransitionKind,
}: {
  tracks: EditorTrack[];
  onRemoveClip: (trackId: string, clipId: string) => void;
  onImport: () => void;
  transitionKind: TransitionKind;
  onTransitionKind: (kind: TransitionKind) => void;
}) {
  const [tab, setTab] = useState<TabKey>("media");
  const [sel, setSel] = useState("clips");
  const [expanded, setExpanded] = useState<Set<string>>(new Set(["import", "preset"]));
  const groups = GROUPS[tab];
  const totalClips = tracks.reduce((n, t) => n + t.clips.length, 0);

  function switchTab(next: TabKey) {
    setTab(next);
    setSel(GROUPS[next][0].key);
  }

  function toggleExpand(key: string) {
    setExpanded((prev) => {
      const next = new Set(prev);
      if (next.has(key)) next.delete(key);
      else next.add(key);
      return next;
    });
  }

  function renderContent() {
    if (tab === "media" && sel === "clips") {
      return (
        <div className="flex flex-col gap-1.5">
          {totalClips === 0 && (
            <p className="py-6 text-center text-[11px] leading-relaxed text-muted-foreground">
              在激活轨上框选后
              <br />
              点「+」添加片段
            </p>
          )}
          {tracks
            .filter((t) => t.clips.length > 0)
            .map((t) => (
              <div key={t.id} className="flex flex-col gap-1.5">
                <span className="text-[10px] text-muted-foreground/70">{t.name}</span>
                {t.clips.map((c, i) => (
                  <div
                    key={c.id}
                    className="group flex items-center gap-2 rounded-md border border-border px-2 py-1 text-xs"
                  >
                    <span className="text-muted-foreground">片段 {i + 1}</span>
                    <span className="truncate font-mono tabular-nums">
                      {fmt(c.start)} ~ {fmt(c.end)}
                    </span>
                    <button
                      type="button"
                      title="删除片段"
                      onClick={() => onRemoveClip(t.id, c.id)}
                      className="ml-auto inline-flex size-6 shrink-0 items-center justify-center rounded-full text-muted-foreground transition-colors hover:bg-accent hover:text-destructive"
                    >
                      <Trash2 className="size-3" />
                    </button>
                  </div>
                ))}
              </div>
            ))}
        </div>
      );
    }
    if (tab === "media" && sel === "local") {
      return (
        <button
          type="button"
          onClick={onImport}
          className="group flex w-full items-center gap-3 rounded-xl border border-dashed border-border px-4 py-5 text-left transition-colors hover:border-primary/50 hover:bg-accent/20"
        >
          <span className="flex size-11 shrink-0 items-center justify-center rounded-full bg-primary/10 transition-colors group-hover:bg-primary/20">
            <FolderInput className="size-5 text-primary" />
          </span>
          <span className="flex flex-col gap-0.5">
            <span className="text-xs font-medium text-foreground">打开本地视频</span>
            <span className="text-[11px] text-muted-foreground">
              支持 mp4 / mov / mkv / webm / avi
            </span>
          </span>
        </button>
      );
    }
    if (tab === "media" && sel === "exports") {
      return <ExportsContent />;
    }
    if (tab === "transition") {
      const presets: { kind: TransitionKind; label: string; desc: string }[] = [
        { kind: "none", label: "无", desc: "片段直接拼接(流拷贝,最快)" },
        { kind: "dissolve", label: "叠化", desc: "前后画面交叉溶解过渡" },
        { kind: "fade", label: "淡黑", desc: "经黑场淡出淡入过渡" },
      ];
      return (
        <div className="flex flex-col gap-1.5">
          {presets.map((p) => (
            <button
              key={p.kind}
              type="button"
              onClick={() => {
                onTransitionKind(p.kind);
                toast.success(`导出转场已设为「${p.label}」`);
              }}
              className={`flex items-center gap-2 rounded-md border px-2.5 py-2 text-left text-xs transition-colors ${
                transitionKind === p.kind
                  ? "border-primary bg-primary/10"
                  : "border-border hover:border-primary/40 hover:bg-accent/40"
              }`}
            >
              <Scissors className="size-3.5 shrink-0 text-muted-foreground" />
              <span className="flex min-w-0 flex-1 flex-col gap-0.5">
                <span className={transitionKind === p.kind ? "text-primary" : ""}>{p.label}</span>
                <span className="text-[10px] text-muted-foreground">{p.desc}</span>
              </span>
              {transitionKind === p.kind && (
                <span className="text-[10px] text-primary">使用中</span>
              )}
            </button>
          ))}
          <p className="mt-1 text-[10px] leading-relaxed text-muted-foreground/70">
            预设即属性面板「导出」的转场设置;转场时长在属性面板调
          </p>
        </div>
      );
    }
    return (
      <div className="flex h-full flex-col items-center justify-center gap-1.5 text-muted-foreground">
        <Film className="size-6 opacity-40" />
        <span className="text-[11px]">
          {TABS.find((t) => t.key === tab)?.label} · 建设中
        </span>
      </div>
    );
  }

  return (
    <div className="flex min-h-0 flex-1 flex-col">
      {/* 大类 tab 栏(图标 + 名称,横向滚动) */}
      <div className="flex shrink-0 gap-0.5 overflow-x-auto border-b border-border px-1">
        {TABS.map((t) => (
          <button
            key={t.key}
            type="button"
            onClick={() => switchTab(t.key)}
            className={`flex w-14 shrink-0 flex-col items-center gap-0.5 rounded-t-md px-1 py-1.5 transition-colors ${
              tab === t.key ? "text-primary" : "text-muted-foreground hover:text-foreground"
            }`}
          >
            <t.icon className="size-4" />
            <span className="text-[10px]">{t.label}</span>
          </button>
        ))}
      </div>
      <div className="flex min-h-0 flex-1">
        {/* 中类 / 小类菜单 */}
        <div className="w-24 shrink-0 overflow-y-auto border-r border-border p-1.5">
          {groups.map((g) => (
            <div key={g.key}>
              <button
                type="button"
                onClick={() => {
                  setSel(g.key);
                  if (g.children) toggleExpand(g.key);
                }}
                className={`flex w-full items-center gap-1 rounded-md px-2 py-1.5 text-xs transition-colors ${
                  sel === g.key
                    ? "bg-accent text-foreground"
                    : "text-muted-foreground hover:bg-accent/60 hover:text-foreground"
                }`}
              >
                {g.label}
                {g.children &&
                  (expanded.has(g.key) ? (
                    <ChevronDown className="ml-auto size-3" />
                  ) : (
                    <ChevronRight className="ml-auto size-3" />
                  ))}
              </button>
              {g.children &&
                expanded.has(g.key) &&
                g.children.map((c) => (
                  <button
                    key={c.key}
                    type="button"
                    onClick={() => setSel(c.key)}
                    className={`flex w-full items-center rounded-md py-1.5 pl-6 pr-2 text-[11px] transition-colors ${
                      sel === c.key
                        ? "text-primary"
                        : "text-muted-foreground hover:text-foreground"
                    }`}
                  >
                    {c.label}
                  </button>
                ))}
            </div>
          ))}
        </div>
        {/* 内容区 */}
        <div className="min-w-0 flex-1 overflow-y-auto p-2.5">{renderContent()}</div>
      </div>
    </div>
  );
}
