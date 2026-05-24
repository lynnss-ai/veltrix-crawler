import { useEffect, useState, type FormEvent, type ReactNode } from "react";
import {
  Bot,
  Boxes,
  CalendarClock,
  ChevronsUpDown,
  Clapperboard,
  Contact,
  FileStack,
  FolderKanban,
  Grip,
  Image,
  Images,
  MessageSquare,
  KeyRound,
  LayoutDashboard,
  LogOut,
  Network,
  Radar,
  RefreshCw,
  Info,
  Loader2,
  Settings,
  Smartphone,
  Tags,
  Unplug,
  UserCog,
  Users,
  type LucideIcon,
} from "lucide-react";
import {
  Sidebar,
  SidebarContent,
  SidebarFooter,
  SidebarGroup,
  SidebarGroupContent,
  SidebarGroupLabel,
  SidebarHeader,
  SidebarMenu,
  SidebarMenuButton,
  SidebarMenuItem,
} from "@/components/ui/sidebar";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import {
  Sheet,
  SheetContent,
  SheetDescription,
  SheetFooter,
  SheetHeader,
  SheetTitle,
} from "@/components/ui/sheet";
import { FieldError } from "@/components/FieldError";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { SimpleTooltip } from "@/components/SimpleTooltip";
import { toast } from "sonner";
import { cn } from "@/lib/utils";

// 两级导航:分组 + 带图标子页面。
export type PageKey =
  | "dashboard"
  | "collect-tasks"
  | "accounts"
  | "assets-all"
  | "assets-content"
  | "assets-image"
  | "industry"
  | "platforms"
  | "customers"
  | "users"
  | "system-config"
  | "chat-sessions"
  | "chat-assistant"
  | "cowork-space"
  | "cowork-team";

interface SubItem {
  key: PageKey;
  label: string;
  icon: LucideIcon;
}

interface MenuGroup {
  title: string;
  items: SubItem[];
}

const MENU_GROUPS: MenuGroup[] = [
  {
    title: "数据看板",
    items: [{ key: "dashboard", label: "数据概览", icon: LayoutDashboard }],
  },
  {
    title: "采集中心",
    items: [
      { key: "collect-tasks", label: "任务调度", icon: CalendarClock },
      { key: "accounts", label: "账号管理", icon: Users },
    ],
  },
  {
    title: "内容资产",
    items: [
      { key: "assets-all", label: "全量库", icon: Boxes },
      { key: "assets-content", label: "内容库", icon: FileStack },
      { key: "assets-image", label: "图片库", icon: Images },
    ],
  },
  {
    title: "基础设施",
    items: [
      { key: "industry", label: "行业类别", icon: Tags },
      { key: "platforms", label: "平台管理", icon: Network },
      { key: "customers", label: "客户管理", icon: Contact },
    ],
  },
  {
    title: "系统管理",
    items: [
      { key: "users", label: "用户管理", icon: UserCog },
      { key: "system-config", label: "系统配置", icon: Settings },
    ],
  },
];

// 产品平台矩阵:同一账号体系下的多个 AI 产品,Logo 旁切换;当前产品为采集,其余占位
const PRODUCT_PLATFORMS: {
  key: string;
  name: string;
  icon: LucideIcon;
  current?: boolean;
}[] = [
  { key: "crawler", name: "数据挖掘", icon: Radar, current: true },
  { key: "video", name: "AI 短视频生成", icon: Clapperboard },
  { key: "image", name: "AI 图片生成", icon: Image },
];

// 顶层工作区分类:management(当前采集管理)、chat(对话)、cowork(协作);后两者暂为占位
export type Workspace = "management" | "chat" | "cowork";

const WORKSPACES: { key: Workspace; label: string }[] = [
  { key: "management", label: "管理" },
  { key: "chat", label: "对话" },
  { key: "cowork", label: "协作" },
];

// 各工作区的导航菜单;management 沿用现有 MENU_GROUPS,其余先占位
const WORKSPACE_MENUS: Record<Workspace, MenuGroup[]> = {
  management: MENU_GROUPS,
  chat: [
    {
      title: "对话",
      items: [
        { key: "chat-sessions", label: "会话", icon: MessageSquare },
        { key: "chat-assistant", label: "智能助手", icon: Bot },
      ],
    },
  ],
  cowork: [
    {
      title: "协作",
      items: [
        { key: "cowork-space", label: "工作空间", icon: FolderKanban },
        { key: "cowork-team", label: "团队成员", icon: Users },
      ],
    },
  ],
};

