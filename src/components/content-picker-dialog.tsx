// 资产选择弹窗:从「全量库」已采集内容里挑选,带入 AI 对话。
// 文案用途:按内容多选(最多 12 篇)。图片用途:按「张」选(全局最多 12 张),
// 图文相册可内联展开逐张挑选;图源=封面时每条内容按 1 张封面计。
import { useEffect, useMemo, useRef, useState } from "react";
import { convertFileSrc } from "@tauri-apps/api/core";
import {
  ArrowDown,
  ArrowUp,
  ArrowUpDown,
  Check,
  ChevronDown,
  ChevronsUpDown,
  FileText,
  Image as ImageIcon,
  Images,
  Loader2,
  Search,
} from "lucide-react";

import { toast } from "sonner";

import {
  api,
  type ChatAttachment,
  type ContentView,
  type IndustryView,
} from "@/lib/api";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import {
  Command,
  CommandEmpty,
  CommandGroup,
  CommandInput,
  CommandItem,
  CommandList,
} from "@/components/ui/command";
import {
  Popover,
  PopoverContent,
  PopoverTrigger,
} from "@/components/ui/popover";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { EmptyState } from "@/components/EmptyState";

export type AssetPickMode = "copy" | "image";

// 图片逐张挑选回传项:某条内容 + 是否只取封面 + 选中的本地图片位置(图文用)
export interface AssetImagePick {
  content: ContentView;
  coverOnly: boolean;
  indices: number[];
}

// 确认结果:文案回传内容数组,图片回传逐内容的挑选项
export type AssetPickResult =
  | { mode: "copy"; contents: ContentView[] }
  | { mode: "image"; picks: AssetImagePick[] };

// 单次最多选择数:文案 12 篇 / 图片 12 张
const MAX_PICK = 12;

// 列表分批渲染:首屏 + 滚到底每次再加载一批(6 列 × 10 行),避免一次性渲染上千卡片卡顿
const PAGE_SIZE = 60;

// 排序维度:创建时间(采集落库 collectedAt)/ 发布时间(publishedAt),各支持升降序
type SortField = "collected" | "published";
type SortKey = `${SortField}-desc` | `${SortField}-asc`;

// 图片用途的图源筛选(参考图片库):图文=图文内容的图片 / 封面=全部内容的封面
type ImageSource = "image" | "cover";

interface ContentPickerDialogProps {
  open: boolean;
  mode: AssetPickMode;
  /** 父级记忆的本用途已选项;打开时回填勾选(文案=内容 id,图片=图片 key) */
  initialSelectedIds: string[];
  onOpenChange: (open: boolean) => void;
  onPick: (result: AssetPickResult) => void;
}

const MODE_META: Record<
  AssetPickMode,
  { title: string; description: string }
> = {
  copy: {
    title: "选择资产文案",
    description:
      "可多选(最多 12 篇),按「标题 / 文案 / 互动数据 / 地址」结构化插入输入框,多条自动编号",
  },
  image: {
    title: "选择资产图片",
    description:
      "按张选(最多 12 张),图文相册可展开逐张挑选;仅本地已下载素材",
  },
};

// 图片 key:`${内容 id}#${本地图片位置}`(封面图源位置固定 0)
const imageKey = (contentId: string, pos: number) => `${contentId}#${pos}`;
const contentIdOfKey = (key: string) => key.slice(0, key.lastIndexOf("#"));
const posOfKey = (key: string) => Number(key.slice(key.lastIndexOf("#") + 1));

