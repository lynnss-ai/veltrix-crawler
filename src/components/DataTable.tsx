import { type ReactNode, useState } from "react";
import {
  type ColumnDef,
  type ColumnFiltersState,
  type FilterFn,
  type RowSelectionState,
  type SortingState,
  type Table as TanstackTable,
  flexRender,
  getCoreRowModel,
  getFacetedRowModel,
  getFacetedUniqueValues,
  getFilteredRowModel,
  getPaginationRowModel,
  getSortedRowModel,
  useReactTable,
} from "@tanstack/react-table";
import { Card, CardContent, CardFooter } from "@/components/ui/card";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { DataTablePagination } from "@/components/DataTablePagination";

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
}: DataTableProps<TData, TValue>) {
  const [sorting, setSorting] = useState<SortingState>([]);
  const [columnFilters, setColumnFilters] = useState<ColumnFiltersState>([]);
  const [globalFilter, setGlobalFilter] = useState("");
  const [rowSelection, setRowSelection] = useState<RowSelectionState>({});

  const table = useReactTable({
    data,
    columns,
    state: { sorting, columnFilters, globalFilter, rowSelection },
    globalFilterFn,
    getRowId,
    enableRowSelection: true,
    onSortingChange: setSorting,
    onColumnFiltersChange: setColumnFilters,
    onGlobalFilterChange: setGlobalFilter,
    onRowSelectionChange: setRowSelection,
    getCoreRowModel: getCoreRowModel(),
    getSortedRowModel: getSortedRowModel(),
    getFilteredRowModel: getFilteredRowModel(),
    getPaginationRowModel: getPaginationRowModel(),
    getFacetedRowModel: getFacetedRowModel(),
    getFacetedUniqueValues: getFacetedUniqueValues(),
    initialState: { pagination: { pageSize: defaultPageSize } },
  });

  return (
    <div className="flex min-h-0 w-full min-w-0 flex-1 flex-col gap-4">
      {renderToolbar?.(table)}

      <Card className="flex min-h-0 flex-1 flex-col gap-0 overflow-hidden py-0">
        {/* 不在此层滚动:滚动交给 Table 内的 table-container,thead 的 sticky 才能吸顶 */}
        <CardContent className="min-h-0 flex-1 overflow-hidden px-0">
          {/* min-w-max 让表宽随列内容扩展,table-container 的 overflow-auto 提供横向/纵向滚动 */}
          <Table className="min-w-max">
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
                        className={
                          isLast
                            ? "sticky right-0 top-0 z-20 bg-muted pr-6 shadow-[-4px_0_8px_-4px_rgba(0,0,0,0.06)]"
                            : "sticky top-0 z-10 bg-muted"
                        }
                      >
                        {header.isPlaceholder
                          ? null
                          : flexRender(
                              header.column.columnDef.header,
                              header.getContext(),
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
                          className={
                            isLast
                              ? "sticky right-0 z-10 bg-card pr-6 shadow-[-4px_0_8px_-4px_rgba(0,0,0,0.06)]"
                              : ""
                          }
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
