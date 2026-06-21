import { useState } from "react";
import {
  Brain,
  CalendarClock,
  ChevronDown,
  ChevronRight,
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
  LayoutDashboard,
  LogOut,
  MoreVertical,
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
import { Button } from "@/components/ui/button";
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
import {
  Dialog,
  DialogContent,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { toast } from "sonner";
import { cn } from "@/lib/utils";
import { useWorkspaceOrder } from "@/hooks/use-workspace-order";
import { useChat } from "@/hooks/use-chat";
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
  | "user-center"
  | "chat-sessions"
  | "chat-assistant"
  | "chat-history"
  | "memory-center"
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
      // 「系统配置」已从侧栏移除,统一从个人中心 → 系统设置进入(见用户下拉菜单)
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
      // 会话列表在侧栏动态渲染(ChatConversationList);这里登记菜单项仅用于默认页与面包屑解析。
      // 记忆管理为整页模块(全局记忆 / 会话记忆),从对话侧栏「记忆管理」入口进入。
      items: [
        { key: "chat-sessions", label: "会话", icon: MessageSquare },
        { key: "memory-center", label: "记忆管理", icon: Brain },
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

// 不在侧栏导航中、但可由个人中心等入口进入的页面,补面包屑(标题栏 H1 取 page)
const OFF_NAV_PAGES: Partial<
  Record<PageKey, { group: string; page: string }>
> = {
  "system-config": { group: "个人中心", page: "系统设置" },
  "user-center": { group: "个人中心", page: "个人中心" },
  "chat-history": { group: "对话", page: "对话记录" },
};

// 根据页面 key 反查所属分组与页面名,用于顶栏面包屑(跨全部工作区);
// 侧栏菜单查不到再回退 OFF_NAV_PAGES(如系统设置)
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
  return OFF_NAV_PAGES[key] ?? { group: "", page: "" };
}

// 是否为脱离侧栏导航的页面(系统设置 / 个人中心):顶栏据此显示关闭按钮、关闭时返回来源页
export function isOffNavPage(key: PageKey): boolean {
  return key in OFF_NAV_PAGES;
}

// 把会话按最近更新时间分桶:今天 / 昨天 / 近 7 天 / 更早(后端已按 updatedAt 倒序,桶内保持有序)
// 「最近对话」侧栏展示上限:超出的与已归档会话都在「对话记录」页(查看更多)管理。
const RECENT_LIMIT = 20;

// 对话工作区侧栏:新对话按钮 + 历史会话(按时间分组,数据来自 ChatProvider)。
// 点会话切到对话页并设为当前;每项可重命名/删除。
function ChatConversationList({
  active,
  onChange,
}: {
  active: PageKey;
  onChange: (key: PageKey) => void;
}) {
  const { conversations, activeId, setActiveId, setPendingAgentType, reload } =
    useChat();
  const onChat = active === "chat-sessions";
  // 最近对话:排除已归档,按更新倒序,只取最近 RECENT_LIMIT 条
  const recent = conversations
    .filter((c) => !c.archived)
    .sort((a, b) => b.updatedAt - a.updatedAt)
    .slice(0, RECENT_LIMIT);

  // 重命名 / 删除会话:自定义弹框(替代原生 prompt 与无确认直删)
  const [renameTarget, setRenameTarget] = useState<ConversationView | null>(
    null,
  );
  const [renameValue, setRenameValue] = useState("");
  const [deleteTarget, setDeleteTarget] = useState<ConversationView | null>(
    null,
  );
  // 「最近对话」分组折叠态(默认展开)
  const [recentCollapsed, setRecentCollapsed] = useState(false);

  // 开新会话:设场景类型 + 清当前会话 + 切到对话页(首条消息时按场景建会话)
  function startNew(agentType: string) {
    setPendingAgentType(agentType);
    setActiveId(null);
    onChange("chat-sessions");
  }

  // 重命名:打开弹框,回填当前标题
  function handleRename(c: ConversationView) {
    setRenameValue(c.title);
    setRenameTarget(c);
  }

  async function submitRename() {
    if (!renameTarget) return;
    const title = renameValue.trim();
    if (!title || title === renameTarget.title) {
      setRenameTarget(null);
      return;
    }
    try {
      await api.renameConversation(renameTarget.id, title);
      setRenameTarget(null);
      await reload();
    } catch (e) {
      toast.error(`重命名失败: ${e}`);
    }
  }

  // 删除:打开二次确认弹框
  function handleDelete(c: ConversationView) {
    setDeleteTarget(c);
  }

  async function confirmDelete() {
    if (!deleteTarget) return;
    const target = deleteTarget;
    try {
      await api.deleteConversation(target.id);
      if (activeId === target.id) setActiveId(null);
      setDeleteTarget(null);
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
          <span className="truncate">{c.title}</span>
        </SidebarMenuButton>
        <DropdownMenu>
          <DropdownMenuTrigger asChild>
            <button
              type="button"
              className="absolute right-1 top-1/2 -translate-y-1/2 rounded p-0.5 text-muted-foreground opacity-0 transition-opacity hover:bg-sidebar-accent hover:text-foreground group-hover/conv:opacity-100 data-[state=open]:opacity-100"
            >
              <MoreVertical className="size-4" />
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
      {/* 新对话 / 新编程会话:黑白低调菜单项(参考 Claude),悬停浅底,无彩色填充 */}
      <div className="p-2 pb-1">
        {/* 新对话:统一入口;进入新会话后在窗口内选择智能体类型(对话 / 编程…),页面随之变形 */}
        <button
          type="button"
          onClick={() => startNew("chat")}
          className="flex w-full items-center gap-2 rounded-md px-2 py-2 text-sm font-medium text-sidebar-foreground transition-colors hover:bg-sidebar-accent hover:text-sidebar-accent-foreground"
        >
          <SquarePen className="size-4" />
          新对话
        </button>
        {/* 记忆管理:整页模块(全局记忆 / 会话记忆),点击进入整页而非弹窗 */}
        <button
          type="button"
          onClick={() => onChange("memory-center")}
          className={cn(
            "mt-0.5 flex w-full items-center gap-2 rounded-md px-2 py-2 text-sm font-medium transition-colors",
            active === "memory-center"
              ? "bg-primary/10 text-primary shadow-[inset_2px_0_0_var(--primary)]"
              : "text-sidebar-foreground hover:bg-sidebar-accent hover:text-sidebar-accent-foreground",
          )}
        >
          <Brain className="size-4" />
          记忆管理
        </button>
      </div>

      {/* 最近对话:仅最近 20 条(排除归档),更多 / 搜索 / 批量管理走「查看更多」对话记录页 */}
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
          <SidebarGroup className="group/recent px-1 py-1">
            {/* 「最近对话」+ 折叠箭头合一(箭头紧跟文字右侧、常驻);「查看更多」推到最右,悬浮分组才显示 */}
            <div className="flex items-center gap-1 pr-1">
              <button
                type="button"
                aria-label={recentCollapsed ? "展开最近对话" : "折叠最近对话"}
                onClick={() => setRecentCollapsed((v) => !v)}
                className="flex items-center gap-1 rounded px-0.5 text-[11px] font-medium text-muted-foreground transition-colors hover:text-foreground"
              >
                最近对话
                {recentCollapsed ? (
                  <ChevronRight className="size-3" />
                ) : (
                  <ChevronDown className="size-3" />
                )}
              </button>
              <button
                type="button"
                onClick={() => onChange("chat-history")}
                className="ml-auto flex items-center gap-0.5 rounded px-1 py-0.5 text-[11px] text-muted-foreground opacity-0 transition-opacity hover:bg-sidebar-accent hover:text-foreground group-hover/recent:opacity-100"
              >
                查看更多
                <ChevronRight className="size-3" />
              </button>
            </div>
            {!recentCollapsed && (
              <SidebarGroupContent>
                {recent.length > 0 ? (
                  <SidebarMenu>{recent.map(renderItem)}</SidebarMenu>
                ) : (
                  <div className="px-2 py-3 text-[11px] text-muted-foreground">
                    最近对话已全部归档,点「查看更多」管理
                  </div>
                )}
              </SidebarGroupContent>
            )}
          </SidebarGroup>
        )}
      </div>

      {/* 重命名会话:自定义弹框(替代原生 prompt) */}
      <Dialog
        open={renameTarget !== null}
        onOpenChange={(open) => !open && setRenameTarget(null)}
      >
        <DialogContent className="sm:max-w-sm">
          <DialogHeader>
            <DialogTitle>重命名会话</DialogTitle>
          </DialogHeader>
          <Input
            autoFocus
            value={renameValue}
            onChange={(e) => setRenameValue(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter") {
                e.preventDefault();
                void submitRename();
              }
            }}
            placeholder="输入会话标题"
          />
          <DialogFooter>
            <Button variant="outline" onClick={() => setRenameTarget(null)}>
              取消
            </Button>
            <Button onClick={() => void submitRename()}>确定</Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      {/* 删除会话:二次确认(与全局删除弹窗统一用 AlertDialog) */}
      <AlertDialog
        open={deleteTarget !== null}
        onOpenChange={(open) => !open && setDeleteTarget(null)}
      >
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>删除会话</AlertDialogTitle>
            <AlertDialogDescription>
              确定删除「{deleteTarget?.title || "新对话"}」?此操作不可恢复。
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel>取消</AlertDialogCancel>
            <AlertDialogAction
              className="bg-destructive text-white hover:bg-destructive/90"
              onClick={() => void confirmDelete()}
            >
              删除
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
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
  // 工作区切换标签的展示顺序(系统配置可调);按保存顺序排列,缺失项忽略
  const [wsOrder] = useWorkspaceOrder();
  const orderedWorkspaces = wsOrder
    .map((key) => WORKSPACES.find((w) => w.key === key))
    .filter((w): w is (typeof WORKSPACES)[number] => Boolean(w));

  return (
    // 侧栏固定容器默认 top-0/h-svh,会顶到自定义标题栏后面;
    // 这里按标题栏高度 --titlebar-h 下移并缩高,使其从标题栏下方开始
    <Sidebar
      // 对话 / 协作工作区:收起即完全隐藏(offcanvas);营销保持收成图标条(icon)
      collapsible={workspace === "management" ? "icon" : "offcanvas"}
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
                  onClick={() => onChange("user-center")}
                >
                  <UserRound />
                  个人中心
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
    </Sidebar>
  );
}
