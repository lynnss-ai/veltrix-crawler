// 通用分页栏:不依赖 @tanstack/react-table,接受受控的 pageIndex/pageCount。
// 视觉与 DataTablePagination 一致(首/上/下/末页图标 + 页码 + 总数 + 可选每页行数),
// 供非表格的自定义列表(如对话记录)复用,保持全站分页风格统一。
import {
  ChevronLeft,
  ChevronRight,
  ChevronsLeft,
  ChevronsRight,
} from "lucide-react";
import { Button } from "@/components/ui/button";
import { Label } from "@/components/ui/label";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";

// 每页行数默认档位(全站分页共用,DataTablePagination 亦复用此常量)
export const DEFAULT_PAGE_SIZE_OPTIONS = [50, 100, 200, 500];

interface PaginationProps {
  /** 当前页(0-based) */
  pageIndex: number;
  /** 总页数 */
  pageCount: number;
  /** 翻页回调(参数为目标页 0-based) */
  onPageChange: (pageIndex: number) => void;
  /** 可选:总条数,显示「共 N 条」 */
  totalCount?: number;
  /** 计数单位,如「条」「个」 */
  itemLabel?: string;
  /** 可选:每页行数(传入则显示选择器) */
  pageSize?: number;
  pageSizeOptions?: number[];
  onPageSizeChange?: (size: number) => void;
}

export function Pagination({
  pageIndex,
  pageCount,
  onPageChange,
  totalCount,
  itemLabel = "条",
  pageSize,
  pageSizeOptions = DEFAULT_PAGE_SIZE_OPTIONS,
  onPageSizeChange,
}: PaginationProps) {
  const count = Math.max(1, pageCount);
  const canPrev = pageIndex > 0;
  const canNext = pageIndex < count - 1;
  const showPageSize = pageSize != null && onPageSizeChange != null;

  return (
    <div className="flex w-full items-center justify-between gap-4">
      <span className="hidden flex-1 text-sm text-muted-foreground sm:inline">
        {totalCount != null ? `共 ${totalCount} ${itemLabel}` : ""}
      </span>
      <div className="flex items-center gap-4 sm:gap-6 lg:gap-8">
        {showPageSize && (
          <div className="flex items-center gap-2">
            <Label htmlFor="rows-per-page" className="text-sm font-medium">
              每页行数
            </Label>
            <Select
              value={String(pageSize)}
              onValueChange={(v) => onPageSizeChange(Number(v))}
            >
              <SelectTrigger
                size="sm"
                id="rows-per-page"
                data-pagination="true"
                className="h-7 w-20"
              >
                <SelectValue />
              </SelectTrigger>
              <SelectContent side="top">
                {pageSizeOptions.map((size) => (
                  <SelectItem key={size} value={String(size)}>
                    {size}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          </div>
        )}
        <div className="flex h-7 w-fit items-center justify-center text-sm font-medium">
          第 {pageIndex + 1} / {count} 页
        </div>
        <div className="flex items-center gap-2">
          <Button
            variant="outline"
            size="icon-sm"
            className="hidden lg:flex"
            onClick={() => onPageChange(0)}
            disabled={!canPrev}
          >
            <ChevronsLeft />
            <span className="sr-only">首页</span>
          </Button>
          <Button
            variant="outline"
            size="icon-sm"
            onClick={() => onPageChange(pageIndex - 1)}
            disabled={!canPrev}
          >
            <ChevronLeft />
            <span className="sr-only">上一页</span>
          </Button>
          <Button
            variant="outline"
            size="icon-sm"
            onClick={() => onPageChange(pageIndex + 1)}
            disabled={!canNext}
          >
            <ChevronRight />
            <span className="sr-only">下一页</span>
          </Button>
          <Button
            variant="outline"
            size="icon-sm"
            className="hidden lg:flex"
            onClick={() => onPageChange(count - 1)}
            disabled={!canNext}
          >
            <ChevronsRight />
            <span className="sr-only">末页</span>
          </Button>
        </div>
      </div>
    </div>
  );
}
