import {
  useEffect,
  useMemo,
  useState,
  type FormEvent,
  type KeyboardEvent,
} from "react";
import { type ColumnDef, type FilterFn } from "@tanstack/react-table";
import { MoreVertical, SquarePen, Plus, Search, Trash2, X } from "lucide-react";
import {
  api,
  formatTimestamp,
  type CustomerInput,
  type CustomerView,
  type IndustryView,
} from "@/lib/api";
import { FORM_CONTROL_SIZING } from "@/lib/form-sizing";
import { ErrorBanner } from "@/components/ErrorBanner";
import { DataTable } from "@/components/DataTable";
import { EmptyState } from "@/components/EmptyState";
import { DataTableColumnHeader } from "@/components/DataTableColumnHeader";
import { DataTableFacetedFilter } from "@/components/DataTableFacetedFilter";
import { StatusBadge, type StatusTone } from "@/components/StatusBadge";
import { FieldError } from "@/components/FieldError";
import { CodeField, generateCode } from "@/components/CodeField";
import { RefreshButton } from "@/components/RefreshButton";
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

// 客户管理(CRM):接真实后端 customer 表 list/upsert/remove 命令。

type CustomerStatus =
  | "new"
  | "following"
  | "negotiating"
  | "closed"
  | "lost"
  | "dormant";

// 列表项复用后端视图;状态字段后端为字符串,渲染时按已知枚举取文案
type CustomerItem = CustomerView;

// 客户状态:文案 + 色调
const CUSTOMER_STATUS_META: Record<
  CustomerStatus,
  { label: string; tone: StatusTone }
> = {
  new: { label: "新客户", tone: "info" },
  following: { label: "跟进中", tone: "warning" },
  negotiating: { label: "洽谈中", tone: "warning" },
  closed: { label: "已成交", tone: "success" },
  lost: { label: "已流失", tone: "danger" },
  dormant: { label: "休眠", tone: "neutral" },
};

const STATUS_ORDER: CustomerStatus[] = [
  "new",
  "following",
  "negotiating",
  "closed",
  "lost",
  "dormant",
];


// 客户状态色调兜底:后端状态为任意字符串,未知时按 neutral 渲染
function customerStatusMeta(status: string): { label: string; tone: StatusTone } {
  return (
    CUSTOMER_STATUS_META[status as CustomerStatus] ?? {
      label: status || "—",
      tone: "neutral",
    }
  );
}

// 全局搜索:匹配姓名 / 联系方式 / 公司 / 邮箱 / 微信 / 跟踪人
const customerFilterFn: FilterFn<CustomerItem> = (row, _columnId, value) => {
  const c = row.original;
  return `${c.name} ${c.phone} ${c.company} ${c.email} ${c.wechat} ${c.owner}`
    .toLowerCase()
    .includes(String(value).toLowerCase());
};

