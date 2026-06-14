import { useState, type FormEvent } from "react";
import {
  Contact,
  Eye,
  EyeOff,
  Globe,
  Layers,
  Loader2,
  LogIn,
  Lock,
  Radar,
  ShieldCheck,
  Sparkles,
  Target,
  User,
  type LucideIcon,
} from "lucide-react";
import { toast } from "sonner";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { api, type UserView } from "@/lib/api";

interface LoginPageProps {
  onSuccess: (user: UserView, remember: boolean) => void;
}

interface Feature {
  icon: LucideIcon;
  title: string;
  desc: string;
}

// 右侧品牌区营销卖点
const FEATURES: Feature[] = [
  {
    icon: Globe,
    title: "多平台数据采集",
    desc: "覆盖主流平台,内容与互动数据一键聚合",
  },
  {
    icon: Sparkles,
    title: "AI 智能分析",
    desc: "意图识别 + 意向评分,洞察数据背后的机会",
  },
  {
    icon: Contact,
    title: "客户全周期管理",
    desc: "从线索到成交,跟进状态尽在掌握",
  },
  {
    icon: Target,
    title: "行业关键词运营",
    desc: "按行业组织关键词,精准触达目标人群",
  },
  {
    icon: Layers,
    title: "多账号矩阵",
    desc: "账号集群并行运转,效率与稳定兼得",
  },
  {
    icon: ShieldCheck,
    title: "数据安全可控",
    desc: "本地存储 + 权限分级,数据自主掌控",
  },
];

// 背景漂浮光点(位置 / 大小 / 动画时长与延迟)
const PARTICLES = [
  { left: "12%", top: "22%", size: 6, dur: 14, delay: 0 },
  { left: "72%", top: "16%", size: 4, dur: 18, delay: 2 },
  { left: "86%", top: "58%", size: 8, dur: 16, delay: 1 },
  { left: "24%", top: "72%", size: 5, dur: 20, delay: 3 },
  { left: "56%", top: "82%", size: 4, dur: 15, delay: 1.5 },
];

