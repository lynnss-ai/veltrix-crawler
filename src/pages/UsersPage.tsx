import {
  useEffect,
  useMemo,
  useRef,
  useState,
  type ChangeEvent,
  type FormEvent,
} from "react";
import { type ColumnDef, type FilterFn } from "@tanstack/react-table";
import {
  Camera,
  CircleCheck,
  CircleDashed,
  Eye,
  EyeOff,
  KeyRound,
  MoreVertical,
  Pencil,
  Plus,
  RefreshCw,
  Search,
  Trash2,
  Upload,
  Users,
  X,
  type LucideIcon,
} from "lucide-react";
import {
  api,
  formatTimestamp,
  type UserInput,
  type UserView,
} from "@/lib/api";
import { FORM_CONTROL_SIZING } from "@/lib/form-sizing";
import { FieldError } from "@/components/FieldError";
import { generatePassword } from "@/lib/password";
import { toast } from "sonner";
import { Avatar } from "@/components/Avatar";
import { DataTable } from "@/components/DataTable";
import { DataTableColumnHeader } from "@/components/DataTableColumnHeader";
import { DataTableFacetedFilter } from "@/components/DataTableFacetedFilter";
import { StatusBadge, type StatusTone } from "@/components/StatusBadge";
import { RefreshButton } from "@/components/RefreshButton";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
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
import { Switch } from "@/components/ui/switch";
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
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
} from "@/components/ui/alert-dialog";

// 用户管理:基于 TanStack DataTable 的增删改查(软删除)。当前为前端占位,
// 待后端新增 user 表(密码哈希存储、软删 deleted_at)与 list/upsert/remove_user 命令后接入。

type UserStatus = "enabled" | "disabled";
// 数据级别:all 可见全部数据,self 仅可见自己创建的数据
type DataScope = "all" | "self";

const DATA_SCOPE_META: Record<DataScope, string> = {
  all: "全部数据",
  self: "仅自己",
};

// 列表项复用后端用户视图(不含密码);提交用 UserInput
type UserItem = UserView;

// 简单邮箱格式校验
const EMAIL_PATTERN = /^[^\s@]+@[^\s@]+\.[^\s@]+$/;
// 密码最小长度(大小写字母 + 数字,至少 6 位)
const MIN_PASSWORD_LENGTH = 6;

// 新建用户默认头像:DiceBear 抽象彩色图案,基于随机种子(离线时由 Avatar 回退首字母)
function randomAvatarUrl(): string {
  return `https://api.dicebear.com/9.x/thumbs/svg?seed=${crypto.randomUUID()}`;
}
// 头像上传大小上限(2MB);转 base64 内联存储,过大影响性能
const MAX_AVATAR_BYTES = 2 * 1024 * 1024;

const STATUS_META: Record<
  UserStatus,
  { label: string; tone: StatusTone; icon: LucideIcon }
> = {
  enabled: { label: "启用", tone: "success", icon: CircleCheck },
  disabled: { label: "禁用", tone: "neutral", icon: CircleDashed },
};

// 全局搜索:匹配用户名 / 昵称 / 邮箱
const globalFilterFn: FilterFn<UserItem> = (row, _columnId, value) => {
  const u = row.original;
  return `${u.username} ${u.nickname} ${u.email}`
    .toLowerCase()
    .includes(String(value).toLowerCase());
};

