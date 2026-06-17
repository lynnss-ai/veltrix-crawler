import { useEffect, useState } from "react";
import { Loader2, Plus, SquarePen, Trash2 } from "lucide-react";
import { toast } from "sonner";

import { api, type ChatMemoryView } from "@/lib/api";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Switch } from "@/components/ui/switch";
import { Textarea } from "@/components/ui/textarea";
import { SimpleTooltip } from "@/components/SimpleTooltip";
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
} from "@/components/ui/alert-dialog";

// 全局用户记忆管理:总开关 + 列表(自动/手动)+ 增删改 + 单条启停 + 清空。
// 同时用于「系统设置 → AI 记忆」分区与对话页「记忆管理」弹窗,逻辑保持单一来源。
export function ChatMemoryManager() {
  const [enabled, setEnabled] = useState(true);
  const [memories, setMemories] = useState<ChatMemoryView[]>([]);
  const [loading, setLoading] = useState(true);
  const [newContent, setNewContent] = useState("");
  const [adding, setAdding] = useState(false);
  // 行内编辑:正在编辑的记忆 id 与草稿
  const [editingId, setEditingId] = useState<number | null>(null);
  const [editDraft, setEditDraft] = useState("");
  const [clearOpen, setClearOpen] = useState(false);

  function reload() {
    setLoading(true);
    api
      .listChatMemories()
      .then(setMemories)
      .catch((e) => toast.error(`加载记忆失败: ${e}`))
      .finally(() => setLoading(false));
  }

  useEffect(() => {
    api.getChatMemoryEnabled().then(setEnabled).catch(() => {});
    reload();
  }, []);

  // 全局开关:乐观更新,失败回滚
  async function toggleEnabled(next: boolean) {
    setEnabled(next);
    try {
      await api.setChatMemoryEnabled(next);
    } catch (e) {
      setEnabled(!next);
      toast.error(`保存失败: ${e}`);
    }
  }

  async function addMemory() {
    const text = newContent.trim();
    if (!text) return;
    setAdding(true);
    try {
      await api.addChatMemory(text);
      setNewContent("");
      reload();
    } catch (e) {
      toast.error(`添加失败: ${e}`);
    } finally {
      setAdding(false);
    }
  }

  // 单条启用/停用:仅影响该条是否注入对话(乐观更新)
  async function toggleItem(memory: ChatMemoryView) {
    const next = !memory.enabled;
    setMemories((prev) =>
      prev.map((x) => (x.id === memory.id ? { ...x, enabled: next } : x)),
    );
    try {
      await api.updateChatMemory(memory.id, memory.content, next);
    } catch (e) {
      setMemories((prev) =>
        prev.map((x) =>
          x.id === memory.id ? { ...x, enabled: memory.enabled } : x,
        ),
      );
      toast.error(`更新失败: ${e}`);
    }
  }

  function startEdit(memory: ChatMemoryView) {
    setEditingId(memory.id);
    setEditDraft(memory.content);
  }

  async function saveEdit(memory: ChatMemoryView) {
    const text = editDraft.trim();
    if (!text) {
      toast.error("记忆内容不能为空");
      return;
    }
    try {
      await api.updateChatMemory(memory.id, text, memory.enabled);
      setMemories((prev) =>
        prev.map((x) => (x.id === memory.id ? { ...x, content: text } : x)),
      );
      setEditingId(null);
    } catch (e) {
      toast.error(`保存失败: ${e}`);
    }
  }

  async function removeItem(id: number) {
    try {
      await api.deleteChatMemory(id);
      setMemories((prev) => prev.filter((x) => x.id !== id));
    } catch (e) {
      toast.error(`删除失败: ${e}`);
    }
  }

  async function clearAll() {
    try {
      await api.clearChatMemories();
      setMemories([]);
      setClearOpen(false);
      toast.success("已清空全部记忆");
    } catch (e) {
      toast.error(`清空失败: ${e}`);
    }
  }

  return (
    <div className="space-y-4">
      {/* 全局开关 */}
      <div className="flex items-center justify-between rounded-md border bg-muted/20 px-3 py-2.5">
        <div className="space-y-0.5">
          <div className="text-sm font-medium">启用对话记忆</div>
          <div className="text-xs text-muted-foreground">
            关闭后:不再自动提取新记忆,也不会把已有记忆注入对话(记忆仍会保留)。
          </div>
        </div>
        <Switch checked={enabled} onCheckedChange={toggleEnabled} />
      </div>

      {/* 手动添加 */}
      <div className="flex gap-2">
        <Input
          placeholder="手动添加一条记忆,如:我是一名前端工程师"
          value={newContent}
          onChange={(e) => setNewContent(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter") {
              e.preventDefault();
              void addMemory();
            }
          }}
        />
        <Button
          className="shrink-0"
          onClick={() => void addMemory()}
          disabled={adding || !newContent.trim()}
        >
          {adding ? (
            <Loader2 className="size-4 animate-spin" />
          ) : (
            <Plus className="size-4" />
          )}
          添加
        </Button>
      </div>

      {/* 记忆列表 */}
      {loading ? (
        <div className="flex items-center justify-center py-10 text-sm text-muted-foreground">
          <Loader2 className="mr-2 size-4 animate-spin" />
          加载中…
        </div>
      ) : memories.length === 0 ? (
        <div className="rounded-md border border-dashed py-10 text-center text-sm text-muted-foreground">
          暂无记忆。开启后 AI 会在对话中自动积累,你也可以在上方手动添加。
        </div>
      ) : (
        <div className="space-y-2">
          <div className="flex items-center justify-between">
            <span className="text-xs text-muted-foreground">
              共 {memories.length} 条
            </span>
            <Button
              variant="ghost"
              size="sm"
              className="h-7 text-xs text-destructive hover:text-destructive"
              onClick={() => setClearOpen(true)}
            >
              <Trash2 className="size-3.5" />
              清空全部
            </Button>
          </div>
          <div className="veltrix-thin-scrollbar max-h-[50vh] space-y-2 overflow-auto pr-1">
            {memories.map((m) => (
              <div
                key={m.id}
                className="group flex items-start gap-2.5 rounded-md border bg-card px-3 py-2.5"
              >
                <Switch
                  className="mt-0.5 shrink-0"
                  checked={m.enabled}
                  onCheckedChange={() => void toggleItem(m)}
                />
                <div className="min-w-0 flex-1">
                  {editingId === m.id ? (
                    <div className="flex flex-col gap-2">
                      <Textarea
                        autoFocus
                        value={editDraft}
                        onChange={(e) => setEditDraft(e.target.value)}
                        className="min-h-16 resize-y text-sm [field-sizing:content]"
                      />
                      <div className="flex gap-2">
                        <Button
                          size="sm"
                          className="h-7"
                          onClick={() => void saveEdit(m)}
                        >
                          保存
                        </Button>
                        <Button
                          size="sm"
                          variant="ghost"
                          className="h-7"
                          onClick={() => setEditingId(null)}
                        >
                          取消
                        </Button>
                      </div>
                    </div>
                  ) : (
                    <>
                      <p
                        className={`whitespace-pre-wrap break-words text-sm ${
                          m.enabled
                            ? "text-foreground"
                            : "text-muted-foreground line-through"
                        }`}
                      >
                        {m.content}
                      </p>
                      <div className="mt-1 flex items-center gap-2">
                        <Badge variant="secondary" className="text-[10px]">
                          {m.source === "auto" ? "自动提取" : "手动添加"}
                        </Badge>
                        <span className="text-[10px] text-muted-foreground">
                          {new Date(m.updatedAt * 1000).toLocaleDateString("zh-CN")}
                        </span>
                      </div>
                    </>
                  )}
                </div>
                {editingId !== m.id && (
                  <div className="flex shrink-0 items-center gap-0.5 opacity-0 transition-opacity group-hover:opacity-100">
                    <SimpleTooltip content="编辑">
                      <Button
                        variant="ghost"
                        size="icon"
                        className="size-7"
                        onClick={() => startEdit(m)}
                      >
                        <SquarePen className="size-3.5" />
                      </Button>
                    </SimpleTooltip>
                    <SimpleTooltip content="删除">
                      <Button
                        variant="ghost"
                        size="icon"
                        className="size-7 text-destructive hover:text-destructive"
                        onClick={() => void removeItem(m.id)}
                      >
                        <Trash2 className="size-3.5" />
                      </Button>
                    </SimpleTooltip>
                  </div>
                )}
              </div>
            ))}
          </div>
        </div>
      )}

      {/* 清空全部:二次确认 */}
      <AlertDialog open={clearOpen} onOpenChange={setClearOpen}>
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>清空全部记忆?</AlertDialogTitle>
            <AlertDialogDescription>
              将删除当前账号的全部对话记忆(共 {memories.length} 条),此操作不可恢复。
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel>取消</AlertDialogCancel>
            <AlertDialogAction
              onClick={() => void clearAll()}
              className="bg-destructive text-white hover:bg-destructive/90"
            >
              清空
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
    </div>
  );
}
