// 对话外壳:统一入口,按当前会话的 agent_type 自动分发页面布局,无需手动选择。
// 新会话先按 chat(单栏)呈现;发送时按意图判为 coding / rpa 则建对应会话并自动切到对应布局。
// 已建会话锁定其 agent_type。
import { X } from "lucide-react";
import { useChat } from "@/hooks/use-chat";
import { ChatPage } from "@/pages/ChatPage";
import { CodingLayout } from "@/components/coding-layout";
import { RpaLayout } from "@/components/rpa-layout";
import { ComputerLayout } from "@/components/computer-layout";
import { LocalLayout } from "@/components/local-layout";

// 交接提示条里的 Agent 中文名(与 ChatPage 的 AGENT_LABELS 保持一致)
const AGENT_LABELS: Record<string, string> = {
  coding: "编程",
  rpa: "RPA 浏览器",
  computer: "电脑操作",
  local: "本机助手",
};

export function ConversationShell() {
  const {
    conversations,
    activeId,
    setActiveId,
    pendingAgentType,
    setPendingAgentType,
    handoffNotice,
    setHandoffNotice,
    setPrefillMessage,
  } = useChat();
  const active = conversations.find((c) => c.id === activeId) ?? null;
  const agentType = active?.agentType ?? pendingAgentType;

  // 「改回普通对话」:回到待建的普通 chat 会话,把交接前的首条消息预填进输入框(不自动发送,由用户决定)
  function backToPlainChat() {
    const n = handoffNotice;
    if (!n) return;
    setHandoffNotice(null);
    setPendingAgentType("chat");
    setActiveId(null);
    setPrefillMessage(n.text);
  }

  const layout =
    agentType === "coding" ? (
      <CodingLayout />
    ) : agentType === "rpa" ? (
      <RpaLayout />
    ) : agentType === "computer" ? (
      <ComputerLayout />
    ) : agentType === "local" ? (
      <LocalLayout />
    ) : (
      // orchestrator(默认)与 legacy chat 都走 ChatPage,由其内部按 agentType 切换发送/渲染
      <ChatPage />
    );

  // 交接提示条:只在仍停留在交接目标会话时显示;关闭/改回即清,状态在 ChatProvider,随工作区卸载清理
  const showNotice = handoffNotice && activeId === handoffNotice.convId;

  return (
    <div className="flex min-h-0 min-w-0 flex-1 flex-col">
      {showNotice && (
        <div className="flex shrink-0 items-center gap-2 border-b bg-primary/5 px-4 py-2 text-sm">
          <span className="flex-1 text-foreground">
            已为你切换到「{AGENT_LABELS[handoffNotice.type] ?? handoffNotice.type}
            」智能体,首条消息将由它处理
          </span>
          <button
            type="button"
            onClick={backToPlainChat}
            className="shrink-0 rounded-md border border-primary/40 px-2 py-1 text-xs text-primary hover:bg-primary/10"
          >
            改回普通对话
          </button>
          <button
            type="button"
            onClick={() => setHandoffNotice(null)}
            title="关闭提示"
            className="shrink-0 text-muted-foreground hover:text-foreground"
          >
            <X className="size-4" />
          </button>
        </div>
      )}
      {layout}
    </div>
  );
}
