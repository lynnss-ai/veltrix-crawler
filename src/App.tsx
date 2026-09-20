import {
  lazy,
  Suspense,
  useEffect,
  useRef,
  useState,
  type CSSProperties,
  type ReactNode,
} from "react";
import {
  AppSidebar,
  getPageBreadcrumb,
  getProductDefaultPage,
  getWorkspaceDefaultPage,
  isOffNavPage,
  type PageKey,
  type ProductKey,
  type ServiceKey,
  type Workspace,
} from "@/components/app-sidebar";
import { Button } from "@/components/ui/button";
import { X } from "lucide-react";
import { type RemoteStatus } from "@/components/RemoteConnect";
import { api, type UserView } from "@/lib/api";
import { TitleBar } from "@/components/TitleBar";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { isTauri } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { toast } from "sonner";
import { SidebarInset, SidebarProvider } from "@/components/ui/sidebar";
import type { TaskContentFilter } from "@/pages/collect-meta";
import { ChatProvider } from "@/components/chat-context";
import { PageLoading } from "@/components/PageLoading";
import { PlaceholderPage } from "@/pages/PlaceholderPage";
import { LoginPage } from "@/pages/LoginPage";
import { SetupWizard } from "@/pages/SetupWizard";
import { checkForUpdate } from "@/lib/updater";

// 业务页面按导航按需加载。内容库、设置、AI 工作区依赖较重,不应阻塞登录后首屏。
const DashboardPage = lazy(() =>
  import("@/pages/DashboardPage").then((m) => ({ default: m.DashboardPage })),
);
const CollectPage = lazy(() =>
  import("@/pages/CollectPage").then((m) => ({ default: m.CollectPage })),
);
const AccountsPage = lazy(() =>
  import("@/pages/AccountsPage").then((m) => ({ default: m.AccountsPage })),
);
const PublishAccountsPage = lazy(() =>
  import("@/pages/PublishAccountsPage").then((m) => ({
    default: m.PublishAccountsPage,
  })),
);
const SettingsPage = lazy(() =>
  import("@/pages/SettingsPage").then((m) => ({ default: m.SettingsPage })),
);
const IndustryPage = lazy(() =>
  import("@/pages/IndustryPage").then((m) => ({ default: m.IndustryPage })),
);
const CustomersPage = lazy(() =>
  import("@/pages/CustomersPage").then((m) => ({ default: m.CustomersPage })),
);
const ProjectsPage = lazy(() =>
  import("@/pages/ProjectsPage").then((m) => ({ default: m.ProjectsPage })),
);
const TeamsPage = lazy(() =>
  import("@/pages/TeamsPage").then((m) => ({ default: m.TeamsPage })),
);
const AuthorLibraryPage = lazy(() =>
  import("@/pages/AuthorLibraryPage").then((m) => ({
    default: m.AuthorLibraryPage,
  })),
);
const MemoryCenterPage = lazy(() =>
  import("@/pages/MemoryCenterPage").then((m) => ({
    default: m.MemoryCenterPage,
  })),
);
const ConversationsPage = lazy(() =>
  import("@/pages/ConversationsPage").then((m) => ({
    default: m.ConversationsPage,
  })),
);
const ConversationShell = lazy(() =>
  import("@/components/conversation-shell").then((m) => ({
    default: m.ConversationShell,
  })),
);
const ContentLibraryPage = lazy(() =>
  import("@/pages/ContentLibraryPage").then((m) => ({
    default: m.ContentLibraryPage,
  })),
);
const CommentLibraryPage = lazy(() =>
  import("@/pages/CommentLibraryPage").then((m) => ({
    default: m.CommentLibraryPage,
  })),
);
const UsersPage = lazy(() =>
  import("@/pages/UsersPage").then((m) => ({ default: m.UsersPage })),
);
const BillingPage = lazy(() =>
  import("@/pages/BillingPage").then((m) => ({ default: m.BillingPage })),
);
const UserCenterPage = lazy(() =>
  import("@/pages/UserCenterPage").then((m) => ({
    default: m.UserCenterPage,
  })),
);
const PromptsPage = lazy(() =>
  import("@/pages/PromptsPage").then((m) => ({ default: m.PromptsPage })),
);

// 登录态持久化键:桌面端走 IPC、不发 token,登录用户存 localStorage,刷新 / 重开免登录
const AUTH_STORAGE_KEY = "veltrix.auth.user";

