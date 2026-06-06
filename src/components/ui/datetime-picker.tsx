"use client";

// 日期 + 时间组合选择器:Popover 触发,内含 Calendar(选日期)+ HH:mm 时间输入。
// 维护一个 Date 值,父组件不关心拆分;为空表示未选择。

import { format } from "date-fns";
import { CalendarIcon } from "lucide-react";

import { Button } from "@/components/ui/button";
import { Calendar } from "@/components/ui/calendar";
import { Input } from "@/components/ui/input";
import {
  Popover,
  PopoverContent,
  PopoverTrigger,
} from "@/components/ui/popover";
import { cn } from "@/lib/utils";

interface DateTimePickerProps {
  value?: Date;
  onChange: (date: Date | undefined) => void;
  placeholder?: string;
  disabled?: boolean;
  id?: string;
  className?: string;
  /// 禁选今天之前的日期(用于定时任务,不允许选过去时间)
  disablePast?: boolean;
}

export function DateTimePicker({
  value,
  onChange,
  placeholder = "选择日期与时间",
  disabled,
  id,
  className,
  disablePast = false,
}: DateTimePickerProps) {
  // 时间输入显示值:value 存在则同步,否则空
  const timeStr = value ? format(value, "HH:mm") : "";

  function applyDate(next: Date | undefined) {
    if (!next) {
      onChange(undefined);
      return;
    }
    // 选日期时:保留原时间(若有),否则取当前时间
    const base = value ?? new Date();
    next.setHours(base.getHours(), base.getMinutes(), 0, 0);
    onChange(next);
  }

  function applyTime(str: string) {
    if (!str) return;
    const [hh, mm] = str.split(":").map((s) => parseInt(s, 10));
    if (Number.isNaN(hh) || Number.isNaN(mm)) return;
    const base = value ? new Date(value) : new Date();
    base.setHours(hh, mm, 0, 0);
    onChange(base);
  }

  const todayStart = (() => {
    const d = new Date();
    d.setHours(0, 0, 0, 0);
    return d;
  })();

  return (
    <Popover>
      <PopoverTrigger asChild>
        <Button
          id={id}
          type="button"
          variant="outline"
          disabled={disabled}
          className={cn(
            "w-full justify-start font-normal cursor-pointer",
            !value && "text-muted-foreground",
            className,
          )}
        >
          <CalendarIcon className="size-4" />
          {value ? format(value, "yyyy-MM-dd HH:mm") : placeholder}
        </Button>
      </PopoverTrigger>
      <PopoverContent className="w-auto p-0" align="start">
        <Calendar
          mode="single"
          selected={value}
          onSelect={applyDate}
          disabled={disablePast ? { before: todayStart } : undefined}
        />
        <div className="border-t p-3">
          <label className="mb-1.5 block text-xs text-muted-foreground">
            时间
          </label>
          <Input
            type="time"
            value={timeStr}
            onChange={(e) => applyTime(e.target.value)}
            disabled={!value}
            className="w-full"
          />
        </div>
      </PopoverContent>
    </Popover>
  );
}
