// AI 回复的 Markdown 实时渲染:react-markdown + remark-gfm(表格/删除线等)。
// 代码块单独渲染:顶部条显示语言 + 复制 / 下载;为兼顾流式性能,不做语法高亮着色。
import { memo, useEffect, useState, type ReactNode } from "react";
import Markdown from "react-markdown";
import remarkGfm from "remark-gfm";
import { Prism as SyntaxHighlighter } from "react-syntax-highlighter";
import {
  oneDark,
  oneLight,
} from "react-syntax-highlighter/dist/esm/styles/prism";
import { useTheme } from "next-themes";
import mermaid from "mermaid";
import { Check, Code, Copy, Download, Workflow } from "lucide-react";
import { invoke } from "@tauri-apps/api/core";
import { toast } from "sonner";
import { recordDownload } from "@/lib/download-history";

// 语言 → 下载文件扩展名(未知按 txt)
const LANG_EXT: Record<string, string> = {
  javascript: "js",
  js: "js",
  typescript: "ts",
  ts: "ts",
  tsx: "tsx",
  jsx: "jsx",
  python: "py",
  py: "py",
  rust: "rs",
  go: "go",
  java: "java",
  kotlin: "kt",
  json: "json",
  html: "html",
  css: "css",
  scss: "scss",
  bash: "sh",
  shell: "sh",
  sh: "sh",
  sql: "sql",
  yaml: "yml",
  yml: "yml",
  markdown: "md",
  md: "md",
  c: "c",
  cpp: "cpp",
  "c++": "cpp",
  csharp: "cs",
  cs: "cs",
  php: "php",
  ruby: "rb",
  swift: "swift",
};

// 代码块:语言标签 + 复制 / 下载 + 按语言语法高亮(配色随明暗主题切换)。
// plain=true(流式生成中):用普通等宽 pre,高度增长平稳、不逐帧重着色,避免下方 loading 抖动重绘。
function CodeBlock({
  code,
  lang,
  plain,
}: {
  code: string;
  lang: string;
  plain?: boolean;
}) {
  const [copied, setCopied] = useState(false);
  const { resolvedTheme } = useTheme();
  const isDark = resolvedTheme === "dark";

  async function copy() {
    try {
      await navigator.clipboard.writeText(code);
      setCopied(true);
      setTimeout(() => setCopied(false), 1500);
    } catch {
      toast.error("复制失败");
    }
  }

  // WebView2 不支持 <a download>,走后端保存对话框写文件
  async function download() {
    const ext = LANG_EXT[lang.toLowerCase()] || "txt";
    try {
      const path = await invoke<string | null>("save_text_dialog", {
        content: code,
        fileName: `code-${Date.now()}.${ext}`,
      });
      if (path) {
        recordDownload({ path, kind: "代码" });
        toast.success("已保存");
      }
    } catch (e) {
      toast.error(`保存失败: ${e}`);
    }
  }

  return (
    <div className="my-2 overflow-hidden rounded-lg border bg-muted/60">
      <div className="flex items-center justify-between border-b bg-muted/80 px-3 py-1">
        <span className="font-mono text-[11px] text-muted-foreground">
          {lang || "code"}
        </span>
        <div className="flex items-center gap-1">
          <button
            type="button"
            onClick={copy}
            title="复制代码"
            className="inline-flex items-center gap-1 rounded px-1.5 py-0.5 text-[11px] text-muted-foreground transition-colors hover:bg-accent hover:text-foreground"
          >
            {copied ? <Check className="size-3" /> : <Copy className="size-3" />}
            {copied ? "已复制" : "复制"}
          </button>
          <button
            type="button"
            onClick={() => void download()}
            title="下载代码"
            className="inline-flex items-center gap-1 rounded px-1.5 py-0.5 text-[11px] text-muted-foreground transition-colors hover:bg-accent hover:text-foreground"
          >
            <Download className="size-3" />
            下载
          </button>
        </div>
      </div>
      {plain ? (
        <pre className="veltrix-thin-scrollbar overflow-x-auto p-3 text-xs leading-relaxed">
          <code className="font-mono">{code}</code>
        </pre>
      ) : (
        <SyntaxHighlighter
          language={lang || "text"}
          style={isDark ? oneDark : oneLight}
          customStyle={{
            margin: 0,
            padding: "0.75rem",
            background: "transparent",
            fontSize: "0.75rem",
            lineHeight: "1.6",
          }}
          codeTagProps={{
            style: { fontFamily: "ui-monospace, SFMono-Regular, monospace" },
          }}
          className="veltrix-thin-scrollbar"
        >
          {code}
        </SyntaxHighlighter>
      )}
    </div>
  );
}

