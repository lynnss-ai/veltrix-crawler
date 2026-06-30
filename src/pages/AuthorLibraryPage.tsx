// 作者库:采集到的作者档案(authors 表)。画像 + 已采内容聚合 + 监控开关。
// 筛选:左侧行业栏(作者跨行业按其内容所属任务聚合)+ 平台 chip + 仅看监控 + 关键字。
import { memo, useEffect, useMemo, useState } from "react";
import { type ColumnDef } from "@tanstack/react-table";
import { Eye, RefreshCw, Search, UserRound, X } from "lucide-react";
import { openUrl } from "@tauri-apps/plugin-opener";
import { toast } from "sonner";

import { DataTable } from "@/components/DataTable";
import { DataTableColumnHeader } from "@/components/DataTableColumnHeader";
import { EmptyState } from "@/components/EmptyState";
import { FORM_CONTROL_SIZING } from "@/lib/form-sizing";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Switch } from "@/components/ui/switch";
import { SimpleTooltip } from "@/components/SimpleTooltip";
import { FilterSidebar, IndustryFilterToggle } from "@/components/library-filters";
import { useResponsiveCollapse } from "@/hooks/use-responsive-collapse";
import {
  api,
  type AuthorView,
  type IndustryView,
  type PlatformConfig,
} from "@/lib/api";
import {
  authorProfileUrl,
  platformChipClass,
  platformClass,
} from "@/lib/platforms";

// 支持画像补采的平台(搜索响应缺画像、需打开主页拦截补全);其余平台搜索已含完整画像。
// 抖音搜索不含粉丝/关注/获赞/属地,同样需打开主页补采
const ENRICH_SUPPORTED = new Set(["douyin", "xhs", "kuaishou", "bilibili", "youtube"]);

// 互动数:过万折算成「万」
function formatCount(n?: number | null): string {
  if (n == null) return "—";
  if (n >= 10000) return `${(n / 10000).toFixed(1)}万`;
  return String(n);
}

// Unix 秒 → 本地日期
function formatDate(ts?: number | null): string {
  if (!ts) return "—";
  return new Date(ts * 1000).toLocaleDateString();
}

