// 编排器右侧富面板:RPA 浏览器(内嵌真实 webview + 接口拦截)。按 conversationId 复用同会话的子 webview。
// 与 rpa-layout 的 AgentWebviewHost/NetworkPanel 同源(暂自包含,后续可合并去重)。
import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import { listen } from "@tauri-apps/api/event";
import { ChevronDown, ChevronRight, Network } from "lucide-react";

import { api, type NetworkEntryView } from "@/lib/api";
import { Input } from "@/components/ui/input";

/// 一个会话的内嵌浏览器 + 接口面板。conversationId 为空时显示占位。
export function RpaWorkPanel({
  conversationId,
  sending,
}: {
  conversationId: string | null;
  sending: boolean;
}) {
  return (
    <div className="flex min-h-0 min-w-0 flex-1 flex-col">
      <AgentWebviewHost activeId={conversationId} sending={sending} />
      <NetworkPanel activeId={conversationId} />
    </div>
  );
}

// 内嵌浏览器宿主:占位区域,真实 webview 由后端 add_child 后按本区域 DOM rect 定位覆盖。
function AgentWebviewHost({
  activeId,
  sending,
}: {
  activeId: string | null;
  sending: boolean;
}) {
  const hostRef = useRef<HTMLDivElement>(null);
  const rafRef = useRef<number | null>(null);
  const activeRef = useRef(activeId);
  activeRef.current = activeId;

  const syncBounds = useCallback(() => {
    const id = activeRef.current;
    const el = hostRef.current;
    if (!id || !el) return;
    const r = el.getBoundingClientRect();
    void api
      .setAgentWebviewBounds(id, r.left, r.top, r.width, r.height)
      .catch((e) => console.debug("设置 webview 边界失败:", e));
  }, []);

  const scheduleSync = useCallback(() => {
    if (rafRef.current != null) cancelAnimationFrame(rafRef.current);
    rafRef.current = requestAnimationFrame(() => {
      rafRef.current = null;
      syncBounds();
    });
  }, [syncBounds]);

  useLayoutEffect(() => {
    scheduleSync();
    if (activeId)
      void api.showAgentWebview(activeId).catch((e) => console.debug("显示 webview 失败:", e));
    const ro = new ResizeObserver(() => scheduleSync());
    if (hostRef.current) ro.observe(hostRef.current);
    window.addEventListener("resize", scheduleSync);
    return () => {
      ro.disconnect();
      window.removeEventListener("resize", scheduleSync);
      if (activeId)
        void api.hideAgentWebview(activeId).catch((e) => console.debug("隐藏 webview 失败:", e));
    };
  }, [activeId, scheduleSync]);

  useEffect(() => {
    if (!sending) {
      scheduleSync();
      if (activeRef.current)
        void api
          .showAgentWebview(activeRef.current)
          .catch((e) => console.debug("显示 webview 失败:", e));
    }
  }, [sending, scheduleSync]);

  useEffect(() => {
    let dispose: (() => void) | undefined;
    let disposed = false;
    listen<{ conversationId: string }>("agent-webview-ready", (e) => {
      if (e.payload.conversationId !== activeRef.current) return;
      scheduleSync();
      void api
        .showAgentWebview(e.payload.conversationId)
        .catch((e) => console.debug("显示新 webview 失败:", e));
    }).then(
      (fn) => {
        if (disposed) fn();
        else dispose = fn;
      },
      () => {},
    );
    return () => {
      disposed = true;
      dispose?.();
    };
  }, [scheduleSync]);

  // 卸载(切走子面板 / 离开):藏掉全部内嵌 webview,防原生层盖住其它页面
  useEffect(() => {
    return () => {
      void api.hideAllAgentWebviews().catch((e) => console.debug("隐藏全部 webview 失败:", e));
    };
  }, []);

  return (
    <div className="relative min-h-0 flex-1 bg-muted/20">
      <div ref={hostRef} className="absolute inset-0" />
      {!activeId && (
        <div className="pointer-events-none absolute inset-0 flex items-center justify-center px-8 text-center text-xs text-muted-foreground">
          编排器委派浏览器任务后,这里实时显示内嵌浏览器的操作。
        </div>
      )}
    </div>
  );
}

