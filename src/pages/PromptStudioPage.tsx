import {
  useCallback,
  useEffect,
  useMemo,
  useState,
  type FormEvent,
} from "react";
import { type ColumnDef, type FilterFn } from "@tanstack/react-table";
import {
  ChevronLeft,
  Filter,
  Lightbulb,
  MoreVertical,
  Plus,
  Search,
  SquarePen,
  Trash2,
} from "lucide-react";
import { useResponsiveCollapse } from "@/hooks/use-responsive-collapse";
import { SimpleTooltip } from "@/components/SimpleTooltip";
import {
  api,
  type PromptCategoryInput,
  type PromptCategoryView,
  type ShotPromptInput,
  type ShotPromptView,
} from "@/lib/api";
import { FORM_CONTROL_SIZING } from "@/lib/form-sizing";
import { ErrorBanner } from "@/components/ErrorBanner";
import { DataTable } from "@/components/DataTable";
import { EmptyState } from "@/components/EmptyState";
import { DataTableColumnHeader } from "@/components/DataTableColumnHeader";
import { RefreshButton } from "@/components/RefreshButton";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Textarea } from "@/components/ui/textarea";
import {
  Sheet,
  SheetContent,
  SheetDescription,
  SheetFooter,
  SheetHeader,
  SheetTitle,
} from "@/components/ui/sheet";
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

// 内容创作 - 提示词管理(主从):分类目录 → 分镜镜头提示词。接真实后端 prompt_category / shot_prompt 命令。

// 提示词全局搜索:匹配标题与正文
const promptFilterFn: FilterFn<ShotPromptView> = (row, _columnId, value) => {
  const q = String(value).toLowerCase();
  return (
    row.original.name.toLowerCase().includes(q) ||
    row.original.content.toLowerCase().includes(q)
  );
};