export function AuthorLibraryPage() {
  const [authors, setAuthors] = useState<AuthorView[]>([]);
  const [platforms, setPlatforms] = useState<PlatformConfig[]>([]);
  const [industries, setIndustries] = useState<IndustryView[]>([]);
  const [search, setSearch] = useState("");
  const [platformFilter, setPlatformFilter] = useState(""); // ""=全部
  const [industryFilter, setIndustryFilter] = useState("__all");
  const [monitoredOnly, setMonitoredOnly] = useState(false);
  const [sidebarCollapsed, setSidebarCollapsed] = useResponsiveCollapse();
  // 正在切换监控的作者 id 集合(行级 loading,防重复点击)
  const [toggling, setToggling] = useState<Set<string>>(new Set());
  // 画像补采进行中:禁用按钮,防重复触发
  const [enriching, setEnriching] = useState(false);

  const platformName = (id: string) =>
    platforms.find((p) => p.id === id)?.name ?? id;

  useEffect(() => {
    api
      .listAuthors()
      .then(setAuthors)
      .catch((e) => toast.error(`加载作者失败: ${e}`));
    api.listPlatforms().then(setPlatforms).catch((e) => console.warn("加载平台列表失败:", e));
    api.listIndustries().then(setIndustries).catch((e) => console.warn("加载行业列表失败:", e));
  }, []);

  const platformOptions = useMemo(() => platforms.map((p) => p.id), [platforms]);

  // 各行业作者数(侧栏角标);作者可跨行业,命中任一行业即计入
  const industryCounts = useMemo(() => {
    const map: Record<string, number> = { __all: authors.length };
    for (const a of authors) {
      for (const ind of a.industries) {
        map[ind] = (map[ind] ?? 0) + 1;
      }
    }
    return map;
  }, [authors]);

  const filtered = useMemo(() => {
    return authors.filter((a) => {
      if (platformFilter && a.platform !== platformFilter) return false;
      if (industryFilter !== "__all" && !a.industries.includes(industryFilter))
        return false;
      if (monitoredOnly && !a.isMonitored) return false;
      if (search) {
        const q = search.toLowerCase();
        return (
          a.nickname.toLowerCase().includes(q) ||
          a.uid.toLowerCase().includes(q) ||
          (a.platformId ?? "").toLowerCase().includes(q)
        );
      }
      return true;
    });
  }, [authors, platformFilter, industryFilter, monitoredOnly, search]);

  const hasFilter =
    platformFilter !== "" ||
    industryFilter !== "__all" ||
    monitoredOnly ||
    search !== "";

  // 切换作者监控:行级 loading,成功后就地更新
  async function toggleMonitor(a: AuthorView, next: boolean) {
    if (toggling.has(a.id)) return;
    setToggling((prev) => new Set(prev).add(a.id));
    try {
      await api.setAuthorMonitoredById(a.id, next);
      setAuthors((prev) =>
        prev.map((x) => (x.id === a.id ? { ...x, isMonitored: next } : x)),
      );
      toast.success(next ? "已开启作者监控" : "已关闭作者监控");
    } catch (e) {
      toast.error(`操作失败: ${e}`);
    } finally {
      setToggling((prev) => {
        const nextSet = new Set(prev);
        nextSet.delete(a.id);
        return nextSet;
      });
    }
  }

  // 切换作者黑名单:加入后再次采集会排除其内容。行级 loading,成功后就地更新
  async function toggleBlacklist(a: AuthorView, next: boolean) {
    if (toggling.has(a.id)) return;
    setToggling((prev) => new Set(prev).add(a.id));
    try {
      await api.setAuthorBlacklistedById(a.id, next);
      setAuthors((prev) =>
        prev.map((x) => (x.id === a.id ? { ...x, isBlacklisted: next } : x)),
      );
      toast.success(next ? "已加入黑名单 · 采集将排除其内容" : "已移出黑名单");
    } catch (e) {
      toast.error(`操作失败: ${e}`);
    } finally {
      setToggling((prev) => {
        const nextSet = new Set(prev);
        nextSet.delete(a.id);
        return nextSet;
      });
    }
  }

  // 画像补采:对当前筛选下「支持补采」的作者逐个打开主页拦截画像接口刷新档案。
  // 串行 + 逐个开窗,可能较慢——用当前筛选(平台 / 仅看监控 / 关键字)控制范围。
  async function enrichFiltered() {
    if (enriching) return;
    const eligible = filtered.filter((a) => ENRICH_SUPPORTED.has(a.platform));
    if (eligible.length === 0) {
      toast.info("当前筛选下没有可补采的作者(支持:抖音 / 小红书 / 快手 / B站 / YouTube)");
      return;
    }
    setEnriching(true);
    const toastId = toast.loading(
      `正在补采 ${eligible.length} 位作者画像 · 会逐个打开主页,请稍候…`,
    );
    try {
      const r = await api.enrichAuthors(eligible.map((a) => a.id));
      toast.success(
        `补采完成 · 更新 ${r.updated} · 跳过 ${r.skipped} · 失败 ${r.failed}`,
        { id: toastId },
      );
      // 跳过 / 失败明细量大,前端 toast 只给汇总,逐条打到控制台便于排查
      if (r.messages.length) console.warn("画像补采明细:", r.messages);
      // 刷新档案,展示最新画像
      const fresh = await api.listAuthors();
      setAuthors(fresh);
    } catch (e) {
      toast.error(`补采失败: ${e}`, { id: toastId });
    } finally {
      setEnriching(false);
    }
  }

  // 依赖 platforms / toggling:平台名与行级开关态变化时重建列
  // eslint-disable-next-line react-hooks/exhaustive-deps
  const columns = useMemo<ColumnDef<AuthorView>[]>(
    () => [
      {
        id: "author",
        header: "作者",
        enableSorting: false,
        cell: ({ row }) => <AuthorCell a={row.original} />,
      },
      {
        id: "platform",
        accessorKey: "platform",
        header: ({ column }) => (
          <DataTableColumnHeader column={column} title="平台" />
        ),
        cell: ({ row }) => (
          <span
            className={`inline-block whitespace-nowrap rounded px-2 py-0.5 text-[11px] font-medium ${platformClass(row.original.platform)}`}
          >
            {platformName(row.original.platform)}
          </span>
        ),
      },
      {
        id: "followerCount",
        accessorKey: "followerCount",
        header: ({ column }) => (
          <DataTableColumnHeader column={column} title="粉丝" />
        ),
        cell: ({ row }) => (
          <span className="whitespace-nowrap font-mono text-xs">
            {formatCount(row.original.followerCount)}
          </span>
        ),
      },
      {
        id: "totalFavorited",
        accessorKey: "totalFavorited",
        header: ({ column }) => (
          <DataTableColumnHeader column={column} title="获赞" />
        ),
        cell: ({ row }) => (
          <span className="whitespace-nowrap font-mono text-xs">
            {formatCount(row.original.totalFavorited)}
          </span>
        ),
      },
      {
        id: "contentCount",
        accessorKey: "contentCount",
        header: ({ column }) => (
          <DataTableColumnHeader column={column} title="已采内容" />
        ),
        cell: ({ row }) => (
          <span className="whitespace-nowrap font-mono text-xs text-sky-600 dark:text-sky-400">
            {row.original.contentCount}
          </span>
        ),
      },
      {
        id: "location",
        accessorKey: "location",
        header: ({ column }) => (
          <DataTableColumnHeader column={column} title="属地" />
        ),
        cell: ({ row }) => (
          <span className="whitespace-nowrap text-xs text-muted-foreground">
            {row.original.location ?? "—"}
          </span>
        ),
      },
      {
        id: "lastCollectedAt",
        accessorKey: "lastCollectedAt",
        header: ({ column }) => (
          <DataTableColumnHeader column={column} title="最近采集" />
        ),
        cell: ({ row }) => (
          <span className="whitespace-nowrap text-xs text-muted-foreground">
            {formatDate(row.original.lastCollectedAt)}
          </span>
        ),
      },
      {
        id: "monitored",
        accessorKey: "isMonitored",
        header: ({ column }) => (
          <DataTableColumnHeader column={column} title="监控" />
        ),
        cell: ({ row }) => {
          const a = row.original;
          return (
            <span className="flex items-center gap-2 text-xs">
              <Switch
                checked={a.isMonitored}
                disabled={toggling.has(a.id)}
                onCheckedChange={(v) => toggleMonitor(a, v)}
              />
              <span
                className={
                  a.isMonitored
                    ? "text-emerald-600 dark:text-emerald-400"
                    : "text-muted-foreground"
                }
              >
                {a.isMonitored ? "监控中" : "未监控"}
              </span>
            </span>
          );
        },
      },
      {
        id: "blacklisted",
        accessorKey: "isBlacklisted",
        header: ({ column }) => (
          <DataTableColumnHeader column={column} title="黑名单" />
        ),
        cell: ({ row }) => {
          const a = row.original;
          return (
            <span className="flex items-center gap-2 text-xs">
              <Switch
                checked={a.isBlacklisted}
                disabled={toggling.has(a.id)}
                onCheckedChange={(v) => toggleBlacklist(a, v)}
              />
              <span
                className={
                  a.isBlacklisted
                    ? "text-red-600 dark:text-red-400"
                    : "text-muted-foreground"
                }
              >
                {a.isBlacklisted ? "已拉黑" : "正常"}
              </span>
            </span>
          );
        },
      },
    ],
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [platforms, toggling],
  );

  return (
    <div className="flex min-h-0 min-w-0 flex-1 gap-4">
      {/* 左侧:行业筛选(可折叠);作者行业由其内容所属任务聚合 */}
      {!sidebarCollapsed && (
        <FilterSidebar
          industries={industries}
          industryCounts={industryCounts}
          industryFilter={industryFilter}
          onIndustry={setIndustryFilter}
          onCollapse={() => setSidebarCollapsed(true)}
        />
      )}

      <div
        className={`flex min-h-0 min-w-0 flex-1 flex-col gap-3 ${FORM_CONTROL_SIZING}`}
      >
        {/* 第一行:行业按钮(收起态)+ 关键字搜索 + 重置 */}
        <div className="flex flex-wrap items-center gap-x-4 gap-y-2">
          {sidebarCollapsed && (
            <IndustryFilterToggle onExpand={() => setSidebarCollapsed(false)} />
          )}
          <div className="relative w-full sm:w-72 lg:w-80">
            <Search className="pointer-events-none absolute left-3 top-1/2 size-4 -translate-y-1/2 text-muted-foreground" />
            <Input
              value={search}
              onChange={(e) => setSearch(e.target.value)}
              placeholder="昵称 / UID / 平台号"
              className="pl-9"
            />
          </div>
          {hasFilter && (
            <Button
              variant="ghost"
              className="cursor-pointer px-2 lg:px-3"
              onClick={() => {
                setPlatformFilter("");
                setIndustryFilter("__all");
                setMonitoredOnly(false);
                setSearch("");
              }}
            >
              重置
              <X />
            </Button>
          )}
        </div>
        {/* 第二行:平台筛选 + 仅看监控,移到搜索框下方 */}
        <div className="flex flex-wrap items-center gap-2">
          {platformOptions.map((id) => (
            <button
              key={id}
              type="button"
              onClick={() =>
                setPlatformFilter((prev) => (prev === id ? "" : id))
              }
              className={platformChipClass(id, platformFilter === id)}
            >
              {platformName(id)}
            </button>
          ))}
          <label className="ml-2 flex cursor-pointer items-center gap-2 text-xs text-muted-foreground">
            <Switch checked={monitoredOnly} onCheckedChange={setMonitoredOnly} />
            仅看监控中
          </label>
          {/* 画像补采:对当前筛选下支持的作者打开主页拦截画像接口刷新档案 */}
          <SimpleTooltip content="对当前筛选下「抖音 / 小红书 / 快手 / B站 / YouTube」的作者打开主页,补全粉丝 / 签名等画像(逐个开窗,较慢)">
            <Button
              variant="outline"
              size="sm"
              className="ml-auto cursor-pointer"
              disabled={enriching}
              onClick={enrichFiltered}
            >
              <RefreshCw className={enriching ? "animate-spin" : ""} />
              {enriching ? "补采中…" : "补采画像"}
            </Button>
          </SimpleTooltip>
        </div>

        <DataTable
          columns={columns}
          data={filtered}
          itemLabel="作者"
          getRowId={(a) => a.id}
          defaultPageSize={50}
          emptyState={
            <EmptyState
              title="暂无作者"
              description="采集任务完成后,内容作者会自动归档到这里"
            />
          }
        />
      </div>
    </div>
  );
}



