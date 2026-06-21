import { type Table } from "@tanstack/react-table";
import { Pagination, DEFAULT_PAGE_SIZE_OPTIONS } from "@/components/Pagination";

// 表格分页栏:把 @tanstack/react-table 的分页状态适配为通用 Pagination 的受控属性,
// 渲染交给 Pagination,避免与非表格列表的分页样式各写一套。
interface DataTablePaginationProps<TData> {
  table: Table<TData>;
  pageSizeOptions?: number[];
  itemLabel?: string; // 计数单位,如 "用户"
}

export function DataTablePagination<TData>({
  table,
  pageSizeOptions = DEFAULT_PAGE_SIZE_OPTIONS,
  itemLabel = "行",
}: DataTablePaginationProps<TData>) {
  const { pageIndex, pageSize } = table.getState().pagination;

  return (
    <Pagination
      pageIndex={pageIndex}
      pageCount={table.getPageCount()}
      onPageChange={(i) => table.setPageIndex(i)}
      totalCount={table.getFilteredRowModel().rows.length}
      itemLabel={itemLabel}
      pageSize={pageSize}
      pageSizeOptions={pageSizeOptions}
      onPageSizeChange={(size) => table.setPageSize(size)}
    />
  );
}
