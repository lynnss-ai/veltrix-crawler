// LLM 模型厂商管理:厂商列表(可增删改)ProvidersSection + 厂商表单抽屉 ProviderFormSheet。从 SettingsPage 拆出。
import { useMemo, useState } from "react";
import type { FormEvent } from "react";
import type { Provider, ProviderPreset } from "./settings-meta";
import { MODEL_CAPABILITIES } from "./settings-meta";
import type { ModelSpec, ModelCapability } from "@/lib/api";
import { cn } from "@/lib/utils";
import { RequiredMark } from "./settings-shared";
import type { ColumnDef, FilterFn } from "@tanstack/react-table";
import { Eye, EyeOff, MoreVertical, SquarePen, Plus, Search, Trash2 } from "lucide-react";
import { SimpleTooltip } from "@/components/SimpleTooltip";
import { RefreshButton } from "@/components/RefreshButton";
import { FieldError } from "@/components/FieldError";
import { DataTable } from "@/components/DataTable";
import { DataTableColumnHeader } from "@/components/DataTableColumnHeader";
import { StatusBadge } from "@/components/StatusBadge";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { Sheet, SheetContent, SheetDescription, SheetFooter, SheetHeader, SheetTitle } from "@/components/ui/sheet";
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuSeparator, DropdownMenuTrigger } from "@/components/ui/dropdown-menu";

const providerFilterFn: FilterFn<Provider> = (row, _columnId, value) =>
  `${row.original.name} ${row.original.apiUrl}`
    .toLowerCase()
    .includes(String(value).toLowerCase());

