// 自定义无边框标题栏:左侧侧栏开关 + 文件/窗口/帮助菜单 + 中部可拖拽区 + 右侧窗口控制按钮。
// 窗口装饰已在 tauri.conf.json 关闭(decorations:false),拖拽与最小化/最大化/关闭全部走前端。
import { useEffect, useState } from "react";
import {
  Copy,
  Minus,
  PanelLeftClose,
  PanelLeftOpen,
  Square,
  X,
} from "lucide-react";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { WebviewWindow } from "@tauri-apps/api/webviewWindow";
import { getVersion } from "@tauri-apps/api/app";
import { toast } from "sonner";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
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

// 新开一个应用窗口:label 以 app- 前缀匹配 capability,继承完整 IPC 与窗口控制权限;
// 同源 localStorage 共享,新窗口自动复用登录态。保持无边框与主窗口一致。
function openNewWindow(): void {
  try {
    // 运行时不可用 Date.now 之外的唯一源,这里用时间戳保证 label 唯一
    const label = `app-${Date.now()}`;
    const win = new WebviewWindow(label, {
      url: "/",
      title: "veltrix-crawler",
      width: 1100,
      height: 720,
      decorations: false,
    });
    win.once("tauri://error", (event) =>
      console.error("新窗口创建失败:", event),
    );
  } catch (error) {
    console.error("打开新窗口失败:", error);
  }
}

interface TitleBarProps {
  // 已登录主界面才显示侧栏开关与菜单栏;登录/向导/加载页隐藏
  showSidebarTrigger: boolean;
  sidebarOpen: boolean;
  onToggleSidebar: () => void;
  // 跳转到系统配置页(文件 › 设置)
  onOpenSettings: () => void;
}

export function TitleBar({
  showSidebarTrigger,
  sidebarOpen,
  onToggleSidebar,
  onOpenSettings,
}: TitleBarProps) {
  // 最大化状态决定还原/最大化图标;监听窗口尺寸变化保持同步(拖拽贴边、双击标题栏等)
  const [isMaximized, setIsMaximized] = useState(false);
  const [isAboutOpen, setIsAboutOpen] = useState(false);
  const [appVersion, setAppVersion] = useState("");

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

    // 关于弹窗展示版本号,best-effort 拉取,失败不阻塞
    getVersion()
      .then(setAppVersion)
      .catch((error) => console.error("读取应用版本失败:", error));

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

          {/* 文件菜单 */}
          <TitleMenu label="文件">
            <DropdownMenuItem onClick={openNewWindow}>新窗口</DropdownMenuItem>
            <DropdownMenuItem onClick={onOpenSettings}>设置</DropdownMenuItem>
            <DropdownMenuItem onClick={() => setIsAboutOpen(true)}>
              关于我们
            </DropdownMenuItem>
            <DropdownMenuSeparator />
            <DropdownMenuItem
              className="text-destructive focus:text-destructive focus:bg-destructive/10"
              onClick={() => runWindowAction("close")}
            >
              退出
            </DropdownMenuItem>
          </TitleMenu>

          {/* 窗口菜单 */}
          <TitleMenu label="窗口">
            <DropdownMenuItem onClick={() => runWindowAction("minimize")}>
              最小化
            </DropdownMenuItem>
            <DropdownMenuItem onClick={() => runWindowAction("toggleMaximize")}>
              {isMaximized ? "向下还原" : "最大化"}
            </DropdownMenuItem>
          </TitleMenu>

          {/* 帮助菜单 */}
          <TitleMenu label="帮助">
            <DropdownMenuItem
              onClick={() => toast.info("使用帮助即将上线")}
            >
              使用帮助
            </DropdownMenuItem>
            <DropdownMenuItem
              onClick={() => toast.info("当前已是最新版本")}
            >
              检查更新
            </DropdownMenuItem>
          </TitleMenu>
        </div>
      )}

      {/* 中部:空白可拖拽区。data-tauri-drag-region 让整块响应拖动,双击触发最大化/还原 */}
      <div data-tauri-drag-region className="h-full flex-1" />

      {/* 右侧:窗口控制按钮。不在拖拽区内,保证点击不被拖拽截获 */}
      <div className="flex h-full">
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

      <AboutDialog
        open={isAboutOpen}
        onOpenChange={setIsAboutOpen}
        version={appVersion}
      />
    </header>
  );
}

// 标题栏单个下拉菜单:文字触发 + 下拉内容,统一样式
function TitleMenu({
  label,
  children,
}: {
  label: string;
  children: React.ReactNode;
}) {
  return (
    <DropdownMenu>
      <DropdownMenuTrigger asChild>
        <button
          type="button"
          className="inline-flex h-7 items-center rounded-md px-2 text-xs text-muted-foreground transition-colors hover:bg-accent hover:text-foreground data-[state=open]:bg-accent data-[state=open]:text-foreground"
        >
          {label}
        </button>
      </DropdownMenuTrigger>
      <DropdownMenuContent align="start" sideOffset={4} className="w-40">
        {children}
      </DropdownMenuContent>
    </DropdownMenu>
  );
}

// 关于对话框:产品信息与版本号
function AboutDialog({
  open,
  onOpenChange,
  version,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  version: string;
}) {
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="sm:max-w-sm">
        <DialogHeader>
          <DialogTitle>关于 Veltrix</DialogTitle>
          <DialogDescription>
            版本 {version || "—"}
          </DialogDescription>
        </DialogHeader>
        <div className="space-y-2 text-sm text-muted-foreground">
          <p>Veltrix 协作平台 · 多账号采集与内容资产管理桌面端。</p>
          <p className="text-xs">© 2026 Veltrix. 保留所有权利。</p>
        </div>
      </DialogContent>
    </Dialog>
  );
}
