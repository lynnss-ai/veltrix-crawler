// 编排器右侧富面板:编程预览 + 文件。按 conversationId 复用同会话的工作区与(全局单实例)dev server。
// 聚焦版:预览(dev server iframe)+ 只读文件查看;完整编辑器/终端/版本仍在「编程」独立页。
import { memo, useCallback, useEffect, useRef, useState } from "react";
import { Loader2, Play, RefreshCw, Square } from "lucide-react";
import { toast } from "sonner";
import { Prism as SyntaxHighlighter } from "react-syntax-highlighter";
import {
  oneDark,
  oneLight,
} from "react-syntax-highlighter/dist/esm/styles/prism";
import { useTheme } from "next-themes";

import { api, type DevServerStatus } from "@/lib/api";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { FileTree } from "@/components/file-tree";

export function CodingWorkPanel({
  conversationId,
  sending = false,
}: {
  conversationId: string | null;
  /** 所属会话是否正在跑一轮 Agent(编排器委派中);回合结束的边沿触发自动预览 */
  sending?: boolean;
}) {
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
  // 后端已拉起进程但还没探测到端口(首次 npx 下载依赖可能要几十秒):视为启动中,
  // 给明确的进度反馈而不是把「启动预览」按钮立刻还回去——否则用户以为没反应再点,
  // 旧逻辑每点一次就杀掉还没起完的上一次,永远起不来。
  const startingUp = !!(ours && status?.running && !status?.port);
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
  // 有 package.json(根或子目录,后端启动时会自动 cd 进去)→ npm run dev;否则仅静态 index.html → 起静态服务器
  useEffect(() => {
    if (cmdEditedRef.current || files.length === 0) return;
    if (files.some((f) => f === "package.json" || f.endsWith("/package.json"))) {
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

  // 回合结束(sending 由 true 变 false):刷新文件列表;服务已在跑则重载 iframe 反映最新构建,
  // 并重置自动启动标记(上一轮可能因内容未就绪失败过,新一轮生成完成允许再自动试)
  const wasSendingRef = useRef(sending);
  const autoTriedRef = useRef<string | null>(null);
  useEffect(() => {
    if (wasSendingRef.current && !sending) {
      void loadFiles();
      if (running) setReloadKey((k) => k + 1);
      autoTriedRef.current = null;
    }
    wasSendingRef.current = sending;
  }, [sending, running, loadFiles]);

  // 自动启动预览:有文件、未在跑、未启动中即静默拉起;开发服务器被别的会话占用时不抢
  // (仍可点「启动预览」手动接管)。每会话只自动试一次,失败不死循环(回合结束会重置)。
  useEffect(() => {
    if (!conversationId || sending || starting) return;
    if (files.length === 0 || running) return;
    if (status?.running) return;
    if (autoTriedRef.current === conversationId) return;
    autoTriedRef.current = conversationId;
    void startPreview(true);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [conversationId, sending, starting, files, running, status]);

  async function startPreview(silent = false) {
    if (!conversationId) return;
    setStarting(true);
    try {
      await api.startDevServer(conversationId, cmd.trim() || "npm run dev");
    } catch (e) {
      // 自动启动(silent)失败不弹错——工作区可能还没可预览内容;手动点才提示
      if (!silent) toast.error(`启动预览失败: ${e}`);
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
          ) : startingUp ? (
            <Button
              size="sm"
              variant="ghost"
              className="h-7 gap-1 px-2 text-xs text-destructive"
              onClick={() => void stopPreview()}
            >
              <Square className="size-3.5" />
              停止
            </Button>
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
          ) : startingUp || starting ? (
            <div className="flex h-full flex-col items-center justify-center gap-3 px-8 text-center">
              <Loader2 className="size-6 animate-spin text-muted-foreground" />
              <p className="text-sm text-muted-foreground">
                开发服务器启动中,请稍候…(首次运行需安装依赖,可能要几十秒)
              </p>
              {status?.logs && status.logs.length > 0 && (
                <pre className="veltrix-thin-scrollbar max-h-40 w-full max-w-md overflow-y-auto whitespace-pre-wrap break-all rounded-md bg-muted/50 p-2 text-left text-[11px] leading-relaxed text-muted-foreground">
                  {status.logs.slice(-8).join("\n")}
                </pre>
              )}
            </div>
          ) : (
            <div className="flex h-full flex-col items-center justify-center gap-3 px-8 text-center">
              <p className="text-sm text-muted-foreground">
                委派编程子智能体生成项目后会自动启动预览;未自动启动时可在此手动执行:
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
              <FileTree files={files} selected={sel} onSelect={setSel} />
            )}
          </div>
          {/* 只读查看(代码文件按扩展名语法高亮) */}
          <div className="veltrix-thin-scrollbar min-w-0 flex-1 overflow-auto">
            {sel ? (
              <FileContent file={sel} content={content} />
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

// 文件扩展名 → Prism 语言:已知的代码 / 标记类型才高亮,其余(txt / 日志等)按纯文本展示。
const EXT_LANG: Record<string, string> = {
  js: "javascript",
  mjs: "javascript",
  jsx: "jsx",
  ts: "typescript",
  tsx: "tsx",
  py: "python",
  rs: "rust",
  go: "go",
  java: "java",
  kt: "kotlin",
  json: "json",
  html: "markup",
  htm: "markup",
  vue: "markup",
  css: "css",
  scss: "scss",
  less: "less",
  sh: "bash",
  bash: "bash",
  sql: "sql",
  yml: "yaml",
  yaml: "yaml",
  toml: "toml",
  md: "markdown",
  c: "c",
  h: "c",
  cpp: "cpp",
  cs: "csharp",
  php: "php",
  rb: "ruby",
  swift: "swift",
};

// 只读文件内容:代码文件走 Prism 语法高亮(配色随明暗主题),非代码文件维持纯文本。
// memo 是关键:面板每 1.5s 轮询 dev server 状态会整体重渲染,不 memo 时 Prism 每次都把
// 整个文件重新分词着色一遍(文件稍大就卡)。超大文件直接纯文本,避免分词耗时与长 DOM。
const HIGHLIGHT_MAX_BYTES = 200 * 1024;
const FileContent = memo(function FileContent({
  file,
  content,
}: {
  file: string;
  content: string;
}) {
  const { resolvedTheme } = useTheme();
  const ext = file.split(".").pop()?.toLowerCase() ?? "";
  const lang = EXT_LANG[ext];
  if (!lang || content.length > HIGHLIGHT_MAX_BYTES) {
    return (
      <pre className="whitespace-pre-wrap break-words p-3 text-[12px] leading-relaxed">
        {content}
      </pre>
    );
  }
  return (
    <SyntaxHighlighter
      language={lang}
      style={resolvedTheme === "dark" ? oneDark : oneLight}
      customStyle={{
        margin: 0,
        padding: "0.75rem",
        background: "transparent",
        fontSize: "12px",
        lineHeight: "1.6",
      }}
      codeTagProps={{
        style: { fontFamily: "ui-monospace, SFMono-Regular, monospace" },
      }}
      className="veltrix-thin-scrollbar"
      wrapLongLines
    >
      {content}
    </SyntaxHighlighter>
  );
});

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
