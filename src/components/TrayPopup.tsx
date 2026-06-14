// 托盘弹出面板:点击系统托盘图标后弹出的自定义无边框小窗口,替代传统系统右键菜单。
// 由后端在托盘点击时定位并显示;失焦自动隐藏(后端处理)。窗口透明,故根容器自带圆角 + 阴影。
import { useEffect, useState } from "react";
import { DownloadCloud, Eye, Power, Radar } from "lucide-react";
import { invoke } from "@tauri-apps/api/core";
import { getCurrentWindow } from "@tauri-apps/api/window";

import { checkForUpdate, currentVersion } from "@/lib/updater";

export function TrayPopup() {
  const [version, setVersion] = useState("");

  useEffect(() => {
    currentVersion()
      .then(setVersion)
      .catch(() => {});
  }, []);

  function hideSelf() {
    getCurrentWindow()
      .hide()
      .catch(() => {});
  }

  return (
    <div className="flex h-screen w-screen flex-col overflow-hidden rounded-xl border border-border bg-background shadow-2xl">
      {/* 顶部:应用信息 */}
      <div className="flex items-center gap-3 border-b border-border px-4 py-3.5">
        <div className="flex size-10 shrink-0 items-center justify-center rounded-lg bg-primary/10 text-primary">
          <Radar className="size-5" />
        </div>
        <div className="min-w-0">
          <div className="text-sm font-semibold text-foreground">
            VeltrixLoop
          </div>
          <div className="font-mono text-xs text-muted-foreground">
            {version ? `v${version}` : "—"}
          </div>
        </div>
      </div>

      {/* 操作区 */}
      <div className="flex flex-1 flex-col gap-0.5 p-2">
        <button
          type="button"
          onClick={() => {
            invoke("show_main_from_tray").catch(() => {});
          }}
          className="flex items-center gap-3 rounded-lg px-3 py-2.5 text-sm text-foreground transition-colors hover:bg-accent"
        >
          <Eye className="size-4 text-muted-foreground" />
          显示主窗口
        </button>
        <button
          type="button"
          onClick={() => {
            checkForUpdate(false);
            hideSelf();
          }}
          className="flex items-center gap-3 rounded-lg px-3 py-2.5 text-sm text-foreground transition-colors hover:bg-accent"
        >
          <DownloadCloud className="size-4 text-muted-foreground" />
          检查更新
        </button>
      </div>

      {/* 底部:退出 */}
      <div className="border-t border-border p-2">
        <button
          type="button"
          onClick={() => {
            invoke("quit_app").catch(() => {});
          }}
          className="flex w-full items-center gap-3 rounded-lg px-3 py-2.5 text-sm text-destructive transition-colors hover:bg-destructive/10"
        >
          <Power className="size-4" />
          退出 VeltrixLoop
        </button>
      </div>
    </div>
  );
}
