import { useState, type FormEvent } from "react";
import {
  Check,
  Copy,
  Database,
  Download,
  Eye,
  EyeOff,
  HardDrive,
  Loader2,
  Radar,
  RefreshCw,
  Server,
} from "lucide-react";
import { api, type UserView } from "@/lib/api";
import { save } from "@tauri-apps/plugin-dialog";
import { generatePassword } from "@/lib/password";
import { Button } from "@/components/ui/button";
import { SimpleTooltip } from "@/components/SimpleTooltip";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { FieldError } from "@/components/FieldError";
import { toast } from "sonner";

// 首次启动初始化向导:无任何用户时引导选择数据库 + 设置超级管理员,完成后自动登录。
// 当前「是否已初始化」用 localStorage 占位标记;接后端后应改为查询 users 表是否为空。

const EMAIL_PATTERN = /^[^\s@]+@[^\s@]+\.[^\s@]+$/;
const MIN_PASSWORD_LENGTH = 6;
const DEFAULT_MAX_CONNECTIONS = 10;
const PG_URL_PATTERN = /^(postgres:\/\/|postgresql:\/\/)/i;
const PG_URL_EXAMPLE = "postgres://veltrix:Passw0rd@127.0.0.1:5432/veltrix_db";

// 初始化页背景渐变:按钮主色 primary 对角过渡到背景色,每次打开随机取一档(类名为完整字面量以便 Tailwind 提取)
const GRADIENTS = [
  "from-primary/24 to-background",
  "from-primary/20 to-background",
  "from-primary/18 to-background",
];

