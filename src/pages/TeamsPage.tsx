import { useEffect, useMemo, useState, type FormEvent } from "react";
import { format } from "date-fns";
import {
  MoreVertical,
  SquarePen,
  Plus,
  Search,
  Trash2,
  UsersRound,
  UserPlus,
  X,
} from "lucide-react";
import { toast } from "sonner";
import {
  api,
  type TeamInput,
  type TeamView,
  type UserView,
} from "@/lib/api";
import { FORM_CONTROL_SIZING } from "@/lib/form-sizing";
import { ErrorBanner } from "@/components/ErrorBanner";
import { EmptyState } from "@/components/EmptyState";
import { FieldError } from "@/components/FieldError";
import { CodeField, generateCode } from "@/components/CodeField";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Textarea } from "@/components/ui/textarea";
import { Checkbox } from "@/components/ui/checkbox";
import {
  Card,
} from "@/components/ui/card";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import {
  Sheet,
  SheetContent,
  SheetDescription,
  SheetFooter,
  SheetHeader,
  SheetTitle,
} from "@/components/ui/sheet";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
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

// 团队管理(系统管理):接真实后端 team 表 list/upsert/remove 命令。
// 成员从系统用户(用户管理)中多选,memberIds 关联 users.id;用户被删除后悬空 id 自动过滤。

type TeamItem = TeamView;

// 搜索:匹配团队名称 / 编码 / 备注
function teamMatches(t: TeamItem, keyword: string): boolean {
  return `${t.name} ${t.code} ${t.remark}`
    .toLowerCase()
    .includes(keyword.toLowerCase());
}

// 成员显示名:昵称优先,退用户名;都不认识(用户已删)返回空串由调用方过滤
function memberLabel(u: UserView | undefined): string {
  if (!u) return "";
  return u.nickname || u.username;
}

// 创建 / 更新时间统一「年-月-日 时:分:秒」
function fmtDateTime(ts: number): string {
  return format(new Date(ts * 1000), "yyyy-MM-dd HH:mm:ss");
}

