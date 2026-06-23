// 记忆管理:对话工作区下的整页模块。
// 全局记忆(控制台式:数字条 + 工具栏 + 列表)/ 会话记忆(左右双栏 master-detail)两个标签。
import { useEffect, useMemo, useState } from "react";
import {
  Brain,
  Loader2,
  MessageSquare,
  Pin,
  Plus,
  Search,
  Sparkles,
  SquarePen,
  Trash2,
} from "lucide-react";
import { toast } from "sonner";

import {
  api,
  type ChatMemoryView,
  type ConversationView,
  type EmbeddingConfigView,
} from "@/lib/api";
import { useChat } from "@/hooks/use-chat";
import { EmptyState } from "@/components/EmptyState";
import { SimpleTooltip } from "@/components/SimpleTooltip";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import { Input } from "@/components/ui/input";
import { Switch } from "@/components/ui/switch";
import { Textarea } from "@/components/ui/textarea";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
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
import { cn } from "@/lib/utils";

// 记忆分类的中文标签(与后端 MEM_TYPES 对应)
const MEMORY_TYPE_LABELS: Record<string, string> = {
  identity: "身份",
  preference: "偏好",
  project: "项目",
  relationship: "人际",
  habit: "习惯",
  other: "其它",
};

export function MemoryCenterPage() {
  return (
    <div className="flex min-h-0 min-w-0 flex-1 flex-col overflow-hidden p-0.5">
      <MemorySection />
    </div>
  );
}

// 记忆分区:全局记忆 / 会话记忆 两个标签
function MemorySection() {
  return (
    <Tabs defaultValue="global" className="flex min-h-0 flex-1 flex-col gap-3">
      <TabsList className="shrink-0 self-start">
        <TabsTrigger value="global">全局记忆</TabsTrigger>
        <TabsTrigger value="conversation">会话记忆</TabsTrigger>
      </TabsList>
      <TabsContent value="global" className="min-h-0 flex-1 overflow-auto">
        <GlobalMemorySection />
      </TabsContent>
      <TabsContent value="conversation" className="min-h-0 flex-1">
        <ConversationMemorySection />
      </TabsContent>
    </Tabs>
  );
}

type SourceFilter = "all" | "auto" | "manual";
type StatusFilter = "all" | "enabled" | "disabled";

// 语义检索默认厂商:Qwen text-embedding-v4(OpenAI 兼容 /embeddings,中文效果好)
const EMBED_DEFAULT_URL = "https://dashscope.aliyuncs.com/compatible-mode/v1";
const EMBED_DEFAULT_MODEL = "text-embedding-v4";

// 顶部数字条单元:大数字 + 小标签
function MetricPill({ label, value }: { label: string; value: number }) {
  return (
    <div className="flex min-w-[68px] flex-col rounded-lg border bg-card px-3 py-1.5">
      <span className="text-lg font-semibold leading-tight text-foreground">
        {value}
      </span>
      <span className="text-[11px] text-muted-foreground">{label}</span>
    </div>
  );
}