export function ContentPickerDialog({
  open,
  mode,
  initialSelectedIds,
  onOpenChange,
  onPick,
}: ContentPickerDialogProps) {
  const [contents, setContents] = useState<ContentView[]>([]);
  const [industries, setIndustries] = useState<IndustryView[]>([]);
  const [loading, setLoading] = useState(false);
  const [keyword, setKeyword] = useState("");
  // 行业筛选:"__all"=不限;其余为行业名(与 content.industry 比对)。industryOpen 控制可搜索下拉
  const [industryFilter, setIndustryFilter] = useState("__all");
  const [industryOpen, setIndustryOpen] = useState(false);
  const [sortBy, setSortBy] = useState<SortKey>("collected-desc");
  const [imageSource, setImageSource] = useState<ImageSource>("image");
  // 文案用途:内容 id 选中集
  const [selected, setSelected] = useState<Set<string>>(new Set());
  // 图片用途:图片 key 选中集(全局最多 12 张)
  const [selectedImages, setSelectedImages] = useState<Set<string>>(new Set());
  // 图文相册内联展开:当前展开的内容 id;galleryCache 缓存该相册的本地图片 base64(供展示)
  const [expandedId, setExpandedId] = useState<string | null>(null);
  const [galleryCache, setGalleryCache] = useState<
    Record<string, ChatAttachment[]>
  >({});
  const [galleryLoading, setGalleryLoading] = useState<Set<string>>(new Set());
  // 当前已渲染数量(分批);滚到底递增
  const [visibleCount, setVisibleCount] = useState(PAGE_SIZE);
  const isFirstRender = useRef(true);

  // 打开时拉全量库并按父级记忆回填勾选;关闭时清空搜索词与图源
  useEffect(() => {
    if (!open) {
      setKeyword("");
      setImageSource("image");
      setIndustryFilter("__all");
      return;
    }
    if (mode === "copy") {
      setSelected(new Set(initialSelectedIds));
    } else {
      setSelectedImages(new Set(initialSelectedIds));
    }
    setLoading(true);
    api
      .listContents()
      .then(setContents)
      .catch(() => setContents([]))
      .finally(() => setLoading(false));
    // 行业下拉数据源(失败按空,降级为「全部行业」)
    api
      .listIndustries()
      .then(setIndustries)
      .catch(() => setIndustries([]));
    // initialSelectedIds 仅在打开瞬间取一次
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open]);

  // 切换图源时重置图片选择(图文/封面是不同图集,key 含义不同),跳过首渲染避免清掉回填
  useEffect(() => {
    if (isFirstRender.current) {
      isFirstRender.current = false;
      return;
    }
    setSelectedImages(new Set());
    setExpandedId(null);
  }, [imageSource]);

  // 打开 / 切换筛选条件时,分批渲染回到首屏
  useEffect(() => {
    setVisibleCount(PAGE_SIZE);
  }, [open, keyword, sortBy, imageSource, industryFilter]);

  // 图片用途按图源限定范围(均要求有本地封面才能定位本地素材):
  // 图文=图文内容的图片;封面=全部有封面的内容(视频封面也可作图片素材)
  const pickable = useMemo(() => {
    if (mode !== "image") return contents;
    if (imageSource === "image") {
      return contents.filter((c) => c.kind === "image" && !!c.coverPath);
    }
    return contents.filter((c) => !!c.coverPath);
  }, [contents, mode, imageSource]);

  // 行业 + 关键词过滤(行业按 content.industry 精确匹配;关键词命中标题/描述/命中词/作者/平台)
  const filtered = useMemo(() => {
    const kw = keyword.trim().toLowerCase();
    return pickable.filter((c) => {
      if (industryFilter !== "__all" && c.industry !== industryFilter) {
        return false;
      }
      if (!kw) return true;
      return [c.title, c.desc, c.keyword, c.authorNickname, c.platform]
        .filter(Boolean)
        .some((field) => field!.toLowerCase().includes(kw));
    });
  }, [pickable, keyword, industryFilter]);

  // 排序:按创建时间(collectedAt)或发布时间(publishedAt);发布时间缺失按 0 处理(沉底)
  const sorted = useMemo(() => {
    const dir = sortBy.endsWith("-asc") ? 1 : -1;
    const byPublished = sortBy.startsWith("published");
    return [...filtered].sort((a, b) => {
      const av = (byPublished ? a.publishedAt : a.collectedAt) ?? 0;
      const bv = (byPublished ? b.publishedAt : b.collectedAt) ?? 0;
      return (av - bv) * dir;
    });
  }, [filtered, sortBy]);

  function toggleSort(field: SortField) {
    setSortBy((prev) =>
      prev.startsWith(field)
        ? prev.endsWith("-desc")
          ? `${field}-asc`
          : `${field}-desc`
        : `${field}-desc`,
    );
  }

  // 文案用途:内容级多选
  function toggleContent(id: string) {
    setSelected((prev) => {
      const next = new Set(prev);
      if (next.has(id)) {
        next.delete(id);
        return next;
      }
      if (next.size >= MAX_PICK) {
        toast.error(`最多选择 ${MAX_PICK} 篇`);
        return prev;
      }
      next.add(id);
      return next;
    });
  }

  // 图片用途:逐张选,全局额度
  function toggleImage(key: string) {
    setSelectedImages((prev) => {
      const next = new Set(prev);
      if (next.has(key)) {
        next.delete(key);
        return next;
      }
      if (next.size >= MAX_PICK) {
        toast.error(`最多选择 ${MAX_PICK} 张`);
        return prev;
      }
      next.add(key);
      return next;
    });
  }

  // 展开图文相册:首次展开时拉取该相册本地图片 base64 供展示
  async function handleExpand(c: ContentView) {
    if (expandedId === c.id) {
      setExpandedId(null);
      return;
    }
    setExpandedId(c.id);
    if (galleryCache[c.id] || galleryLoading.has(c.id)) return;
    setGalleryLoading((s) => new Set(s).add(c.id));
    try {
      const atts = await api.buildContentAttachments(c.id, false);
      setGalleryCache((m) => ({ ...m, [c.id]: atts }));
    } catch (e) {
      toast.error(`加载图片失败: ${e}`);
    } finally {
      setGalleryLoading((s) => {
        const n = new Set(s);
        n.delete(c.id);
        return n;
      });
    }
  }

  const count = mode === "copy" ? selected.size : selectedImages.size;
  // 分批渲染:只渲染前 visibleCount 个;滚到底再加载下一批
  const visible = sorted.slice(0, visibleCount);
  const hasMore = visibleCount < sorted.length;
  function handleListScroll(e: React.UIEvent<HTMLDivElement>) {
    const el = e.currentTarget;
    if (el.scrollTop + el.clientHeight >= el.scrollHeight - 240) {
      setVisibleCount((c) => (c < sorted.length ? c + PAGE_SIZE : c));
    }
  }

  function confirm() {
    if (count === 0) return;
    if (mode === "copy") {
      onPick({
        mode: "copy",
        contents: pickable.filter((c) => selected.has(c.id)),
      });
      onOpenChange(false);
      return;
    }
    // 图片:按内容聚合选中的图片位置
    const coverOnly = imageSource === "cover";
    const byContent = new Map<string, number[]>();
    for (const key of selectedImages) {
      const id = contentIdOfKey(key);
      const arr = byContent.get(id) ?? [];
      arr.push(posOfKey(key));
      byContent.set(id, arr);
    }
    const picks: AssetImagePick[] = [];
    for (const [id, indices] of byContent) {
      const content = pickable.find((c) => c.id === id);
      if (!content) continue;
      picks.push({ content, coverOnly, indices: indices.sort((a, b) => a - b) });
    }
    onPick({ mode: "image", picks });
    onOpenChange(false);
  }

  const meta = MODE_META[mode];
  const unit = mode === "copy" ? "篇" : "张";

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="flex max-h-[80vh] flex-col gap-3 sm:max-w-4xl">
        <DialogHeader>
          <DialogTitle>{meta.title}</DialogTitle>
          <DialogDescription>{meta.description}</DialogDescription>
        </DialogHeader>

        <div className="flex items-center gap-2">
          {/* 行业筛选(可搜索):对文案 / 图片两种用途都生效 */}
          <Popover open={industryOpen} onOpenChange={setIndustryOpen}>
            <PopoverTrigger asChild>
              <Button
                type="button"
                variant="outline"
                role="combobox"
                aria-expanded={industryOpen}
                className="h-9 w-32 shrink-0 justify-between gap-1 px-2.5 text-xs font-normal"
              >
                <span className="truncate">
                  {industryFilter === "__all" ? "全部行业" : industryFilter}
                </span>
                <ChevronsUpDown className="size-3.5 shrink-0 opacity-50" />
              </Button>
            </PopoverTrigger>
            <PopoverContent className="w-48 p-0" align="start">
              <Command>
                <CommandInput placeholder="搜索行业…" className="h-9" />
                <CommandList>
                  <CommandEmpty>无匹配行业</CommandEmpty>
                  <CommandGroup>
                    <CommandItem
                      value="全部行业"
                      onSelect={() => {
                        setIndustryFilter("__all");
                        setIndustryOpen(false);
                      }}
                    >
                      全部行业
                      <Check
                        className={`ml-auto size-4 ${
                          industryFilter === "__all" ? "opacity-100" : "opacity-0"
                        }`}
                      />
                    </CommandItem>
                    {industries.map((i) => (
                      <CommandItem
                        key={i.id}
                        value={i.name}
                        onSelect={() => {
                          setIndustryFilter(i.name);
                          setIndustryOpen(false);
                        }}
                      >
                        <span className="truncate">{i.name}</span>
                        <Check
                          className={`ml-auto size-4 ${
                            industryFilter === i.name ? "opacity-100" : "opacity-0"
                          }`}
                        />
                      </CommandItem>
                    ))}
                  </CommandGroup>
                </CommandList>
              </Command>
            </PopoverContent>
          </Popover>
          <div className="relative flex-1">
            <Search className="absolute left-2.5 top-1/2 size-4 -translate-y-1/2 text-muted-foreground" />
            <Input
              autoFocus
              value={keyword}
              onChange={(e) => setKeyword(e.target.value)}
              placeholder="搜索标题 / 描述 / 关键词 / 作者"
              className="pl-8"
            />
          </div>
          {/* 两个独立排序按钮(互斥):点已选维度切升降序,点另一个切换维度 */}
          <SortButton
            label="创建时间"
            active={sortBy.startsWith("collected")}
            asc={sortBy === "collected-asc"}
            onClick={() => toggleSort("collected")}
          />
          <SortButton
            label="发布时间"
            active={sortBy.startsWith("published")}
            asc={sortBy === "published-asc"}
            onClick={() => toggleSort("published")}
          />
        </div>

        {/* 图源筛选(仅图片用途)+ 已选计数 */}
        {mode === "image" && (
          <div className="flex items-center gap-2 text-xs">
            <span className="font-medium text-muted-foreground">图源</span>
            <FilterChip
              label="图文"
              active={imageSource === "image"}
              onClick={() => setImageSource("image")}
            />
            <FilterChip
              label="封面"
              active={imageSource === "cover"}
              onClick={() => setImageSource("cover")}
            />
            <span className="ml-auto text-muted-foreground">
              已选 {count}/{MAX_PICK}
            </span>
          </div>
        )}

        {/* py-1:给卡片的边框/选中环留出空间,否则首行顶边会被 overflow 裁掉。onScroll 分批加载 */}
        <div
          onScroll={handleListScroll}
          className="veltrix-thin-scrollbar -mx-1 min-h-0 flex-1 overflow-y-auto px-1 py-1"
        >
          {loading ? (
            <div className="flex h-32 items-center justify-center text-muted-foreground">
              <Loader2 className="size-5 animate-spin" />
            </div>
          ) : filtered.length === 0 ? (
            <EmptyState
              title={pickable.length === 0 ? "没有可选的资产" : "无匹配结果"}
              description={
                pickable.length === 0
                  ? mode === "copy"
                    ? "全量库暂无已采集内容"
                    : "全量库暂无「已下载到本地」的封面/图片,请先到全量库下载素材"
                  : "换个行业或关键词试试"
              }
            />
          ) : mode === "copy" ? (
            <ul className="grid grid-cols-6 gap-2">
              {visible.map((c) => (
                <CopyCard
                  key={c.id}
                  content={c}
                  selected={selected.has(c.id)}
                  onToggle={() => toggleContent(c.id)}
                />
              ))}
            </ul>
          ) : (
            <ul className="grid grid-cols-6 gap-2">
              {visible.map((c) => (
                <ImageItem
                  key={c.id}
                  content={c}
                  coverMode={imageSource === "cover"}
                  selectedImages={selectedImages}
                  expanded={expandedId === c.id}
                  gallery={galleryCache[c.id]}
                  galleryLoading={galleryLoading.has(c.id)}
                  remaining={MAX_PICK - selectedImages.size}
                  onToggleCover={() => toggleImage(imageKey(c.id, 0))}
                  onExpand={() => handleExpand(c)}
                  onToggleImage={(pos) => toggleImage(imageKey(c.id, pos))}
                />
              ))}
            </ul>
          )}
          {/* 分批加载提示:还有更多则提示继续下滑,否则显示总数 */}
          {!loading && filtered.length > 0 && (
            <p className="py-3 text-center text-[11px] text-muted-foreground">
              {hasMore
                ? `下滑加载更多(已显示 ${visible.length}/${sorted.length})`
                : `共 ${sorted.length} 条`}
            </p>
          )}
        </div>

        <DialogFooter>
          <Button variant="outline" onClick={() => onOpenChange(false)}>
            取消
          </Button>
          <Button disabled={count === 0} onClick={confirm}>
            加入到对话{count > 0 ? ` (${count} ${unit})` : ""}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

// 文案卡片:内容级勾选
function CopyCard({
  content,
  selected,
  onToggle,
}: {
  content: ContentView;
  selected: boolean;
  onToggle: () => void;
}) {
  const coverSrc = content.coverPath
    ? convertFileSrc(content.coverPath)
    : content.coverUrl || content.imageUrls[0] || "";
  return (
    <li>
      <button
        type="button"
        onClick={onToggle}
        aria-pressed={selected}
        className={`group relative flex w-full flex-col overflow-hidden rounded-lg border text-left transition-colors ${
          selected
            ? "border-primary ring-2 ring-primary"
            : "hover:border-primary hover:bg-accent"
        }`}
      >
        <CheckBadge selected={selected} />
        <Thumb src={coverSrc} kind={content.kind} />
        <CardMeta content={content} />
      </button>
    </li>
  );
}

// 图片用途的内容项:封面图源=整卡按 1 张封面勾选;图文图源=点开内联展开逐张挑选
function ImageItem({
  content,
  coverMode,
  selectedImages,
  expanded,
  gallery,
  galleryLoading,
  remaining,
  onToggleCover,
  onExpand,
  onToggleImage,
}: {
  content: ContentView;
  coverMode: boolean;
  selectedImages: Set<string>;
  expanded: boolean;
  gallery: ChatAttachment[] | undefined;
  galleryLoading: boolean;
  remaining: number;
  onToggleCover: () => void;
  onExpand: () => void;
  onToggleImage: (pos: number) => void;
}) {
  const coverSrc = content.coverPath
    ? convertFileSrc(content.coverPath)
    : content.coverUrl || content.imageUrls[0] || "";
  // 该内容已选张数(key 前缀匹配)
  let picked = 0;
  selectedImages.forEach((k) => {
    if (contentIdOfKey(k) === content.id) picked += 1;
  });
  const firstSelected = selectedImages.has(imageKey(content.id, 0));
  const total =
    content.imageTotal || content.imageDone || content.imageUrls.length || 1;
  // 封面图源、或只有 1 张图片 → 直接勾选,不展开;多图才点开逐张挑
  const directSelect = coverMode || total <= 1;

  return (
    <>
      <li>
        <button
          type="button"
          onClick={directSelect ? onToggleCover : onExpand}
          aria-pressed={directSelect ? firstSelected : picked > 0}
          className={`group relative flex w-full flex-col overflow-hidden rounded-lg border text-left transition-colors ${
            (directSelect ? firstSelected : picked > 0)
              ? "border-primary ring-2 ring-primary"
              : "hover:border-primary hover:bg-accent"
          }`}
        >
          {directSelect ? (
            <CheckBadge selected={firstSelected} />
          ) : (
            // 图文多图:展开箭头 + 已选/总数;选中部分时角标变主题色作标记
            <span
              className={`absolute right-1.5 top-1.5 z-10 inline-flex items-center gap-0.5 rounded px-1.5 py-0.5 text-[10px] font-medium ${
                picked > 0
                  ? "bg-primary text-primary-foreground"
                  : "bg-black/55 text-white"
              }`}
            >
              <ChevronDown
                className={`size-3 transition-transform ${expanded ? "rotate-180" : ""}`}
              />
              {picked > 0 ? `${picked}/${total}` : total}
            </span>
          )}
          {directSelect && (
            <span className="absolute bottom-1.5 left-1.5 z-10 inline-flex items-center gap-0.5 rounded bg-black/55 px-1.5 py-0.5 text-[10px] font-medium text-white">
              <Images className="size-3" />1
            </span>
          )}
          <Thumb src={coverSrc} kind={content.kind} />
          <CardMeta content={content} />
        </button>
      </li>

      {/* 图文多图相册内联展开(整行):逐张缩略图勾选,受全局剩余额度限制 */}
      {!directSelect && expanded && (
        <li className="col-span-full">
          <div className="rounded-lg border bg-muted/40 p-2">
            {galleryLoading || !gallery ? (
              <div className="flex h-16 items-center justify-center text-muted-foreground">
                <Loader2 className="size-4 animate-spin" />
              </div>
            ) : gallery.length === 0 ? (
              <p className="py-4 text-center text-xs text-muted-foreground">
                该内容暂无已下载到本地的图片
              </p>
            ) : (
              <>
                <p className="mb-1.5 text-[11px] text-muted-foreground">
                  共 {gallery.length} 张{remaining <= 0 ? "(已达 12 张上限)" : `,还可选 ${remaining} 张`}
                </p>
                <div className="grid grid-cols-8 gap-1.5">
                  {gallery.map((att, pos) => {
                    const isSel = selectedImages.has(imageKey(content.id, pos));
                    return (
                      <button
                        type="button"
                        key={pos}
                        onClick={() => onToggleImage(pos)}
                        className={`relative aspect-square overflow-hidden rounded-md border transition-colors ${
                          isSel
                            ? "border-primary ring-2 ring-primary"
                            : "hover:border-primary"
                        }`}
                      >
                        <img
                          src={`data:${att.mime};base64,${att.data}`}
                          alt=""
                          className="size-full object-cover"
                        />
                        <CheckBadge selected={isSel} small />
                      </button>
                    );
                  })}
                </div>
              </>
            )}
          </div>
        </li>
      )}
    </>
  );
}

// 方形复选角标
function CheckBadge({
  selected,
  small,
}: {
  selected: boolean;
  small?: boolean;
}) {
  return (
    <span
      className={`absolute right-1 top-1 z-10 flex items-center justify-center rounded-sm border transition-colors ${
        small ? "size-4" : "size-5"
      } ${
        selected
          ? "border-primary bg-primary text-primary-foreground"
          : "border-white/70 bg-black/30 text-transparent"
      }`}
    >
      <Check className={small ? "size-3" : "size-3.5"} />
    </span>
  );
}

// 方形缩略图(本地优先;无图按类型占位)
function Thumb({ src, kind }: { src: string; kind: ContentView["kind"] }) {
  return (
    <div className="flex aspect-square w-full items-center justify-center overflow-hidden bg-muted">
      {src ? (
        <img
          src={src}
          alt=""
          loading="lazy"
          className="size-full object-cover"
          onError={(e) => {
            e.currentTarget.style.display = "none";
          }}
        />
      ) : kind === "video" ? (
        <FileText className="size-6 text-muted-foreground" />
      ) : (
        <ImageIcon className="size-6 text-muted-foreground" />
      )}
    </div>
  );
}

// 卡片底部:标题 + 平台·作者
function CardMeta({ content }: { content: ContentView }) {
  const primary = content.title?.trim() || content.desc?.trim() || "(无标题)";
  return (
    <div className="min-w-0 p-2">
      <p className="truncate text-xs font-medium">{primary}</p>
      <p className="truncate text-[11px] text-muted-foreground">
        {content.platform} · {content.authorNickname || "未知作者"}
      </p>
    </div>
  );
}

// 排序按钮:点亮表示当前按此维度排序,箭头示方向;未选显示双向箭头
function SortButton({
  label,
  active,
  asc,
  onClick,
}: {
  label: string;
  active: boolean;
  asc: boolean;
  onClick: () => void;
}) {
  return (
    <Button
      type="button"
      variant={active ? "secondary" : "outline"}
      size="sm"
      onClick={onClick}
      className={`h-9 shrink-0 gap-1 px-2.5 text-xs ${
        active ? "border-primary/40 text-foreground" : "text-muted-foreground"
      }`}
    >
      {label}
      {active ? (
        asc ? (
          <ArrowUp className="size-3.5" />
        ) : (
          <ArrowDown className="size-3.5" />
        )
      ) : (
        <ArrowUpDown className="size-3.5 opacity-50" />
      )}
    </Button>
  );
}

// 图源筛选 chip(图文 / 封面)
function FilterChip({
  label,
  active,
  onClick,
}: {
  label: string;
  active: boolean;
  onClick: () => void;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      className={`rounded-full border px-2.5 py-1 text-xs transition-colors ${
        active
          ? "border-primary bg-primary/10 text-primary"
          : "border-border text-muted-foreground hover:bg-accent"
      }`}
    >
      {label}
    </button>
  );
}