export function PromptStudioPage() {
  const [sbCollapsed, setSbCollapsed] = useResponsiveCollapse();
  const [categories, setCategories] = useState<PromptCategoryView[]>([]);
  const [prompts, setPrompts] = useState<ShotPromptView[]>([]);
  // 各分类提示词数量(左侧列表角标),按需异步加载
  const [promptCounts, setPromptCounts] = useState<Record<string, number>>({});
  const [selectedId, setSelectedId] = useState<string>("");
  const [error, setError] = useState<string | null>(null);

  // 分类的新增 / 编辑 / 删除
  const [categoryForm, setCategoryForm] = useState<PromptCategoryView | null>(
    null,
  );
  const [isCategoryFormOpen, setIsCategoryFormOpen] = useState(false);
  const [categoryDeleteTarget, setCategoryDeleteTarget] =
    useState<PromptCategoryView | null>(null);

  // 提示词的新增 / 编辑 / 删除
  const [promptForm, setPromptForm] = useState<ShotPromptView | null>(null);
  const [isPromptFormOpen, setIsPromptFormOpen] = useState(false);
  const [promptDeleteTarget, setPromptDeleteTarget] =
    useState<ShotPromptView | null>(null);

  const selectedCategory =
    categories.find((c) => c.id === selectedId) ?? null;

  // 加载分类列表,并并行统计各分类提示词数量
  const loadCategories = useCallback(async () => {
    try {
      const list = await api.listPromptCategories();
      setCategories(list);
      setSelectedId((prev) => prev || list[0]?.id || "");
      const entries = await Promise.all(
        list.map((cat) =>
          api
            .listShotPrompts(cat.id)
            .then((items) => [cat.id, items.length] as const)
            .catch(() => [cat.id, 0] as const),
        ),
      );
      setPromptCounts(Object.fromEntries(entries));
    } catch (e) {
      setError(String(e));
    }
  }, []);

  // 加载选中分类的提示词
  const loadPrompts = useCallback(async (categoryId: string) => {
    if (!categoryId) {
      setPrompts([]);
      return;
    }
    try {
      const list = await api.listShotPrompts(categoryId);
      setPrompts(list);
      setPromptCounts((prev) => ({ ...prev, [categoryId]: list.length }));
    } catch (e) {
      setError(String(e));
    }
  }, []);

  useEffect(() => {
    loadCategories();
  }, [loadCategories]);

  useEffect(() => {
    loadPrompts(selectedId);
  }, [selectedId, loadPrompts]);

  const promptCountOf = (categoryId: string) => promptCounts[categoryId] ?? 0;

  // ---- 分类操作 ----
  function openCreateCategory() {
    setCategoryForm(null);
    setIsCategoryFormOpen(true);
  }

  async function submitCategory(category: PromptCategoryInput) {
    try {
      await api.upsertPromptCategory(category);
      setSelectedId(category.id);
      setIsCategoryFormOpen(false);
      await loadCategories();
    } catch (e) {
      setError(String(e));
    }
  }

  async function confirmDeleteCategory() {
    if (!categoryDeleteTarget) return;
    const targetId = categoryDeleteTarget.id;
    try {
      await api.removePromptCategory(targetId);
      if (selectedId === targetId) {
        setSelectedId("");
        setPrompts([]);
      }
      await loadCategories();
    } catch (e) {
      setError(String(e));
    }
    setCategoryDeleteTarget(null);
  }

  // ---- 提示词操作 ----
  async function submitPrompt(prompt: ShotPromptInput) {
    try {
      await api.upsertShotPrompt(prompt);
      setIsPromptFormOpen(false);
      await loadPrompts(selectedId);
    } catch (e) {
      setError(String(e));
    }
  }

  async function confirmDeletePrompt() {
    if (!promptDeleteTarget) return;
    try {
      await api.removeShotPrompt(promptDeleteTarget.id);
      await loadPrompts(selectedId);
    } catch (e) {
      setError(String(e));
    }
    setPromptDeleteTarget(null);
  }

  const columns = useMemo<ColumnDef<ShotPromptView>[]>(
    () => [
      {
        accessorKey: "name",
        header: ({ column }) => (
          <DataTableColumnHeader column={column} title="标题" />
        ),
        cell: ({ row }) => (
          <span className="font-medium text-foreground">{row.original.name}</span>
        ),
      },
      {
        accessorKey: "content",
        header: "提示词",
        enableSorting: false,
        cell: ({ row }) => (
          <span className="line-clamp-2 max-w-md text-sm text-muted-foreground">
            {row.original.content}
          </span>
        ),
      },
      {
        accessorKey: "remark",
        header: "备注",
        enableSorting: false,
        cell: ({ row }) => (
          <span className="line-clamp-1 max-w-40 text-sm text-muted-foreground">
            {row.original.remark}
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
                  <DropdownMenuItem
                    onClick={() => {
                      setPromptForm(p);
                      setIsPromptFormOpen(true);
                    }}
                  >
                    <SquarePen />
                    编辑
                  </DropdownMenuItem>
                  <DropdownMenuSeparator />
                  <DropdownMenuItem
                    className="text-destructive focus:text-destructive"
                    onClick={() => setPromptDeleteTarget(p)}
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

  return (
    <>
      <div
        className={`flex min-h-0 flex-1 flex-col gap-4 ${FORM_CONTROL_SIZING}`}
      >
        <ErrorBanner message={error} onClose={() => setError(null)} />
        <div className="flex min-h-0 flex-1 gap-4">
          {/* 左侧:提示词分类目录(可收起) */}
          {!sbCollapsed && (
            <div className="flex w-56 shrink-0 flex-col overflow-hidden rounded-xl border bg-card lg:w-64">
              <div className="flex h-8 items-center justify-between border-b px-4">
                <span className="text-sm font-semibold">提示词分类</span>
                <div className="flex items-center gap-1">
                  <Button
                    size="icon-sm"
                    variant="ghost"
                    onClick={openCreateCategory}
                  >
                    <Plus />
                    <span className="sr-only">新增分类</span>
                  </Button>
                  <SimpleTooltip content="收起">
                    <Button
                      variant="ghost"
                      size="icon-xs"
                      className="cursor-pointer"
                      onClick={() => setSbCollapsed(true)}
                    >
                      <ChevronLeft />
                    </Button>
                  </SimpleTooltip>
                </div>
              </div>
              <div className="flex-1 space-y-0.5 overflow-auto p-2">
                {categories.length === 0 && (
                  <div className="flex flex-col items-center gap-2 px-2 py-8 text-center">
                    <Lightbulb className="size-6 text-muted-foreground/50" />
                    <p className="text-xs text-muted-foreground">
                      暂无分类,点击右上角 + 新增
                    </p>
                  </div>
                )}
                {categories.map((cat) => {
                  const isActive = cat.id === selectedId;
                  return (
                    <div
                      key={cat.id}
                      onClick={() => setSelectedId(cat.id)}
                      className={`group flex cursor-pointer items-center gap-2 rounded-md px-2 py-2 text-sm transition-colors ${
                        isActive
                          ? "bg-accent font-medium text-accent-foreground"
                          : "hover:bg-accent/50"
                      }`}
                    >
                      <span className="flex-1 truncate">{cat.name}</span>
                      <span className="text-xs text-muted-foreground">
                        {promptCountOf(cat.id)}
                      </span>
                      <DropdownMenu>
                        <DropdownMenuTrigger asChild>
                          <Button
                            variant="ghost"
                            size="icon-xs"
                            className="text-muted-foreground"
                            onClick={(e) => e.stopPropagation()}
                          >
                            <MoreVertical />
                            <span className="sr-only">操作</span>
                          </Button>
                        </DropdownMenuTrigger>
                        <DropdownMenuContent align="end" className="w-32">
                          <DropdownMenuItem
                            onClick={() => {
                              setCategoryForm(cat);
                              setIsCategoryFormOpen(true);
                            }}
                          >
                            <SquarePen />
                            编辑
                          </DropdownMenuItem>
                          <DropdownMenuSeparator />
                          <DropdownMenuItem
                            className="text-destructive focus:text-destructive"
                            onClick={() => setCategoryDeleteTarget(cat)}
                          >
                            <Trash2 />
                            删除
                          </DropdownMenuItem>
                        </DropdownMenuContent>
                      </DropdownMenu>
                    </div>
                  );
                })}
              </div>
            </div>
          )}

          {/* 右侧:提示词数据表 / 占位 */}
          <div className="flex min-w-0 flex-1 flex-col gap-2">
            {sbCollapsed && !selectedCategory && (
              <div>
                <SimpleTooltip content="展开分类筛选">
                  <Button
                    variant="outline"
                    className="cursor-pointer"
                    onClick={() => setSbCollapsed(false)}
                  >
                    <Filter />
                    分类
                  </Button>
                </SimpleTooltip>
              </div>
            )}
            {selectedCategory ? (
              <DataTable
                columns={columns}
                data={prompts}
                itemLabel="提示词"
                globalFilterFn={promptFilterFn}
                getRowId={(p) => p.id}
                emptyState={
                  <EmptyState
                    title={`「${selectedCategory.name}」暂无提示词`}
                    description="点击右上角「新增提示词」添加"
                  />
                }
                renderToolbar={(table) => (
                  <div className="flex flex-col gap-3 sm:flex-row sm:items-center sm:justify-between">
                    <div className="flex items-center gap-2">
                      {sbCollapsed && (
                        <SimpleTooltip content="展开分类筛选">
                          <Button
                            variant="outline"
                            className="cursor-pointer"
                            onClick={() => setSbCollapsed(false)}
                          >
                            <Filter />
                            分类
                          </Button>
                        </SimpleTooltip>
                      )}
                      <div className="relative w-full sm:w-80 lg:w-96">
                        <Search className="pointer-events-none absolute left-3 top-1/2 size-4 -translate-y-1/2 text-muted-foreground" />
                        <Input
                          placeholder={`搜索「${selectedCategory.name}」的提示词`}
                          className="pl-9"
                          value={(table.getState().globalFilter as string) ?? ""}
                          onChange={(e) => table.setGlobalFilter(e.target.value)}
                        />
                      </div>
                    </div>
                    <div className="flex items-center gap-2">
                      <RefreshButton onClick={() => loadPrompts(selectedId)} />
                      <Button
                        onClick={() => {
                          setPromptForm(null);
                          setIsPromptFormOpen(true);
                        }}
                      >
                        <Plus />
                        新增提示词
                      </Button>
                    </div>
                  </div>
                )}
              />
            ) : (
              <div className="flex min-h-0 flex-1 items-center justify-center rounded-xl border border-dashed">
                <EmptyState
                  title="请选择提示词分类"
                  description="在左侧选择或新增一个分类目录后管理提示词"
                />
              </div>
            )}
          </div>
        </div>
      </div>

      <CategoryFormSheet
        key={isCategoryFormOpen ? (categoryForm?.id ?? "new-category") : "idle"}
        open={isCategoryFormOpen}
        initial={categoryForm}
        onOpenChange={setIsCategoryFormOpen}
        onSubmit={submitCategory}
      />

      <PromptFormSheet
        key={isPromptFormOpen ? (promptForm?.id ?? "new-prompt") : "idle"}
        open={isPromptFormOpen}
        initial={promptForm}
        categoryId={selectedId}
        onOpenChange={setIsPromptFormOpen}
        onSubmit={submitPrompt}
      />

      <AlertDialog
        open={categoryDeleteTarget !== null}
        onOpenChange={(open) => !open && setCategoryDeleteTarget(null)}
      >
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>
              删除分类「{categoryDeleteTarget?.name}」?
            </AlertDialogTitle>
            <AlertDialogDescription>
              该分类下的所有提示词也会一并删除,此操作不可恢复。
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel>取消</AlertDialogCancel>
            <AlertDialogAction
              onClick={confirmDeleteCategory}
              className="bg-destructive text-white hover:bg-destructive/90"
            >
              删除
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>

      <AlertDialog
        open={promptDeleteTarget !== null}
        onOpenChange={(open) => !open && setPromptDeleteTarget(null)}
      >
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>
              删除提示词「{promptDeleteTarget?.name}」?
            </AlertDialogTitle>
            <AlertDialogDescription>
              删除后不可恢复。
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel>取消</AlertDialogCancel>
            <AlertDialogAction
              onClick={confirmDeletePrompt}
              className="bg-destructive text-white hover:bg-destructive/90"
            >
              删除
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
    </>
  );
}

// 分类 新增 / 编辑 抽屉
function CategoryFormSheet({
  open,
  initial,
  onOpenChange,
  onSubmit,
}: {
  open: boolean;
  initial: PromptCategoryView | null;
  onOpenChange: (open: boolean) => void;
  onSubmit: (category: PromptCategoryInput) => void;
}) {
  const isEdit = initial !== null;
  const [name, setName] = useState(initial?.name ?? "");
  const [remark, setRemark] = useState(initial?.remark ?? "");
  const [error, setError] = useState<string | null>(null);

  function handleSubmit(event: FormEvent) {
    event.preventDefault();
    if (!name.trim()) {
      setError("请输入分类名称");
      return;
    }
    onSubmit({
      id: initial?.id ?? crypto.randomUUID(),
      name: name.trim(),
      remark: remark.trim(),
    });
  }

  return (
    <Sheet open={open} onOpenChange={onOpenChange}>
      <SheetContent
        className="flex w-full flex-col gap-0 p-0 sm:max-w-[600px]"
        blockClose={
          name !== (initial?.name ?? "") || remark !== (initial?.remark ?? "")
        }
      >
        <SheetHeader className="border-b">
          <SheetTitle>{isEdit ? "编辑分类" : "新增分类"}</SheetTitle>
          <SheetDescription>
            分类用于归类分镜镜头提示词,如 图像分镜 / 视频分镜 / 镜头景别。
          </SheetDescription>
        </SheetHeader>
        <form onSubmit={handleSubmit} className="flex min-h-0 flex-1 flex-col">
          <div className="flex-1 space-y-4 overflow-y-auto p-5">
            {error && (
              <div className="rounded-lg bg-destructive/10 px-3 py-2 text-sm text-destructive">
                {error}
              </div>
            )}
            <div className="space-y-1.5">
              <Label htmlFor="category-name">分类名称</Label>
              <Input
                id="category-name"
                placeholder="如:图像分镜 / 视频分镜 / 特写镜头"
                value={name}
                onChange={(e) => setName(e.target.value)}
                autoFocus
              />
            </div>
            <div className="space-y-1.5">
              <Label htmlFor="category-remark">备注</Label>
              <Input
                id="category-remark"
                placeholder="补充说明,选填"
                value={remark}
                onChange={(e) => setRemark(e.target.value)}
              />
            </div>
          </div>
          <SheetFooter className="flex-row justify-end gap-2 border-t">
            <Button
              type="button"
              variant="outline"
              onClick={() => onOpenChange(false)}
            >
              取消
            </Button>
            <Button type="submit">保存</Button>
          </SheetFooter>
        </form>
      </SheetContent>
    </Sheet>
  );
}

// 提示词 新增 / 编辑 抽屉
function PromptFormSheet({
  open,
  initial,
  categoryId,
  onOpenChange,
  onSubmit,
}: {
  open: boolean;
  initial: ShotPromptView | null;
  categoryId: string;
  onOpenChange: (open: boolean) => void;
  onSubmit: (prompt: ShotPromptInput) => void;
}) {
  const isEdit = initial !== null;
  const [name, setName] = useState(initial?.name ?? "");
  const [content, setContent] = useState(initial?.content ?? "");
  const [remark, setRemark] = useState(initial?.remark ?? "");
  const [error, setError] = useState<string | null>(null);

  function handleSubmit(event: FormEvent) {
    event.preventDefault();
    if (!name.trim()) {
      setError("请输入提示词标题");
      return;
    }
    if (!content.trim()) {
      setError("请输入提示词正文");
      return;
    }
    onSubmit({
      id: initial?.id ?? crypto.randomUUID(),
      categoryId: initial?.categoryId ?? categoryId,
      name: name.trim(),
      content: content.trim(),
      remark: remark.trim(),
    });
  }

  const isDirty =
    name !== (initial?.name ?? "") ||
    content !== (initial?.content ?? "") ||
    remark !== (initial?.remark ?? "");

  return (
    <Sheet open={open} onOpenChange={onOpenChange}>
      <SheetContent
        className="flex w-full flex-col gap-0 p-0 sm:max-w-[600px]"
        blockClose={isDirty}
      >
        <SheetHeader className="border-b">
          <SheetTitle>{isEdit ? "编辑提示词" : "新增提示词"}</SheetTitle>
          <SheetDescription>
            分镜镜头提示词,用于图像 / 视频生成。
          </SheetDescription>
        </SheetHeader>
        <form onSubmit={handleSubmit} className="flex min-h-0 flex-1 flex-col">
          <div className="flex-1 space-y-4 overflow-y-auto p-5">
            {error && (
              <div className="rounded-lg bg-destructive/10 px-3 py-2 text-sm text-destructive">
                {error}
              </div>
            )}
            <div className="space-y-1.5">
              <Label htmlFor="prompt-name">标题</Label>
              <Input
                id="prompt-name"
                placeholder="如:远景开场 / 产品特写"
                value={name}
                onChange={(e) => setName(e.target.value)}
                autoFocus
              />
            </div>
            <div className="space-y-1.5">
              <Label htmlFor="prompt-content">提示词正文</Label>
              <Textarea
                id="prompt-content"
                className="min-h-60"
                placeholder="输入该分镜镜头的提示词正文…"
                value={content}
                onChange={(e) => setContent(e.target.value)}
              />
            </div>
            <div className="space-y-1.5">
              <Label htmlFor="prompt-remark">备注</Label>
              <Input
                id="prompt-remark"
                placeholder="补充说明,选填"
                value={remark}
                onChange={(e) => setRemark(e.target.value)}
              />
            </div>
          </div>
          <SheetFooter className="flex-row justify-end gap-2 border-t">
            <Button
              type="button"
              variant="outline"
              onClick={() => onOpenChange(false)}
            >
              取消
            </Button>
            <Button type="submit">保存</Button>
          </SheetFooter>
        </form>
      </SheetContent>
    </Sheet>
  );
}
