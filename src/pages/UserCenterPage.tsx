import { useEffect, useState, type FormEvent, type ReactNode } from "react";
import { KeyRound, ShieldCheck, UserRound } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { FieldError } from "@/components/FieldError";
import { toast } from "sonner";
import { api, type UserView } from "@/lib/api";
import { formatTimestamp } from "@/lib/utils";

// 个人中心分区:左导航切换,布局与系统设置一致(左导航 + 右内容)
type Section = "profile" | "password";

const NAV: { key: Section; label: string; icon: typeof UserRound }[] = [
  { key: "profile", label: "个人资料", icon: UserRound },
  { key: "password", label: "修改密码", icon: KeyRound },
];

const SCOPE_LABEL: Record<string, string> = {
  all: "全部数据",
  self: "仅本人数据",
};

const STATUS_LABEL: Record<string, string> = {
  enabled: "正常",
  disabled: "已禁用",
};

// 新密码最小长度,与后端 change_password 校验保持一致
const MIN_PASSWORD_LENGTH = 6;

// Tauri invoke 失败时 reject 的是错误字符串,这里统一兜底成可读文案
function errMsg(e: unknown): string {
  if (typeof e === "string") return e;
  if (e instanceof Error) return e.message;
  return "操作失败,请重试";
}

export function UserCenterPage({
  currentUser,
  onProfileUpdated,
}: {
  currentUser: UserView;
  onProfileUpdated: (user: UserView) => void;
}) {
  const [section, setSection] = useState<Section>("profile");

  return (
    <div className="flex min-h-0 flex-1 gap-4">
      {/* 左侧:分类菜单(与系统设置同一套视觉) */}
      <div className="flex w-40 shrink-0 flex-col gap-1 rounded-xl border bg-card p-2 lg:w-48">
        {NAV.map((item) => {
          const Icon = item.icon;
          const isActive = section === item.key;
          return (
            <button
              key={item.key}
              type="button"
              onClick={() => setSection(item.key)}
              className={`flex w-full items-center gap-2.5 rounded-md px-3 py-2 text-left text-sm transition-colors ${
                isActive
                  ? "bg-accent font-medium text-accent-foreground"
                  : "text-muted-foreground hover:bg-accent/50 hover:text-foreground"
              }`}
            >
              <Icon className="size-4 shrink-0" />
              {item.label}
            </button>
          );
        })}
      </div>

      {/* 右侧:内容区 */}
      <div className="flex min-h-0 min-w-0 flex-1 flex-col overflow-hidden rounded-xl border bg-card">
        {section === "profile" ? (
          <ProfilePanel
            user={currentUser}
            onProfileUpdated={onProfileUpdated}
          />
        ) : (
          <PasswordPanel />
        )}
      </div>
    </div>
  );
}

// 个人资料:用户名/数据范围/状态/创建时间只读,昵称/邮箱/头像/备注可编辑
function ProfilePanel({
  user,
  onProfileUpdated,
}: {
  user: UserView;
  onProfileUpdated: (user: UserView) => void;
}) {
  const [nickname, setNickname] = useState(user.nickname);
  const [email, setEmail] = useState(user.email);
  const [avatar, setAvatar] = useState(user.avatar);
  const [remark, setRemark] = useState(user.remark);
  const [saving, setSaving] = useState(false);

  // user 变化(上次保存返回最新值)时同步表单
  useEffect(() => {
    setNickname(user.nickname);
    setEmail(user.email);
    setAvatar(user.avatar);
    setRemark(user.remark);
  }, [user]);

  const isDirty =
    nickname !== user.nickname ||
    email !== user.email ||
    avatar !== user.avatar ||
    remark !== user.remark;

  async function handleSave() {
    if (saving || !isDirty) return;
    setSaving(true);
    try {
      const updated = await api.updateProfile(nickname, email, avatar, remark);
      onProfileUpdated(updated);
      toast.success("个人资料已更新");
    } catch (e) {
      toast.error(errMsg(e));
    } finally {
      setSaving(false);
    }
  }

  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <div className="flex-1 space-y-5 overflow-y-auto p-6">
        <div className="flex items-center gap-3">
          {avatar ? (
            <img
              src={avatar}
              alt={user.username}
              className="size-12 shrink-0 rounded-lg object-cover"
            />
          ) : (
            <span className="flex size-12 shrink-0 items-center justify-center rounded-lg bg-primary/10 text-lg font-medium text-primary">
              {user.username.charAt(0).toUpperCase()}
            </span>
          )}
          <div className="min-w-0">
            <div className="flex items-center gap-1.5">
              <span className="truncate text-base font-medium">
                {user.nickname || user.username}
              </span>
              {user.isSuperAdmin && (
                <span className="inline-flex items-center gap-1 rounded bg-amber-500/10 px-1.5 py-0.5 text-xs font-medium text-amber-600 dark:text-amber-400">
                  <ShieldCheck className="size-3" />
                  超级管理员
                </span>
              )}
            </div>
            <span className="text-xs text-muted-foreground">
              @{user.username}
            </span>
          </div>
        </div>

        {/* 只读信息:账号身份相关字段不允许本人自助修改 */}
        <div className="max-w-xl rounded-lg border bg-muted/20 px-4 py-2">
          <InfoRow label="用户名" value={user.username} />
          <InfoRow
            label="数据范围"
            value={SCOPE_LABEL[user.dataScope] ?? user.dataScope}
          />
          <InfoRow
            label="账号状态"
            value={STATUS_LABEL[user.status] ?? user.status}
          />
          <InfoRow label="创建时间" value={formatTimestamp(user.createdAt)} />
        </div>

        <div className="max-w-xl space-y-4">
          <div className="space-y-1.5">
            <Label htmlFor="profile-nickname">昵称</Label>
            <Input
              id="profile-nickname"
              value={nickname}
              onChange={(e) => setNickname(e.target.value)}
              placeholder="用于展示的名称"
            />
          </div>
          <div className="space-y-1.5">
            <Label htmlFor="profile-email">邮箱</Label>
            <Input
              id="profile-email"
              type="email"
              value={email}
              onChange={(e) => setEmail(e.target.value)}
              placeholder="example@domain.com"
            />
          </div>
          <div className="space-y-1.5">
            <Label htmlFor="profile-avatar">头像地址</Label>
            <Input
              id="profile-avatar"
              value={avatar}
              onChange={(e) => setAvatar(e.target.value)}
              placeholder="https://…(图片 URL)"
            />
          </div>
          <div className="space-y-1.5">
            <Label htmlFor="profile-remark">备注</Label>
            <Input
              id="profile-remark"
              value={remark}
              onChange={(e) => setRemark(e.target.value)}
              placeholder="可选"
            />
          </div>
        </div>
      </div>

      <div className="flex justify-end border-t px-6 py-4">
        <Button onClick={handleSave} disabled={!isDirty || saving}>
          {saving ? "保存中…" : "保存资料"}
        </Button>
      </div>
    </div>
  );
}