interface NetRow {
  id: number;
  url: string;
  body: string;
}

// 接口拦截面板:实时显示内嵌 webview 发出的 JSON 响应。
function NetworkPanel({ activeId }: { activeId: string | null }) {
  const [open, setOpen] = useState(false);
  const [rows, setRows] = useState<NetRow[]>([]);
  const [filter, setFilter] = useState("");
  const [expanded, setExpanded] = useState<number | null>(null);
  const idRef = useRef(0);
  const activeRef = useRef(activeId);
  activeRef.current = activeId;

  const toRows = useCallback(
    (list: NetworkEntryView[]): NetRow[] =>
      list.map((e) => ({ id: idRef.current++, url: e.url, body: e.body })),
    [],
  );

  useEffect(() => {
    setRows([]);
    setExpanded(null);
    if (!activeId) return;
    api
      .getAgentNetwork(activeId)
      .then((list) => setRows(toRows(list)))
      .catch((e) => console.debug("加载网络拦截记录失败:", e));
  }, [activeId, toRows]);

  useEffect(() => {
    let dispose: (() => void) | undefined;
    let disposed = false;
    listen<{ conversationId: string; url: string; body: string }>("agent-network", (e) => {
      if (e.payload.conversationId !== activeRef.current) return;
      setRows((prev) => {
        const next = [...prev, { id: idRef.current++, url: e.payload.url, body: e.payload.body }];
        return next.length > 200 ? next.slice(next.length - 200) : next;
      });
    }).then(
      (fn) => {
        if (disposed) fn();
        else dispose = fn;
      },
      () => {},
    );
    return () => {
      disposed = true;
      dispose?.();
    };
  }, []);

  const shown = useMemo(() => {
    const f = filter.trim().toLowerCase();
    const list = f ? rows.filter((r) => r.url.toLowerCase().includes(f)) : rows;
    return list.slice().reverse();
  }, [rows, filter]);

  return (
    <div className="flex shrink-0 flex-col border-t">
      <button
        type="button"
        onClick={() => setOpen((v) => !v)}
        className="flex items-center gap-1.5 px-3 py-2 text-xs text-muted-foreground hover:bg-muted/40"
      >
        {open ? <ChevronDown className="size-3.5" /> : <ChevronRight className="size-3.5" />}
        <Network className="size-3.5 text-primary" />
        <span className="font-medium text-foreground">接口拦截</span>
        <span className="rounded bg-muted px-1.5 py-0.5 tabular-nums">{rows.length}</span>
      </button>
      {open && (
        <div className="flex h-56 flex-col gap-2 px-3 pb-3">
          <Input
            value={filter}
            onChange={(e) => setFilter(e.target.value)}
            placeholder="按 URL 关键词过滤,如 api / search"
            className="h-7 text-xs"
          />
          <div className="veltrix-thin-scrollbar min-h-0 flex-1 space-y-1 overflow-y-auto">
            {shown.length === 0 ? (
              <div className="px-1 py-6 text-center text-xs text-muted-foreground">
                {rows.length === 0
                  ? "暂无拦截到的 JSON 接口响应。"
                  : "没有匹配过滤词的接口"}
              </div>
            ) : (
              shown.map((r) => (
                <div key={r.id} className="rounded border bg-card/50">
                  <button
                    type="button"
                    onClick={() => setExpanded((cur) => (cur === r.id ? null : r.id))}
                    className="flex w-full items-start gap-1.5 px-2 py-1 text-left"
                  >
                    <span className="min-w-0 flex-1 break-all font-mono text-[11px] text-foreground">
                      {r.url}
                    </span>
                  </button>
                  {expanded === r.id && (
                    <pre className="veltrix-thin-scrollbar max-h-48 overflow-auto border-t bg-muted/30 px-2 py-1.5 text-[11px] leading-relaxed">
                      {r.body}
                    </pre>
                  )}
                </div>
              ))
            )}
          </div>
        </div>
      )}
    </div>
  );
}