export function TeamsPage({ currentUser }: { currentUser: string }) {
  const [teams, setTeams] = useState<TeamItem[]>([]);
  const [users, setUsers] = useState<UserView[]>([]);
  const [editing, setEditing] = useState<TeamItem | null>(null);
  const [isFormOpen, setIsFormOpen] = useState(false);
  const [deleteTarget, setDeleteTarget] = useState<TeamItem | null>(null);
  // 成员管理弹窗目标(卡片底部「成员管理」入口)
  const [memberTarget, setMemberTarget] = useState<TeamItem | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);

  const userById = useMemo(() => {
    const m = new Map<string, UserView>();
    for (const u of users) m.set(u.id, u);
    return m;
  }, [users]);

  async function loadTeams() {
    setLoading(true);
    try {
      setTeams(await api.listTeams());
    } catch (e) {
      setError(String(e));
    } finally {
      setLoading(false);
    }
  }

  useEffect(() => {
    loadTeams();
    api
      .listUsers()
      .then(setUsers)
      .catch((e) => setError(String(e)));
  }, []);

  async function submit(input: TeamInput) {
    try {
      await api.upsertTeam(input);
      setIsFormOpen(false);
      await loadTeams();
    } catch (e) {
      setError(String(e));
    }
  }

  async function confirmDelete() {
    if (!deleteTarget) return;
    try {
      await api.removeTeam(deleteTarget.id);
      await loadTeams();
    } catch (e) {
      setError(String(e));
    }
    setDeleteTarget(null);
  }

  // 卡片网格的搜索过滤(名称 / 编码 / 备注)
  const [keyword, setKeyword] = useState("");
  const filtered = useMemo(
    () => teams.filter((t) => teamMatches(t, keyword.trim())),
    [teams, keyword],
  );

  return (
    <div className={`flex min-h-0 min-w-0 flex-1 flex-col gap-2.5 ${FORM_CONTROL_SIZING}`}>
      <ErrorBanner message={error} onClose={() => setError(null)} />

      {/* 工具栏:搜索 + 新增 */}
      <div className="flex flex-col gap-3 sm:flex-row sm:items-center sm:justify-between">
        <div className="relative w-full sm:max-w-sm">
          <Search className="pointer-events-none absolute left-3 top-1/2 size-4 -translate-y-1/2 text-muted-foreground" />
          <Input
            placeholder="搜索团队名称 / 编码 / 备注"
            className="pl-9"
            value={keyword}
            onChange={(e) => setKeyword(e.target.value)}
          />
        </div>
        <Button
          className="h-10"
          onClick={() => {
            setEditing(null);
            setIsFormOpen(true);
          }}
        >
          <Plus />
          新增团队
        </Button>
      </div>

      {/* 团队卡片网格 */}
      {loading ? (
        <div className="grid gap-3 sm:grid-cols-2 xl:grid-cols-3">
          {Array.from({ length: 6 }).map((_, i) => (
            <div key={i} className="h-44 animate-pulse rounded-xl border bg-muted/40" />
          ))}
        </div>
      ) : filtered.length === 0 ? (
        <EmptyState
          title={keyword.trim() ? "没有匹配的团队" : "暂无团队"}
          description={
            keyword.trim()
              ? "换个关键词试试"
              : "点击右上角「新增团队」开始组建团队"
          }
        />
      ) : (
        <div className="grid min-w-0 gap-3 overflow-y-auto pb-1 sm:grid-cols-2 xl:grid-cols-3">
          {filtered.map((t) => {
            const labels = t.memberIds
              .map((id) => memberLabel(userById.get(id)))
              .filter(Boolean);
            return (
              <Card key={t.id} className="min-w-0 gap-0 overflow-hidden py-0">
                {/* 主体:首字母标识 + 名称/编码 + 成员徽章 */}
                <div className="space-y-3 p-4">
                  <div className="flex items-center gap-3">
                    <span className="flex size-10 shrink-0 items-center justify-center rounded-lg bg-primary/10 text-sm font-semibold text-primary">
                      {t.name.charAt(0).toUpperCase() || "团"}
                    </span>
                    <div className="min-w-0 flex-1">
                      <div className="truncate font-medium text-foreground">
                        {t.name}
                      </div>
                      <div className="font-mono text-xs text-muted-foreground">
                        {t.code}
                      </div>
                    </div>
                    <span className="shrink-0 rounded bg-muted px-1.5 py-0.5 text-xs text-muted-foreground">
                      {labels.length} 人
                    </span>
                  </div>
                  {labels.length === 0 ? (
                    <div className="text-xs text-muted-foreground">
                      暂无成员
                    </div>
                  ) : (
                    <div className="flex flex-wrap gap-1">
                      {labels.map((label, i) => (
                        <span
                          key={`${label}-${i}`}
                          className="rounded bg-muted px-1.5 py-0.5 text-xs"
                        >
                          {label}
                        </span>
                      ))}
                    </div>
                  )}
                  <div className="text-xs text-muted-foreground">
                    更新:{fmtDateTime(t.updatedAt)}
                  </div>
                </div>
                {/* 底部操作条:成员管理 + 更多(编辑 / 删除) */}
                <div className="flex items-center justify-between border-t bg-muted/30 px-2 py-1.5">
                  <Button
                    variant="ghost"
                    size="sm"
                    className="h-8 cursor-pointer px-2 text-xs"
                    onClick={() => setMemberTarget(t)}
                  >
                    <UsersRound className="size-3.5" />
                    成员管理
                  </Button>
                  <DropdownMenu>
                    <DropdownMenuTrigger asChild>
                      <Button
                        variant="ghost"
                        size="icon-sm"
                        className="text-muted-foreground"
                      >
                        <MoreVertical />
                        <span className="sr-only">更多操作</span>
                      </Button>
                    </DropdownMenuTrigger>
                    <DropdownMenuContent align="end" className="w-32">
                      <DropdownMenuItem
                        onClick={() => {
                          setEditing(t);
                          setIsFormOpen(true);
                        }}
                      >
                        <SquarePen />
                        编辑
                      </DropdownMenuItem>
                      <DropdownMenuSeparator />
                      <DropdownMenuItem
                        className="text-destructive focus:text-destructive"
                        onClick={() => setDeleteTarget(t)}
                      >
                        <Trash2 />
                        删除
                      </DropdownMenuItem>
                    </DropdownMenuContent>
                  </DropdownMenu>
                </div>
              </Card>
            );
          })}
        </div>
      )}

      <Sheet open={isFormOpen} onOpenChange={setIsFormOpen}>
        <TeamFormSheet
          key={isFormOpen ? (editing?.id ?? "new") : "idle"}
          initial={editing}
          currentUser={currentUser}
          users={users}
          onSubmit={submit}
          onCancel={() => setIsFormOpen(false)}
        />
      </Sheet>

      {/* 成员管理弹窗:就地增减成员,保存即生效 */}
      {memberTarget && (
        <MemberManageDialog
          key={memberTarget.id}
          team={memberTarget}
          users={users}
          onClose={() => setMemberTarget(null)}
          onSaved={loadTeams}
        />
      )}

      <AlertDialog
        open={deleteTarget !== null}
        onOpenChange={(open) => !open && setDeleteTarget(null)}
      >
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>删除团队</AlertDialogTitle>
            <AlertDialogDescription>
              将永久删除团队「{deleteTarget?.name}
              」,成员用户本身不受影响,此操作不可恢复。
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel className="cursor-pointer">
              取消
            </AlertDialogCancel>
            <AlertDialogAction
              className="cursor-pointer bg-destructive text-white hover:bg-destructive/90"
              onClick={confirmDelete}
            >
              确认删除
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
    </div>
  );
}

