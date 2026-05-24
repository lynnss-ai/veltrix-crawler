import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
import { ThemeProvider } from "./components/theme-provider";
import { TooltipProvider } from "./components/ui/tooltip";
import { Toaster } from "./components/ui/sonner";
import "./index.css";

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <ThemeProvider defaultTheme="system" storageKey="veltrix-theme">
      <TooltipProvider delayDuration={200}>
        <App />
        <Toaster richColors position="top-center" />
      </TooltipProvider>
    </ThemeProvider>
  </React.StrictMode>,
);
