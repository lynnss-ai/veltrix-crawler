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

// 托盘弹出面板窗口与主窗口加载同一前端,按窗口 label 区分渲染哪一个根组件
let windowLabel = "main";
try {
  windowLabel = getCurrentWindow().label;
} catch {
  // 非 Tauri 环境(纯浏览器调试)下取不到窗口,按主窗口处理
}
const isTrayPopup = windowLabel === "tray-popup";

// 托盘面板窗口透明:根 html/body 背景透明,圆角外不显示底色
if (isTrayPopup) {
  document.documentElement.style.background = "transparent";
  document.body.style.background = "transparent";
}

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <ErrorBoundary>
      <ThemeProvider defaultTheme="system" storageKey="veltrix-theme">
        <TooltipProvider delayDuration={200}>
          {isTrayPopup ? <TrayPopup /> : <App />}
          {!isTrayPopup && <Toaster richColors position="top-center" />}
        </TooltipProvider>
      </ThemeProvider>
    </ErrorBoundary>
  </React.StrictMode>,
);
