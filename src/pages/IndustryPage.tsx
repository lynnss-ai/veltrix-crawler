import {
  useCallback,
  useEffect,
  useMemo,
  useState,
  type FormEvent,
} from "react";
import {
  type ColumnDef,
  type FilterFn,
} from "@tanstack/react-table";
import {
  Check,
  ChevronLeft,
  Copy,
  Filter,
  MoreVertical,
  SquarePen,
  Plus,
  RefreshCw,
  Radar,
  Search,
  Trash2,
} from "lucide-react";
import { useResponsiveCollapse } from "@/hooks/use-responsive-collapse";
import { SimpleTooltip } from "@/components/SimpleTooltip";
import {
  api,
  type IndustryInput,
  type IndustryView,
  type KeywordDto,
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

// 行业类别 + 关键词管理(主从)。接真实后端 industry / keyword 命令。

// 行业编码字符集(去掉易混淆的 0/O/1/I/L)与长度
const CODE_CHARS = "ABCDEFGHJKMNPQRSTUVWXYZ23456789";
const CODE_LENGTH = 6;

// 生成行业编码,如 IND-7K3P9Q
function generateIndustryCode(): string {
  let suffix = "";
  for (let i = 0; i < CODE_LENGTH; i += 1) {
    suffix += CODE_CHARS[Math.floor(Math.random() * CODE_CHARS.length)];
  }
  return `IND-${suffix}`;
}

// 关键词全局搜索:匹配关键词文本
const keywordFilterFn: FilterFn<KeywordDto> = (row, _columnId, value) =>
  row.original.word.toLowerCase().includes(String(value).toLowerCase());

export function IndustryPage() {
  const [sbCollapsed, setSbCollapsed] = useResponsiveCollapse();
  const [industries, setIndustries] = useState<IndustryView[]>([]);
  const [keywords, setKeywords] = useState<KeywordDto[]>([]);
  // 各行业关键词数量(左侧列表角标),按需异步加载
  const [keywordCounts, setKeywordCounts] = useState<Record<string, number>>(
    {},
  );
  const [selectedId, setSelectedId] = useState<string>("");
  const [error, setError] = useState<string | null>(null);

  // 行业的新增 / 编辑 / 删除
  const [industryForm, setIndustryForm] = useState<IndustryView | null>(null);
  const [isIndustryFormOpen, setIsIndustryFormOpen] = useState(false);
  const [industryDeleteTarget, setIndustryDeleteTarget] =
    useState<IndustryView | null>(null);

  // 关键词的新增 / 编辑 / 删除
  const [keywordForm, setKeywordForm] = useState<KeywordDto | null>(null);
  const [isKeywordFormOpen, setIsKeywordFormOpen] = useState(false);
  const [keywordDeleteTarget, setKeywordDeleteTarget] =
    useState<KeywordDto | null>(null);

  const selectedIndustry = industries.find((i) => i.id === selectedId) ?? null;

  // 加载行业列表,并并行统计各行业关键词数量
  const loadIndustries = useCallback(async () => {
    try {
      const list = await api.listIndustries();
      setIndustries(list);
      setSelectedId((prev) => prev || list[0]?.id || "");
      const entries = await Promise.all(
        list.map((ind) =>
          api
            .listKeywords(ind.id)
            .then((kws) => [ind.id, kws.length] as const)
            .catch(() => [ind.id, 0] as const),
        ),
      );
      setKeywordCounts(Object.fromEntries(entries));
    } catch (e) {
      setError(String(e));
    }
  }, []);

  // 加载选中行业的关键词
  const loadKeywords = useCallback(async (industryId: string) => {
    if (!industryId) {
      setKeywords([]);
      return;
    }
    try {
      const list = await api.listKeywords(industryId);
      setKeywords(list);
      setKeywordCounts((prev) => ({ ...prev, [industryId]: list.length }));
    } catch (e) {
      setError(String(e));
    }
  }, []);

  useEffect(() => {
    loadIndustries();
  }, [loadIndustries]);

  useEffect(() => {
    loadKeywords(selectedId);
  }, [selectedId, loadKeywords]);

  const keywordData = keywords;

  const keywordCountOf = (industryId: string) => keywordCounts[industryId] ?? 0;

  // ---- 行业操作 ----
  function openCreateIndustry() {
    setIndustryForm(null);
    setIsIndustryFormOpen(true);
  }

  async function submitIndustry(industry: IndustryInput) {
    try {
      await api.upsertIndustry(industry);
      setSelectedId(industry.id);
      setIsIndustryFormOpen(false);
      await loadIndustries();
    } catch (e) {
      setError(String(e));
    }
  }

  async function confirmDeleteIndustry() {
    if (!industryDeleteTarget) return;
    const targetId = industryDeleteTarget.id;
    try {
      await api.removeIndustry(targetId);
      // 删除后清空右侧并刷新行业列表
      if (selectedId === targetId) {
        setSelectedId("");
        setKeywords([]);
      }
      await loadIndustries();
    } catch (e) {
      setError(String(e));
    }
    setIndustryDeleteTarget(null);
  }

  // ---- 关键词操作 ----
  // 批量新增(多行) / 单个编辑(upsert),完成后刷新当前行业关键词
  async function submitKeywords(items: KeywordDto[], isEdit: boolean) {
    try {
      if (isEdit) {
        for (const kw of items) {
          await api.upsertKeyword(kw);
        }
      } else {
        await api.createKeywords(
          selectedId,
          items.map((k) => k.word),
        );
      }
      setIsKeywordFormOpen(false);
      await loadKeywords(selectedId);
    } catch (e) {
      setError(String(e));
    }
  }

  async function confirmDeleteKeyword() {
    if (!keywordDeleteTarget) return;
    try {
      await api.removeKeyword(keywordDeleteTarget.id);
      await loadKeywords(selectedId);
    } catch (e) {
      setError(String(e));
    }
    setKeywordDeleteTarget(null);
  }

  const columns = useMemo<ColumnDef<KeywordDto>[]>(
    () => [
      {
        accessorKey: "word",
        header: ({ column }) => (
          <DataTableColumnHeader column={column} title="关键词" />
        ),
        cell: ({ row }) => (
          <span className="font-medium text-foreground">
            {row.original.word}
          </span>
        ),
      },
      {
        id: "actions",
        header: () => <div className="text-right">操作</div>,
        enableSorting: false,
        cell: ({ row }) => {
          const k = row.original;
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
                      setKeywordForm(k);
                      setIsKeywordFormOpen(true);
                    }}
                  >
                    <SquarePen />
                    编辑
                  </DropdownMenuItem>
                  <DropdownMenuSeparator />
                  <DropdownMenuItem
                    className="text-destructive focus:text-destructive"
                    onClick={() => setKeywordDeleteTarget(k)}
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
          {/* 左侧:行业类别(可收起) */}
        {!sbCollapsed && (
        <div className="flex w-56 shrink-0 flex-col overflow-hidden rounded-xl border bg-card lg:w-64">
          <div className="flex h-8 items-center justify-between border-b px-4">
            <span className="text-sm font-semibold">行业类别</span>
            <div className="flex items-center gap-1">
              <Button size="icon-sm" variant="ghost" onClick={openCreateIndustry}>
                <Plus />
                <span className="sr-only">新增行业</span>
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
            {industries.length === 0 && (
              <div className="flex flex-col items-center gap-2 px-2 py-8 text-center">
                <Radar className="size-6 text-muted-foreground/50" />
                <p className="text-xs text-muted-foreground">
                  暂无行业,点击右上角 + 新增
                </p>
              </div>
            )}
            {industries.map((ind) => {
              const isActive = ind.id === selectedId;
              return (
                <div
                  key={ind.id}
                  onClick={() => setSelectedId(ind.id)}
                  className={`group flex cursor-pointer items-center gap-2 rounded-md px-2 py-2 text-sm transition-colors ${
                    isActive
                      ? "bg-accent font-medium text-accent-foreground"
                      : "hover:bg-accent/50"
                  }`}
                >
                  <span className="flex-1 truncate">{ind.name}</span>
                  <span className="text-xs text-muted-foreground">
                    {keywordCountOf(ind.id)}
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
                          setIndustryForm(ind);
                          setIsIndustryFormOpen(true);
                        }}
                      >
                        <SquarePen />
                        编辑
                      </DropdownMenuItem>
                      <DropdownMenuSeparator />
                      <DropdownMenuItem
                        className="text-destructive focus:text-destructive"
                        onClick={() => setIndustryDeleteTarget(ind)}
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

        {/* 右侧:数据表(展开按钮在 toolbar 内,与搜索框对齐)/ 占位 */}
        <div className="flex min-w-0 flex-1 flex-col gap-2">
          {/* 占位态下没有 DataTable toolbar,这里也放一个展开按钮 */}
          {sbCollapsed && !selectedIndustry && (
            <div>
              <SimpleTooltip content="展开行业筛选">
                <Button
                  variant="outline"
                  className="cursor-pointer"
                  onClick={() => setSbCollapsed(false)}
                >
                  <Filter />
                  行业
                </Button>
              </SimpleTooltip>
            </div>
          )}
        {selectedIndustry ? (
          <DataTable
            columns={columns}
            data={keywordData}
            itemLabel="关键词"
            globalFilterFn={keywordFilterFn}
            getRowId={(k) => k.id}
            emptyState={
              <EmptyState
                title={`「${selectedIndustry.name}」暂无关键词`}
                description="点击右上角「新增关键词」添加"
              />
            }
            renderToolbar={(table) => (
              <div className="flex flex-col gap-3 sm:flex-row sm:items-center sm:justify-between">
                <div className="flex items-center gap-2">
                  {sbCollapsed && (
                    <SimpleTooltip content="展开行业筛选">
                      <Button
                        variant="outline"
                        className="cursor-pointer"
                        onClick={() => setSbCollapsed(false)}
                      >
                        <Filter />
                        行业
                      </Button>
                    </SimpleTooltip>
                  )}
                  <div className="relative w-full sm:w-80 lg:w-96">
                    <Search className="pointer-events-none absolute left-3 top-1/2 size-4 -translate-y-1/2 text-muted-foreground" />
                    <Input
                      placeholder={`搜索「${selectedIndustry.name}」的关键词`}
                      className="pl-9"
                      value={(table.getState().globalFilter as string) ?? ""}
                      onChange={(e) => table.setGlobalFilter(e.target.value)}
                    />
                  </div>
                </div>
                <div className="flex items-center gap-2">
                  <RefreshButton onClick={() => loadKeywords(selectedId)} />
                  <Button
                    onClick={() => {
                      setKeywordForm(null);
                      setIsKeywordFormOpen(true);
                    }}
                  >
                    <Plus />
                    新增关键词
                  </Button>
                </div>
              </div>
            )}
          />
        ) : (
          <div className="flex min-h-0 flex-1 items-center justify-center rounded-xl border border-dashed">
            <EmptyState
              title="请选择行业类别"
              description="在左侧选择或新增一个行业类别后查看关键词"
            />
          </div>
        )}
        </div>
        </div>
      </div>

      <IndustryFormSheet
        key={isIndustryFormOpen ? (industryForm?.id ?? "new-industry") : "idle"}
        open={isIndustryFormOpen}
        initial={industryForm}
        onOpenChange={setIsIndustryFormOpen}
        onSubmit={submitIndustry}
      />

      <KeywordFormSheet
        key={isKeywordFormOpen ? (keywordForm?.id ?? "new-keyword") : "idle"}
        open={isKeywordFormOpen}
        initial={keywordForm}
        industryId={selectedId}
        onOpenChange={setIsKeywordFormOpen}
        onSubmit={submitKeywords}
      />

      <AlertDialog
        open={industryDeleteTarget !== null}
        onOpenChange={(open) => !open && setIndustryDeleteTarget(null)}
      >
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>
              删除行业「{industryDeleteTarget?.name}」?
            </AlertDialogTitle>
            <AlertDialogDescription>
              该行业下的所有关键词也会一并删除,此操作不可恢复。
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel>取消</AlertDialogCancel>
            <AlertDialogAction
              onClick={confirmDeleteIndustry}
              className="bg-destructive text-white hover:bg-destructive/90"
            >
              删除
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>

      <AlertDialog
        open={keywordDeleteTarget !== null}
        onOpenChange={(open) => !open && setKeywordDeleteTarget(null)}
      >
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>
              删除关键词「{keywordDeleteTarget?.word}」?
            </AlertDialogTitle>
            <AlertDialogDescription>
              删除后将不再用于该行业的采集匹配。
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel>取消</AlertDialogCancel>
            <AlertDialogAction
              onClick={confirmDeleteKeyword}
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

// 行业 新增 / 编辑 抽屉
function IndustryFormSheet({
  open,
  initial,
  onOpenChange,
  onSubmit,
}: {
  open: boolean;
  initial: IndustryView | null;
  onOpenChange: (open: boolean) => void;
  onSubmit: (industry: IndustryInput) => void;
}) {
  const isEdit = initial !== null;
  const [code, setCode] = useState(initial?.code ?? generateIndustryCode());
  const [name, setName] = useState(initial?.name ?? "");
  const [remark, setRemark] = useState(initial?.remark ?? "");
  const [error, setError] = useState<string | null>(null);
  const [copied, setCopied] = useState(false);

  // 复制编码到剪贴板(部分环境无权限时提示手动复制)
  async function handleCopyCode() {
    try {
      await navigator.clipboard.writeText(code);
      setCopied(true);
      setTimeout(() => setCopied(false), 1500);
    } catch {
      setError("复制失败,请手动复制");
    }
  }

  function handleSubmit(event: FormEvent) {
    event.preventDefault();
    if (!name.trim()) {
      setError("请输入行业名称");
      return;
    }
    onSubmit({
      id: initial?.id ?? crypto.randomUUID(),
      code,
      name: name.trim(),
      remark: remark.trim(),
    });
  }

  return (
    <Sheet open={open} onOpenChange={onOpenChange}>
      <SheetContent
        className="flex w-full flex-col gap-0 p-0 sm:max-w-[600px]"
        blockClose={name !== (initial?.name ?? "") || remark !== (initial?.remark ?? "")}
      >
        <SheetHeader className="border-b">
          <SheetTitle>{isEdit ? "编辑行业" : "新增行业"}</SheetTitle>
          <SheetDescription>行业用于归类采集关键词。</SheetDescription>
        </SheetHeader>
        <form onSubmit={handleSubmit} className="flex min-h-0 flex-1 flex-col">
          <div className="flex-1 space-y-4 overflow-y-auto p-5">
            {error && (
              <div className="rounded-lg bg-destructive/10 px-3 py-2 text-sm text-destructive">
                {error}
              </div>
            )}
            <div className="space-y-1.5">
              <Label htmlFor="industry-name">行业名称</Label>
              <Input
                id="industry-name"
                placeholder="如:美妆个护 / 3C 数码"
                value={name}
                onChange={(e) => setName(e.target.value)}
                autoFocus
              />
            </div>
            <div className="space-y-1.5">
              <Label htmlFor="industry-code">编码</Label>
              <div className="flex items-center gap-2">
                <Input
                  id="industry-code"
                  value={code}
                  readOnly
                  className="font-mono"
                />
                <Button
                  type="button"
                  variant="outline"
                  size="icon"
                  className="shrink-0"
                  title="重新生成"
                  onClick={() => setCode(generateIndustryCode())}
                >
                  <RefreshCw />
                  <span className="sr-only">重新生成</span>
                </Button>
                <Button
                  type="button"
                  variant="outline"
                  size="icon"
                  className="shrink-0"
                  title={copied ? "已复制" : "复制"}
                  onClick={handleCopyCode}
                >
                  {copied ? <Check /> : <Copy />}
                  <span className="sr-only">复制</span>
                </Button>
              </div>
              <p className="text-xs text-muted-foreground">
                系统自动生成,可点刷新重新生成
              </p>
            </div>
            <div className="space-y-1.5">
              <Label htmlFor="industry-remark">备注</Label>
              <Input
                id="industry-remark"
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

// 关键词 新增(批量,每行一个) / 编辑(单个) 抽屉
function KeywordFormSheet({
  open,
  initial,
  industryId,
  onOpenChange,
  onSubmit,
}: {
  open: boolean;
  initial: KeywordDto | null;
  industryId: string;
  onOpenChange: (open: boolean) => void;
  onSubmit: (keywords: KeywordDto[], isEdit: boolean) => void;
}) {
  const isEdit = initial !== null;
  const [word, setWord] = useState(initial?.word ?? ""); // 编辑:单个
  const [bulk, setBulk] = useState(""); // 新增:多行批量
  const [error, setError] = useState<string | null>(null);

  function handleSubmit(event: FormEvent) {
    event.preventDefault();
    if (isEdit) {
      if (!word.trim()) {
        setError("请输入关键词");
        return;
      }
      onSubmit([{ ...initial, word: word.trim() }], true);
      return;
    }
    // 批量:按行拆分,去掉空行与重复项;id 由后端生成,这里仅传 word
    const words = Array.from(
      new Set(
        bulk
          .split("\n")
          .map((w) => w.trim())
          .filter(Boolean),
      ),
    );
    if (words.length === 0) {
      setError("请至少输入一个关键词");
      return;
    }
    onSubmit(
      words.map((w) => ({
        id: "",
        industryId,
        word: w,
      })),
      false,
    );
  }

  return (
    <Sheet open={open} onOpenChange={onOpenChange}>
      <SheetContent
        className="flex w-full flex-col gap-0 p-0 sm:max-w-[600px]"
        blockClose={isEdit ? word !== (initial?.word ?? "") : bulk.trim() !== ""}
      >
        <SheetHeader className="border-b">
          <SheetTitle>{isEdit ? "编辑关键词" : "新增关键词"}</SheetTitle>
          <SheetDescription>
            {isEdit
              ? "修改该关键词。"
              : "每行一个关键词,支持批量添加;重复项会自动忽略。"}
          </SheetDescription>
        </SheetHeader>
        <form onSubmit={handleSubmit} className="flex min-h-0 flex-1 flex-col">
          <div className="flex-1 space-y-4 overflow-y-auto p-5">
            {error && (
              <div className="rounded-lg bg-destructive/10 px-3 py-2 text-sm text-destructive">
                {error}
              </div>
            )}
            {isEdit ? (
              <div className="space-y-1.5">
                <Label htmlFor="keyword-word">关键词</Label>
                <Input
                  id="keyword-word"
                  placeholder="输入关键词"
                  value={word}
                  onChange={(e) => setWord(e.target.value)}
                  autoFocus
                />
              </div>
            ) : (
              <div className="space-y-1.5">
                <Label htmlFor="keyword-bulk">关键词</Label>
                <Textarea
                  id="keyword-bulk"
                  className="min-h-80"
                  placeholder={"每行一个关键词,例如:\n双十一\n直播带货\n购物节"}
                  value={bulk}
                  onChange={(e) => setBulk(e.target.value)}
                  autoFocus
                />
                <p className="text-xs text-muted-foreground">
                  每行一个关键词,一次可添加多个
                </p>
              </div>
            )}
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