// 某工作区的默认页(第一个菜单项),切换工作区时跳转到此
export function getWorkspaceDefaultPage(workspace: Workspace): PageKey {
  return WORKSPACE_MENUS[workspace][0].items[0].key;
}

// 根据页面 key 反查所属分组与页面名,用于顶栏面包屑(跨全部工作区)
export function getPageBreadcrumb(key: PageKey): {
  group: string;
  page: string;
} {
  for (const groups of Object.values(WORKSPACE_MENUS)) {
    for (const group of groups) {
      const item = group.items.find((i) => i.key === key);
      if (item) return { group: group.title, page: item.label };
    }
  }
  return { group: "", page: "" };
}

// 远程上报连接状态;后端 RemoteConfig 就绪后由上报模块驱动
export type RemoteStatus = "connected" | "disconnected" | "failed";

const REMOTE_STATUS_META: Record<
  RemoteStatus,
  { label: string; className: string }
> = {
  connected: { label: "远程已连接", className: "text-emerald-500" },
  disconnected: { label: "远程未连接", className: "text-muted-foreground" },
  failed: { label: "远程连接失败", className: "text-destructive" },
};

interface AppSidebarProps {
  workspace: Workspace;
  onWorkspaceChange: (workspace: Workspace) => void;
  active: PageKey;
  onChange: (key: PageKey) => void;
  user: string;
  onLogout: () => void;
  remoteStatus: RemoteStatus;
}

