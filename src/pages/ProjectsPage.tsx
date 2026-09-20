import { useEffect, useMemo, useState, type FormEvent } from "react";
import { type ColumnDef, type FilterFn } from "@tanstack/react-table";
import { format, parse } from "date-fns";
import { MoreVertical, SquarePen, Plus, Search, Trash2 } from "lucide-react";
import {
  api,
  type CustomerView,
  type ProjectInput,
  type ProjectView,
} from "@/lib/api";
import { formatTimestamp } from "@/lib/utils";
import { FORM_CONTROL_SIZING } from "@/lib/form-sizing";
import { ErrorBanner } from "@/components/ErrorBanner";
import { DataTable } from "@/components/DataTable";
import { EmptyState } from "@/components/EmptyState";
import { DataTableColumnHeader } from "@/components/DataTableColumnHeader";
import { FieldError } from "@/components/FieldError";
import { CodeField, generateCode } from "@/components/CodeField";
import { DatePicker } from "@/components/ui/date-picker";
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

// 项目信息(运营 - 客户管理):接真实后端 project 表 list/upsert/remove 命令。
// 所属客户下拉来自「客户信息」维护的真实数据;客户被删除后项目悬空,显示「未关联客户」。

type ProjectItem = ProjectView;

// 全局搜索:匹配项目名称 / 编码 / 所属客户名 / 备注
const projectFilterFn: FilterFn<ProjectItem> = (row, _columnId, value) => {
  const p = row.original;
  return `${p.name} ${p.code} ${p.remark}`
    .toLowerCase()
    .includes(String(value).toLowerCase());
};

