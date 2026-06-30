import { type ReactNode, useEffect, useState } from "react";
import {
  type ColumnDef,
  type ColumnFiltersState,
  type ColumnOrderState,
  type ColumnSizingState,
  type FilterFn,
  type RowData,
  type RowSelectionState,
  type SortingState,
  type Table as TanstackTable,
  type VisibilityState,
  flexRender,
  getCoreRowModel,
  getFacetedRowModel,
  getFacetedUniqueValues,
  getFilteredRowModel,
  getPaginationRowModel,
  getSortedRowModel,
  useReactTable,
} from "@tanstack/react-table";
import { GripVertical, RotateCcw, SlidersHorizontal } from "lucide-react";
import { cn } from "@/lib/utils";
import { Card, CardContent, CardFooter } from "@/components/ui/card";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import { Input } from "@/components/ui/input";
import {
  Popover,
  PopoverContent,
  PopoverTrigger,
} from "@/components/ui/popover";
import { DataTablePagination } from "@/components/DataTablePagination";

// 列定义可通过 meta 指定:表头 / 单元格额外类名(单列调内边距),以及列设置面板里显示的列名 title
declare module "@tanstack/react-table" {
  // eslint-disable-next-line @typescript-eslint/no-unused-vars
  interface ColumnMeta<TData extends RowData, TValue> {
    headerClassName?: string;
    cellClassName?: string;
    title?: string;
  }
}

// 列自定义持久化结构(localStorage):列显隐 / 列序 / 列宽
type TableCustomizeState = {
  visibility?: VisibilityState;
  order?: ColumnOrderState;
  sizing?: ColumnSizingState;
};

function loadCustomize(key?: string): TableCustomizeState | null {
  if (!key) return null;
  try {
    const raw = localStorage.getItem(key);
    return raw ? (JSON.parse(raw) as TableCustomizeState) : null;
  } catch {
    // 存储损坏 / 不可用时退回默认,不影响表格使用
    return null;
  }
}

// 通用数据表(基于 TanStack Table):整页高度适配 + 表头吸顶 + 排序/筛选/行选择/分页。
// 工具栏通过 renderToolbar 拿到 table 实例,自行渲染搜索/筛选/操作。
interface DataTableProps<TData, TValue> {
  columns: ColumnDef<TData, TValue>[];
  data: TData[];
  itemLabel?: string;
  globalFilterFn?: FilterFn<TData>;
  getRowId?: (row: TData, index: number) => string;
  renderToolbar?: (table: TanstackTable<TData>) => ReactNode;
  emptyState?: ReactNode;
  /// 每页默认条数(不传默认 50)
  defaultPageSize?: number;
  // 列自定义(显隐 / 列序 / 列宽)+ localStorage 持久化:传入唯一 key 才启用,仅个别列表用。
  // 启用后表头右上出现「列设置」,并支持拖拽表头边缘调宽(软调:auto 布局下作宽度提示)。
  customizeKey?: string;
}

