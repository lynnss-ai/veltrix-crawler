// 屏幕录制入口(供「电脑操作」页与对话页输入框加号复用):
// 加号点击只「打开悬浮控制条」,真正的开始/停止/录音开关都在悬浮条上手动操作。
// 这里另监听后端「录制已保存」事件,在主窗口弹提示(悬浮窗没有 Toaster)。
import { useEffect, useRef } from "react";
import { listen } from "@tauri-apps/api/event";
import { toast } from "sonner";

import { api } from "@/lib/api";

// onSaved:录制保存完成时的处理(如把视频加入当前对话)。提供则由调用方接管提示;
// 不提供则走默认 toast(「打开文件夹」)。用 ref 持有,避免回调每次变更导致监听重订阅。
export function useScreenRecording(onSaved?: (path: string) => void) {
  const onSavedRef = useRef(onSaved);
  onSavedRef.current = onSaved;

  // 录屏保存完成(后端停止录制后向主窗口推送):优先交回调,否则弹提示并提供「打开所在文件夹」
  useEffect(() => {
    let dispose: (() => void) | undefined;
    let disposed = false;
    listen<{ path: string }>("recording-saved", (e) => {
      const path = e.payload.path;
      const cb = onSavedRef.current;
      if (cb) {
        cb(path);
        return;
      }
      toast.success("屏幕录制已保存", {
        action: {
          label: "打开文件夹",
          onClick: () => {
            api.revealPath(path).catch((err) => toast.error(`打开失败: ${err}`));
          },
        },
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
