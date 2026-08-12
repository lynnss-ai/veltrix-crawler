// 对话共享状态的 Context 定义与读取 Hook。
// 从 chat-context.tsx 抽出:让 chat-context.tsx 只导出 ChatProvider 组件,
// 恢复其 Fast Refresh 热更新(组件文件混入非组件导出会被 react-refresh 判为不兼容,触发整页刷新)。
import { createContext, useContext } from "react";
import type { ConversationView, ProviderDto } from "@/lib/api";

export interface ChatContextValue {
  conversations: ConversationView[];
  activeId: string | null;
  setActiveId: (id: string | null) => void;
  /** 模型厂商列表:登录后在 Provider 层加载一次、全局共享,避免各布局重挂载时重拉导致「尚无可用模型」竞态 */
  providers: ProviderDto[];
  /** 新建会话时的待用场景类型(chat / coding);开新会话(activeId=null)时决定首条消息建会话的 agent_type 与布局 */
  pendingAgentType: string;
  setPendingAgentType: (t: string) => void;
  /** 交接给新布局的首条消息:对话页按意图判为编程时,建好 coding 会话后把首条消息交给 CodingLayout 自动发送 */
  pendingFirstMessage: string | null;
  setPendingFirstMessage: (m: string | null) => void;
  /** 首条消息自动交接后的顶部提示条(「已为你切换到 XX 智能体」):ChatPage 交接时置位,ConversationShell 在新布局顶部渲染 */
  handoffNotice: { convId: string; type: string; text: string } | null;
  setHandoffNotice: (
    n: { convId: string; type: string; text: string } | null,
  ) => void;
  /** 「改回普通对话」预填文本:把交接前的首条消息放进 ChatPage 输入框(不自动发送),消费后清空 */
  prefillMessage: string | null;
  setPrefillMessage: (m: string | null) => void;
  /** 从后端重新拉取会话列表(新建 / 删除 / 改标题后调用) */
  reload: () => Promise<void>;
}

export const ChatContext = createContext<ChatContextValue | null>(null);

export function useChat(): ChatContextValue {
  const ctx = useContext(ChatContext);
  if (!ctx) throw new Error("useChat 必须在 ChatProvider 内使用");
  return ctx;
}