// 新增 / 编辑团队(置于 Sheet 内)
function TeamFormSheet({
  initial,
  currentUser,
  users,
  onSubmit,
  onCancel,
}: {
  initial: TeamItem | null;
  currentUser: string;
  users: UserView[];
  onSubmit: (team: TeamInput) => void;
  onCancel: () => void;
}) {
  const isEdit = initial !== null;
  const [code, setCode] = useState(initial?.code ?? generateCode("TEAM"));
  const [name, setName] = useState(initial?.name ?? "");
  const [memberIds, setMemberIds] = useState<string[]>(
    initial?.memberIds ?? [],
  );
  const [remark, setRemark] = useState(initial?.remark ?? "");
  const [submitted, setSubmitted] = useState(false);
  // 归属:新建关联当前用户,编辑保留原归属
  const owner = initial?.owner ?? currentUser;

  function toggleMember(id: string) {
    setMemberIds((ids) =>
      ids.includes(id) ? ids.filter((x) => x !== id) : [...ids, id],
    );
  }

  function handleSubmit(event: FormEvent) {
    event.preventDefault();
    setSubmitted(true);
    if (!name.trim() || memberIds.length === 0) {
      return;
    }
    onSubmit({
      id: initial?.id ?? crypto.randomUUID(),
      code,
      name: name.trim(),
      memberIds,
      remark: remark.trim(),
      owner,
    });
  }

  return (
    <SheetContent
      className="flex w-full flex-col gap-0 p-0 sm:max-w-[600px]"
      blockClose={
        name !== (initial?.name ?? "") ||
        remark !== (initial?.remark ?? "") ||
        memberIds.join(",") !== (initial?.memberIds ?? []).join(",")
      }
    >
      <SheetHeader className="border-b">
        <SheetTitle>{isEdit ? "编辑团队" : "新增团队"}</SheetTitle>
        <SheetDescription>
          维护团队档案,成员从系统用户(用户管理)中多选。
        </SheetDescription>
      </SheetHeader>
      <form onSubmit={handleSubmit} className="flex min-h-0 flex-1 flex-col">
        <div className="flex-1 space-y-6 overflow-y-auto p-5">
          <div className="space-y-3">
            <div className="space-y-1.5">
              <Label htmlFor="team-name">
                团队名称 <span className="text-destructive">*</span>
              </Label>
              <Input
                id="team-name"
                placeholder="团队名称"
                value={name}
                onChange={(e) => setName(e.target.value)}
                aria-invalid={submitted && !name.trim()}
                autoFocus
              />
              <FieldError
                show={submitted && !name.trim()}
                message="团队名称不可为空"
              />
            </div>
            <div className="space-y-1.5">
              <Label htmlFor="team-code">团队编码</Label>
              <CodeField
                id="team-code"
                value={code}
                onRegenerate={
                  isEdit ? undefined : () => setCode(generateCode("TEAM"))
                }
              />
              <p className="text-xs text-muted-foreground">
                {isEdit
                  ? "创建后不可修改,可复制"
                  : "系统自动生成,可刷新或复制"}
              </p>
            </div>
            <div className="space-y-1.5">
              <Label>
                团队成员 <span className="text-destructive">*</span>
              </Label>
              <div
                className="rounded-md border aria-invalid:border-destructive"
                aria-invalid={submitted && memberIds.length === 0}
              >
                {users.length === 0 ? (
                  <div className="px-3 py-2 text-sm text-muted-foreground">
                    暂无用户,请先到「用户管理」添加
                  </div>
                ) : (
                  <div className="max-h-56 overflow-y-auto p-1">
                    {users.map((u) => {
                      const checked = memberIds.includes(u.id);
                      return (
                        <label
                          key={u.id}
                          className="flex cursor-pointer items-center gap-2 rounded px-2 py-1.5 hover:bg-muted/60"
                        >
                          <Checkbox
                            checked={checked}
                            onCheckedChange={() => toggleMember(u.id)}
                          />
                          <span className="text-sm">
                            {u.nickname || u.username}
                          </span>
                          <span className="text-xs text-muted-foreground">
                            @{u.username}
                          </span>
                        </label>
                      );
                    })}
                  </div>
                )}
              </div>
              <FieldError
                show={submitted && memberIds.length === 0}
                message="请至少选择一名成员"
              />
              {memberIds.length > 0 && (
                <div className="flex flex-wrap gap-1 pt-1">
                  {memberIds.map((id) => {
                    const u = users.find((x) => x.id === id);
                    const label = memberLabel(u);
                    if (!label) return null;
                    return (
                      <span
                        key={id}
                        className="inline-flex items-center gap-1 rounded bg-muted px-1.5 py-0.5 text-xs"
                      >
                        {label}
                        <button
                          type="button"
                          className="cursor-pointer text-muted-foreground hover:text-foreground"
                          onClick={() => toggleMember(id)}
                        >
                          <X className="size-3" />
                        </button>
                      </span>
                    );
                  })}
                </div>
              )}
            </div>
            <div className="space-y-1.5">
              <Label htmlFor="team-remark">备注</Label>
              <Textarea
                id="team-remark"
                placeholder="补充说明(可选)"
                rows={3}
                value={remark}
                onChange={(e) => setRemark(e.target.value)}
              />
            </div>
          </div>
        </div>
        <SheetFooter className="flex-row justify-end gap-2 border-t">
          <Button type="button" variant="outline" onClick={onCancel}>
            取消
          </Button>
          <Button type="submit">保存</Button>
        </SheetFooter>
      </form>
    </SheetContent>
  );
}

