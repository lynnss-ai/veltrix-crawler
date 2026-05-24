import { useEffect, useState } from "react";
import { Activity, Network, Users } from "lucide-react";
import { api } from "@/lib/api";
import { ErrorBanner } from "@/components/ErrorBanner";
import { StatCard } from "@/components/StatCard";

interface Overview {
  platforms: number;
  enabledPlatforms: number;
  accounts: number;
}

// 进场 stagger 步进(ms)
const ENTER_STEP_MS = 80;

// 数据概览:聚合现有可用数据(平台数、启用数、账号总数)做概要展示。
export function DashboardPage() {
  const [overview, setOverview] = useState<Overview | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    (async () => {
      try {
        const platforms = await api.listPlatforms();
        // 各平台账号数并行统计,单平台失败不影响整体
        const counts = await Promise.all(
          platforms.map((p) =>
            api
              .listAccounts(p.id)
              .then((list) => list.length)
              .catch(() => 0),
          ),
        );
        setOverview({
          platforms: platforms.length,
          enabledPlatforms: platforms.filter((p) => p.enabled).length,
          accounts: counts.reduce((sum, n) => sum + n, 0),
        });
      } catch (e) {
        setError(String(e));
      }
    })();
  }, []);

  return (
    <div className="space-y-6">
      <ErrorBanner message={error} onClose={() => setError(null)} />

      <div className="grid grid-cols-1 gap-5 sm:grid-cols-3">
        <StatCard
          icon={Network}
          label="接入平台"
          value={overview?.platforms ?? "—"}
          hint={`${overview?.enabledPlatforms ?? 0} 个已启用`}
          delay={0}
        />
        <StatCard
          icon={Users}
          label="账号总数"
          value={overview?.accounts ?? "—"}
          hint="跨全部平台"
          delay={ENTER_STEP_MS}
        />
        <StatCard
          icon={Activity}
          label="今日采集"
          value="—"
          hint="待任务调度接入"
          delay={ENTER_STEP_MS * 2}
        />
      </div>

      <div
        className="veltrix-card veltrix-enter p-6"
        style={{ animationDelay: `${ENTER_STEP_MS * 3}ms` }}
      >
        <h2 className="mb-3 text-sm font-semibold text-foreground">系统状态</h2>
        <ul className="space-y-2 text-sm text-muted-foreground">
          <li>• 采集方式:RPA 驱动 + WebView 接口拦截</li>
          <li>• 存储后端:SeaORM(SQLite / PostgreSQL 运行时二选一)</li>
          <li>• 账号隔离:每账号独立 WebView 数据目录</li>
        </ul>
      </div>
    </div>
  );
}
