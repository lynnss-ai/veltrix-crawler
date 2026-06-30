// 编排器右侧富面板:编程预览 + 文件。按 conversationId 复用同会话的工作区与(全局单实例)dev server。
// 聚焦版:预览(dev server iframe)+ 只读文件查看;完整编辑器/终端/版本仍在「编程」独立页。
import { useCallback, useEffect, useRef, useState } from "react";
import { FileText, Loader2, Play, RefreshCw, Square } from "lucide-react";
import { toast } from "sonner";

import { api, type DevServerStatus } from "@/lib/api";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";

export function CodingWorkPanel({ conversationId }: { conversationId: string | null }) {
  const [tab, setTab] = useState<"preview" | "files">("preview");
  const [status, setStatus] = useState<DevServerStatus | null>(null);
  const [cmd, setCmd] = useState("npm run dev");
  const cmdEditedRef = useRef(false); // 用户改过命令后,不再按文件自动覆盖
  const [starting, setStarting] = useState(false);
  const [files, setFiles] = useState<string[]>([]);
  const [sel, setSel] = useState<string | null>(null);
  const [content, setContent] = useState("");
  // iframe 刷新键:dev server 起来或手动刷新时 +1 强制重载
  const [reloadKey, setReloadKey] = useState(0);
  const wasRunningRef = useRef(false);

  // 轮询 dev server 状态(全局单实例,按 conversationId 隔离)
  useEffect(() => {
    let stop = false;
    let t: number | undefined;
    const poll = async () => {
      try {
        const s = await api.getDevServerStatus();
        if (!stop) setStatus(s);
      } catch {
        /* 忽略 */
      }
      if (!stop) t = window.setTimeout(poll, 1500);
    };
    void poll();
    return () => {
      stop = true;
      if (t) clearTimeout(t);
    };
  }, []);

  const ours = !!status && status.conversationId === conversationId;
  const running = !!(ours && status?.running && status?.port);
  // 刚从未运行变为运行:自动切到预览并刷新 iframe
  useEffect(() => {
    if (running && !wasRunningRef.current) {
      setReloadKey((k) => k + 1);
      setTab("preview");
    }
    wasRunningRef.current = running;
  }, [running]);

  const loadFiles = useCallback(async () => {
    if (!conversationId) return;
    try {
      setFiles(await api.listWorkspaceFiles(conversationId));
    } catch {
      /* 忽略 */
    }
  }, [conversationId]);
  useEffect(() => {
    void loadFiles();
  }, [loadFiles]);

  // 按工作区文件自动给启动命令一个合理默认(用户改过则不覆盖):
  // 有 package.json → npm run dev;否则仅静态 index.html → 起静态服务器
  useEffect(() => {
    if (cmdEditedRef.current || files.length === 0) return;
    if (files.includes("package.json")) {
      setCmd("npm run dev");
    } else if (files.some((f) => f.split("/").pop() === "index.html")) {
      setCmd("npx --yes serve -l 5173");
    }
  }, [files]);

  useEffect(() => {
    if (!conversationId || !sel) {
      setContent("");
      return;
    }
    api
      .readWorkspaceFile(conversationId, sel)
      .then(setContent)
      .catch(() => setContent("(读取失败)"));
  }, [conversationId, sel]);

  async function startPreview() {
    if (!conversationId) return;
    setStarting(true);
    try {
      await api.startDevServer(conversationId, cmd.trim() || "npm run dev");
    } catch (e) {
      toast.error(`启动预览失败: ${e}`);
    } finally {
      setStarting(false);
    }
  }
  async function stopPreview() {
    try {
      await api.stopDevServer();
    } catch (e) {
      toast.error(`停止失败: ${e}`);
    }
  }

  return (
    <div className="flex min-h-0 min-w-0 flex-1 flex-col border-l">
      {/* tab 栏 */}
      <div className="flex shrink-0 items-center gap-1 border-b px-2 py-1.5">
        <TabBtn label="预览" active={tab === "preview"} onClick={() => setTab("preview")} />
        <TabBtn label="文件" active={tab === "files"} onClick={() => setTab("files")} />
        <div className="ml-auto flex items-center gap-1">
          {running ? (
            <>
              <Button
                size="sm"
                variant="ghost"
                className="h-7 gap-1 px-2 text-xs"
                onClick={() => setReloadKey((k) => k + 1)}
              >
                <RefreshCw className="size-3.5" />
                刷新
              </Button>
              <Button
                size="sm"
                variant="ghost"
                className="h-7 gap-1 px-2 text-xs text-destructive"
                onClick={() => void stopPreview()}
              >
                <Square className="size-3.5" />
                停止
              </Button>
            </>
          ) : (
            <Button
              size="sm"
              variant="ghost"
              className="h-7 gap-1 px-2 text-xs"
              onClick={() => void loadFiles()}
            >
              <RefreshCw className="size-3.5" />
              刷新文件
            </Button>
          )}
        </div>
      </div>

      {tab === "preview" ? (
        <div className="min-h-0 flex-1">
          {running && status?.port ? (
            <iframe
              key={reloadKey}
              title="preview"
              src={`http://localhost:${status.port}`}
              className="size-full border-0 bg-white"
            />
          ) : (
            <div className="flex h-full flex-col items-center justify-center gap-3 px-8 text-center">
              <p className="text-sm text-muted-foreground">
                委派编程子智能体生成项目后,启动开发服务器即可实时预览。
              </p>
              <div className="flex w-full max-w-sm items-center gap-2">
                <Input
                  value={cmd}
                  onChange={(e) => {
                    cmdEditedRef.current = true;
                    setCmd(e.target.value);
                  }}
                  placeholder="启动命令:有 package.json 用 npm run dev;纯静态 index.html 用 npx --yes serve -l 5173"
                  className="h-8 text-xs"
                />
                <Button
                  size="sm"
                  className="h-8 shrink-0 gap-1 px-3"
                  disabled={!conversationId || starting}
                  onClick={() => void startPreview()}
                >
                  {starting ? (
                    <Loader2 className="size-3.5 animate-spin" />
                  ) : (
                    <Play className="size-3.5" />
                  )}
                  启动预览
                </Button>
              </div>
              {status && status.running && !ours && (
                <p className="text-[11px] text-amber-600 dark:text-amber-400">
                  开发服务器正被另一个会话占用,启动会接管它。
                </p>
              )}
            </div>
          )}
        </div>
      ) : (
        <div className="flex min-h-0 flex-1">
          {/* 文件树 */}
          <div className="veltrix-thin-scrollbar w-48 shrink-0 overflow-y-auto border-r p-1.5">
            {files.length === 0 ? (
              <p className="px-2 py-4 text-center text-xs text-muted-foreground">暂无文件</p>
            ) : (
              files.map((f) => (
                <button
                  key={f}
                  type="button"
                  onClick={() => setSel(f)}
                  className={`flex w-full items-center gap-1.5 rounded px-2 py-1 text-left text-xs transition-colors ${
                    sel === f
                      ? "bg-primary/10 text-primary"
                      : "text-muted-foreground hover:bg-accent hover:text-foreground"
                  }`}
                >
                  <FileText className="size-3.5 shrink-0" />
                  <span className="truncate">{f}</span>
                </button>
              ))
            )}
          </div>
          {/* 只读查看 */}
          <div className="veltrix-thin-scrollbar min-w-0 flex-1 overflow-auto">
            {sel ? (
              <pre className="whitespace-pre-wrap break-words p-3 text-[12px] leading-relaxed">
                {content}
              </pre>
            ) : (
              <div className="flex h-full items-center justify-center text-xs text-muted-foreground">
                选择左侧文件查看内容
              </div>
            )}
          </div>
        </div>
      )}
    </div>
  );
}

function TabBtn({
  label,
  active,
  onClick,
}: {
  label: string;
  active: boolean;
  onClick: () => void;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      className={`rounded-md px-2.5 py-1 text-xs font-medium transition-colors ${
        active ? "bg-primary/10 text-primary" : "text-muted-foreground hover:bg-accent"
      }`}
    >
      {label}
    </button>
  );
}