// 全局记忆(控制台式):数字条 + 工具栏(搜索/筛选/添加弹出)+ 批量 + 列表
function GlobalMemorySection() {
  const [memories, setMemories] = useState<ChatMemoryView[]>([]);
  const [enabled, setEnabled] = useState(true);
  const [loading, setLoading] = useState(true);
  const [search, setSearch] = useState("");
  const [sourceFilter, setSourceFilter] = useState<SourceFilter>("all");
  const [statusFilter, setStatusFilter] = useState<StatusFilter>("all");
  const [typeFilter, setTypeFilter] = useState<string>("all");
  const [selected, setSelected] = useState<Set<number>>(new Set());
  // 添加用行内展开的撰写区(不常占位),点「添加」展开
  const [composing, setComposing] = useState(false);
  const [newContent, setNewContent] = useState("");
  const [adding, setAdding] = useState(false);
  const [editingId, setEditingId] = useState<number | null>(null);
  const [editDraft, setEditDraft] = useState("");
  // 危险操作二次确认(清空 / 批量删除共用)
  const [confirm, setConfirm] = useState<
    null | { title: string; desc: string; run: () => void | Promise<void> }
  >(null);
  // 语义检索(embedding)配置
  const [embedCfg, setEmbedCfg] = useState<EmbeddingConfigView | null>(null);
  const [embedOpen, setEmbedOpen] = useState(false);
  const [embedUrl, setEmbedUrl] = useState(EMBED_DEFAULT_URL);
  const [embedModel, setEmbedModel] = useState(EMBED_DEFAULT_MODEL);
  const [embedKey, setEmbedKey] = useState("");
  const [savingEmbed, setSavingEmbed] = useState(false);

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
    api
      .getEmbeddingConfig()
      .then((cfg) => {
        setEmbedCfg(cfg);
        if (cfg.apiUrl) setEmbedUrl(cfg.apiUrl);
        if (cfg.model) setEmbedModel(cfg.model);
      })
      .catch(() => {});
    reload();
  }, []);

  // 语义检索是否已就绪:三要素齐全(apiKey 已存或本次填了)
  const embedReady = !!embedCfg?.hasApiKey;

  async function saveEmbed() {
    const url = embedUrl.trim();
    const model = embedModel.trim();
    if (!url || !model) {
      toast.error("请填写 Base URL 与模型名");
      return;
    }
    if (!embedCfg?.hasApiKey && !embedKey.trim()) {
      toast.error("请填写 API Key");
      return;
    }
    setSavingEmbed(true);
    try {
      await api.setEmbeddingConfig(url, model, embedKey.trim());
      setEmbedKey("");
      const cfg = await api.getEmbeddingConfig();
      setEmbedCfg(cfg);
      setEmbedOpen(false);
      toast.success("已保存,新对话将按语义检索注入记忆");
    } catch (e) {
      toast.error(`保存失败: ${e}`);
    } finally {
      setSavingEmbed(false);
    }
  }

  async function togglePin(m: ChatMemoryView) {
    const next = !m.pinned;
    setMemories((prev) =>
      prev.map((x) => (x.id === m.id ? { ...x, pinned: next } : x)),
    );
    try {
      await api.setChatMemoryPinned(m.id, next);
    } catch (e) {
      setMemories((prev) =>
        prev.map((x) => (x.id === m.id ? { ...x, pinned: m.pinned } : x)),
      );
      toast.error(`置顶失败: ${e}`);
    }
  }

  const filtered = useMemo(() => {
    const q = search.trim().toLowerCase();
    return memories.filter((m) => {
      if (q && !m.content.toLowerCase().includes(q)) return false;
      if (sourceFilter !== "all" && m.source !== sourceFilter) return false;
      if (typeFilter !== "all" && m.memType !== typeFilter) return false;
      if (statusFilter === "enabled" && !m.enabled) return false;
      if (statusFilter === "disabled" && m.enabled) return false;
      return true;
    });
  }, [memories, search, sourceFilter, typeFilter, statusFilter]);

  const stats = useMemo(() => {
    const total = memories.length;
    const enabledCount = memories.filter((m) => m.enabled).length;
    const autoCount = memories.filter((m) => m.source === "auto").length;
    return { total, enabledCount, autoCount, manualCount: total - autoCount };
  }, [memories]);

  // 选中只对当前筛选可见项生效,避免「隐藏却被批量操作」
  const filteredIds = useMemo(
    () => new Set(filtered.map((m) => m.id)),
    [filtered],
  );
  const visibleSelected = useMemo(
    () => [...selected].filter((id) => filteredIds.has(id)),
    [selected, filteredIds],
  );
  const allVisibleSelected =
    filtered.length > 0 && visibleSelected.length === filtered.length;

  function toggleSelect(id: number) {
    setSelected((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  }
  function toggleSelectAll() {
    setSelected(allVisibleSelected ? new Set() : new Set(filtered.map((m) => m.id)));
  }

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

  async function toggleItem(m: ChatMemoryView) {
    const next = !m.enabled;
    setMemories((prev) =>
      prev.map((x) => (x.id === m.id ? { ...x, enabled: next } : x)),
    );
    try {
      await api.updateChatMemory(m.id, m.content, next);
    } catch (e) {
      setMemories((prev) =>
        prev.map((x) => (x.id === m.id ? { ...x, enabled: m.enabled } : x)),
      );
      toast.error(`更新失败: ${e}`);
    }
  }

  async function saveEdit(m: ChatMemoryView) {
    const text = editDraft.trim();
    if (!text) {
      toast.error("记忆内容不能为空");
      return;
    }
    try {
      await api.updateChatMemory(m.id, text, m.enabled);
      setMemories((prev) =>
        prev.map((x) => (x.id === m.id ? { ...x, content: text } : x)),
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
      setSelected((prev) => {
        const next = new Set(prev);
        next.delete(id);
        return next;
      });
    } catch (e) {
      toast.error(`删除失败: ${e}`);
    }
  }

  // 批量操作:后端无批量接口,客户端并发逐条调用现有命令
  async function bulkSetEnabled(value: boolean) {
    const targets = memories.filter((m) => visibleSelected.includes(m.id));
    try {
      await Promise.all(
        targets.map((m) => api.updateChatMemory(m.id, m.content, value)),
      );
      setSelected(new Set());
      reload();
    } catch (e) {
      toast.error(`批量操作失败: ${e}`);
    }
  }
  async function bulkDelete() {
    const ids = [...visibleSelected];
    try {
      await Promise.all(ids.map((id) => api.deleteChatMemory(id)));
      setSelected(new Set());
      reload();
      toast.success(`已删除 ${ids.length} 条`);
    } catch (e) {
      toast.error(`批量删除失败: ${e}`);
    }
  }
  async function clearAll() {
    try {
      await api.clearChatMemories();
      setMemories([]);
      setSelected(new Set());
      toast.success("已清空全部记忆");
    } catch (e) {
      toast.error(`清空失败: ${e}`);
    }
  }

  return (
    <div className="space-y-3">
      {/* 数字条 + 总开关 */}
      <div className="flex flex-wrap items-center gap-2">
        <MetricPill label="总记忆" value={stats.total} />
        <MetricPill label="启用" value={stats.enabledCount} />
        <MetricPill label="自动" value={stats.autoCount} />
        <MetricPill label="手动" value={stats.manualCount} />
        <Button
          variant={embedOpen ? "secondary" : "outline"}
          className="ml-auto"
          onClick={() => setEmbedOpen((v) => !v)}
        >
          <Sparkles className="size-4" />
          语义检索
          <span
            className={cn(
              "ml-1 size-1.5 rounded-full",
              embedReady ? "bg-emerald-500" : "bg-muted-foreground/40",
            )}
          />
        </Button>
        <div className="flex items-center gap-2 rounded-lg border bg-card px-3 py-2">
          <span className="text-xs text-muted-foreground">对话记忆</span>
          <Switch checked={enabled} onCheckedChange={toggleEnabled} />
        </div>
      </div>

      {/* 语义检索配置(点「语义检索」展开):配齐后记忆按当前问题相关度注入,而非一股脑全塞 */}
      {embedOpen && (
        <div className="space-y-2 rounded-lg border bg-muted/20 p-3">
          <p className="text-xs text-muted-foreground">
            配置 embedding 厂商后,每轮对话只注入与当前问题最相关的记忆(RAG)。推荐 Qwen
            text-embedding-v4(OpenAI 兼容)。未配置时回退为「最近更新优先」。
          </p>
          <div className="flex flex-wrap gap-2">
            <Input
              value={embedUrl}
              onChange={(e) => setEmbedUrl(e.target.value)}
              placeholder="Base URL"
              className="min-w-[260px] flex-1"
            />
            <Input
              value={embedModel}
              onChange={(e) => setEmbedModel(e.target.value)}
              placeholder="模型名,如 text-embedding-v4"
              className="min-w-[180px] flex-1"
            />
          </div>
          <div className="flex flex-wrap gap-2">
            <Input
              type="password"
              value={embedKey}
              onChange={(e) => setEmbedKey(e.target.value)}
              placeholder={
                embedCfg?.hasApiKey ? "API Key(已配置,留空不改)" : "API Key"
              }
              className="min-w-[260px] flex-1"
            />
            <Button
              className="shrink-0"
              onClick={() => void saveEmbed()}
              disabled={savingEmbed}
            >
              {savingEmbed ? <Loader2 className="size-4 animate-spin" /> : null}
              保存
            </Button>
          </div>
        </div>
      )}

      {/* 工具栏:搜索 + 筛选 + 添加 */}
      <div className="flex flex-wrap items-center gap-2">
        <div className="relative min-w-[180px] flex-1">
          <Search className="absolute left-2.5 top-1/2 size-4 -translate-y-1/2 text-muted-foreground" />
          <Input
            value={search}
            onChange={(e) => setSearch(e.target.value)}
            placeholder="搜索记忆内容"
            className="pl-8"
          />
        </div>
        <Select
          value={sourceFilter}
          onValueChange={(v) => setSourceFilter(v as SourceFilter)}
        >
          <SelectTrigger className="w-28">
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            <SelectItem value="all">全部来源</SelectItem>
            <SelectItem value="auto">自动提取</SelectItem>
            <SelectItem value="manual">手动添加</SelectItem>
          </SelectContent>
        </Select>
        <Select value={typeFilter} onValueChange={setTypeFilter}>
          <SelectTrigger className="w-28">
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            <SelectItem value="all">全部分类</SelectItem>
            <SelectItem value="identity">身份</SelectItem>
            <SelectItem value="preference">偏好</SelectItem>
            <SelectItem value="project">项目</SelectItem>
            <SelectItem value="relationship">人际</SelectItem>
            <SelectItem value="habit">习惯</SelectItem>
            <SelectItem value="other">其它</SelectItem>
          </SelectContent>
        </Select>
        <Select
          value={statusFilter}
          onValueChange={(v) => setStatusFilter(v as StatusFilter)}
        >
          <SelectTrigger className="w-28">
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            <SelectItem value="all">全部状态</SelectItem>
            <SelectItem value="enabled">已启用</SelectItem>
            <SelectItem value="disabled">已停用</SelectItem>
          </SelectContent>
        </Select>
        <Button
          variant={composing ? "secondary" : "default"}
          onClick={() => setComposing((v) => !v)}
        >
          <Plus className="size-4" />
          添加
        </Button>
      </div>

      {/* 行内撰写区(点「添加」展开) */}
      {composing && (
        <div className="flex gap-2 rounded-lg border bg-muted/20 p-2">
          <Input
            autoFocus
            value={newContent}
            onChange={(e) => setNewContent(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter") {
                e.preventDefault();
                void addMemory();
              }
            }}
            placeholder="输入一条要长期记住的信息,如:我是一名前端工程师"
          />
          <Button
            className="shrink-0"
            onClick={() => void addMemory()}
            disabled={adding || !newContent.trim()}
          >
            {adding ? <Loader2 className="size-4 animate-spin" /> : null}
            保存
          </Button>
          <Button
            variant="ghost"
            className="shrink-0"
            onClick={() => {
              setComposing(false);
              setNewContent("");
            }}
          >
            取消
          </Button>
        </div>
      )}

      {/* 全选 + 批量操作 */}
      {filtered.length > 0 && (
        <div className="flex items-center justify-between gap-2 px-0.5 text-xs">
          <label className="flex cursor-pointer items-center gap-2">
            <Checkbox
              checked={allVisibleSelected}
              onCheckedChange={toggleSelectAll}
            />
            <span className="text-muted-foreground">
              {visibleSelected.length > 0
                ? `已选 ${visibleSelected.length} 条`
                : `共 ${filtered.length} 条`}
            </span>
          </label>
          <div className="flex items-center gap-1">
            {visibleSelected.length > 0 && (
              <>
                <Button
                  variant="ghost"
                  size="sm"
                  className="h-7"
                  onClick={() => void bulkSetEnabled(true)}
                >
                  启用
                </Button>
                <Button
                  variant="ghost"
                  size="sm"
                  className="h-7"
                  onClick={() => void bulkSetEnabled(false)}
                >
                  停用
                </Button>
                <Button
                  variant="ghost"
                  size="sm"
                  className="h-7 text-destructive hover:text-destructive"
                  onClick={() =>
                    setConfirm({
                      title: `删除选中的 ${visibleSelected.length} 条记忆?`,
                      desc: "此操作不可恢复。",
                      run: bulkDelete,
                    })
                  }
                >
                  删除
                </Button>
              </>
            )}
            <Button
              variant="ghost"
              size="sm"
              className="h-7 text-destructive hover:text-destructive"
              onClick={() =>
                setConfirm({
                  title: "清空全部记忆?",
                  desc: `将删除当前账号的全部 ${stats.total} 条记忆,不可恢复。`,
                  run: clearAll,
                })
              }
            >
              清空全部
            </Button>
          </div>
        </div>
      )}

      {/* 列表 */}
      {loading ? (
        <div className="flex items-center justify-center py-12 text-sm text-muted-foreground">
          <Loader2 className="mr-2 size-4 animate-spin" />
          加载中…
        </div>
      ) : filtered.length === 0 ? (
        <EmptyState
          icon={Brain}
          title={memories.length === 0 ? "暂无记忆" : "没有匹配的记忆"}
          description={
            memories.length === 0
              ? "开启后 AI 会在对话中自动积累,你也可以点「添加」手动写入。"
              : "调整搜索或筛选条件试试。"
          }
        />
      ) : (
        <div className="space-y-1.5">
          {filtered.map((m) => (
            <div
              key={m.id}
              className="group flex items-start gap-2.5 rounded-lg border bg-card px-3 py-2.5"
            >
              <Checkbox
                className="mt-1 shrink-0"
                checked={selected.has(m.id)}
                onCheckedChange={() => toggleSelect(m.id)}
              />
              {/* 状态点:绿=启用 / 空心=停用,点击切换 */}
              <SimpleTooltip content={m.enabled ? "已启用 · 点击停用" : "已停用 · 点击启用"}>
                <button
                  type="button"
                  onClick={() => void toggleItem(m)}
                  className={cn(
                    "mt-1.5 size-2.5 shrink-0 rounded-full transition-colors",
                    m.enabled
                      ? "bg-primary"
                      : "border border-muted-foreground/40 bg-transparent hover:border-primary",
                  )}
                />
              </SimpleTooltip>
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
                      className={cn(
                        "whitespace-pre-wrap break-words text-sm",
                        m.enabled ? "text-foreground" : "text-muted-foreground",
                      )}
                    >
                      {m.content}
                    </p>
                    <div className="mt-1 flex flex-wrap items-center gap-1.5 text-[11px] text-muted-foreground">
                      {m.pinned && (
                        <>
                          <span className="font-medium text-primary">置顶</span>
                          <span>·</span>
                        </>
                      )}
                      <span className="rounded bg-muted px-1.5 py-0.5 text-foreground/70">
                        {MEMORY_TYPE_LABELS[m.memType] ?? m.memType}
                      </span>
                      <span>·</span>
                      <span>{m.source === "auto" ? "自动" : "手动"}</span>
                      <span>·</span>
                      <SimpleTooltip content={`重要度 ${m.importance}/5`}>
                        <span className="text-amber-500">
                          {"★".repeat(Math.max(0, Math.min(5, m.importance)))}
                          <span className="text-muted-foreground/40">
                            {"★".repeat(Math.max(0, 5 - m.importance))}
                          </span>
                        </span>
                      </SimpleTooltip>
                      {m.hitCount > 0 && (
                        <>
                          <span>·</span>
                          <span>命中 {m.hitCount}</span>
                        </>
                      )}
                      <span>·</span>
                      <span>
                        {new Date(m.updatedAt * 1000).toLocaleDateString("zh-CN")}
                      </span>
                    </div>
                  </>
                )}
              </div>
              {editingId !== m.id && (
                <div className="flex shrink-0 items-center gap-0.5">
                  {/* 置顶按钮:已置顶恒显示(主色),未置顶悬停显示 */}
                  <SimpleTooltip
                    content={m.pinned ? "已置顶 · 点击取消" : "置顶(每轮恒注入)"}
                  >
                    <Button
                      variant="ghost"
                      size="icon"
                      className={cn(
                        "size-7 transition-opacity",
                        m.pinned
                          ? "text-primary opacity-100"
                          : "opacity-0 group-hover:opacity-100",
                      )}
                      onClick={() => void togglePin(m)}
                    >
                      <Pin className={cn("size-3.5", m.pinned && "fill-current")} />
                    </Button>
                  </SimpleTooltip>
                  <div className="flex items-center gap-0.5 opacity-0 transition-opacity group-hover:opacity-100">
                    <SimpleTooltip content="编辑">
                      <Button
                        variant="ghost"
                        size="icon"
                        className="size-7"
                        onClick={() => {
                          setEditingId(m.id);
                          setEditDraft(m.content);
                        }}
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
                </div>
              )}
            </div>
          ))}
        </div>
      )}

      {/* 危险操作二次确认 */}
      <AlertDialog
        open={!!confirm}
        onOpenChange={(open) => {
          if (!open) setConfirm(null);
        }}
      >
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>{confirm?.title}</AlertDialogTitle>
            <AlertDialogDescription>{confirm?.desc}</AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel>取消</AlertDialogCancel>
            <AlertDialogAction
              className="bg-destructive text-white hover:bg-destructive/90"
              onClick={() => {
                const run = confirm?.run;
                setConfirm(null);
                void run?.();
              }}
            >
              确定
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
    </div>
  );
}

