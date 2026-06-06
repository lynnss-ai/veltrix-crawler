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
  MoreVertical,
  Pencil,
  Plus,
  Search,
  Trash2,
} from "lucide-react";
import { useResponsiveCollapse } from "@/hooks/use-responsive-collapse";
import { SimpleTooltip } from "@/components/SimpleTooltip";
import {
  api,
  type PlatformConfig,
  type ApiView,
  type ApiInput,
} from "@/lib/api";
import { FORM_CONTROL_SIZING } from "@/lib/form-sizing";
import { ErrorBanner } from "@/components/ErrorBanner";
import { DataTable } from "@/components/DataTable";
import { CodeField, generateCode } from "@/components/CodeField";
import { FieldError } from "@/components/FieldError";
import { DataTableColumnHeader } from "@/components/DataTableColumnHeader";
import { RefreshButton } from "@/components/RefreshButton";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Switch } from "@/components/ui/switch";
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

// 平台下的 API 配置:列表项复用后端 ApiView,表单提交用不含时间字段的 ApiInput
type ApiItem = ApiView;

function generatePlatformId(): string {
  return `plat-${Date.now().toString(36)}${Math.random().toString(36).slice(2, 6)}`;
}

function generateApiId(): string {
  return `api-${Date.now().toString(36)}${Math.random().toString(36).slice(2, 6)}`;
}

// API 全局搜索:匹配名称 / url / 备注
const apiFilterFn: FilterFn<ApiItem> = (row, _columnId, value) => {
  const a = row.original;
  return `${a.name} ${a.url} ${a.remark}`
    .toLowerCase()
    .includes(String(value).toLowerCase());
};

