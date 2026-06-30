// 远程控制全局入口:顶部栏按钮 + 配对弹窗。
// 按钮颜色按 RemoteStatus 反映远程会话健康度,弹窗内驱动 cloud_pair_init 拿真实连接码。

import { useCallback, useEffect, useState, type ReactNode } from "react";
import {
  Copy,
  Info,
  Loader2,
  MonitorSmartphone,
  RefreshCw,
  Smartphone,
  Unplug,
} from "lucide-react";
import { toast } from "sonner";

import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { SimpleTooltip } from "@/components/SimpleTooltip";
import { api, type CloudConfigView, type CloudPairView } from "@/lib/api";

export type RemoteStatus = "connected" | "disconnected" | "failed";

const REMOTE_STATUS_META: Record<
  RemoteStatus,
  { label: string; className: string }
> = {
  connected: { label: "远程已连接", className: "text-emerald-500" },
  disconnected: { label: "远程未连接", className: "text-muted-foreground" },
  failed: { label: "远程连接失败", className: "text-destructive" },
};

// 顶部栏触发按钮 + 弹窗,组件自管 open 态,父组件只传 status
export function RemoteConnectButton({ status }: { status: RemoteStatus }) {
  const [open, setOpen] = useState(false);
  const meta = REMOTE_STATUS_META[status];

  return (
    <>
      <SimpleTooltip content={meta.label}>
        <button
          type="button"
          onClick={() => setOpen(true)}
          className={`relative inline-flex size-8 shrink-0 items-center justify-center rounded-md text-foreground transition-colors hover:bg-accent ${meta.className}`}
        >
          <MonitorSmartphone className="size-[1.1rem]" />
          {/* 状态点:连接 / 失败时高亮,未连接时省略 */}
          {status !== "disconnected" && (
            <span
              className={`absolute right-1 top-1 size-1.5 rounded-full ${
                status === "connected" ? "bg-emerald-500" : "bg-destructive"
              }`}
            />
          )}
          <span className="sr-only">{meta.label}</span>
        </button>
      </SimpleTooltip>
      <RemoteConnectDialog open={open} onOpenChange={setOpen} status={status} />
    </>
  );
}

