// 平台管理 Sheet:平台列表 + 增删改。从独立的「平台管理」页搬到「平台账号」页,
// 以一个右侧抽屉的形式提供,账号按这里维护的平台分类。
import { useEffect, useState, type FormEvent } from "react";
import { Network, Plus, SquarePen, Trash2 } from "lucide-react";
import { toast } from "sonner";

import { api, type PlatformConfig } from "@/lib/api";
import {
  Sheet,
  SheetContent,
  SheetDescription,
  SheetFooter,
  SheetHeader,
  SheetTitle,
} from "@/components/ui/sheet";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Switch } from "@/components/ui/switch";
import { CodeField, generateCode } from "@/components/CodeField";
import { FieldError } from "@/components/FieldError";
import { SimpleTooltip } from "@/components/SimpleTooltip";
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
} from "@/components/ui/alert-dialog";

function generatePlatformId(): string {
  return `plat-${Date.now().toString(36)}${Math.random().toString(36).slice(2, 6)}`;
}

// 平台管理主抽屉:列出平台 + 添加/编辑/删除。onChanged 在增删改后通知外部(账号页)刷新平台。
export function PlatformManagerSheet({
  open,
  onOpenChange,
  onChanged,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  onChanged?: () => void;
}) {
  const [platforms, setPlatforms] = useState<PlatformConfig[]>([]);
  const [formOpen, setFormOpen] = useState(false);
  const [formInitial, setFormInitial] = useState<PlatformConfig | null>(null);
  const [deleteTarget, setDeleteTarget] = useState<PlatformConfig | null>(null);

  function load() {
    api
      .listPlatforms()
      .then(setPlatforms)
      .catch((e) => toast.error(`加载平台失败: ${e}`));
  }
  // 打开时加载最新平台
  useEffect(() => {
    if (open) load();
  }, [open]);

  function submit(platform: PlatformConfig) {
    api
      .upsertPlatform(platform)
      .then(() => {
        setFormOpen(false);
        load();
        onChanged?.();
        toast.success("平台已保存");
      })
      .catch((e) => toast.error(`保存失败: ${e}`));
  }

  // 列表内直接切换启用 / 停用,即时保存
  function toggleEnabled(platform: PlatformConfig, enabled: boolean) {
    api
      .upsertPlatform({ ...platform, enabled })
      .then(() => {
        load();
        onChanged?.();
        toast.success(enabled ? "平台已启用" : "平台已停用");
      })
      .catch((e) => toast.error(`操作失败: ${e}`));
  }

  function confirmDelete() {
    if (!deleteTarget) return;
    const target = deleteTarget;
    setDeleteTarget(null);
    api
      .removePlatform(target.id)
      .then(() => {
        load();
        onChanged?.();
        toast.success("平台已删除");
      })
      .catch((e) => toast.error(`删除失败: ${e}`));
  }

  return (
    <>
      <Sheet open={open} onOpenChange={onOpenChange}>
        <SheetContent
          side="right"
          className="flex w-full flex-col gap-0 p-0 sm:max-w-[600px]"
        >
          <SheetHeader className="border-b">
            <SheetTitle>平台管理</SheetTitle>
            <SheetDescription>
              在这里维护需要采集的平台:可新增平台、修改访问链接、启用或停用、删除。平台账号会按这里配置的平台归类管理。
            </SheetDescription>
          </SheetHeader>

          <div className="min-h-0 flex-1 overflow-y-auto p-4">
            {platforms.length === 0 ? (
              <div className="flex flex-col items-center justify-center gap-2 py-16 text-center text-muted-foreground">
                <Network className="size-8 opacity-40" />
                <p className="text-sm">还没有平台,点击底部「添加平台」新增</p>
              </div>
            ) : (
              <div className="space-y-2">
                {platforms.map((p) => (
                  <div
                    key={p.id}
                    className="flex items-center gap-3 rounded-lg border bg-card px-3.5 py-3 transition-colors hover:border-foreground/20"
                  >
                    <div className="min-w-0 flex-1">
                      <div className="truncate text-sm font-medium text-foreground">
                        {p.name}
                      </div>
                      <div className="mt-0.5 truncate text-xs text-muted-foreground">
                        {p.login_url || "未配置访问链接"}
                      </div>
                    </div>
                    <div className="flex shrink-0 items-center gap-2">
                      <SimpleTooltip
                        content={p.enabled ? "已启用,点击停用" : "已停用,点击启用"}
                      >
                        <div className="flex items-center">
                          <Switch
                            checked={p.enabled}
                            onCheckedChange={(v) => toggleEnabled(p, v)}
                          />
                        </div>
                      </SimpleTooltip>
                      <SimpleTooltip content="编辑">
                        <button
                          type="button"
                          onClick={() => {
                            setFormInitial(p);
                            setFormOpen(true);
                          }}
                          className="inline-flex size-8 cursor-pointer items-center justify-center rounded-md text-muted-foreground transition-colors hover:bg-accent hover:text-foreground"
                        >
                          <SquarePen className="size-4" />
                        </button>
                      </SimpleTooltip>
                      <SimpleTooltip content="删除">
                        <button
                          type="button"
                          onClick={() => setDeleteTarget(p)}
                          className="inline-flex size-8 cursor-pointer items-center justify-center rounded-md text-muted-foreground transition-colors hover:bg-destructive/10 hover:text-destructive"
                        >
                          <Trash2 className="size-4" />
                        </button>
                      </SimpleTooltip>
                    </div>
                  </div>
                ))}
              </div>
            )}
          </div>

          <SheetFooter className="border-t p-4">
            <Button
              type="button"
              className="w-full"
              onClick={() => {
                setFormInitial(null);
                setFormOpen(true);
              }}
            >
              <Plus className="size-4" />
              添加平台
            </Button>
          </SheetFooter>
        </SheetContent>
      </Sheet>

      <PlatformFormSheet
        key={formInitial?.id ?? "new"}
        open={formOpen}
        initial={formInitial}
        onOpenChange={setFormOpen}
        onSubmit={submit}
      />

      <AlertDialog
        open={!!deleteTarget}
        onOpenChange={(o) => !o && setDeleteTarget(null)}
      >
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>删除平台</AlertDialogTitle>
            <AlertDialogDescription>
              确定删除平台「{deleteTarget?.name}」?该平台下的账号不会被删除,但会失去平台归属。
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel>取消</AlertDialogCancel>
            <AlertDialogAction
              onClick={confirmDelete}
              className="bg-destructive text-white hover:bg-destructive/90"
            >
              删除
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
    </>
  );
}

