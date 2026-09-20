import {
  useEffect,
  useMemo,
  useState,
  type FormEvent,
  type KeyboardEvent,
} from "react";
import { type ColumnDef, type FilterFn } from "@tanstack/react-table";
import { format } from "date-fns";
import { MoreVertical, SquarePen, Plus, Search, Trash2, X, Eye } from "lucide-react";
import { api, type PromptDto } from "@/lib/api";
import { MarkdownMessage } from "@/components/MarkdownMessage";
import { FORM_CONTROL_SIZING } from "@/lib/form-sizing";
import { ErrorBanner } from "@/components/ErrorBanner";
import { DataTable } from "@/components/DataTable";
import { EmptyState } from "@/components/EmptyState";
import { DataTableColumnHeader } from "@/components/DataTableColumnHeader";
import { DataTableFacetedFilter } from "@/components/DataTableFacetedFilter";
import { StatusBadge, type StatusTone } from "@/components/StatusBadge";
import { FieldError } from "@/components/FieldError";
import { CodeField, generateCode } from "@/components/CodeField";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Textarea } from "@/components/ui/textarea";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import {
  Sheet,
  SheetContent,
  SheetDescription,
  SheetFooter,
  SheetHeader,
  SheetTitle,
} from "@/components/ui/sheet";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
} from "@/components/ui/alert-dialog";

// 提示词管理(创作):接真实后端 prompts 表 list/upsert/remove 命令。
// 适配模型为 tag 多值(Enter / 逗号添加),类型与来源用于筛选与归档。

type PromptItem = PromptDto;

type PromptKind = "image" | "video";

// 类型:文案 + 色调;未知值(旧数据空串 / adapt 等)按 neutral 原样展示
const PROMPT_KIND_META: Record<PromptKind, { label: string; tone: StatusTone }> = {
  image: { label: "图片", tone: "info" },
  video: { label: "视频", tone: "success" },
};

function kindMeta(kind: string): { label: string; tone: StatusTone } {
  return (
    PROMPT_KIND_META[kind as PromptKind] ?? {
      label: kind || "—",
      tone: "neutral",
    }
  );
}

// 常用适配模型预置:按提示词类型给出,点击即添加 / 移除;仍支持手动输入自定义模型
const IMAGE_MODEL_PRESETS = ["Nano Banana Pro", "Nano Banana 2", "ChatGPT", "Grok"];
const VIDEO_MODEL_PRESETS = ["Seedance 2.0"];

// 创建 / 更新时间统一「年-月-日 时:分:秒」
function fmtDateTime(ts: number): string {
  if (!ts) return "—";
  return format(new Date(ts * 1000), "yyyy-MM-dd HH:mm:ss");
}

// 全局搜索:匹配名称 / 编码 / 内容 / 来源 / 适配模型
const promptFilterFn: FilterFn<PromptItem> = (row, _columnId, value) => {
  const p = row.original;
  return `${p.name} ${p.code} ${p.content} ${p.source} ${p.models.join(" ")}`
    .toLowerCase()
    .includes(String(value).toLowerCase());
};

