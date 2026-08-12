// AI 对话共享状态 Provider:会话列表 + 当前会话,供侧边栏(展示会话列表)与对话页共用。
// Context 定义与 useChat 读取 Hook 在 @/hooks/use-chat;本文件只导出组件,保证 Fast Refresh 可热更新。
import { useCallback, useEffect, useState, type ReactNode } from "react";
import { listen } from "@tauri-apps/api/event";
import { api, type ConversationView, type ProviderDto } from "@/lib/api";
import { ChatContext } from "@/hooks/use-chat";

export function ChatProvider({ children }: { children: ReactNode }) {
  const [conversations, setConversations] = useState<ConversationView[]>([]);
  const [activeId, setActiveId] = useState<string | null>(null);
  // 默认新会话=统一编排器(可直接对话,需要时把专门智能体当工具委派)
  const [pendingAgentType, setPendingAgentType] = useState<string>("orchestrator");
  const [pendingFirstMessage, setPendingFirstMessage] = useState<string | null>(
    null,
  );
  const [providers, setProviders] = useState<ProviderDto[]>([]);
  // 首条消息自动交接提示条:ChatPage 建好 Agent 会话后置位,ConversationShell 在新布局顶部渲染;状态随 Provider 卸载清理
  const [handoffNotice, setHandoffNotice] = useState<{
    convId: string;
    type: string;
    text: string;
  } | null>(null);
  // 「改回普通对话」预填文本:ChatPage 消费(填入输入框,不自动发送)后清空
  const [prefillMessage, setPrefillMessage] = useState<string | null>(null);

  const reload = useCallback(async () => {
    try {
      setConversations(await api.listConversations());
    } catch {
      // 未登录 / 后端未就绪时忽略,稍后由页面重试
    }
  }, []);

  useEffect(() => {
    void reload();
    // 模型厂商列表加载一次(全局共享);失败忽略,配置后重进对话工作区会重载
    api.listProviders().then(setProviders).catch((e) => console.warn("加载模型厂商列表失败:", e));
  }, [reload]);

  // 后端 AI 概括出会话标题后经 conversation-title 事件推送:就地更新列表里对应会话,不必整表重拉
  useEffect(() => {
    const unlisten = listen<{ conversationId: string; title: string }>(
      "conversation-title",
      (e) => {
        setConversations((prev) =>
          prev.map((c) =>
            c.id === e.payload.conversationId
              ? { ...c, title: e.payload.title }
              : c,
          ),
        );
      },
    );
    return () => {
      unlisten.then((fn) => fn());
    };
  }, []);

  return (
    <ChatContext.Provider
      value={{
        conversations,
        activeId,
        setActiveId,
        providers,
        pendingAgentType,
        setPendingAgentType,
        pendingFirstMessage,
        setPendingFirstMessage,
        handoffNotice,
        setHandoffNotice,
        prefillMessage,
        setPrefillMessage,
        reload,
      }}
    >
      {children}
    </ChatContext.Provider>
  );
}