export function CustomersPage({ currentUser }: { currentUser: string }) {
  const [customers, setCustomers] = useState<CustomerItem[]>([]);
  const [editing, setEditing] = useState<CustomerItem | null>(null);
  const [isFormOpen, setIsFormOpen] = useState(false);
  const [deleteTarget, setDeleteTarget] = useState<CustomerItem | null>(null);
  const [error, setError] = useState<string | null>(null);
  // 所属行业下拉来自「行业类别」维护的真实数据
  const [industries, setIndustries] = useState<IndustryView[]>([]);

  async function loadCustomers() {
    try {
      setCustomers(await api.listCustomers());
    } catch (e) {
      setError(String(e));
    }
  }

  useEffect(() => {
    loadCustomers();
    api
      .listIndustries()
      .then(setIndustries)
      .catch((e) => setError(String(e)));
  }, []);

  async function submit(input: CustomerInput) {
    try {
      await api.upsertCustomer(input);
      setIsFormOpen(false);
      await loadCustomers();
    } catch (e) {
      setError(String(e));
    }
  }

  async function confirmDelete() {
    if (!deleteTarget) return;
    try {
      await api.removeCustomer(deleteTarget.id);
      await loadCustomers();
    } catch (e) {
      setError(String(e));
    }
    setDeleteTarget(null);
  }

  const columns = useMemo<ColumnDef<CustomerItem>[]>(
    () => [
      {
        accessorKey: "name",
        header: ({ column }) => (
          <DataTableColumnHeader column={column} title="客户" />
        ),
        cell: ({ row }) => {
          const c = row.original;
          const sub = [c.company, c.position].filter(Boolean).join(" · ");
          return (
            <div className="flex flex-col">
              <span className="font-medium text-foreground">{c.name}</span>
              {sub && (
                <span className="text-xs text-muted-foreground">{sub}</span>
              )}
            </div>
          );
        },
      },
      {
        accessorKey: "phone",
        header: "联系方式",
        cell: ({ row }) => row.original.phone || "—",
      },
      {
        accessorKey: "industry",
        header: "所属行业",
        cell: ({ row }) => row.original.industry || "—",
      },
      {
        accessorKey: "status",
        header: "状态",
        filterFn: (row, id, value) =>
          (value as string[]).includes(row.getValue(id)),
        cell: ({ row }) => {
          const meta = customerStatusMeta(row.original.status);
          return <StatusBadge tone={meta.tone}>{meta.label}</StatusBadge>;
        },
      },
      {
        accessorKey: "owner",
        header: "跟踪人",
        cell: ({ row }) => row.original.owner || "—",
      },
      {
        accessorKey: "updatedAt",
        header: ({ column }) => (
          <DataTableColumnHeader column={column} title="更新时间" />
        ),
        cell: ({ row }) => (
          <span className="text-muted-foreground">
            {formatTimestamp(row.original.updatedAt)}
          </span>
        ),
      },
      {
        id: "actions",
        header: () => <div className="text-right">操作</div>,
        enableSorting: false,
        cell: ({ row }) => {
          const c = row.original;
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
                      setEditing(c);
                      setIsFormOpen(true);
                    }}
                  >
                    <SquarePen />
                    编辑
                  </DropdownMenuItem>
                  <DropdownMenuSeparator />
                  <DropdownMenuItem
                    className="text-destructive focus:text-destructive"
                    onClick={() => setDeleteTarget(c)}
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

  const statusOptions = STATUS_ORDER.map((s) => ({
    label: CUSTOMER_STATUS_META[s].label,
    value: s,
  }));

  return (
    <div className={`flex min-h-0 flex-1 flex-col gap-4 ${FORM_CONTROL_SIZING}`}>
      <ErrorBanner message={error} onClose={() => setError(null)} />
      <DataTable
        columns={columns}
        data={customers}
        itemLabel="客户"
        globalFilterFn={customerFilterFn}
        getRowId={(row) => row.id}
        renderToolbar={(table) => (
          <div className="flex flex-col gap-3 sm:flex-row sm:items-center sm:justify-between">
            <div className="flex flex-1 flex-wrap items-center gap-2">
              <div className="relative w-full sm:max-w-sm">
                <Search className="pointer-events-none absolute left-3 top-1/2 size-4 -translate-y-1/2 text-muted-foreground" />
                <Input
                  placeholder="搜索姓名 / 电话 / 公司"
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
              <RefreshButton onClick={loadCustomers} />
              <Button
                onClick={() => {
                  setEditing(null);
                  setIsFormOpen(true);
                }}
              >
                <Plus />
                新增客户
              </Button>
            </div>
          </div>
        )}
        emptyState={
          <EmptyState
            title="暂无客户"
            description="点击右上角「新增客户」开始建立客户档案"
          />
        }
      />

      <Sheet open={isFormOpen} onOpenChange={setIsFormOpen}>
        <CustomerFormSheet
          key={isFormOpen ? (editing?.id ?? "new") : "idle"}
          initial={editing}
          currentUser={currentUser}
          industries={industries}
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
            <AlertDialogTitle>删除客户「{deleteTarget?.name}」?</AlertDialogTitle>
            <AlertDialogDescription>
              删除后该客户档案将从列表移除,此操作不可撤销。
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
    </div>
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

// 新增 / 编辑客户(置于 Sheet 内)
function CustomerFormSheet({
  initial,
  currentUser,
  industries,
  onSubmit,
  onCancel,
}: {
  initial: CustomerItem | null;
  currentUser: string;
  industries: IndustryView[];
  onSubmit: (customer: CustomerInput) => void;
  onCancel: () => void;
}) {
  const isEdit = initial !== null;
  const [code, setCode] = useState(initial?.code ?? generateCode("CUS"));
  const [name, setName] = useState(initial?.name ?? "");
  const [phone, setPhone] = useState(initial?.phone ?? "");
  const [email, setEmail] = useState(initial?.email ?? "");
  const [company, setCompany] = useState(initial?.company ?? "");
  const [position, setPosition] = useState(initial?.position ?? "");
  const [wechat, setWechat] = useState(initial?.wechat ?? "");
  const [industry, setIndustry] = useState(initial?.industry ?? "");
  const [tags, setTags] = useState<string[]>(initial?.tags ?? []);
  const [tagInput, setTagInput] = useState("");
  const [source, setSource] = useState(initial?.source ?? "");
  const [status, setStatus] = useState<CustomerStatus>(
    (initial?.status as CustomerStatus) ?? "new",
  );
  const [remark, setRemark] = useState(initial?.remark ?? "");
  const [submitted, setSubmitted] = useState(false);
  // 跟踪人:新建关联当前用户,编辑保留原跟踪人
  const owner = initial?.owner ?? currentUser;

  function addTag() {
    const value = tagInput.trim();
    if (value && !tags.includes(value)) setTags([...tags, value]);
    setTagInput("");
  }

  function handleTagKeyDown(event: KeyboardEvent<HTMLInputElement>) {
    if (event.key === "Enter" || event.key === ",") {
      event.preventDefault();
      addTag();
    }
  }

  function handleSubmit(event: FormEvent) {
    event.preventDefault();
    setSubmitted(true);
    if (
      !name.trim() ||
      !phone.trim() ||
      !company.trim() ||
      !industry ||
      !status
    ) {
      return;
    }
    onSubmit({
      id: initial?.id ?? crypto.randomUUID(),
      code,
      name: name.trim(),
      phone: phone.trim(),
      email: email.trim(),
      company: company.trim(),
      position: position.trim(),
      wechat: wechat.trim(),
      industry,
      tags,
      source: source.trim(),
      status,
      owner,
      remark: remark.trim(),
    });
  }

  return (
    <SheetContent
      className="flex w-full flex-col gap-0 p-0 sm:max-w-[600px]"
      blockClose={
        name !== (initial?.name ?? "") ||
        phone !== (initial?.phone ?? "") ||
        email !== (initial?.email ?? "") ||
        company !== (initial?.company ?? "") ||
        position !== (initial?.position ?? "") ||
        wechat !== (initial?.wechat ?? "") ||
        industry !== (initial?.industry ?? "") ||
        source !== (initial?.source ?? "") ||
        status !== (initial?.status ?? "new") ||
        remark !== (initial?.remark ?? "") ||
        tags.join(",") !== (initial?.tags ?? []).join(",")
      }
    >
      <SheetHeader className="border-b">
        <SheetTitle>{isEdit ? "编辑客户" : "新增客户"}</SheetTitle>
        <SheetDescription>
          维护客户档案与跟进状态,跟踪人默认关联创建者。
        </SheetDescription>
      </SheetHeader>
      <form onSubmit={handleSubmit} className="flex min-h-0 flex-1 flex-col">
        <div className="flex-1 space-y-6 overflow-y-auto p-5">
          <div className="space-y-3">
            <SectionLabel>基本信息</SectionLabel>
            <div className="space-y-1.5">
              <Label htmlFor="customer-code">客户编号</Label>
              <CodeField
                id="customer-code"
                value={code}
                onRegenerate={() => setCode(generateCode("CUS"))}
              />
              <p className="text-xs text-muted-foreground">
                系统自动生成,可刷新或复制
              </p>
            </div>
            <div className="space-y-1.5">
              <Label>
                所属行业 <span className="text-destructive">*</span>
              </Label>
              <Select value={industry} onValueChange={setIndustry}>
                <SelectTrigger
                  className="w-full"
                  aria-invalid={submitted && !industry}
                >
                  <SelectValue placeholder="选择所属行业" />
                </SelectTrigger>
                <SelectContent>
                  {industries.length === 0 ? (
                    <div className="px-2 py-1.5 text-sm text-muted-foreground">
                      暂无行业,请先到「行业类别」添加
                    </div>
                  ) : (
                    industries.map((item) => (
                      <SelectItem key={item.id} value={item.name}>
                        {item.name}
                      </SelectItem>
                    ))
                  )}
                </SelectContent>
              </Select>
              <FieldError
                show={submitted && !industry}
                message="请选择所属行业"
              />
            </div>
            <div className="space-y-1.5">
              <Label htmlFor="customer-name">
                客户姓名 <span className="text-destructive">*</span>
              </Label>
              <Input
                id="customer-name"
                placeholder="客户姓名"
                value={name}
                onChange={(e) => setName(e.target.value)}
                aria-invalid={submitted && !name.trim()}
                autoFocus
              />
              <FieldError
                show={submitted && !name.trim()}
                message="客户姓名不可为空"
              />
            </div>
            <div className="grid gap-4 sm:grid-cols-2">
              <div className="space-y-1.5">
                <Label htmlFor="customer-phone">
                  联系方式 <span className="text-destructive">*</span>
                </Label>
                <Input
                  id="customer-phone"
                  placeholder="手机 / 座机"
                  value={phone}
                  onChange={(e) => setPhone(e.target.value)}
                  aria-invalid={submitted && !phone.trim()}
                />
                <FieldError
                  show={submitted && !phone.trim()}
                  message="联系方式不可为空"
                />
              </div>
              <div className="space-y-1.5">
                <Label htmlFor="customer-wechat">微信号</Label>
                <Input
                  id="customer-wechat"
                  placeholder="微信号"
                  value={wechat}
                  onChange={(e) => setWechat(e.target.value)}
                />
              </div>
            </div>
            <div className="space-y-1.5">
              <Label htmlFor="customer-email">电子邮箱</Label>
              <Input
                id="customer-email"
                type="email"
                placeholder="name@example.com"
                value={email}
                onChange={(e) => setEmail(e.target.value)}
              />
            </div>
          </div>

          <div className="space-y-3">
            <SectionLabel>公司信息</SectionLabel>
            <div className="grid gap-4 sm:grid-cols-2">
              <div className="space-y-1.5">
                <Label htmlFor="customer-company">
                  公司 / 机构 <span className="text-destructive">*</span>
                </Label>
                <Input
                  id="customer-company"
                  placeholder="所在公司或机构"
                  value={company}
                  onChange={(e) => setCompany(e.target.value)}
                  aria-invalid={submitted && !company.trim()}
                />
                <FieldError
                  show={submitted && !company.trim()}
                  message="公司 / 机构不可为空"
                />
              </div>
              <div className="space-y-1.5">
                <Label htmlFor="customer-position">职务</Label>
                <Input
                  id="customer-position"
                  placeholder="如:市场总监"
                  value={position}
                  onChange={(e) => setPosition(e.target.value)}
                />
              </div>
            </div>
          </div>

          <div className="space-y-3">
            <SectionLabel>跟进管理</SectionLabel>
            <div className="grid gap-4 sm:grid-cols-2">
              <div className="space-y-1.5">
                <Label>
                  客户状态 <span className="text-destructive">*</span>
                </Label>
                <Select
                  value={status}
                  onValueChange={(v) => setStatus(v as CustomerStatus)}
                >
                  <SelectTrigger className="w-full">
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    {STATUS_ORDER.map((s) => (
                      <SelectItem key={s} value={s}>
                        {CUSTOMER_STATUS_META[s].label}
                      </SelectItem>
                    ))}
                  </SelectContent>
                </Select>
              </div>
              <div className="space-y-1.5">
                <Label htmlFor="customer-source">来源</Label>
                <Input
                  id="customer-source"
                  placeholder="如:展会 / 推荐 / 广告"
                  value={source}
                  onChange={(e) => setSource(e.target.value)}
                />
              </div>
            </div>
            <div className="space-y-1.5">
              <Label htmlFor="customer-tags">标签</Label>
              <div className="flex gap-2">
                <Input
                  id="customer-tags"
                  placeholder="输入后回车添加,可添加多个"
                  value={tagInput}
                  onChange={(e) => setTagInput(e.target.value)}
                  onKeyDown={handleTagKeyDown}
                />
                <Button
                  type="button"
                  variant="outline"
                  className="shrink-0"
                  onClick={addTag}
                >
                  添加
                </Button>
              </div>
              {tags.length > 0 && (
                <div className="flex flex-wrap gap-1.5 pt-1">
                  {tags.map((tag) => (
                    <span
                      key={tag}
                      className="inline-flex items-center gap-1 rounded-md bg-muted px-2 py-0.5 text-xs text-foreground"
                    >
                      {tag}
                      <button
                        type="button"
                        className="text-muted-foreground hover:text-foreground"
                        onClick={() =>
                          setTags(tags.filter((t) => t !== tag))
                        }
                      >
                        <X className="size-3" />
                      </button>
                    </span>
                  ))}
                </div>
              )}
            </div>
            <div className="space-y-1.5">
              <Label>跟踪人</Label>
              <Input value={owner} readOnly className="bg-muted/40" />
              <p className="text-xs text-muted-foreground">
                默认关联创建者,暂不支持修改
              </p>
            </div>
          </div>

          <div className="space-y-3">
            <SectionLabel>备注</SectionLabel>
            <Textarea
              className="min-h-24"
              placeholder="跟进记录、需求要点等"
              value={remark}
              onChange={(e) => setRemark(e.target.value)}
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