export function AppSidebar({
  workspace,
  onWorkspaceChange,
  active,
  onChange,
  user,
  onLogout,
  remoteStatus,
}: AppSidebarProps) {
  const [changePwdOpen, setChangePwdOpen] = useState(false);
  const [remoteOpen, setRemoteOpen] = useState(false);
  const remoteMeta = REMOTE_STATUS_META[remoteStatus];

  return (
    <Sidebar collapsible="icon">
      <SidebarHeader>
        <SidebarMenu>
          <SidebarMenuItem className="flex items-center gap-1">
            <SidebarMenuButton size="lg" className="cursor-default">
              <div className="flex aspect-square size-8 items-center justify-center rounded-lg bg-primary text-primary-foreground">
                <Radar className="size-4" />
              </div>
              <div className="grid flex-1 text-left text-sm leading-tight">
                <span className="truncate font-semibold">Veltrix</span>
                <span className="truncate text-xs text-muted-foreground">
                  协作平台
                </span>
              </div>
            </SidebarMenuButton>
            {/* 产品平台切换:列出旗下 AI 产品,切换到对应平台(其余产品暂占位) */}
            <DropdownMenu>
              <DropdownMenuTrigger asChild>
                <Button
                  variant="ghost"
                  size="icon"
                  className="size-8 shrink-0 group-data-[collapsible=icon]:hidden"
                  title="切换平台"
                >
                  <Grip className="size-4" />
                  <span className="sr-only">切换平台</span>
                </Button>
              </DropdownMenuTrigger>
              <DropdownMenuContent
                align="start"
                side="right"
                sideOffset={8}
                className="w-56"
              >
                <DropdownMenuLabel>切换平台</DropdownMenuLabel>
                <DropdownMenuSeparator />
                {PRODUCT_PLATFORMS.map((product) => {
                  const Icon = product.icon;
                  return (
                    <DropdownMenuItem
                      key={product.key}
                      disabled={product.current}
                      onClick={() => {
                        if (!product.current) {
                          toast.info(`${product.name} 即将上线`);
                        }
                      }}
                    >
                      <Icon />
                      <span className="flex-1">{product.name}</span>
                      {product.current && (
                        <span className="text-xs text-muted-foreground">
                          当前
                        </span>
                      )}
                    </DropdownMenuItem>
                  );
                })}
              </DropdownMenuContent>
            </DropdownMenu>
          </SidebarMenuItem>
        </SidebarMenu>

        {/* 工作区分类切换:管理 / 对话 / 协作(折叠态隐藏) */}
        <div className="-mb-2 flex gap-1 rounded-lg bg-sidebar-accent/50 p-1 group-data-[collapsible=icon]:hidden">
          {WORKSPACES.map((ws) => (
            <button
              key={ws.key}
              type="button"
              onClick={() => onWorkspaceChange(ws.key)}
              className={cn(
                "flex-1 rounded-md px-2 py-1.5 text-xs font-medium transition-colors",
                workspace === ws.key
                  ? "bg-background text-foreground shadow-sm"
                  : "text-muted-foreground hover:text-foreground",
              )}
            >
              {ws.label}
            </button>
          ))}
        </div>
      </SidebarHeader>

      <SidebarContent>
        {WORKSPACE_MENUS[workspace].map((group) => (
          <SidebarGroup key={group.title}>
            <SidebarGroupLabel>{group.title}</SidebarGroupLabel>
            <SidebarGroupContent>
              <SidebarMenu>
                {group.items.map((item) => {
                  const Icon = item.icon;
                  return (
                    <SidebarMenuItem key={item.key}>
                      <SidebarMenuButton
                        isActive={active === item.key}
                        onClick={() => onChange(item.key)}
                        tooltip={item.label}
                      >
                        <Icon />
                        <span>{item.label}</span>
                      </SidebarMenuButton>
                    </SidebarMenuItem>
                  );
                })}
              </SidebarMenu>
            </SidebarGroupContent>
          </SidebarGroup>
        ))}
      </SidebarContent>

      <SidebarFooter>
        <SidebarMenu>
          <SidebarMenuItem className="flex items-center gap-2">
            <DropdownMenu>
              <DropdownMenuTrigger asChild>
                <SidebarMenuButton
                  size="lg"
                  className="flex-1 data-[state=open]:bg-sidebar-accent data-[state=open]:text-sidebar-accent-foreground"
                >
                  <span className="flex aspect-square size-8 items-center justify-center rounded-lg bg-primary/10 text-xs font-medium text-primary">
                    {user.charAt(0).toUpperCase()}
                  </span>
                  <div className="grid flex-1 text-left text-sm leading-tight">
                    <span className="truncate font-medium">{user}</span>
                    <span className="truncate text-xs text-muted-foreground">
                      管理员
                    </span>
                  </div>
                  <ChevronsUpDown className="size-4 shrink-0 text-muted-foreground" />
                </SidebarMenuButton>
              </DropdownMenuTrigger>
              <DropdownMenuContent
                side="top"
                align="start"
                sideOffset={8}
                className="w-[calc(var(--radix-dropdown-menu-trigger-width)_+_3.5rem)] p-2"
              >
                <DropdownMenuLabel className="p-0 font-normal">
                  <div className="flex items-center gap-2.5 px-1.5 py-2">
                    <span className="flex aspect-square size-9 shrink-0 items-center justify-center rounded-lg bg-primary/10 text-sm font-medium text-primary">
                      {user.charAt(0).toUpperCase()}
                    </span>
                    <div className="grid flex-1 text-left leading-tight">
                      <span className="truncate text-sm font-medium">{user}</span>
                      <span className="truncate text-xs text-muted-foreground">
                        管理员
                      </span>
                    </div>
                  </div>
                </DropdownMenuLabel>
                <DropdownMenuSeparator className="my-1.5" />
                <DropdownMenuItem
                  className="gap-2.5 py-2"
                  onClick={() => setChangePwdOpen(true)}
                >
                  <KeyRound />
                  修改密码
                </DropdownMenuItem>
                <DropdownMenuItem
                  className="gap-2.5 py-2"
                  onClick={() => onChange("system-config")}
                >
                  <Settings />
                  系统设置
                </DropdownMenuItem>
                <DropdownMenuSeparator className="my-1.5" />
                <DropdownMenuItem className="gap-2.5 py-2" onClick={onLogout}>
                  <LogOut />
                  退出登录
                </DropdownMenuItem>
              </DropdownMenuContent>
            </DropdownMenu>

            {/* 远程手机控制:独立于个人中心(不被其点击区覆盖),手机远程控制状态 */}
            <SimpleTooltip content={remoteMeta.label}>
              <button
                type="button"
                onClick={() => setRemoteOpen(true)}
                className={`flex size-12 shrink-0 items-center justify-center rounded-lg border bg-sidebar transition-colors hover:bg-sidebar-accent group-data-[collapsible=icon]:hidden ${remoteMeta.className}`}
              >
                <Smartphone className="size-5" />
                <span className="sr-only">{remoteMeta.label}</span>
              </button>
            </SimpleTooltip>
          </SidebarMenuItem>
        </SidebarMenu>
      </SidebarFooter>

      <ChangePasswordSheet
        open={changePwdOpen}
        onOpenChange={setChangePwdOpen}
      />

      <RemoteConnectDialog
        open={remoteOpen}
        onOpenChange={setRemoteOpen}
        status={remoteStatus}
      />
    </Sidebar>
  );
}

