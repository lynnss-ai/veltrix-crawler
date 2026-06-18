// 任务新建/编辑表单抽屉:从 CollectPage 抽出的独立 Sheet 组件(props 自洽,无闭包捕获)。
import { useEffect, useState } from "react";
import type { FormEvent } from "react";
import { api } from "@/lib/api";
import type { IndustryView, TaskInput } from "@/lib/api";
import { filterMetaFor, COMMENT_TIME_RANGE_META, COMMENT_LIMIT_OPTIONS, TRIGGER_META, DEFAULT_STRATEGY } from "./collect-meta";
import type { TaskTrigger, SortMode, TimeRange, TaskItem, CommentTimeRange, PlatformOption } from "./collect-meta";
import { openUrl } from "@tauri-apps/plugin-opener";
import { CircleSlash2 } from "lucide-react";
import { toast } from "sonner";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Switch } from "@/components/ui/switch";
import { Textarea } from "@/components/ui/textarea";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { Sheet, SheetContent, SheetDescription, SheetFooter, SheetHeader, SheetTitle } from "@/components/ui/sheet";

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
      .catch(() => {});
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
  // 切换平台时:若当前排序/时间不在该平台支持项内,回退到该平台首项(综合/不限)
  useEffect(() => {
    const meta = filterMetaFor(platform);
    if (!meta.sort.some((o) => o.value === sortMode)) {
      setSortMode(meta.sort[0].value);
    }
    if (!meta.time.some((o) => o.value === timeRange)) {
      setTimeRange(meta.time[0].value);
    }
    // 仅在平台变化时纠正,不依赖 sortMode/timeRange
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
    };
    onSubmit(input);
  }

  return (
    <Sheet open={open} onOpenChange={onOpenChange}>
      <SheetContent className="flex w-full flex-col gap-0 sm:max-w-[600px]">
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
              rows={10}
              value={keywordsRaw}
              onChange={(e) => setKeywordsRaw(e.target.value)}
              placeholder={"婴儿辅食\n宝宝湿疹\n孕妇维生素"}
              className="min-h-48 font-mono text-sm"
            />
          </div>
          {/* 采集策略 */}
          <div className="grid grid-cols-2 gap-3">
            <div className="space-y-1.5">
              <Label htmlFor="task-sort">排序方式</Label>
              <Select
                value={sortMode}
                onValueChange={(v) => setSortMode(v as SortMode)}
              >
                <SelectTrigger id="task-sort" className="w-full">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  {filterMetaFor(platform).sort.map((o) => (
                    <SelectItem key={o.value} value={o.value}>
                      {o.label}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
            </div>
            <div className="space-y-1.5">
              <Label htmlFor="task-time">发布时间</Label>
              <Select
                value={timeRange}
                onValueChange={(v) => setTimeRange(v as TimeRange)}
              >
                <SelectTrigger id="task-time" className="w-full">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  {filterMetaFor(platform).time.map((o) => (
                    <SelectItem key={o.value} value={o.value}>
                      {o.label}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
            </div>
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
              <Label htmlFor="task-ai-extract" className="cursor-pointer">
                AI 文案提取
              </Label>
              <p className="text-xs text-muted-foreground">
                对采集到的视频自动提取文案(转音频后做语音转写);依赖 ffmpeg
                {ffmpegAvailable === true && (
                  <span className="ml-1 text-emerald-600 dark:text-emerald-400">
                    · 已检测到,可正常转写
                  </span>
                )}
                {ffmpegAvailable === false && (
                  <>
                    ,未安装可
                    <button
                      type="button"
                      className="ml-1 cursor-pointer text-primary underline underline-offset-2 hover:text-primary/80"
                      onClick={() =>
                        openUrl("https://ffmpeg.org/download.html").catch((e) =>
                          toast.error(`打开下载页失败: ${e}`),
                        )
                      }
                    >
                      点此下载
                    </button>
                  </>
                )}
              </p>
            </div>
            <Switch
              id="task-ai-extract"
              checked={aiExtract}
              onCheckedChange={setAiExtract}
            />
          </div>
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
              checked={obsidianConfigured && autoSyncObsidian}
              onCheckedChange={setAutoSyncObsidian}
              disabled={!obsidianConfigured}
            />
          </div>

          {/* 评论采集:开启后展开评论抓取规则 + 意图分析 */}
          <div className="rounded-md border">
            <div className="flex items-center justify-between px-3 py-2.5">
              <div className="space-y-0.5">
                <Label
                  htmlFor="task-collect-comments"
                  className="cursor-pointer"
                >
                  评论采集
                </Label>
                <p className="text-xs text-muted-foreground">
                  抓取内容下的评论,用于后续意向客户分析
                </p>
              </div>
              <Switch
                id="task-collect-comments"
                checked={collectComments}
                onCheckedChange={(v) => {
                  setCollectComments(v);
                  // 意图分析依赖评论采集,关闭评论采集时同步关闭意图分析
                  if (!v) setAnalyzeCommentIntent(false);
                }}
              />
            </div>

            {collectComments && (
              <div className="space-y-3 border-t px-3 py-3">
                <div className="grid grid-cols-2 gap-3">
                  <div className="space-y-1.5">
                    <Label htmlFor="task-comment-time">评论时间范围</Label>
                    <Select
                      value={commentTimeRange}
                      onValueChange={(v) =>
                        setCommentTimeRange(v as CommentTimeRange)
                      }
                    >
                      <SelectTrigger id="task-comment-time" className="w-full">
                        <SelectValue />
                      </SelectTrigger>
                      <SelectContent>
                        {(
                          Object.keys(
                            COMMENT_TIME_RANGE_META,
                          ) as CommentTimeRange[]
                        ).map((k) => (
                          <SelectItem key={k} value={k}>
                            {COMMENT_TIME_RANGE_META[k].label}
                          </SelectItem>
                        ))}
                      </SelectContent>
                    </Select>
                  </div>
                  <div className="space-y-1.5">
                    <Label htmlFor="task-comment-limit">单视频上限</Label>
                    <Select value={commentLimit} onValueChange={setCommentLimit}>
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
                </div>

                <div className="flex items-center justify-between rounded-md border px-3 py-2.5">
                  <div className="space-y-0.5">
                    <Label
                      htmlFor="task-comment-intent"
                      className="cursor-pointer"
                    >
                      评论意图分析
                    </Label>
                    <p className="text-xs text-muted-foreground">
                      采集完成后用 AI 分析评论,提取有意向的客户
                    </p>
                  </div>
                  <Switch
                    id="task-comment-intent"
                    checked={analyzeCommentIntent}
                    onCheckedChange={setAnalyzeCommentIntent}
                  />
                </div>
              </div>
            )}
          </div>

          <div className="space-y-1.5">
            <Label>触发方式</Label>
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

        <SheetFooter>
          <Button type="submit" form="task-form" className="cursor-pointer">
            {initial ? "保存修改" : "创建采集任务"}
          </Button>
        </SheetFooter>
      </SheetContent>
    </Sheet>
  );
}

// ---- 空状态提示 + 单行操作按钮组(含归档/删除确认) ----

