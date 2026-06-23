// 对话记录管理页:侧栏「最近对话 → 查看更多」进入。支持搜索、活跃/归档分栏、
// 批量选中删除、单条重命名/归档/删除。数据来自 ChatProvider,操作后 reload 同步侧栏。
import { useEffect, useMemo, useState } from "react";
import {
  Archive,
  ArchiveRestore,
  CalendarDays,
  MessageSquare,
  MoreHorizontal,
  Search,
  SquarePen,
  Trash2,
  X,
} from "lucide-react";
import { type DateRange } from "react-day-picker";
import { toast } from "sonner";
import { api, type ConversationView } from "@/lib/api";
import type { PageKey } from "@/components/app-sidebar";
import { useChat } from "@/hooks/use-chat";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Calendar } from "@/components/ui/calendar";
import {
  Popover,
  PopoverContent,
  PopoverTrigger,
} from "@/components/ui/popover";
import { Checkbox } from "@/components/ui/checkbox";
import { Tabs, TabsList, TabsTrigger } from "@/components/ui/tabs";
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
import {
  Dialog,
  DialogContent,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { Pagination } from "@/components/Pagination";

function formatTime(ts: number): string {
  const d = new Date(ts * 1000);
  const p = (n: number) => String(n).padStart(2, "0");
  return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())} ${p(d.getHours())}:${p(d.getMinutes())}`;
}

function fmtMd(d: Date): string {
  const p = (n: number) => String(n).padStart(2, "0");
  return `${p(d.getMonth() + 1)}-${p(d.getDate())}`;
}

export function ConversationsPage({
  onNavigate,
}: {
  onNavigate: (key: PageKey) => void;
}) {
  const { conversations, setActiveId, reload } = useChat();
  const [tab, setTab] = useState<"active" | "archived">("active");
  const [query, setQuery] = useState("");
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [renameTarget, setRenameTarget] = useState<ConversationView | null>(
    null,
  );
  const [renameValue, setRenameValue] = useState("");
  const [deleteTarget, setDeleteTarget] = useState<ConversationView | null>(
    null,
  );
  const [batchDeleteOpen, setBatchDeleteOpen] = useState(false);
  const [range, setRange] = useState<DateRange | undefined>();
  const [page, setPage] = useState(1);
  const [pageSize, setPageSize] = useState(50);

  // 当前分栏 + 搜索 + 日期区间过滤后的列表(按更新时间倒序)
  const list = useMemo(() => {
    const q = query.trim().toLowerCase();
    // 按「更新时间」落在所选日期区间内过滤;只选一天则取当天 0 点~23:59
    const fromTs = range?.from
      ? Math.floor(new Date(range.from).setHours(0, 0, 0, 0) / 1000)
      : undefined;
    const endDate = range?.to ?? range?.from;
    const toTs = endDate
      ? Math.floor(new Date(endDate).setHours(23, 59, 59, 999) / 1000)
      : undefined;
    return conversations
      .filter((c) => (tab === "archived" ? c.archived : !c.archived))
      .filter((c) => !q || c.title.toLowerCase().includes(q))
      .filter((c) => fromTs === undefined || c.updatedAt >= fromTs)
      .filter((c) => toTs === undefined || c.updatedAt <= toTs)
      .sort((a, b) => b.updatedAt - a.updatedAt);
  }, [conversations, tab, query, range]);

  // 选择集仅对当前列表有效:切换分栏 / 搜索变化时,剔除已不在列表中的选中项
  useEffect(() => {
    setSelected((prev) => {
      const ids = new Set(list.map((c) => c.id));
      const next = new Set([...prev].filter((id) => ids.has(id)));
      return next.size === prev.size ? prev : next;
    });
  }, [list]);

  // 过滤条件(分栏 / 搜索 / 日期)变化时回到第一页
  useEffect(() => {
    setPage(1);
  }, [tab, query, range]);

  // 前端分页:总页数 + 当前页(防越界)+ 本页切片
  const pageCount = Math.max(1, Math.ceil(list.length / pageSize));
  const currentPage = Math.min(page, pageCount);
  const pageItems = list.slice(
    (currentPage - 1) * pageSize,
    currentPage * pageSize,
  );

  const selectedCount = list.filter((c) => selected.has(c.id)).length;
  // 「全选」只针对当前页可见项(pageItems):表头复选框就在本页之上,
  // 若勾选跨页全部,用户会误删翻不到的会话(批量删除不可恢复)。跨页累计选择仍可逐行勾选。
  const pageSelectedCount = pageItems.filter((c) => selected.has(c.id)).length;
  const allPageSelected =
    pageItems.length > 0 && pageSelectedCount === pageItems.length;
  const rangeLabel =
    range?.from && range?.to
      ? `${fmtMd(range.from)} ~ ${fmtMd(range.to)}`
      : range?.from
        ? fmtMd(range.from)
        : "全部日期";

  function toggleSelect(id: string) {
    setSelected((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  }

  function toggleSelectAll() {
    setSelected((prev) => {
      const next = new Set(prev);
      // 本页已全选 → 取消本页;否则把本页全部加入(其它页已选项保留)
      if (allPageSelected) pageItems.forEach((c) => next.delete(c.id));
      else pageItems.forEach((c) => next.add(c.id));
      return next;
    });
  }

  function openConversation(c: ConversationView) {
    setActiveId(c.id);
    onNavigate("chat-sessions");
  }

  function startRename(c: ConversationView) {
    setRenameValue(c.title);
    setRenameTarget(c);
  }

  async function submitRename() {
    if (!renameTarget) return;
    const title = renameValue.trim();
    if (!title || title === renameTarget.title) {
      setRenameTarget(null);
      return;
    }
    try {
      await api.renameConversation(renameTarget.id, title);
      setRenameTarget(null);
      await reload();
    } catch (e) {
      toast.error(`重命名失败: ${e}`);
    }
  }

  async function toggleArchive(c: ConversationView) {
    try {
      await api.archiveConversation(c.id, !c.archived);
      await reload();
      toast.success(c.archived ? "已恢复会话" : "已归档会话");
    } catch (e) {
      toast.error(`操作失败: ${e}`);
    }
  }

  async function confirmDelete() {
    if (!deleteTarget) return;
    const target = deleteTarget;
    try {
      await api.deleteConversation(target.id);
      setDeleteTarget(null);
      await reload();
      toast.success("已删除会话");
    } catch (e) {
      toast.error(`删除失败: ${e}`);
    }
  }

  async function confirmBatchDelete() {
    const ids = list.filter((c) => selected.has(c.id)).map((c) => c.id);
    let failed = 0;
    for (const id of ids) {
      try {
        await api.deleteConversation(id);
      } catch {
        failed += 1;
      }
    }
    setBatchDeleteOpen(false);
    setSelected(new Set());
    await reload();
    if (failed > 0) toast.error(`${failed} 条删除失败`);
    else toast.success(`已删除 ${ids.length} 条会话`);
  }

  // 多选归档 / 恢复(归档可逆,直接执行不再二次确认)
  async function batchArchive(archived: boolean) {
    const ids = list.filter((c) => selected.has(c.id)).map((c) => c.id);
    let failed = 0;
    for (const id of ids) {
      try {
        await api.archiveConversation(id, archived);
      } catch {
        failed += 1;
      }
    }
    setSelected(new Set());
    await reload();
    if (failed > 0) toast.error(`${failed} 条操作失败`);
    else
      toast.success(
        archived ? `已归档 ${ids.length} 条` : `已恢复 ${ids.length} 条`,
      );
  }

  return (
    <div className="flex min-h-0 flex-1 flex-col gap-3">
      {/* 工具栏:分栏 + 搜索 + 批量操作 */}
      <div className="flex flex-wrap items-center gap-3">
        <Tabs
          value={tab}
          onValueChange={(v) => {
            setTab(v as "active" | "archived");
            setSelected(new Set());
          }}
        >
          <TabsList>
            <TabsTrigger value="active">活跃</TabsTrigger>
            <TabsTrigger value="archived">归档</TabsTrigger>
          </TabsList>
        </Tabs>
        {/* 日期筛选:放在搜索框前;去掉 size=sm,高度与搜索框一致 */}
        <Popover>
          <PopoverTrigger asChild>
            <Button variant="outline" className="cursor-pointer">
              <CalendarDays className="size-4" />
              {rangeLabel}
              {range?.from && (
                <span
                  role="button"
                  tabIndex={-1}
                  onClick={(e) => {
                    e.stopPropagation();
                    setRange(undefined);
                  }}
                  className="-mr-1 ml-1 inline-flex items-center rounded p-0.5 text-muted-foreground hover:bg-muted hover:text-foreground"
                >
                  <X className="size-3.5" />
                </span>
              )}
            </Button>
          </PopoverTrigger>
          <PopoverContent className="w-auto p-0" align="start">
            <Calendar
              mode="range"
              numberOfMonths={2}
              selected={range}
              onSelect={setRange}
            />
            <div className="flex justify-end border-t p-2">
              <Button
                variant="ghost"
                size="sm"
                className="cursor-pointer"
                onClick={() => setRange(undefined)}
              >
                清除
              </Button>
            </div>
          </PopoverContent>
        </Popover>
        <div className="relative w-56">
          <Search className="pointer-events-none absolute left-2.5 top-1/2 size-4 -translate-y-1/2 text-muted-foreground" />
          <Input
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            placeholder="搜索对话标题"
            className="pl-8"
          />
        </div>
        <div className="ml-auto flex items-center gap-2">
          {selectedCount > 0 && (
            <>
              <span className="text-sm text-muted-foreground">
                已选 {selectedCount} 条
              </span>
              <Button
                variant="outline"
                size="sm"
                onClick={() => void batchArchive(tab !== "archived")}
              >
                {tab === "archived" ? (
                  <>
                    <ArchiveRestore className="size-4" />
                    批量恢复
                  </>
                ) : (
                  <>
                    <Archive className="size-4" />
                    批量归档
                  </>
                )}
              </Button>
              <Button
                variant="destructive"
                size="sm"
                onClick={() => setBatchDeleteOpen(true)}
              >
                <Trash2 className="size-4" />
                批量删除
              </Button>
            </>
          )}
        </div>
      </div>

      {/* 列表 */}
      {list.length === 0 ? (
        <div className="flex flex-1 flex-col items-center justify-center gap-2 text-muted-foreground">
          <MessageSquare className="size-8 opacity-40" />
          <span className="text-sm">
            {query.trim()
              ? "没有匹配的对话"
              : tab === "archived"
                ? "暂无归档对话"
                : "暂无活跃对话"}
          </span>
        </div>
      ) : (
        <div className="flex min-h-0 flex-1 flex-col overflow-hidden rounded-xl border border-border bg-card text-card-foreground shadow-sm">
          {/* 表头:全选 */}
          <div className="flex items-center gap-3 border-b px-3 py-2 text-xs text-muted-foreground">
            <Checkbox
              checked={allPageSelected}
              onCheckedChange={toggleSelectAll}
              aria-label="全选本页"
            />
            <span className="flex-1">标题</span>
            <span className="w-32 shrink-0 text-left">更新时间</span>
            <span className="w-7 shrink-0" />
          </div>
          <div className="veltrix-thin-scrollbar min-h-0 flex-1 overflow-y-auto">
            {pageItems.map((c) => {
              const checked = selected.has(c.id);
              return (
                <div
                  key={c.id}
                  className="group flex items-center gap-3 border-b px-3 py-2.5 last:border-b-0"
                >
                  <Checkbox
                    checked={checked}
                    onCheckedChange={() => toggleSelect(c.id)}
                    aria-label="选择"
                  />
                  <button
                    type="button"
                    onClick={() => openConversation(c)}
                    className="flex min-w-0 flex-1 items-center gap-2 text-left"
                  >
                    <span className="truncate text-sm text-foreground">
                      {c.title || "新对话"}
                    </span>
                  </button>
                  <span className="w-32 shrink-0 text-left font-mono text-xs text-muted-foreground">
                    {formatTime(c.updatedAt)}
                  </span>
                  <DropdownMenu>
                    <DropdownMenuTrigger asChild>
                      <button
                        type="button"
                        className="flex w-7 shrink-0 items-center justify-center rounded py-1 text-muted-foreground opacity-0 transition-opacity hover:bg-muted hover:text-foreground group-hover:opacity-100 data-[state=open]:opacity-100"
                      >
                        <MoreHorizontal className="size-4" />
                      </button>
                    </DropdownMenuTrigger>
                    <DropdownMenuContent align="end">
                      <DropdownMenuItem onClick={() => startRename(c)}>
                        <SquarePen className="size-4" />
                        重命名
                      </DropdownMenuItem>
                      <DropdownMenuItem onClick={() => void toggleArchive(c)}>
                        {c.archived ? (
                          <>
                            <ArchiveRestore className="size-4" />
                            取消归档
                          </>
                        ) : (
                          <>
                            <Archive className="size-4" />
                            归档
                          </>
                        )}
                      </DropdownMenuItem>
                      <DropdownMenuItem
                        variant="destructive"
                        onClick={() => setDeleteTarget(c)}
                      >
                        <Trash2 className="size-4" />
                        删除
                      </DropdownMenuItem>
                    </DropdownMenuContent>
                  </DropdownMenu>
                </div>
              );
            })}
          </div>
          {/* 分页:统一组件 */}
          <div className="border-t px-3 py-2">
            <Pagination
              pageIndex={currentPage - 1}
              pageCount={pageCount}
              onPageChange={(i) => setPage(i + 1)}
              totalCount={list.length}
              itemLabel="条"
              pageSize={pageSize}
              pageSizeOptions={[50, 100, 200]}
              onPageSizeChange={(size) => {
                setPageSize(size);
                setPage(1);
              }}
            />
          </div>
        </div>
      )}

      {/* 重命名弹框 */}
      <Dialog
        open={renameTarget !== null}
        onOpenChange={(open) => !open && setRenameTarget(null)}
      >
        <DialogContent className="sm:max-w-sm">
          <DialogHeader>
            <DialogTitle>重命名会话</DialogTitle>
          </DialogHeader>
          <Input
            autoFocus
            value={renameValue}
            onChange={(e) => setRenameValue(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter") {
                e.preventDefault();
                void submitRename();
              }
            }}
            placeholder="输入会话标题"
          />
          <DialogFooter>
            <Button variant="outline" onClick={() => setRenameTarget(null)}>
              取消
            </Button>
            <Button onClick={() => void submitRename()}>确定</Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      {/* 单条删除确认(与全局删除弹窗统一用 AlertDialog) */}
      <AlertDialog
        open={deleteTarget !== null}
        onOpenChange={(open) => !open && setDeleteTarget(null)}
      >
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>删除会话</AlertDialogTitle>
            <AlertDialogDescription>
              确定删除「{deleteTarget?.title || "新对话"}」?此操作不可恢复。
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel>取消</AlertDialogCancel>
            <AlertDialogAction
              className="bg-destructive text-white hover:bg-destructive/90"
              onClick={() => void confirmDelete()}
            >
              删除
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>

      {/* 批量删除确认 */}
      <AlertDialog open={batchDeleteOpen} onOpenChange={setBatchDeleteOpen}>
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>批量删除会话</AlertDialogTitle>
            <AlertDialogDescription>
              确定删除选中的 {selectedCount} 条会话?此操作不可恢复。
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel>取消</AlertDialogCancel>
            <AlertDialogAction
              className="bg-destructive text-white hover:bg-destructive/90"
              onClick={() => void confirmBatchDelete()}
            >
              删除
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
    </div>
  );
}
