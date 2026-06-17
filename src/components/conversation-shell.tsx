// 对话外壳:统一入口,按当前会话的 agent_type 自动分发页面布局,无需手动选择。
// 新会话先按 chat(单栏)呈现;发送时按意图判为 coding / rpa 则建对应会话并自动切到对应布局。
// 已建会话锁定其 agent_type。
import { useChat } from "@/hooks/use-chat";
import { ChatPage } from "@/pages/ChatPage";
import { CodingLayout } from "@/components/coding-layout";
import { RpaLayout } from "@/components/rpa-layout";

export function ConversationShell() {
  const { conversations, activeId, pendingAgentType } = useChat();
  const active = conversations.find((c) => c.id === activeId) ?? null;
  const agentType = active?.agentType ?? pendingAgentType;

  if (agentType === "coding") return <CodingLayout />;
  if (agentType === "rpa") return <RpaLayout />;
  return <ChatPage />;
}
