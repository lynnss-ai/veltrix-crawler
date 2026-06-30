import React from "react";
import ReactDOM from "react-dom/client";
import { getCurrentWindow } from "@tauri-apps/api/window";
import App from "./App";
import { TrayPopup } from "./components/TrayPopup";
import { ErrorBoundary } from "./components/ErrorBoundary";
import { ThemeProvider } from "./components/theme-provider";
import { TooltipProvider } from "./components/ui/tooltip";
import { Toaster } from "./components/ui/sonner";
import "./index.css";

// 托盘弹出面板窗口与主窗口加载同一前端,按窗口 label 区分渲染哪一个根组件。
// 录屏悬浮条已拆为独立入口(recording-overlay.html),不再走这里。
let windowLabel = "main";
try {
  windowLabel = getCurrentWindow().label;
} catch {
  // 非 Tauri 环境(纯浏览器调试)下取不到窗口,按主窗口处理
}
const isTrayPopup = windowLabel === "tray-popup";
// 透明无边框小窗(托盘面板):根 html/body 背景透明,圆角外不显示底色
const isTransparentWindow = isTrayPopup;

if (isTransparentWindow) {
  document.documentElement.style.background = "transparent";
  document.body.style.background = "transparent";
}

function RootView() {
  if (isTrayPopup) return <TrayPopup />;
  return <App />;
}

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <ErrorBoundary>
      <ThemeProvider defaultTheme="system" storageKey="veltrix-theme">
        <TooltipProvider delayDuration={200}>
          <RootView />
          {!isTransparentWindow && (
            // 顶部偏移 = 标题栏高度 + 余量,避免 top-center 的 toast 压住自定义标题栏
            <Toaster
              richColors
              position="top-center"
              closeButton={false}
              offset={{ top: "calc(var(--titlebar-h) + 12px)" }}
              mobileOffset={{ top: "calc(var(--titlebar-h) + 12px)" }}
            />
          )}
        </TooltipProvider>
      </ThemeProvider>
    </ErrorBoundary>
  </React.StrictMode>,
);
