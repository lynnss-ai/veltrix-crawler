// 素材面板(剪映式):大类 tab 栏 + 中类/小类菜单 + 内容区。
// 真实内容:素材-片段(当前工程片段列表)/ 素材-导入(打开视频)/ 素材-导出记录(后端导出目录扫描);
// 转场-预设(点击应用到属性面板的导出转场设置),文字/字幕已接入轨道。其余大类暂为占位,接入时往 GROUPS 挂节点、
// 在 renderContent 加分支即可。智能包装 / 数字人不上(需求明确移除)。
import { useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { openPath } from "@tauri-apps/plugin-opener";
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
  Plus,
  Play,
  RefreshCw,
  Repeat2,
  Scissors,
  SlidersHorizontal,
  Sticker,
  Trash2,
  Type,
  Wand2,
} from "lucide-react";
import { toast } from "sonner";

import { api } from "@/lib/api";
import type { CreationExportProgress, EditorTrack, ExportItem, VideoTransformInput } from "@/lib/api-types";
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

// 导出记录内容:成片可直接播放 / 定位 / 删除；监听导出完成事件自动刷新。
function ExportsContent({ onExportAgain }: { onExportAgain: () => void }) {
  const [items, setItems] = useState<ExportItem[] | null>(null);
  const [refreshing, setRefreshing] = useState(false);
  const [pendingDelete, setPendingDelete] = useState<string | null>(null);
  const load = async (quiet = false) => {
    if (!quiet) setRefreshing(true);
    try {
      setItems(await api.creationListExports());
    } catch (e) {
      toast.error(`读取导出记录失败: ${e}`);
    } finally {
      setRefreshing(false);
    }
  };
  useEffect(() => {
    void load(true);
    let active = true;
    let dispose: (() => void) | undefined;
    void listen<CreationExportProgress>("creation-export-progress", (event) => {
      if (event.payload.percent >= 100) void load(true);
    }).then((unlisten) => {
      if (active) dispose = unlisten;
      else unlisten();
    });
    return () => {
      active = false;
      dispose?.();
    };
    // load 只依赖稳定的 api 方法；这里仅在面板挂载时注册一次事件。
    // eslint-disable-next-line react-hooks/exhaustive-deps
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
  async function remove(item: ExportItem) {
    if (pendingDelete !== item.name) {
      setPendingDelete(item.name);
      return;
    }
    try {
      await api.creationDeleteExport(item.name);
      setItems((current) => current?.filter((value) => value.name !== item.name) ?? []);
      setPendingDelete(null);
      toast.success("导出记录已删除");
    } catch (error) {
      toast.error(`删除失败: ${error}`);
    }
  }
  return (
    <div className="flex flex-col gap-2">
      <div className="flex items-center justify-between">
        <span className="text-[10px] text-muted-foreground">最近 {items.length} 个成片</span>
        <button
          type="button"
          title="刷新导出记录"
          aria-label="刷新导出记录"
          onClick={() => void load()}
          className="inline-flex size-7 items-center justify-center rounded-md text-muted-foreground transition-colors hover:bg-accent hover:text-foreground"
        >
          <RefreshCw className={`size-3.5 ${refreshing ? "animate-spin" : ""}`} />
        </button>
      </div>
      {items.map((it) => (
        <article
          key={it.name}
          className="group flex flex-col gap-2 rounded-lg border border-border bg-card/40 p-2 text-xs transition-colors hover:border-primary/35"
        >
          <div className="flex min-w-0 items-start gap-2">
            <span className="mt-0.5 flex size-7 shrink-0 items-center justify-center rounded-md bg-primary/10 text-primary">
              <Download className="size-3.5" />
            </span>
            <span className="flex min-w-0 flex-1 flex-col gap-0.5">
              <span className="truncate font-medium" title={it.name}>{it.name}</span>
              <span className="text-[10px] text-muted-foreground">
                {it.width > 0 ? `${it.width}×${it.height} · ` : ""}
                {it.durationSecs > 0 ? `${fmt(it.durationSecs)} · ` : ""}
                {fmtSize(it.size)}
              </span>
              <time className="text-[10px] text-muted-foreground/75">
                {format(new Date(it.createdAt * 1000), "yyyy-MM-dd HH:mm")}
              </time>
            </span>
          </div>
          <div className="grid grid-cols-4 gap-1 border-t border-border/70 pt-2">
            <button
              type="button"
              onClick={() => void openPath(it.path).catch((e) => toast.error(`播放失败: ${e}`))}
              className="inline-flex items-center justify-center gap-1 rounded-md py-1 text-[10px] text-muted-foreground hover:bg-accent hover:text-foreground"
            >
              <Play className="size-3" />播放
            </button>
            <button
              type="button"
              onClick={() => void api.revealPath(it.path).catch((e) => toast.error(`打开失败: ${e}`))}
              className="inline-flex items-center justify-center gap-1 rounded-md py-1 text-[10px] text-muted-foreground hover:bg-accent hover:text-foreground"
            >
              <FolderInput className="size-3" />位置
            </button>
            <button
              type="button"
              title="使用当前工程和参数再次导出"
              onClick={onExportAgain}
              className="inline-flex items-center justify-center gap-1 rounded-md py-1 text-[10px] text-muted-foreground hover:bg-accent hover:text-foreground"
            >
              <Repeat2 className="size-3" />再导出
            </button>
            <button
              type="button"
              onBlur={() => window.setTimeout(() => setPendingDelete(null), 120)}
              onClick={() => void remove(it)}
              className={`inline-flex items-center justify-center gap-1 rounded-md py-1 text-[10px] transition-colors ${
                pendingDelete === it.name
                  ? "bg-destructive text-destructive-foreground"
                  : "text-muted-foreground hover:bg-destructive/10 hover:text-destructive"
              }`}
            >
              <Trash2 className="size-3" />{pendingDelete === it.name ? "确认" : "删除"}
            </button>
          </div>
        </article>
      ))}
    </div>
  );
}

export function MaterialPanel({
  tracks,
  onRemoveClip,
  onImport,
  onImportAudio,
  onAddText,
  transitionKind,
  onTransitionKind,
  onExportAgain,
  onApplyVideoStyle,
}: {
  tracks: EditorTrack[];
  onRemoveClip: (trackId: string, clipId: string) => void;
  onImport: () => void;
  onImportAudio: () => void;
  onAddText: (text: string) => void;
  transitionKind: TransitionKind;
  onTransitionKind: (kind: TransitionKind) => void;
  onExportAgain: () => void;
  onApplyVideoStyle: (patch: Partial<VideoTransformInput>) => void;
}) {
  const [tab, setTab] = useState<TabKey>("media");
  const [sel, setSel] = useState("clips");
  const [expanded, setExpanded] = useState<Set<string>>(new Set(["import", "preset"]));
  const [textDraft, setTextDraft] = useState("");
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
            <span className="text-xs font-medium text-foreground">导入视频素材</span>
            <span className="text-[11px] text-muted-foreground">
              可多选 mp4 / mov / mkv / webm / avi
            </span>
          </span>
        </button>
      );
    }
    if (tab === "media" && sel === "exports") {
      return <ExportsContent onExportAgain={onExportAgain} />;
    }
    if (tab === "audio") {
      const audioTracks = tracks.filter((t) => t.type === "audio");
      const audioCount = audioTracks.reduce((n, t) => n + t.clips.length, 0);
      return (
        <div className="flex flex-col gap-2">
          <button
            type="button"
            onClick={onImportAudio}
            className="group flex w-full items-center gap-3 rounded-xl border border-dashed border-border px-3 py-4 text-left transition-colors hover:border-primary/50 hover:bg-accent/20"
          >
            <span className="flex size-10 shrink-0 items-center justify-center rounded-full bg-primary/10">
              <Music className="size-4.5 text-primary" />
            </span>
            <span className="flex flex-col gap-0.5">
              <span className="text-xs font-medium text-foreground">导入音乐或音效</span>
              <span className="text-[10px] text-muted-foreground">支持 mp3 / wav / m4a / aac / flac / ogg</span>
            </span>
          </button>
          {audioCount > 0 && (
            <div className="flex flex-col gap-1.5">
              {audioTracks.flatMap((track) =>
                track.clips.map((clip) => (
                  <div key={clip.id} className="flex items-center gap-2 rounded-md border border-border px-2 py-1.5 text-xs">
                    <Music className="size-3.5 shrink-0 text-emerald-500" />
                    <span className="min-w-0 flex-1 truncate" title={clip.inputPath}>
                      {clip.inputPath?.split(/[\\/]/).pop() ?? `${track.name}片段`}
                    </span>
                    <button
                      type="button"
                      title="删除音频片段"
                      onClick={() => onRemoveClip(track.id, clip.id)}
                      className="inline-flex size-6 shrink-0 items-center justify-center rounded-full text-muted-foreground hover:bg-accent hover:text-destructive"
                    >
                      <Trash2 className="size-3" />
                    </button>
                  </div>
                )),
              )}
            </div>
          )}
        </div>
      );
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
    if (tab === "filter") {
      const presets = [
        ["none", "原片", "无额外色彩处理"],
        ["vivid", "鲜明", "提升饱和度与层次"],
        ["cinema", "电影", "高对比低饱和"],
        ["warm", "暖阳", "偏暖自然肤色"],
        ["cool", "清冷", "冷色通透氛围"],
        ["mono", "黑白", "单色高对比"],
      ] as const;
      return (
        <div className="grid grid-cols-2 gap-2">
          {presets.map(([filter, label, desc]) => (
            <button key={filter} type="button" onClick={() => onApplyVideoStyle({ filter })} className="group flex flex-col overflow-hidden rounded-lg border border-border text-left hover:border-primary/45">
              <span className={`h-12 w-full ${filter === "vivid" ? "bg-gradient-to-br from-fuchsia-500/70 via-amber-400/70 to-cyan-400/70" : filter === "cinema" ? "bg-gradient-to-br from-amber-950 via-slate-700 to-teal-950" : filter === "warm" ? "bg-gradient-to-br from-orange-300 to-rose-500" : filter === "cool" ? "bg-gradient-to-br from-cyan-300 to-indigo-600" : filter === "mono" ? "bg-gradient-to-br from-white via-zinc-500 to-black" : "bg-gradient-to-br from-slate-300 via-slate-500 to-slate-700"}`} />
              <span className="p-2"><strong className="block text-[11px] font-medium">{label}</strong><span className="text-[9px] text-muted-foreground">{desc}</span></span>
            </button>
          ))}
        </div>
      );
    }
    if (tab === "adjust") {
      return (
        <div className="flex flex-col gap-2">
          <p className="text-[11px] leading-relaxed text-muted-foreground">选中视频片段后，可在右侧属性面板精确调整亮度、对比度、饱和度、色温和色相。</p>
          <button type="button" onClick={() => onApplyVideoStyle({ brightness: 0, contrast: 1, saturation: 1, temperature: 0, hue: 0 })} className="rounded-md border border-border px-3 py-2 text-xs hover:bg-accent">重置所选片段色彩</button>
        </div>
      );
    }
    if (tab === "text" || tab === "subtitle") {
      const textTracks = tracks.filter((track) => track.type === "text");
      const textClips = textTracks.flatMap((track) => track.clips);
      return (
        <div className="flex flex-col gap-3">
          <div className="flex flex-col gap-2 rounded-lg border border-border p-2.5">
            <textarea
              value={textDraft}
              maxLength={500}
              rows={4}
              placeholder="输入字幕或画面文字…"
              onChange={(e) => setTextDraft(e.target.value)}
              className="min-h-20 resize-y rounded-md border border-input bg-transparent px-2.5 py-2 text-xs outline-none transition-colors placeholder:text-muted-foreground focus:border-ring"
            />
            <button
              type="button"
              disabled={!textDraft.trim()}
              onClick={() => {
                const text = textDraft.trim();
                if (!text) return;
                onAddText(text);
                setTextDraft("");
              }}
              className="inline-flex h-8 items-center justify-center gap-1.5 rounded-md bg-primary px-3 text-xs text-primary-foreground transition-opacity disabled:cursor-not-allowed disabled:opacity-45"
            >
              <Plus className="size-3.5" />
              添加到播放头
            </button>
            <span className="text-[10px] leading-relaxed text-muted-foreground">
              默认显示 3 秒；选中字幕片段后可在右侧属性面板修改样式和时长。
            </span>
          </div>
          {textClips.length > 0 && (
            <div className="flex flex-col gap-1.5">
              <span className="text-[10px] text-muted-foreground">工程文字</span>
              {textClips.map((clip) => (
                <div key={clip.id} className="group flex items-center gap-2 rounded-md border border-border px-2 py-1.5 text-xs">
                  <Captions className="size-3.5 shrink-0 text-amber-500" />
                  <span className="min-w-0 flex-1 truncate" title={clip.text}>{clip.text}</span>
                  <button
                    type="button"
                    title="删除字幕"
                    onClick={() => {
                      const track = textTracks.find((candidate) => candidate.clips.some((item) => item.id === clip.id));
                      if (track) onRemoveClip(track.id, clip.id);
                    }}
                    className="inline-flex size-6 shrink-0 items-center justify-center rounded-full text-muted-foreground hover:bg-accent hover:text-destructive"
                  >
                    <Trash2 className="size-3" />
                  </button>
                </div>
              ))}
            </div>
          )}
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
      <div className="veltrix-editor-scrollbar flex shrink-0 gap-0.5 overflow-x-auto border-b border-border px-1">
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
        <div className="veltrix-editor-scrollbar w-24 shrink-0 overflow-y-auto border-r border-border p-1.5">
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
        <div className="veltrix-editor-scrollbar min-w-0 flex-1 overflow-y-auto p-2.5">{renderContent()}</div>
      </div>
    </div>
  );
}
