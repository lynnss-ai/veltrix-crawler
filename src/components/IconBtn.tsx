// 操作栏小图标按钮:SimpleTooltip 包裹,支持 active 高亮。
import type { ReactNode } from "react";
import { SimpleTooltip } from "@/components/SimpleTooltip";

export function IconBtn({
  title,
  active,
  onClick,
  children,
}: {
  title: string;
  active?: boolean;
  onClick: () => void;
  children: ReactNode;
}) {
  return (
    <SimpleTooltip content={title}>
      <button
        type="button"
        aria-label={title}
        onClick={onClick}
        className={`rounded p-1 transition-colors hover:bg-accent hover:text-foreground ${
          active ? "text-primary" : ""
        }`}
      >
        {children}
      </button>
    </SimpleTooltip>
  );
}