function loadStoredUser(): UserView | null {
  try {
    // 「记住我」存 localStorage(持久),否则存 sessionStorage(仅本次会话);恢复时都读
    const raw =
      localStorage.getItem(AUTH_STORAGE_KEY) ??
      sessionStorage.getItem(AUTH_STORAGE_KEY);
    return raw ? (JSON.parse(raw) as UserView) : null;
  } catch {
    return null;
  }
}

// 当前工作区 / 页面持久化键:刷新按钮是整页 reload,靠它在重载后停留原页而非回到数据概览
const NAV_STORAGE_KEY = "veltrix.nav.page";

function loadStoredNav(): {
  product: ProductKey;
  workspace: Workspace;
  active: PageKey;
} | null {
  try {
    const raw = sessionStorage.getItem(NAV_STORAGE_KEY);
    if (!raw) return null;
    const nav = JSON.parse(raw) as {
      product: ProductKey;
      workspace: Workspace;
      active: PageKey;
    };
    // 已下线产品(如内容创作)的残留记录:回退到协作平台,避免落在无导航的状态
    if (nav.product !== "crawler" && nav.product !== "publish") {
      nav.product = "crawler";
      nav.active = "dashboard";
    }
    return nav;
  } catch {
    return null;
  }
}

function renderPage(
  active: PageKey,
  loggedUser: UserView,
  onProfileUpdated: (user: UserView) => void,
  onNavigate: (key: PageKey, ctx?: TaskContentFilter) => void,
  navCtx: TaskContentFilter | null,
  drillSource: PageKey | null,
  collectDetailId: string | null,
  onCollectDetailChange: (id: string | null) => void,
): ReactNode {
  switch (active) {
    case "dashboard":
      return <DashboardPage />;
    case "collect-tasks":
      return (
        <CollectPage
          onNavigate={onNavigate}
          detailId={collectDetailId}
          onDetailChange={onCollectDetailChange}
        />
      );
    case "accounts":
      return <AccountsPage currentUser={loggedUser.username} />;
    case "publish-dashboard":
      return (
        <PlaceholderPage
          title="数据大盘"
          description="发布数据大盘建设中。后续汇总各客户账号的发布量、今日已发与账号状态。"
        />
      );
    case "publish-content":
      return (
        <PlaceholderPage
          title="内容发布"
          description="自动发布流程建设中。后续支持选择素材与客户账号,自动完成发布。"
        />
      );
    case "publish-accounts":
      return <PublishAccountsPage />;
    case "system-config":
      return <SettingsPage />;
    case "user-center":
      return (
        <UserCenterPage
          currentUser={loggedUser}
          onProfileUpdated={onProfileUpdated}
        />
      );
    case "users":
      return <UsersPage />;
    case "billing":
      return <BillingPage />;
    case "industry":
      return <IndustryPage />;
    case "customers":
      return <CustomersPage currentUser={loggedUser.username} />;
    case "projects":
      return <ProjectsPage currentUser={loggedUser.username} />;
    case "chat-sessions":
      return <ConversationShell />;
    case "chat-history":
      return <ConversationsPage onNavigate={onNavigate} />;
    case "memory-center":
      return <MemoryCenterPage />;
    case "cowork-prompts":
      return <PromptsPage />;
    case "cowork-team":
      return <TeamsPage currentUser={loggedUser.username} />;
    // 三个库共用组件,必须用 key 强制各自独立挂载:
    // 否则路由切换时 React 复用实例,上一个库的筛选/视图状态会带到下一个库
    case "assets-all":
      return (
        <ContentLibraryPage
          // 带任务穿透时按「任务+关键词+单次运行起点」入 key,强制重挂载并应用过滤;
          // 不同穿透目标 key 各异,避免上一次「清除筛选」的残留态带到下一次穿透
          key={
            navCtx
              ? `assets-all-${navCtx.taskId}-${navCtx.keyword ?? ""}-${navCtx.runStart ?? ""}`
              : "assets-all"
          }
          title="全量库"
          taskFilter={navCtx ?? undefined}
          onBack={
            navCtx && drillSource ? () => onNavigate(drillSource) : undefined
          }
        />
      );
    case "assets-content":
      return (
        <ContentLibraryPage
          key="assets-content"
          title="内容库"
          kindFilter="video"
        />
      );
    case "assets-image":
      return (
        <ContentLibraryPage
          key="assets-image"
          title="图片库"
          kindFilter="image"
        />
      );
    case "assets-comment":
      return <CommentLibraryPage />;
    case "assets-author":
      return <AuthorLibraryPage />;
    default:
      return null;
  }
}

