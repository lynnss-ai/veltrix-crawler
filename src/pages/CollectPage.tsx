import { useEffect, useState } from "react";
import { Loader2, Radar } from "lucide-react";
import {
  api,
  type AccountView,
  type CollectResult,
  type PlatformConfig,
} from "@/lib/api";
import { ErrorBanner } from "@/components/ErrorBanner";
import { StatCard } from "@/components/StatCard";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";

// 进场 stagger 步进(ms)
const ENTER_STEP_MS = 80;

// 采集工作台:选平台 + 账号 + 关键词,驱动可见 WebView 做 RPA 采集并展示拦截结果。
export function CollectPage() {
  const [platforms, setPlatforms] = useState<PlatformConfig[]>([]);
  const [platform, setPlatform] = useState("");
  const [accounts, setAccounts] = useState<AccountView[]>([]);
  const [accountId, setAccountId] = useState("");
  const [keyword, setKeyword] = useState("");
  const [running, setRunning] = useState(false);
  const [result, setResult] = useState<CollectResult | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    api
      .listPlatforms()
      .then((list) => {
        setPlatforms(list);
        setPlatform((prev) => prev || list[0]?.id || "");
      })
      .catch((e) => setError(String(e)));
  }, []);

  useEffect(() => {
    if (!platform) return;
    api
      .listAccounts(platform)
      .then((list) => {
        setAccounts(list);
        setAccountId(list[0]?.id || "");
      })
      .catch((e) => setError(String(e)));
  }, [platform]);

  async function handleStart() {
    if (!platform || !accountId || !keyword.trim()) {
      setError("请选择平台、账号并填写关键词");
      return;
    }
    setRunning(true);
    setError(null);
    setResult(null);
    try {
      const r = await api.startCollect(platform, keyword.trim(), accountId);
      setResult(r);
    } catch (e) {
      setError(String(e));
    } finally {
      setRunning(false);
    }
  }

  return (
    <div className="mx-auto max-w-4xl space-y-6">
      <ErrorBanner message={error} onClose={() => setError(null)} />

      <div className="veltrix-card veltrix-enter p-6">
        <div className="grid grid-cols-1 gap-4 sm:grid-cols-2">
          <div className="space-y-1.5">
            <Label>平台</Label>
            <Select value={platform} onValueChange={setPlatform}>
              <SelectTrigger className="w-full">
                <SelectValue placeholder="选择平台" />
              </SelectTrigger>
              <SelectContent>
                {platforms.map((p) => (
                  <SelectItem key={p.id} value={p.id}>
                    {p.name}
                    {p.enabled ? "" : "(已停用)"}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          </div>

          <div className="space-y-1.5">
            <Label>账号</Label>
            <Select
              value={accountId}
              onValueChange={setAccountId}
              disabled={accounts.length === 0}
            >
              <SelectTrigger className="w-full">
                <SelectValue placeholder="无可用账号" />
              </SelectTrigger>
              <SelectContent>
                {accounts.map((a) => (
                  <SelectItem key={a.id} value={a.id}>
                    {a.label || a.id}({a.status})
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          </div>
        </div>

        <div className="mt-4 space-y-1.5">
          <Label htmlFor="keyword">关键词</Label>
          <Input
            id="keyword"
            placeholder="输入搜索关键词,如:露营装备"
            value={keyword}
            onChange={(e) => setKeyword(e.target.value)}
            onKeyDown={(e) => e.key === "Enter" && handleStart()}
          />
        </div>

        <Button onClick={handleStart} disabled={running} className="mt-5">
          {running ? (
            <>
              <Loader2 className="animate-spin" />
              采集中…(WebView 正在搜索并滚动)
            </>
          ) : (
            <>
              <Radar />
              开始采集
            </>
          )}
        </Button>
      </div>

      {result && (
        <>
          <div className="grid grid-cols-3 gap-4">
            <StatCard
              label="拦截接口数"
              value={result.intercepted}
              delay={0}
            />
            <StatCard
              label="解析内容数"
              value={result.contents.length}
              delay={ENTER_STEP_MS}
            />
            <StatCard
              label="解析评论数"
              value={result.comments.length}
              delay={ENTER_STEP_MS * 2}
            />
          </div>

          <div
            className="veltrix-card veltrix-enter p-6"
            style={{ animationDelay: `${ENTER_STEP_MS * 3}ms` }}
          >
            <h3 className="mb-2 text-xs font-medium text-muted-foreground">
              命中接口 URL(用于核对拦截规则)
            </h3>
            {result.urls.length === 0 ? (
              <p className="text-sm text-muted-foreground">
                未拦截到任何接口,请到「平台管理」核对 intercept_patterns 与搜索 URL
              </p>
            ) : (
              <ul className="max-h-60 space-y-1 overflow-auto rounded-lg bg-muted/50 p-3">
                {result.urls.map((u, i) => (
                  <li
                    key={i}
                    className="truncate font-mono text-xs text-muted-foreground"
                    title={u}
                  >
                    {u}
                  </li>
                ))}
              </ul>
            )}
          </div>
        </>
      )}
    </div>
  );
}
