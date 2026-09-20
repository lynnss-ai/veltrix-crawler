"use client";

// 纯日期选择器:Popover 触发,内含 Calendar(与 DateTimePicker 同模式,不带时分)。
// 值为 undefined 表示未选择。

import { format } from "date-fns";
import { zhCN } from "date-fns/locale";
import { CalendarIcon } from "lucide-react";

import { Button } from "@/components/ui/button";
import { Calendar } from "@/components/ui/calendar";
import {
  Popover,
  PopoverContent,
  PopoverTrigger,
} from "@/components/ui/popover";
import { cn } from "@/lib/utils";

interface DatePickerProps {
  value?: Date;
  onChange: (date: Date | undefined) => void;
  placeholder?: string;
  disabled?: boolean;
  id?: string;
  className?: string;
  /// 透出校验态(与 Input 的 aria-invalid 一致,红框提示)
  "aria-invalid"?: boolean;
  /// 禁选该日期之前的日期(如结束日期不早于开始日期)
  disableBefore?: Date;
  /// 禁选该日期之后的日期
  disableAfter?: Date;
}

export function DatePicker({
  value,
  onChange,
  placeholder = "选择日期",
  disabled,
  id,
  className,
  "aria-invalid": ariaInvalid,
  disableBefore,
  disableAfter,
}: DatePickerProps) {
  return (
    <Popover>
      <PopoverTrigger asChild>
        <Button
          id={id}
          type="button"
          variant="outline"
          disabled={disabled}
          aria-invalid={ariaInvalid}
          className={cn(
            "w-full justify-start font-normal cursor-pointer",
            !value && "text-muted-foreground",
            className,
          )}
        >
          <CalendarIcon className="size-4" />
          {value ? format(value, "yyyy-MM-dd") : placeholder}
        </Button>
      </PopoverTrigger>
      <PopoverContent className="w-auto p-0" align="start">
        <Calendar
          mode="single"
          selected={value}
          onSelect={onChange}
          locale={zhCN}
          defaultMonth={value}
          disabled={[
            ...(disableBefore ? [{ before: disableBefore }] : []),
            ...(disableAfter ? [{ after: disableAfter }] : []),
          ]}
        />
      </PopoverContent>
    </Popover>
  );
}