// 成员管理弹窗:当前成员可移除,候选用户可添加;至少保留一名成员,保存后整体提交
function MemberManageDialog({
  team,
  users,
  onClose,
  onSaved,
}: {
  team: TeamItem;
  users: UserView[];
  onClose: () => void;
  onSaved: () => Promise<void>;
}) {
  const [memberIds, setMemberIds] = useState<string[]>(team.memberIds);
  const [saving, setSaving] = useState(false);
  const [submitted, setSubmitted] = useState(false);

  const members = memberIds
    .map((id) => users.find((u) => u.id === id))
    .filter((u): u is UserView => Boolean(u));
  const candidates = users.filter((u) => !memberIds.includes(u.id));

  function add(id: string) {
    setMemberIds((ids) => [...ids, id]);
  }
  function remove(id: string) {
    setMemberIds((ids) => ids.filter((x) => x !== id));
  }

  async function save() {
    setSubmitted(true);
    if (memberIds.length === 0) return;
    setSaving(true);
    try {
      await api.upsertTeam({
        id: team.id,
        code: team.code,
        name: team.name,
        memberIds,
        remark: team.remark,
        owner: team.owner,
      });
      toast.success("成员已更新");
      await onSaved();
      onClose();
    } catch (e) {
      toast.error(`保存失败: ${e}`);
    } finally {
      setSaving(false);
    }
  }

  return (
    <Dialog open onOpenChange={(o) => !o && onClose()}>
      <DialogContent className="sm:max-w-md">
        <DialogHeader>
          <DialogTitle>成员管理 · {team.name}</DialogTitle>
          <DialogDescription>
            调整团队成员,保存后生效;团队至少保留一名成员。
          </DialogDescription>
        </DialogHeader>

        <div className="space-y-3">
          <div className="space-y-1.5">
            <div className="text-xs font-medium text-muted-foreground">
              当前成员({members.length})
            </div>
            <div className="max-h-44 overflow-y-auto rounded-md border p-1">
              {members.length === 0 ? (
                <div className="px-2 py-3 text-center text-xs text-muted-foreground">
                  暂无成员,请从下方添加
                </div>
              ) : (
                members.map((u) => (
                  <div
                    key={u.id}
                    className="flex items-center gap-2 rounded px-2 py-1.5"
                  >
                    <span className="text-sm">{memberLabel(u)}</span>
                    <span className="text-xs text-muted-foreground">
                      @{u.username}
                    </span>
                    <Button
                      type="button"
                      variant="ghost"
                      size="icon-xs"
                      className="ml-auto cursor-pointer text-muted-foreground hover:text-destructive"
                      title="移出团队"
                      onClick={() => remove(u.id)}
                    >
                      <X className="size-3.5" />
                    </Button>
                  </div>
                ))
              )}
            </div>
            <FieldError
              show={submitted && memberIds.length === 0}
              message="团队至少保留一名成员"
            />
          </div>

          <div className="space-y-1.5">
            <div className="text-xs font-medium text-muted-foreground">
              添加成员({candidates.length})
            </div>
            <div className="max-h-44 overflow-y-auto rounded-md border p-1">
              {candidates.length === 0 ? (
                <div className="px-2 py-3 text-center text-xs text-muted-foreground">
                  所有用户都已在团队中
                </div>
              ) : (
                candidates.map((u) => (
                  <div
                    key={u.id}
                    className="flex items-center gap-2 rounded px-2 py-1.5 hover:bg-muted/60"
                  >
                    <span className="text-sm">{memberLabel(u)}</span>
                    <span className="text-xs text-muted-foreground">
                      @{u.username}
                    </span>
                    <Button
                      type="button"
                      variant="ghost"
                      size="icon-xs"
                      className="ml-auto cursor-pointer text-muted-foreground hover:text-primary"
                      title="加入团队"
                      onClick={() => add(u.id)}
                    >
                      <UserPlus className="size-3.5" />
                    </Button>
                  </div>
                ))
              )}
            </div>
          </div>
        </div>

        <DialogFooter className="flex-row justify-end gap-2">
          <Button
            type="button"
            variant="outline"
            className="cursor-pointer"
            disabled={saving}
            onClick={onClose}
          >
            取消
          </Button>
          <Button
            type="button"
            className="cursor-pointer"
            disabled={saving}
            onClick={save}
          >
            {saving ? "保存中…" : "保存"}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