export function PromptsPage() {
  const [prompts, setPrompts] = useState<PromptItem[]>([]);
  const [editing, setEditing] = useState<PromptItem | null>(null);
  const [isFormOpen, setIsFormOpen] = useState(false);
  const [deleteTarget, setDeleteTarget] = useState<PromptItem | null>(null);
  // Markdown 预览弹窗目标(点内容单元格 / 菜单「预览」打开)
  const [previewTarget, setPreviewTarget] = useState<PromptItem | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);

  async function loadPrompts() {
    setLoading(true);
    try {
      setPrompts(await api.listPrompts());
    } catch (e) {
      setError(String(e));
    } finally {
      setLoading(false);
    }
  }

  useEffect(() => {
    loadPrompts();
  }, []);

  async function submit(input: PromptDto) {
    try {
      await api.upsertPrompt(input);
      setIsFormOpen(false);
      await loadPrompts();
    } catch (e) {
      setError(String(e));
    }
  }

  async function confirmDelete() {
    if (!deleteTarget) return;
    try {
      await api.removePrompt(deleteTarget.id);
      await loadPrompts();
    } catch (e) {
      setError(String(e));
    }
    setDeleteTarget(null);
  }

  const columns = useMemo<ColumnDef<PromptItem>[]>(
    () => [
      {
        accessorKey: "name",
        header: ({ column }) => (
          <DataTableColumnHeader column={column} title="提示词名称" />
        ),
        cell: ({ row }) => (
          <span className="font-medium text-foreground">
            {row.original.name}
          </span>
        ),
      },
      {
        accessorKey: "kind",
        header: "类别",
        filterFn: (row, id, value) =>
          (value as string[]).includes(row.getValue(id)),
        cell: ({ row }) => {
          const meta = kindMeta(row.original.kind);
          return <StatusBadge tone={meta.tone}>{meta.label}</StatusBadge>;
        },
      },
      {
        accessorKey: "code",
        header: "提示词编码",
        cell: ({ row }) => (
          <span className="font-mono text-xs">{row.original.code}</span>
        ),
      },
      {
        accessorKey: "content",
        header: "提示词内容",
        enableSorting: false,
        cell: ({ row }) => (
          // 内容为 Markdown:点击打开渲染预览
          <button
            type="button"
            title="点击预览 Markdown 渲染效果"
            className="line-clamp-2 max-w-[280px] cursor-pointer text-left text-muted-foreground hover:text-foreground"
            onClick={() => setPreviewTarget(row.original)}
          >
            {row.original.content}
          </button>
        ),
      },
      {
        accessorKey: "models",
        header: "适配模型",
        enableSorting: false,
        cell: ({ row }) => {
          const models = row.original.models;
          if (models.length === 0) {
            return <span className="text-muted-foreground">—</span>;
          }
          return (
            <div className="flex max-w-[220px] flex-wrap gap-1">
              {models.map((m) => (
                <span key={m} className="rounded bg-muted px-1.5 py-0.5 text-xs">
                  {m}
                </span>
              ))}
            </div>
          );
        },
      },
      {
        accessorKey: "source",
        header: "来源",
        cell: ({ row }) => {
          const p = row.original;
          if (!p.source && !p.sourceDataId) return "—";
          return (
            <div className="flex flex-col">
              {p.source && <span>{p.source}</span>}
              {p.sourceDataId && (
                <span
                  className="max-w-[180px] truncate font-mono text-xs text-muted-foreground"
                  title={p.sourceDataId}
                >
                  {p.sourceDataId}
                </span>
              )}
            </div>
          );
        },
      },
      {
        accessorKey: "createdAt",
        header: ({ column }) => (
          <DataTableColumnHeader column={column} title="创建时间" />
        ),
        cell: ({ row }) => (
          <span className="text-muted-foreground">
            {fmtDateTime(row.original.createdAt)}
          </span>
        ),
      },
      {
        accessorKey: "updatedAt",
        header: ({ column }) => (
          <DataTableColumnHeader column={column} title="更新时间" />
        ),
        cell: ({ row }) => (
          <span className="text-muted-foreground">
            {fmtDateTime(row.original.updatedAt)}
          </span>
        ),
      },
      {
        id: "actions",
        header: () => <div className="text-right">操作</div>,
        enableSorting: false,
        cell: ({ row }) => {
          const p = row.original;
          return (
            <div className="flex justify-end">
              <DropdownMenu>
                <DropdownMenuTrigger asChild>
                  <Button
                    variant="ghost"
                    size="icon-sm"
                    className="text-muted-foreground"
                  >
                    <MoreVertical />
                    <span className="sr-only">操作</span>
                  </Button>
                </DropdownMenuTrigger>
                <DropdownMenuContent align="end" className="w-32">
                  <DropdownMenuItem onClick={() => setPreviewTarget(p)}>
                    <Eye />
                    预览
                  </DropdownMenuItem>
                  <DropdownMenuItem
                    onClick={() => {
                      setEditing(p);
                      setIsFormOpen(true);
                    }}
                  >
                    <SquarePen />
                    编辑
                  </DropdownMenuItem>
                  <DropdownMenuSeparator />
                  <DropdownMenuItem
                    className="text-destructive focus:text-destructive"
                    onClick={() => setDeleteTarget(p)}
                  >
                    <Trash2 />
                    删除
                  </DropdownMenuItem>
                </DropdownMenuContent>
              </DropdownMenu>
            </div>
          );
        },
      },
    ],
    [],
  );

  const kindOptions = (
    Object.entries(PROMPT_KIND_META) as [PromptKind, { label: string }][]
  ).map(([value, meta]) => ({ label: meta.label, value }));

  // 来源下拉选项:预置 opennana,并并入库中已出现的来源(旧值仍可选)
  const sourceOptions = Array.from(
    new Set(["opennana", ...prompts.map((p) => p.source).filter(Boolean)]),
  );

  return (
    <div className={`flex min-h-0 flex-1 flex-col gap-2.5 ${FORM_CONTROL_SIZING}`}>
      <ErrorBanner message={error} onClose={() => setError(null)} />
      <DataTable
        columns={columns}
        data={prompts}
        loading={loading}
        itemLabel="提示词"
        globalFilterFn={promptFilterFn}
        getRowId={(row) => row.id}
        renderToolbar={(table) => (
          <div className="flex flex-col gap-3 sm:flex-row sm:items-center sm:justify-between">
            <div className="flex flex-1 flex-wrap items-center gap-2">
              <DataTableFacetedFilter
                column={table.getColumn("kind")}
                title="类别"
                options={kindOptions}
              />
              <div className="relative w-full sm:max-w-sm">
                <Search className="pointer-events-none absolute left-3 top-1/2 size-4 -translate-y-1/2 text-muted-foreground" />
                <Input
                  placeholder="搜索名称 / 编码 / 内容 / 来源 / 模型"
                  className="pl-9"
                  value={(table.getState().globalFilter as string) ?? ""}
                  onChange={(e) => table.setGlobalFilter(e.target.value)}
                />
              </div>
            </div>
            <Button
              className="h-10"
              onClick={() => {
                setEditing(null);
                setIsFormOpen(true);
              }}
            >
              <Plus />
              新增提示词
            </Button>
          </div>
        )}
        emptyState={
          <EmptyState
            title="暂无提示词"
            description="点击右上角「新增提示词」开始建立提示词库"
          />
        }
      />

      <Sheet open={isFormOpen} onOpenChange={setIsFormOpen}>
        <PromptFormSheet
          key={isFormOpen ? (editing?.id ?? "new") : "idle"}
          initial={editing}
          sourceOptions={sourceOptions}
          onSubmit={submit}
          onCancel={() => setIsFormOpen(false)}
        />
      </Sheet>

      <AlertDialog
        open={deleteTarget !== null}
        onOpenChange={(open) => !open && setDeleteTarget(null)}
      >
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>删除提示词</AlertDialogTitle>
            <AlertDialogDescription>
              将永久删除提示词「{deleteTarget?.name}」,此操作不可恢复。
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel className="cursor-pointer">
              取消
            </AlertDialogCancel>
            <AlertDialogAction
              className="cursor-pointer bg-destructive text-white hover:bg-destructive/90"
              onClick={confirmDelete}
            >
              确认删除
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>

      {/* Markdown 预览:提示词内容按 Markdown 渲染 */}
      <Dialog
        open={previewTarget !== null}
        onOpenChange={(open) => !open && setPreviewTarget(null)}
      >
        <DialogContent className="flex max-h-[80vh] flex-col sm:max-w-2xl">
          <DialogHeader>
            <DialogTitle>{previewTarget?.name}</DialogTitle>
            <DialogDescription>
              {previewTarget?.code} · Markdown 渲染预览
            </DialogDescription>
          </DialogHeader>
          <div className="min-h-0 flex-1 overflow-y-auto rounded-md border p-4">
            {previewTarget?.example && (
              <div className="mb-4 space-y-1.5">
                <p className="text-xs text-muted-foreground">示例</p>
                <ExamplePreview
                  url={previewTarget.example}
                  kind={previewTarget.kind}
                />
              </div>
            )}
            {previewTarget && <MarkdownMessage content={previewTarget.content} />}
          </div>
        </DialogContent>
      </Dialog>
    </div>
  );
}

