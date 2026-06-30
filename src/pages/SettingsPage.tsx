import {
  useEffect,
  useMemo,
  useRef,
  useState,
  type FormEvent,
} from "react";
import { SECTION_GROUPS, CLEAR_CONFIRM_TEXT, formatBytes } from "./settings-meta";
import type { SectionKey, Provider } from "./settings-meta";
import { SettingsCard, Row } from "./settings-shared";
import { ProvidersSection, ProviderFormSheet } from "./settings-providers";
import { Check, ExternalLink, FolderOpen, GripVertical, Loader2, TriangleAlert, Unplug, X } from "lucide-react";
import { open } from "@tauri-apps/plugin-dialog";
import { openPath, revealItemInDir } from "@tauri-apps/plugin-opener";
import { SimpleTooltip } from "@/components/SimpleTooltip";
import { FORM_CONTROL_SIZING } from "@/lib/form-sizing";
import { toast } from "sonner";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import {
  api,
  type AppConfig,
  type CloudConfigView,
  type CloudConnectionState,
  type RoleModelConfig,
} from "@/lib/api";
import { WORKSPACES, type Workspace } from "@/components/app-sidebar";
import { useWorkspaceOrder } from "@/hooks/use-workspace-order";
import { ErrorBanner } from "@/components/ErrorBanner";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Switch } from "@/components/ui/switch";
import { Textarea } from "@/components/ui/textarea";
import {
  Card,
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


export function SettingsPage() {
  const [active, setActive] = useState<SectionKey>("general");
  const [cfg, setCfg] = useState<AppConfig | null>(null);
  const [error, setError] = useState<string | null>(null);

  const [providers, setProviders] = useState<Provider[]>([]);
  // 厂商预设 + 能力(code/name/apiUrl/chat/asr,后端单一真相源):
  // 供新增厂商下拉与「语音转写」按 ASR 过滤
  const [caps, setCaps] = useState<
    {
      code: string;
      name: string;
      apiUrl: string;
      chat: boolean;
      asr: boolean;
    }[]
  >([]);

  const [clearOpen, setClearOpen] = useState(false);
  const [clearText, setClearText] = useState("");
  const [clearPassword, setClearPassword] = useState("");
  // 是否连同媒体资源文件一并清空(默认清,关掉则只清库、保留已下载素材)
  const [clearMedia, setClearMedia] = useState(true);
  const [clearing, setClearing] = useState(false);
  // 清空成功后自增,作为 GeneralSection 重新拉取数据库大小的触发器
  const [generalRefreshKey, setGeneralRefreshKey] = useState(0);
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

  async function handleClearData() {
    setClearing(true);
    try {
      await api.clearBusinessData(clearPassword, clearMedia);
      setClearOpen(false);
      setClearText("");
      setClearPassword("");
      setGeneralRefreshKey((key) => key + 1);
      toast.success("业务数据已清空");
    } catch (e) {
      // 密码错误等后端校验失败:保留对话框,提示原因供用户重试
      toast.error(`清空失败: ${e}`);
    } finally {
      setClearing(false);
    }
  }
  useEffect(() => {
    api
      .getAppConfig()
      .then(setCfg)
      .catch((e) => setError(String(e)));
    reloadProviders();
    api
      .listProviderCapabilities()
      .then(setCaps)
      .catch((e) => console.warn("加载模型能力列表失败:", e));
  }, []);

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
        {active === "providers" ? (
          <div
            key={active}
            className="flex min-h-0 min-w-0 flex-1 flex-col duration-200 animate-in fade-in-50"
          >
            {active === "providers" && (
              <ProvidersSection
                providers={providers}
                presetCount={caps.length}
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
            
          </div>
        ) : (
          <div
            key={active}
            className="min-h-0 min-w-0 flex-1 space-y-4 overflow-auto p-0.5 duration-200 animate-in fade-in-50"
          >
            {active === "general" && (
              <GeneralSection
                cfg={cfg}
                refreshKey={generalRefreshKey}
                onClearData={() => setClearOpen(true)}
              />
            )}
            {active === "remote-control" && <RemoteControlSection />}
            {active === "transcription" && (
              <TranscriptionSection initial={cfg?.transcription} />
            )}
            {active === "role-models" && (
              <RoleModelSection providers={providers} />
            )}
            {active === "intent" && (
              <IntentSection initial={cfg?.intent} />
            )}
            {active === "obsidian" && <ObsidianSection />}
          </div>
        )}
      </div>

      

      <ProviderFormSheet
        key={isProviderFormOpen ? (providerForm?.id ?? "new-provider") : "idle"}
        open={isProviderFormOpen}
        initial={providerForm}
        providers={providers}
        presets={caps}
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
                {clearMedia ? (
                  <X className="size-3.5 shrink-0 text-destructive" />
                ) : (
                  <Check className="size-3.5 shrink-0 text-emerald-500" />
                )}
                存储路径下的媒体 / 图片等文件
                {!clearMedia && "(保留)"}
              </li>
            </ul>
            {/* 可选项:是否连同媒体资源文件一并清空 */}
            <label className="mt-2.5 flex cursor-pointer items-center gap-2 border-t border-destructive/15 pt-2.5 text-xs text-muted-foreground">
              <Switch checked={clearMedia} onCheckedChange={setClearMedia} />
              同时清空媒体资源文件
            </label>
            <div className="mt-2 flex items-center gap-2 text-xs text-muted-foreground">
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
            <AlertDialogCancel disabled={clearing}>取消</AlertDialogCancel>
            <AlertDialogAction
              disabled={
                clearing ||
                clearText.trim() !== CLEAR_CONFIRM_TEXT ||
                clearPassword.length === 0
              }
              onClick={(e) => {
                // 阻止 AlertDialog 默认的点击即关闭:校验失败时需保留对话框供重试
                e.preventDefault();
                void handleClearData();
              }}
              className="bg-destructive text-white hover:bg-destructive/90"
            >
              {clearing && <Loader2 className="size-4 animate-spin" />}
              确认清空
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
function WorkspaceOrderEditor() {
  const [order, setOrder] = useWorkspaceOrder();
  // 拖动期间的本地工作副本;非拖动态与 order 同步
  const [items, setItems] = useState<Workspace[]>(order);
  const [draggingKey, setDraggingKey] = useState<string | null>(null);
  const draggingRef = useRef<string | null>(null);
  const itemsRef = useRef<Workspace[]>(order);
  const listRef = useRef<HTMLUListElement>(null);

  // 外部改了顺序且当前不在拖动时,同步本地副本
  useEffect(() => {
    if (!draggingRef.current) {
      setItems(order);
      itemsRef.current = order;
    }
  }, [order]);

  const labelOf = (key: string) =>
    WORKSPACES.find((w) => w.key === key)?.label ?? key;

  function startDrag(e: React.PointerEvent<HTMLLIElement>, key: string) {
    if (e.button !== 0) return; // 仅左键
    e.currentTarget.setPointerCapture(e.pointerId); // 后续 move/up 锁定到本行
    draggingRef.current = key;
    setDraggingKey(key);
  }

  function onMove(e: React.PointerEvent<HTMLLIElement>) {
    const key = draggingRef.current;
    const listEl = listRef.current;
    if (!key || !listEl) return;
    // 按指针 Y 命中目标行(过各行中线即换位)
    const rows = Array.from(listEl.children) as HTMLElement[];
    let target = rows.length - 1;
    for (let i = 0; i < rows.length; i++) {
      const r = rows[i].getBoundingClientRect();
      if (e.clientY < (r.top + r.bottom) / 2) {
        target = i;
        break;
      }
    }
    setItems((prev) => {
      const from = prev.indexOf(key as Workspace);
      if (from < 0 || from === target) return prev;
      const next = [...prev];
      const [moved] = next.splice(from, 1);
      next.splice(target, 0, moved);
      itemsRef.current = next;
      return next;
    });
  }

  function endDrag() {
    if (draggingRef.current) {
      // 顺序确有变化才持久化并提示(原地松手不打扰)
      const changed = itemsRef.current.some((k, i) => k !== order[i]);
      if (changed) {
        setOrder(itemsRef.current);
        toast.success("菜单顺序已更新");
      }
    }
    draggingRef.current = null;
    setDraggingKey(null);
  }

  return (
    <ul ref={listRef} className="space-y-2 select-none">
      {items.map((key, idx) => (
        <li
          key={key}
          onPointerDown={(e) => startDrag(e, key)}
          onPointerMove={onMove}
          onPointerUp={endDrag}
          onPointerCancel={endDrag}
          className={`flex touch-none items-center gap-2 rounded-md border bg-card px-3 py-2 transition-colors ${
            draggingKey === key
              ? "cursor-grabbing border-primary bg-primary/5 shadow-sm"
              : "cursor-grab"
          }`}
        >
          <GripVertical className="size-4 shrink-0 text-muted-foreground" />
          <span className="w-5 text-center font-mono text-xs text-muted-foreground">
            {idx + 1}
          </span>
          <span className="flex-1 text-sm font-medium">{labelOf(key)}</span>
        </li>
      ))}
    </ul>
  );
}

// 必填字段标记
// 管理员级配置:URL 通常在部署时一次性配好;手机端配对在侧栏「远程控制」弹窗发起
function RemoteControlSection() {
  const [cfg, setCfg] = useState<CloudConfigView | null>(null);
  const [state, setState] = useState<CloudConnectionState | null>(null);
  const [baseUrlInput, setBaseUrlInput] = useState("");
  const [loginUser, setLoginUser] = useState("");
  const [loginPwd, setLoginPwd] = useState("");
  const [loggingIn, setLoggingIn] = useState(false);

  const refresh = async () => {
    try {
      const [c, s] = await Promise.all([
        api.cloudGetConfig(),
        api.cloudGetStatus(),
      ]);
      setCfg(c);
      setState(s);
      setBaseUrlInput(c.base_url);
    } catch (e) {
      toast.error(`加载远程配置失败: ${e}`);
    }
  };

  useEffect(() => {
    void refresh();
    // 连接态可能因 WS 重连变化,轻量轮询;后续可换 Tauri Event 推
    const t = setInterval(() => {
      api.cloudGetStatus().then(setState).catch((e) => console.warn("获取云端状态失败:", e));
    }, 5000);
    return () => clearInterval(t);
  }, []);

  const urlDirty = baseUrlInput !== (cfg?.base_url ?? "");
  const loggedIn = !!cfg?.user_token;
  const paired = !!cfg?.pc_token;

  async function saveBaseUrl() {
    const url = baseUrlInput.trim();
    if (url && !/^https?:\/\//i.test(url)) {
      toast.error("云端地址必须以 http(s):// 开头");
      return;
    }
    try {
      await api.cloudSaveBaseUrl(url);
      toast.success("云端地址已保存");
      await refresh();
    } catch (e) {
      toast.error(`保存失败: ${e}`);
    }
  }

  async function doLogin(e: FormEvent) {
    e.preventDefault();
    if (!loginUser || !loginPwd) return;
    setLoggingIn(true);
    try {
      await api.cloudLogin(loginUser, loginPwd);
      setLoginPwd("");
      toast.success("登录成功");
      await refresh();
    } catch (e) {
      toast.error(`登录失败: ${e}`);
    } finally {
      setLoggingIn(false);
    }
  }

  async function disconnect() {
    try {
      await api.cloudDisconnect();
      toast.success("已断开远程连接");
      await refresh();
    } catch (e) {
      toast.error(`断开失败: ${e}`);
    }
  }

  return (
    <>
      <SettingsCard
        title="云端地址"
        description="中转服务部署后填写一次;PC 通过此地址注册与上报状态。"
        dirty={urlDirty}
        onSave={saveBaseUrl}
      >
        <div className="space-y-1.5">
          <Label htmlFor="cloud-base-url">中转服务 URL</Label>
          <Input
            id="cloud-base-url"
            placeholder="https://veltrix.example.com"
            value={baseUrlInput}
            onChange={(e) => setBaseUrlInput(e.target.value)}
          />
          {cfg?.device_id && (
            <p className="text-xs text-muted-foreground">
              本机设备 ID:
              <span className="ml-1 font-mono">{cfg.device_id}</span>
            </p>
          )}
        </div>
      </SettingsCard>

      <SettingsCard
        title="云端账号"
        description="登录后即可在侧栏「远程控制」中发起手机配对。"
      >
        {!cfg?.base_url ? (
          <div className="rounded-lg border border-amber-500/30 bg-amber-500/10 px-3 py-2 text-sm text-amber-600 dark:text-amber-400">
            请先配置云端地址。
          </div>
        ) : loggedIn ? (
          <dl className="space-y-3">
            <Row label="登录状态">
              <span className="inline-flex items-center gap-1.5 text-emerald-600 dark:text-emerald-400">
                <Check className="size-4" />
                已登录
              </span>
            </Row>
            <Row label="手机配对">
              {paired ? "已配对" : "未配对(到侧栏「远程控制」扫码绑定)"}
            </Row>
            <Row label="WS 连接">
              {state?.connected ? (
                <span className="text-emerald-600 dark:text-emerald-400">
                  在线
                </span>
              ) : (
                <span className="text-muted-foreground">离线</span>
              )}
            </Row>
            {state?.last_report_at && (
              <Row label="上次上报">
                {new Date(state.last_report_at * 1000).toLocaleString()}
              </Row>
            )}
            {state?.last_error && (
              <Row label="最近错误">
                <span className="text-destructive">{state.last_error}</span>
              </Row>
            )}
            <div className="flex gap-2 pt-1">
              <Button variant="outline" onClick={disconnect}>
                <Unplug />
                断开并清除凭证
              </Button>
            </div>
          </dl>
        ) : (
          <form className="space-y-3" onSubmit={doLogin}>
            <div className="space-y-1.5">
              <Label htmlFor="cloud-user">账号</Label>
              <Input
                id="cloud-user"
                value={loginUser}
                onChange={(e) => setLoginUser(e.target.value)}
              />
            </div>
            <div className="space-y-1.5">
              <Label htmlFor="cloud-pwd">密码</Label>
              <Input
                id="cloud-pwd"
                type="password"
                value={loginPwd}
                onChange={(e) => setLoginPwd(e.target.value)}
              />
            </div>
            <Button type="submit" disabled={loggingIn}>
              {loggingIn && <Loader2 className="size-4 animate-spin" />}
              登录云端
            </Button>
          </form>
        )}
      </SettingsCard>
    </>
  );
}

function GeneralSection({
  cfg,
  refreshKey,
  onClearData,
}: {
  cfg: AppConfig | null;
  refreshKey: number;
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
    // 存储路径展示完整绝对路径(默认 "media"/相对由后端 get_media_root 补全)
    api
      .getMediaRoot()
      .then((p) => {
        setStoragePath(p);
        setStorageBaseline(p);
      })
      .catch((e) => console.warn("获取存储路径失败:", e));
    // refreshKey 变化(清空数据后)重新拉取数据库大小,数字即时反映清空结果
  }, [refreshKey]);
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
        title="菜单顺序"
        description="拖动调整侧边栏顶部工作区(营销 / 对话 / 协作)的排列顺序,松手即时生效。"
      >
        <WorkspaceOrderEditor />
      </SettingsCard>

      <SettingsCard
        title="存储"
        description="采集数据与媒体文件的本地落地目录。"
        dirty={storageDirty}
        onSave={() => {
          api
            .setStoragePath(storagePath.trim())
            .then(() => {
              setStorageBaseline(storagePath);
              toast.success("存储路径已保存");
            })
            .catch((e) => toast.error(String(e)));
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

// Obsidian vault 配置(每用户各自;采集内容同步为 Markdown 写入)
function ObsidianSection() {
  const [vault, setVault] = useState("");
  const [baseline, setBaseline] = useState("");
  useEffect(() => {
    api
      .getObsidianVault()
      .then((v) => {
        setVault(v);
        setBaseline(v);
      })
      .catch((e) => console.warn("获取 Obsidian 仓库路径失败:", e));
  }, []);
  const dirty = vault.trim() !== baseline;
  return (
    <SettingsCard
      title="Obsidian"
      description="配置你的 Obsidian vault 根路径;采集内容可在内容库手动同步,或任务勾选采集完成后自动同步,写入该库的 Veltrix 目录。"
      dirty={dirty}
      onSave={() => {
        const v = vault.trim();
        api
          .setObsidianVault(v)
          .then(() => {
            setBaseline(v);
            setVault(v);
            toast.success("Obsidian 配置已保存");
          })
          .catch((e) => toast.error(`保存失败: ${e}`));
      }}
    >
      <div className="space-y-1.5">
        <Label htmlFor="obsidian-vault">Vault 根路径</Label>
        <div className="flex gap-2">
          <Input
            id="obsidian-vault"
            placeholder="如 D:\Obsidian\MyVault"
            value={vault}
            onChange={(e) => setVault(e.target.value)}
          />
          <Button
            type="button"
            variant="outline"
            className="shrink-0"
            onClick={async () => {
              const selected = await open({ directory: true, multiple: false });
              if (typeof selected === "string") setVault(selected);
            }}
          >
            <FolderOpen />
            选择
          </Button>
          <Button
            type="button"
            variant="outline"
            className="shrink-0"
            disabled={!vault.trim()}
            onClick={() =>
              openPath(vault.trim()).catch((e) =>
                toast.error(`无法打开目录: ${e}`),
              )
            }
          >
            <ExternalLink />
            打开
          </Button>
        </div>
        <p className="text-xs text-muted-foreground">
          内容写入 vault 下的 Veltrix/ 目录,媒体存入 Veltrix/assets/。每个用户各自配置。
        </p>
      </div>
    </SettingsCard>
  );
}
// 语音转写默认接入(未配置过时自动预填,用户只需补 API Key 即可保存生效)
const DEFAULT_ASR_API_URL = "https://api.xiaomimimo.com/v1";
const DEFAULT_ASR_MODEL = "mimo-v2.5-asr";

function TranscriptionSection({
  initial,
}: {
  initial?: { api_url: string; model: string };
}) {
  const [apiUrl, setApiUrl] = useState("");
  const [model, setModel] = useState("");
  const [apiKey, setApiKey] = useState("");
  const [baseUrl, setBaseUrl] = useState("");
  const [baseModel, setBaseModel] = useState("");

  // 回填(api_url/model 来自配置;api_key 存数据库不回显)。
  // 未配置过(空值)时回退到 MiMo 默认,与 base 一致避免一进页面就显示「未保存」。
  useEffect(() => {
    const url = initial?.api_url || DEFAULT_ASR_API_URL;
    const m = initial?.model || DEFAULT_ASR_MODEL;
    setApiUrl(url);
    setModel(m);
    setBaseUrl(url);
    setBaseModel(m);
    setApiKey("");
  }, [initial]);

  const dirty = apiUrl !== baseUrl || model !== baseModel || apiKey.trim() !== "";

  return (
    <SettingsCard
      title="语音转写"
      description="直接配置语音识别(ASR)接口:API 地址、密钥与模型(采集完成后把视频音频转写为文案)。"
      dirty={dirty}
      onSave={() => {
        api
          .setTranscriptionConfig(apiUrl, model, apiKey)
          .then(() => {
            setBaseUrl(apiUrl);
            setBaseModel(model);
            setApiKey("");
            toast.success("语音转写配置已保存");
          })
          .catch((e) => toast.error(`保存失败: ${e}`));
      }}
    >
      <div className="grid gap-4 sm:grid-cols-3">
        <div className="space-y-1.5">
          <Label htmlFor="asr-url">API 地址</Label>
          <Input
            id="asr-url"
            placeholder="https://api.xiaomimimo.com/v1"
            value={apiUrl}
            onChange={(e) => setApiUrl(e.target.value)}
          />
        </div>
        <div className="space-y-1.5">
          <Label htmlFor="asr-key">API Key</Label>
          <Input
            id="asr-key"
            type="password"
            placeholder="留空则不修改已保存的密钥"
            value={apiKey}
            onChange={(e) => setApiKey(e.target.value)}
          />
        </div>
        <div className="space-y-1.5">
          <Label htmlFor="asr-model">模型</Label>
          <Input
            id="asr-model"
            placeholder="mimo-v2.5-asr"
            value={model}
            onChange={(e) => setModel(e.target.value)}
          />
        </div>
      </div>
    </SettingsCard>
  );
}

// 意向分析默认提示词模板(未配置过时自动填入,用户可微调后保存)
const DEFAULT_INTENT_PROMPT =
  "你是一名专业的用户购买意向分析助手。请根据用户在社交平台发布的评论内容,判断其购买/咨询意向的强弱等级。\n" +
  "判定标准(必须从以下四个等级中选择其一):\n" +
  "- 高:明确表达购买意愿,如主动询价、咨询购买/下单方式、索要链接或留下联系方式。\n" +
  "- 中:对产品表现出明显兴趣,如询问功能、规格、效果、对比同类,但尚未明确表达购买。\n" +
  "- 低:仅有泛泛互动或轻微关注,如随意点赞式评论、简单夸赞,无明显购买倾向。\n" +
  "- 无:与购买无关的评论,如纯吐槽、调侃、广告引流、无意义灌水等。\n" +
  "请结合评论语义客观判断,避免主观臆测,并为每条评论给出简要理由。";

// 意向分析常用服务预设:点击快捷填入 API 地址 + 模型(仍可手改)
const INTENT_PROVIDERS: { label: string; apiUrl: string; model: string }[] = [
  { label: "智谱 GLM", apiUrl: "https://open.bigmodel.cn/api/paas/v4", model: "glm-4" },
  { label: "DeepSeek", apiUrl: "https://api.deepseek.com", model: "deepseek-v4-flash" },
  { label: "小米 MiMo", apiUrl: "https://api.xiaomimimo.com/v1", model: "MiMo-V2-Flash" },
];

function IntentSection({
  initial,
}: {
  initial?: {
    api_url: string;
    model: string;
    intent_prompt: string;
    batch_size: number;
  };
}) {
  const [apiUrl, setApiUrl] = useState("");
  const [model, setModel] = useState("");
  const [apiKey, setApiKey] = useState("");
  const [batchSize, setBatchSize] = useState("10");
  const [promptContent, setPromptContent] = useState("");
  const [base, setBase] = useState({ apiUrl: "", model: "", batchSize: "10", prompt: "" });
  // 提示词默认预览(渲染 Markdown),点「编辑」切到源码编辑
  const [editingPrompt, setEditingPrompt] = useState(false);

  useEffect(() => {
    if (!initial) return;
    const url = initial.api_url ?? "";
    const m = initial.model ?? "";
    const bs = String(initial.batch_size ?? 10);
    const content = initial.intent_prompt ?? "";
    setApiUrl(url);
    setModel(m);
    setBatchSize(bs);
    setPromptContent(content || DEFAULT_INTENT_PROMPT);
    setApiKey("");
    setBase({ apiUrl: url, model: m, batchSize: bs, prompt: content });
  }, [initial]);

  const dirty =
    apiUrl !== base.apiUrl ||
    model !== base.model ||
    batchSize !== base.batchSize ||
    promptContent !== base.prompt ||
    apiKey.trim() !== "";

  async function handleSave() {
    if (!promptContent.trim()) {
      toast.error("请填写提示词");
      return;
    }
    try {
      await api.setIntentConfig(apiUrl, model, promptContent, Number(batchSize) || 0, apiKey);
      setBase({ apiUrl, model, batchSize, prompt: promptContent });
      setApiKey("");
      toast.success("意向分析配置已保存");
    } catch (e) {
      toast.error(`保存失败: ${e}`);
    }
  }

  const toggleCls = (on: boolean) =>
    `rounded-md px-2.5 py-1 text-xs font-medium transition-colors ${
      on ? "bg-primary text-primary-foreground" : "text-muted-foreground hover:bg-accent"
    }`;

  return (
    <SettingsCard
      title="AI 意向分析"
      description="大模型接口分析用户评论意向(高/中/低/无)。可点下方服务快捷填入接口地址与模型,再填 Key。"
      dirty={dirty}
      onSave={handleSave}
    >
      <div className="flex flex-wrap gap-2">
        {INTENT_PROVIDERS.map((sp) => (
          <button
            key={sp.label}
            type="button"
            onClick={() => {
              setApiUrl(sp.apiUrl);
              setModel(sp.model);
            }}
            className={`rounded-md border px-3 py-1 text-xs font-medium transition-colors ${
              apiUrl === sp.apiUrl
                ? "border-primary bg-primary/10 text-primary"
                : "border-input text-muted-foreground hover:bg-accent"
            }`}
          >
            {sp.label}
          </button>
        ))}
      </div>
      {/* API 地址 + API Key 一行 */}
      <div className="grid gap-4 sm:grid-cols-2">
        <div className="space-y-1.5">
          <Label htmlFor="intent-url">API 地址</Label>
          <Input
            id="intent-url"
            placeholder="https://api.deepseek.com"
            value={apiUrl}
            onChange={(e) => setApiUrl(e.target.value)}
          />
        </div>
        <div className="space-y-1.5">
          <Label htmlFor="intent-key">API Key</Label>
          <Input
            id="intent-key"
            type="password"
            placeholder="留空则不修改已保存的密钥"
            value={apiKey}
            onChange={(e) => setApiKey(e.target.value)}
          />
        </div>
      </div>
      {/* 模型 + 批处理大小 一行 */}
      <div className="grid gap-4 sm:grid-cols-2">
        <div className="space-y-1.5">
          <Label htmlFor="intent-model">模型</Label>
          <Input
            id="intent-model"
            placeholder="如 deepseek-v4-flash / glm-4"
            value={model}
            onChange={(e) => setModel(e.target.value)}
          />
        </div>
        <div className="space-y-1.5">
          <Label htmlFor="intent-batch">批处理大小</Label>
          <Input
            id="intent-batch"
            type="number"
            min={1}
            value={batchSize}
            onChange={(e) => setBatchSize(e.target.value)}
          />
        </div>
      </div>
      {/* 提示词:Markdown,默认预览,可切编辑 */}
      <div className="space-y-1.5">
        <div className="flex items-center justify-between">
          <Label htmlFor="intent-prompt">提示词(Markdown)</Label>
          <div className="flex gap-1 rounded-md bg-muted/50 p-0.5">
            <button
              type="button"
              className={toggleCls(!editingPrompt)}
              onClick={() => setEditingPrompt(false)}
            >
              预览
            </button>
            <button
              type="button"
              className={toggleCls(editingPrompt)}
              onClick={() => setEditingPrompt(true)}
            >
              编辑
            </button>
          </div>
        </div>
        {editingPrompt ? (
          <Textarea
            id="intent-prompt"
            placeholder="支持 Markdown。例如:请根据以下用户评论判断其购买意向(高/中/低/无),并给出理由。"
            className="min-h-[40vh] max-h-[70vh] resize-y [field-sizing:content]"
            value={promptContent}
            onChange={(e) => setPromptContent(e.target.value)}
          />
        ) : (
          <div className="min-h-[40vh] max-h-[70vh] overflow-auto rounded-md border border-input bg-muted/20 px-3 py-2 text-sm [&_h1]:mb-1 [&_h1]:mt-2 [&_h1]:text-base [&_h1]:font-bold [&_h2]:mb-1 [&_h2]:mt-2 [&_h2]:font-semibold [&_h3]:font-medium [&_p]:my-1 [&_ul]:my-1 [&_ul]:list-disc [&_ul]:pl-5 [&_ol]:my-1 [&_ol]:list-decimal [&_ol]:pl-5 [&_li]:my-0.5 [&_strong]:font-semibold [&_code]:rounded [&_code]:bg-muted [&_code]:px-1 [&_code]:py-0.5 [&_a]:text-primary [&_a]:underline [&_blockquote]:border-l-2 [&_blockquote]:border-border [&_blockquote]:pl-3 [&_blockquote]:text-muted-foreground">
            {promptContent.trim() ? (
              <ReactMarkdown remarkPlugins={[remarkGfm]}>
                {promptContent}
              </ReactMarkdown>
            ) : (
              <span className="text-muted-foreground">
                提示词为空,点「编辑」添加。
              </span>
            )}
          </div>
        )}
      </div>
    </SettingsCard>
  );
}

// 角色模型:一个可选模型 = 厂商 + 模型名(value 用 "providerId::model" 编码,与对话页一致)。
type RoleModelOption = { value: string; label: string };

// 从厂商列表展开出可用模型(有 apiKey + 具备「对话」能力的模型才算可用);编码同对话页 buildModelOptions。
// 角色模型(分类/摘要/套用)都是文本任务,按「对话」能力过滤。
function buildRoleModelOptions(providers: Provider[]): RoleModelOption[] {
  const out: RoleModelOption[] = [];
  for (const p of providers) {
    if (!p.apiKey.trim()) continue;
    for (const spec of p.models) {
      const model = spec.name.trim();
      if (!model || !spec.capabilities.includes("text")) continue;
      out.push({ value: `${p.id}::${model}`, label: `${p.name} · ${model}` });
    }
  }
  return out;
}

// 跟随会话模型(空值)在 Select 里用一个哨兵值表示(Select 不接受空串 value)
const ROLE_MODEL_FOLLOW = "__follow__";

// 单个角色的模型选择行:复用对话页同款 providerId::model 下拉,空=跟随会话模型。
function RoleModelRow({
  label,
  hint,
  options,
  value,
  onChange,
}: {
  label: string;
  hint: string;
  options: RoleModelOption[];
  value: string;
  onChange: (next: string) => void;
}) {
  return (
    <div className="space-y-1.5">
      <Label>{label}</Label>
      <Select
        value={value ? value : ROLE_MODEL_FOLLOW}
        onValueChange={(v) => onChange(v === ROLE_MODEL_FOLLOW ? "" : v)}
      >
        <SelectTrigger className="w-full">
          <SelectValue />
        </SelectTrigger>
        <SelectContent>
          <SelectItem value={ROLE_MODEL_FOLLOW}>跟随会话模型(默认)</SelectItem>
          {options.map((o) => (
            <SelectItem key={o.value} value={o.value}>
              {o.label}
            </SelectItem>
          ))}
        </SelectContent>
      </Select>
      <p className="text-xs text-muted-foreground">{hint}</p>
    </div>
  );
}

// 角色模型:让「杂活」走单独配置的便宜模型,主任务仍走会话绑定模型。
// 仅在「选模型」这层加维度,不改对话 / 编程 Agent 的下游逻辑。
function RoleModelSection({ providers }: { providers: Provider[] }) {
  const options = useMemo(() => buildRoleModelOptions(providers), [providers]);
  const [cfg, setCfg] = useState<RoleModelConfig>({
    classifyModel: "",
    summaryModel: "",
    applyModel: "",
  });
  const [base, setBase] = useState<RoleModelConfig>({
    classifyModel: "",
    summaryModel: "",
    applyModel: "",
  });

  useEffect(() => {
    api
      .getRoleModels()
      .then((r) => {
        setCfg(r);
        setBase(r);
      })
      .catch((e) => console.warn("获取角色模型配置失败:", e));
  }, []);

  const dirty =
    cfg.classifyModel !== base.classifyModel ||
    cfg.summaryModel !== base.summaryModel ||
    cfg.applyModel !== base.applyModel;

  return (
    <SettingsCard
      title="角色模型"
      description="为「杂活」单独指定便宜模型:意图分类、摘要/标题/记忆提取、套用改动。留空则跟随会话当前模型。主对话与编程主循环始终用会话模型,不受此处影响。"
      dirty={dirty}
      onSave={() => {
        api
          .setRoleModels(cfg)
          .then(() => {
            setBase(cfg);
            toast.success("角色模型已保存");
          })
          .catch((e) => toast.error(`保存失败: ${e}`));
      }}
    >
      {options.length === 0 ? (
        <p className="text-sm text-muted-foreground">
          暂无可用模型,请先到「模型厂商」配置 API Key 与模型后再来选择。
        </p>
      ) : (
        <div className="grid gap-4 sm:grid-cols-3">
          <RoleModelRow
            label="意图分类"
            hint="判断首条消息是否编程任务(只回一个词),适合最便宜的小模型。"
            options={options}
            value={cfg.classifyModel}
            onChange={(v) => setCfg((c) => ({ ...c, classifyModel: v }))}
          />
          <RoleModelRow
            label="摘要 / 标题 / 记忆"
            hint="长会话压缩、自动起标题、记忆提取等后台杂活。"
            options={options}
            value={cfg.summaryModel}
            onChange={(v) => setCfg((c) => ({ ...c, summaryModel: v }))}
          />
          <RoleModelRow
            label="套用改动"
            hint="编程 Agent 应用改动场景(预留),暂可留空。"
            options={options}
            value={cfg.applyModel}
            onChange={(v) => setCfg((c) => ({ ...c, applyModel: v }))}
          />
        </div>
      )}
    </SettingsCard>
  );
}

// 模型厂商搜索:匹配名称 / API URL
