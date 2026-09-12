// 屏幕录制入口(供标题栏、「电脑操作」页与对话页输入框加号复用):
// 点击「开 / 关悬浮控制条」(已开且未录制时再次点击 = 收起),真正的开始/停止都在悬浮条上手动操作。
// 这里另监听后端「录制已保存 / 录制失败」事件,在主窗口弹提示(悬浮窗没有 Toaster)。
import { useEffect, useRef } from "react";
import { listen } from "@tauri-apps/api/event";
import { toast } from "sonner";

import { api } from "@/lib/api";

// onSaved 处理器注册表 + 共享监听:标题栏常驻挂载,对话/电脑操作页也各自挂本 hook,
// 若每个实例各挂一份 listen,保存事件会被多处消费(页面挂起录屏的同时又弹默认提示)。
// 改为模块级单份监听统一分发:有注册处理器则全部转交(如挂到对话输入区),没有才走默认 toast。
const savedHandlers = new Set<(path: string) => void>();
let listenersReady: Promise<void> | undefined;

function ensureListeners(): Promise<void> {
  if (listenersReady) return listenersReady;
  listenersReady = Promise.all([
    // 录屏保存完成(后端停止录制后向主窗口推送)
    listen<{ path: string }>("recording-saved", (e) => {
      const path = e.payload.path;
      const handlers = [...savedHandlers];
      if (handlers.length > 0) {
        handlers.forEach((h) => h(path));
        return;
      }
      toast.success("屏幕录制已保存", {
        action: {
          label: "打开文件夹",
          onClick: () => {
            api
              .revealPath(path)
              .catch((err) => toast.error(`打开失败: ${err}`));
          },
        },
      });
    }),
    // 录制失败(ffmpeg 启动即退 / 未产出有效视频):后端已还原主窗口,这里弹错误提示
    listen<{ message: string }>("recording-failed", (e) => {
      toast.error(e.payload.message || "屏幕录制失败");
    }),
  ]).then(() => undefined);
  return listenersReady;
}

// onSaved:录制保存完成时的处理(如把视频加入当前对话)。提供则接管保存事件(不再弹默认提示);
// 不提供(如标题栏入口)则仅激活共享监听,事件落默认 toast。用 ref 持有,避免回调每次变更导致重订阅。
export function useScreenRecording(onSaved?: (path: string) => void) {
  const onSavedRef = useRef(onSaved);
  onSavedRef.current = onSaved;

  useEffect(() => {
    void ensureListeners();
    if (!onSavedRef.current) return;
    const handler = (path: string) => onSavedRef.current?.(path);
    savedHandlers.add(handler);
    return () => {
      savedHandlers.delete(handler);
    };
  }, []);

  // 打开录屏悬浮控制条(ffmpeg 不可用等错误在此弹 toast,主窗口可见)
  async function openOverlay() {
    try {
      await api.openRecordingOverlay();
    } catch (e) {
      toast.error(`打开屏幕录制失败: ${e}`);
    }
  }

  return { openOverlay };
}