export function DataTable<TData, TValue>({
  columns,
  data,
  itemLabel,
  globalFilterFn,
  getRowId,
  renderToolbar,
  emptyState,
  defaultPageSize = 50,
  customizeKey,
}: DataTableProps<TData, TValue>) {
  const customizable = !!customizeKey;
  const [sorting, setSorting] = useState<SortingState>([]);
  const [columnFilters, setColumnFilters] = useState<ColumnFiltersState>([]);
  const [globalFilter, setGlobalFilter] = useState("");
  const [rowSelection, setRowSelection] = useState<RowSelectionState>({});
  // 列自定义三态:初始从 localStorage 恢复(仅 customizeKey 存在时)
  const [columnVisibility, setColumnVisibility] = useState<VisibilityState>(
    () => loadCustomize(customizeKey)?.visibility ?? {},
  );
  const [columnOrder, setColumnOrder] = useState<ColumnOrderState>(
    () => loadCustomize(customizeKey)?.order ?? [],
  );
  const [columnSizing, setColumnSizing] = useState<ColumnSizingState>(
    () => loadCustomize(customizeKey)?.sizing ?? {},
  );

  // 列自定义变更即持久化(防抖意义不大,数据量小直接写)
  useEffect(() => {
    if (!customizeKey) return;
    try {
      localStorage.setItem(
        customizeKey,
        JSON.stringify({
          visibility: columnVisibility,
          order: columnOrder,
          sizing: columnSizing,
        }),
      );
    } catch {
      // 写入失败(隐私模式 / 配额)忽略,不阻塞交互
    }
  }, [customizeKey, columnVisibility, columnOrder, columnSizing]);

  const table = useReactTable({
    data,
    columns,
    state: {
      sorting,
      columnFilters,
      globalFilter,
      rowSelection,
      columnVisibility,
      columnOrder,
      columnSizing,
    },
    globalFilterFn,
    getRowId,
    enableRowSelection: true,
    enableColumnResizing: customizable,
    columnResizeMode: "onChange",
    // 列宽下限,避免拖到 0;表头/单元格按 getSize() 取宽(fixed 布局下权威生效)
    defaultColumn: { minSize: 48 },
    onSortingChange: setSorting,
    onColumnFiltersChange: setColumnFilters,
    onGlobalFilterChange: setGlobalFilter,
    onRowSelectionChange: setRowSelection,
    onColumnVisibilityChange: setColumnVisibility,
    onColumnOrderChange: setColumnOrder,
    onColumnSizingChange: setColumnSizing,
    getCoreRowModel: getCoreRowModel(),
    getSortedRowModel: getSortedRowModel(),
    getFilteredRowModel: getFilteredRowModel(),
    getPaginationRowModel: getPaginationRowModel(),
    getFacetedRowModel: getFacetedRowModel(),
    getFacetedUniqueValues: getFacetedUniqueValues(),
    initialState: { pagination: { pageSize: defaultPageSize } },
  });

  // 未做过列宽设置时保持 auto 布局(列宽随内容自适应,即默认观感);
  // 一旦用户设过任一列宽,才切到 table-fixed 让设定的宽度权威生效。
  const useFixed = customizable && Object.keys(columnSizing).length > 0;

  return (
    <div className="flex min-h-0 w-full min-w-0 flex-1 flex-col gap-4">
      {customizable ? (
        <div className="flex items-center gap-2">
          <div className="min-w-0 flex-1">{renderToolbar?.(table)}</div>
          <ColumnSettings table={table} />
        </div>
      ) : (
        renderToolbar?.(table)
      )}

      <Card className="flex min-h-0 flex-1 flex-col gap-0 overflow-hidden py-0">
        {/* 不在此层滚动:滚动交给 Table 内的 table-container,thead 的 sticky 才能吸顶 */}
        <CardContent className="min-h-0 flex-1 overflow-hidden px-0">
          {/* 默认 min-w-max 让表宽随列内容扩展;列自定义时改 table-fixed + 显式表宽,
              让拖拽列宽权威生效(content 不再反向撑列),table-container 的 overflow-auto 提供横向滚动 */}
          <Table
            className={useFixed ? "table-fixed" : "min-w-max"}
            style={useFixed ? { width: table.getTotalSize() } : undefined}
          >
            {/* sticky 下放到每个 th(而非 thead):避免 sticky 嵌套导致最后一列横向 sticky 失效。
                普通列只吸顶(top-0),最后一列同时吸顶+吸右(top-0 right-0),角落 z 最高 */}
            <TableHeader className="[&_th]:font-semibold [&_th]:text-muted-foreground">
              {table.getHeaderGroups().map((headerGroup) => (
                <TableRow key={headerGroup.id}>
                  {headerGroup.headers.map((header, idx) => {
                    const isLast = idx === headerGroup.headers.length - 1;
                    return (
                      <TableHead
                        key={header.id}
                        style={
                          useFixed ? { width: header.getSize() } : undefined
                        }
                        className={cn(
                          isLast
                            ? "sticky right-0 top-0 z-20 bg-muted pr-6 shadow-[-4px_0_8px_-4px_rgba(0,0,0,0.06)]"
                            : "sticky top-0 z-10 bg-muted",
                          header.column.columnDef.meta?.headerClassName,
                        )}
                      >
                        {header.isPlaceholder
                          ? null
                          : flexRender(
                              header.column.columnDef.header,
                              header.getContext(),
                            )}
                        {/* 列宽拖拽手柄:拖动表头右缘调宽 */}
                        {customizable && header.column.getCanResize() && (
                          <div
                            onMouseDown={header.getResizeHandler()}
                            onTouchStart={header.getResizeHandler()}
                            onClick={(e) => e.stopPropagation()}
                            className={cn(
                              "absolute right-0 top-0 z-10 h-full w-1.5 cursor-col-resize touch-none select-none",
                              header.column.getIsResizing()
                                ? "bg-primary"
                                : "bg-transparent hover:bg-primary/40",
                            )}
                          />
                        )}
                      </TableHead>
                    );
                  })}
                </TableRow>
              ))}
            </TableHeader>
            <TableBody>
              {table.getRowModel().rows.length === 0 ? (
                <TableRow className="hover:bg-transparent">
                  <TableCell colSpan={columns.length}>
                    {emptyState ?? (
                      <div className="py-12 text-center text-sm text-muted-foreground">
                        暂无数据
                      </div>
                    )}
                  </TableCell>
                </TableRow>
              ) : (
                table.getRowModel().rows.map((row) => (
                  <TableRow
                    key={row.id}
                    data-state={row.getIsSelected() ? "selected" : undefined}
                    // 行分割线:虚线;主题 --border 偏淡,用 foreground/15 让虚线更清晰
                    className="border-b border-dashed border-foreground/15"
                  >
                    {row.getVisibleCells().map((cell, idx, arr) => {
                      const isLast = idx === arr.length - 1;
                      return (
                        <TableCell
                          key={cell.id}
                          style={
                            useFixed
                              ? { width: cell.column.getSize() }
                              : undefined
                          }
                          className={cn(
                            isLast
                              ? "sticky right-0 z-10 bg-card pr-6 shadow-[-4px_0_8px_-4px_rgba(0,0,0,0.06)]"
                              : "",
                            cell.column.columnDef.meta?.cellClassName,
                          )}
                        >
                          {flexRender(
                            cell.column.columnDef.cell,
                            cell.getContext(),
                          )}
                        </TableCell>
                      );
                    })}
                  </TableRow>
                ))
              )}
            </TableBody>
          </Table>
        </CardContent>
        <CardFooter className="border-t py-3">
          <DataTablePagination table={table} itemLabel={itemLabel} />
        </CardFooter>
      </Card>
    </div>
  );
}

