import { useEffect, useState, type ReactNode } from "react";
import {
  AppSidebar,
  getPageBreadcrumb,
  getWorkspaceDefaultPage,
  type PageKey,
  type RemoteStatus,
  type Workspace,
} from "@/components/app-sidebar";
import { api, type UserView } from "@/lib/api";
import { SiteHeader } from "@/components/site-header";
import { SidebarInset, SidebarProvider } from "@/components/ui/sidebar";
import { DashboardPage } from "@/pages/DashboardPage";
import { CollectPage } from "@/pages/CollectPage";
import { AccountsPage } from "@/pages/AccountsPage";
import { PlatformsPage } from "@/pages/PlatformsPage";
import { SettingsPage } from "@/pages/SettingsPage";
import { IndustryPage } from "@/pages/IndustryPage";
import { CustomersPage } from "@/pages/CustomersPage";
import { UsersPage } from "@/pages/UsersPage";
import { PlaceholderPage } from "@/pages/PlaceholderPage";
import { LoginPage } from "@/pages/LoginPage";
import { SetupWizard } from "@/pages/SetupWizard";

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
    case "platforms":
      return <PlatformsPage />;
    case "system-config":
      return <SettingsPage />;
    case "users":
      return <UsersPage />;
    case "industry":
      return <IndustryPage />;
    case "customers":
      return <CustomersPage currentUser={currentUser} />;
    case "chat-sessions":
      return (
        <PlaceholderPage
          title="会话"
          description="对话模块建设中。后续接入 AI 会话与历史记录。"
        />
      );
    case "chat-assistant":
      return (
        <PlaceholderPage
          title="智能助手"
          description="对话模块建设中。后续接入智能助手与多轮问答。"
        />
      );
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
    case "assets-all":
      return (
        <PlaceholderPage
          title="全量库"
          description="采集到的全部原始数据汇总。需后端将拦截响应解析落库 + 提供分页查询命令后接入。"
        />
      );
    case "assets-content":
      return (
        <PlaceholderPage
          title="内容库"
          description="结构化后的内容(视频 / 图文 / 笔记)。需平台适配器 parse 实现 + 内容表与查询命令。"
        />
      );
    case "assets-image":
      return (
        <PlaceholderPage
          title="图片库"
          description="采集内容中的图片资产。需后端下载落地图片 + 媒体表后接入。"
        />
      );
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

  // 启动恢复登录态:从 storage 恢复出用户时,先同步后端当前用户再放行页面渲染;
  // 无恢复用户(走登录 / 向导)则直接视为就绪,不阻塞登录页
  useEffect(() => {
    const restored = loadStoredUser();
    if (!restored) {
      setSessionReady(true);
      return;
    }
    api
      .setCurrentUser(restored.username, restored.dataScope)
      .then(() => setSessionReady(true))
      // 同步失败也放行(页面会在缺过滤上下文下退化为全部可见),避免卡死在加载页
      .catch(() => setSessionReady(true));
    // 仅挂载时执行一次
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // 已有登录态但后端会话尚未就绪:渲染加载占位,阻止页面提前发 list 请求
  if (loggedUser && !sessionReady) {
    return (
      <div className="flex min-h-svh items-center justify-center bg-background text-sm text-muted-foreground">
        加载中…
      </div>
    );
  }

  if (!loggedUser) {
    if (bootState === "loading") {
      return (
        <div className="flex min-h-svh items-center justify-center bg-background text-sm text-muted-foreground">
          加载中…
        </div>
      );
    }
    if (bootState === "setup") {
      return <SetupWizard onComplete={handleAuthed} />;
    }
    return <LoginPage onSuccess={handleAuthed} />;
  }

  const breadcrumb = getPageBreadcrumb(active);

  return (
    <SidebarProvider>
      <AppSidebar
        workspace={workspace}
        onWorkspaceChange={handleWorkspaceChange}
        active={active}
        onChange={setActive}
        user={loggedUser.username}
        onLogout={handleLogout}
        remoteStatus={remoteStatus}
      />
      <SidebarInset>
        <SiteHeader group={breadcrumb.group} page={breadcrumb.page} />
        <div className="flex flex-1 flex-col gap-4 p-4 md:p-6">
          {renderPage(active, loggedUser.username)}
        </div>
      </SidebarInset>
    </SidebarProvider>
  );
}

export default App;
