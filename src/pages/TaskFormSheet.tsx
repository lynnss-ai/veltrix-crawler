// 任务新建/编辑表单抽屉:从 CollectPage 抽出的独立 Sheet 组件(props 自洽,无闭包捕获)。
import { useEffect, useState } from "react";
import type { FormEvent, ReactNode } from "react";
import { api } from "@/lib/api";
import type { IndustryView, TaskInput } from "@/lib/api";
import { filterMetaFor, extraFiltersFor, COMMENT_TIME_RANGE_META, COMMENT_LIMIT_OPTIONS, TRIGGER_META, DEFAULT_STRATEGY } from "./collect-meta";
import type { TaskTrigger, SortMode, TimeRange, TaskItem, CommentTimeRange, PlatformOption } from "./collect-meta";
import { openUrl } from "@tauri-apps/plugin-opener";
import {
  ArrowDownUp,
  CalendarClock,
  CircleSlash2,
  Clock,
  type LucideIcon,
} from "lucide-react";
import { Separator } from "@/components/ui/separator";
import { toast } from "sonner";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Switch } from "@/components/ui/switch";
import { Textarea } from "@/components/ui/textarea";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { Sheet, SheetContent, SheetDescription, SheetFooter, SheetHeader, SheetTitle } from "@/components/ui/sheet";

// 表单分区小标题:图标 + 文案,统一各组视觉,降低「一长条字段堆叠」的杂乱感
function SectionLabel({ icon: Icon, children }: { icon: LucideIcon; children: ReactNode }) {
  return (
    <div className="flex items-center gap-1.5 text-xs font-semibold text-muted-foreground">
      <Icon className="size-3.5" />
      {children}
    </div>
  );
}

