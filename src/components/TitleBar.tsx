// 自定义无边框标题栏:左侧侧栏开关 + 远程控制 + 「更多」工具菜单(屏幕录制/远程连接)
// + 中部可拖拽区 + 右侧刷新/检查更新/主题/下载记录 + 窗口控制按钮(最小化/最大化/关闭)。
// 窗口装饰已在 tauri.conf.json 关闭(decorations:false),拖拽与最小化/最大化/关闭全部走前端。
import { useEffect, useState } from "react";
import {
  CircleArrowUp,
  Copy,
  LayoutGrid,
  Minus,
  MonitorSmartphone,
  PanelLeftClose,
  PanelLeftOpen,
  RotateCw,
  Square,
  Video,
  X,
} from "lucide-react";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { checkForUpdate, currentVersion } from "@/lib/updater";
import { useScreenRecording } from "@/hooks/use-screen-recording";
import { SimpleTooltip } from "@/components/SimpleTooltip";
import { ModeToggle } from "@/components/mode-toggle";
import { DownloadHistory } from "@/components/DownloadHistory";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import {
  RemoteConnectDialog,
  type RemoteStatus,
} from "@/components/RemoteConnect";

type WindowAction = "minimize" | "toggleMaximize" | "close";

// 统一封装窗口操作:非 Tauri 环境(纯浏览器调试)或 IPC 失败时记录上下文,不静默吞错
async function runWindowAction(action: WindowAction): Promise<void> {
  try {
    const appWindow = getCurrentWindow();
    if (action === "minimize") {
      await appWindow.minimize();
    } else if (action === "toggleMaximize") {
      await appWindow.toggleMaximize();
    } else {
      // 关闭 = 隐藏到系统托盘,不退出程序;真正退出走托盘菜单「退出」
      await appWindow.hide();
    }
  } catch (error) {
    console.error(`窗口操作失败 (${action}):`, error);
  }
}

interface TitleBarProps {
  // 已登录主界面才显示侧栏开关与菜单栏;登录/向导/加载页隐藏
  showSidebarTrigger: boolean;
  sidebarOpen: boolean;
  onToggleSidebar: () => void;
  // 远程连接状态(登录态时在标题栏右侧显示连接入口)
  remoteStatus: RemoteStatus;
}

