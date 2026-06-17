// 代码编辑器:CodeMirror 6,按扩展名语法高亮 + 可编辑 + 保存(Ctrl/Cmd+S 或按钮)。
// 用于文件面板的代码预览/编辑;暗色跟随全局主题(监听 <html> 的 dark class)。
import { useEffect, useState } from "react";
import CodeMirror from "@uiw/react-codemirror";
import { EditorView } from "@codemirror/view";
import { javascript } from "@codemirror/lang-javascript";
import { html } from "@codemirror/lang-html";
import { css } from "@codemirror/lang-css";
import { json } from "@codemirror/lang-json";
import { python } from "@codemirror/lang-python";
import { markdown } from "@codemirror/lang-markdown";
import { rust } from "@codemirror/lang-rust";
import { oneDark } from "@codemirror/theme-one-dark";
import { Loader2, Save } from "lucide-react";

import { Button } from "@/components/ui/button";

// 按文件扩展名挑选语言高亮扩展(覆盖前端主力 + 常见脚本;未知类型不高亮)
function langExt(path: string) {
  const ext = path.split(".").pop()?.toLowerCase() ?? "";
  if (["js", "jsx", "mjs", "cjs"].includes(ext)) return [javascript({ jsx: true })];
  if (ext === "ts") return [javascript({ typescript: true })];
  if (ext === "tsx") return [javascript({ typescript: true, jsx: true })];
  if (["html", "htm", "vue", "svelte"].includes(ext)) return [html()];
  if (["css", "scss", "less"].includes(ext)) return [css()];
  if (ext === "json") return [json()];
  if (ext === "py") return [python()];
  if (["md", "markdown"].includes(ext)) return [markdown()];
  if (ext === "rs") return [rust()];
  return [];
}

// 等宽字体 + 字号 + VSCode 风格观感(折叠槽 / 行号 / 细滚动条)
const fontTheme = EditorView.theme({
  "&": { fontSize: "13px", height: "100%" },
  ".cm-scroller": {
    fontFamily:
      "'Cascadia Code','JetBrains Mono','Source Code Pro',Consolas,'Courier New',monospace",
    lineHeight: "1.6",
    // 横纵都可滚动(长行不换行,与 VSCode 一致)
    overflow: "auto",
  },
  ".cm-gutters": { border: "none", backgroundColor: "transparent" },
  // 折叠槽:平时淡、悬停高亮(VSCode 习惯)
  ".cm-foldGutter .cm-gutterElement": { cursor: "pointer", opacity: "0.5" },
  ".cm-foldGutter .cm-gutterElement:hover": { opacity: "1" },
  // 折叠后的占位符更醒目
  ".cm-foldPlaceholder": {
    backgroundColor: "rgba(128,128,128,0.18)",
    border: "none",
    borderRadius: "3px",
    color: "inherit",
    padding: "0 4px",
    margin: "0 2px",
  },
  // 细滚动条(浅灰、悬停加深),贴近 VSCode
  ".cm-scroller::-webkit-scrollbar": { width: "12px", height: "12px" },
  ".cm-scroller::-webkit-scrollbar-thumb": {
    backgroundColor: "rgba(128,128,128,0.4)",
    borderRadius: "6px",
    border: "3px solid transparent",
    backgroundClip: "content-box",
  },
  ".cm-scroller::-webkit-scrollbar-thumb:hover": {
    backgroundColor: "rgba(128,128,128,0.65)",
  },
});

export function CodeEditor({
  path,
  value,
  onSave,
}: {
  path: string;
  value: string;
  onSave: (content: string) => Promise<void> | void;
}) {
  const [draft, setDraft] = useState(value);
  const [saving, setSaving] = useState(false);
  // 暗色跟随 <html> 的 dark class(不依赖具体主题方案,切换实时响应)
  const [isDark, setIsDark] = useState(
    () =>
      typeof document !== "undefined" &&
      document.documentElement.classList.contains("dark"),
  );
  useEffect(() => {
    const obs = new MutationObserver(() =>
      setIsDark(document.documentElement.classList.contains("dark")),
    );
    obs.observe(document.documentElement, {
      attributes: true,
      attributeFilter: ["class"],
    });
    return () => obs.disconnect();
  }, []);

  // 外部内容刷新(保存后重读 / 同名文件内容变化)时同步草稿
  useEffect(() => {
    setDraft(value);
  }, [value]);

  const dirty = draft !== value;

  async function save() {
    if (!dirty || saving) return;
    setSaving(true);
    try {
      await onSave(draft);
    } finally {
      setSaving(false);
    }
  }

  return (
    <div
      className="flex min-h-0 flex-1 flex-col"
      onKeyDownCapture={(e) => {
        // Ctrl/Cmd+S 保存(拦截浏览器默认保存)
        if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "s") {
          e.preventDefault();
          void save();
        }
      }}
    >
      <div className="flex shrink-0 items-center justify-between border-b px-2 py-1 text-[11px] text-muted-foreground">
        <span className="truncate font-mono" title={path}>
          {path}
          {dirty ? " ●" : ""}
        </span>
        <Button
          type="button"
          size="sm"
          variant={dirty ? "default" : "outline"}
          className="h-6 shrink-0 gap-1 px-2 text-[11px]"
          disabled={!dirty || saving}
          onClick={() => void save()}
          title="保存(Ctrl/Cmd+S)"
        >
          {saving ? (
            <Loader2 className="size-3 animate-spin" />
          ) : (
            <Save className="size-3" />
          )}
          保存
        </Button>
      </div>
      <div className="min-h-0 flex-1 overflow-hidden">
        <CodeMirror
          value={draft}
          height="100%"
          theme={isDark ? oneDark : "light"}
          extensions={[...langExt(path), fontTheme]}
          onChange={(v) => setDraft(v)}
          basicSetup={{
            lineNumbers: true,
            highlightActiveLine: true,
            highlightActiveLineGutter: true,
            foldGutter: true,
            foldKeymap: true,
            bracketMatching: true,
            closeBrackets: true,
            autocompletion: true,
            indentOnInput: true,
            highlightSelectionMatches: true,
            searchKeymap: true,
          }}
        />
      </div>
    </div>
  );
}
