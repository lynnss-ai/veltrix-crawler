import {
  useCallback,
  useEffect,
  useMemo,
  useState,
  type FormEvent,
} from "react";
import { type ColumnDef, type FilterFn } from "@tanstack/react-table";
import { listen } from "@tauri-apps/api/event";
import {
  ChevronLeft,
  Filter,
  LogIn,
  MoreVertical,
  Plus,
  Rocket,
  Search,
  SquarePen,
  Trash2,
  UserX,
  XCircle,
} from "lucide-react";
import { toast } from "sonner";
import { useResponsiveCollapse } from "@/hooks/use-responsive-collapse";
import { SimpleTooltip } from "@/components/SimpleTooltip";
import {
  api,
  type PublishAccountView,
  type PublishCustomerView,
  type PublishPlatformView,
} from "@/lib/api";
import { cn, formatTimestamp } from "@/lib/utils";
import { platformClass, platformLabel } from "@/lib/platforms";
import { ErrorBanner } from "@/components/ErrorBanner";
import { DataTable } from "@/components/DataTable";
import { PageLoading } from "@/components/PageLoading";
import { FieldError } from "@/components/FieldError";
import { FORM_CONTROL_SIZING } from "@/lib/form-sizing";
import { DataTableColumnHeader } from "@/components/DataTableColumnHeader";
import { DataTableFacetedFilter } from "@/components/DataTableFacetedFilter";
import { StatusBadge, type StatusTone } from "@/components/StatusBadge";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
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

// 发布账号状态 -> 中文标签 + 语义色(active 绿 / invalid 灰 / limited 黄 / disabled 红)
const PUBLISH_STATUS_META: Record<string, { label: string; tone: StatusTone }> =
  {
    active: { label: "正常", tone: "success" },
    invalid: { label: "失效", tone: "neutral" },
    limited: { label: "受限", tone: "warning" },
    disabled: { label: "停用", tone: "danger" },
  };

function statusMetaOf(status: string): { label: string; tone: StatusTone } {
  return PUBLISH_STATUS_META[status] ?? { label: status, tone: "neutral" };
}

// 账号全局搜索:匹配名称 / 昵称
const accountFilterFn: FilterFn<PublishAccountView> = (
  row,
  _columnId,
  value,
) => {
  const q = String(value).toLowerCase();
  return (
    row.original.label.toLowerCase().includes(q) ||
    row.original.nickname.toLowerCase().includes(q)
  );
};

// 客户是发布账号的必选归属:侧栏只列 CRM 客户,不设「全部 / 未分类」默认项。
// 「未关联客户」为兜底分组(选中态的伪 id):旧数据 / 客户被删导致的悬空账号在此可见,便于编辑后重新归属。
const ORPHAN_KEY = "__orphan__";