// Mermaid 图表块:默认渲染图表,可切「代码 ⇄ 图表」;复制 / 下载源码。
// plain=true(流式生成中):只显示源码,不渲染未完成的图表。配色随主题切换。
function MermaidBlock({ code, plain }: { code: string; plain?: boolean }) {
  const { resolvedTheme } = useTheme();
  const isDark = resolvedTheme === "dark";
  const [view, setView] = useState<"diagram" | "code">("diagram");
  const [svg, setSvg] = useState("");
  const [err, setErr] = useState("");
  const [copied, setCopied] = useState(false);

  useEffect(() => {
    if (plain || view !== "diagram") return;
    let cancelled = false;
    mermaid.initialize({
      startOnLoad: false,
      theme: isDark ? "dark" : "default",
      securityLevel: "strict",
      fontFamily: "inherit",
      // 自适应容器宽度;连线用曲线(沿用之前方式)
      flowchart: { useMaxWidth: true, htmlLabels: false, curve: "basis" },
      sequence: { useMaxWidth: true },
      gantt: { useMaxWidth: true },
    });
    // id 不能含特殊字符;随机化避免多图冲突
    const id = `mmd-${Math.random().toString(36).slice(2)}`;
    mermaid.render(id, code).then(
      (res) => {
        if (!cancelled) {
          setSvg(res.svg);
          setErr("");
        }
      },
      (e: unknown) => {
        if (!cancelled) {
          setErr(e instanceof Error ? e.message : String(e));
          setSvg("");
        }
      },
    );
    return () => {
      cancelled = true;
    };
  }, [code, isDark, view, plain]);

  async function copy() {
    try {
      await navigator.clipboard.writeText(code);
      setCopied(true);
      setTimeout(() => setCopied(false), 1500);
    } catch {
      toast.error("复制失败");
    }
  }
  async function download() {
    try {
      const path = await invoke<string | null>("save_text_dialog", {
        content: code,
        fileName: `diagram-${Date.now()}.mmd`,
      });
      if (path) {
        recordDownload({ path, kind: "图表" });
        toast.success("已保存");
      }
    } catch (e) {
      toast.error(`保存失败: ${e}`);
    }
  }

  const showCode = plain || view === "code";

  return (
    <div className="my-2 overflow-hidden rounded-lg border bg-muted/60">
      <div className="flex items-center justify-between border-b bg-muted/80 px-3 py-1">
        <span className="font-mono text-[11px] text-muted-foreground">
          mermaid
        </span>
        <div className="flex items-center gap-1">
          {/* 完成后才提供「代码 ⇄ 图表」切换 */}
          {!plain && (
            <button
              type="button"
              onClick={() =>
                setView((v) => (v === "diagram" ? "code" : "diagram"))
              }
              className="inline-flex items-center gap-1 rounded px-1.5 py-0.5 text-[11px] text-muted-foreground transition-colors hover:bg-accent hover:text-foreground"
            >
              {view === "diagram" ? (
                <>
                  <Code className="size-3" />
                  代码
                </>
              ) : (
                <>
                  <Workflow className="size-3" />
                  图表
                </>
              )}
            </button>
          )}
          <button
            type="button"
            onClick={copy}
            title="复制源码"
            className="inline-flex items-center gap-1 rounded px-1.5 py-0.5 text-[11px] text-muted-foreground transition-colors hover:bg-accent hover:text-foreground"
          >
            {copied ? <Check className="size-3" /> : <Copy className="size-3" />}
            {copied ? "已复制" : "复制"}
          </button>
          <button
            type="button"
            onClick={() => void download()}
            title="下载源码"
            className="inline-flex items-center gap-1 rounded px-1.5 py-0.5 text-[11px] text-muted-foreground transition-colors hover:bg-accent hover:text-foreground"
          >
            <Download className="size-3" />
            下载
          </button>
        </div>
      </div>
      {showCode ? (
        <pre className="veltrix-thin-scrollbar overflow-x-auto p-3 text-xs leading-relaxed">
          <code className="font-mono">{code}</code>
        </pre>
      ) : err ? (
        <div className="p-3 text-xs text-destructive">图表渲染失败:{err}</div>
      ) : svg ? (
        <div
          className="bg-card p-3 [&_svg]:mx-auto [&_svg]:block [&_svg]:h-auto [&_svg]:max-w-full"
          dangerouslySetInnerHTML={{ __html: svg }}
        />
      ) : (
        <div className="p-3 text-xs text-muted-foreground">图表渲染中…</div>
      )}
    </div>
  );
}