// 会话记忆列表每次滚动加载的条数(虚拟分页)
const SESSION_PAGE_SIZE = 50;

// 会话按最近更新时间分桶(今天/昨天/近 7 天/更早),桶内保持后端的倒序
function groupConversationsByTime(
  conversations: ConversationView[],
): { label: string; items: ConversationView[] }[] {
  const now = new Date();
  const todayStart =
    new Date(now.getFullYear(), now.getMonth(), now.getDate()).getTime() / 1000;
  const yesterdayStart = todayStart - 86400;
  const weekStart = todayStart - 6 * 86400;
  const buckets: Record<string, ConversationView[]> = {
    今天: [],
    昨天: [],
    "近 7 天": [],
    更早: [],
  };
  for (const c of conversations) {
    if (c.updatedAt >= todayStart) buckets["今天"].push(c);
    else if (c.updatedAt >= yesterdayStart) buckets["昨天"].push(c);
    else if (c.updatedAt >= weekStart) buckets["近 7 天"].push(c);
    else buckets["更早"].push(c);
  }
  return ["今天", "昨天", "近 7 天", "更早"]
    .map((label) => ({ label, items: buckets[label] }))
    .filter((g) => g.items.length > 0);
}

// 会话记忆(左右双栏):左选会话,右大编辑区查看/编辑该会话滚动摘要
function ConversationMemorySection() {
  const { conversations, reload } = useChat();
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [search, setSearch] = useState("");
  const [draft, setDraft] = useState("");
  const [loading, setLoading] = useState(false);
  const [saving, setSaving] = useState(false);
  // 滚动虚拟分页:先渲染前 N 条,滚动到底再加载下一批
  const [visibleCount, setVisibleCount] = useState(SESSION_PAGE_SIZE);
  const [deleteTarget, setDeleteTarget] = useState<ConversationView | null>(null);

  const filtered = useMemo(() => {
    const q = search.trim().toLowerCase();
    if (!q) return conversations;
    return conversations.filter((c) => (c.title || "").toLowerCase().includes(q));
  }, [conversations, search]);
  // 搜索变化时重置已加载量
  useEffect(() => {
    setVisibleCount(SESSION_PAGE_SIZE);
  }, [search]);
  const groups = useMemo(
    () => groupConversationsByTime(filtered.slice(0, visibleCount)),
    [filtered, visibleCount],
  );
  const hasMore = filtered.length > visibleCount;
  const selectedTitle =
    conversations.find((c) => c.id === selectedId)?.title || "新对话";

  async function select(id: string) {
    setSelectedId(id);
    setDraft("");
    setLoading(true);
    try {
      setDraft(await api.getConversationSummary(id));
    } catch (e) {
      toast.error(`加载会话记忆失败: ${e}`);
    } finally {
      setLoading(false);
    }
  }

  async function save() {
    if (!selectedId) return;
    setSaving(true);
    try {
      await api.updateConversationSummary(selectedId, draft);
      toast.success("已保存");
    } catch (e) {
      toast.error(`保存失败: ${e}`);
    } finally {
      setSaving(false);
    }
  }

  // 删除会话记忆 = 删除该会话(后端级联删消息 / 摘要 / 附件),再刷新列表
  async function confirmDelete() {
    if (!deleteTarget) return;
    const id = deleteTarget.id;
    try {
      await api.deleteConversation(id);
      if (selectedId === id) {
        setSelectedId(null);
        setDraft("");
      }
      setDeleteTarget(null);
      await reload();
      toast.success("已删除会话及其记忆");
    } catch (e) {
      toast.error(`删除失败: ${e}`);
    }
  }

  if (conversations.length === 0) {
    return (
      <EmptyState
        icon={MessageSquare}
        title="暂无会话"
        description="开始对话后,较长会话会自动生成可在此查看 / 编辑的记忆摘要。"
      />
    );
  }

  return (
    <div className="flex min-h-[60vh] gap-3">
      {/* 左:会话列表 */}
      <div className="flex w-80 shrink-0 flex-col rounded-lg border bg-card">
        <div className="border-b p-2">
          <div className="relative">
            <Search className="absolute left-2.5 top-1/2 size-4 -translate-y-1/2 text-muted-foreground" />
            <Input
              value={search}
              onChange={(e) => setSearch(e.target.value)}
              placeholder="搜索会话"
              className="h-8 pl-8 text-sm"
            />
          </div>
        </div>
        <div
          className="veltrix-thin-scrollbar min-h-0 flex-1 overflow-y-auto p-1.5"
          onScroll={(e) => {
            const el = e.currentTarget;
            if (el.scrollHeight - el.scrollTop - el.clientHeight < 120) {
              setVisibleCount((c) =>
                c < filtered.length ? c + SESSION_PAGE_SIZE : c,
              );
            }
          }}
        >
          {groups.length === 0 ? (
            <div className="py-8 text-center text-xs text-muted-foreground">
              没有匹配的会话
            </div>
          ) : (
            <>
              {groups.map((g) => (
                <div key={g.label} className="mb-1">
                  <div className="px-2 py-1 text-[11px] font-medium text-muted-foreground">
                    {g.label}
                  </div>
                  {g.items.map((c) => (
                    <div
                      key={c.id}
                      className={cn(
                        "group flex items-center gap-1 rounded-md pr-1 transition-colors",
                        selectedId === c.id
                          ? "bg-primary/10"
                          : "hover:bg-accent/50",
                      )}
                    >
                      <button
                        type="button"
                        onClick={() => void select(c.id)}
                        className={cn(
                          "flex min-w-0 flex-1 items-center gap-2 px-2 py-1.5 text-left text-sm",
                          selectedId === c.id
                            ? "font-medium text-primary"
                            : "text-foreground",
                        )}
                      >
                        <MessageSquare className="size-3.5 shrink-0 opacity-70" />
                        <span className="truncate">{c.title || "新对话"}</span>
                      </button>
                      <SimpleTooltip content="删除会话及其记忆">
                        <button
                          type="button"
                          onClick={() => setDeleteTarget(c)}
                          className="shrink-0 rounded p-1 text-muted-foreground opacity-0 transition-opacity hover:bg-destructive/10 hover:text-destructive group-hover:opacity-100"
                        >
                          <Trash2 className="size-3.5" />
                        </button>
                      </SimpleTooltip>
                    </div>
                  ))}
                </div>
              ))}
              {hasMore && (
                <div className="py-2 text-center text-[11px] text-muted-foreground">
                  下滑加载更多…
                </div>
              )}
            </>
          )}
        </div>
      </div>

      {/* 右:摘要编辑区 */}
      <div className="flex min-w-0 flex-1 flex-col rounded-lg border bg-card p-3">
        {selectedId === null ? (
          <div className="flex flex-1 items-center justify-center px-6 text-center text-sm text-muted-foreground">
            从左侧选择一个会话,查看 / 编辑它的记忆摘要
          </div>
        ) : (
          <>
            <div className="mb-2 flex items-center gap-2">
              <MessageSquare className="size-4 shrink-0 text-muted-foreground" />
              <span className="truncate text-sm font-medium">{selectedTitle}</span>
            </div>
            {loading ? (
              <div className="flex flex-1 items-center justify-center text-sm text-muted-foreground">
                <Loader2 className="mr-2 size-4 animate-spin" />
                加载中…
              </div>
            ) : (
              <>
                <Textarea
                  value={draft}
                  onChange={(e) => setDraft(e.target.value)}
                  placeholder="该会话暂无摘要(对话较短时无需)。可手动写入要点,作为后续对话的前情提要。"
                  className="veltrix-thin-scrollbar min-h-0 flex-1 resize-none text-sm"
                />
                <div className="mt-2 flex items-center justify-between">
                  <span className="text-[11px] text-muted-foreground">
                    {draft.length} 字
                  </span>
                  <Button size="sm" onClick={() => void save()} disabled={saving}>
                    {saving && <Loader2 className="size-4 animate-spin" />}
                    保存
                  </Button>
                </div>
              </>
            )}
          </>
        )}
      </div>

      {/* 删除会话记忆确认(级联删消息 / 摘要 / 附件) */}
      <AlertDialog
        open={deleteTarget !== null}
        onOpenChange={(open) => !open && setDeleteTarget(null)}
      >
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>删除会话记忆</AlertDialogTitle>
            <AlertDialogDescription>
              将删除「{deleteTarget?.title || "新对话"}」整个会话(含全部消息与记忆摘要),不可恢复。确定?
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel>取消</AlertDialogCancel>
            <AlertDialogAction
              className="bg-destructive text-white hover:bg-destructive/90"
              onClick={() => void confirmDelete()}
            >
              删除
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
    </div>
  );
}