// 修改密码弹窗。提交逻辑当前为前端占位,待后端 change_password 命令就绪后替换。
function ChangePasswordSheet({
  open,
  onOpenChange,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
}) {
  const [oldPwd, setOldPwd] = useState("");
  const [newPwd, setNewPwd] = useState("");
  const [confirm, setConfirm] = useState("");
  const [submitted, setSubmitted] = useState(false);
  const [done, setDone] = useState(false);

  // 每次打开重置表单状态(组件常驻不卸载,需手动清理)
  useEffect(() => {
    if (open) {
      setOldPwd("");
      setNewPwd("");
      setConfirm("");
      setSubmitted(false);
      setDone(false);
    }
  }, [open]);

  const mismatch = confirm.length > 0 && newPwd !== confirm;

  function handleSubmit(event: FormEvent) {
    event.preventDefault();
    setSubmitted(true);
    // 必填为空 / 两次不一致时改为字段下方提示,不再用顶部红框
    if (!oldPwd || !newPwd || !confirm || newPwd !== confirm) {
      return;
    }
    // TODO: invoke("change_password", { oldPwd, newPwd }),由后端校验旧密码并更新哈希
    setDone(true);
    setTimeout(() => onOpenChange(false), 900);
  }

  const dirty = Boolean(oldPwd || newPwd || confirm);

  return (
    <Sheet open={open} onOpenChange={onOpenChange}>
      <SheetContent
        className="flex w-full flex-col gap-0 p-0 sm:max-w-md"
        blockClose={dirty && !done}
      >
        <SheetHeader className="border-b">
          <SheetTitle>修改密码</SheetTitle>
          <SheetDescription>修改成功后下次登录生效。</SheetDescription>
        </SheetHeader>
        {done ? (
          <div className="flex-1 p-5">
            <p className="text-sm text-emerald-600 dark:text-emerald-400">
              密码修改成功(待接后端)
            </p>
          </div>
        ) : (
          <form onSubmit={handleSubmit} className="flex min-h-0 flex-1 flex-col">
            <div className="flex-1 space-y-4 overflow-y-auto p-5">
              <div className="space-y-1.5">
                <Label htmlFor="old-pwd">
                  当前密码 <span className="text-destructive">*</span>
                </Label>
                <Input
                  id="old-pwd"
                  type="password"
                  value={oldPwd}
                  onChange={(e) => setOldPwd(e.target.value)}
                  aria-invalid={submitted && !oldPwd}
                  autoFocus
                />
                <FieldError
                  show={submitted && !oldPwd}
                  message="当前密码不可为空"
                />
              </div>
              <div className="space-y-1.5">
                <Label htmlFor="new-pwd">
                  新密码 <span className="text-destructive">*</span>
                </Label>
                <Input
                  id="new-pwd"
                  type="password"
                  value={newPwd}
                  onChange={(e) => setNewPwd(e.target.value)}
                  aria-invalid={submitted && !newPwd}
                />
                <FieldError
                  show={submitted && !newPwd}
                  message="新密码不可为空"
                />
              </div>
              <div className="space-y-1.5">
                <Label htmlFor="confirm-pwd">
                  确认新密码 <span className="text-destructive">*</span>
                </Label>
                <Input
                  id="confirm-pwd"
                  type="password"
                  value={confirm}
                  onChange={(e) => setConfirm(e.target.value)}
                  aria-invalid={(submitted && !confirm) || mismatch}
                />
                <FieldError
                  show={submitted && !confirm}
                  message="请再次输入新密码"
                />
                <FieldError show={mismatch} message="两次输入的新密码不一致" />
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
              <Button type="submit">确认</Button>
            </SheetFooter>
          </form>
        )}
      </SheetContent>
    </Sheet>
  );
}

// 远程连接二维码占位:确定性伪随机图案 + 三个定位角,仅作示意。
// 真实二维码待后端生成含一次性连接 token 的内容后替换。
function QrPlaceholder({
  seed = 0,
  className = "size-44",
}: {
  seed?: number;
  className?: string;
}) {
  const SIZE = 21;
  const dots: ReactNode[] = [];
  for (let row = 0; row < SIZE; row += 1) {
    for (let col = 0; col < SIZE; col += 1) {
      const inFinder =
        (row < 7 && col < 7) ||
        (row < 7 && col >= SIZE - 7) ||
        (row >= SIZE - 7 && col < 7);
      if (inFinder) continue;
      // 基于坐标 + seed 的确定性散列;seed 变化即重新生成图案(刷新二维码)
      if ((row * 31 + col * 17 + row * col + seed * 13) % 3 === 0) {
        dots.push(
          <rect key={`${row}-${col}`} x={col} y={row} width="1" height="1" />,
        );
      }
    }
  }
  const finders: [number, number][] = [
    [0, 0],
    [SIZE - 7, 0],
    [0, SIZE - 7],
  ];
  return (
    <svg
      viewBox="0 0 21 21"
      shapeRendering="crispEdges"
      className={`${className} text-slate-900`}
    >
      <rect width="21" height="21" fill="white" />
      <g fill="currentColor">{dots}</g>
      {finders.map(([x, y]) => (
        <g key={`${x}-${y}`} fill="currentColor">
          <rect x={x} y={y} width="7" height="7" />
          <rect x={x + 1} y={y + 1} width="5" height="5" fill="white" />
          <rect x={x + 2} y={y + 2} width="3" height="3" />
        </g>
      ))}
    </svg>
  );
}

