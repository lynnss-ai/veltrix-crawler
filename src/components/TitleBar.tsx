// 自定义无边框标题栏:左侧侧栏开关 + 中部可拖拽区 + 右侧窗口控制按钮(最小化/最大化/关闭) + 检查更新 + 远程连接。
// 窗口装饰已在 tauri.conf.json 关闭(decorations:false),拖拽与最小化/最大化/关闭全部走前端。
import { useEffect, useState } from "react";
import {
  CircleArrowUp,
  Copy,
  Minus,
  PanelLeftClose,
  PanelLeftOpen,
  RotateCw,
  Square,
  X,
} from "lucide-react";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { checkForUpdate } from "@/lib/updater";
import { SimpleTooltip } from "@/components/SimpleTooltip";
import { ModeToggle } from "@/components/mode-toggle";
import { DownloadHistory } from "@/components/DownloadHistory";
import {
  RemoteConnectButton,
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
    <header className="flex h-(--titlebar-h) shrink-0 items-center border-b bg-background select-none">
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
          <RemoteConnectButton status={remoteStatus} />
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
            <SimpleTooltip content="检查软件更新" side="bottom">
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