// 列设置面板:勾选显隐 + 拖拽重排(原生 HTML5 拖放)+ 重置。
// 仅列出可隐藏列(enableHiding!==false),固定列(如「执行」「操作」)不参与显隐 / 重排,始终居两端。
function ColumnSettings<TData>({ table }: { table: TanstackTable<TData> }) {
  const [dragId, setDragId] = useState<string | null>(null);
  const [overId, setOverId] = useState<string | null>(null);

  const allLeaf = table.getAllLeafColumns();
  const savedOrder = table.getState().columnOrder;
  // getAllLeafColumns 是定义顺序,不含 columnOrder;按当前列序重排(含隐藏列)才能正确显示与重排
  const orderedIds = savedOrder.length ? savedOrder : allLeaf.map((c) => c.id);
  const byId = new Map(allLeaf.map((c) => [c.id, c] as const));
  const ordered = orderedIds
    .map((id) => byId.get(id))
    .filter((c): c is (typeof allLeaf)[number] => !!c);
  const manageable = ordered.filter((c) => c.getCanHide());
  if (manageable.length === 0) return null;
  const manageableIds = manageable.map((c) => c.id);
  // 可重排列两端的固定列(执行 / 操作),重排时保持不动
  const headFixed = orderedIds.slice(0, orderedIds.indexOf(manageableIds[0]));
  const tailFixed = orderedIds.slice(
    orderedIds.indexOf(manageableIds[manageableIds.length - 1]) + 1,
  );

  function reorder(fromId: string, toId: string) {
    if (fromId === toId) return;
    const next = [...manageableIds];
    const from = next.indexOf(fromId);
    const to = next.indexOf(toId);
    if (from < 0 || to < 0) return;
    next.splice(to, 0, next.splice(from, 1)[0]);
    table.setColumnOrder([...headFixed, ...next, ...tailFixed]);
  }

  return (
    <Popover>
      <PopoverTrigger asChild>
        <Button
          variant="outline"
          size="sm"
          className="shrink-0 cursor-pointer"
        >
          <SlidersHorizontal />
          列设置
        </Button>
      </PopoverTrigger>
      <PopoverContent align="end" className="w-72 p-2">
        <div className="mb-1.5 flex items-center justify-between px-1">
          <span className="text-xs text-muted-foreground">
            显隐 · 拖动排序 · 宽度(px)
          </span>
          <Button
            variant="ghost"
            size="xs"
            className="cursor-pointer text-muted-foreground"
            onClick={() => {
              table.setColumnVisibility({});
              table.setColumnOrder([]);
              table.resetColumnSizing();
            }}
          >
            <RotateCcw />
            重置
          </Button>
        </div>
        <div className="space-y-0.5">
          {manageable.map((col) => (
            <div
              key={col.id}
              onDragOver={(e) => {
                e.preventDefault();
                setOverId(col.id);
              }}
              onDrop={() => {
                if (dragId) reorder(dragId, col.id);
                setDragId(null);
                setOverId(null);
              }}
              onDragEnd={() => {
                setDragId(null);
                setOverId(null);
              }}
              className={cn(
                "flex items-center gap-1.5 rounded-md px-1 py-1",
                dragId === col.id && "opacity-50",
                overId === col.id && dragId && dragId !== col.id
                  ? "bg-accent"
                  : "hover:bg-accent/50",
              )}
            >
              {/* 仅手柄可拖拽,避免点勾选框 / 宽度输入误触发拖动 */}
              <span
                draggable
                onDragStart={() => setDragId(col.id)}
                title="拖动排序"
                className="shrink-0 cursor-grab active:cursor-grabbing"
              >
                <GripVertical className="size-3.5 text-muted-foreground" />
              </span>
              <Checkbox
                checked={col.getIsVisible()}
                onCheckedChange={(v) => col.toggleVisibility(!!v)}
                className="cursor-pointer"
              />
              <span className="flex-1 truncate text-sm">
                {col.columnDef.meta?.title ?? col.id}
              </span>
              {/* 列宽数值:未自定义时显示默认宽,可直接改数(回车 / 拖拽 / 此处三者同步) */}
              {col.getCanResize() && (
                <Input
                  type="number"
                  min={48}
                  value={Math.round(
                    table.getState().columnSizing[col.id] ?? col.getSize(),
                  )}
                  onChange={(e) => {
                    const v = Number(e.target.value);
                    if (!Number.isFinite(v) || v <= 0) return;
                    table.setColumnSizing((prev) => ({ ...prev, [col.id]: v }));
                  }}
                  className="h-6 w-14 shrink-0 rounded px-1 text-right text-xs tabular-nums md:text-xs [appearance:textfield] [&::-webkit-inner-spin-button]:appearance-none [&::-webkit-outer-spin-button]:appearance-none"
                />
              )}
            </div>
          ))}
        </div>
      </PopoverContent>
    </Popover>
  );
}