// 远程连接弹窗:手机 App 扫码连接,远程查看 / 监控本机数据采集。仅允许一台设备。
// 连接态由后端 RemoteConfig 上报驱动;扫码绑定 / 断开当前为前端占位。
function RemoteConnectDialog({
  open,
  onOpenChange,
  status,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  status: RemoteStatus;
}) {
  const isConnected = status === "connected";
  // 连接二维码刷新种子;变化即重新生成图案(真实场景为重新申请一次性连接 token)
  const [qrSeed, setQrSeed] = useState(0);

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="sm:max-w-md">
        <DialogHeader>
          <DialogTitle className="flex items-center gap-2">
            <Smartphone className="size-5" />
            远程控制
          </DialogTitle>
          <DialogDescription>
            使用 Veltrix 手机 App 扫码连接,随时随地远程查看与监控本机的数据采集情况。
          </DialogDescription>
        </DialogHeader>

        {isConnected ? (
          <div className="space-y-3">
            <div className="flex items-center gap-3 rounded-lg border bg-card p-3">
              <div className="flex size-10 shrink-0 items-center justify-center rounded-lg bg-emerald-500/10">
                <Smartphone className="size-5 text-emerald-500" />
              </div>
              <div className="min-w-0 flex-1">
                <div className="flex items-center gap-1.5 font-medium">
                  <span className="truncate">已绑定设备</span>
                  <span className="size-2 shrink-0 rounded-full bg-emerald-500" />
                </div>
                <div className="text-xs text-muted-foreground">
                  在线 · 远程监控中
                </div>
              </div>
            </div>
            <div className="flex gap-2">
              <Button
                variant="outline"
                className="flex-1"
                onClick={() => {
                  // TODO: invoke("remote_reconnect"),重新建立远程会话
                  toast.success("正在刷新重连…(待接后端)");
                }}
              >
                <RefreshCw />
                刷新重连
              </Button>
              <Button
                variant="destructive"
                className="flex-1"
                onClick={() => {
                  // TODO: invoke("remote_disconnect"),断开会话并更新上报状态
                  toast.success("已断开远程连接(待接后端)");
                  onOpenChange(false);
                }}
              >
                <Unplug />
                断开连接
              </Button>
            </div>
          </div>
        ) : (
          <div className="flex flex-col items-center gap-3">
            <div className="rounded-xl border bg-white p-3 shadow-sm">
              <QrPlaceholder seed={qrSeed} />
            </div>
            <div className="flex items-center gap-2 text-sm text-muted-foreground">
              <Loader2 className="size-4 animate-spin" />
              打开手机 App「扫一扫」完成连接
            </div>
            <Button
              variant="outline"
              size="sm"
              onClick={() => {
                // TODO: invoke("remote_refresh_token"),重新申请连接二维码
                setQrSeed((s) => s + 1);
                toast.success("二维码已刷新");
              }}
            >
              <RefreshCw />
              刷新二维码
            </Button>
          </div>
        )}

        {/* App 下载入口:扫码下载手机端 */}
        <div className="flex items-center gap-3 rounded-lg border bg-muted/30 p-3">
          <div className="shrink-0 rounded-md border bg-white p-1.5">
            <QrPlaceholder seed={99} className="size-16" />
          </div>
          <div className="min-w-0 flex-1">
            <div className="text-sm font-medium">下载 Veltrix 手机 App</div>
            <div className="mt-0.5 text-xs text-muted-foreground">
              扫码下载,支持 iOS / Android
            </div>
          </div>
        </div>

        <div className="flex items-start gap-2 rounded-lg bg-muted/50 px-3 py-2 text-xs text-muted-foreground">
          <Info className="mt-0.5 size-3.5 shrink-0" />
          出于数据安全考虑,同一时间仅允许一台设备远程连接。
        </div>
      </DialogContent>
    </Dialog>
  );
}