// 修改密码:argon2 校验旧密码由后端 change_password 完成
function PasswordPanel() {
  const [oldPwd, setOldPwd] = useState("");
  const [newPwd, setNewPwd] = useState("");
  const [confirm, setConfirm] = useState("");
  const [submitted, setSubmitted] = useState(false);
  const [saving, setSaving] = useState(false);

  const mismatch = confirm.length > 0 && newPwd !== confirm;
  const tooShort = newPwd.length > 0 && newPwd.length < MIN_PASSWORD_LENGTH;

  async function handleSubmit(event: FormEvent) {
    event.preventDefault();
    if (saving) return;
    setSubmitted(true);
    // 必填为空 / 太短 / 两次不一致时拦在前端,不发请求
    if (
      !oldPwd ||
      !newPwd ||
      !confirm ||
      newPwd !== confirm ||
      newPwd.length < MIN_PASSWORD_LENGTH
    ) {
      return;
    }
    setSaving(true);
    try {
      await api.changePassword(oldPwd, newPwd);
      toast.success("密码已修改,下次登录生效");
      setOldPwd("");
      setNewPwd("");
      setConfirm("");
      setSubmitted(false);
    } catch (e) {
      toast.error(errMsg(e));
    } finally {
      setSaving(false);
    }
  }

  return (
    <form onSubmit={handleSubmit} className="flex min-h-0 flex-1 flex-col">
      <div className="flex-1 space-y-4 overflow-y-auto p-6">
        <div className="max-w-md space-y-1.5">
          <Label htmlFor="uc-old-pwd">
            当前密码 <span className="text-destructive">*</span>
          </Label>
          <Input
            id="uc-old-pwd"
            type="password"
            value={oldPwd}
            onChange={(e) => setOldPwd(e.target.value)}
            aria-invalid={submitted && !oldPwd}
            autoComplete="current-password"
          />
          <FieldError show={submitted && !oldPwd} message="当前密码不可为空" />
        </div>
        <div className="max-w-md space-y-1.5">
          <Label htmlFor="uc-new-pwd">
            新密码 <span className="text-destructive">*</span>
          </Label>
          <Input
            id="uc-new-pwd"
            type="password"
            value={newPwd}
            onChange={(e) => setNewPwd(e.target.value)}
            aria-invalid={(submitted && !newPwd) || tooShort}
            autoComplete="new-password"
          />
          <FieldError show={submitted && !newPwd} message="新密码不可为空" />
          <FieldError
            show={tooShort}
            message={`新密码至少 ${MIN_PASSWORD_LENGTH} 位`}
          />
        </div>
        <div className="max-w-md space-y-1.5">
          <Label htmlFor="uc-confirm-pwd">
            确认新密码 <span className="text-destructive">*</span>
          </Label>
          <Input
            id="uc-confirm-pwd"
            type="password"
            value={confirm}
            onChange={(e) => setConfirm(e.target.value)}
            aria-invalid={(submitted && !confirm) || mismatch}
            autoComplete="new-password"
          />
          <FieldError show={submitted && !confirm} message="请再次输入新密码" />
          <FieldError show={mismatch} message="两次输入的新密码不一致" />
        </div>
      </div>
      <div className="flex justify-end border-t px-6 py-4">
        <Button type="submit" disabled={saving}>
          {saving ? "提交中…" : "确认修改"}
        </Button>
      </div>
    </form>
  );
}

function InfoRow({ label, value }: { label: string; value: ReactNode }) {
  return (
    <div className="flex items-center justify-between gap-4 border-b py-2 text-sm last:border-b-0">
      <span className="shrink-0 text-muted-foreground">{label}</span>
      <span className="truncate font-medium">{value}</span>
    </div>
  );
}