export function UsersPage() {
  const [users, setUsers] = useState<UserView[]>([]);
  const [editing, setEditing] = useState<UserItem | null>(null);
  const [isFormOpen, setIsFormOpen] = useState(false);
  const [isFormDirty, setIsFormDirty] = useState(false);
  const [deleteTarget, setDeleteTarget] = useState<UserItem | null>(null);
  const [resetTarget, setResetTarget] = useState<UserItem | null>(null);

  // 从后端加载用户列表(后端已过滤软删除)
  function reload() {
    api
      .listUsers()
      .then(setUsers)
      .catch((e) => toast.error(`加载用户失败: ${e}`));
  }
  useEffect(() => {
    reload();
  }, []);

  const data = users;

  function openCreate() {
    setEditing(null);
    setIsFormOpen(true);
  }

  function confirmDelete() {
    if (!deleteTarget) return;
    api
      .removeUser(deleteTarget.id)
      .then(() => {
        toast.success(`已删除「${deleteTarget.username}」`);
        setDeleteTarget(null);
        reload();
      })
      .catch((e) => toast.error(`删除失败: ${e}`));
  }

  function confirmReset(user: UserItem, password: string) {
    api
      .resetUserPassword(user.id, password)
      .then(() => {
        toast.success(`已重置「${user.username}」的密码`);
        setResetTarget(null);
        reload();
      })
      .catch((e) => toast.error(`重置失败: ${e}`));
  }

  function submit(input: UserInput) {
    api
      .upsertUser(input)
      .then(() => {
        setIsFormOpen(false);
        reload();
      })
      .catch((e) => toast.error(`保存失败: ${e}`));
  }

  // 列定义:setter 引用稳定,故依赖留空
  const columns = useMemo<ColumnDef<UserItem>[]>(
    () => [
      {
        accessorKey: "username",
        header: ({ column }) => (
          <DataTableColumnHeader column={column} title="用户" />
        ),
        cell: ({ row }) => {
          const u = row.original;
          return (
            <div className="flex items-center gap-3">
              <Avatar src={u.avatar} name={u.nickname || u.username} />
              <div className="grid">
                <span className="font-medium text-foreground">
                  {u.username}
                </span>
                <span className="text-xs text-muted-foreground">
                  {u.email || "—"}
                </span>
              </div>
            </div>
          );
        },
      },
      {
        accessorKey: "nickname",
        header: "昵称",
        cell: ({ row }) => row.original.nickname || "—",
        enableSorting: false,
      },
      {
        accessorKey: "status",
        header: "状态",
        enableSorting: false,
        filterFn: (row, id, value) =>
          (value as string[]).includes(row.getValue(id) as string),
        cell: ({ row }) => {
          const meta = STATUS_META[row.original.status as UserStatus];
          const Icon = meta.icon;
          return (
            <StatusBadge tone={meta.tone}>
              <Icon className="size-3.5" />
              {meta.label}
            </StatusBadge>
          );
        },
      },
      {
        accessorKey: "dataScope",
        header: "数据级别",
        cell: ({ row }) => (
          <span className="text-muted-foreground">
            {DATA_SCOPE_META[row.original.dataScope as DataScope]}
          </span>
        ),
      },
      {
        accessorKey: "createdAt",
        header: ({ column }) => (
          <DataTableColumnHeader column={column} title="创建时间" />
        ),
        cell: ({ row }) => (
          <span className="text-muted-foreground">
            {formatTimestamp(row.original.createdAt)}
          </span>
        ),
      },
      {
        id: "actions",
        header: () => <div className="text-right">操作</div>,
        enableSorting: false,
        cell: ({ row }) => {
          const u = row.original;
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
                <DropdownMenuContent align="end" className="w-40">
                  <DropdownMenuItem
                    onClick={() => {
                      setEditing(u);
                      setIsFormOpen(true);
                    }}
                  >
                    <Pencil />
                    编辑
                  </DropdownMenuItem>
                  <DropdownMenuItem onClick={() => setResetTarget(u)}>
                    <KeyRound />
                    重置密码
                  </DropdownMenuItem>
                  <DropdownMenuSeparator />
                  <DropdownMenuItem
                    className="text-destructive focus:text-destructive"
                    onClick={() => setDeleteTarget(u)}
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

  const emptyState = (
    <div className="flex flex-col items-center justify-center gap-2 py-12 text-center">
      <div className="flex size-12 items-center justify-center rounded-full bg-muted text-muted-foreground">
        <Users className="size-6" />
      </div>
      <p className="text-sm font-medium text-foreground">没有符合条件的用户</p>
      <p className="text-xs text-muted-foreground">
        调整搜索 / 筛选,或点击「新增用户」创建账号
      </p>
    </div>
  );

  return (
    <div className={`flex min-h-0 flex-1 flex-col ${FORM_CONTROL_SIZING}`}>
      <DataTable
        columns={columns}
        data={data}
        itemLabel="用户"
        globalFilterFn={globalFilterFn}
        getRowId={(u) => u.id}
        emptyState={emptyState}
        renderToolbar={(table) => (
          <div className="flex flex-col gap-3 sm:flex-row sm:items-center sm:justify-between">
            <div className="flex flex-1 flex-wrap items-center gap-2">
              <div className="relative w-full sm:max-w-xs">
                <Search className="pointer-events-none absolute left-3 top-1/2 size-4 -translate-y-1/2 text-muted-foreground" />
                <Input
                  placeholder="搜索用户名 / 昵称 / 邮箱"
                  className="pl-9"
                  value={(table.getState().globalFilter as string) ?? ""}
                  onChange={(e) => table.setGlobalFilter(e.target.value)}
                />
              </div>
              <DataTableFacetedFilter
                column={table.getColumn("status")}
                title="状态"
                options={[
                  { label: "启用", value: "enabled", icon: CircleCheck },
                  { label: "禁用", value: "disabled", icon: CircleDashed },
                ]}
              />
              {(table.getState().columnFilters.length > 0 ||
                ((table.getState().globalFilter as string) ?? "") !== "") && (
                <Button
                  variant="ghost"
                  className="px-2 lg:px-3"
                  onClick={() => {
                    table.resetColumnFilters();
                    table.setGlobalFilter("");
                  }}
                >
                  重置
                  <X />
                </Button>
              )}
            </div>
            <div className="flex items-center gap-2">
              <RefreshButton onClick={reload} />
              <Button onClick={openCreate}>
                <Plus />
                新增用户
              </Button>
            </div>
          </div>
        )}
      />

      <Sheet open={isFormOpen} onOpenChange={setIsFormOpen}>
        <SheetContent
          className="flex w-full flex-col gap-0 p-0 sm:max-w-lg"
          blockClose={isFormDirty}
        >
          <UserForm
            key={isFormOpen ? (editing?.id ?? "new") : "idle"}
            initial={editing}
            onSubmit={submit}
            onCancel={() => setIsFormOpen(false)}
            onDirtyChange={setIsFormDirty}
          />
        </SheetContent>
      </Sheet>

      <AlertDialog
        open={deleteTarget !== null}
        onOpenChange={(open) => !open && setDeleteTarget(null)}
      >
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>
              删除用户「{deleteTarget?.username}」?
            </AlertDialogTitle>
            <AlertDialogDescription>
              删除后该用户将不在列表中显示。此操作为软删除,数据仍可在后端审计中追溯。
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel>取消</AlertDialogCancel>
            <AlertDialogAction
              onClick={confirmDelete}
              className="bg-destructive text-white hover:bg-destructive/90"
            >
              删除
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>

      <ResetPasswordSheet
        target={resetTarget}
        onOpenChange={(open) => !open && setResetTarget(null)}
        onConfirm={confirmReset}
      />
    </div>
  );
}

// 重置密码抽屉:为指定用户随机生成新密码,可查看 / 刷新 / 手动修改
function ResetPasswordSheet({
  target,
  onOpenChange,
  onConfirm,
}: {
  target: UserItem | null;
  onOpenChange: (open: boolean) => void;
  onConfirm: (user: UserItem, password: string) => void;
}) {
  const [password, setPassword] = useState("");
  const [show, setShow] = useState(true);
  // 打开(target 变化)时生成一个新随机密码
  useEffect(() => {
    if (target) {
      setPassword(generatePassword());
      setShow(true);
    }
  }, [target]);

  const tooShort = password.trim().length < MIN_PASSWORD_LENGTH;

  return (
    <Sheet open={target !== null} onOpenChange={onOpenChange}>
      <SheetContent
        className="flex w-full flex-col gap-0 p-0 sm:max-w-md"
        blockClose
      >
        <SheetHeader className="border-b">
          <SheetTitle>重置密码</SheetTitle>
          <SheetDescription>
            为用户「{target?.username}」生成新密码,可刷新或手动修改。
          </SheetDescription>
        </SheetHeader>
        <div className="flex-1 space-y-1.5 overflow-y-auto p-5">
          <Label htmlFor="reset-pwd">新密码</Label>
          <div className="flex gap-2">
            <div className="relative flex-1">
              <Input
                id="reset-pwd"
                type={show ? "text" : "password"}
                className="pr-9"
                value={password}
                onChange={(e) => setPassword(e.target.value)}
                aria-invalid={tooShort}
              />
              <button
                type="button"
                onClick={() => setShow((s) => !s)}
                className="absolute right-2 top-1/2 -translate-y-1/2 text-muted-foreground transition-colors hover:text-foreground"
                title={show ? "隐藏" : "显示"}
              >
                {show ? (
                  <EyeOff className="size-4" />
                ) : (
                  <Eye className="size-4" />
                )}
              </button>
            </div>
            <Button
              type="button"
              variant="outline"
              size="icon"
              className="shrink-0"
              title="重新生成"
              onClick={() => {
                setPassword(generatePassword());
                setShow(true);
              }}
            >
              <RefreshCw />
              <span className="sr-only">重新生成</span>
            </Button>
          </div>
          <FieldError
            show={tooShort}
            message="密码至少 6 位(大小写字母 + 数字)"
          />
        </div>
        <SheetFooter className="flex-row justify-end gap-2 border-t">
          <Button
            type="button"
            variant="outline"
            onClick={() => onOpenChange(false)}
          >
            取消
          </Button>
          <Button
            type="button"
            disabled={tooShort}
            onClick={() => target && onConfirm(target, password.trim())}
          >
            确认重置
          </Button>
        </SheetFooter>
      </SheetContent>
    </Sheet>
  );
}

// 表单分组小标题
function SectionLabel({ children }: { children: string }) {
  return (
    <div className="text-xs font-medium uppercase tracking-wide text-muted-foreground">
      {children}
    </div>
  );
}

// 新增 / 编辑表单(置于 Sheet 内)。提交逻辑当前为前端占位,待后端 upsert_user 命令就绪后替换。
function UserForm({
  initial,
  onSubmit,
  onCancel,
  onDirtyChange,
}: {
  initial: UserItem | null;
  onSubmit: (user: UserInput) => void;
  onCancel: () => void;
  onDirtyChange: (dirty: boolean) => void;
}) {
  const isEdit = initial !== null;
  const [username, setUsername] = useState(initial?.username ?? "");
  // 新建用户默认随机生成初始密码(可查看/刷新/手动改);编辑不在此改密码,改用列表「重置密码」
  const [password, setPassword] = useState(() =>
    isEdit ? "" : generatePassword(),
  );
  const [showPassword, setShowPassword] = useState(false);
  const [email, setEmail] = useState(initial?.email ?? "");
  const [nickname, setNickname] = useState(initial?.nickname ?? "");
  const [initialAvatar] = useState(() =>
    isEdit ? (initial?.avatar ?? "") : randomAvatarUrl(),
  );
  const [avatar, setAvatar] = useState(initialAvatar);
  const [remark, setRemark] = useState(initial?.remark ?? "");
  const [status, setStatus] = useState<UserStatus>(
    (initial?.status as UserStatus) ?? "enabled",
  );
  const [dataScope, setDataScope] = useState<DataScope>(
    (initial?.dataScope as DataScope) ?? "self",
  );
  const [error, setError] = useState<string | null>(null);
  const [submitted, setSubmitted] = useState(false);
  const fileInputRef = useRef<HTMLInputElement>(null);

  // 用户改动过任一字段即视为已编辑,向父级上报以阻止误关(随机初始密码不计入)
  const isDirty =
    username !== (initial?.username ?? "") ||
    email !== (initial?.email ?? "") ||
    nickname !== (initial?.nickname ?? "") ||
    avatar !== initialAvatar ||
    remark !== (initial?.remark ?? "") ||
    status !== (initial?.status ?? "enabled");
  useEffect(() => {
    onDirtyChange(isDirty);
  }, [isDirty, onDirtyChange]);

  // 头像上传:本地读为 base64 data URL 内联预览;接后端后改为上传文件并存 URL
  function handleAvatarChange(event: ChangeEvent<HTMLInputElement>) {
    const file = event.target.files?.[0];
    if (!file) return;
    if (!file.type.startsWith("image/")) {
      setError("请选择图片文件");
      return;
    }
    if (file.size > MAX_AVATAR_BYTES) {
      setError("头像不能超过 2MB");
      return;
    }
    const reader = new FileReader();
    reader.onload = () => {
      if (typeof reader.result === "string") setAvatar(reader.result);
    };
    reader.onerror = () => setError("头像读取失败");
    reader.readAsDataURL(file);
    // 重置 value 以便能再次选择同一文件
    event.target.value = "";
  }

  function handleSubmit(event: FormEvent) {
    event.preventDefault();
    setSubmitted(true);
    // 必填项为空时改为字段下方提示(见各输入框的 FieldError),不再用顶部红框
    if (!username.trim() || (!isEdit && !password) || !nickname.trim()) {
      return;
    }
    if (email && !EMAIL_PATTERN.test(email)) {
      setError("邮箱格式不正确");
      return;
    }
    onSubmit({
      id: initial?.id ?? crypto.randomUUID(),
      username: username.trim(),
      password, // 编辑时为空表示不改;新建为随机/手填密码
      email: email.trim(),
      nickname: nickname.trim(),
      avatar: avatar.trim(),
      remark: remark.trim(),
      status,
      dataScope,
    });
  }

  const displayName = nickname || username;

  return (
    <>
      <SheetHeader className="border-b">
        <SheetTitle>{isEdit ? "编辑用户" : "新增用户"}</SheetTitle>
        <SheetDescription>
          {isEdit
            ? "修改用户资料,密码留空表示不变更。"
            : "创建一个新的后台账号。"}
        </SheetDescription>
      </SheetHeader>

      <form onSubmit={handleSubmit} className="flex min-h-0 flex-1 flex-col">
        <div className="flex-1 space-y-6 overflow-y-auto p-5">
          {error && (
            <div className="rounded-lg bg-destructive/10 px-3 py-2 text-sm text-destructive">
              {error}
            </div>
          )}

          {/* 头像:点击头像或按钮上传,hover 显示相机遮罩 */}
          <div className="flex items-center gap-3">
            <button
              type="button"
              onClick={() => fileInputRef.current?.click()}
              className="group relative shrink-0 rounded-full ring-offset-2 ring-offset-background focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
            >
              <Avatar src={avatar} name={displayName} size="lg" />
              <span className="absolute inset-0 flex items-center justify-center rounded-full bg-black/50 opacity-0 transition-opacity group-hover:opacity-100">
                <Camera className="h-5 w-5 text-white" />
              </span>
            </button>
            <input
              ref={fileInputRef}
              type="file"
              accept="image/*"
              className="hidden"
              onChange={handleAvatarChange}
            />
            <div className="flex flex-col gap-1.5">
              <div className="flex items-center gap-2">
                <Button
                  type="button"
                  variant="outline"
                  size="sm"
                  onClick={() => fileInputRef.current?.click()}
                >
                  <Upload />
                  上传头像
                </Button>
                {avatar && (
                  <Button
                    type="button"
                    variant="ghost"
                    size="sm"
                    className="text-destructive"
                    onClick={() => setAvatar("")}
                  >
                    移除
                  </Button>
                )}
              </div>
              <p className="text-xs text-muted-foreground">
                支持 JPG/PNG,不超过 2MB
              </p>
            </div>
          </div>

          {/* 账号信息 */}
          <div className="space-y-3">
            <SectionLabel>账号信息</SectionLabel>
            <div className="space-y-1.5">
              <Label htmlFor="username">
                用户名 <span className="text-destructive">*</span>
              </Label>
              <Input
                id="username"
                placeholder="登录用户名"
                value={username}
                onChange={(e) => setUsername(e.target.value)}
                aria-invalid={submitted && !username.trim()}
                disabled={isEdit}
                autoFocus={!isEdit}
              />
              {isEdit ? (
                <p className="text-xs text-muted-foreground">
                  用户名创建后不可修改
                </p>
              ) : (
                <FieldError
                  show={submitted && !username.trim()}
                  message="用户名不可为空"
                />
              )}
            </div>
            {!isEdit && (
              <div className="space-y-1.5">
                <Label htmlFor="password">
                  密码 <span className="text-destructive">*</span>
                </Label>
                <div className="flex gap-2">
                  <div className="relative flex-1">
                    <Input
                      id="password"
                      type={showPassword ? "text" : "password"}
                      className="pr-9"
                      placeholder="初始密码"
                      value={password}
                      onChange={(e) => setPassword(e.target.value)}
                      aria-invalid={submitted && !password}
                    />
                    <button
                      type="button"
                      onClick={() => setShowPassword((s) => !s)}
                      className="absolute right-2 top-1/2 -translate-y-1/2 text-muted-foreground transition-colors hover:text-foreground"
                      title={showPassword ? "隐藏" : "显示"}
                    >
                      {showPassword ? (
                        <EyeOff className="size-4" />
                      ) : (
                        <Eye className="size-4" />
                      )}
                      <span className="sr-only">
                        {showPassword ? "隐藏密码" : "显示密码"}
                      </span>
                    </button>
                  </div>
                  <Button
                    type="button"
                    variant="outline"
                    size="icon"
                    className="shrink-0"
                    title="重新生成"
                    onClick={() => {
                      setPassword(generatePassword());
                      setShowPassword(true);
                    }}
                  >
                    <RefreshCw />
                    <span className="sr-only">重新生成</span>
                  </Button>
                </div>
                <p className="text-xs text-muted-foreground">
                  系统随机生成(大小写字母 + 数字),可查看 / 刷新 / 手动修改
                </p>
                <FieldError
                  show={submitted && !password}
                  message="密码不可为空"
                />
              </div>
            )}
          </div>

          {/* 个人资料 */}
          <div className="space-y-3">
            <SectionLabel>个人资料</SectionLabel>
            <div className="space-y-1.5">
              <Label htmlFor="nickname">
                昵称 <span className="text-destructive">*</span>
              </Label>
              <Input
                id="nickname"
                placeholder="显示名称"
                value={nickname}
                onChange={(e) => setNickname(e.target.value)}
                aria-invalid={submitted && !nickname.trim()}
              />
              <FieldError
                show={submitted && !nickname.trim()}
                message="昵称不可为空"
              />
            </div>
            <div className="space-y-1.5">
              <Label htmlFor="email">邮箱</Label>
              <Input
                id="email"
                placeholder="name@company.com(可选)"
                value={email}
                onChange={(e) => setEmail(e.target.value)}
              />
            </div>
            <div className="space-y-1.5">
              <Label htmlFor="remark">备注</Label>
              <Textarea
                id="remark"
                placeholder="备注信息(可选)"
                className="min-h-20"
                rows={3}
                value={remark}
                onChange={(e) => setRemark(e.target.value)}
              />
            </div>
          </div>

          {/* 状态 */}
          <div className="space-y-3">
            <SectionLabel>账号状态</SectionLabel>
            <div className="flex items-center justify-between rounded-lg border border-border p-3">
              <div>
                <div className="text-sm font-medium text-foreground">
                  启用账号
                </div>
                <div className="text-xs text-muted-foreground">
                  停用后该账号无法登录后台
                </div>
              </div>
              <Switch
                checked={status === "enabled"}
                onCheckedChange={(v) => setStatus(v ? "enabled" : "disabled")}
              />
            </div>
            <div className="space-y-1.5">
              <Label>数据级别</Label>
              <Select
                value={dataScope}
                onValueChange={(v) => setDataScope(v as DataScope)}
              >
                <SelectTrigger className="w-full">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="all">全部数据</SelectItem>
                  <SelectItem value="self">仅自己</SelectItem>
                </SelectContent>
              </Select>
              <p className="text-xs text-muted-foreground">
                控制账号 / 任务 / 资产 / 客户等业务数据的可见范围
              </p>
            </div>
          </div>
        </div>

        <SheetFooter className="flex-row justify-end gap-2 border-t">
          <Button type="button" variant="outline" onClick={onCancel}>
            取消
          </Button>
          <Button type="submit">保存</Button>
        </SheetFooter>
      </form>
    </>
  );
}