export function ProjectsPage({ currentUser }: { currentUser: string }) {
  const [projects, setProjects] = useState<ProjectItem[]>([]);
  const [customers, setCustomers] = useState<CustomerView[]>([]);
  const [editing, setEditing] = useState<ProjectItem | null>(null);
  const [isFormOpen, setIsFormOpen] = useState(false);
  const [deleteTarget, setDeleteTarget] = useState<ProjectItem | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);

  const customerName = (id: string) =>
    customers.find((c) => c.id === id)?.name ?? "";

  async function loadProjects() {
    setLoading(true);
    try {
      setProjects(await api.listProjects());
    } catch (e) {
      setError(String(e));
    } finally {
      setLoading(false);
    }
  }

  useEffect(() => {
    loadProjects();
    api
      .listCustomers()
      .then(setCustomers)
      .catch((e) => setError(String(e)));
  }, []);

  async function submit(input: ProjectInput) {
    try {
      await api.upsertProject(input);
      setIsFormOpen(false);
      await loadProjects();
    } catch (e) {
      setError(String(e));
    }
  }

  async function confirmDelete() {
    if (!deleteTarget) return;
    try {
      await api.removeProject(deleteTarget.id);
      await loadProjects();
    } catch (e) {
      setError(String(e));
    }
    setDeleteTarget(null);
  }

  const columns = useMemo<ColumnDef<ProjectItem>[]>(
    () => [
      {
        accessorKey: "name",
        header: ({ column }) => (
          <DataTableColumnHeader column={column} title="项目名称" />
        ),
        cell: ({ row }) => (
          <span className="font-medium text-foreground">
            {row.original.name}
          </span>
        ),
      },
      {
        accessorKey: "code",
        header: "项目编码",
        cell: ({ row }) => (
          <span className="font-mono text-xs">{row.original.code}</span>
        ),
      },
      {
        accessorKey: "customerId",
        header: "所属客户",
        cell: ({ row }) => {
          const name = customerName(row.original.customerId);
          return name ? (
            name
          ) : (
            <span className="text-muted-foreground">未关联客户</span>
          );
        },
      },
      {
        accessorKey: "startDate",
        header: "开始日期",
        cell: ({ row }) => row.original.startDate || "—",
      },
      {
        accessorKey: "endDate",
        header: "结束日期",
        cell: ({ row }) => row.original.endDate || "—",
      },
      {
        accessorKey: "remark",
        header: "备注",
        enableSorting: false,
        cell: ({ row }) => (
          <span className="line-clamp-1 max-w-[220px] text-muted-foreground">
            {row.original.remark || "—"}
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
            {formatTimestamp(row.original.updatedAt)}
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
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [customers],
  );

  return (
    <div className={`flex min-h-0 flex-1 flex-col gap-2.5 ${FORM_CONTROL_SIZING}`}>
      <ErrorBanner message={error} onClose={() => setError(null)} />
      <DataTable
        columns={columns}
        data={projects}
        loading={loading}
        itemLabel="项目"
        globalFilterFn={projectFilterFn}
        getRowId={(row) => row.id}
        renderToolbar={(table) => (
          <div className="flex flex-col gap-3 sm:flex-row sm:items-center sm:justify-between">
            <div className="relative w-full sm:max-w-sm">
              <Search className="pointer-events-none absolute left-3 top-1/2 size-4 -translate-y-1/2 text-muted-foreground" />
              <Input
                placeholder="搜索项目名称 / 编码 / 备注"
                className="pl-9"
                value={(table.getState().globalFilter as string) ?? ""}
                onChange={(e) => table.setGlobalFilter(e.target.value)}
              />
            </div>
            <Button
              className="h-10"
              onClick={() => {
                setEditing(null);
                setIsFormOpen(true);
              }}
            >
              <Plus />
              新增项目
            </Button>
          </div>
        )}
        emptyState={
          <EmptyState
            title="暂无项目"
            description="点击右上角「新增项目」开始建立项目档案"
          />
        }
      />

      <Sheet open={isFormOpen} onOpenChange={setIsFormOpen}>
        <ProjectFormSheet
          key={isFormOpen ? (editing?.id ?? "new") : "idle"}
          initial={editing}
          currentUser={currentUser}
          customers={customers}
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
            <AlertDialogTitle>删除项目</AlertDialogTitle>
            <AlertDialogDescription>
              将永久删除项目「{deleteTarget?.name}」,此操作不可恢复。
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
    </div>
  );
}

// 新增 / 编辑项目(置于 Sheet 内)
function ProjectFormSheet({
  initial,
  currentUser,
  customers,
  onSubmit,
  onCancel,
}: {
  initial: ProjectItem | null;
  currentUser: string;
  customers: CustomerView[];
  onSubmit: (project: ProjectInput) => void;
  onCancel: () => void;
}) {
  const isEdit = initial !== null;
  const [code, setCode] = useState(initial?.code ?? generateCode("PRJ"));
  const [name, setName] = useState(initial?.name ?? "");
  const [customerId, setCustomerId] = useState(initial?.customerId ?? "");
  const [startDate, setStartDate] = useState(initial?.startDate ?? "");
  const [endDate, setEndDate] = useState(initial?.endDate ?? "");
  const [remark, setRemark] = useState(initial?.remark ?? "");
  const [submitted, setSubmitted] = useState(false);
  // 归属:新建关联当前用户,编辑保留原归属
  const owner = initial?.owner ?? currentUser;

  function handleSubmit(event: FormEvent) {
    event.preventDefault();
    setSubmitted(true);
    if (!name.trim() || !customerId || !startDate || !endDate) {
      return;
    }
    if (startDate > endDate) {
      return;
    }
    onSubmit({
      id: initial?.id ?? crypto.randomUUID(),
      code,
      name: name.trim(),
      customerId,
      startDate,
      endDate,
      remark: remark.trim(),
      owner,
    });
  }

  const dateOrderInvalid =
    !!startDate && !!endDate && startDate > endDate;
  return (
    <SheetContent
      className="flex w-full flex-col gap-0 p-0 sm:max-w-[600px]"
      blockClose={
        name !== (initial?.name ?? "") ||
        customerId !== (initial?.customerId ?? "") ||
        startDate !== (initial?.startDate ?? "") ||
        endDate !== (initial?.endDate ?? "") ||
        remark !== (initial?.remark ?? "")
      }
    >
      <SheetHeader className="border-b">
        <SheetTitle>{isEdit ? "编辑项目" : "新增项目"}</SheetTitle>
        <SheetDescription>
          维护项目档案,所属客户下拉来自「客户信息」。
        </SheetDescription>
      </SheetHeader>
      <form onSubmit={handleSubmit} className="flex min-h-0 flex-1 flex-col">
        <div className="flex-1 space-y-6 overflow-y-auto p-5">
          <div className="space-y-3">
            <div className="space-y-1.5">
              <Label htmlFor="project-name">
                项目名称 <span className="text-destructive">*</span>
              </Label>
              <Input
                id="project-name"
                placeholder="项目名称"
                value={name}
                onChange={(e) => setName(e.target.value)}
                aria-invalid={submitted && !name.trim()}
                autoFocus
              />
              <FieldError
                show={submitted && !name.trim()}
                message="项目名称不可为空"
              />
            </div>
            <div className="space-y-1.5">
              <Label htmlFor="project-code">项目编码</Label>
              <CodeField
                id="project-code"
                value={code}
                onRegenerate={() => setCode(generateCode("PRJ"))}
              />
              <p className="text-xs text-muted-foreground">
                系统自动生成,可刷新或复制
              </p>
            </div>
            <div className="space-y-1.5">
              <Label>
                所属客户 <span className="text-destructive">*</span>
              </Label>
              <Select value={customerId} onValueChange={setCustomerId}>
                <SelectTrigger
                  className="w-full"
                  aria-invalid={submitted && !customerId}
                >
                  <SelectValue placeholder="选择所属客户" />
                </SelectTrigger>
                <SelectContent>
                  {customers.length === 0 ? (
                    <div className="px-2 py-1.5 text-sm text-muted-foreground">
                      暂无客户,请先到「客户信息」添加
                    </div>
                  ) : (
                    customers.map((c) => (
                      <SelectItem key={c.id} value={c.id}>
                        {c.name}
                        {c.company ? `(${c.company})` : ""}
                      </SelectItem>
                    ))
                  )}
                </SelectContent>
              </Select>
              <FieldError
                show={submitted && !customerId}
                message="请选择所属客户"
              />
            </div>
            <div className="grid gap-4 sm:grid-cols-2">
              <div className="space-y-1.5">
                <Label htmlFor="project-start">
                  开始日期 <span className="text-destructive">*</span>
                </Label>
                <DatePicker
                  id="project-start"
                  value={
                    startDate
                      ? parse(startDate, "yyyy-MM-dd", new Date())
                      : undefined
                  }
                  onChange={(d) =>
                    setStartDate(d ? format(d, "yyyy-MM-dd") : "")
                  }
                  aria-invalid={submitted && (!startDate || dateOrderInvalid)}
                  // 已选结束日期时,开始日期禁选其后的日期
                  disableAfter={
                    endDate
                      ? parse(endDate, "yyyy-MM-dd", new Date())
                      : undefined
                  }
                />
                <FieldError
                  show={submitted && !startDate}
                  message="请选择开始日期"
                />
              </div>
              <div className="space-y-1.5">
                <Label htmlFor="project-end">
                  结束日期 <span className="text-destructive">*</span>
                </Label>
                <DatePicker
                  id="project-end"
                  value={
                    endDate
                      ? parse(endDate, "yyyy-MM-dd", new Date())
                      : undefined
                  }
                  onChange={(d) =>
                    setEndDate(d ? format(d, "yyyy-MM-dd") : "")
                  }
                  aria-invalid={submitted && (!endDate || dateOrderInvalid)}
                  // 结束日期禁选开始日期之前的日期
                  disableBefore={
                    startDate
                      ? parse(startDate, "yyyy-MM-dd", new Date())
                      : undefined
                  }
                />
                <FieldError
                  show={submitted && !endDate}
                  message="请选择结束日期"
                />
              </div>
            </div>
            <FieldError
              show={submitted && dateOrderInvalid}
              message="开始日期不能晚于结束日期"
            />
            <div className="space-y-1.5">
              <Label htmlFor="project-remark">备注</Label>
              <Textarea
                id="project-remark"
                placeholder="补充说明(可选)"
                rows={3}
                value={remark}
                onChange={(e) => setRemark(e.target.value)}
              />
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
    </SheetContent>
  );
}
