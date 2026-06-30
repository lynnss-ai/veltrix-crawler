import { CalendarDays, X } from "lucide-react";
import { type DateRange } from "react-day-picker";
import { Button } from "@/components/ui/button";
import { Calendar } from "@/components/ui/calendar";
import {
  Popover,
  PopoverContent,
  PopoverTrigger,
} from "@/components/ui/popover";

export function DateRangePicker({
  range,
  onChange,
  onReset,
}: {
  range: DateRange | undefined;
  onChange: (r: DateRange | undefined) => void;
  onReset: () => void;
}) {
  const label = range?.from
    ? range.to
      ? `${range.from.toLocaleDateString()} - ${range.to.toLocaleDateString()}`
      : range.from.toLocaleDateString()
    : "近 15 天";

  return (
    <Popover>
      <PopoverTrigger asChild>
        <Button variant="outline" size="sm" className="cursor-pointer">
          <CalendarDays className="size-4" />
          {label}
          {range?.from && (
            <span
              role="button"
              tabIndex={-1}
              onClick={(e) => {
                e.stopPropagation();
                onReset();
              }}
              className="-mr-1 ml-1 inline-flex items-center rounded p-0.5 text-muted-foreground hover:bg-muted hover:text-foreground"
            >
              <X className="size-3.5" />
            </span>
          )}
        </Button>
      </PopoverTrigger>
      <PopoverContent className="w-auto p-0" align="end">
        <Calendar
          mode="range"
          numberOfMonths={2}
          selected={range}
          onSelect={onChange}
        />
        <div className="flex justify-end border-t p-2">
          <Button
            variant="ghost"
            size="sm"
            className="cursor-pointer"
            onClick={onReset}
          >
            重置为近 15 天
          </Button>
        </div>
      </PopoverContent>
    </Popover>
  );
}
