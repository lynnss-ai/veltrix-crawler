import { useState } from "react";
import { RefreshCw } from "lucide-react";
import { Button } from "@/components/ui/button";
import { SimpleTooltip } from "@/components/SimpleTooltip";

// 各数据表格页工具栏统一的「刷新」按钮。
// 内部自管 loading,避免每个调用方各自维护重复的加载态;
// 用 try/finally 兜底是因为 reload 可能抛错(如接口失败),仍需恢复按钮可点与停止转圈。
export function RefreshButton({
  onClick,
  disabled,
}: {
  onClick: () => void | Promise<void>;
  disabled?: boolean;
}) {
  const [isLoading, setIsLoading] = useState(false);

  async function handleClick() {
    setIsLoading(true);
    try {
      await onClick();
    } finally {
      setIsLoading(false);
    }
  }

  return (
    <SimpleTooltip content="刷新">
      <Button
        variant="outline"
        size="icon"
        disabled={disabled || isLoading}
        onClick={handleClick}
      >
        <RefreshCw className={isLoading ? "animate-spin" : undefined} />
      </Button>
    </SimpleTooltip>
  );
}
