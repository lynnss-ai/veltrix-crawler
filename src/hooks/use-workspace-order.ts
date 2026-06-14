// 工作区(营销 / 对话 / 协作)排列顺序:存 localStorage,系统配置里可调,侧栏即时响应。
import { useEffect, useState } from "react";
import type { Workspace } from "@/components/app-sidebar";

const STORAGE_KEY = "veltrix.workspace.order";
// 改动后用自定义事件通知同窗口的侧栏即时刷新(localStorage 的 storage 事件只跨窗口触发)
const CHANGE_EVENT = "veltrix-workspace-order-changed";
// 合法工作区集合,数组顺序即默认顺序
const ALL: Workspace[] = ["management", "chat", "cowork"];

/** 读取保存的工作区顺序;缺失 / 损坏 / 与合法集合不符时回退默认顺序。 */
export function readWorkspaceOrder(): Workspace[] {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (raw) {
      const arr = JSON.parse(raw) as Workspace[];
      // 必须恰好覆盖全部合法 key(防止增删工作区后旧配置残缺)
      if (
        Array.isArray(arr) &&
        arr.length === ALL.length &&
        ALL.every((k) => arr.includes(k))
      ) {
        return arr;
      }
    }
  } catch {
    // 解析失败回退默认
  }
  return ALL;
}

/** 持久化工作区顺序并广播变更事件。 */
export function writeWorkspaceOrder(order: Workspace[]): void {
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(order));
  } catch {
    // localStorage 不可用(隐私模式等)时仅本次会话生效
  }
  window.dispatchEvent(new Event(CHANGE_EVENT));
}

/** 响应式工作区顺序:系统配置改动经自定义事件即时同步到侧栏。 */
export function useWorkspaceOrder(): [Workspace[], (order: Workspace[]) => void] {
  const [order, setOrder] = useState<Workspace[]>(readWorkspaceOrder);
  useEffect(() => {
    const sync = () => setOrder(readWorkspaceOrder());
    window.addEventListener(CHANGE_EVENT, sync);
    window.addEventListener("storage", sync);
    return () => {
      window.removeEventListener(CHANGE_EVENT, sync);
      window.removeEventListener("storage", sync);
    };
  }, []);
  const update = (next: Workspace[]) => {
    writeWorkspaceOrder(next);
    setOrder(next);
  };
  return [order, update];
}