// 平台新增 / 编辑抽屉:名称、编码、访问链接、启用;编辑时保留 collect 等透传字段。
function PlatformFormSheet({
  open,
  initial,
  onOpenChange,
  onSubmit,
}: {
  open: boolean;
  initial: PlatformConfig | null;
  onOpenChange: (open: boolean) => void;
  onSubmit: (platform: PlatformConfig) => void;
}) {
  const isEdit = initial !== null;
  const [name, setName] = useState(initial?.name ?? "");
  const [code, setCode] = useState(
    (initial?.code as string | undefined) ?? generateCode("PLT"),
  );
  const [loginUrl, setLoginUrl] = useState(initial?.login_url ?? "");
  const [enabled, setEnabled] = useState(initial?.enabled ?? true);
  const [submitted, setSubmitted] = useState(false);

  function handleSubmit(event: FormEvent) {
    event.preventDefault();
    setSubmitted(true);
    if (!name.trim() || !code.trim() || !loginUrl.trim()) {
      return;
    }
    onSubmit({
      ...(initial ?? {}),
      id: initial?.id ?? generatePlatformId(),
      code,
      name: name.trim(),
      login_url: loginUrl.trim(),
      enabled,
    } as PlatformConfig);
  }

  return (
    <Sheet open={open} onOpenChange={onOpenChange}>
      <SheetContent
        side="right"
        className="flex w-full flex-col gap-0 p-0 sm:max-w-[600px]"
      >
        <SheetHeader className="border-b">
          <SheetTitle>{isEdit ? "编辑平台" : "新增平台"}</SheetTitle>
          <SheetDescription>配置平台的基本信息与启用状态。</SheetDescription>
        </SheetHeader>
        <form onSubmit={handleSubmit} className="flex min-h-0 flex-1 flex-col">
          <div className="flex-1 space-y-4 overflow-y-auto p-5">
            <div className="space-y-1.5">
              <Label htmlFor="pm-name">
                平台名称 <span className="text-destructive">*</span>
              </Label>
              <Input
                id="pm-name"
                placeholder="如:抖音 / 小红书 / 快手"
                value={name}
                onChange={(e) => setName(e.target.value)}
                aria-invalid={submitted && !name.trim()}
                autoFocus
              />
              <FieldError
                show={submitted && !name.trim()}
                message="请输入平台名称"
              />
            </div>
            <div className="space-y-1.5">
              <Label htmlFor="pm-code">
                编码 <span className="text-destructive">*</span>
              </Label>
              <CodeField
                id="pm-code"
                value={code}
                onRegenerate={() => setCode(generateCode("PLT"))}
              />
              <p className="text-xs text-muted-foreground">
                系统自动生成,可刷新或复制
              </p>
              <FieldError show={submitted && !code.trim()} message="编码不可为空" />
            </div>
            <div className="space-y-1.5">
              <Label htmlFor="pm-url">
                访问链接 <span className="text-destructive">*</span>
              </Label>
              <Input
                id="pm-url"
                placeholder="平台首页 / 登录页,如 https://www.douyin.com"
                value={loginUrl}
                onChange={(e) => setLoginUrl(e.target.value)}
                aria-invalid={submitted && !loginUrl.trim()}
              />
              <FieldError
                show={submitted && !loginUrl.trim()}
                message="请输入访问链接"
              />
            </div>
            <div className="flex items-center justify-between rounded-lg border border-border p-3">
              <div>
                <div className="text-sm font-medium text-foreground">启用平台</div>
                <div className="text-xs text-muted-foreground">
                  停用后该平台不参与采集
                </div>
              </div>
              <Switch checked={enabled} onCheckedChange={setEnabled} />
            </div>
          </div>
          <SheetFooter className="flex-row justify-end gap-2 border-t">
            <Button
              type="button"
              variant="outline"
              onClick={() => onOpenChange(false)}
            >
              取消
            </Button>
            <Button type="submit">保存</Button>
          </SheetFooter>
        </form>
      </SheetContent>
    </Sheet>
  );
}