export function TitleBar({
  showSidebarTrigger,
  sidebarOpen,
  onToggleSidebar,
  remoteStatus,
}: TitleBarProps) {
  // 最大化状态决定还原/最大化图标;监听窗口尺寸变化保持同步(拖拽贴边、双击标题栏等)
  const [isMaximized, setIsMaximized] = useState(false);
  // 当前应用版本:用于「检查更新」按钮悬浮提示展示
  const [appVersion, setAppVersion] = useState("");
  // 「更多」菜单里的「远程连接」项:直接驱动配对弹窗
  const [remoteDialogOpen, setRemoteDialogOpen] = useState(false);
  // 屏幕录制入口(「更多」菜单项):只开/关悬浮控制条,开始/停止在悬浮条上操作;
  // 不传 onSaved——录制完成走默认 toast(打开文件夹),对话页在场时由其接管挂到输入区
  const { openOverlay: openScreenRecording } = useScreenRecording();

  useEffect(() => {
    currentVersion()
      .then(setAppVersion)
      .catch((error) => console.warn("获取应用版本失败:", error));
  }, []);

  useEffect(() => {
    const appWindow = getCurrentWindow();
    let unlisten: (() => void) | undefined;

    const syncMaximized = () => {
      appWindow
        .isMaximized()
        .then(setIsMaximized)
        .catch((error) => console.error("读取窗口最大化状态失败:", error));
    };

    syncMaximized();
    appWindow
      .onResized(syncMaximized)
      .then((fn) => {
        unlisten = fn;
      })
      .catch((error) => console.error("监听窗口尺寸变化失败:", error));

    return () => unlisten?.();
  }, []);

  return (
    // 模态弹层(Sheet/Dialog)打开时 Radix 会给 body 设 pointer-events:none;这里显式放行 +
    // data-app-titlebar 标记,保证窗口最小化/最大化/关闭与拖拽始终可用,并让弹层「点外部关闭」逻辑识别并忽略标题栏点击。
    <header
      data-app-titlebar
      className="pointer-events-auto flex h-(--titlebar-h) shrink-0 items-center border-b bg-background select-none"
    >
      {showSidebarTrigger && (
        <div className="flex items-center gap-0.5 pr-1 pl-2">
          <button
            type="button"
            onClick={onToggleSidebar}
            title={sidebarOpen ? "收起侧边栏" : "展开侧边栏"}
            className="inline-flex size-7 items-center justify-center rounded-md text-muted-foreground transition-colors hover:bg-accent hover:text-foreground"
          >
            {/* 展开态显示"收起"图标,收起态显示"展开"图标 */}
            {sidebarOpen ? (
              <PanelLeftClose className="size-4" />
            ) : (
              <PanelLeftOpen className="size-4" />
            )}
            <span className="sr-only">切换侧边栏</span>
          </button>
          <span className="mx-1 h-4 w-px bg-border" />
          {/* 更多工具:屏幕录制 / 远程连接 等收纳进下拉,后续新工具继续往里加 */}
          <DropdownMenu>
            <SimpleTooltip content="更多工具" side="bottom">
              <DropdownMenuTrigger asChild>
                <button
                  type="button"
                  className="inline-flex size-8 shrink-0 items-center justify-center rounded-md text-muted-foreground transition-colors hover:bg-accent hover:text-foreground"
                >
                  <LayoutGrid className="size-[1.1rem]" />
                  <span className="sr-only">更多工具</span>
                </button>
              </DropdownMenuTrigger>
            </SimpleTooltip>
            <DropdownMenuContent align="start" sideOffset={6}>
              <DropdownMenuItem onClick={() => void openScreenRecording()}>
                <Video className="size-4" />
                屏幕录制
              </DropdownMenuItem>
              <DropdownMenuItem onClick={() => setRemoteDialogOpen(true)}>
                <MonitorSmartphone className="size-4" />
                远程连接
              </DropdownMenuItem>
            </DropdownMenuContent>
          </DropdownMenu>
          <RemoteConnectDialog
            open={remoteDialogOpen}
            onOpenChange={setRemoteDialogOpen}
            status={remoteStatus}
          />
        </div>
      )}

      {/* 中部:空白可拖拽区。data-tauri-drag-region 让整块响应拖动,双击触发最大化/还原 */}
      <div data-tauri-drag-region className="h-full flex-1" />

      {/* 右侧:刷新 + 检查更新 + 主题切换 + 窗口控制按钮。不在拖拽区内,保证点击不被拖拽截获 */}
      <div className="flex h-full items-center">
        {/* 刷新 / 检查更新 / 切换主题 一组,间距放宽不挤;刷新在检查更新左边,二者仅登录态显示 */}
        <div className="flex items-center gap-2 px-1.5">
          {showSidebarTrigger && (
            <SimpleTooltip content="刷新" side="bottom">
              <button
                type="button"
                onClick={() => window.location.reload()}
                className="inline-flex size-7 items-center justify-center rounded-md text-muted-foreground transition-colors hover:bg-accent hover:text-foreground"
              >
                <RotateCw className="size-4" />
                <span className="sr-only">刷新</span>
              </button>
            </SimpleTooltip>
          )}
          {showSidebarTrigger && (
            <SimpleTooltip
              content={
                appVersion ? `检查版本更新: V${appVersion}` : "检查软件更新"
              }
              side="bottom"
            >
              <button
                type="button"
                onClick={() => checkForUpdate(false)}
                className="inline-flex size-7 items-center justify-center rounded-md text-muted-foreground transition-colors hover:bg-accent hover:text-foreground"
              >
                <CircleArrowUp className="size-4" />
                <span className="sr-only">检查更新</span>
              </button>
            </SimpleTooltip>
          )}
          <ModeToggle className="size-7" />
          {/* 历史下载记录:主题切换右侧,仅登录态显示 */}
          {showSidebarTrigger && <DownloadHistory />}
        </div>
        <span className="mx-1.5 h-4 w-px bg-border" />
        <button
          type="button"
          onClick={() => runWindowAction("minimize")}
          title="最小化"
          className="inline-flex h-full w-11 items-center justify-center text-muted-foreground transition-colors hover:bg-accent hover:text-foreground"
        >
          <Minus className="size-4" />
          <span className="sr-only">最小化</span>
        </button>
        <button
          type="button"
          onClick={() => runWindowAction("toggleMaximize")}
          title={isMaximized ? "向下还原" : "最大化"}
          className="inline-flex h-full w-11 items-center justify-center text-muted-foreground transition-colors hover:bg-accent hover:text-foreground"
        >
          {isMaximized ? (
            <Copy className="size-3.5" />
          ) : (
            <Square className="size-3.5" />
          )}
          <span className="sr-only">{isMaximized ? "向下还原" : "最大化"}</span>
        </button>
        <button
          type="button"
          onClick={() => runWindowAction("close")}
          title="关闭"
          className="inline-flex h-full w-11 items-center justify-center text-muted-foreground transition-colors hover:bg-destructive hover:text-white"
        >
          <X className="size-4" />
          <span className="sr-only">关闭</span>
        </button>
      </div>
    </header>
  );
}
