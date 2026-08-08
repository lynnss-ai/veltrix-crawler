"use client";

import * as React from "react";
import { format } from "date-fns";
import { ChevronLeft, ChevronRight } from "lucide-react";
import { DayPicker } from "react-day-picker";
import { zhCN } from "react-day-picker/locale";
import "react-day-picker/style.css";

import { cn } from "@/lib/utils";

export type CalendarProps = React.ComponentProps<typeof DayPicker>;

// react-day-picker v10 默认样式即 OK,这里仅微调与 shadcn 主题贴合的色板
function Calendar({ className, classNames, showOutsideDays = true, ...props }: CalendarProps) {
  return (
    <DayPicker
      // 全局中文:星期/月份用 zh-CN(周一起始),标题按中文习惯显示「2026年1月」
      locale={zhCN}
      formatters={{
        formatCaption: (month) => format(month, "yyyy年M月"),
      }}
      showOutsideDays={showOutsideDays}
      className={cn("relative p-3", className)}
      classNames={{
        months: "flex flex-col sm:flex-row gap-2",
        month: "flex flex-col gap-3",
        month_caption: "flex justify-center pt-1 relative items-center w-full",
        caption_label: "text-sm font-medium",
        nav: "absolute inset-x-3 top-3 z-10 flex items-center justify-between",
        button_previous:
          "size-7 inline-flex items-center justify-center rounded-md hover:bg-accent cursor-pointer disabled:opacity-40",
        button_next:
          "size-7 inline-flex items-center justify-center rounded-md hover:bg-accent cursor-pointer disabled:opacity-40",
        month_grid: "w-full border-collapse",
        weekdays: "flex",
        weekday: "text-muted-foreground rounded-md w-8 font-normal text-[0.8rem]",
        week: "flex w-full mt-2",
        day: "size-8 text-center text-sm p-0 relative",
        day_button:
          "size-8 inline-flex items-center justify-center rounded-md hover:bg-accent cursor-pointer aria-selected:opacity-100",
        selected:
          "[&>button]:bg-primary [&>button]:text-primary-foreground [&>button]:hover:bg-primary",
        // 区间:起止为实心圆点,中间为连续浅色条(覆盖 selected 的圆角蓝块)
        range_start: "rounded-l-md bg-accent",
        range_end: "rounded-r-md bg-accent",
        range_middle:
          "bg-accent [&>button]:!bg-transparent [&>button]:!text-accent-foreground [&>button]:!rounded-none [&>button]:hover:!bg-accent",
        today: "[&>button]:bg-accent [&>button]:text-accent-foreground",
        outside: "text-muted-foreground/40",
        disabled: "text-muted-foreground/40 [&>button]:cursor-not-allowed",
        ...classNames,
      }}
      components={{
        Chevron: ({ orientation }) =>
          orientation === "left" ? (
            <ChevronLeft className="size-4" />
          ) : (
            <ChevronRight className="size-4" />
          ),
      }}
      {...props}
    />
  );
}

export { Calendar };
