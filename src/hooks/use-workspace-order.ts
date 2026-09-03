// 侧栏服务(运营 / 对话 / 创作 / 发布服务)排列顺序:存 localStorage,系统设置里可调,侧栏即时响应。
import { useEffect, useState } from "react";
import type { ServiceKey } from "@/components/app-sidebar";

const STORAGE_KEY = "veltrix.workspace.order";
// 改动后用自定义事件通知同窗口的侧栏即时刷新(localStorage 的 storage 事件只跨窗口触发)
const CHANGE_EVENT = "veltrix-workspace-order-changed";
// 合法服务集合,数组顺序即默认顺序。服务栏只平铺工作区(运营/对话/创作);
// 发布服务是独立产品、不进服务栏,入口在 Logo 右侧「切换平台」。
const ALL: ServiceKey[] = ["management", "chat", "cowork", "publish"];

/** 读取保存的服务顺序;缺失 / 损坏时回退默认顺序。 */
export function readWorkspaceOrder(): ServiceKey[] {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (raw) {
      const arr = JSON.parse(raw);
      if (Array.isArray(arr)) {
        // 容忍性合并:保留已保存的合法顺序,后来新增的服务按默认序补尾——
        // 老配置(如只有 3 个工作区)不因服务增加而整体失效
        const kept = arr.filter((k): k is ServiceKey => ALL.includes(k));
        return [...kept, ...ALL.filter((k) => !kept.includes(k))];
      }
    }
  } catch {
    // 解析失败回退默认
  }
  return ALL;
}

/** 持久化服务顺序并广播变更事件。 */
export function writeWorkspaceOrder(order: ServiceKey[]): void {
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(order));
  } catch {
    // localStorage 不可用(隐私模式等)时仅本次会话生效
  }
  window.dispatchEvent(new Event(CHANGE_EVENT));
}

/** 响应式服务顺序:系统设置改动经自定义事件即时同步到侧栏。 */
export function useWorkspaceOrder(): [ServiceKey[], (order: ServiceKey[]) => void] {
  const [order, setOrder] = useState<ServiceKey[]>(readWorkspaceOrder);
  useEffect(() => {
    const sync = () => setOrder(readWorkspaceOrder());
    window.addEventListener(CHANGE_EVENT, sync);
    window.addEventListener("storage", sync);
    return () => {
      window.removeEventListener(CHANGE_EVENT, sync);
      window.removeEventListener("storage", sync);
    };
  }, []);
  const update = (next: ServiceKey[]) => {
    writeWorkspaceOrder(next);
    setOrder(next);
  };
  return [order, update];
}