// 客户账号(主从):左侧 CRM 客户列表(数据采集 > 客户管理维护,此处只读)、右侧该客户的发布账号。
export function PublishAccountsPage() {
  const [sbCollapsed, setSbCollapsed] = useResponsiveCollapse();
  const [platforms, setPlatforms] = useState<PublishPlatformView[]>([]);
  const [customers, setCustomers] = useState<PublishCustomerView[]>([]);
  // 当前选中的客户 id;"" = 暂无客户可选;ORPHAN_KEY = 未关联客户兜底分组
  const [selectedCustomer, setSelectedCustomer] = useState("");
  const [accounts, setAccounts] = useState<PublishAccountView[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);

  const [editingAccount, setEditingAccount] = useState<PublishAccountView | null>(null);
  const [isAccountFormOpen, setIsAccountFormOpen] = useState(false);
  const [deleteAccountTarget, setDeleteAccountTarget] =
    useState<PublishAccountView | null>(null);

  const loadCustomers = useCallback(async () => {
    try {
      const list = await api.listPublishCustomers();
      setCustomers(list);
      // 选中项失效(未选 / 客户被删)时落到第一个客户;一个客户都没有则保持 ""
      //(ORPHAN_KEY 选中态不纠偏:未关联分组不受客户增删影响)
      setSelectedCustomer((prev) =>
        prev === ORPHAN_KEY || list.some((c) => c.id === prev)
          ? prev
          : (list[0]?.id ?? ""),
      );
    } catch (e) {
      setError(String(e));
    }
  }, []);

  // 一次性取全部账号,客户过滤在前端做(选中哪个客户就过滤哪个)
  const loadAccounts = useCallback(async () => {
    try {
      setAccounts(await api.listPublishAccounts(null));
    } catch (e) {
      setError(String(e));
    }
  }, []);

  useEffect(() => {
    Promise.all([
      api.listPublishPlatforms().then(setPlatforms),
      loadCustomers(),
      loadAccounts(),
    ]).finally(() => setLoading(false));
  }, [loadCustomers, loadAccounts]);

  // 登录窗口内登录成功 / 状态变化:后端推送事件,刷新账号列表与客户计数
  useEffect(() => {
    const unlisten = listen("publish-account-updated", () => {
      loadAccounts();
      loadCustomers();
    });
    return () => {
      unlisten.then((dispose) => dispose());
    };
  }, [loadAccounts, loadCustomers]);

  // 平台显示名:优先查发布平台清单(如「视频号助手」),清单没有(旧数据 / 未启用)回退标准名
  const platformName = useCallback(
    (id: string) =>
      platforms.find((p) => p.id === id)?.name ?? platformLabel(id),
    [platforms],
  );

  // 悬空账号:categoryId 不在客户列表中(旧分类数据 / 客户被删),进「未关联客户」兜底分组
  const orphanAccounts = useMemo(
    () => accounts.filter((a) => !customers.some((c) => c.id === a.categoryId)),
    [accounts, customers],
  );

  const visibleAccounts = useMemo(
    () =>
      selectedCustomer === ORPHAN_KEY
        ? orphanAccounts
        : accounts.filter((a) => a.categoryId === selectedCustomer),
    [accounts, orphanAccounts, selectedCustomer],
  );

  async function handleLogin(account: PublishAccountView) {
    try {
      await api.openPublishAccountLogin(account.id);
    } catch (e) {
      setError(String(e));
    }
  }

  async function handleCloseWindow(account: PublishAccountView) {
    try {
      await api.closePublishAccountWindow(account.id);
    } catch (e) {
      setError(String(e));
    }
  }

  async function confirmDeleteAccount() {
    if (!deleteAccountTarget) return;
    try {
      await api.deletePublishAccount(deleteAccountTarget.id);
      await Promise.all([loadAccounts(), loadCustomers()]);
      toast.success("已删除账号");
    } catch (e) {
      setError(String(e));
    }
    setDeleteAccountTarget(null);
  }

  const columns = useMemo<ColumnDef<PublishAccountView>[]>(
    () => [
      {
        accessorKey: "platform",
        header: "平台",
        enableSorting: false,
        cell: ({ row }) => (
          <Badge
            variant="outline"
            className={cn(
              "border-transparent",
              platformClass(row.original.platform),
            )}
          >
            {platformName(row.original.platform)}
          </Badge>
        ),
      },
      {
        accessorKey: "label",
        header: ({ column }) => (
          <DataTableColumnHeader column={column} title="账号" />
        ),
        cell: ({ row }) => {
          const a = row.original;
          return (
            <div className="flex min-w-0 flex-col">
              <span className="truncate font-medium text-foreground">
                {a.label || "未命名账号"}
              </span>
              {a.nickname && (
                <span className="truncate text-xs text-muted-foreground">
                  {a.nickname}
                </span>
              )}
            </div>
          );
        },
      },
      {
        accessorKey: "code",
        header: "编码",
        enableSorting: false,
        cell: ({ row }) => (
          <span className="font-mono text-xs text-muted-foreground">
            {row.original.code || "—"}
          </span>
        ),
      },
      {
        accessorKey: "status",
        header: "状态",
        enableSorting: false,
        filterFn: (row, id, value) =>
          (value as string[]).includes(row.getValue(id) as string),
        cell: ({ row }) => {
          const a = row.original;
          const meta = statusMetaOf(a.status);
          const badge = <StatusBadge tone={meta.tone}>{meta.label}</StatusBadge>;
          // 失效 / 受限时悬浮显示后端给的原因
          return a.failReason ? (
            <SimpleTooltip content={a.failReason}>{badge}</SimpleTooltip>
          ) : (
            badge
          );
        },
      },
      {
        accessorKey: "todayPublished",
        header: ({ column }) => (
          <DataTableColumnHeader column={column} title="今日已发" />
        ),
        cell: ({ row }) => (
          <span className="text-muted-foreground">
            {row.original.todayPublished}
          </span>
        ),
      },
      {
        accessorKey: "lastLoginAt",
        header: ({ column }) => (
          <DataTableColumnHeader column={column} title="最近登录" />
        ),
        cell: ({ row }) => (
          <span className="text-muted-foreground">
            {formatTimestamp(row.original.lastLoginAt)}
          </span>
        ),
      },
      {
        id: "actions",
        header: () => <div className="text-right">操作</div>,
        enableSorting: false,
        cell: ({ row }) => {
          const a = row.original;
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
                <DropdownMenuContent align="end" className="w-36">
                  <DropdownMenuItem onClick={() => handleLogin(a)}>
                    <LogIn />
                    登录
                  </DropdownMenuItem>
                  <DropdownMenuItem onClick={() => handleCloseWindow(a)}>
                    <XCircle />
                    关闭窗口
                  </DropdownMenuItem>
                  <DropdownMenuItem
                    onClick={() => {
                      setEditingAccount(a);
                      setIsAccountFormOpen(true);
                    }}
                  >
                    <SquarePen />
                    编辑
                  </DropdownMenuItem>
                  <DropdownMenuSeparator />
                  <DropdownMenuItem
                    className="text-destructive focus:text-destructive"
                    onClick={() => setDeleteAccountTarget(a)}
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
    // platformName 依赖 platforms,setter 稳定;platforms 变化后由数据刷新重渲染
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [platformName],
  );

  const statusOptions = Object.entries(PUBLISH_STATUS_META).map(
    ([value, meta]) => ({ label: meta.label, value }),
  );

  const selectedCustomerName =
    selectedCustomer === ORPHAN_KEY
      ? "未关联客户"
      : (customers.find((c) => c.id === selectedCustomer)?.name ?? "");

  if (
    loading &&
    platforms.length === 0 &&
    customers.length === 0 &&
    accounts.length === 0
  ) {
    return <PageLoading />;
  }

  return (
    <div
      className={`flex min-h-0 flex-1 flex-col gap-4 ${FORM_CONTROL_SIZING}`}
    >
      <ErrorBanner message={error} onClose={() => setError(null)} />

      <div className="flex min-h-0 flex-1 gap-4">
        {/* 左侧:客户(只读,来自数据采集 > 客户管理;可收起,窄屏自动收起) */}
        {!sbCollapsed && (
          <div className="flex w-56 shrink-0 flex-col overflow-hidden rounded-xl border bg-card lg:w-64">
            <div className="flex h-10 items-center justify-between border-b px-4">
              <span className="text-sm font-semibold">客户</span>
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
            <div className="flex-1 space-y-0.5 overflow-auto p-2">
              {customers.length === 0 && (
                <p className="px-2 py-8 text-center text-xs text-muted-foreground">
                  暂无客户,请先在 数据采集 &gt; 客户管理 中新增客户
                </p>
              )}
              {customers.map((c) => (
                // 客户条目:名称 + 编码 + 账号数(右对齐);客户的增删改在数据采集 > 客户管理,此处只读
                <div
                  key={c.id}
                  onClick={() => setSelectedCustomer(c.id)}
                  className={cn(
                    "flex w-full cursor-pointer items-center gap-2 rounded-md px-2 py-2 text-sm transition-colors",
                    c.id === selectedCustomer
                      ? "bg-accent font-medium text-accent-foreground"
                      : "hover:bg-accent/50",
                  )}
                >
                  <span className="flex min-w-0 flex-1 flex-col">
                    <span className="truncate">{c.name}</span>
                    <span className="truncate font-mono text-xs text-muted-foreground">
                      {c.code}
                    </span>
                  </span>
                  <span className="text-xs text-muted-foreground">
                    {c.accountCount}
                  </span>
                </div>
              ))}
              {/* 兜底分组:悬空账号(旧分类数据 / 客户被删)在此可见,编辑账号可重新归属 */}
              {orphanAccounts.length > 0 && (
                <div
                  onClick={() => setSelectedCustomer(ORPHAN_KEY)}
                  className={cn(
                    "flex w-full cursor-pointer items-center gap-2 rounded-md px-2 py-2 text-sm transition-colors",
                    selectedCustomer === ORPHAN_KEY
                      ? "bg-accent font-medium text-accent-foreground"
                      : "hover:bg-accent/50",
                  )}
                >
                  <UserX className="size-4 shrink-0 text-muted-foreground" />
                  <span className="flex-1 truncate text-muted-foreground">
                    未关联客户
                  </span>
                  <span className="text-xs text-muted-foreground">
                    {orphanAccounts.length}
                  </span>
                </div>
              )}
            </div>
          </div>
        )}

        {/* 右侧:账号数据表 */}
        <div className="flex min-w-0 flex-1 flex-col gap-2">
          <DataTable
            columns={columns}
            data={visibleAccounts}
            loading={loading}
            itemLabel="账号"
            globalFilterFn={accountFilterFn}
            getRowId={(a) => a.id}
            emptyState={
              <div className="flex flex-col items-center justify-center gap-2 py-12 text-center">
                <div className="flex size-12 items-center justify-center rounded-full bg-muted text-muted-foreground">
                  <Rocket className="size-6" />
                </div>
                {customers.length === 0 &&
                selectedCustomer !== ORPHAN_KEY ? (
                  <>
                    <p className="text-sm font-medium text-foreground">
                      还没有客户
                    </p>
                    <p className="text-xs text-muted-foreground">
                      发布账号必须归属客户,请先在 数据采集 &gt; 客户管理 中新增客户
                    </p>
                  </>
                ) : (
                  <>
                    <p className="text-sm font-medium text-foreground">
                      「{selectedCustomerName}」暂无发布账号
                    </p>
                    <p className="text-xs text-muted-foreground">
                      点击右上角「新增账号」,创建后将打开平台登录窗口
                    </p>
                  </>
                )}
              </div>
            }
            renderToolbar={(table) => (
              <div className="flex flex-col gap-3 sm:flex-row sm:items-center sm:justify-between">
                <div className="flex flex-1 items-center gap-2">
                  {sbCollapsed && (
                    <SimpleTooltip content="展开客户筛选">
                      <Button
                        variant="outline"
                        className="h-10 cursor-pointer"
                        onClick={() => setSbCollapsed(false)}
                      >
                        <Filter />
                        客户
                      </Button>
                    </SimpleTooltip>
                  )}
                  <div className="relative w-full sm:max-w-sm">
                    <Search className="pointer-events-none absolute left-3 top-1/2 size-4 -translate-y-1/2 text-muted-foreground" />
                    <Input
                      placeholder="搜索账号名称 / 昵称"
                      className="pl-9"
                      value={(table.getState().globalFilter as string) ?? ""}
                      onChange={(e) => table.setGlobalFilter(e.target.value)}
                    />
                  </div>
                  <DataTableFacetedFilter
                    column={table.getColumn("status")}
                    title="状态"
                    options={statusOptions}
                  />
                </div>
                <div className="flex items-center gap-2">
                  <Button
                    className="h-10"
                    onClick={() => {
                      // 账号必须归属客户:一个客户都没有时引导去客户管理
                      if (customers.length === 0) {
                        toast.info("请先在 数据采集 > 客户管理 中新增客户");
                        return;
                      }
                      setEditingAccount(null);
                      setIsAccountFormOpen(true);
                    }}
                  >
                    <Plus />
                    新增账号
                  </Button>
                </div>
              </div>
            )}
          />
        </div>
      </div>

      <AccountFormDialog
        key={isAccountFormOpen ? (editingAccount?.id ?? "new-account") : "idle"}
        open={isAccountFormOpen}
        initial={editingAccount}
        platforms={platforms}
        customers={customers}
        defaultCustomerId={
          selectedCustomer && selectedCustomer !== ORPHAN_KEY
            ? selectedCustomer
            : null
        }
        onOpenChange={setIsAccountFormOpen}
        onSubmitted={() => {
          setIsAccountFormOpen(false);
          loadAccounts();
          loadCustomers();
        }}
        onError={setError}
      />

      {/* 删除账号确认 */}
      <AlertDialog
        open={deleteAccountTarget !== null}
        onOpenChange={(open) => !open && setDeleteAccountTarget(null)}
      >
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>
              删除账号「
              {deleteAccountTarget?.label || deleteAccountTarget?.id}」?
            </AlertDialogTitle>
            <AlertDialogDescription>
              删除后该账号将从发布账号池移除,其登录窗口与登录态也会关闭。
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel>取消</AlertDialogCancel>
            <AlertDialogAction
              onClick={confirmDeleteAccount}
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

// 账号 新增 / 编辑 对话框。新增选平台 + 客户 + 名称;编辑仅改客户与名称。客户必选。
function AccountFormDialog({
  open,
  initial,
  platforms,
  customers,
  defaultCustomerId,
  onOpenChange,
  onSubmitted,
  onError,
}: {
  open: boolean;
  initial: PublishAccountView | null;
  platforms: PublishPlatformView[];
  customers: PublishCustomerView[];
  defaultCustomerId: string | null;
  onOpenChange: (open: boolean) => void;
  onSubmitted: () => void;
  onError: (message: string) => void;
}) {
  const isEdit = initial !== null;
  const enabledPlatforms = platforms.filter((p) => p.enabled);
  const [platform, setPlatform] = useState(
    initial?.platform ?? enabledPlatforms[0]?.id ?? "",
  );
  const [categoryId, setCategoryId] = useState(
    initial?.categoryId ?? defaultCustomerId,
  );
  const [label, setLabel] = useState(initial?.label ?? "");
  const [submitted, setSubmitted] = useState(false);
  const [saving, setSaving] = useState(false);

  async function handleSubmit(event: FormEvent) {
    event.preventDefault();
    setSubmitted(true);
    // 客户必选:账号必须归属真实客户
    if (!label.trim() || !categoryId || (!isEdit && !platform)) return;
    setSaving(true);
    try {
      if (isEdit) {
        await api.updatePublishAccount(initial.id, categoryId, label.trim());
      } else {
        // 创建后立即打开平台登录窗口,引导用户扫码 / 登录
        const created = await api.createPublishAccount({
          platform,
          categoryId,
          label: label.trim(),
        });
        api
          .openPublishAccountLogin(created.id)
          .catch((e) => console.warn("打开登录窗口失败:", e));
        toast.success("已创建账号,请在打开的窗口中登录");
      }
      onSubmitted();
    } catch (e) {
      onError(String(e));
    } finally {
      setSaving(false);
    }
  }

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="sm:max-w-md">
        <DialogHeader>
          <DialogTitle>{isEdit ? "编辑账号" : "新增账号"}</DialogTitle>
          <DialogDescription>
            {isEdit
              ? "修改账号名称或所属客户。"
              : "创建后将自动打开平台登录窗口,扫码 / 登录完成授权。"}
          </DialogDescription>
        </DialogHeader>
        <form onSubmit={handleSubmit} className="space-y-4">
          {!isEdit && (
            <div className="space-y-1.5">
              <Label htmlFor="publish-account-platform">
                平台 <span className="text-destructive">*</span>
              </Label>
              <Select value={platform} onValueChange={setPlatform}>
                <SelectTrigger id="publish-account-platform" className="w-full">
                  <SelectValue placeholder="请选择平台" />
                </SelectTrigger>
                <SelectContent>
                  {enabledPlatforms.length === 0 ? (
                    <div className="px-2 py-1.5 text-xs text-muted-foreground">
                      暂无启用平台
                    </div>
                  ) : (
                    enabledPlatforms.map((p) => (
                      <SelectItem key={p.id} value={p.id}>
                        {p.name}
                      </SelectItem>
                    ))
                  )}
                </SelectContent>
              </Select>
              <FieldError
                show={submitted && !platform}
                message="请选择平台"
              />
            </div>
          )}
          <div className="space-y-1.5">
            <Label htmlFor="publish-account-customer">
              所属客户 <span className="text-destructive">*</span>
            </Label>
            <Select
              value={categoryId ?? ""}
              onValueChange={(v) => setCategoryId(v)}
            >
              <SelectTrigger id="publish-account-customer" className="w-full">
                <SelectValue placeholder="请选择客户" />
              </SelectTrigger>
              <SelectContent>
                {customers.map((c) => (
                  <SelectItem key={c.id} value={c.id}>
                    {c.name}({c.code})
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
            <FieldError
              show={submitted && !categoryId}
              message="请选择客户"
            />
          </div>
          <div className="space-y-1.5">
            <Label htmlFor="publish-account-label">
              名称 <span className="text-destructive">*</span>
            </Label>
            <Input
              id="publish-account-label"
              placeholder="用于在列表中识别,如「主号」「小号-1」"
              value={label}
              onChange={(e) => setLabel(e.target.value)}
              aria-invalid={submitted && !label.trim()}
              autoFocus
            />
            <FieldError
              show={submitted && !label.trim()}
              message="账号名称不可为空"
            />
          </div>
          <DialogFooter>
            <Button
              type="button"
              variant="outline"
              onClick={() => onOpenChange(false)}
            >
              取消
            </Button>
            <Button type="submit" disabled={saving}>
              {isEdit ? "保存" : "创建并登录"}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}
