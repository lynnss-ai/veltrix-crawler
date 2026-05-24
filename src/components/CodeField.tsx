import { useState } from "react";
import { Check, Copy, RefreshCw } from "lucide-react";
import { toast } from "sonner";
import { Input } from "@/components/ui/input";
import { Button } from "@/components/ui/button";

// 编码字符集(去掉易混淆的 0/O/1/I/L)与长度
const CODE_CHARS = "ABCDEFGHJKMNPQRSTUVWXYZ23456789";
const CODE_LENGTH = 6;

// 生成带前缀的编码,如 PRV-7K3P9Q
export function generateCode(prefix: string): string {
  let suffix = "";
  for (let i = 0; i < CODE_LENGTH; i += 1) {
    suffix += CODE_CHARS[Math.floor(Math.random() * CODE_CHARS.length)];
  }
  return `${prefix}-${suffix}`;
}

// 只读编码输入框 + 刷新(重新生成) + 复制。供行业/提示词/厂商等编码字段复用。
export function CodeField({
  id,
  value,
  onRegenerate,
}: {
  id?: string;
  value: string;
  onRegenerate: () => void;
}) {
  const [copied, setCopied] = useState(false);

  async function handleCopy() {
    try {
      await navigator.clipboard.writeText(value);
      setCopied(true);
      setTimeout(() => setCopied(false), 1500);
      toast.success(`已复制:${value}`);
    } catch {
      toast.error("复制失败,请手动复制");
    }
  }

  return (
    <div className="flex items-center gap-2">
      <Input id={id} value={value} readOnly className="font-mono" />
      <Button
        type="button"
        variant="outline"
        size="icon"
        className="shrink-0"
        title="重新生成"
        onClick={onRegenerate}
      >
        <RefreshCw />
        <span className="sr-only">重新生成</span>
      </Button>
      <Button
        type="button"
        variant="outline"
        size="icon"
        className="shrink-0"
        title={copied ? "已复制" : "复制"}
        onClick={handleCopy}
      >
        {copied ? <Check /> : <Copy />}
        <span className="sr-only">复制</span>
      </Button>
    </div>
  );
}
