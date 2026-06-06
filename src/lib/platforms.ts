// 平台仅由"平台管理"维护,这里不再保存具体平台列表/名称。
// 唯一保留的是:给任意平台 id 分配一个稳定的标签颜色(避免没维护过的平台显示成无色)。
//
// 名称要由调用方自己从 api.listPlatforms() 拿到的数据查表(每个页面已经在这么用)。

// 调色板:UI 一致性的有限色相;通过 id 哈希定到其中一种,颜色稳定且不重复维护
const PALETTE: string[] = [
  "bg-rose-500/10 text-rose-600 dark:text-rose-400",
  "bg-amber-500/10 text-amber-600 dark:text-amber-400",
  "bg-emerald-500/10 text-emerald-600 dark:text-emerald-400",
  "bg-sky-500/10 text-sky-600 dark:text-sky-400",
  "bg-blue-500/10 text-blue-600 dark:text-blue-400",
  "bg-violet-500/10 text-violet-600 dark:text-violet-400",
  "bg-fuchsia-500/10 text-fuchsia-600 dark:text-fuchsia-400",
  "bg-slate-500/10 text-slate-700 dark:text-slate-300",
];

// 简单字符串哈希(FNV-1a 变体),把任意 id 稳定映射到 0..PALETTE.length
function hashToIndex(id: string): number {
  let h = 2166136261;
  for (let i = 0; i < id.length; i += 1) {
    h ^= id.charCodeAt(i);
    h = Math.imul(h, 16777619);
  }
  return Math.abs(h) % PALETTE.length;
}

/** 按 platform id 取一个稳定的标签底色 className;空 id 回退中性色 */
export function platformClass(id: string): string {
  if (!id) return "bg-muted text-muted-foreground";
  return PALETTE[hashToIndex(id)];
}
