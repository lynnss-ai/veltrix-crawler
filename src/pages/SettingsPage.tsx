import {
  useEffect,
  useMemo,
  useState,
  type FormEvent,
  type ReactNode,
} from "react";
import { type ColumnDef, type FilterFn } from "@tanstack/react-table";
import {
  AudioLines,
  Bot,
  Check,
  ExternalLink,
  Eye,
  EyeOff,
  FolderOpen,
  Loader2,
  MessageSquareText,
  MoreVertical,
  Pencil,
  Plus,
  Search,
  Settings2,
  Sparkles,
  Trash2,
  TriangleAlert,
  X,
} from "lucide-react";
import { open } from "@tauri-apps/plugin-dialog";
import { openPath, revealItemInDir } from "@tauri-apps/plugin-opener";
import { SimpleTooltip } from "@/components/SimpleTooltip";
import { RefreshButton } from "@/components/RefreshButton";
import { FORM_CONTROL_SIZING } from "@/lib/form-sizing";
import { FieldError } from "@/components/FieldError";
import { toast } from "sonner";
import { api, type AppConfig } from "@/lib/api";
import { DataTable } from "@/components/DataTable";
import { DataTableColumnHeader } from "@/components/DataTableColumnHeader";
import { StatusBadge } from "@/components/StatusBadge";
import { CodeField, generateCode } from "@/components/CodeField";
import { ErrorBanner } from "@/components/ErrorBanner";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Textarea } from "@/components/ui/textarea";
import {
  Card,
  CardAction,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from "@/components/ui/card";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import {
  Sheet,
  SheetContent,
  SheetDescription,
  SheetFooter,
  SheetHeader,
  SheetTitle,
} from "@/components/ui/sheet";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
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

// 配置项当前为前端占位,待后端 set_config / clear_data 等命令就绪后接入。

const SECTION_GROUPS = [
  {
    title: "基础设置",
    items: [{ key: "general", label: "常规", icon: Settings2 }],
  },
  {
    title: "AI 配置",
    items: [
      { key: "providers", label: "模型厂商", icon: Bot },
      { key: "transcription", label: "语音转写", icon: AudioLines },
      { key: "intent", label: "意向分析", icon: Sparkles },
      { key: "prompts", label: "提示词", icon: MessageSquareText },
    ],
  },
] as const;
type SectionKey = (typeof SECTION_GROUPS)[number]["items"][number]["key"];

interface TranscriptionConfig {
  providerId: string;
  model: string;
}

interface IntentConfig {
  providerId: string;
  model: string;
  promptId: string;
  batchSize: string;
}

const DEFAULT_TRANSCRIPTION: TranscriptionConfig = { providerId: "", model: "" };

const DEFAULT_INTENT: IntentConfig = {
  providerId: "",
  model: "",
  promptId: "",
  batchSize: "10",
};

interface Prompt {
  id: string;
  code: string;
  name: string;
  content: string;
}

interface Provider {
  id: string;
  code: string;
  name: string;
  apiUrl: string;
  apiKey: string;
  models: string; // 每行一个模型
}

// 清空数据需输入的确认词
const CLEAR_CONFIRM_TEXT = "清空数据";

// 字节数格式化为可读大小
function formatBytes(bytes: number): string {
  if (bytes <= 0) return "0 B";
  const units = ["B", "KB", "MB", "GB", "TB"];
  let value = bytes;
  let i = 0;
  while (value >= 1024 && i < units.length - 1) {
    value /= 1024;
    i += 1;
  }
  return `${value.toFixed(i === 0 ? 0 : 1)} ${units[i]}`;
}

export function SettingsPage() {
  const [active, setActive] = useState<SectionKey>("general");
  const [cfg, setCfg] = useState<AppConfig | null>(null);
  const [error, setError] = useState<string | null>(null);

  const [prompts, setPrompts] = useState<Prompt[]>([]);
  const [providers, setProviders] = useState<Provider[]>([]);

  const [clearOpen, setClearOpen] = useState(false);
  const [clearText, setClearText] = useState("");
  const [clearPassword, setClearPassword] = useState("");
  const [promptForm, setPromptForm] = useState<Prompt | null>(null);
  const [isPromptFormOpen, setIsPromptFormOpen] = useState(false);
  const [promptDeleteTarget, setPromptDeleteTarget] = useState<Prompt | null>(
    null,
  );
  const [providerForm, setProviderForm] = useState<Provider | null>(null);
  const [isProviderFormOpen, setIsProviderFormOpen] = useState(false);
  const [providerDeleteTarget, setProviderDeleteTarget] =
    useState<Provider | null>(null);

  function reloadProviders() {
    api
      .listProviders()
      .then(setProviders)
      .catch((e) => setError(String(e)));
  }
  function reloadPrompts() {
    api
      .listPrompts()
      .then(setPrompts)
      .catch((e) => setError(String(e)));
  }
  useEffect(() => {
    api
      .getAppConfig()
      .then(setCfg)
      .catch((e) => setError(String(e)));
    reloadProviders();
    reloadPrompts();
  }, []);

  function submitPrompt(prompt: Prompt) {
    api
      .upsertPrompt(prompt)
      .then(() => {
        setIsPromptFormOpen(false);
        reloadPrompts();
      })
      .catch((e) => toast.error(`保存失败: ${e}`));
  }

  function confirmDeletePrompt() {
    if (!promptDeleteTarget) return;
    api
      .removePrompt(promptDeleteTarget.id)
      .then(() => {
        setPromptDeleteTarget(null);
        reloadPrompts();
      })
      .catch((e) => toast.error(`删除失败: ${e}`));
  }

  function submitProvider(provider: Provider) {
    api
      .upsertProvider(provider)
      .then(() => {
        setIsProviderFormOpen(false);
        reloadProviders();
      })
      .catch((e) => toast.error(`保存失败: ${e}`));
  }

  function confirmDeleteProvider() {
    if (!providerDeleteTarget) return;
    api
      .removeProvider(providerDeleteTarget.id)
      .then(() => {
        setProviderDeleteTarget(null);
        reloadProviders();
      })
      .catch((e) => toast.error(`删除失败: ${e}`));
  }

  return (
    <div
      className={`flex min-h-0 flex-1 flex-col gap-4 ${FORM_CONTROL_SIZING}`}
    >
      <ErrorBanner message={error} onClose={() => setError(null)} />

      <div className="flex min-h-0 flex-1 gap-4">
        {/* 左侧:分类菜单 */}
        <div className="flex w-40 shrink-0 flex-col gap-3 rounded-xl border bg-card p-2 lg:w-48">
          {SECTION_GROUPS.map((group) => (
            <div key={group.title} className="space-y-0.5">
              <div className="px-3 py-1 text-xs font-semibold uppercase tracking-wide text-muted-foreground">
                {group.title}
              </div>
              {group.items.map((s) => {
                const Icon = s.icon;
                return (
                  <button
                    key={s.key}
                    onClick={() => setActive(s.key)}
                    className={`flex w-full items-center gap-2.5 rounded-md px-3 py-2 text-left text-sm transition-colors ${
                      active === s.key
                        ? "bg-accent font-medium text-accent-foreground"
                        : "text-muted-foreground hover:bg-accent/50 hover:text-foreground"
                    }`}
                  >
                    <Icon className="size-4 shrink-0" />
                    {s.label}
                  </button>
                );
              })}
            </div>
          ))}
        </div>

        {/* 右侧:对应内容。模型厂商用整页 DataTable;其余分类为可滚动的卡片表单 */}
        {active === "providers" || active === "prompts" ? (
          <div
            key={active}
            className="flex min-h-0 min-w-0 flex-1 flex-col duration-200 animate-in fade-in-50"
          >
            {active === "providers" && (
              <ProvidersSection
                providers={providers}
                onCreate={() => {
                  setProviderForm(null);
                  setIsProviderFormOpen(true);
                }}
                onEdit={(p) => {
                  setProviderForm(p);
                  setIsProviderFormOpen(true);
                }}
                onDelete={(p) => setProviderDeleteTarget(p)}
                onReload={reloadProviders}
              />
            )}
            {active === "prompts" && (
              <PromptsSection
                prompts={prompts}
                onCreate={() => {
                  setPromptForm(null);
                  setIsPromptFormOpen(true);
                }}
                onEdit={(p) => {
                  setPromptForm(p);
                  setIsPromptFormOpen(true);
                }}
                onDelete={(p) => setPromptDeleteTarget(p)}
                onReload={reloadPrompts}
              />
            )}
          </div>
        ) : (
          <div
            key={active}
            className="min-h-0 min-w-0 flex-1 space-y-4 overflow-auto p-0.5 duration-200 animate-in fade-in-50"
          >
            {active === "general" && (
              <GeneralSection cfg={cfg} onClearData={() => setClearOpen(true)} />
            )}
            {active === "transcription" && (
              <TranscriptionSection providers={providers} />
            )}
            {active === "intent" && (
              <IntentSection providers={providers} prompts={prompts} />
            )}
          </div>
        )}
      </div>

      <PromptFormSheet
        key={isPromptFormOpen ? (promptForm?.id ?? "new-prompt") : "idle"}
        open={isPromptFormOpen}
        initial={promptForm}
        onOpenChange={setIsPromptFormOpen}
        onSubmit={submitPrompt}
      />

      <ProviderFormSheet
        key={isProviderFormOpen ? (providerForm?.id ?? "new-provider") : "idle"}
        open={isProviderFormOpen}
        initial={providerForm}
        onOpenChange={setIsProviderFormOpen}
        onSubmit={submitProvider}
      />

      <AlertDialog
        open={clearOpen}
        onOpenChange={(open) => {
          setClearOpen(open);
          if (!open) {
            setClearText("");
            setClearPassword("");
          }
        }}
      >
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle className="flex items-center gap-2">
              <TriangleAlert className="size-4 shrink-0 text-destructive" />
              清空业务数据
            </AlertDialogTitle>
            <AlertDialogDescription>
              此操作不可恢复,请谨慎确认。
            </AlertDialogDescription>
          </AlertDialogHeader>

          <div className="rounded-lg border border-destructive/20 bg-destructive/5 p-3 text-sm">
            <div className="mb-2 font-medium text-destructive">将永久删除</div>
            <ul className="space-y-1.5 text-muted-foreground">
              <li className="flex items-center gap-2">
                <X className="size-3.5 shrink-0 text-destructive" />
                数据库中的采集内容与统计
              </li>
              <li className="flex items-center gap-2">
                <X className="size-3.5 shrink-0 text-destructive" />
                存储路径下的媒体 / 图片等文件
              </li>
            </ul>
            <div className="mt-2.5 flex items-center gap-2 border-t border-destructive/15 pt-2.5 text-xs text-muted-foreground">
              <Check className="size-3.5 shrink-0 text-emerald-500" />
              平台与账号配置保留
            </div>
          </div>

          <div className="space-y-4">
            <div className="space-y-1.5">
              <Label htmlFor="clear-confirm">
                请输入{" "}
                <span className="font-mono font-semibold text-destructive">
                  [{CLEAR_CONFIRM_TEXT}]
                </span>{" "}
                以确认
              </Label>
              <Input
                id="clear-confirm"
                placeholder={CLEAR_CONFIRM_TEXT}
                value={clearText}
                onChange={(e) => setClearText(e.target.value)}
              />
            </div>
            <div className="space-y-1.5">
              <Label htmlFor="clear-password">当前用户密码</Label>
              <Input
                id="clear-password"
                type="password"
                placeholder="输入当前登录用户的密码"
                value={clearPassword}
                onChange={(e) => setClearPassword(e.target.value)}
              />
            </div>
          </div>
          <AlertDialogFooter>
            <AlertDialogCancel>取消</AlertDialogCancel>
            <AlertDialogAction
              disabled={
                clearText.trim() !== CLEAR_CONFIRM_TEXT ||
                clearPassword.length === 0
              }
              onClick={() => {
                // TODO: invoke("clear_business_data", { password: clearPassword })
                //       后端校验密码后:清空业务表 + 递归删除存储路径下的文件
                setClearOpen(false);
                setClearText("");
                setClearPassword("");
              }}
              className="bg-destructive text-white hover:bg-destructive/90"
            >
              确认清空
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>

      <AlertDialog
        open={promptDeleteTarget !== null}
        onOpenChange={(open) => !open && setPromptDeleteTarget(null)}
      >
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>
              删除提示词「{promptDeleteTarget?.name}」?
            </AlertDialogTitle>
            <AlertDialogDescription>
              删除后引用该提示词的分析任务需重新配置。
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel>取消</AlertDialogCancel>
            <AlertDialogAction
              onClick={confirmDeletePrompt}
              className="bg-destructive text-white hover:bg-destructive/90"
            >
              删除
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>

      <AlertDialog
        open={providerDeleteTarget !== null}
        onOpenChange={(open) => !open && setProviderDeleteTarget(null)}
      >
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>
              删除厂商「{providerDeleteTarget?.name}」?
            </AlertDialogTitle>
            <AlertDialogDescription>
              引用该厂商的语音转写 / 意向分析配置需重新选择。
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel>取消</AlertDialogCancel>
            <AlertDialogAction
              onClick={confirmDeleteProvider}
              className="bg-destructive text-white hover:bg-destructive/90"
            >
              删除
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
    </div>
  );
}

// 通用配置卡片:标题 + 内容 + (可选)保存按钮,保存后短暂提示
function SettingsCard({
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

// 必填字段标记
function RequiredMark() {
  return <span className="ml-0.5 text-destructive">*</span>;
}

function Row({ label, children }: { label: string; children: ReactNode }) {
  return (
    <div className="flex gap-3 text-sm">
      <dt className="w-28 shrink-0 text-muted-foreground">{label}</dt>
      <dd className="min-w-0 flex-1 text-foreground">{children}</dd>
    </div>
  );
}

function GeneralSection({
  cfg,
  onClearData,
}: {
  cfg: AppConfig | null;
  onClearData: () => void;
}) {
  const [storagePath, setStoragePath] = useState("");
  const [storageBaseline, setStorageBaseline] = useState("");
  const [dbSize, setDbSize] = useState<number | null>(null);
  const [dbUrl, setDbUrl] = useState("");
  const [maxConn, setMaxConn] = useState("8");
  const [dbBaseline, setDbBaseline] = useState({ url: "", maxConn: "8" });
  const [testing, setTesting] = useState(false);
  const [dbPath, setDbPath] = useState<string | null>(null);
  const [dataDir, setDataDir] = useState("");

  useEffect(() => {
    api
      .getDatabaseSize()
      .then(setDbSize)
      .catch(() => setDbSize(null));
    api
      .getDatabasePath()
      .then(setDbPath)
      .catch(() => setDbPath(null));
    api
      .getDataDir()
      .then(setDataDir)
      .catch(() => setDataDir(""));
  }, []);
  useEffect(() => {
    if (cfg) {
      const url = cfg.database.url ?? "";
      const conn = String(cfg.database.max_connections);
      setDbUrl(url);
      setMaxConn(conn);
      setDbBaseline({ url, maxConn: conn });
    }
  }, [cfg]);

  const storageDirty = storagePath !== storageBaseline;
  const dbDirty = dbUrl !== dbBaseline.url || maxConn !== dbBaseline.maxConn;

  // 选择目录(Tauri dialog)
  async function pickStorageDir() {
    const selected = await open({ directory: true, multiple: false });
    if (typeof selected === "string") setStoragePath(selected);
  }
  // 在系统文件管理器中打开目录(Tauri opener)
  async function openStorageDir() {
    if (!storagePath) return;
    try {
      await openPath(storagePath);
    } catch (e) {
      toast.error(`无法打开目录: ${e}`);
    }
  }
  // 测试数据库连接串能否连通
  async function testDbConnection() {
    const url = dbUrl.trim();
    if (!/^(sqlite:|postgres:\/\/|postgresql:\/\/)/i.test(url)) {
      toast.error("连接串格式不正确,应以 sqlite: 或 postgres:// 开头");
      return;
    }
    setTesting(true);
    try {
      await api.testDatabaseConnection(url);
      toast.success("数据库连接成功");
    } catch (e) {
      toast.error(`连接失败: ${e}`);
    } finally {
      setTesting(false);
    }
  }

  return (
    <>
      <SettingsCard
        title="存储"
        description="采集数据与媒体文件的本地落地目录。"
        dirty={storageDirty}
        onSave={() => {
          // TODO: invoke("set_storage_path", { path: storagePath })
          setStorageBaseline(storagePath);
          toast.success("存储配置已保存(待接后端)");
        }}
      >
        <div className="space-y-1.5">
          <Label htmlFor="storage-path">存储路径</Label>
          <div className="flex gap-2">
            <Input
              id="storage-path"
              placeholder="留空则使用应用默认数据目录"
              value={storagePath}
              onChange={(e) => setStoragePath(e.target.value)}
            />
            <Button
              type="button"
              variant="outline"
              className="shrink-0"
              onClick={pickStorageDir}
            >
              <FolderOpen />
              选择
            </Button>
            <Button
              type="button"
              variant="outline"
              className="shrink-0"
              disabled={!storagePath}
              onClick={openStorageDir}
            >
              <ExternalLink />
              打开
            </Button>
          </div>
          {dataDir && (
            <p className="flex items-center gap-1.5 text-xs text-muted-foreground">
              <span>默认数据目录:</span>
              <SimpleTooltip content={`打开 ${dataDir}`}>
                <button
                  type="button"
                  onClick={() =>
                    openPath(dataDir).catch((e) =>
                      toast.error(`无法打开目录: ${e}`),
                    )
                  }
                  className="truncate font-mono hover:text-foreground hover:underline"
                >
                  {dataDir}
                </button>
              </SimpleTooltip>
            </p>
          )}
        </div>
      </SettingsCard>

      <SettingsCard
        title="数据库"
        description="含密码建议改用环境变量 VELTRIX_DATABASE_URL(优先级更高、不落盘)。"
        dirty={dbDirty}
        onSave={() => {
          const url = dbUrl.trim();
          // 非空连接串必须是合法格式,避免存入无效值导致下次启动连不上
          if (url && !/^(sqlite:|postgres:\/\/|postgresql:\/\/)/i.test(url)) {
            toast.error("连接串格式不正确,应以 sqlite: 或 postgres:// 开头");
            return;
          }
          api
            .setDatabaseConfig(url, Number(maxConn) || 1)
            .then(() => {
              setDbBaseline({ url: dbUrl, maxConn });
              toast.success("数据库配置已保存,重启应用后生效");
            })
            .catch((e) => toast.error(String(e)));
        }}
      >
        <dl className="space-y-3">
          <Row label="当前大小">
            {dbSize === null ? "—" : formatBytes(dbSize)}
          </Row>
          <Row label="后端">
            {dbUrl
              ? dbUrl.startsWith("postgres")
                ? "PostgreSQL"
                : "SQLite"
              : "SQLite(默认本地文件)"}
          </Row>
          {dbPath && (
            <Row label="数据库文件">
              <div className="flex items-start gap-2">
                <div className="min-w-0 flex-1">
                  <div className="text-foreground">
                    {dbPath.split(/[\\/]/).pop()}
                  </div>
                  <SimpleTooltip content={dbPath}>
                    <div className="truncate font-mono text-xs text-muted-foreground">
                      {dbPath}
                    </div>
                  </SimpleTooltip>
                </div>
                <SimpleTooltip content="在文件管理器中打开">
                  <Button
                    type="button"
                    variant="ghost"
                    size="icon-xs"
                    className="shrink-0"
                    onClick={() => revealItemInDir(dbPath)}
                  >
                    <ExternalLink />
                  </Button>
                </SimpleTooltip>
              </div>
            </Row>
          )}
        </dl>
        <div className="space-y-1.5">
          <Label htmlFor="db-url">连接串</Label>
          <div className="flex gap-2">
            <Input
              id="db-url"
              placeholder="留空使用本地 SQLite;postgres://... 切换 PG"
              value={dbUrl}
              onChange={(e) => setDbUrl(e.target.value)}
            />
            <Button
              type="button"
              variant="outline"
              className="shrink-0"
              disabled={!dbUrl.trim() || testing}
              onClick={testDbConnection}
            >
              {testing && <Loader2 className="animate-spin" />}
              测试连接
            </Button>
          </div>
        </div>
        <div className="space-y-1.5">
          <Label htmlFor="db-pool">连接池上限</Label>
          <Input
            id="db-pool"
            type="number"
            min={1}
            value={maxConn}
            onChange={(e) => setMaxConn(e.target.value)}
          />
        </div>
        {dbDirty && (
          <div className="rounded-lg border border-amber-500/30 bg-amber-500/10 px-3 py-2 text-xs text-amber-600 dark:text-amber-400">
            数据库连接配置修改后需重启应用才能重连生效。
          </div>
        )}
      </SettingsCard>

      <Card className="border-destructive/40">
        <CardHeader>
          <CardTitle className="text-destructive">危险操作</CardTitle>
          <CardDescription>清空业务数据不可恢复,请谨慎操作。</CardDescription>
        </CardHeader>
        <CardContent>
          <div className="flex items-center justify-between rounded-lg border border-destructive/30 bg-destructive/5 p-3">
            <div>
              <div className="text-sm font-medium text-foreground">
                清空业务数据
              </div>
              <div className="text-xs text-muted-foreground">
                清空采集内容 / 媒体 / 统计,保留平台与账号配置
              </div>
            </div>
            <Button
              variant="destructive"
              className="bg-destructive text-white hover:bg-destructive/90"
              onClick={onClearData}
            >
              清空数据
            </Button>
          </div>
        </CardContent>
      </Card>
    </>
  );
}

// 提取某厂商的模型列表
function providerModels(provider: Provider | undefined): string[] {
  if (!provider) return [];
  return provider.models
    .split("\n")
    .map((m) => m.trim())
    .filter(Boolean);
}

function TranscriptionSection({ providers }: { providers: Provider[] }) {
  const [value, setValue] = useState<TranscriptionConfig>(DEFAULT_TRANSCRIPTION);
  const [baseline, setBaseline] = useState<TranscriptionConfig>(
    DEFAULT_TRANSCRIPTION,
  );
  const dirty = JSON.stringify(value) !== JSON.stringify(baseline);
  const provider = providers.find((p) => p.id === value.providerId);
  const models = providerModels(provider);

  return (
    <SettingsCard
      title="语音转写"
      description="在「模型厂商」中选择厂商与模型,用于音视频转写。"
      dirty={dirty}
      onSave={() => {
        // TODO: invoke("set_transcription_config", value)
        setBaseline(value);
        toast.success("语音转写配置已保存(待接后端)");
      }}
    >
      <ProviderModelPicker
        providers={providers}
        providerId={value.providerId}
        model={value.model}
        onProviderChange={(id) => setValue({ providerId: id, model: "" })}
        onModelChange={(m) => setValue({ ...value, model: m })}
        models={models}
      />
    </SettingsCard>
  );
}

// 厂商 + 模型 两级选择(从「模型厂商」读取)
function ProviderModelPicker({
  providers,
  providerId,
  model,
  models,
  onProviderChange,
  onModelChange,
}: {
  providers: Provider[];
  providerId: string;
  model: string;
  models: string[];
  onProviderChange: (id: string) => void;
  onModelChange: (model: string) => void;
}) {
  return (
    <div className="grid gap-4 sm:grid-cols-2">
      <div className="space-y-1.5">
        <Label>厂商</Label>
        <Select value={providerId} onValueChange={onProviderChange}>
          <SelectTrigger className="w-full">
            <SelectValue placeholder="选择厂商" />
          </SelectTrigger>
          <SelectContent>
            {providers.length === 0 ? (
              <div className="px-2 py-1.5 text-sm text-muted-foreground">
                请先在「模型厂商」添加厂商
              </div>
            ) : (
              providers.map((p) => (
                <SelectItem key={p.id} value={p.id}>
                  {p.name}
                </SelectItem>
              ))
            )}
          </SelectContent>
        </Select>
      </div>
      <div className="space-y-1.5">
        <Label>模型</Label>
        <Select
          value={model}
          onValueChange={onModelChange}
          disabled={!providerId}
        >
          <SelectTrigger className="w-full">
            <SelectValue placeholder={providerId ? "选择模型" : "请先选择厂商"} />
          </SelectTrigger>
          <SelectContent>
            {models.length === 0 ? (
              <div className="px-2 py-1.5 text-sm text-muted-foreground">
                该厂商暂无模型
              </div>
            ) : (
              models.map((m) => (
                <SelectItem key={m} value={m}>
                  {m}
                </SelectItem>
              ))
            )}
          </SelectContent>
        </Select>
      </div>
    </div>
  );
}

function IntentSection({
  providers,
  prompts,
}: {
  providers: Provider[];
  prompts: Prompt[];
}) {
  const [value, setValue] = useState<IntentConfig>(DEFAULT_INTENT);
  const [baseline, setBaseline] = useState<IntentConfig>(DEFAULT_INTENT);
  const dirty = JSON.stringify(value) !== JSON.stringify(baseline);
  const provider = providers.find((p) => p.id === value.providerId);
  const models = providerModels(provider);

  return (
    <SettingsCard
      title="AI 意向分析"
      description="选择厂商、模型与提示词,调用大模型分析用户意向。"
      dirty={dirty}
      onSave={() => {
        // TODO: invoke("set_intent_config", value)
        setBaseline(value);
        toast.success("意向分析配置已保存(待接后端)");
      }}
    >
      <ProviderModelPicker
        providers={providers}
        providerId={value.providerId}
        model={value.model}
        models={models}
        onProviderChange={(id) =>
          setValue({ ...value, providerId: id, model: "" })
        }
        onModelChange={(m) => setValue({ ...value, model: m })}
      />
      <div className="grid gap-4 sm:grid-cols-2">
        <div className="space-y-1.5">
          <Label>提示词</Label>
          <Select
            value={value.promptId}
            onValueChange={(v) => setValue({ ...value, promptId: v })}
          >
            <SelectTrigger className="w-full">
              <SelectValue placeholder="选择提示词" />
            </SelectTrigger>
            <SelectContent>
              {prompts.length === 0 ? (
                <div className="px-2 py-1.5 text-sm text-muted-foreground">
                  请先在「提示词」中添加
                </div>
              ) : (
                prompts.map((p) => (
                  <SelectItem key={p.id} value={p.id}>
                    {p.name}
                  </SelectItem>
                ))
              )}
            </SelectContent>
          </Select>
        </div>
        <div className="space-y-1.5">
          <Label htmlFor="intent-batch">批处理大小</Label>
          <Input
            id="intent-batch"
            type="number"
            min={1}
            className="w-full"
            value={value.batchSize}
            onChange={(e) => setValue({ ...value, batchSize: e.target.value })}
          />
        </div>
      </div>
    </SettingsCard>
  );
}

// 提示词搜索:匹配名称 / 编码 / 内容
const promptFilterFn: FilterFn<Prompt> = (row, _columnId, value) =>
  `${row.original.name} ${row.original.code} ${row.original.content}`
    .toLowerCase()
    .includes(String(value).toLowerCase());

function PromptsSection({
  prompts,
  onCreate,
  onEdit,
  onDelete,
  onReload,
}: {
  prompts: Prompt[];
  onCreate: () => void;
  onEdit: (prompt: Prompt) => void;
  onDelete: (prompt: Prompt) => void;
  onReload: () => void;
}) {
  const columns = useMemo<ColumnDef<Prompt>[]>(
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
        id: "content",
        header: "内容",
        enableSorting: false,
        cell: ({ row }) => (
          <span
            className="block max-w-[24rem] truncate text-muted-foreground"
            title={row.original.content}
          >
            {row.original.content}
          </span>
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
                    <Pencil />
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
      data={prompts}
      itemLabel="个提示词"
      globalFilterFn={promptFilterFn}
      getRowId={(p) => p.id}
      emptyState={
        <div className="py-12 text-center">
          <p className="text-sm font-medium text-foreground">暂无提示词</p>
          <p className="mt-1 text-xs text-muted-foreground">
            点击右上角「新增」添加提示词模板
          </p>
        </div>
      }
      renderToolbar={(table) => (
        <div className="flex flex-col gap-3 sm:flex-row sm:items-center sm:justify-between">
          <div className="relative w-full sm:max-w-xs">
            <Search className="pointer-events-none absolute left-3 top-1/2 size-4 -translate-y-1/2 text-muted-foreground" />
            <Input
              placeholder="搜索名称 / 编码 / 内容"
              className="pl-9"
              value={(table.getState().globalFilter as string) ?? ""}
              onChange={(e) => table.setGlobalFilter(e.target.value)}
            />
          </div>
          <div className="flex items-center gap-2">
            <RefreshButton onClick={onReload} />
            <Button onClick={onCreate}>
              <Plus />
              新增
            </Button>
          </div>
        </div>
      )}
    />
  );
}

// 模型厂商搜索:匹配名称 / API URL
const providerFilterFn: FilterFn<Provider> = (row, _columnId, value) =>
  `${row.original.name} ${row.original.apiUrl}`
    .toLowerCase()
    .includes(String(value).toLowerCase());

function ProvidersSection({
  providers,
  onCreate,
  onEdit,
  onDelete,
  onReload,
}: {
  providers: Provider[];
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
          const models = row.original.models
            .split("\n")
            .map((m) => m.trim())
            .filter(Boolean);
          return models.length === 0 ? (
            <span className="text-muted-foreground">—</span>
          ) : (
            <div className="flex flex-wrap gap-1">
              {models.map((m) => (
                <Badge key={m} variant="secondary" className="font-normal">
                  {m}
                </Badge>
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
                    <Pencil />
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
          <div className="relative w-full sm:max-w-xs">
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
            <Button onClick={onCreate}>
              <Plus />
              新增
            </Button>
          </div>
        </div>
      )}
    />
  );
}

// 厂商 新增 / 编辑 抽屉
function ProviderFormSheet({
  open,
  initial,
  onOpenChange,
  onSubmit,
}: {
  open: boolean;
  initial: Provider | null;
  onOpenChange: (open: boolean) => void;
  onSubmit: (provider: Provider) => void;
}) {
  const isEdit = initial !== null;
  const [code, setCode] = useState(initial?.code ?? generateCode("PRV"));
  const [name, setName] = useState(initial?.name ?? "");
  const [apiUrl, setApiUrl] = useState(initial?.apiUrl ?? "");
  const [apiKey, setApiKey] = useState(initial?.apiKey ?? "");
  const [models, setModels] = useState(initial?.models ?? "");
  const [showKey, setShowKey] = useState(false);
  const [submitted, setSubmitted] = useState(false);

  function handleSubmit(event: FormEvent) {
    event.preventDefault();
    setSubmitted(true);
    // 必填项为空时改为字段下方提示,不再用顶部红框
    if (!name.trim() || !apiUrl.trim() || !apiKey.trim() || !models.trim()) {
      return;
    }
    onSubmit({
      id: initial?.id ?? crypto.randomUUID(),
      code,
      name: name.trim(),
      apiUrl: apiUrl.trim(),
      apiKey: apiKey.trim(),
      models: models.trim(),
    });
  }

  // 用户改动过任一字段(编码自动生成不计入)即视为已编辑,阻止误关
  const isDirty =
    name !== (initial?.name ?? "") ||
    apiUrl !== (initial?.apiUrl ?? "") ||
    apiKey !== (initial?.apiKey ?? "") ||
    models !== (initial?.models ?? "");

  return (
    <Sheet open={open} onOpenChange={onOpenChange}>
      <SheetContent
        className="flex w-full flex-col gap-0 p-0 sm:max-w-lg"
        blockClose={isDirty}
      >
        <SheetHeader className="border-b">
          <SheetTitle>{isEdit ? "编辑厂商" : "新增厂商"}</SheetTitle>
          <SheetDescription>
            配置大模型厂商的接口与可用模型,供语音转写、意向分析引用。
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
                  厂商名称 <RequiredMark />
                </Label>
                <Input
                  id="provider-name"
                  placeholder="如 OpenAI / DeepSeek"
                  value={name}
                  onChange={(e) => setName(e.target.value)}
                  aria-invalid={submitted && !name.trim()}
                  autoFocus
                />
                <FieldError
                  show={submitted && !name.trim()}
                  message="厂商名称不可为空"
                />
              </div>
              <div className="space-y-1.5">
                <Label htmlFor="provider-code">编码</Label>
                <CodeField
                  id="provider-code"
                  value={code}
                  onRegenerate={() => setCode(generateCode("PRV"))}
                />
                <p className="text-xs text-muted-foreground">
                  系统自动生成,可刷新或复制
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
              <div className="space-y-1.5">
                <Textarea
                  id="provider-models"
                  className="min-h-68"
                  placeholder={"每行一个模型,例如:\ngpt-4o\ngpt-4o-mini"}
                  value={models}
                  onChange={(e) => setModels(e.target.value)}
                  aria-invalid={submitted && !models.trim()}
                />
                <p className="text-xs text-muted-foreground">
                  每行一个模型,供语音转写 / 意向分析选择
                </p>
                <FieldError
                  show={submitted && !models.trim()}
                  message="请至少填写一个可用模型"
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

// 提示词 新增 / 编辑 抽屉
function PromptFormSheet({
  open,
  initial,
  onOpenChange,
  onSubmit,
}: {
  open: boolean;
  initial: Prompt | null;
  onOpenChange: (open: boolean) => void;
  onSubmit: (prompt: Prompt) => void;
}) {
  const isEdit = initial !== null;
  const [code, setCode] = useState(initial?.code ?? generateCode("PRM"));
  const [name, setName] = useState(initial?.name ?? "");
  const [content, setContent] = useState(initial?.content ?? "");
  const [submitted, setSubmitted] = useState(false);

  function handleSubmit(event: FormEvent) {
    event.preventDefault();
    setSubmitted(true);
    if (!name.trim() || !content.trim()) {
      return;
    }
    onSubmit({
      id: initial?.id ?? crypto.randomUUID(),
      code,
      name: name.trim(),
      content: content.trim(),
    });
  }

  // 用户改动过名称或内容(编码自动生成不计入)即视为已编辑,阻止误关
  const isDirty =
    name !== (initial?.name ?? "") || content !== (initial?.content ?? "");

  return (
    <Sheet open={open} onOpenChange={onOpenChange}>
      <SheetContent
        className="flex w-full flex-col gap-0 p-0 sm:max-w-2xl"
        blockClose={isDirty}
      >
        <SheetHeader className="border-b">
          <SheetTitle>{isEdit ? "编辑提示词" : "新增提示词"}</SheetTitle>
          <SheetDescription>提示词将作为大模型分析任务的指令。</SheetDescription>
        </SheetHeader>
        <form onSubmit={handleSubmit} className="flex min-h-0 flex-1 flex-col">
          <div className="flex-1 space-y-4 overflow-y-auto p-5">
            <div className="space-y-1.5">
              <Label htmlFor="prompt-name">
                名称 <RequiredMark />
              </Label>
              <Input
                id="prompt-name"
                placeholder="如:意向分析 / 内容摘要"
                value={name}
                onChange={(e) => setName(e.target.value)}
                aria-invalid={submitted && !name.trim()}
                autoFocus
              />
              <FieldError
                show={submitted && !name.trim()}
                message="名称不可为空"
              />
            </div>
            <div className="space-y-1.5">
              <Label htmlFor="prompt-code">编码</Label>
              <CodeField
                id="prompt-code"
                value={code}
                onRegenerate={() => setCode(generateCode("PRM"))}
              />
              <p className="text-xs text-muted-foreground">系统自动生成,可刷新</p>
            </div>
            <div className="space-y-1.5">
              <Label htmlFor="prompt-content">
                内容 <RequiredMark />
              </Label>
              <Textarea
                id="prompt-content"
                className="min-h-128"
                placeholder="输入提示词内容,作为大模型分析任务的指令,例如:请根据以下用户评论判断其购买意向(高/中/低),并给出理由。"
                value={content}
                onChange={(e) => setContent(e.target.value)}
                aria-invalid={submitted && !content.trim()}
              />
              <FieldError
                show={submitted && !content.trim()}
                message="内容不可为空"
              />
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
