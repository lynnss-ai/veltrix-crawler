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
  ExternalLink,
  Filter,
  LogIn,
  LogOut,
  MoreVertical,
  Network,
  SquarePen,
  Plus,
  Search,
  Trash2,
} from "lucide-react";
import { useResponsiveCollapse } from "@/hooks/use-responsive-collapse";
import { SimpleTooltip } from "@/components/SimpleTooltip";
import {
  api,
  type AccountInput,
  type AccountView,
  type PlatformConfig,
} from "@/lib/api";
import { formatTimestamp } from "@/lib/utils";
import { ErrorBanner } from "@/components/ErrorBanner";
import { DataTable } from "@/components/DataTable";
import { FieldError } from "@/components/FieldError";
import { CodeField, generateCode } from "@/components/CodeField";
import { FORM_CONTROL_SIZING } from "@/lib/form-sizing";
import { DataTableColumnHeader } from "@/components/DataTableColumnHeader";
import { DataTableFacetedFilter } from "@/components/DataTableFacetedFilter";
import { StatusBadge, type StatusTone } from "@/components/StatusBadge";
import { PlatformManagerSheet } from "@/components/platform-manager-sheet";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
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

// 账号状态 -> 中文标签 + 语义色
const ACCOUNT_STATUS_META: Record<string, { label: string; tone: StatusTone }> =
  {
    active: { label: "正常", tone: "success" },
    cooldown: { label: "冷却中", tone: "warning" },
    invalid: { label: "失效", tone: "danger" },
    disabled: { label: "停用", tone: "neutral" },
  };

function statusMetaOf(status: string): { label: string; tone: StatusTone } {
  return ACCOUNT_STATUS_META[status] ?? { label: status, tone: "neutral" };
}

// 账号全局搜索:匹配名称
const accountFilterFn: FilterFn<AccountView> = (row, _columnId, value) =>
  row.original.label.toLowerCase().includes(String(value).toLowerCase());

// 账号 ID 由系统生成(用户无需关心),时间戳 + 随机后缀保证唯一
function generateAccountId(): string {
  return `acc-${Date.now().toString(36)}${Math.random().toString(36).slice(2, 6)}`;
}