function App() {
  // 初始值从 localStorage 恢复:刷新页面不丢登录态
  const [loggedUser, setLoggedUser] = useState<UserView | null>(loadStoredUser);
  // 初始产品 / 工作区 / 页面从 sessionStorage 恢复:刷新后停留原页(无记录时回默认)
  const storedNav = loadStoredNav();
  const [product, setProduct] = useState<ProductKey>(
    storedNav?.product ?? "crawler",
  );
  const [workspace, setWorkspace] = useState<Workspace>(
    storedNav?.workspace ?? "management",
  );
  // 旧版工作空间 / 视频剪辑 / 文案撰写 / 素材管理页已移除，存量导航记录回退到仍可用的创作页。
  const storedActive = storedNav?.active === ("cowork-space" as string) ||
    storedNav?.active === ("cowork-video" as string) ||
    storedNav?.active === ("cowork-copy" as string) ||
    storedNav?.active === ("cowork-assets" as string)
    ? ("cowork-prompts" as PageKey)
    : storedNav?.active;
  const [active, setActive] = useState<PageKey>(storedActive ?? "dashboard");
  // 记住进入脱离侧栏页面(系统设置/个人中心)前的来源页,供关闭返回
  const prevPageRef = useRef<PageKey>("dashboard");
  // 后端会话就绪标志:后端 set_current_user 完成后才允许各页面发 list 请求,
  // 否则服务端按 dataScope 过滤会因当前用户缺失而出错
  const [sessionReady, setSessionReady] = useState(false);

  // 工作区 / 当前页持久化:刷新按钮走整页 window.location.reload(),靠这个在重载后恢复原页(而非回数据概览)
  useEffect(() => {
    sessionStorage.setItem(
      NAV_STORAGE_KEY,
      JSON.stringify({ product, workspace, active }),
    );
  }, [product, workspace, active]);

  // 登录 / 初始化成功:先同步后端当前用户,再持久化登录态。
  // remember=true 存 localStorage(关掉重开仍免登录);false 存 sessionStorage(仅本次会话,关闭后需重新登录)
  async function handleAuthed(user: UserView, remember = true) {
    // 必须先让后端会话就绪,确保 setLoggedUser 触发的页面渲染在过滤上下文之后
    await api.setCurrentUser(user.username, user.dataScope);
    const payload = JSON.stringify(user);
    if (remember) {
      localStorage.setItem(AUTH_STORAGE_KEY, payload);
      sessionStorage.removeItem(AUTH_STORAGE_KEY);
    } else {
      sessionStorage.setItem(AUTH_STORAGE_KEY, payload);
      localStorage.removeItem(AUTH_STORAGE_KEY);
    }
    setLoggedUser(user);
    setSessionReady(true);
  }
  // 个人中心更新资料:同步内存登录态并刷新原本所在的持久化(username/dataScope 不变,无需重设后端会话)
  function handleProfileUpdated(updated: UserView) {
    setLoggedUser(updated);
    const payload = JSON.stringify(updated);
    if (localStorage.getItem(AUTH_STORAGE_KEY) !== null) {
      localStorage.setItem(AUTH_STORAGE_KEY, payload);
    } else if (sessionStorage.getItem(AUTH_STORAGE_KEY) !== null) {
      sessionStorage.setItem(AUTH_STORAGE_KEY, payload);
    }
  }
  // 退出登录:清除后端会话与两处登录态
  function handleLogout() {
    // 后端清理失败不阻塞退出
    api.clearCurrentUser().catch((e) => console.warn("清除后端会话失败:", e));
    localStorage.removeItem(AUTH_STORAGE_KEY);
    sessionStorage.removeItem(AUTH_STORAGE_KEY);
    setLoggedUser(null);
    setSessionReady(false);
    setBootState("login");
  }

  // 切换产品时整体跳到该产品默认落地页(本期不记忆各产品上次停留页)
  function handleProductChange(next: ProductKey) {
    setProduct(next);
    setActive(getProductDefaultPage(next));
  }

  // 服务统一切换(侧栏平铺 tab / Logo 右侧「切换平台」):运营/对话/创作属协作平台工作区,
  // 发布服务是独立产品;从发布服务切回工作区时一并把产品切回协作平台
  function handleServiceChange(next: ServiceKey) {
    if (next === "publish") {
      handleProductChange("publish");
      return;
    }
    setProduct("crawler");
    setWorkspace(next);
    setActive(getWorkspaceDefaultPage(next));
  }

  // 数据穿透:从任务列表/详情跳全量库时携带的过滤上下文(按任务 / 单次运行);跳别处自动清空
  const [navCtx, setNavCtx] = useState<TaskContentFilter | null>(null);
  // 数据穿透来源页:带 ctx 跳转视为穿透,记下出发页,供穿透目标页「返回」回到原处
  const [drillSource, setDrillSource] = useState<PageKey | null>(null);
  // 任务调度详情页 id 提升到 App:CollectPage 穿透跳走会卸载,本地 state 丢失,
  // 从详情穿透再返回时会掉回列表;由 App 持有即可「从哪穿透回哪去」
  const [collectDetailId, setCollectDetailId] = useState<string | null>(null);
  // 进入「系统设置 / 个人中心」等脱离侧栏的页面前记住来源页,关闭时返回该页
  function handleNavigate(next: PageKey, ctx?: TaskContentFilter) {
    if (isOffNavPage(next) && !isOffNavPage(active)) {
      prevPageRef.current = active;
    }
    // 带穿透上下文 = 数据穿透,记下出发页;否则清空(普通跳转不保留返回)
    setDrillSource(ctx ? active : null);
    setNavCtx(ctx ?? null);
    setActive(next);
  }
  function closeOffNavPage() {
    setActive(prevPageRef.current);
  }
  // 远程上报连接状态;后端 RemoteConfig 上报模块就绪前先占位为未连接
  const [remoteStatus] = useState<RemoteStatus>("disconnected");

  // 侧栏按窗口宽度自动展开/收起:窄屏(<1024px) 收起腾出表格空间;用户可手动覆盖
  const [sidebarOpen, setSidebarOpen] = useState(
    typeof window !== "undefined" ? window.innerWidth >= 1024 : true,
  );
  useEffect(() => {
    const onResize = () => setSidebarOpen(window.innerWidth >= 1024);
    window.addEventListener("resize", onResize);
    return () => window.removeEventListener("resize", onResize);
  }, []);

  // 采集安全验证提示:任一采集窗口检测到风控验证(collect-verify present=true)即全局置顶常驻提示,
  // 提醒用户去采集窗口手动完成;该窗口验证解除(present=false)后移出,集合空则自动收起。
  useEffect(() => {
    const VERIFY_TOAST_ID = "collect-verify";
    const pending = new Set<number>();
    let unlisten: (() => void) | undefined;
    void listen<{ present: boolean; sessionId: number }>(
      "collect-verify",
      (event) => {
        const present = !!event.payload?.present;
        const sessionId = event.payload?.sessionId ?? 0;
        if (present) pending.add(sessionId);
        else pending.delete(sessionId);
        if (pending.size > 0) {
          toast.warning("检测到安全验证 · 采集已暂停", {
            id: VERIFY_TOAST_ID,
            description: "请在采集窗口手动完成验证,完成后将自动恢复采集",
            duration: Infinity,
          });
        } else {
          toast.dismiss(VERIFY_TOAST_ID);
        }
      },
    ).then((fn) => {
      unlisten = fn;
    });
    return () => {
      unlisten?.();
      toast.dismiss(VERIFY_TOAST_ID);
    };
  }, []);
  // 启动引导:loading 加载中,setup 走初始化向导(无任何用户),login 登录页
  const [bootState, setBootState] = useState<"loading" | "setup" | "login">(
    "loading",
  );
  useEffect(() => {
    api
      .hasUsers()
      .then((has) => setBootState(has ? "login" : "setup"))
      .catch(() => setBootState("login"));
  }, []);

  // 启动后台静默检查软件更新(延迟 5s 避免与启动请求争抢);有新版弹原生确认框
  useEffect(() => {
    const timer = setTimeout(() => {
      void checkForUpdate(true);
    }, 5000);
    return () => clearTimeout(timer);
  }, []);

  // 窗口启动隐藏(tauri.conf visible:false)以避免白屏,首帧渲染后再显示
  useEffect(() => {
    // 纯浏览器调试(bun run dev)没有 Tauri 环境,getCurrentWindow 会同步抛错并触发全局错误边界,必须跳过
    if (!isTauri()) return;
    getCurrentWindow()
      .show()
      .catch((e) => console.warn("显示主窗口失败:", e));
  }, []);

  // 启动恢复登录态:先向后端校验该用户在数据库中仍存在且启用(清库 / 删用户 / 禁用后
  // localStorage 里的旧登录态必须作废,否则会以幽灵身份进入主界面),有效才同步后端会话。
  // 校验同时取回最新 dataScope,管理员改过权限也即时生效。
  useEffect(() => {
    const restored = loadStoredUser();
    if (!restored) {
      setSessionReady(true);
      return;
    }
    api
      .verifySessionUser(restored.username)
      .then(async (scope) => {
        if (scope == null) {
          // 用户已不存在:作废本地登录态,回到登录页 / 初始化向导
          localStorage.removeItem(AUTH_STORAGE_KEY);
          sessionStorage.removeItem(AUTH_STORAGE_KEY);
          setLoggedUser(null);
          setSessionReady(true);
          return;
        }
        await api.setCurrentUser(restored.username, scope);
        setSessionReady(true);
      })
      // 校验异常(非 Tauri 调试环境等)放行,避免卡死在加载页
      .catch(() => setSessionReady(true));
    // 仅挂载时执行一次
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // 主体内容随登录/初始化状态切换;标题栏始终常驻,登录态才显示侧栏开关
  const loadingBody = (
    <PageLoading className="h-full bg-background p-4 md:p-6" />
  );

  let body: ReactNode;
  let showSidebarTrigger = false;
  if (loggedUser && !sessionReady) {
    // 已有登录态但后端会话尚未就绪:加载占位,阻止页面提前发 list 请求
    body = loadingBody;
  } else if (!loggedUser) {
    if (bootState === "loading") {
      body = loadingBody;
    } else if (bootState === "setup") {
      body = <SetupWizard onComplete={handleAuthed} />;
    } else {
      body = <LoginPage onSuccess={handleAuthed} />;
    }
  } else {
    showSidebarTrigger = true;
    const breadcrumb = getPageBreadcrumb(active);
    body = (
      <ChatProvider>
        <SidebarProvider
          open={sidebarOpen}
          onOpenChange={setSidebarOpen}
          className="h-full min-h-0"
          style={{ "--sidebar-width": "14rem" } as CSSProperties}
        >
          <AppSidebar
            product={product}
            onProductChange={handleProductChange}
            workspace={workspace}
            onServiceChange={handleServiceChange}
            active={active}
            onChange={handleNavigate}
            user={loggedUser.username}
            onLogout={handleLogout}
          />
          {/* min-w-0 让里面的 DataTable 横向滚动归自己处理,不溢出到窗口 */}
          <SidebarInset className="min-w-0">
            {/* 页面级 Suspense 只替换内容区,切换菜单时侧栏与标题栏保持可见、可操作。 */}
            {active === "chat-sessions" ? (
              <div className="flex min-h-0 min-w-0 flex-1 flex-col">
                <Suspense fallback={<PageLoading variant="workspace" />}>
                  {renderPage(active, loggedUser, handleProfileUpdated, handleNavigate, navCtx, drillSource, collectDetailId, setCollectDetailId)}
                </Suspense>
              </div>
            ) : (
              <div className="flex min-h-0 min-w-0 flex-1 flex-col gap-4 p-2.5">
                {/* 自带头部结构的数据概览页不再重复显示标题。 */}
                {active !== "dashboard" && (
                <div className="flex shrink-0 items-center justify-between gap-3">
                  {/* 关闭入口统一放在标题左侧、标题前面(脱离侧栏的单页面:系统设置/个人中心/对话记录等) */}
                  <div className="flex min-w-0 items-center gap-2">
                    {isOffNavPage(active) && (
                      <Button
                        variant="ghost"
                        size="icon"
                        onClick={closeOffNavPage}
                        title="关闭"
                        className="size-9 text-muted-foreground hover:text-foreground"
                      >
                        <X className="size-6" />
                        <span className="sr-only">关闭</span>
                      </Button>
                    )}
                    <h1 className="truncate text-xl font-semibold text-foreground">
                      {breadcrumb.page}
                    </h1>
                  </div>
                </div>
                )}
                <Suspense fallback={<PageLoading />}>
                  {renderPage(active, loggedUser, handleProfileUpdated, handleNavigate, navCtx, drillSource, collectDetailId, setCollectDetailId)}
                </Suspense>
              </div>
            )}
          </SidebarInset>
        </SidebarProvider>
      </ChatProvider>
    );
  }

  // 无边框窗口:最外层纵向布局 = 标题栏 + 主体;--titlebar-h 供侧栏定位复用
  return (
    <div
      className="flex h-svh flex-col overflow-hidden pt-[env(safe-area-inset-top)] pb-[env(safe-area-inset-bottom)]"
      style={{ "--titlebar-h": "2.25rem" } as CSSProperties}
    >
      <TitleBar
        showSidebarTrigger={showSidebarTrigger}
        sidebarOpen={sidebarOpen}
        onToggleSidebar={() => setSidebarOpen((open) => !open)}
        remoteStatus={remoteStatus}
      />
      <div className="relative min-h-0 flex-1">
        {body}
      </div>
    </div>
  );
}

export default App;