// 解析 PostgreSQL 连接串,拆出主机/端口/数据库/用户名用于确认页展示
function parsePgUrl(
  url: string,
): { host: string; port: string; database: string; user: string } | null {
  try {
    const u = new URL(url);
    return {
      host: u.hostname || "—",
      port: u.port || "5432",
      database: u.pathname.replace(/^\//, "") || "—",
      user: decodeURIComponent(u.username) || "—",
    };
  } catch {
    return null;
  }
}

type DbType = "sqlite" | "postgres";

interface SetupWizardProps {
  onComplete: (user: UserView) => void;
}

export function SetupWizard({ onComplete }: SetupWizardProps) {
  const [gradient] = useState(
    () => GRADIENTS[Math.floor(Math.random() * GRADIENTS.length)],
  );
  const [step, setStep] = useState<1 | 2 | 3>(1);

  // 第一步:数据库
  const [dbType, setDbType] = useState<DbType>("sqlite");
  const [pgUrl, setPgUrl] = useState("");
  const [testing, setTesting] = useState(false);

  // 第二步:超级管理员
  const [username, setUsername] = useState("admin");
  const [nickname, setNickname] = useState("超级管理员");
  const [password, setPassword] = useState(() => generatePassword());
  const [showPassword, setShowPassword] = useState(true);
  const [phone, setPhone] = useState("");
  const [email, setEmail] = useState("");
  const [submitted, setSubmitted] = useState(false);
  const [creating, setCreating] = useState(false);

  const pgInvalid = dbType === "postgres" && !PG_URL_PATTERN.test(pgUrl.trim());
  const pgInfo = dbType === "postgres" ? parsePgUrl(pgUrl) : null;
  // 管理员信息是否填写完整(用于下载 / 提交可用性)
  const isAdminValid =
    username.trim().length > 0 &&
    nickname.trim().length > 0 &&
    password.trim().length >= MIN_PASSWORD_LENGTH &&
    (!email.trim() || EMAIL_PATTERN.test(email));

  async function testConnection() {
    if (pgInvalid) {
      toast.error("连接串应以 postgres:// 开头");
      return;
    }
    setTesting(true);
    try {
      await api.testDatabaseConnection(pgUrl.trim());
      toast.success("连接成功");
    } catch (e) {
      toast.error(`连接失败: ${e}`);
    } finally {
      setTesting(false);
    }
  }

  async function copyPassword() {
    try {
      await navigator.clipboard.writeText(password);
      toast.success("密码已复制");
    } catch {
      toast.error("复制失败,请手动复制");
    }
  }

  // 导出管理员账号信息为 txt(用 Tauri 保存对话框选路径 + 后端写文件,WebView 不支持 blob 下载)
  async function downloadAccountInfo() {
    const content = [
      "VeltrixLoop 配置管理员账号信息",
      "----------------------------------",
      `账号:     ${username}`,
      `昵称:     ${nickname}`,
      `密码:     ${password}`,
      `联系方式: ${phone || "—"}`,
      `邮箱:     ${email || "—"}`,
      `数据库:   ${dbType === "postgres" ? pgUrl || "(未填写连接串)" : "SQLite(本地文件)"}`,
      `生成时间: ${new Date().toLocaleString()}`,
      "",
      "请妥善保管此文件,密码仅在初始化时明文展示一次。",
    ].join("\n");
    try {
      const path = await save({
        defaultPath: "veltrix-admin-account.txt",
        filters: [{ name: "文本文件", extensions: ["txt"] }],
      });
      if (!path) return; // 用户取消
      await api.saveTextFile(path, content);
      toast.success("账号信息已保存");
    } catch (e) {
      toast.error(`保存失败: ${e}`);
    }
  }

  async function goNext() {
    if (dbType === "sqlite") {
      setStep(2);
      return;
    }
    if (pgInvalid) {
      toast.error("请填写正确的 PostgreSQL 连接串");
      return;
    }
    // PostgreSQL 必须连接成功才能进入下一步
    setTesting(true);
    try {
      await api.testDatabaseConnection(pgUrl.trim());
      toast.success("连接成功");
      setStep(2);
    } catch (e) {
      toast.error(`连接失败,无法继续: ${e}`);
    } finally {
      setTesting(false);
    }
  }

  // 第二步:校验管理员信息,通过后进入「配置完成」确认页
  function goStep3(event: FormEvent) {
    event.preventDefault();
    setSubmitted(true);
    if (!isAdminValid) return;
    setStep(3);
  }

  // 第三步:保存数据库配置 + 创建超管 + 自动登录
  async function finish() {
    setCreating(true);
    try {
      // 保存数据库配置(SQLite 用默认本地文件,连接串留空;PG 写入连接串)
      // 注意:选 PG 时配置需重启后才生效,超管会先建到当前(默认 SQLite)库
      const url = dbType === "postgres" ? pgUrl.trim() : "";
      await api.setDatabaseConfig(url, DEFAULT_MAX_CONNECTIONS);
      // 创建超级管理员(密码后端 argon2 哈希),数据级别为全部
      await api.upsertUser({
        id: crypto.randomUUID(),
        username: username.trim(),
        password,
        email: email.trim(),
        nickname: nickname.trim(),
        avatar: "",
        remark: "系统初始化创建",
        status: "enabled",
        dataScope: "all",
      });
      // 自动登录:用刚建的超管换取用户信息
      const user = await api.login(username.trim(), password);
      toast.success("初始化完成,正在进入…");
      onComplete(user);
    } catch (e) {
      toast.error(`初始化失败: ${e}`);
    } finally {
      setCreating(false);
    }
  }

  return (
    <div
      className={`flex min-h-svh items-center justify-center bg-background bg-gradient-to-br p-6 ${gradient} [&_[data-slot=input]]:h-11 [&_[data-slot=button]:not([data-size^=icon])]:h-11`}
    >
      <div className="w-full max-w-xl rounded-2xl border bg-card p-8 shadow-lg">
        {/* 品牌头 */}
        <div className="mb-6 flex items-center justify-center gap-2.5">
          <div className="flex size-9 items-center justify-center rounded-xl bg-gradient-to-br from-indigo-500 to-violet-600 text-white shadow-lg shadow-indigo-500/30">
            <Radar className="size-5" />
          </div>
          <div className="leading-tight">
            <div className="font-semibold">VeltrixLoop</div>
            <div className="text-xs text-muted-foreground">首次启动 · 初始化</div>
          </div>
        </div>

        {/* 步骤指示 */}
        <div className="mb-6 flex items-center gap-2 text-xs font-medium">
          <StepDot index={1} current={step} label="数据库选型" />
          <div className="h-px flex-1 bg-border" />
          <StepDot index={2} current={step} label="配置管理员" />
          <div className="h-px flex-1 bg-border" />
          <StepDot index={3} current={step} label="完成" />
        </div>

        <div className="flex min-h-[460px] flex-col">
        {step === 1 && (
          <div className="flex flex-1 flex-col gap-4 duration-300 animate-in fade-in-50 slide-in-from-right-4">
            <div className="space-y-1.5">
              <Label>选择数据库</Label>
              <div className="space-y-2">
                <DbOption
                  active={dbType === "sqlite"}
                  icon={HardDrive}
                  title="SQLite"
                  desc="本地文件,免配置"
                  onClick={() => setDbType("sqlite")}
                />
                <DbOption
                  active={dbType === "postgres"}
                  icon={Server}
                  title="PostgreSQL"
                  desc="远程 / 共享库"
                  onClick={() => setDbType("postgres")}
                />
              </div>
            </div>

            <div className="space-y-1.5">
              <Label htmlFor="pg-url">连接字符串</Label>
              <div className="flex gap-2">
                <Input
                  id="pg-url"
                  placeholder={
                    dbType === "sqlite"
                      ? "SQLite 使用本地文件,无需连接字符串"
                      : "postgres://user:pass@host:5432/dbname"
                  }
                  value={dbType === "sqlite" ? "" : pgUrl}
                  onChange={(e) => setPgUrl(e.target.value)}
                  disabled={dbType === "sqlite"}
                  aria-invalid={
                    dbType === "postgres" && pgUrl.length > 0 && pgInvalid
                  }
                />
                <Button
                  type="button"
                  variant="outline"
                  className="shrink-0"
                  disabled={dbType === "sqlite" || testing || pgInvalid}
                  onClick={testConnection}
                >
                  {testing ? (
                    <Loader2 className="animate-spin" />
                  ) : (
                    <Check />
                  )}
                  测试
                </Button>
              </div>

              {dbType === "sqlite" ? (
                <div className="flex items-start gap-2 rounded-lg bg-muted/50 px-3 py-2 text-xs text-muted-foreground">
                  <Database className="mt-0.5 size-3.5 shrink-0" />
                  将使用应用数据目录下的本地数据库文件,无需额外配置。
                </div>
              ) : (
                <div className="space-y-2">
                  <div className="space-y-1 rounded-md bg-muted/50 px-3 py-2 text-xs text-muted-foreground">
                    <div>
                      格式:{" "}
                      <code className="font-mono text-foreground">
                        postgres://用户名:密码@主机:端口/数据库名
                      </code>
                    </div>
                    <div className="flex flex-wrap items-center gap-1.5">
                      <span>示例:</span>
                      <code className="font-mono break-all">
                        {PG_URL_EXAMPLE}
                      </code>
                      <button
                        type="button"
                        className="text-primary hover:underline"
                        onClick={() => setPgUrl(PG_URL_EXAMPLE)}
                      >
                        填入
                      </button>
                    </div>
                    <div>
                      含密码的连接字符串建议改用环境变量 VELTRIX_DATABASE_URL
                    </div>
                  </div>
                  {pgInfo && (
                    <div className="grid grid-cols-2 gap-x-3 gap-y-1.5 rounded-md border bg-muted/30 px-3 py-2 text-xs">
                      <PgField label="主机" value={pgInfo.host} />
                      <PgField label="端口" value={pgInfo.port} />
                      <PgField label="数据库" value={pgInfo.database} />
                      <PgField label="用户名" value={pgInfo.user} />
                    </div>
                  )}
                </div>
              )}
            </div>

            <Button
              type="button"
              className="mt-auto w-full"
              disabled={testing}
              onClick={goNext}
            >
              {testing ? (
                <>
                  <Loader2 className="animate-spin" />
                  连接中…
                </>
              ) : (
                "下一步"
              )}
            </Button>
          </div>
        )}

        {step === 2 && (
          <form
            onSubmit={goStep3}
            className="flex flex-1 flex-col gap-4 duration-300 animate-in fade-in-50 slide-in-from-right-4"
          >
            <div className="space-y-1.5">
              <Label htmlFor="setup-username">
                管理员账号 <span className="text-destructive">*</span>
              </Label>
              <Input
                id="setup-username"
                placeholder="登录用户名"
                value={username}
                onChange={(e) => setUsername(e.target.value)}
                aria-invalid={submitted && !username.trim()}
                autoFocus
              />
              <FieldError
                show={submitted && !username.trim()}
                message="账号不可为空"
              />
            </div>

            <div className="space-y-1.5">
              <Label htmlFor="setup-nickname">
                昵称 <span className="text-destructive">*</span>
              </Label>
              <Input
                id="setup-nickname"
                placeholder="显示名称"
                value={nickname}
                onChange={(e) => setNickname(e.target.value)}
                aria-invalid={submitted && !nickname.trim()}
              />
              <FieldError
                show={submitted && !nickname.trim()}
                message="昵称不可为空"
              />
            </div>

            <div className="space-y-1.5">
              <Label htmlFor="setup-password">
                密码 <span className="text-destructive">*</span>
              </Label>
              <div className="flex gap-2">
                <div className="relative flex-1">
                  <Input
                    id="setup-password"
                    type={showPassword ? "text" : "password"}
                    className="pr-9"
                    value={password}
                    onChange={(e) => setPassword(e.target.value)}
                    aria-invalid={
                      submitted && password.trim().length < MIN_PASSWORD_LENGTH
                    }
                  />
                  <button
                    type="button"
                    onClick={() => setShowPassword((s) => !s)}
                    className="absolute right-2 top-1/2 -translate-y-1/2 text-muted-foreground transition-colors hover:text-foreground"
                  >
                    {showPassword ? (
                      <EyeOff className="size-4" />
                    ) : (
                      <Eye className="size-4" />
                    )}
                  </button>
                </div>
                <SimpleTooltip content="重新生成">
                  <Button
                    type="button"
                    variant="outline"
                    size="icon"
                    className="size-11 shrink-0"
                    aria-label="重新生成"
                    onClick={() => {
                      setPassword(generatePassword());
                      setShowPassword(true);
                    }}
                  >
                    <RefreshCw />
                  </Button>
                </SimpleTooltip>
                <SimpleTooltip content="复制">
                  <Button
                    type="button"
                    variant="outline"
                    size="icon"
                    className="size-11 shrink-0"
                    aria-label="复制"
                    onClick={copyPassword}
                  >
                    <Copy />
                  </Button>
                </SimpleTooltip>
              </div>
              <FieldError
                show={submitted && password.trim().length < MIN_PASSWORD_LENGTH}
                message="密码至少 6 位(大小写字母 + 数字)"
              />
            </div>

            <div className="space-y-1.5">
              <Label htmlFor="setup-phone">联系方式</Label>
              <Input
                id="setup-phone"
                inputMode="numeric"
                maxLength={20}
                placeholder="手机号(仅数字,选填)"
                value={phone}
                onChange={(e) =>
                  setPhone(e.target.value.replace(/\D/g, "").slice(0, 20))
                }
              />
            </div>

            <div className="space-y-1.5">
              <Label htmlFor="setup-email">电子邮箱</Label>
              <Input
                id="setup-email"
                type="email"
                placeholder="name@example.com(选填)"
                value={email}
                onChange={(e) => setEmail(e.target.value)}
                aria-invalid={
                  submitted &&
                  email.trim().length > 0 &&
                  !EMAIL_PATTERN.test(email)
                }
              />
              <FieldError
                show={
                  submitted &&
                  email.trim().length > 0 &&
                  !EMAIL_PATTERN.test(email)
                }
                message="邮箱格式不正确"
              />
            </div>

            <div className="mt-auto flex gap-2">
              <Button
                type="button"
                variant="outline"
                className="flex-1"
                onClick={() => setStep(1)}
              >
                上一步
              </Button>
              <Button type="submit" className="flex-1">
                下一步
              </Button>
            </div>
          </form>
        )}

        {step === 3 && (
          <div className="flex flex-1 flex-col gap-4 duration-300 animate-in fade-in-50 slide-in-from-right-4">
            <div className="space-y-2 rounded-lg border bg-muted/30 p-4 text-sm">
              <div className="font-medium text-foreground">
                配置完成,请确认信息
              </div>
              <SummaryRow
                label="数据库"
                value={
                  dbType === "postgres" ? "PostgreSQL" : "SQLite(本地文件)"
                }
              />
              {dbType === "postgres" && (
                <>
                  <SummaryRow label="连接串" value={pgUrl} />
                  {pgInfo && (
                    <>
                      <SummaryRow label="主机" value={pgInfo.host} />
                      <SummaryRow label="端口" value={pgInfo.port} />
                      <SummaryRow label="数据库" value={pgInfo.database} />
                      <SummaryRow label="用户名" value={pgInfo.user} />
                    </>
                  )}
                </>
              )}
              <SummaryRow label="管理员账号" value={username} />
              <SummaryRow label="昵称" value={nickname} />
              <SummaryRow label="密码" value={password} />
              <SummaryRow label="联系方式" value={phone} />
              <SummaryRow label="邮箱" value={email} />
            </div>

            <Button
              type="button"
              variant="outline"
              className="w-full"
              onClick={downloadAccountInfo}
            >
              <Download />
              下载账号信息(txt)
            </Button>

            <div className="mt-auto flex gap-2">
              <Button
                type="button"
                variant="outline"
                className="flex-1"
                onClick={() => setStep(2)}
              >
                上一步
              </Button>
              <Button
                type="button"
                className="flex-1"
                disabled={creating}
                onClick={finish}
              >
                {creating ? (
                  <>
                    <Loader2 className="animate-spin" />
                    创建中…
                  </>
                ) : (
                  "完成并进入"
                )}
              </Button>
            </div>
          </div>
        )}
        </div>
      </div>
    </div>
  );
}

function StepDot({
  index,
  current,
  label,
}: {
  index: 1 | 2 | 3;
  current: 1 | 2 | 3;
  label: string;
}) {
  const done = current > index;
  const active = current === index;
  return (
    <div className="flex items-center gap-1.5">
      <span
        className={`flex size-5 items-center justify-center rounded-full text-[0.7rem] transition-all duration-300 ${
          active
            ? "scale-110 bg-primary text-primary-foreground shadow-md shadow-primary/40 ring-2 ring-primary/30"
            : done
              ? "bg-primary text-primary-foreground"
              : "bg-muted text-muted-foreground"
        }`}
      >
        {done ? <Check className="size-3" /> : index}
      </span>
      <span
        className={`transition-colors ${
          active ? "font-medium text-foreground" : "text-muted-foreground"
        }`}
      >
        {label}
      </span>
    </div>
  );
}

function SummaryRow({ label, value }: { label: string; value: string }) {
  return (
    <div className="flex gap-3">
      <span className="w-20 shrink-0 text-muted-foreground">{label}</span>
      <span className="min-w-0 flex-1 break-all text-foreground">{value}</span>
    </div>
  );
}

function PgField({ label, value }: { label: string; value: string }) {
  return (
    <div className="flex min-w-0 gap-1.5">
      <span className="shrink-0 text-muted-foreground">{label}:</span>
      <span className="min-w-0 truncate text-foreground">{value}</span>
    </div>
  );
}

function DbOption({
  active,
  icon: Icon,
  title,
  desc,
  onClick,
}: {
  active: boolean;
  icon: typeof HardDrive;
  title: string;
  desc: string;
  onClick: () => void;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      className={`flex w-full items-center gap-3 rounded-lg border p-3 text-left transition-colors ${
        active
          ? "border-primary bg-primary/5 ring-1 ring-primary"
          : "hover:bg-muted/50"
      }`}
    >
      <span
        className={`flex size-10 shrink-0 items-center justify-center rounded-lg transition-colors ${
          active
            ? "bg-primary/10 text-primary"
            : "bg-muted text-muted-foreground"
        }`}
      >
        <Icon className="size-5" />
      </span>
      <div className="min-w-0 flex-1">
        <div className="text-sm font-medium">{title}</div>
        <div className="text-xs text-muted-foreground">{desc}</div>
      </div>
      {active && <Check className="size-4 shrink-0 text-primary" />}
    </button>
  );
}