// 登录页(参考 shadcn login-04 分屏布局):左侧表单 + 右侧品牌视觉。
export function LoginPage({ onSuccess }: LoginPageProps) {
  const [username, setUsername] = useState("");
  const [password, setPassword] = useState("");
  const [showPassword, setShowPassword] = useState(false);
  const [remember, setRemember] = useState(true);
  const [loading, setLoading] = useState(false);

  async function handleSubmit(event: FormEvent) {
    event.preventDefault();
    if (!username.trim() || !password) {
      toast.error("请输入用户名和密码");
      return;
    }
    setLoading(true);
    try {
      const user = await api.login(username.trim(), password);
      onSuccess(user, remember);
    } catch (e) {
      toast.error(String(e));
    } finally {
      setLoading(false);
    }
  }

  return (
    <div className="grid min-h-svh lg:grid-cols-2">
      {/* 左侧:登录表单(跟随主题,与初始化页一致的 primary 渐变背景) */}
      <div className="flex flex-col gap-4 bg-gradient-to-br from-primary/15 to-background p-6 md:p-10">
        <div className="flex items-center gap-2.5">
          <div className="flex size-9 items-center justify-center rounded-xl bg-gradient-to-br from-indigo-500 to-violet-600 text-white shadow-lg shadow-indigo-500/30">
            <Radar className="size-5" />
          </div>
          <div className="leading-tight">
            <div className="font-semibold">VeltrixLoop</div>
            <div className="text-xs text-muted-foreground">协作平台</div>
          </div>
        </div>

        <div className="flex flex-1 items-center justify-center">
          <form onSubmit={handleSubmit} className="w-full max-w-sm space-y-6">
            <div className="space-y-2">
              <h1 className="text-3xl font-bold tracking-tight text-foreground">
                欢迎回来
              </h1>
              <p className="text-sm text-muted-foreground">
                登录后与团队一起高效协作
              </p>
            </div>

            <div className="space-y-4">
              <div className="space-y-1.5">
                <Label htmlFor="username">用户名</Label>
                <div className="relative">
                  <User className="pointer-events-none absolute left-3 top-1/2 size-4 -translate-y-1/2 text-muted-foreground" />
                  <Input
                    id="username"
                    placeholder="请输入用户名"
                    className="h-11 pl-9"
                    value={username}
                    onChange={(e) => setUsername(e.target.value)}
                    autoFocus
                  />
                </div>
              </div>

              <div className="space-y-1.5">
                <div className="flex items-center justify-between">
                  <Label htmlFor="password">密码</Label>
                  <button
                    type="button"
                    onClick={() => toast.info("请联系管理员重置密码")}
                    className="text-xs text-muted-foreground transition-colors hover:text-primary"
                  >
                    忘记密码?
                  </button>
                </div>
                <div className="relative">
                  <Lock className="pointer-events-none absolute left-3 top-1/2 size-4 -translate-y-1/2 text-muted-foreground" />
                  <Input
                    id="password"
                    type={showPassword ? "text" : "password"}
                    placeholder="请输入密码"
                    className="h-11 pl-9 pr-9"
                    value={password}
                    onChange={(e) => setPassword(e.target.value)}
                  />
                  <button
                    type="button"
                    onClick={() => setShowPassword((s) => !s)}
                    className="absolute right-3 top-1/2 -translate-y-1/2 text-muted-foreground transition-colors hover:text-foreground"
                  >
                    {showPassword ? (
                      <EyeOff className="size-4" />
                    ) : (
                      <Eye className="size-4" />
                    )}
                    <span className="sr-only">
                      {showPassword ? "隐藏密码" : "显示密码"}
                    </span>
                  </button>
                </div>
              </div>

              <div className="flex items-center gap-2">
                <Checkbox
                  id="remember"
                  checked={remember}
                  onCheckedChange={(v) => setRemember(v === true)}
                />
                <Label
                  htmlFor="remember"
                  className="text-sm font-normal text-muted-foreground"
                >
                  记住我
                </Label>
              </div>
            </div>

            <Button
              type="submit"
              className="h-11 w-full gap-2 border-0 bg-gradient-to-r from-indigo-500 to-violet-600 bg-clip-border text-base font-medium text-white shadow-lg shadow-indigo-500/30 transition-all hover:-translate-y-0.5 hover:shadow-xl hover:shadow-indigo-500/45 active:translate-y-0 active:shadow-md disabled:translate-y-0 disabled:opacity-70"
              disabled={loading}
            >
              {loading ? (
                <>
                  <Loader2 className="animate-spin" />
                  登录中…
                </>
              ) : (
                <>
                  <LogIn className="size-4" />
                  登 录
                </>
              )}
            </Button>
          </form>
        </div>

        <div className="text-center text-xs text-muted-foreground">
          © 2026 VeltrixLoop · 让团队协作更高效
        </div>
      </div>

      {/* 右侧:品牌展示区(固定深色科技感,小屏隐藏) */}
      <div className="relative hidden flex-col overflow-hidden bg-slate-950 p-12 text-white lg:flex">
        {/* 科技网格纹理(中心渐隐) */}
        <div
          className="pointer-events-none absolute inset-0 opacity-[0.12]"
          style={{
            backgroundImage:
              "linear-gradient(to right, rgb(129 140 248 / 0.5) 1px, transparent 1px), linear-gradient(to bottom, rgb(129 140 248 / 0.5) 1px, transparent 1px)",
            backgroundSize: "44px 44px",
            maskImage:
              "radial-gradient(ellipse at center, black 35%, transparent 78%)",
            WebkitMaskImage:
              "radial-gradient(ellipse at center, black 35%, transparent 78%)",
          }}
        />
        {/* 流动光晕 */}
        <div
          className="pointer-events-none absolute -left-24 -top-24 h-80 w-80 rounded-full bg-indigo-600/30 blur-3xl"
          style={{ animation: "veltrix-float 16s ease-in-out infinite" }}
        />
        <div
          className="pointer-events-none absolute -bottom-20 right-0 h-96 w-96 rounded-full bg-fuchsia-600/20 blur-3xl"
          style={{ animation: "veltrix-float-rev 22s ease-in-out infinite" }}
        />
        {/* 漂浮发光光点 */}
        {PARTICLES.map((p, i) => (
          <div
            key={i}
            className="pointer-events-none absolute rounded-full bg-indigo-300/70"
            style={{
              left: p.left,
              top: p.top,
              width: p.size,
              height: p.size,
              boxShadow: "0 0 10px 1px rgb(129 140 248 / 0.7)",
              animation: `veltrix-float ${p.dur}s ease-in-out ${p.delay}s infinite`,
            }}
          />
        ))}

        <div className="relative z-10 flex flex-1 flex-col justify-center">
          <h2 className="bg-gradient-to-r from-white via-indigo-100 to-indigo-300 bg-clip-text text-4xl font-bold leading-tight text-transparent">
            协同共创 · 数据致胜
          </h2>
          <p className="mt-4 max-w-md text-slate-300">
            数据采集、AI 创作、客户增长,团队在一个平台高效协作。
          </p>

          <div className="mt-10 grid grid-cols-2 gap-3">
            {FEATURES.map((feature, index) => {
              const Icon = feature.icon;
              return (
                <div
                  key={feature.title}
                  style={{
                    // 依次入场:每块延迟 80ms,fill-mode backwards 保证延迟期间保持初始隐藏态
                    animationDelay: `${index * 80}ms`,
                    animationDuration: "500ms",
                    animationFillMode: "backwards",
                  }}
                  className="group flex animate-in items-start gap-4 rounded-xl border border-white/10 bg-white/5 p-3.5 backdrop-blur-sm transition-all duration-300 fade-in-0 slide-in-from-bottom-3 hover:-translate-y-0.5 hover:border-indigo-400/40 hover:bg-white/10 hover:shadow-lg hover:shadow-indigo-500/10"
                >
                  <div className="rounded-lg bg-indigo-500/20 p-2 transition-all duration-300 group-hover:scale-110 group-hover:bg-indigo-500/30">
                    <Icon className="h-5 w-5 text-indigo-300 transition-colors group-hover:text-indigo-200" />
                  </div>
                  <div>
                    <div className="font-medium">{feature.title}</div>
                    <div className="mt-0.5 text-sm text-slate-400">
                      {feature.desc}
                    </div>
                  </div>
                </div>
              );
            })}
          </div>
        </div>
      </div>
    </div>
  );
}
