// 录屏悬浮条的独立入口:只打包 RecordingOverlay,避免像主入口那样加载整个应用 bundle
//(react-markdown / mermaid / 语法高亮 / 所有页面),否则新建 WebView 要等好几秒才渲染出悬浮条。
import React from "react";
import ReactDOM from "react-dom/client";
import { RecordingOverlay } from "./components/RecordingOverlay";
import { ThemeProvider } from "./components/theme-provider";
import "./index.css";

// 透明无边框小窗:根背景透明,圆角外不显示底色
document.documentElement.style.background = "transparent";
document.body.style.background = "transparent";

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <ThemeProvider defaultTheme="system" storageKey="veltrix-theme">
      <RecordingOverlay />
    </ThemeProvider>
  </React.StrictMode>,
);