function RemoteConnectDialog({
  open,
  onOpenChange,
  status,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  status: RemoteStatus;
}) {
  const isConnected = status === "connected";
  const [qrSeed, setQrSeed] = useState(0);

  const [cloudCfg, setCloudCfg] = useState<CloudConfigView | null>(null);
  const [pairData, setPairData] = useState<CloudPairView | null>(null);
  const [pairLoading, setPairLoading] = useState(false);
  const [pairError, setPairError] = useState<string | null>(null);

  const fetchPair = useCallback(async () => {
    setPairLoading(true);
    setPairError(null);
    try {
      const view = await api.cloudPairInit();
      setPairData(view);
      setQrSeed((s) => s + 1);
    } catch (e: unknown) {
      setPairError(String(e));
    } finally {
      setPairLoading(false);
    }
  }, []);

  useEffect(() => {
    if (!open || isConnected) return;
    let cancelled = false;
    api
      .cloudGetConfig()
      .then((cfg) => {
        if (cancelled) return;
        setCloudCfg(cfg);
        if (cfg.base_url && cfg.user_token) {
          void fetchPair();
        }
      })
      .catch((e: unknown) => {
        if (cancelled) return;
        setPairError(String(e));
      });
    return () => {
      cancelled = true;
    };
  }, [open, isConnected, fetchPair]);

  const realCode = pairData?.code ?? "";
  const pairCodeFormatted = realCode
    ? `${realCode.slice(0, 3)} ${realCode.slice(3)}`
    : "--- ---";

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="sm:max-w-md">
        <DialogHeader>
          <DialogTitle className="flex items-center gap-2">
            <Smartphone className="size-5" />
            远程控制
          </DialogTitle>
          <DialogDescription>
            使用 VeltrixLoop 手机 App 扫码连接,随时随地远程查看与监控本机的数据采集情况。
          </DialogDescription>
        </DialogHeader>

        {isConnected ? (
          <div className="space-y-3">
            <div className="flex items-center gap-3 rounded-lg border bg-card p-3">
              <div className="flex size-10 shrink-0 items-center justify-center rounded-lg bg-emerald-500/10">
                <Smartphone className="size-5 text-emerald-500" />
              </div>
              <div className="min-w-0 flex-1">
                <div className="flex items-center gap-1.5 font-medium">
                  <span className="truncate">已绑定设备</span>
                  <span className="size-2 shrink-0 rounded-full bg-emerald-500" />
                </div>
                <div className="text-xs text-muted-foreground">
                  在线 · 远程监控中
                </div>
              </div>
            </div>
            <div className="flex gap-2">
              <Button
                variant="outline"
                className="flex-1"
                onClick={() => {
                  toast.success("正在刷新重连…(待接后端)");
                }}
              >
                <RefreshCw />
                刷新重连
              </Button>
              <Button
                variant="destructive"
                className="flex-1"
                onClick={() => {
                  toast.success("已断开远程连接(待接后端)");
                  onOpenChange(false);
                }}
              >
                <Unplug />
                断开连接
              </Button>
            </div>
          </div>
        ) : (
          (() => {
            if (!cloudCfg) {
              return (
                <div className="flex items-center justify-center py-8 text-sm text-muted-foreground">
                  <Loader2 className="mr-2 size-4 animate-spin" />
                  加载云端配置…
                </div>
              );
            }

            if (!cloudCfg.base_url || !cloudCfg.user_token) {
              return (
                <div className="space-y-3 py-2">
                  <div className="rounded-lg border border-amber-500/30 bg-amber-500/10 px-3 py-2.5 text-sm text-amber-700 dark:text-amber-400">
                    {!cloudCfg.base_url
                      ? "尚未配置云端地址,请先到「系统配置 → 远程控制」完成配置。"
                      : "尚未登录云端,请先到「系统配置 → 远程控制」登录后再来配对。"}
                  </div>
                </div>
              );
            }

            return (
              <div className="flex flex-col items-center gap-3">
                <div className="rounded-xl border bg-white p-3 shadow-sm">
                  <QrPlaceholder seed={qrSeed} />
                </div>
                <div className="flex items-center gap-2 text-sm text-muted-foreground">
                  {pairLoading ? (
                    <>
                      <Loader2 className="size-4 animate-spin" />
                      正在申请连接码…
                    </>
                  ) : (
                    "打开手机 App 扫码,或在手机端手动输入下方连接码"
                  )}
                </div>

                <div className="w-full rounded-lg border bg-muted/30 px-3 py-2.5">
                  <div className="flex items-center justify-between gap-2">
                    <span className="text-xs text-muted-foreground">连接码</span>
                    <button
                      type="button"
                      className="inline-flex items-center gap-1 text-xs text-muted-foreground transition-colors hover:text-foreground disabled:opacity-50"
                      disabled={!realCode}
                      onClick={() => {
                        if (!realCode) return;
                        navigator.clipboard.writeText(realCode).then(
                          () => toast.success("连接码已复制"),
                          () => toast.error("复制失败"),
                        );
                      }}
                    >
                      <Copy className="size-3" />
                      复制
                    </button>
                  </div>
                  <div className="mt-1 text-center font-mono text-2xl font-semibold tracking-[0.3em] text-foreground">
                    {pairCodeFormatted}
                  </div>
                  <div className="mt-1 text-center text-[10px] text-muted-foreground">
                    {pairData
                      ? `${pairData.expires_in} 秒内有效 · 也可在手机端手动输入此码完成绑定`
                      : "等待生成…"}
                  </div>
                </div>

                {pairError && (
                  <div className="w-full text-center text-xs text-destructive">
                    {pairError}
                  </div>
                )}

                <Button
                  variant="outline"
                  size="sm"
                  onClick={fetchPair}
                  disabled={pairLoading}
                >
                  <RefreshCw />
                  刷新连接码
                </Button>
              </div>
            );
          })()
        )}

        <div className="flex items-center gap-3 rounded-lg border bg-muted/30 p-3">
          <div className="shrink-0 rounded-md border bg-white p-1.5">
            <QrPlaceholder seed={99} className="size-16" />
          </div>
          <div className="min-w-0 flex-1">
            <div className="text-sm font-medium">下载 VeltrixLoop 手机 App</div>
            <div className="mt-0.5 text-xs text-muted-foreground">
              扫码下载,支持 iOS / Android
            </div>
          </div>
        </div>

        <div className="flex items-start gap-2 rounded-lg bg-muted/50 px-3 py-2 text-xs text-muted-foreground">
          <Info className="mt-0.5 size-3.5 shrink-0" />
          出于数据安全考虑,同一时间仅允许一台设备远程连接。
        </div>
      </DialogContent>
    </Dialog>
  );
}

// 占位二维码:确定性伪随机图案 + 三个定位角,仅作示意。
// 真实二维码待装 qrcode.react 后由 pairData.qr_payload 驱动
function QrPlaceholder({
  seed = 0,
  className = "size-44",
}: {
  seed?: number;
  className?: string;
}) {
  const SIZE = 21;
  const dots: ReactNode[] = [];
  for (let row = 0; row < SIZE; row += 1) {
    for (let col = 0; col < SIZE; col += 1) {
      const inFinder =
        (row < 7 && col < 7) ||
        (row < 7 && col >= SIZE - 7) ||
        (row >= SIZE - 7 && col < 7);
      if (inFinder) continue;
      if ((row * 31 + col * 17 + row * col + seed * 13) % 3 === 0) {
        dots.push(
          <rect key={`${row}-${col}`} x={col} y={row} width="1" height="1" />,
        );
      }
    }
  }
  const finders: [number, number][] = [
    [0, 0],
    [SIZE - 7, 0],
    [0, SIZE - 7],
  ];
  return (
    <svg
      viewBox="0 0 21 21"
      shapeRendering="crispEdges"
      className={`${className} text-slate-900`}
    >
      <rect width="21" height="21" fill="white" />
      <g fill="currentColor">{dots}</g>
      {finders.map(([x, y]) => (
        <g key={`${x}-${y}`} fill="currentColor">
          <rect x={x} y={y} width="7" height="7" />
          <rect x={x + 1} y={y + 1} width="5" height="5" fill="white" />
          <rect x={x + 2} y={y + 2} width="3" height="3" />
        </g>
      ))}
    </svg>
  );
}
