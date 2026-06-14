// 资产库 / 评论库共用的筛选组件:行业侧栏(带角标)+ 日期区间 + 筛选 chip。
// 与 ContentLibraryPage 内的同名组件保持一致的样式。
import { CalendarDays, ChevronLeft, Filter, X } from "lucide-react";
import { type DateRange } from "react-day-picker";
import { Button } from "@/components/ui/button";
import { Calendar } from "@/components/ui/calendar";
import {
  Popover,
  PopoverContent,
  PopoverTrigger,
} from "@/components/ui/popover";
import { SimpleTooltip } from "@/components/SimpleTooltip";
import { type IndustryView } from "@/lib/api";

// 日期格式 YYYY-MM-DD
function fmtDate(d: Date): string {
  const p = (n: number) => String(n).padStart(2, "0");
  return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())}`;
}

// 日期区间筛选:ts(Unix 秒)是否落在 [from 当天 0 点, to 当天 23:59] 内;未选起始=全部。
export function inDateRange(
  ts: number | null | undefined,
  range: DateRange | undefined,
): boolean {
  if (!range?.from) return true;
  if (!ts) return false; // 选了区间却没有时间的记录排除
  const ms = ts * 1000;
  const from = new Date(range.from).setHours(0, 0, 0, 0);
  const to = (range.to ? new Date(range.to) : new Date(range.from)).setHours(
    23,
    59,
    59,
    999,
  );
  return ms >= from && ms <= to;
}

// 筛选 chip:常规圆角矩形,选中高亮(与采集任务页平台筛选一致)
export function FilterChip({
  label,
  active,
  onClick,
}: {
  label: string;
  active: boolean;
  onClick: () => void;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      className={`cursor-pointer rounded-md border px-3 py-1 text-xs transition-colors ${
        active
          ? "border-primary bg-primary text-primary-foreground"
          : "border-border text-muted-foreground hover:bg-accent hover:text-foreground"
      }`}
    >
      {label}
    </button>
  );
}

// 左侧筛选侧栏:行业筛选(带数量角标)
export function FilterSidebar({
  industries,
  industryCounts,
  industryFilter,
  onIndustry,
  onCollapse,
}: {
  industries: IndustryView[];
  industryCounts: Record<string, number>;
  industryFilter: string;
  onIndustry: (v: string) => void;
  onCollapse: () => void;
}) {
  return (
    <div className="flex w-48 shrink-0 flex-col overflow-hidden rounded-xl border bg-card">
      <div className="flex h-10 items-center justify-between border-b px-3">
        <div className="flex items-center gap-1.5 text-sm font-medium">
          <Filter className="size-3.5 text-muted-foreground" />
          行业筛选
        </div>
        <SimpleTooltip content="收起">
          <Button
            variant="ghost"
            size="icon-xs"
            className="cursor-pointer"
            onClick={onCollapse}
          >
            <ChevronLeft />
          </Button>
        </SimpleTooltip>
      </div>
      <div className="flex-1 space-y-0.5 overflow-auto p-2">
        <IndustryFilterItem
          label="全部行业"
          count={industryCounts.__all ?? 0}
          active={industryFilter === "__all"}
          onClick={() => onIndustry("__all")}
        />
        {industries.map((ind) => (
          <IndustryFilterItem
            key={ind.id}
            label={ind.name}
            count={industryCounts[ind.name] ?? 0}
            active={industryFilter === ind.name}
            onClick={() => onIndustry(ind.name)}
          />
        ))}
      </div>
    </div>
  );
}

function IndustryFilterItem({
  label,
  count,
  active,
  onClick,
}: {
  label: string;
  count: number;
  active: boolean;
  onClick: () => void;
}) {
  return (
    <div
      onClick={onClick}
      className={`flex cursor-pointer items-center gap-2 rounded-md px-2 py-1.5 text-sm transition-colors ${
        active
          ? "bg-accent font-medium text-accent-foreground"
          : "hover:bg-accent/50"
      }`}
    >
      <span className="flex-1 truncate">{label}</span>
      <span className="text-xs text-muted-foreground">{count}</span>
    </div>
  );
}

// 日期区间筛选:Popover + 双月日历;未选显示字段名,选中显示「字段名 · 区间」,可清除
export function DateRangeFilter({
  title,
  value,
  onChange,
}: {
  title: string;
  value: DateRange | undefined;
  onChange: (range: DateRange | undefined) => void;
}) {
  const range = value?.from
    ? value.to
      ? `${fmtDate(value.from)} ~ ${fmtDate(value.to)}`
      : fmtDate(value.from)
    : "";
  const label = range ? `${title} · ${range}` : title;
  return (
    <Popover>
      <PopoverTrigger asChild>
        <Button variant="outline" className="h-10 cursor-pointer">
          <CalendarDays className="size-3.5" />
          {label}
          {value?.from && (
            <span
              role="button"
              tabIndex={-1}
              onClick={(e) => {
                e.stopPropagation();
                onChange(undefined);
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
          selected={value}
          onSelect={onChange}
          numberOfMonths={2}
        />
        {value?.from && (
          <div className="border-t p-2 text-right">
            <Button
              variant="ghost"
              size="sm"
              className="cursor-pointer"
              onClick={() => onChange(undefined)}
            >
              清除
            </Button>
          </div>
        )}
      </PopoverContent>
    </Popover>
  );
}