// 作者单元格:头像(点击跳主页)+ 昵称 + 平台号/UID + 简介(截断)
const AuthorCell = memo(function AuthorCell({ a }: { a: AuthorView }) {
  const homeUrl = authorProfileUrl(a.platform, a.uid, a.platformId);
  return (
    <div className="flex w-[26rem] max-w-full items-center gap-3 py-1">
      {a.avatar ? (
        <SimpleTooltip content={homeUrl ? "打开作者主页" : "暂无主页链接"}>
          <img
            src={a.avatar}
            alt=""
            loading="lazy"
            onClick={
              homeUrl
                ? () =>
                    openUrl(homeUrl).catch((e) =>
                      toast.error(`打开作者主页失败: ${e}`),
                    )
                : undefined
            }
            onError={(e) => {
              e.currentTarget.style.display = "none";
            }}
            className={`size-10 shrink-0 rounded-full border object-cover transition ${
              homeUrl
                ? "cursor-pointer hover:ring-2 hover:ring-primary hover:ring-offset-1"
                : ""
            }`}
          />
        </SimpleTooltip>
      ) : (
        <div className="flex size-10 shrink-0 items-center justify-center rounded-full border bg-muted text-muted-foreground">
          <UserRound className="size-5" />
        </div>
      )}
      <div className="flex min-w-0 flex-col">
        <span className="truncate text-sm font-medium text-foreground">
          {a.nickname || "未知作者"}
        </span>
        <span className="truncate text-xs text-muted-foreground">
          {a.platformId ? `@${a.platformId}` : a.uid}
        </span>
        {a.signature && (
          <span className="line-clamp-1 text-xs text-muted-foreground/80">
            {a.signature}
          </span>
        )}
      </div>
      {a.contentCount > 0 && (
        <SimpleTooltip content="该作者已采内容数">
          <span className="ml-auto inline-flex shrink-0 items-center gap-1 rounded-md bg-sky-500/10 px-2 py-0.5 text-[11px] font-medium text-sky-600 dark:text-sky-400">
            <Eye className="size-3" />
            {a.contentCount}
          </span>
        </SimpleTooltip>
      )}
    </div>
  );
});
