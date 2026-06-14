import { useEffect, useState, type CSSProperties, type ReactNode } from "react";
import {
  AppSidebar,
  getPageBreadcrumb,
  getWorkspaceDefaultPage,
  type PageKey,
  type Workspace,
} from "@/components/app-sidebar";
import { type RemoteStatus } from "@/components/RemoteConnect";
import { api, type UserView } from "@/lib/api";
import { TitleBar } from "@/components/TitleBar";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { listen } from "@tauri-apps/api/event";
import { toast } from "sonner";
import { SidebarInset, SidebarProvider } from "@/components/ui/sidebar";
import { DashboardPage } from "@/pages/DashboardPage";
import { CollectPage } from "@/pages/CollectPage";
import { AccountsPage } from "@/pages/AccountsPage";
import { SettingsPage } from "@/pages/SettingsPage";
import { IndustryPage } from "@/pages/IndustryPage";
import { CustomersPage } from "@/pages/CustomersPage";
import { AuthorLibraryPage } from "@/pages/AuthorLibraryPage";
import { ChatPage } from "@/pages/ChatPage";
import { ChatProvider } from "@/components/chat-context";
import { ContentLibraryPage } from "@/pages/ContentLibraryPage";
import { CommentLibraryPage } from "@/pages/CommentLibraryPage";
import { UsersPage } from "@/pages/UsersPage";
import { PlaceholderPage } from "@/pages/PlaceholderPage";
import { LoginPage } from "@/pages/LoginPage";
import { SetupWizard } from "@/pages/SetupWizard";
import { checkForUpdate, currentVersion } from "@/lib/updater";

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

function renderPage(active: PageKey, currentUser: string): ReactNode {
  switch (active) {
    case "dashboard":
      return <DashboardPage />;
    case "collect-tasks":
      return <CollectPage />;
    case "accounts":
      return <AccountsPage currentUser={currentUser} />;
    case "system-config":
      return <SettingsPage />;
    case "users":
      return <UsersPage />;
    case "industry":
      return <IndustryPage />;
    case "customers":
      return <CustomersPage currentUser={currentUser} />;
    case "chat-sessions":
      return <ChatPage />;
    case "cowork-space":
      return (
        <PlaceholderPage
          title="工作空间"
          description="协作模块建设中。后续接入团队共享工作区。"
        />
      );
    case "cowork-team":
      return (
        <PlaceholderPage
          title="团队成员"
          description="协作模块建设中。后续接入成员与权限管理。"
        />
      );
    // 三个库共用组件,必须用 key 强制各自独立挂载:
    // 否则路由切换时 React 复用实例,上一个库的筛选/视图状态会带到下一个库
    case "assets-all":
      return <ContentLibraryPage key="assets-all" title="全量库" />;
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
  const [workspace, setWorkspace] = useState<Workspace>("management");
  const [active, setActive] = useState<PageKey>("dashboard");
  // 后端会话就绪标志:后端 set_current_user 完成后才允许各页面发 list 请求,
  // 否则服务端按 dataScope 过滤会因当前用户缺失而出错
  const [sessionReady, setSessionReady] = useState(false);

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
  // 退出登录:清除后端会话与两处登录态
  function handleLogout() {
    // 后端清理失败不阻塞退出
    api.clearCurrentUser().catch(() => {});
    localStorage.removeItem(AUTH_STORAGE_KEY);
    sessionStorage.removeItem(AUTH_STORAGE_KEY);
    setLoggedUser(null);
    setSessionReady(false);
    setBootState("login");
  }

  // 切换工作区时跳转到该工作区的默认页
  function handleWorkspaceChange(next: Workspace) {
    setWorkspace(next);
    setActive(getWorkspaceDefaultPage(next));
  }
  // 远程上报连接状态;后端 RemoteConfig 上报模块就绪前先占位为未连接
  const [remoteStatus] = useState<RemoteStatus>("disconnected");
  const [appVersion, setAppVersion] = useState("");

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
    currentVersion()
      .then(setAppVersion)
      .catch(() => {});
    const timer = setTimeout(() => {
      void checkForUpdate(true);
    }, 5000);
    return () => clearTimeout(timer);
  }, []);

  // 窗口启动隐藏(tauri.conf visible:false)以避免白屏,首帧渲染后再显示
  useEffect(() => {
    getCurrentWindow()
      .show()
      .catch(() => {});
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
    <div className="flex h-full items-center justify-center bg-background text-sm text-muted-foreground">
      加载中…
    </div>
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
          workspace={workspace}
          onWorkspaceChange={handleWorkspaceChange}
          active={active}
          onChange={setActive}
          user={loggedUser.username}
          onLogout={handleLogout}
        />
        {/* min-w-0 让里面的 DataTable 横向滚动归自己处理,不溢出到窗口 */}
        <SidebarInset className="min-w-0">
          {/* 对话页整页即会话,不套标题与内边距外层;其余页保留标题 + 留白 */}
          {active === "chat-sessions" ? (
            <div className="flex min-h-0 min-w-0 flex-1 flex-col">
              {renderPage(active, loggedUser.username)}
            </div>
          ) : (
            <div className="flex min-h-0 min-w-0 flex-1 flex-col gap-4 p-4 md:p-6">
              <div className="flex shrink-0 items-center justify-between gap-3">
                <h1 className="text-xl font-semibold text-foreground">
                  {breadcrumb.page}
                </h1>
                {active === "dashboard" && (
                  <span className="flex items-center gap-1.5 text-xs text-muted-foreground">
                    当前版本
                    <span className="font-mono font-medium text-foreground">
                      v{appVersion || "—"}
                    </span>
                  </span>
                )}
              </div>
              {renderPage(active, loggedUser.username)}
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
      className="flex h-svh flex-col overflow-hidden"
      style={{ "--titlebar-h": "2.25rem" } as CSSProperties}
    >
      <TitleBar
        showSidebarTrigger={showSidebarTrigger}
        sidebarOpen={sidebarOpen}
        onToggleSidebar={() => setSidebarOpen((open) => !open)}
        remoteStatus={remoteStatus}
      />
      <div className="relative min-h-0 flex-1">{body}</div>
    </div>
  );
}

export default App;
