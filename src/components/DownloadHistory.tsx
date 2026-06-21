// 标题栏「历史下载记录」面板:列出导出/下载过的文件,逐条标记本地是否还存在,点击打开所在目录。
// 记录由各导出动作经 lib/download-history 写入 localStorage;存在性由后端 path_exists 实时判定。
import { useCallback, useEffect, useState } from "react";
import { FolderDown, FolderOpen, Trash2, X } from "lucide-react";
import { toast } from "sonner";

import { api } from "@/lib/api";
import {
  DOWNLOAD_HISTORY_EVENT,
  clearDownloadHistory,
  getDownloadHistory,
  removeDownloadRecord,
  type DownloadRecord,
} from "@/lib/download-history";
import {
  Popover,
  PopoverContent,
  PopoverTrigger,
} from "@/components/ui/popover";
import { SimpleTooltip } from "@/components/SimpleTooltip";

function fmtTime(ms: number): string {
  const d = new Date(ms);
  const p = (n: number) => String(n).padStart(2, "0");
  return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())} ${p(d.getHours())}:${p(d.getMinutes())}`;
}

export function DownloadHistory() {
  const [open, setOpen] = useState(false);
  const [records, setRecords] = useState<DownloadRecord[]>([]);
  // 路径 → 是否存在(undefined=检测中)
  const [exists, setExists] = useState<Record<string, boolean>>({});

  // 重新加载列表并逐条查本地存在性
  const refresh = useCallback(() => {
    const list = getDownloadHistory();
    setRecords(list);
    list.forEach((r) => {
      api
        .pathExists(r.path)
        .then((ok) => setExists((m) => ({ ...m, [r.path]: ok })))
        .catch(() => setExists((m) => ({ ...m, [r.path]: false })));
    });
  }, []);

  // 挂载即读一次(供计数角标);历史变更事件(导出成功)即时刷新
  useEffect(() => {
    setRecords(getDownloadHistory());
    const onChange = () => refresh();
    window.addEventListener(DOWNLOAD_HISTORY_EVENT, onChange);
    return () => window.removeEventListener(DOWNLOAD_HISTORY_EVENT, onChange);
  }, [refresh]);

  // 每次打开面板都重查存在性(文件可能已被外部删除/移动)
  useEffect(() => {
    if (open) refresh();
  }, [open, refresh]);

  async function reveal(r: DownloadRecord) {
    try {
      await api.revealPath(r.path);
    } catch (e) {
      toast.error(`打开目录失败:${e}`);
    }
  }

  const count = records.length;

  return (
    <Popover open={open} onOpenChange={setOpen}>
      <PopoverTrigger asChild>
        <button
          type="button"
          title="历史下载记录"
          className="inline-flex size-7 items-center justify-center rounded-md text-muted-foreground transition-colors hover:bg-accent hover:text-foreground"
        >
          <FolderDown className="size-4" />
          <span className="sr-only">历史下载记录</span>
        </button>
      </PopoverTrigger>
      <PopoverContent align="end" className="w-96 p-0">
        <div className="flex items-center justify-between border-b px-3 py-2">
          <span className="text-sm font-medium">历史下载记录</span>
          {count > 0 && (
            <button
              type="button"
              onClick={() => {
                clearDownloadHistory();
                setRecords([]);
              }}
              className="inline-flex items-center gap-1 rounded px-1.5 py-0.5 text-xs text-muted-foreground transition-colors hover:bg-accent hover:text-foreground"
            >
              <Trash2 className="size-3.5" /> 清空
            </button>
          )}
        </div>
        {count === 0 ? (
          <div className="px-3 py-8 text-center text-xs text-muted-foreground">
            暂无下载记录
          </div>
        ) : (
          <ul className="veltrix-thin-scrollbar max-h-80 overflow-y-auto py-1">
            {records.map((r) => {
              const ok = exists[r.path];
              return (
                <li
                  key={r.id}
                  className="group flex items-center gap-1.5 px-2 py-1.5 hover:bg-accent/60"
                >
                  <SimpleTooltip content={r.path}>
                    <button
                      type="button"
                      onClick={() => reveal(r)}
                      className="flex min-w-0 flex-1 flex-col items-start gap-0.5 text-left"
                    >
                      <span className="flex w-full items-center gap-1.5">
                        <span className="truncate text-xs font-medium">
                          {r.name}
                        </span>
                        <ExistBadge state={ok} />
                      </span>
                      <span className="truncate text-[11px] text-muted-foreground">
                        {r.kind} · {fmtTime(r.savedAt)}
                      </span>
                    </button>
                  </SimpleTooltip>
                  <SimpleTooltip
                    content={ok === false ? "文件已不在,打开所在目录" : "打开所在目录"}
                  >
                    <button
                      type="button"
                      onClick={() => reveal(r)}
                      aria-label="打开所在目录"
                      className="shrink-0 rounded p-1 text-muted-foreground transition-colors hover:bg-accent hover:text-foreground"
                    >
                      <FolderOpen className="size-3.5" />
                    </button>
                  </SimpleTooltip>
                  <SimpleTooltip content="移除该记录">
                    <button
                      type="button"
                      onClick={() => {
                        removeDownloadRecord(r.id);
                        setRecords(getDownloadHistory());
                      }}
                      aria-label="移除该记录"
                      className="shrink-0 rounded p-1 text-muted-foreground opacity-0 transition-colors group-hover:opacity-100 hover:bg-accent hover:text-foreground"
                    >
                      <X className="size-3.5" />
                    </button>
                  </SimpleTooltip>
                </li>
              );
            })}
          </ul>
        )}
      </PopoverContent>
    </Popover>
  );
}

// 本地存在性角标:检测中(灰)/ 本地存在(绿)/ 已丢失(红)
function ExistBadge({ state }: { state: boolean | undefined }) {
  if (state === undefined) {
    return (
      <span className="shrink-0 rounded-full bg-muted px-1.5 py-0.5 text-[10px] text-muted-foreground">
        检测中
      </span>
    );
  }
  return state ? (
    <span className="shrink-0 rounded-full bg-emerald-100 px-1.5 py-0.5 text-[10px] font-medium text-emerald-700 dark:bg-emerald-950/60 dark:text-emerald-300">
      本地存在
    </span>
  ) : (
    <span className="shrink-0 rounded-full bg-red-100 px-1.5 py-0.5 text-[10px] font-medium text-red-700 dark:bg-red-950/60 dark:text-red-300">
      已丢失
    </span>
  );
}
