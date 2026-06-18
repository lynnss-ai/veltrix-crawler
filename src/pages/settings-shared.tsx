// SettingsPage 各 section 共用的展示型小组件:卡片容器(含保存按钮)/ 必填标记 / 标签行。
import { Button } from "@/components/ui/button";
import { Card, CardAction, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import type { ReactNode } from "react";

export function SettingsCard({
  title,
  description,
  children,
  onSave,
  dirty,
}: {
  title: string;
  description?: string;
  children: ReactNode;
  onSave?: () => void;
  dirty?: boolean;
}) {
  return (
    <Card>
      <CardHeader>
        <CardTitle>{title}</CardTitle>
        {description && <CardDescription>{description}</CardDescription>}
        {onSave && (
          <CardAction className="self-center">
            <Button disabled={!dirty} onClick={onSave}>
              保存
            </Button>
          </CardAction>
        )}
      </CardHeader>
      <CardContent className="space-y-4">{children}</CardContent>
    </Card>
  );
}

// 工作区顺序编辑:拖动调整侧栏顶部工作区切换标签的排列。
// 用指针事件(pointer events)+ pointer capture 实现,不依赖 HTML5 drag API
// (WebView2 里原生 DnD 不可靠);拖动期间用本地副本即时重排,松手才持久化一次。

export function RequiredMark() {
  return <span className="ml-0.5 text-destructive">*</span>;
}

export function Row({ label, children }: { label: string; children: ReactNode }) {
  return (
    <div className="flex gap-3 text-sm">
      <dt className="w-28 shrink-0 text-muted-foreground">{label}</dt>
      <dd className="min-w-0 flex-1 text-foreground">{children}</dd>
    </div>
  );
}

// 远程控制配置:云端中转服务地址 + 登录态 + 实时连接状态。