export function TaskFormSheet({
  open,
  initial,
  industries,
  platforms,
  onOpenChange,
  onSubmit,
}: {
  open: boolean;
  initial: TaskItem | null;
  industries: IndustryView[];
  platforms: PlatformOption[];
  onOpenChange: (v: boolean) => void;
  onSubmit: (input: TaskInput) => void;
}) {
  const [name, setName] = useState(initial?.name ?? "");
  const [industry, setIndustry] = useState(initial?.industry ?? "");
  const [platform, setPlatform] = useState(initial?.platform ?? "");
  const [keywordsRaw, setKeywordsRaw] = useState(
    initial?.keywords.join("\n") ?? "",
  );

  // 行业变更时自动从该行业拉关键词填充(可编辑);编辑模式下首次保持原值不覆盖
  const initialIndustryName = initial?.industry ?? "";
  useEffect(() => {
    // 编辑态首次进入 = 用户没主动切换行业,不动 textarea
    if (industry === initialIndustryName && keywordsRaw) return;
    const ind = industries.find((i) => i.name === industry);
    if (!ind) return;
    api
      .listKeywords(ind.id)
      .then((rows) => setKeywordsRaw(rows.map((r) => r.word).join("\n")))
      .catch((e) => console.warn("加载关键词列表失败:", e));
    // 这里有意只跟 industry 变化,不依赖 keywordsRaw,避免循环
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [industry, industries]);
  const [trigger, setTrigger] = useState<TaskTrigger>(
    initial?.trigger ?? "once-now",
  );
  const [scheduledAt, setScheduledAt] = useState<string>(
    initial?.scheduledAt ?? "",
  );
  const [watchIntervalMin, setWatchIntervalMin] = useState<string>(
    String(initial?.watchIntervalMin ?? 30),
  );

  // 采集策略字段
  const [sortMode, setSortMode] = useState<SortMode>(
    initial?.sortMode ?? DEFAULT_STRATEGY.sortMode,
  );
  const [timeRange, setTimeRange] = useState<TimeRange>(
    initial?.timeRange ?? DEFAULT_STRATEGY.timeRange,
  );
  const [perKeywordLimit, setPerKeywordLimit] = useState(
    String(initial?.perKeywordLimit ?? DEFAULT_STRATEGY.perKeywordLimit),
  );
  const [minLikes, setMinLikes] = useState(
    String(initial?.minLikes ?? DEFAULT_STRATEGY.minLikes),
  );
  const [aiExtract, setAiExtract] = useState(
    initial?.aiExtract ?? DEFAULT_STRATEGY.aiExtract,
  );
  const [autoSyncObsidian, setAutoSyncObsidian] = useState(
    initial?.autoSyncObsidian ?? DEFAULT_STRATEGY.autoSyncObsidian,
  );
  // 平台专属额外筛选(抖音:视频时长/搜索范围/内容形式),{维度id: 选中文案},"any"/缺省=不限
  const [extraFilters, setExtraFilters] = useState<Record<string, string>>(
    initial?.extraFilters ?? {},
  );
  // 切换平台时:若当前排序/时间不在该平台支持项内,回退到该平台首项(综合/不限);
  // 额外筛选只保留新平台声明的维度键(切平台即重置,编辑态首进因键合法故保留)
  useEffect(() => {
    const meta = filterMetaFor(platform);
    // 平台无排序/时间筛选项(如快手)时数组为空,不纠正(保留默认 synthetic/any,后端不点击)
    if (meta.sort.length > 0 && !meta.sort.some((o) => o.value === sortMode)) {
      setSortMode(meta.sort[0].value);
    }
    if (meta.time.length > 0 && !meta.time.some((o) => o.value === timeRange)) {
      setTimeRange(meta.time[0].value);
    }
    const dimIds = new Set(extraFiltersFor(platform).map((d) => d.id));
    setExtraFilters((prev) => {
      const next: Record<string, string> = {};
      for (const k of Object.keys(prev)) if (dimIds.has(k)) next[k] = prev[k];
      return next;
    });
    // 仅在平台变化时纠正,不依赖 sortMode/timeRange/extraFilters
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [platform]);
  // ffmpeg 安装检测:null=检测中,true/false=结果;已装则隐藏「点此下载」引导
  const [ffmpegAvailable, setFfmpegAvailable] = useState<boolean | null>(null);
  // Obsidian vault 是否已配置:未配置则禁用「自动同步」开关并强制关闭
  const [obsidianConfigured, setObsidianConfigured] = useState(false);

  // 表单打开时检测 ffmpeg 安装情况与 Obsidian 配置:决定下载引导是否显示 / 同步开关是否可用
  useEffect(() => {
    if (!open) return;
    api
      .checkFfmpeg()
      .then((s) => setFfmpegAvailable(s.available))
      .catch(() => setFfmpegAvailable(false));
    api
      .getObsidianVault()
      .then((vault) => {
        const configured = vault.trim().length > 0;
        setObsidianConfigured(configured);
        // 未配置却已勾选(如编辑旧任务后 vault 被清空)→ 关掉,避免存下无法生效的自动同步
        if (!configured) setAutoSyncObsidian(false);
      })
      .catch(() => setObsidianConfigured(false));
    // 仅在打开时检测一次;setState 恒稳定,无需进依赖
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open]);

  // 评论采集
  const [collectComments, setCollectComments] = useState(
    initial?.collectComments ?? DEFAULT_STRATEGY.collectComments,
  );
  const [commentTimeRange, setCommentTimeRange] = useState<CommentTimeRange>(
    initial?.commentTimeRange ?? DEFAULT_STRATEGY.commentTimeRange,
  );
  const [commentLimit, setCommentLimit] = useState(
    String(initial?.commentLimit ?? DEFAULT_STRATEGY.commentLimit),
  );
  const [analyzeCommentIntent, setAnalyzeCommentIntent] = useState(
    initial?.analyzeCommentIntent ?? DEFAULT_STRATEGY.analyzeCommentIntent,
  );

  function handleSubmit(e: FormEvent) {
    e.preventDefault();
    const trimmedName = name.trim();
    if (!trimmedName) {
      toast.error("请输入任务名");
      return;
    }
    if (!industry) {
      toast.error("请选择所属行业");
      return;
    }
    if (!platform) {
      toast.error("请选择所属平台");
      return;
    }
    // 多行关键词:严格按行分隔,每行 trim 后取非空
    const keywords = keywordsRaw
      .split(/\r?\n/)
      .map((k) => k.trim())
      .filter(Boolean);
    if (keywords.length === 0) {
      toast.error("请至少输入一个关键词");
      return;
    }
    const input: TaskInput = {
      id: initial?.id ?? crypto.randomUUID(),
      name: trimmedName,
      industry,
      platform,
      keywords,
      trigger,
      scheduledAt: trigger === "daily" && scheduledAt ? scheduledAt : null,
      watchIntervalMin:
        trigger === "watching"
          ? Math.max(30, Number(watchIntervalMin) || 30)
          : null,
      sortMode,
      timeRange,
      perKeywordLimit: Math.max(1, Number(perKeywordLimit) || 50),
      minLikes: Math.max(0, Number(minLikes) || 0),
      aiExtract,
      autoSyncObsidian,
      collectComments,
      // 评论参数仅在开启评论采集时有意义;意图分析依赖评论采集,关闭时强制 false
      commentTimeRange,
      commentLimit: collectComments ? Math.max(0, Number(commentLimit) || 0) : 0,
      analyzeCommentIntent: collectComments && analyzeCommentIntent,
      // 只提交真实选中的额外筛选(剔除「不限」/any),落库即一组待点击文案
      extraFilters: Object.fromEntries(
        Object.entries(extraFilters).filter(([, v]) => v && v !== "any"),
      ),
    };
    onSubmit(input);
  }

  // 该平台提供哪些采集筛选(快手无任何筛选 → 整块隐藏);排序/时间为空数组的平台不渲染对应选择器
  const platformSort = filterMetaFor(platform).sort;
  const platformTime = filterMetaFor(platform).time;
  const platformDims = extraFiltersFor(platform);
  const hasFilters =
    platformSort.length > 0 ||
    platformTime.length > 0 ||
    platformDims.length > 0;
  // 表单是否已填值(名称 / 关键词 / 平台 / 行业 任一非空):有内容时点表单外不自动关闭,防误触丢失
  const isDirty =
    name.trim() !== "" ||
    keywordsRaw.trim() !== "" ||
    platform !== "" ||
    industry !== "";

  return (
    <Sheet open={open} onOpenChange={onOpenChange}>
      <SheetContent
        className="flex w-full flex-col gap-0 sm:max-w-[820px]"
        onEscapeKeyDown={(e) => {
          // 有内容时按 Esc 也不关闭(防误触丢内容),仅右上角 × 显式关闭
          if (isDirty) {
            e.preventDefault();
            toast.info("表单有内容,如需关闭请点右上角 ×");
          }
        }}
        onInteractOutside={(e) => {
          // 点击自定义窗口标题栏(最小化 / 最大化 / 关闭 / 拖拽)不视为关闭表单:放行窗口操作,不弹提示、不关闭
          const target = e.detail.originalEvent.target as HTMLElement | null;
          if (target?.closest("[data-app-titlebar]")) {
            e.preventDefault();
            return;
          }
          // 已填内容时,点击遮罩 / 表单外不自动关闭(防误触丢内容);用右上角 × 显式关闭
          if (isDirty) {
            e.preventDefault();
            toast.info("表单有内容,如需关闭请点右上角 ×");
          }
        }}
      >
        <SheetHeader>
          <SheetTitle>{initial ? "编辑采集任务" : "创建采集任务"}</SheetTitle>
          <SheetDescription>
            配置采集目标 + 触发方式;保存后任务进入「等待运行」,可手动启动。
          </SheetDescription>
        </SheetHeader>

        <form
          id="task-form"
          onSubmit={handleSubmit}
          className="flex-1 space-y-4 overflow-y-auto px-4 py-2"
        >
          <div className="space-y-1.5">
            <Label htmlFor="task-name">
              任务名 <span className="text-destructive">*</span>
            </Label>
            <Input
              id="task-name"
              value={name}
              onChange={(e) => setName(e.target.value)}
              placeholder="例:小红书 · 母婴关键词监控"
            />
          </div>
          {/* 行业 + 平台 并排,省竖向空间 */}
          <div className="grid grid-cols-2 gap-3">
            <div className="space-y-1.5">
              <Label htmlFor="task-industry">
              所属行业 <span className="text-destructive">*</span>
            </Label>
            <Select value={industry} onValueChange={setIndustry}>
              <SelectTrigger id="task-industry" className="w-full">
                <SelectValue placeholder="请选择所属行业" />
              </SelectTrigger>
              <SelectContent>
                {industries.map((ind) => (
                  <SelectItem key={ind.id} value={ind.name}>
                    {ind.name}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          </div>
          <div className="space-y-1.5">
            <Label htmlFor="task-platform">
              所属平台 <span className="text-destructive">*</span>
            </Label>
            <Select value={platform} onValueChange={setPlatform}>
              <SelectTrigger id="task-platform" className="w-full">
                <SelectValue placeholder="请选择所属平台" />
              </SelectTrigger>
              <SelectContent>
                {platforms.length === 0 ? (
                  <div className="px-2 py-1.5 text-xs text-muted-foreground">
                    暂无启用平台
                  </div>
                ) : (
                  platforms.map((p) => (
                    <SelectItem
                      key={p.id}
                      value={p.id}
                      disabled={p.active === 0}
                      className="[&>span:last-child]:w-full"
                    >
                      <span className="flex w-full items-center gap-1.5">
                        <span>{p.name}</span>
                        <span className="ml-auto text-xs text-muted-foreground">
                          有效账号
                        </span>
                        <span className="rounded bg-muted px-1.5 py-0.5 font-mono text-[11px] text-muted-foreground">
                          {p.active}/{p.total}
                        </span>
                      </span>
                    </SelectItem>
                  ))
                )}
              </SelectContent>
            </Select>
          </div>
          </div>
          {/* 采集筛选:紧跟平台下方,不加标题/分割线/外框。该平台无任何筛选项(如快手)时整块隐藏 */}
          {hasFilters && (
              <div className="space-y-3">
                {/* 排序 / 发布时间 + 平台专属维度同排平铺,flex-wrap 窄屏才换行;
                    每项 flex-1 等分、min-w 保证 Select 不被压扁 */}
                <div className="flex flex-wrap gap-3">
                  {platformSort.length > 0 && (
                    <div className="min-w-[140px] flex-1 space-y-1.5">
                      <Label
                        htmlFor="task-sort"
                        className="flex items-center gap-1.5 text-sky-600 dark:text-sky-400"
                      >
                        <ArrowDownUp className="size-3.5" />
                        排序方式
                      </Label>
                      <Select
                        value={sortMode}
                        onValueChange={(v) => setSortMode(v as SortMode)}
                      >
                        <SelectTrigger id="task-sort" className="w-full">
                          <SelectValue />
                        </SelectTrigger>
                        <SelectContent>
                          {platformSort.map((o) => (
                            <SelectItem key={o.value} value={o.value}>
                              {o.label}
                            </SelectItem>
                          ))}
                        </SelectContent>
                      </Select>
                    </div>
                  )}
                  {platformTime.length > 0 && (
                    <div className="min-w-[140px] flex-1 space-y-1.5">
                      <Label
                        htmlFor="task-time"
                        className="flex items-center gap-1.5 text-amber-600 dark:text-amber-400"
                      >
                        <CalendarClock className="size-3.5" />
                        发布时间
                      </Label>
                      <Select
                        value={timeRange}
                        onValueChange={(v) => setTimeRange(v as TimeRange)}
                      >
                        <SelectTrigger id="task-time" className="w-full">
                          <SelectValue />
                        </SelectTrigger>
                        <SelectContent>
                          {platformTime.map((o) => (
                            <SelectItem key={o.value} value={o.value}>
                              {o.label}
                            </SelectItem>
                          ))}
                        </SelectContent>
                      </Select>
                    </div>
                  )}
                  {platformDims.map((dim) => (
                    <div key={dim.id} className="min-w-[140px] flex-1 space-y-1.5">
                      <Label
                        htmlFor={`task-ef-${dim.id}`}
                        className={`flex items-center gap-1.5 ${dim.color}`}
                      >
                        <dim.icon className="size-3.5" />
                        {dim.label}
                      </Label>
                      <Select
                        value={extraFilters[dim.id] ?? "any"}
                        onValueChange={(v) =>
                          setExtraFilters((prev) => ({ ...prev, [dim.id]: v }))
                        }
                      >
                        <SelectTrigger id={`task-ef-${dim.id}`} className="w-full">
                          <SelectValue />
                        </SelectTrigger>
                        <SelectContent>
                          {dim.options.map((o) => (
                            <SelectItem key={o.value} value={o.value}>
                              {o.label}
                            </SelectItem>
                          ))}
                        </SelectContent>
                      </Select>
                    </div>
                  ))}
                </div>
              </div>
          )}
          <div className="space-y-1.5">
            <div className="flex items-center justify-between">
              <Label htmlFor="task-keywords">
                关键词 <span className="text-destructive">*</span>
              </Label>
              <span className="text-xs text-muted-foreground">
                多个关键词每行一个
              </span>
            </div>
            <Textarea
              id="task-keywords"
              rows={6}
              value={keywordsRaw}
              onChange={(e) => setKeywordsRaw(e.target.value)}
              placeholder="请输入搜索关键词，每行一个"
              // 固定 6 排高度,超出走滚动条:field-sizing-fixed 覆盖基类的 field-sizing-content(否则随内容自增高)
              className="h-36 resize-none overflow-y-auto field-sizing-fixed font-mono text-sm"
            />
          </div>
          <div className="grid grid-cols-2 gap-3">
            <div className="space-y-1.5">
              <Label htmlFor="task-limit">每组返回数量</Label>
              <Input
                id="task-limit"
                type="number"
                min={1}
                value={perKeywordLimit}
                onChange={(e) => setPerKeywordLimit(e.target.value)}
              />
              <p className="text-xs text-muted-foreground">
                每个关键词最多返回的条数
              </p>
            </div>
            <div className="space-y-1.5">
              <Label htmlFor="task-min-likes">最低点赞数</Label>
              <Input
                id="task-min-likes"
                type="number"
                min={0}
                value={minLikes}
                onChange={(e) => setMinLikes(e.target.value)}
              />
              <p className="text-xs text-muted-foreground">
                低于该值的内容直接抛弃
              </p>
            </div>
          </div>
          <div className="flex items-center justify-between rounded-md border px-3 py-2.5">
            <div className="space-y-0.5">
              <div className="flex flex-wrap items-center gap-1">
                <Label htmlFor="task-ai-extract" className="cursor-pointer">
                  AI 文案提取
                </Label>
                {/* ffmpeg 依赖与检测状态:用 [] 紧跟标题后,整段统一灰色(不再用绿色区分) */}
                <span className="text-xs">
                  {ffmpegAvailable === false ? (
                    // 未安装:整段红色,下载链接同色加下划线
                    <span className="text-red-600 dark:text-red-400">
                      [依赖 ffmpeg · 未安装,
                      <button
                        type="button"
                        className="cursor-pointer underline underline-offset-2 hover:text-red-700 dark:hover:text-red-300"
                        onClick={() =>
                          openUrl("https://ffmpeg.org/download.html").catch((e) =>
                            toast.error(`打开下载页失败: ${e}`),
                          )
                        }
                      >
                        点此下载
                      </button>
                      ]
                    </span>
                  ) : ffmpegAvailable === true ? (
                    // 已安装:绿色表示就绪
                    <span className="text-emerald-600 dark:text-emerald-400">
                      [依赖 ffmpeg · 已安装]
                    </span>
                  ) : (
                    <span className="text-muted-foreground">[依赖 ffmpeg]</span>
                  )}
                </span>
              </div>
              <p className="text-xs text-muted-foreground">
                对采集到的视频自动提取文案(转音频后做语音转写)
              </p>
            </div>
            <Switch
              id="task-ai-extract"
              checked={aiExtract}
              onCheckedChange={setAiExtract}
              className="scale-125"
            />
          </div>
          <div className="rounded-md border">
            <div className="flex items-center justify-between px-3 py-2.5">
              <div className="space-y-0.5">
                <Label
                  htmlFor="task-collect-comments"
                  className="cursor-pointer"
                >
                  开启评论采集
                </Label>
                <p className="text-xs text-muted-foreground">
                  抓取内容下的评论,用于后续意向客户分析
                </p>
              </div>
              <Switch
                id="task-collect-comments"
                className="scale-125"
                checked={collectComments}
                onCheckedChange={(v) => {
                  setCollectComments(v);
                  // 意图分析依赖评论采集,关闭评论采集时同步关闭意图分析
                  if (!v) setAnalyzeCommentIntent(false);
                }}
              />
            </div>

            {/* 参数常驻(不折叠);未开启评论采集时整体禁用 + 置灰。三项均为下拉,一行平铺 */}
            <div
              className={`grid grid-cols-3 gap-3 border-t px-3 py-3 ${
                collectComments ? "" : "pointer-events-none opacity-50"
              }`}
            >
              <div className="space-y-1.5">
                <Label htmlFor="task-comment-time">评论时间</Label>
                <Select
                  value={commentTimeRange}
                  onValueChange={(v) => setCommentTimeRange(v as CommentTimeRange)}
                  disabled={!collectComments}
                >
                  <SelectTrigger id="task-comment-time" className="w-full">
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    {(Object.keys(COMMENT_TIME_RANGE_META) as CommentTimeRange[]).map(
                      (k) => (
                        <SelectItem key={k} value={k}>
                          {COMMENT_TIME_RANGE_META[k].label}
                        </SelectItem>
                      ),
                    )}
                  </SelectContent>
                </Select>
              </div>
              <div className="space-y-1.5">
                <Label htmlFor="task-comment-limit">单视频上限</Label>
                <Select
                  value={commentLimit}
                  onValueChange={setCommentLimit}
                  disabled={!collectComments}
                >
                  <SelectTrigger id="task-comment-limit" className="w-full">
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    {COMMENT_LIMIT_OPTIONS.map((o) => (
                      <SelectItem key={o.value} value={o.value}>
                        {o.label}
                      </SelectItem>
                    ))}
                  </SelectContent>
                </Select>
              </div>
              {/* 评论意图分析:是 / 否 下拉(布尔映射 1/0),依赖评论采集开启 */}
              <div className="space-y-1.5">
                <Label htmlFor="task-comment-intent">评论意图分析</Label>
                <Select
                  value={analyzeCommentIntent ? "1" : "0"}
                  onValueChange={(v) => setAnalyzeCommentIntent(v === "1")}
                  disabled={!collectComments}
                >
                  <SelectTrigger id="task-comment-intent" className="w-full">
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    <SelectItem value="0">否</SelectItem>
                    <SelectItem value="1">是</SelectItem>
                  </SelectContent>
                </Select>
              </div>
            </div>
          </div>

          {/* Obsidian 同步:置于触发方式上方 */}
          <Separator />
          <div className="flex items-center justify-between rounded-md border px-3 py-2.5">
            <div className="space-y-0.5">
              <Label htmlFor="task-auto-sync" className="cursor-pointer">
                采集完成后自动同步到 Obsidian
              </Label>
              <p className="text-xs text-muted-foreground">
                {obsidianConfigured
                  ? "采完自动把内容写入你的 Obsidian vault;未开启也可在内容库手动同步"
                  : "未配置 Obsidian,请先到「系统设置 → Obsidian」配置 vault 路径后再开启"}
              </p>
            </div>
            <Switch
              id="task-auto-sync"
              className="scale-125"
              checked={obsidianConfigured && autoSyncObsidian}
              onCheckedChange={setAutoSyncObsidian}
              disabled={!obsidianConfigured}
            />
          </div>
          <SectionLabel icon={Clock}>触发方式</SectionLabel>
          <div className="space-y-1.5">
            <div className="grid grid-cols-3 gap-2">
              {(Object.keys(TRIGGER_META) as TaskTrigger[]).map((k) => {
                const m = TRIGGER_META[k];
                const Icon = m.icon;
                const active = trigger === k;
                return (
                  <button
                    type="button"
                    key={k}
                    onClick={() => setTrigger(k)}
                    className={`cursor-pointer rounded-md border px-2 py-2 text-xs transition-colors ${
                      active
                        ? "border-primary bg-primary/10 text-primary"
                        : "hover:bg-accent"
                    }`}
                  >
                    <Icon className="mx-auto mb-1 size-4" />
                    {m.label}
                  </button>
                );
              })}
            </div>
          </div>
          {trigger === "daily" && (
            <div className="space-y-1.5">
              <Label>每日执行时间</Label>
              {(() => {
                const [hh = "", mm = ""] = scheduledAt.split(":");
                const pad = (n: number) =>
                  String(Math.max(0, Math.min(59, n))).padStart(2, "0");
                return (
                  <div className="flex items-center gap-2">
                    <Input
                      type="number"
                      min={0}
                      max={23}
                      placeholder="时"
                      value={hh}
                      onChange={(e) => {
                        const n = Number(e.target.value);
                        const next = Number.isNaN(n)
                          ? ""
                          : String(Math.max(0, Math.min(23, n))).padStart(
                              2,
                              "0",
                            );
                        setScheduledAt(`${next}:${mm || "00"}`);
                      }}
                      className="w-20 text-center"
                    />
                    <span className="text-muted-foreground">:</span>
                    <Input
                      type="number"
                      min={0}
                      max={59}
                      placeholder="分"
                      value={mm}
                      onChange={(e) => {
                        const n = Number(e.target.value);
                        const next = Number.isNaN(n) ? "" : pad(n);
                        setScheduledAt(`${hh || "00"}:${next}`);
                      }}
                      className="w-20 text-center"
                    />
                  </div>
                );
              })()}
              <p className="text-xs text-muted-foreground">
                每天到达该时间点时自动启动一次(24 小时制)
              </p>
            </div>
          )}

          {trigger === "watching" && (
            <>
              <div className="space-y-1.5">
                <Label htmlFor="task-watch-interval">轮询间隔(分钟)</Label>
                <Input
                  id="task-watch-interval"
                  type="number"
                  min={30}
                  value={watchIntervalMin}
                  onChange={(e) => setWatchIntervalMin(e.target.value)}
                  onBlur={() => {
                    // 失焦纠偏:低于 30 自动抬到 30(最小轮询间隔)
                    const n = Number(watchIntervalMin);
                    if (!Number.isFinite(n) || n < 30) {
                      setWatchIntervalMin("30");
                    }
                  }}
                  className="w-40"
                />
                <p className="text-xs text-muted-foreground">
                  每过 {watchIntervalMin || "?"} 分钟检查一次是否有新内容(最少 30
                  分钟)
                </p>
              </div>
              <div className="rounded-md border border-sky-500/30 bg-sky-500/10 px-3 py-2 text-xs text-sky-700 dark:text-sky-400">
                <CircleSlash2 className="-mt-0.5 mr-1 inline size-3" />
                持续监听任务运行后不会主动结束,需要手动停止。
              </div>
            </>
          )}
        </form>

        {/* 取消 + 创建:并排右对齐;取消是用户主动关闭,直接关(不触发"有内容"拦截) */}
        <SheetFooter className="flex-row justify-end gap-2">
          <Button
            type="button"
            variant="outline"
            className="h-10 cursor-pointer"
            onClick={() => onOpenChange(false)}
          >
            取消
          </Button>
          {/* 高度对齐筛选下拉(h-10);Button 默认 h-8,这里显式拉高 */}
          <Button type="submit" form="task-form" className="h-10 cursor-pointer">
            {initial ? "保存修改" : "创建任务"}
          </Button>
        </SheetFooter>
      </SheetContent>
    </Sheet>
  );
}

// ---- 空状态提示 + 单行操作按钮组(含归档/删除确认) ----

