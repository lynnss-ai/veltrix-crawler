// 思考过程折叠块:展示模型推理内容(reasoning / thinking)。
// 对话 / 编程 / 电脑 / RPA 四个 Agent 共用——历史消息默认折叠,流式生成中默认展开并显示脉冲。
import { memo, useState } from "react";
import { Brain, ChevronDown, ChevronRight, Loader2 } from "lucide-react";

export const ReasoningBlock = memo(function ReasoningBlock({
  reasoning,
  streaming,
}: {
  reasoning: string;
  streaming?: boolean;
}) {
  // 流式生成中默认展开(让用户看到思考实时发生),历史消息默认折叠不抢占正文
  const [open, setOpen] = useState(!!streaming);
  const text = reasoning.trim();
  if (!text) return null;

  return (
    <div className="mb-1.5 overflow-hidden rounded-md border border-border/60 bg-muted/20">
      <button
        type="button"
        onClick={() => setOpen((v) => !v)}
        className="flex w-full items-center gap-1.5 px-2 py-1.5 text-xs text-muted-foreground transition-colors hover:bg-muted/40"
      >
        {streaming ? (
          <Loader2 className="size-3.5 shrink-0 animate-spin" />
        ) : (
          <Brain className="size-3.5 shrink-0" />
        )}
        <span className="font-medium text-foreground">思考过程</span>
        <span className="ml-auto inline-flex items-center">
          {open ? (
            <ChevronDown className="size-3.5" />
          ) : (
            <ChevronRight className="size-3.5" />
          )}
        </span>
      </button>
      {open && (
        <div className="veltrix-thin-scrollbar max-h-72 overflow-auto whitespace-pre-wrap break-words border-t border-border/60 px-2.5 py-2 text-xs leading-relaxed text-muted-foreground">
          {text}
        </div>
      )}
    </div>
  );
});