export function ProvidersSection({
  providers,
  presetCount,
  onCreate,
  onEdit,
  onDelete,
  onReload,
}: {
  providers: Provider[];
  presetCount: number;
  onCreate: () => void;
  onEdit: (provider: Provider) => void;
  onDelete: (provider: Provider) => void;
  onReload: () => void;
}) {
  const columns = useMemo<ColumnDef<Provider>[]>(
    () => [
      {
        accessorKey: "name",
        header: ({ column }) => (
          <DataTableColumnHeader column={column} title="名称" />
        ),
        cell: ({ row }) => (
          <span className="font-medium text-foreground">{row.original.name}</span>
        ),
      },
      {
        accessorKey: "code",
        header: "编码",
        enableSorting: false,
        cell: ({ row }) => (
          <span className="font-mono text-xs text-muted-foreground">
            {row.original.code}
          </span>
        ),
      },
      {
        accessorKey: "apiUrl",
        header: "API URL",
        enableSorting: false,
        cell: ({ row }) => (
          <span
            className="block max-w-[16rem] truncate font-mono text-xs text-muted-foreground"
            title={row.original.apiUrl}
          >
            {row.original.apiUrl || "—"}
          </span>
        ),
      },
      {
        id: "models",
        header: "模型",
        enableSorting: false,
        cell: ({ row }) => {
          const models = row.original.models;
          return models.length === 0 ? (
            <span className="text-muted-foreground">—</span>
          ) : (
            // 每个模型独占一行(名称 + 能力图标),行间虚线分割
            <div className="flex min-w-[20rem] flex-col divide-y divide-dashed divide-border">
              {models.map((m) => (
                <div
                  key={m.name}
                  className="flex items-center gap-2 py-1.5 first:pt-0 last:pb-0"
                >
                  {/* 名称固定宽度,保证各行能力图标纵向对齐;超长截断,完整名见 title */}
                  <span
                    className="w-[12rem] shrink-0 truncate font-medium text-foreground"
                    title={m.name}
                  >
                    {m.name}
                  </span>
                  {m.capabilities.length > 0 && (
                    // 能力用文字标识药丸,过多时自动换行包裹
                    <span className="flex flex-wrap items-center gap-1">
                      {MODEL_CAPABILITIES.filter((c) =>
                        m.capabilities.includes(c.code),
                      ).map((c) => (
                        <span
                          key={c.code}
                          className="rounded-full border border-border px-2 py-0.5 text-xs text-muted-foreground"
                        >
                          {c.label}
                        </span>
                      ))}
                    </span>
                  )}
                </div>
              ))}
            </div>
          );
        },
      },
      {
        id: "apiKey",
        header: "密钥",
        enableSorting: false,
        cell: ({ row }) =>
          row.original.apiKey ? (
            <StatusBadge tone="success">已配置</StatusBadge>
          ) : (
            <StatusBadge tone="neutral">未配置</StatusBadge>
          ),
      },
      {
        id: "actions",
        header: () => <div className="text-right">操作</div>,
        enableSorting: false,
        cell: ({ row }) => {
          const p = row.original;
          return (
            <div className="flex justify-end">
              <DropdownMenu>
                <DropdownMenuTrigger asChild>
                  <Button
                    variant="ghost"
                    size="icon-sm"
                    className="text-muted-foreground"
                  >
                    <MoreVertical />
                    <span className="sr-only">操作</span>
                  </Button>
                </DropdownMenuTrigger>
                <DropdownMenuContent align="end" className="w-32">
                  <DropdownMenuItem onClick={() => onEdit(p)}>
                    <SquarePen />
                    编辑
                  </DropdownMenuItem>
                  <DropdownMenuSeparator />
                  <DropdownMenuItem
                    className="text-destructive focus:text-destructive"
                    onClick={() => onDelete(p)}
                  >
                    <Trash2 />
                    删除
                  </DropdownMenuItem>
                </DropdownMenuContent>
              </DropdownMenu>
            </div>
          );
        },
      },
    ],
    [onEdit, onDelete],
  );

  return (
    <DataTable
      columns={columns}
      data={providers}
      itemLabel="个配置"
      globalFilterFn={providerFilterFn}
      getRowId={(p) => p.id}
      emptyState={
        <div className="py-12 text-center">
          <p className="text-sm font-medium text-foreground">暂无模型厂商</p>
          <p className="mt-1 text-xs text-muted-foreground">
            点击右上角「新增」添加厂商接口
          </p>
        </div>
      }
      renderToolbar={(table) => (
        <div className="flex flex-col gap-3 sm:flex-row sm:items-center sm:justify-between">
          <div className="relative w-full sm:max-w-sm">
            <Search className="pointer-events-none absolute left-3 top-1/2 size-4 -translate-y-1/2 text-muted-foreground" />
            <Input
              placeholder="搜索名称 / API URL"
              className="pl-9"
              value={(table.getState().globalFilter as string) ?? ""}
              onChange={(e) => table.setGlobalFilter(e.target.value)}
            />
          </div>
          <div className="flex items-center gap-2">
            <RefreshButton onClick={onReload} />
            <SimpleTooltip
              content={
                presetCount > 0 && providers.length >= presetCount
                  ? "已添加全部支持的厂商"
                  : "新增厂商"
              }
            >
              <span>
                <Button
                  onClick={onCreate}
                  disabled={presetCount > 0 && providers.length >= presetCount}
                >
                  <Plus />
                  新增
                </Button>
              </span>
            </SimpleTooltip>
          </div>
        </div>
      )}
    />
  );
}

