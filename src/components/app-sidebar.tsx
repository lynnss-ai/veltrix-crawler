import { useEffect, useState, type FormEvent } from "react";
import {
  CalendarClock,
  ChevronsUpDown,
  Clapperboard,
  Contact,
  Database,
  FileStack,
  FolderKanban,
  Grip,
  Image,
  Images,
  MessageSquare,
  KeyRound,
  LayoutDashboard,
  LogOut,
  MoreHorizontal,
  Radar,
  Rocket,
  Settings,
  SquarePen,
  Tags,
  Trash2,
  UserCog,
  UserRound,
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
import { toast } from "sonner";
import { cn } from "@/lib/utils";
import { useWorkspaceOrder } from "@/hooks/use-workspace-order";
import { useChat } from "@/components/chat-context";
import { api, type ConversationView } from "@/lib/api";

// 两级导航:分组 + 带图标子页面。
export type PageKey =
  | "dashboard"
  | "collect-tasks"
  | "accounts"
  | "assets-all"
  | "assets-content"
  | "assets-image"
  | "assets-comment"
  | "assets-author"
  | "industry"
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
    ],
  },
  {
    title: "内容资产",
    items: [
      { key: "assets-all", label: "全量库", icon: Database },
      { key: "assets-content", label: "内容库", icon: FileStack },
      { key: "assets-image", label: "图片库", icon: Images },
      { key: "assets-comment", label: "评论库", icon: MessageSquare },
      { key: "assets-author", label: "作者库", icon: UserRound },
    ],
  },
  {
    title: "基础设施",
    items: [
      { key: "industry", label: "行业类别", icon: Tags },
      { key: "accounts", label: "平台账号", icon: Users },
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
  { key: "crawler", name: "协作平台", icon: Radar, current: true },
  { key: "video", name: "视频创作", icon: Clapperboard },
  { key: "image", name: "图片创作", icon: Image },
  { key: "publish", name: "发布服务", icon: Rocket },
];

// 顶层工作区分类:management(当前采集管理)、chat(对话)、cowork(协作);后两者暂为占位
export type Workspace = "management" | "chat" | "cowork";

// 工作区元数据(标签固定);展示顺序由 useWorkspaceOrder 控制,可在系统配置调整。
export const WORKSPACES: { key: Workspace; label: string }[] = [
  { key: "management", label: "营销" },
  { key: "chat", label: "对话" },
  { key: "cowork", label: "协作" },
];

// 各工作区的导航菜单;management 沿用现有 MENU_GROUPS,其余先占位
const WORKSPACE_MENUS: Record<Workspace, MenuGroup[]> = {
  management: MENU_GROUPS,
  // 对话工作区的会话列表改为侧栏动态渲染(见 ChatConversationList);
  // 这里仅保留 chat-sessions 作为默认页/面包屑锚点
  chat: [
    {
      title: "对话",
      items: [{ key: "chat-sessions", label: "会话", icon: MessageSquare }],
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

// 把会话按最近更新时间分桶:今天 / 昨天 / 近 7 天 / 更早(后端已按 updatedAt 倒序,桶内保持有序)
function groupConversationsByTime(
  conversations: ConversationView[],
): { label: string; items: ConversationView[] }[] {
  const now = new Date();
  const todayStart = new Date(
    now.getFullYear(),
    now.getMonth(),
    now.getDate(),
  ).getTime() / 1000;
  const yesterdayStart = todayStart - 86400;
  const weekStart = todayStart - 6 * 86400;
  const buckets: Record<string, ConversationView[]> = {
    今天: [],
    昨天: [],
    "近 7 天": [],
    更早: [],
  };
  for (const c of conversations) {
    if (c.updatedAt >= todayStart) buckets["今天"].push(c);
    else if (c.updatedAt >= yesterdayStart) buckets["昨天"].push(c);
    else if (c.updatedAt >= weekStart) buckets["近 7 天"].push(c);
    else buckets["更早"].push(c);
  }
  return ["今天", "昨天", "近 7 天", "更早"]
    .map((label) => ({ label, items: buckets[label] }))
    .filter((g) => g.items.length > 0);
}

// 对话工作区侧栏:新对话按钮 + 历史会话(按时间分组,数据来自 ChatProvider)。
// 点会话切到对话页并设为当前;每项可重命名/删除。
function ChatConversationList({
  active,
  onChange,
}: {
  active: PageKey;
  onChange: (key: PageKey) => void;
}) {
  const { conversations, activeId, setActiveId, reload } = useChat();
  const onChat = active === "chat-sessions";
  const groups = groupConversationsByTime(conversations);

  async function handleRename(c: ConversationView) {
    const next = window.prompt("重命名会话", c.title);
    if (next == null) return;
    const title = next.trim();
    if (!title || title === c.title) return;
    try {
      await api.renameConversation(c.id, title);
      await reload();
    } catch (e) {
      toast.error(`重命名失败: ${e}`);
    }
  }

  async function handleDelete(c: ConversationView) {
    try {
      await api.deleteConversation(c.id);
      if (activeId === c.id) setActiveId(null);
      await reload();
      toast.success("已删除会话");
    } catch (e) {
      toast.error(`删除失败: ${e}`);
    }
  }

  function renderItem(c: ConversationView) {
    const isActive = onChat && activeId === c.id;
    return (
      <SidebarMenuItem key={c.id} className="group/conv">
        <SidebarMenuButton
          isActive={isActive}
          tooltip={c.title}
          onClick={() => {
            setActiveId(c.id);
            onChange("chat-sessions");
          }}
          className="pr-7 data-active:bg-primary/10 data-active:font-medium data-active:text-primary data-active:shadow-[inset_2px_0_0_var(--primary)] data-active:[&_svg]:text-primary"
        >
          <MessageSquare />
          <span className="truncate">{c.title}</span>
        </SidebarMenuButton>
        <DropdownMenu>
          <DropdownMenuTrigger asChild>
            <button
              type="button"
              className="absolute right-1 top-1/2 -translate-y-1/2 rounded p-0.5 text-muted-foreground opacity-0 transition-opacity hover:bg-sidebar-accent hover:text-foreground group-hover/conv:opacity-100 data-[state=open]:opacity-100"
            >
              <MoreHorizontal className="size-4" />
            </button>
          </DropdownMenuTrigger>
          <DropdownMenuContent align="end">
            <DropdownMenuItem onClick={() => handleRename(c)}>
              <SquarePen className="size-4" />
              重命名
            </DropdownMenuItem>
            <DropdownMenuItem
              variant="destructive"
              onClick={() => handleDelete(c)}
            >
              <Trash2 className="size-4" />
              删除
            </DropdownMenuItem>
          </DropdownMenuContent>
        </DropdownMenu>
      </SidebarMenuItem>
    );
  }

  return (
    <div className="flex min-h-0 flex-1 flex-col">
      {/* 新对话:黑白低调菜单项(参考 Claude),悬停浅底,无彩色填充 */}
      <div className="p-2 pb-1">
        <button
          type="button"
          onClick={() => {
            setActiveId(null);
            onChange("chat-sessions");
          }}
          className="flex w-full items-center gap-2 rounded-md px-2 py-2 text-sm font-medium text-sidebar-foreground transition-colors hover:bg-sidebar-accent hover:text-sidebar-accent-foreground"
        >
          <SquarePen className="size-4" />
          新对话
        </button>
      </div>

      {/* 历史对话:按时间分组,滚动区 */}
      <div className="veltrix-thin-scrollbar min-h-0 flex-1 overflow-y-auto px-1 pb-2">
        {conversations.length === 0 ? (
          <div className="flex flex-col items-center gap-2 px-2 py-10 text-center text-muted-foreground">
            <MessageSquare className="size-6 opacity-40" />
            <span className="text-xs">暂无历史对话</span>
            <span className="text-[11px] opacity-70">
              点「新对话」开始
            </span>
          </div>
        ) : (
          groups.map((g) => (
            <SidebarGroup key={g.label} className="px-1 py-1">
              <SidebarGroupLabel className="px-2 text-[11px]">
                {g.label}
              </SidebarGroupLabel>
              <SidebarGroupContent>
                <SidebarMenu>{g.items.map(renderItem)}</SidebarMenu>
              </SidebarGroupContent>
            </SidebarGroup>
          ))
        )}
      </div>
    </div>
  );
}

interface AppSidebarProps {
  workspace: Workspace;
  onWorkspaceChange: (workspace: Workspace) => void;
  active: PageKey;
  onChange: (key: PageKey) => void;
  user: string;
  onLogout: () => void;
}

export function AppSidebar({
  workspace,
  onWorkspaceChange,
  active,
  onChange,
  user,
  onLogout,
}: AppSidebarProps) {
  const [changePwdOpen, setChangePwdOpen] = useState(false);
  // 工作区切换标签的展示顺序(系统配置可调);按保存顺序排列,缺失项忽略
  const [wsOrder] = useWorkspaceOrder();
  const orderedWorkspaces = wsOrder
    .map((key) => WORKSPACES.find((w) => w.key === key))
    .filter((w): w is (typeof WORKSPACES)[number] => Boolean(w));

  return (
    // 侧栏固定容器默认 top-0/h-svh,会顶到自定义标题栏后面;
    // 这里按标题栏高度 --titlebar-h 下移并缩高,使其从标题栏下方开始
    <Sidebar
      collapsible="icon"
      className="top-(--titlebar-h)! h-[calc(100svh-var(--titlebar-h))]!"
    >
      <SidebarHeader>
        <SidebarMenu>
          <SidebarMenuItem className="flex items-center gap-1">
            <SidebarMenuButton size="lg" className="cursor-default">
              <div className="flex aspect-square size-8 items-center justify-center rounded-lg bg-primary text-primary-foreground">
                <Radar className="size-4" />
              </div>
              <div className="grid flex-1 text-left text-sm leading-tight">
                <span className="truncate font-semibold">VeltrixLoop</span>
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
                  const isCurrent = !!product.current;
                  return (
                    <DropdownMenuItem
                      key={product.key}
                      disabled={isCurrent}
                      // disabled 默认会半透明,这里强制保留全不透明,让颜色高亮更明显
                      className={
                        isCurrent
                          ? "data-[disabled]:opacity-100 text-primary focus:text-primary bg-primary/10"
                          : ""
                      }
                      onClick={() => {
                        if (!isCurrent) {
                          toast.info(`${product.name} 即将上线`);
                        }
                      }}
                    >
                      <Icon className={isCurrent ? "text-primary" : ""} />
                      <span className="flex-1">{product.name}</span>
                      {isCurrent && (
                        <span className="rounded bg-primary/15 px-1.5 py-0.5 text-[10px] font-medium text-primary">
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

        {/* 工作区分类切换:营销 / 对话 / 协作(顺序可在系统配置调整,折叠态隐藏) */}
        <div className="-mb-2 flex gap-1 rounded-lg bg-sidebar-accent/50 p-1 group-data-[collapsible=icon]:hidden">
          {orderedWorkspaces.map((ws) => (
            <button
              key={ws.key}
              type="button"
              onClick={() => onWorkspaceChange(ws.key)}
              className={cn(
                "flex-1 rounded-md px-2 py-1.5 text-xs font-medium transition-colors",
                workspace === ws.key
                  ? "bg-background text-primary shadow-sm"
                  : "text-muted-foreground hover:text-foreground",
              )}
            >
              {ws.label}
            </button>
          ))}
        </div>
      </SidebarHeader>

      <SidebarContent className="group-data-[collapsible=icon]:overflow-y-auto">
        {/* 对话工作区:侧栏渲染会话列表(新对话 + 历史),取代静态菜单 */}
        {workspace === "chat" ? (
          <ChatConversationList active={active} onChange={onChange} />
        ) : (
          WORKSPACE_MENUS[workspace].map((group) => (
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
                        // 选中态用主题色高亮(左侧色条 + 主色文字/图标),与未选中明显区分
                        className="data-active:bg-primary/10 data-active:font-semibold data-active:text-primary data-active:shadow-[inset_2px_0_0_var(--primary)] data-active:hover:bg-primary/15 data-active:hover:text-primary data-active:[&_svg]:text-primary"
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
          ))
        )}
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
                className="w-[var(--radix-dropdown-menu-trigger-width)] p-2"
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
                  className="gap-2.5 py-2 cursor-pointer"
                  onClick={() => setChangePwdOpen(true)}
                >
                  <KeyRound />
                  修改密码
                </DropdownMenuItem>
                <DropdownMenuItem
                  className="gap-2.5 py-2 cursor-pointer"
                  onClick={() => onChange("system-config")}
                >
                  <Settings />
                  系统设置
                </DropdownMenuItem>
                <DropdownMenuSeparator className="my-1.5" />
                <DropdownMenuItem
                  className="gap-2.5 py-2 cursor-pointer text-destructive focus:text-destructive focus:bg-destructive/10 [&_svg]:text-destructive"
                  onClick={onLogout}
                >
                  <LogOut />
                  退出登录
                </DropdownMenuItem>
              </DropdownMenuContent>
            </DropdownMenu>

          </SidebarMenuItem>
        </SidebarMenu>
      </SidebarFooter>

      <ChangePasswordSheet
        open={changePwdOpen}
        onOpenChange={setChangePwdOpen}
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

