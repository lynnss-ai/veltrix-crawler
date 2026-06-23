import { useEffect, type Dispatch, type RefObject, type SetStateAction } from "react";
import { listen } from "@tauri-apps/api/event";

// agent-step 进度事件监听:coding / rpa / computer 三个 ReAct 智能体页共用的那段
// 「listen + dispose/disposed 收尾」样板(原本各写一遍、易在卸载时机上出错)。
// 过滤用的会话 ref 由各页传入(coding 按当前查看会话 activeIdRef,rpa/computer 按发送会话
// sendingConvRef),命中即把步骤标签追加进各页自己的 steps 状态。
// matchRef / setSteps 均为稳定引用,监听只需在挂载时建立一次。
export function useAgentStepListener(
  matchRef: RefObject<string | null>,
  setSteps: Dispatch<SetStateAction<string[]>>,
): void {
  useEffect(() => {
    let dispose: (() => void) | undefined;
    let disposed = false;
    listen<{ conversationId: string; label: string }>("agent-step", (e) => {
      if (matchRef.current !== e.payload.conversationId) return;
      setSteps((prev) => [...prev, e.payload.label]);
    }).then(
      (fn) => {
        if (disposed) fn();
        else dispose = fn;
      },
      (err) => {
        // 监听注册失败:agent 进度标签将不更新,记日志便于排查(而非静默)
        console.error("agent-step 监听注册失败:", err);
      },
    );
    return () => {
      disposed = true;
      dispose?.();
    };
    // matchRef / setSteps 稳定,监听挂载一次即可
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);
}