// 厂商 新增 / 编辑 抽屉
export function ProviderFormSheet({
  open,
  initial,
  providers,
  presets,
  onOpenChange,
  onSubmit,
}: {
  open: boolean;
  initial: Provider | null;
  providers: Provider[];
  presets: ProviderPreset[];
  onOpenChange: (open: boolean) => void;
  onSubmit: (provider: Provider) => void;
}) {
  const isEdit = initial !== null;
  // 新增时厂商只能从「未添加的预设」里选,保证不可重复添加
  const usedCodes = new Set(providers.map((p) => p.code));
  const availablePresets = presets.filter((p) => !usedCodes.has(p.code));
  const [code, setCode] = useState(initial?.code ?? "");
  const [name, setName] = useState(initial?.name ?? "");
  const [apiUrl, setApiUrl] = useState(initial?.apiUrl ?? "");
  const [apiKey, setApiKey] = useState(initial?.apiKey ?? "");
  const [models, setModels] = useState<ModelSpec[]>(initial?.models ?? []);
  const [showKey, setShowKey] = useState(false);
  const [submitted, setSubmitted] = useState(false);

  function updateModelName(idx: number, value: string) {
    setModels((ms) => ms.map((m, i) => (i === idx ? { ...m, name: value } : m)));
  }
  function toggleCapability(idx: number, cap: ModelCapability) {
    setModels((ms) =>
      ms.map((m, i) => {
        if (i !== idx) return m;
        const has = m.capabilities.includes(cap);
        return {
          ...m,
          capabilities: has
            ? m.capabilities.filter((c) => c !== cap)
            : [...m.capabilities, cap],
        };
      }),
    );
  }
  function addModel() {
    // 新模型默认仅「对话」,引导用户按需勾选其它能力
    setModels((ms) => [...ms, { name: "", capabilities: ["text"] }]);
  }
  function removeModel(idx: number) {
    setModels((ms) => ms.filter((_, i) => i !== idx));
  }

  function handleSubmit(event: FormEvent) {
    event.preventDefault();
    setSubmitted(true);
    // 清洗:去空名模型、空能力兜底 text(与后端 normalize_capabilities 一致)
    const cleanedModels: ModelSpec[] = models
      .map((m) => ({
        name: m.name.trim(),
        capabilities:
          m.capabilities.length > 0 ? m.capabilities : (["text"] as ModelCapability[]),
      }))
      .filter((m) => m.name);
    // 必填项为空时改为字段下方提示,不再用顶部红框
    if (
      !code ||
      !name.trim() ||
      !apiUrl.trim() ||
      !apiKey.trim() ||
      cleanedModels.length === 0
    ) {
      return;
    }
    onSubmit({
      id: initial?.id ?? crypto.randomUUID(),
      code,
      name: name.trim(),
      apiUrl: apiUrl.trim(),
      apiKey: apiKey.trim(),
      models: cleanedModels,
    });
  }

  // 用户改动过任一字段(编码自动生成不计入)即视为已编辑,阻止误关
  const isDirty =
    name !== (initial?.name ?? "") ||
    apiUrl !== (initial?.apiUrl ?? "") ||
    apiKey !== (initial?.apiKey ?? "") ||
    JSON.stringify(models) !== JSON.stringify(initial?.models ?? []);

  return (
    <Sheet open={open} onOpenChange={onOpenChange}>
      <SheetContent
        className="flex w-full flex-col gap-0 p-0 sm:max-w-[600px]"
        blockClose={isDirty}
      >
        <SheetHeader className="border-b">
          <SheetTitle>{isEdit ? "编辑厂商" : "新增厂商"}</SheetTitle>
          <SheetDescription>
            配置大模型厂商的接口、密钥与可用模型;为每个模型标记能力,供对话 / 智能体按需选用。
          </SheetDescription>
        </SheetHeader>
        <form onSubmit={handleSubmit} className="flex min-h-0 flex-1 flex-col">
          <div className="flex-1 space-y-6 overflow-y-auto p-5">
            <div className="space-y-3">
              <div className="text-xs font-medium uppercase tracking-wide text-muted-foreground">
                基本信息
              </div>
              <div className="space-y-1.5">
                <Label htmlFor="provider-name">
                  厂商 <RequiredMark />
                </Label>
                {isEdit ? (
                  // 编辑态厂商不可改(换厂商等于换一家,应删除后重建)
                  <Input id="provider-name" value={name} disabled />
                ) : (
                  <Select
                    value={code}
                    onValueChange={(v) => {
                      const preset = presets.find((p) => p.code === v);
                      if (preset) {
                        setCode(preset.code);
                        setName(preset.name);
                        setApiUrl(preset.apiUrl);
                      }
                    }}
                  >
                    <SelectTrigger
                      id="provider-name"
                      aria-invalid={submitted && !code}
                    >
                      <SelectValue placeholder="选择厂商" />
                    </SelectTrigger>
                    <SelectContent>
                      {availablePresets.length === 0 ? (
                        <div className="px-2 py-1.5 text-xs text-muted-foreground">
                          已添加全部支持的厂商
                        </div>
                      ) : (
                        availablePresets.map((p) => (
                          <SelectItem key={p.code} value={p.code}>
                            {p.name}
                          </SelectItem>
                        ))
                      )}
                    </SelectContent>
                  </Select>
                )}
                <FieldError show={submitted && !code} message="请选择厂商" />
                <p className="text-xs text-muted-foreground">
                  仅支持 DeepSeek / 千问 Qwen / 小米 MiMo / 智谱 GLM /
                  MiniMax,且每家只能添加一次
                </p>
              </div>
            </div>

            <div className="space-y-3">
              <div className="text-xs font-medium uppercase tracking-wide text-muted-foreground">
                接口配置
              </div>
              <div className="space-y-1.5">
                <Label htmlFor="provider-url">
                  API URL <RequiredMark />
                </Label>
                <Input
                  id="provider-url"
                  placeholder="https://api.openai.com/v1"
                  value={apiUrl}
                  onChange={(e) => setApiUrl(e.target.value)}
                  aria-invalid={submitted && !apiUrl.trim()}
                  disabled={isEdit}
                />
                <FieldError
                  show={submitted && !apiUrl.trim()}
                  message="API URL 不可为空"
                />
              </div>
              <div className="space-y-1.5">
                <Label htmlFor="provider-key">
                  API Key <RequiredMark />
                </Label>
                <div className="relative">
                  <Input
                    id="provider-key"
                    type={showKey ? "text" : "password"}
                    className="pr-10"
                    placeholder="sk-..."
                    value={apiKey}
                    onChange={(e) => setApiKey(e.target.value)}
                    aria-invalid={submitted && !apiKey.trim()}
                  />
                  <button
                    type="button"
                    onClick={() => setShowKey((s) => !s)}
                    className="absolute right-2 top-1/2 -translate-y-1/2 text-muted-foreground transition-colors hover:text-foreground"
                    title={showKey ? "隐藏" : "显示"}
                  >
                    {showKey ? (
                      <EyeOff className="size-4" />
                    ) : (
                      <Eye className="size-4" />
                    )}
                    <span className="sr-only">
                      {showKey ? "隐藏" : "显示"}密钥
                    </span>
                  </button>
                </div>
                <p className="text-xs text-muted-foreground">
                  仅本地保存;含密码建议改用环境变量
                </p>
                <FieldError
                  show={submitted && !apiKey.trim()}
                  message="API Key 不可为空"
                />
              </div>
            </div>

            <div className="space-y-3">
              <div className="text-xs font-medium uppercase tracking-wide text-muted-foreground">
                可用模型 <RequiredMark />
              </div>
              <p className="text-xs text-muted-foreground">
                逐个添加模型并勾选其能力。能力决定该模型在何处可选:对话 / 角色模型需「对话」,
                编程 / 浏览器智能体需「工具调用」,多模态场景按需勾选「图片 / 音频 / 视频」。
              </p>
              <div className="space-y-2">
                {models.map((m, idx) => (
                  <div
                    key={idx}
                    className="space-y-2.5 rounded-lg border border-border bg-muted/30 p-3"
                  >
                    <div className="flex items-center gap-2">
                      <Input
                        placeholder="模型名,如 gpt-4o"
                        value={m.name}
                        onChange={(e) => updateModelName(idx, e.target.value)}
                        aria-invalid={submitted && !m.name.trim()}
                      />
                      <SimpleTooltip content="删除该模型">
                        <Button
                          type="button"
                          variant="ghost"
                          size="icon-sm"
                          className="shrink-0 text-muted-foreground hover:text-destructive"
                          onClick={() => removeModel(idx)}
                        >
                          <Trash2 className="size-4" />
                          <span className="sr-only">删除模型</span>
                        </Button>
                      </SimpleTooltip>
                    </div>
                    <div className="flex flex-wrap gap-1.5">
                      {MODEL_CAPABILITIES.map((cap) => {
                        const active = m.capabilities.includes(cap.code);
                        return (
                          <button
                            type="button"
                            key={cap.code}
                            onClick={() => toggleCapability(idx, cap.code)}
                            className={cn(
                              "inline-flex items-center gap-1 rounded-full border px-2.5 py-1 text-xs font-medium transition-colors",
                              active
                                ? "border-primary/40 bg-primary/10 text-primary"
                                : "border-border text-muted-foreground hover:bg-muted",
                            )}
                          >
                            <cap.icon className="size-3.5" />
                            {cap.label}
                          </button>
                        );
                      })}
                    </div>
                  </div>
                ))}
                <Button
                  type="button"
                  variant="outline"
                  size="sm"
                  className="w-full"
                  onClick={addModel}
                >
                  <Plus className="size-4" /> 添加模型
                </Button>
                <FieldError
                  show={submitted && models.every((m) => !m.name.trim())}
                  message="请至少添加一个可用模型"
                />
              </div>
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