// 示例链接预览:按扩展名判定图片 / 视频,扩展名缺失时按提示词类型兜底
function ExamplePreview({ url, kind }: { url: string; kind: string }) {
  const videoExt = /\.(mp4|webm|mov|m4v)(\?|#|$)/i.test(url);
  const imageExt = /\.(png|jpe?g|gif|webp|bmp|svg|avif)(\?|#|$)/i.test(url);
  const isVideo = videoExt || (!imageExt && kind === "video");
  return isVideo ? (
    <video
      src={url}
      controls
      className="max-h-48 max-w-full rounded-md border bg-black"
    />
  ) : (
    <img
      src={url}
      alt="示例"
      className="max-h-48 max-w-full rounded-md border object-contain"
    />
  );
}

// 新增 / 编辑提示词(置于 Sheet 内)
function PromptFormSheet({
  initial,
  sourceOptions,
  onSubmit,
  onCancel,
}: {
  initial: PromptItem | null;
  sourceOptions: string[];
  onSubmit: (prompt: PromptDto) => void;
  onCancel: () => void;
}) {
  const isEdit = initial !== null;
  const [code, setCode] = useState(initial?.code ?? generateCode("PRM"));
  const [name, setName] = useState(initial?.name ?? "");
  const [kind, setKind] = useState<string>(initial?.kind ?? "image");
  const [content, setContent] = useState(initial?.content ?? "");
  const [source, setSource] = useState(initial?.source ?? "");
  const [sourceDataId, setSourceDataId] = useState(initial?.sourceDataId ?? "");
  const [example, setExample] = useState(initial?.example ?? "");
  const [models, setModels] = useState<string[]>(initial?.models ?? []);
  const [modelInput, setModelInput] = useState("");
  // 内容为 Markdown:编辑 / 预览切换
  const [contentMode, setContentMode] = useState<"edit" | "preview">("edit");
  const [submitted, setSubmitted] = useState(false);

  function addModel() {
    const value = modelInput.trim();
    if (value && !models.includes(value)) setModels([...models, value]);
    setModelInput("");
  }

  function handleModelKeyDown(event: KeyboardEvent<HTMLInputElement>) {
    if (event.key === "Enter" || event.key === ",") {
      event.preventDefault();
      addModel();
    }
  }

  function handleSubmit(event: FormEvent) {
    event.preventDefault();
    setSubmitted(true);
    if (!name.trim() || !content.trim() || !kind) {
      return;
    }
    onSubmit({
      id: initial?.id ?? crypto.randomUUID(),
      code,
      name: name.trim(),
      kind,
      content: content.trim(),
      source: source.trim(),
      sourceDataId: sourceDataId.trim(),
      example: example.trim(),
      models,
      createdAt: initial?.createdAt ?? 0,
      updatedAt: initial?.updatedAt ?? 0,
    });
  }

  return (
    <SheetContent
      className="flex w-full flex-col gap-0 p-0 sm:max-w-[1080px]"
      blockClose={
        name !== (initial?.name ?? "") ||
        kind !== (initial?.kind ?? "image") ||
        content !== (initial?.content ?? "") ||
        source !== (initial?.source ?? "") ||
        sourceDataId !== (initial?.sourceDataId ?? "") ||
        example !== (initial?.example ?? "") ||
        models.join(",") !== (initial?.models ?? []).join(",")
      }
    >
      <SheetHeader className="border-b">
        <SheetTitle>{isEdit ? "编辑提示词" : "新增提示词"}</SheetTitle>
        <SheetDescription>
          维护提示词档案,适配模型支持多个,Enter 或逗号添加为标签。
        </SheetDescription>
      </SheetHeader>
      <form onSubmit={handleSubmit} className="flex min-h-0 flex-1 flex-col">
        <div className="flex min-h-0 flex-1 flex-col gap-4 overflow-y-auto p-5">
          <div className="space-y-3">
            <div className="space-y-1.5">
              <Label htmlFor="prompt-name">
                提示词名称 <span className="text-destructive">*</span>
              </Label>
              <Input
                id="prompt-name"
                placeholder="提示词名称"
                value={name}
                onChange={(e) => setName(e.target.value)}
                aria-invalid={submitted && !name.trim()}
                autoFocus
              />
              <FieldError
                show={submitted && !name.trim()}
                message="提示词名称不可为空"
              />
            </div>
            <div className="grid gap-4 sm:grid-cols-2">
              <div className="space-y-1.5">
                <Label>
                  提示词类别 <span className="text-destructive">*</span>
                </Label>
                <Select value={kind} onValueChange={setKind}>
                  <SelectTrigger className="w-full">
                    <SelectValue placeholder="选择类别" />
                  </SelectTrigger>
                  <SelectContent>
                    <SelectItem value="image">图片</SelectItem>
                    <SelectItem value="video">视频</SelectItem>
                  </SelectContent>
                </Select>
              </div>
              <div className="space-y-1.5">
                <Label htmlFor="prompt-code">提示词编码</Label>
                <CodeField
                  id="prompt-code"
                  value={code}
                  onRegenerate={() => setCode(generateCode("PRM"))}
                />
                <p className="text-xs text-muted-foreground">
                  系统自动生成,可刷新或复制
                </p>
              </div>
            </div>
            <div className="space-y-1.5">
              <Label htmlFor="prompt-models">适配模型</Label>
              <div className="rounded-md border px-2 py-1.5">
                {models.length > 0 && (
                  <div className="flex flex-wrap gap-1 pb-1.5">
                    {models.map((m) => (
                      <span
                        key={m}
                        className="inline-flex items-center gap-1 rounded bg-muted px-1.5 py-0.5 text-xs"
                      >
                        {m}
                        <button
                          type="button"
                          className="cursor-pointer text-muted-foreground hover:text-foreground"
                          onClick={() =>
                            setModels(models.filter((x) => x !== m))
                          }
                        >
                          <X className="size-3" />
                        </button>
                      </span>
                    ))}
                  </div>
                )}
                <input
                  id="prompt-models"
                  className="w-full bg-transparent px-1 py-1 text-sm outline-none placeholder:text-muted-foreground"
                  placeholder="输入模型名,Enter 或逗号添加(如 deepseek-v4-pro)"
                  value={modelInput}
                  onChange={(e) => setModelInput(e.target.value)}
                  onKeyDown={handleModelKeyDown}
                  onBlur={addModel}
                />
              </div>
              {/* 常用模型快捷选择:随提示词类型切换 */}
              <div className="flex flex-wrap gap-1.5 pt-1.5">
                {(kind === "video" ? VIDEO_MODEL_PRESETS : IMAGE_MODEL_PRESETS).map(
                  (m) => {
                    const active = models.includes(m);
                    return (
                      <button
                        key={m}
                        type="button"
                        onClick={() =>
                          setModels(
                            active
                              ? models.filter((x) => x !== m)
                              : [...models, m],
                          )
                        }
                        className={`cursor-pointer rounded-full border px-2.5 py-0.5 text-xs transition-colors ${
                          active
                            ? "border-primary bg-primary/10 text-primary"
                            : "text-muted-foreground hover:border-foreground/40 hover:text-foreground"
                        }`}
                      >
                        {m}
                      </button>
                    );
                  },
                )}
              </div>
              <p className="text-xs text-muted-foreground">
                支持多个,以标签形式管理;点击常用模型可快速添加 / 移除
              </p>
            </div>
            <div className="grid gap-4 sm:grid-cols-2">
              <div className="space-y-1.5">
                <Label>提示词来源</Label>
                <Select value={source} onValueChange={setSource}>
                  <SelectTrigger className="w-full">
                    <SelectValue placeholder="选择来源" />
                  </SelectTrigger>
                  <SelectContent>
                    {sourceOptions.map((s) => (
                      <SelectItem key={s} value={s}>
                        {s}
                      </SelectItem>
                    ))}
                  </SelectContent>
                </Select>
              </div>
              <div className="space-y-1.5">
                <Label htmlFor="prompt-source-data-id">来源数据 id</Label>
                <Input
                  id="prompt-source-data-id"
                  placeholder="取材的内容 / 数据记录 id(可选)"
                  value={sourceDataId}
                  onChange={(e) => setSourceDataId(e.target.value)}
                  className="font-mono"
                />
              </div>
            </div>
            <div className="space-y-1.5">
              <Label htmlFor="prompt-example">示例</Label>
              <Input
                id="prompt-example"
                placeholder="图片或视频链接(可选)"
                value={example}
                onChange={(e) => setExample(e.target.value)}
              />
              {example.trim() && (
                <ExamplePreview url={example.trim()} kind={kind} />
              )}
            </div>
          </div>
          {/* 提示词内容放最后:高度自适应占满剩余空间(Markdown 源文,支持编辑 / 预览切换) */}
          <div className="flex min-h-[240px] flex-1 flex-col space-y-1.5">
            <div className="flex items-center justify-between">
              <Label htmlFor="prompt-content">
                提示词内容 <span className="text-destructive">*</span>
              </Label>
              <div className="flex rounded-md border p-0.5">
                {(["edit", "preview"] as const).map((mode) => (
                  <button
                    key={mode}
                    type="button"
                    onClick={() => setContentMode(mode)}
                    className={`cursor-pointer rounded px-2.5 py-0.5 text-xs transition-colors ${
                      contentMode === mode
                        ? "bg-muted font-medium text-foreground"
                        : "text-muted-foreground hover:text-foreground"
                    }`}
                  >
                    {mode === "edit" ? "编辑" : "预览"}
                  </button>
                ))}
              </div>
            </div>
            {contentMode === "edit" ? (
              <Textarea
                id="prompt-content"
                placeholder="提示词内容(Markdown 格式)"
                className="min-h-0 flex-1 resize-none"
                value={content}
                onChange={(e) => setContent(e.target.value)}
                aria-invalid={submitted && !content.trim()}
              />
            ) : (
              <div className="min-h-0 flex-1 overflow-y-auto rounded-md border p-3">
                {content.trim() ? (
                  <MarkdownMessage content={content} />
                ) : (
                  <p className="text-sm text-muted-foreground">
                    暂无内容,切换到「编辑」输入 Markdown
                  </p>
                )}
              </div>
            )}
            <FieldError
              show={submitted && !content.trim()}
              message="提示词内容不可为空"
            />
          </div>
        </div>
        <SheetFooter className="flex-row justify-end gap-2 border-t">
          <Button type="button" variant="outline" onClick={onCancel}>
            取消
          </Button>
          <Button type="submit">保存</Button>
        </SheetFooter>
      </form>
    </SheetContent>
  );
}