// 账号管理(主从):左侧平台、右侧该平台账号。接真实后端 API。
export function AccountsPage({ currentUser }: { currentUser: string }) {
  const [sbCollapsed, setSbCollapsed] = useResponsiveCollapse();
  const [platforms, setPlatforms] = useState<PlatformConfig[]>([]);
  const [selectedPlatform, setSelectedPlatform] = useState("");
  const [accounts, setAccounts] = useState<AccountView[]>([]);
  const [accountCounts, setAccountCounts] = useState<Record<string, number>>(
    {},
  );
  const [error, setError] = useState<string | null>(null);

  const [editing, setEditing] = useState<AccountView | null>(null);
  const [isFormOpen, setIsFormOpen] = useState(false);
  const [deleteTarget, setDeleteTarget] = useState<AccountView | null>(null);
  // 待确认清空登录状态的账号(null=未弹确认框)
  const [clearLoginTarget, setClearLoginTarget] = useState<AccountView | null>(
    null,
  );
  const [clearingLogin, setClearingLogin] = useState(false);
  const [managerOpen, setManagerOpen] = useState(false);

  const loadAccounts = useCallback(async (platform: string) => {
    if (!platform) return;
    try {
      const list = await api.listAccounts(platform);
      setAccounts(list);
      setAccountCounts((prev) => ({ ...prev, [platform]: list.length }));
    } catch (e) {
      setError(String(e));
    }
  }, []);

  useEffect(() => {
    api
      .listPlatforms()
      .then(async (list) => {
        setPlatforms(list);
        setSelectedPlatform((prev) => prev || list[0]?.id || "");
        // 并行统计各平台账号数,用于左侧列表展示
        const entries = await Promise.all(
          list.map((p) =>
            api
              .listAccounts(p.id)
              .then((a) => [p.id, a.length] as const)
              .catch(() => [p.id, 0] as const),
          ),
        );
        setAccountCounts(Object.fromEntries(entries));
      })
      .catch((e) => setError(String(e)));
  }, []);

  useEffect(() => {
    loadAccounts(selectedPlatform);
  }, [selectedPlatform, loadAccounts]);

  // 登录窗口关闭、账号转 active 后后端推送事件:刷新对应平台账号列表(免手动点刷新)
  useEffect(() => {
    const unlisten = listen<string>("account-login-updated", (event) => {
      if (event.payload === selectedPlatform) {
        loadAccounts(selectedPlatform);
      }
    });
    return () => {
      unlisten.then((dispose) => dispose());
    };
  }, [selectedPlatform, loadAccounts]);

  const selected = platforms.find((p) => p.id === selectedPlatform) ?? null;

  async function submitAccount(input: AccountInput) {
    try {
      await api.upsertAccount(input);
      setIsFormOpen(false);
      await loadAccounts(input.platform);
    } catch (e) {
      setError(String(e));
    }
  }

  async function confirmRemove() {
    if (!deleteTarget) return;
    try {
      await api.removeAccount(deleteTarget.platform, deleteTarget.id);
      await loadAccounts(deleteTarget.platform);
    } catch (e) {
      setError(String(e));
    }
    setDeleteTarget(null);
  }

  async function handleLogin(account: AccountView) {
    try {
      await api.openLoginWindow(
        account.platform,
        account.id,
        account.label || account.id,
      );
    } catch (e) {
      setError(String(e));
    }
  }

  // 清空登录状态(确认后执行):删除该账号 WebView 登录数据并置失效,刷新列表
  async function confirmClearLogin() {
    if (!clearLoginTarget) return;
    setClearingLogin(true);
    try {
      await api.clearAccountLogin(
        clearLoginTarget.platform,
        clearLoginTarget.id,
      );
      await loadAccounts(clearLoginTarget.platform);
      setClearLoginTarget(null);
    } catch (e) {
      setError(String(e));
    } finally {
      setClearingLogin(false);
    }
  }

  // 归属隔离已由后端按 dataScope 服务端过滤,这里直接用返回结果
  const columns = useMemo<ColumnDef<AccountView>[]>(
    () => [
      {
        accessorKey: "label",
        header: ({ column }) => (
          <DataTableColumnHeader column={column} title="账号名称" />
        ),
        cell: ({ row }) => (
          <span className="font-medium text-foreground">
            {row.original.label || "未命名账号"}
          </span>
        ),
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
          // 登录态:active=已登录(可访问平台);invalid=登录失效(需重新扫码登录);
          // 两者都打开该账号专属 WebView(带各自登录态),只是文案与图标不同。
          if (a.status === "invalid") {
            return (
              <Button
                size="sm"
                variant="outline"
                className="h-6 w-24 rounded-full text-xs border-destructive/40 text-destructive hover:bg-destructive/10 hover:text-destructive"
                onClick={() => handleLogin(a)}
              >
                <LogIn className="size-3.5" />
                去登录
              </Button>
            );
          }
          if (a.status === "active") {
            return (
              <Button
                size="sm"
                variant="outline"
                className="h-6 w-24 rounded-full text-xs border-emerald-500/40 text-emerald-600 hover:bg-emerald-500/10 hover:text-emerald-600 dark:text-emerald-400"
                onClick={() => handleLogin(a)}
              >
                <ExternalLink className="size-3.5" />
                访问平台
              </Button>
            );
          }
          // 冷却中 / 停用:保留状态徽章
          const meta = statusMetaOf(a.status);
          return <StatusBadge tone={meta.tone}>{meta.label}</StatusBadge>;
        },
      },
      {
        accessorKey: "risk_count",
        header: ({ column }) => (
          <DataTableColumnHeader column={column} title="风控次数" />
        ),
        cell: ({ row }) => (
          <span className="text-muted-foreground">
            {row.original.risk_count}
          </span>
        ),
      },
      {
        accessorKey: "last_used_at",
        header: ({ column }) => (
          <DataTableColumnHeader column={column} title="最近使用" />
        ),
        cell: ({ row }) => (
          <span className="text-muted-foreground">
            {formatTimestamp(row.original.last_used_at)}
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
                  <DropdownMenuItem
                    onClick={() => {
                      setEditing(a);
                      setIsFormOpen(true);
                    }}
                  >
                    <SquarePen />
                    编辑
                  </DropdownMenuItem>
                  <DropdownMenuItem onClick={() => setClearLoginTarget(a)}>
                    <LogOut />
                    清空登录状态
                  </DropdownMenuItem>
                  <DropdownMenuSeparator />
                  <DropdownMenuItem
                    className="text-destructive focus:text-destructive"
                    onClick={() => setDeleteTarget(a)}
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
    // handleLogin 用 row 内的 platform,setter 稳定,故依赖留空
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [],
  );

  const statusOptions = Object.entries(ACCOUNT_STATUS_META).map(
    ([value, meta]) => ({ label: meta.label, value }),
  );

  return (
    <div
      className={`flex min-h-0 flex-1 flex-col gap-4 ${FORM_CONTROL_SIZING}`}
    >
      <ErrorBanner message={error} onClose={() => setError(null)} />

      <div className="flex min-h-0 flex-1 gap-4">
        {/* 左侧:平台(可收起,窄屏自动收起) */}
        {!sbCollapsed && (
        <div className="flex w-56 shrink-0 flex-col overflow-hidden rounded-xl border bg-card lg:w-64">
          <div className="flex h-10 items-center justify-between border-b px-4">
            <span className="text-sm font-semibold">平台</span>
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
            {platforms.length === 0 && (
              <p className="px-2 py-6 text-center text-xs text-muted-foreground">
                暂无平台,请先到「平台管理」配置
              </p>
            )}
            {platforms.map((p) => {
              const isActive = p.id === selectedPlatform;
              return (
                <button
                  key={p.id}
                  onClick={() => setSelectedPlatform(p.id)}
                  className={`flex w-full items-center gap-2 rounded-md px-2 py-2 text-left text-sm transition-colors ${
                    isActive
                      ? "bg-accent font-medium text-accent-foreground"
                      : "hover:bg-accent/50"
                  }`}
                >
                  <span className="flex-1 truncate">{p.name}</span>
                  {!p.enabled && (
                    <span className="text-xs text-muted-foreground">停用</span>
                  )}
                  <span className="text-xs text-muted-foreground">
                    {accountCounts[p.id] ?? "·"}
                  </span>
                </button>
              );
            })}
          </div>
        </div>
        )}

        {/* 右侧:数据表(展开按钮在 toolbar 内,与搜索框对齐)/ 占位 */}
        <div className="flex min-w-0 flex-1 flex-col gap-2">
          {sbCollapsed && !selected && (
            <div>
              <SimpleTooltip content="展开平台筛选">
                <Button
                  variant="outline"
                  className="h-10 cursor-pointer"
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
            data={accounts}
            itemLabel="账号"
            globalFilterFn={accountFilterFn}
            getRowId={(a) => a.id}
            emptyState={
              <div className="flex flex-col items-center justify-center gap-2 py-12 text-center">
                <div className="flex size-12 items-center justify-center rounded-full bg-muted text-muted-foreground">
                  <Network className="size-6" />
                </div>
                <p className="text-sm font-medium text-foreground">
                  「{selected.name}」暂无账号
                </p>
                <p className="text-xs text-muted-foreground">
                  点击右上角「新增账号」,或添加后用「登录」扫码
                </p>
              </div>
            }
            renderToolbar={(table) => (
              <div className="flex flex-col gap-3 sm:flex-row sm:items-center sm:justify-between">
                <div className="flex flex-1 items-center gap-2">
                  {sbCollapsed && (
                    <SimpleTooltip content="展开平台筛选">
                      <Button
                        variant="outline"
                        className="h-10 cursor-pointer"
                        onClick={() => setSbCollapsed(false)}
                      >
                        <Filter />
                        平台
                      </Button>
                    </SimpleTooltip>
                  )}
                  <div className="relative w-full sm:max-w-sm">
                    <Search className="pointer-events-none absolute left-3 top-1/2 size-4 -translate-y-1/2 text-muted-foreground" />
                    <Input
                      placeholder="搜索账号名称"
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
                    variant="outline"
                    className="h-10"
                    onClick={() => setManagerOpen(true)}
                  >
                    <Network />
                    管理平台
                  </Button>
                  <Button
                    className="h-10"
                    onClick={() => {
                      setEditing(null);
                      setIsFormOpen(true);
                    }}
                  >
                    <Plus />
                    新增账号
                  </Button>
                </div>
              </div>
            )}
          />
        ) : (
          <div className="flex min-h-0 flex-1 items-center justify-center rounded-xl border border-dashed text-sm text-muted-foreground">
            请先在左侧选择一个平台
          </div>
        )}
        </div>
      </div>

      <AccountFormSheet
        key={isFormOpen ? (editing?.id ?? "new-account") : "idle"}
        open={isFormOpen}
        initial={editing}
        platform={selectedPlatform}
        currentUser={currentUser}
        onOpenChange={setIsFormOpen}
        onSubmit={submitAccount}
      />

      <AlertDialog
        open={deleteTarget !== null}
        onOpenChange={(open) => !open && setDeleteTarget(null)}
      >
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>
              删除账号「{deleteTarget?.label || deleteTarget?.id}」?
            </AlertDialogTitle>
            <AlertDialogDescription>
              删除后该账号将从账号池移除,其 Cookie 也会失效。
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel>取消</AlertDialogCancel>
            <AlertDialogAction
              onClick={confirmRemove}
              className="bg-destructive text-white hover:bg-destructive/90"
            >
              删除
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>

      {/* 清空登录状态确认 */}
      <AlertDialog
        open={clearLoginTarget !== null}
        onOpenChange={(open) => !open && setClearLoginTarget(null)}
      >
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>
              清空「{clearLoginTarget?.label || clearLoginTarget?.id}」的登录状态?
            </AlertDialogTitle>
            <AlertDialogDescription>
              将关闭该账号窗口并删除其登录数据(Cookie /
              登录态),账号状态置为失效,需重新登录。账号记录与备注保留。
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel disabled={clearingLogin}>
              取消
            </AlertDialogCancel>
            <AlertDialogAction
              disabled={clearingLogin}
              onClick={(e) => {
                e.preventDefault();
                void confirmClearLogin();
              }}
              className="bg-destructive text-white hover:bg-destructive/90"
            >
              清空登录状态
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>

      <PlatformManagerSheet
        open={managerOpen}
        onOpenChange={setManagerOpen}
        onChanged={() => {
          // 平台增删改后刷新左侧平台列表
          api
            .listPlatforms()
            .then(setPlatforms)
            .catch((e) => console.warn("刷新平台列表失败:", e));
        }}
      />
    </div>
  );
}

// 账号 新增 / 编辑 抽屉。新增可填 ID,编辑时 ID 只读。
function AccountFormSheet({
  open,
  initial,
  platform,
  currentUser,
  onOpenChange,
  onSubmit,
}: {
  open: boolean;
  initial: AccountView | null;
  platform: string;
  currentUser: string;
  onOpenChange: (open: boolean) => void;
  onSubmit: (input: AccountInput) => void;
}) {
  const isEdit = initial !== null;
  const [label, setLabel] = useState(initial?.label ?? "");
  const [code, setCode] = useState(initial?.code ?? generateCode("ACC"));
  const [remark, setRemark] = useState(initial?.remark ?? "");
  const [submitted, setSubmitted] = useState(false);
  // 归属用户:新建关联当前用户,编辑保留原归属
  const owner = initial?.owner ?? currentUser;

  function handleSubmit(event: FormEvent) {
    event.preventDefault();
    setSubmitted(true);
    if (!label.trim()) {
      return;
    }
    // ID 自动生成、Cookie 通过登录扫码获取,均不在表单维护
    onSubmit({
      id: initial?.id ?? generateAccountId(),
      platform: initial?.platform ?? platform,
      label: label.trim(),
      cookie: initial?.cookie ?? "",
      code,
      remark: remark.trim(),
      owner,
    });
  }

  return (
    <Sheet open={open} onOpenChange={onOpenChange}>
      <SheetContent
        className="flex w-full flex-col gap-0 p-0 sm:max-w-[600px]"
        blockClose={
          label !== (initial?.label ?? "") ||
          remark !== (initial?.remark ?? "")
        }
      >
        <SheetHeader className="border-b">
          <SheetTitle>{isEdit ? "编辑账号" : "新增账号"}</SheetTitle>
          <SheetDescription>
            账号名称用于在列表中识别;账号的 Cookie 通过列表「登录」扫码获取。
          </SheetDescription>
        </SheetHeader>
        <form onSubmit={handleSubmit} className="flex min-h-0 flex-1 flex-col">
          <div className="flex-1 space-y-4 overflow-y-auto p-5">
            <div className="space-y-1.5">
              <Label htmlFor="account-label">
                名称 <span className="text-destructive">*</span>
              </Label>
              <Input
                id="account-label"
                placeholder="平台账号"
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
            <div className="space-y-1.5">
              <Label htmlFor="account-code">编码</Label>
              <CodeField
                id="account-code"
                value={code}
                onRegenerate={() => setCode(generateCode("ACC"))}
              />
              <p className="text-xs text-muted-foreground">
                系统自动生成,可刷新或复制
              </p>
            </div>
            <div className="space-y-1.5">
              <Label htmlFor="account-remark">备注</Label>
              <Input
                id="account-remark"
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