// 平台管理(主从):左侧平台(名称/链接/启用,真实 API),右侧该平台的 API 列表(占位)。
export function PlatformsPage() {
  const [sbCollapsed, setSbCollapsed] = useResponsiveCollapse();
  const [platforms, setPlatforms] = useState<PlatformConfig[]>([]);
  const [selectedId, setSelectedId] = useState("");
  // 当前选中平台的 API 子列表(真实持久化)
  const [apis, setApis] = useState<ApiItem[]>([]);
  const [error, setError] = useState<string | null>(null);

  const [platformForm, setPlatformForm] = useState<PlatformConfig | null>(null);
  const [isPlatformFormOpen, setIsPlatformFormOpen] = useState(false);
  const [platformDeleteTarget, setPlatformDeleteTarget] =
    useState<PlatformConfig | null>(null);

  const [apiForm, setApiForm] = useState<ApiItem | null>(null);
  const [isApiFormOpen, setIsApiFormOpen] = useState(false);
  const [apiDeleteTarget, setApiDeleteTarget] = useState<ApiItem | null>(null);

  const loadPlatforms = useCallback(async () => {
    try {
      const list = await api.listPlatforms();
      setPlatforms(list);
      setSelectedId((prev) => prev || list[0]?.id || "");
    } catch (e) {
      setError(String(e));
    }
  }, []);

  useEffect(() => {
    loadPlatforms();
  }, [loadPlatforms]);

  const loadApis = useCallback(async (platformId: string) => {
    if (!platformId) {
      setApis([]);
      return;
    }
    try {
      const list = await api.listApis(platformId);
      setApis(list);
    } catch (e) {
      setError(String(e));
    }
  }, []);

  // 选中平台变化时加载其 API 子列表
  useEffect(() => {
    loadApis(selectedId);
  }, [selectedId, loadApis]);

  const selected = platforms.find((p) => p.id === selectedId) ?? null;
  const apiData = apis;

  // ---- 平台操作(真实 API) ----
  function openCreatePlatform() {
    setPlatformForm(null);
    setIsPlatformFormOpen(true);
  }

  async function submitPlatform(platform: PlatformConfig) {
    try {
      await api.upsertPlatform(platform);
      setIsPlatformFormOpen(false);
      await loadPlatforms();
      setSelectedId(platform.id);
    } catch (e) {
      setError(String(e));
    }
  }

  async function confirmDeletePlatform() {
    if (!platformDeleteTarget) return;
    try {
      await api.removePlatform(platformDeleteTarget.id);
      await loadPlatforms();
      if (selectedId === platformDeleteTarget.id) setSelectedId("");
    } catch (e) {
      setError(String(e));
    }
    setPlatformDeleteTarget(null);
  }

  // ---- API 操作(真实持久化) ----
  async function submitApi(item: ApiInput) {
    try {
      await api.upsertApi(item);
      setIsApiFormOpen(false);
      await loadApis(item.platformId);
    } catch (e) {
      setError(String(e));
    }
  }

  async function confirmDeleteApi() {
    if (!apiDeleteTarget) return;
    const { id, platformId } = apiDeleteTarget;
    try {
      await api.removeApi(id);
      await loadApis(platformId);
    } catch (e) {
      setError(String(e));
    }
    setApiDeleteTarget(null);
  }

  const columns = useMemo<ColumnDef<ApiItem>[]>(
    () => [
      {
        accessorKey: "name",
        header: ({ column }) => (
          <DataTableColumnHeader column={column} title="API 名称" />
        ),
        cell: ({ row }) => (
          <span className="font-medium text-foreground">
            {row.original.name}
          </span>
        ),
      },
      {
        accessorKey: "url",
        header: "URL",
        enableSorting: false,
        cell: ({ row }) => (
          <span
            className="block max-w-[20rem] truncate font-mono text-xs text-muted-foreground"
            title={row.original.url}
          >
            {row.original.url}
          </span>
        ),
      },
      {
        accessorKey: "remark",
        header: "备注",
        enableSorting: false,
        cell: ({ row }) => (
          <span className="text-muted-foreground">
            {row.original.remark || "—"}
          </span>
        ),
      },
      {
        id: "actions",
        header: () => <div className="text-right">操作</div>,
        enableSorting: false,
        cell: ({ row }) => {
          const item = row.original;
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
                      setApiForm(item);
                      setIsApiFormOpen(true);
                    }}
                  >
                    <Pencil />
                    编辑
                  </DropdownMenuItem>
                  <DropdownMenuSeparator />
                  <DropdownMenuItem
                    className="text-destructive focus:text-destructive"
                    onClick={() => setApiDeleteTarget(item)}
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
    <div
      className={`flex min-h-0 flex-1 flex-col gap-4 ${FORM_CONTROL_SIZING}`}
    >
      <ErrorBanner message={error} onClose={() => setError(null)} />

      <div className="flex min-h-0 flex-1 gap-4">
        {/* 左侧:平台(可收起) */}
        {!sbCollapsed && (
        <div className="flex w-56 shrink-0 flex-col overflow-hidden rounded-xl border bg-card lg:w-64">
          <div className="flex items-center justify-between border-b px-4 py-3">
            <span className="text-sm font-semibold">平台</span>
            <div className="flex items-center gap-1">
              <Button size="icon-sm" variant="ghost" onClick={openCreatePlatform}>
                <Plus />
                <span className="sr-only">新增平台</span>
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
            {platforms.length === 0 && (
              <p className="px-2 py-6 text-center text-xs text-muted-foreground">
                暂无平台,点击右上角 + 新增
              </p>
            )}
            {platforms.map((p) => {
              const isActive = p.id === selectedId;
              return (
                <div
                  key={p.id}
                  onClick={() => setSelectedId(p.id)}
                  className={`group flex cursor-pointer items-center gap-2 rounded-md px-2 py-2 text-sm transition-colors ${
                    isActive
                      ? "bg-accent font-medium text-accent-foreground"
                      : "hover:bg-accent/50"
                  }`}
                >
                  <span
                    className={`size-1.5 shrink-0 rounded-full ${
                      p.enabled ? "bg-emerald-500" : "bg-muted-foreground/40"
                    }`}
                  />
                  <span className="flex-1 truncate">{p.name}</span>
                  {isActive && (
                    <span className="text-xs text-muted-foreground">
                      {apis.length}
                    </span>
                  )}
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
                          setPlatformForm(p);
                          setIsPlatformFormOpen(true);
                        }}
                      >
                        <Pencil />
                        编辑
                      </DropdownMenuItem>
                      <DropdownMenuSeparator />
                      <DropdownMenuItem
                        className="text-destructive focus:text-destructive"
                        onClick={() => setPlatformDeleteTarget(p)}
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

        {/* 右侧:数据表(展开按钮在 toolbar 内)/ 占位 */}
        <div className="flex min-w-0 flex-1 flex-col gap-2">
          {sbCollapsed && !selected && (
            <div>
              <SimpleTooltip content="展开平台筛选">
                <Button
                  variant="outline"
                  className="cursor-pointer"
                  onClick={() => setSbCollapsed(false)}
                >
                  <Filter />
                  平台
                </Button>
              </SimpleTooltip>
            </div>
          )}
        {selected ? (
          <DataTable
            columns={columns}
            data={apiData}
            itemLabel="个 API"
            globalFilterFn={apiFilterFn}
            getRowId={(a) => a.id}
            emptyState={
              <div className="flex flex-col items-center justify-center gap-2 py-12 text-center">
                <p className="text-sm font-medium text-foreground">
                  「{selected.name}」暂无 API
                </p>
                <p className="text-xs text-muted-foreground">
                  点击右上角「新增 API」添加该平台的接口配置
                </p>
              </div>
            }
            renderToolbar={(table) => (
              <div className="flex flex-col gap-3 sm:flex-row sm:items-center sm:justify-between">
                <div className="flex items-center gap-2">
                  {sbCollapsed && (
                    <SimpleTooltip content="展开平台筛选">
                      <Button
                        variant="outline"
                        className="cursor-pointer"
                        onClick={() => setSbCollapsed(false)}
                      >
                        <Filter />
                        平台
                      </Button>
                    </SimpleTooltip>
                  )}
                  <div className="relative w-full sm:w-72">
                    <Search className="pointer-events-none absolute left-3 top-1/2 size-4 -translate-y-1/2 text-muted-foreground" />
                    <Input
                      placeholder={`搜索「${selected.name}」的 API`}
                      className="pl-9"
                      value={(table.getState().globalFilter as string) ?? ""}
                      onChange={(e) => table.setGlobalFilter(e.target.value)}
                    />
                  </div>
                </div>
                <div className="flex items-center gap-2">
                  <RefreshButton onClick={() => loadApis(selectedId)} />
                  <Button
                    onClick={() => {
                      setApiForm(null);
                      setIsApiFormOpen(true);
                    }}
                  >
                    <Plus />
                    新增 API
                  </Button>
                </div>
              </div>
            )}
          />
        ) : (
          <div className="flex min-h-0 flex-1 items-center justify-center rounded-xl border border-dashed text-sm text-muted-foreground">
            请先在左侧选择或新增一个平台
          </div>
        )}
        </div>
      </div>

      <PlatformFormSheet
        key={isPlatformFormOpen ? (platformForm?.id ?? "new-platform") : "idle"}
        open={isPlatformFormOpen}
        initial={platformForm}
        onOpenChange={setIsPlatformFormOpen}
        onSubmit={submitPlatform}
      />

      <ApiFormSheet
        key={isApiFormOpen ? (apiForm?.id ?? "new-api") : "idle"}
        open={isApiFormOpen}
        initial={apiForm}
        platformId={selectedId}
        onOpenChange={setIsApiFormOpen}
        onSubmit={submitApi}
      />

      <AlertDialog
        open={platformDeleteTarget !== null}
        onOpenChange={(open) => !open && setPlatformDeleteTarget(null)}
      >
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>
              删除平台「{platformDeleteTarget?.name}」?
            </AlertDialogTitle>
            <AlertDialogDescription>
              删除后该平台及其采集配置将被移除,此操作不可恢复。
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel>取消</AlertDialogCancel>
            <AlertDialogAction
              onClick={confirmDeletePlatform}
              className="bg-destructive text-white hover:bg-destructive/90"
            >
              删除
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>

      <AlertDialog
        open={apiDeleteTarget !== null}
        onOpenChange={(open) => !open && setApiDeleteTarget(null)}
      >
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>
              删除 API「{apiDeleteTarget?.name}」?
            </AlertDialogTitle>
            <AlertDialogDescription>
              删除后该接口配置将从平台移除。
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel>取消</AlertDialogCancel>
            <AlertDialogAction
              onClick={confirmDeleteApi}
              className="bg-destructive text-white hover:bg-destructive/90"
            >
              删除
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
    </div>
  );
}

// 平台 新增 / 编辑 抽屉:名称、访问链接、启用
function PlatformFormSheet({
  open,
  initial,
  onOpenChange,
  onSubmit,
}: {
  open: boolean;
  initial: PlatformConfig | null;
  onOpenChange: (open: boolean) => void;
  onSubmit: (platform: PlatformConfig) => void;
}) {
  const isEdit = initial !== null;
  const [name, setName] = useState(initial?.name ?? "");
  const [code, setCode] = useState(
    (initial?.code as string | undefined) ?? generateCode("PLT"),
  );
  const [loginUrl, setLoginUrl] = useState(initial?.login_url ?? "");
  const [enabled, setEnabled] = useState(initial?.enabled ?? true);
  const [submitted, setSubmitted] = useState(false);

  function handleSubmit(event: FormEvent) {
    event.preventDefault();
    setSubmitted(true);
    if (!name.trim() || !code.trim() || !loginUrl.trim()) {
      return;
    }
    // 编辑保留原有透传字段(collect 等),仅更新名称/编码/链接/启用
    onSubmit({
      ...(initial ?? {}),
      id: initial?.id ?? generatePlatformId(),
      code,
      name: name.trim(),
      login_url: loginUrl.trim(),
      enabled,
    });
  }

  return (
    <Sheet open={open} onOpenChange={onOpenChange}>
      <SheetContent
        className="flex w-full flex-col gap-0 p-0 sm:max-w-md"
        blockClose={
          name !== (initial?.name ?? "") ||
          loginUrl !== (initial?.login_url ?? "") ||
          enabled !== (initial?.enabled ?? true)
        }
      >
        <SheetHeader className="border-b">
          <SheetTitle>{isEdit ? "编辑平台" : "新增平台"}</SheetTitle>
          <SheetDescription>配置平台的基本信息与启用状态。</SheetDescription>
        </SheetHeader>
        <form onSubmit={handleSubmit} className="flex min-h-0 flex-1 flex-col">
          <div className="flex-1 space-y-4 overflow-y-auto p-5">
            <div className="space-y-1.5">
              <Label htmlFor="platform-name">
                平台名称 <span className="text-destructive">*</span>
              </Label>
              <Input
                id="platform-name"
                placeholder="如:抖音 / 小红书 / 微博"
                value={name}
                onChange={(e) => setName(e.target.value)}
                aria-invalid={submitted && !name.trim()}
                autoFocus
              />
              <FieldError
                show={submitted && !name.trim()}
                message="请输入平台名称"
              />
            </div>
            <div className="space-y-1.5">
              <Label htmlFor="platform-code">
                编码 <span className="text-destructive">*</span>
              </Label>
              <CodeField
                id="platform-code"
                value={code}
                onRegenerate={() => setCode(generateCode("PLT"))}
              />
              <p className="text-xs text-muted-foreground">
                系统自动生成,可刷新或复制
              </p>
              <FieldError
                show={submitted && !code.trim()}
                message="编码不可为空"
              />
            </div>
            <div className="space-y-1.5">
              <Label htmlFor="platform-url">
                访问链接 <span className="text-destructive">*</span>
              </Label>
              <Input
                id="platform-url"
                placeholder="平台首页 / 登录页,如 https://www.douyin.com"
                value={loginUrl}
                onChange={(e) => setLoginUrl(e.target.value)}
                aria-invalid={submitted && !loginUrl.trim()}
              />
              <FieldError
                show={submitted && !loginUrl.trim()}
                message="请输入访问链接"
              />
            </div>
            <div className="flex items-center justify-between rounded-lg border border-border p-3">
              <div>
                <div className="text-sm font-medium text-foreground">
                  启用平台
                </div>
                <div className="text-xs text-muted-foreground">
                  停用后该平台不参与采集
                </div>
              </div>
              <Switch checked={enabled} onCheckedChange={setEnabled} />
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

// API 新增 / 编辑 抽屉:名称、url、备注
function ApiFormSheet({
  open,
  initial,
  platformId,
  onOpenChange,
  onSubmit,
}: {
  open: boolean;
  initial: ApiItem | null;
  platformId: string;
  onOpenChange: (open: boolean) => void;
  onSubmit: (item: ApiInput) => void;
}) {
  const isEdit = initial !== null;
  const [name, setName] = useState(initial?.name ?? "");
  const [url, setUrl] = useState(initial?.url ?? "");
  const [remark, setRemark] = useState(initial?.remark ?? "");
  const [submitted, setSubmitted] = useState(false);

  function handleSubmit(event: FormEvent) {
    event.preventDefault();
    setSubmitted(true);
    if (!name.trim() || !url.trim()) {
      return;
    }
    onSubmit({
      id: initial?.id ?? generateApiId(),
      platformId: initial?.platformId ?? platformId,
      name: name.trim(),
      url: url.trim(),
      remark: remark.trim(),
    });
  }

  return (
    <Sheet open={open} onOpenChange={onOpenChange}>
      <SheetContent
        className="flex w-full flex-col gap-0 p-0 sm:max-w-md"
        blockClose={
          name !== (initial?.name ?? "") ||
          url !== (initial?.url ?? "") ||
          remark !== (initial?.remark ?? "")
        }
      >
        <SheetHeader className="border-b">
          <SheetTitle>{isEdit ? "编辑 API" : "新增 API"}</SheetTitle>
          <SheetDescription>配置该平台下的接口信息。</SheetDescription>
        </SheetHeader>
        <form onSubmit={handleSubmit} className="flex min-h-0 flex-1 flex-col">
          <div className="flex-1 space-y-4 overflow-y-auto p-5">
            <div className="space-y-1.5">
              <Label htmlFor="api-name">
                API 名称 <span className="text-destructive">*</span>
              </Label>
              <Input
                id="api-name"
                placeholder="如:搜索接口 / 详情接口"
                value={name}
                onChange={(e) => setName(e.target.value)}
                aria-invalid={submitted && !name.trim()}
                autoFocus
              />
              <FieldError
                show={submitted && !name.trim()}
                message="请输入 API 名称"
              />
            </div>
            <div className="space-y-1.5">
              <Label htmlFor="api-url">
                URL <span className="text-destructive">*</span>
              </Label>
              <Input
                id="api-url"
                placeholder="接口地址,如 https://api.xxx.com/search"
                value={url}
                onChange={(e) => setUrl(e.target.value)}
                aria-invalid={submitted && !url.trim()}
              />
              <FieldError show={submitted && !url.trim()} message="请输入 URL" />
            </div>
            <div className="space-y-1.5">
              <Label htmlFor="api-remark">备注</Label>
              <Input
                id="api-remark"
                placeholder="接口用途说明,选填"
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
