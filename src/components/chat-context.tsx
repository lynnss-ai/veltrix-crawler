// AI 对话共享状态:会话列表 + 当前会话,供侧边栏(展示会话列表)与对话页共用。
import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useState,
  type ReactNode,
} from "react";
import { api, type ConversationView } from "@/lib/api";

interface ChatContextValue {
  conversations: ConversationView[];
  activeId: string | null;
  setActiveId: (id: string | null) => void;
  /** 从后端重新拉取会话列表(新建 / 删除 / 改标题后调用) */
  reload: () => Promise<void>;
}

const ChatContext = createContext<ChatContextValue | null>(null);

export function ChatProvider({ children }: { children: ReactNode }) {
  const [conversations, setConversations] = useState<ConversationView[]>([]);
  const [activeId, setActiveId] = useState<string | null>(null);

  const reload = useCallback(async () => {
    try {
      setConversations(await api.listConversations());
    } catch {
      // 未登录 / 后端未就绪时忽略,稍后由页面重试
    }
  }, []);

  useEffect(() => {
    void reload();
  }, [reload]);

  return (
    <ChatContext.Provider
      value={{ conversations, activeId, setActiveId, reload }}
    >
      {children}
    </ChatContext.Provider>
  );
}

export function useChat(): ChatContextValue {
  const ctx = useContext(ChatContext);
  if (!ctx) throw new Error("useChat 必须在 ChatProvider 内使用");
  return ctx;
}