// 从 react-markdown 传入的 children 还原原始文本(代码块内容)
function childrenToText(children: ReactNode): string {
  if (typeof children === "string") return children;
  if (typeof children === "number") return String(children);
  if (Array.isArray(children)) return children.map(childrenToText).join("");
  if (children && typeof children === "object" && "props" in children) {
    const props = (children as { props?: { children?: ReactNode } }).props;
    return childrenToText(props?.children);
  }
  return "";
}

// 把流式文本切成「已完成块(head)+ 活跃尾块(tail)」:在不处于代码围栏(``` / ~~~)内的空行处切。
// head 在新块完成前内容不变 → 被 memo 跳过重解析;每帧只重解析小体量的 tail,避免长文每帧全量解析(O(n²))。
function splitStreamingMarkdown(content: string): [string, string] {
  const lines = content.split("\n");
  let inFence = false;
  let boundary = -1; // 最后一个「围栏外空行」的行号;head=之前所有行,tail=从该空行起
  for (let i = 0; i < lines.length; i++) {
    if (/^\s{0,3}(```|~~~)/.test(lines[i])) inFence = !inFence;
    if (!inFence && lines[i].trim() === "") boundary = i;
  }
  if (boundary <= 0) return ["", content];
  return [
    lines.slice(0, boundary).join("\n"),
    lines.slice(boundary).join("\n"),
  ];
}

// Markdown 渲染主体(被 memo):content 不变则跳过重新解析。
// streaming=true(流式生成中):代码块用普通 pre(不逐帧高亮),完成后再上语法高亮。
const MarkdownBody = memo(function MarkdownBody({
  content,
  streaming,
}: {
  content: string;
  streaming?: boolean;
}) {
  return (
    <Markdown
      remarkPlugins={[remarkGfm]}
      components={{
        // 代码块走 pre(读原始文本渲染 CodeBlock);行内 code 单独样式
        pre({ children }) {
          const el = (Array.isArray(children) ? children[0] : children) as
            | { props?: { className?: string } }
            | undefined;
          const className = el?.props?.className ?? "";
          const lang = /language-(\w+)/.exec(className)?.[1] ?? "";
          const code = childrenToText(children).replace(/\n$/, "");
          if (lang === "mermaid") {
            return <MermaidBlock code={code} plain={streaming} />;
          }
          return <CodeBlock code={code} lang={lang} plain={streaming} />;
        },
        code({ className, children }) {
          // 行内代码(块级已被 pre 接管)
          if (className?.includes("language-")) {
            return <code className={className}>{children}</code>;
          }
          return (
            <code className="rounded bg-muted px-1 py-0.5 font-mono text-[0.85em]">
              {children}
            </code>
          );
        },
        a({ href, children }) {
          return (
            <a href={href} target="_blank" rel="noreferrer noopener">
              {children}
            </a>
          );
        },
      }}
    >
      {content}
    </Markdown>
  );
});

// 外层容器 + 流式分块。流式时拆「已完成块 + 活跃尾块」,前者 memo 复用、不重解析,显著降低长文流式开销。
export const MarkdownMessage = memo(function MarkdownMessage({
  content,
  streaming,
}: {
  content: string;
  streaming?: boolean;
}) {
  let body: ReactNode;
  if (streaming) {
    const [head, tail] = splitStreamingMarkdown(content);
    body = (
      <>
        {head && <MarkdownBody content={head} streaming />}
        {tail && <MarkdownBody content={tail} streaming />}
      </>
    );
  } else {
    body = <MarkdownBody content={content} />;
  }
  return (
    <div className="text-sm leading-relaxed break-words [&_a]:text-primary [&_a]:underline [&_blockquote]:border-l-2 [&_blockquote]:border-border [&_blockquote]:pl-3 [&_blockquote]:text-muted-foreground [&_h1]:my-2 [&_h1]:text-base [&_h1]:font-semibold [&_h2]:my-2 [&_h2]:text-sm [&_h2]:font-semibold [&_h3]:my-1.5 [&_h3]:font-semibold [&_li]:my-0.5 [&_ol]:my-2 [&_ol]:list-decimal [&_ol]:pl-5 [&_p]:my-2 [&_table]:my-2 [&_table]:w-full [&_table]:border-collapse [&_td]:border [&_td]:px-2 [&_td]:py-1 [&_th]:border [&_th]:bg-muted [&_th]:px-2 [&_th]:py-1 [&_ul]:my-2 [&_ul]:list-disc [&_ul]:pl-5 first:[&>*]:mt-0 last:[&>*]:mb-0">
      {body}
    </div>
  );
});
