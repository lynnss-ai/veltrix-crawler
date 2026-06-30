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

// 实心调色板:与 PALETTE 同色相、同索引,但实底白字。用于叠在封面图等彩色背景上,保证高对比、明显
const SOLID_PALETTE: string[] = [
  "bg-rose-500 text-white",
  "bg-amber-500 text-white",
  "bg-emerald-500 text-white",
  "bg-sky-500 text-white",
  "bg-blue-500 text-white",
  "bg-violet-500 text-white",
  "bg-fuchsia-500 text-white",
  "bg-slate-600 text-white",
];

// 内置平台官方主色调(全局统一,跨页面平台标一致):
//   小红书 = 品牌红 #FF2442 / 快手 = 品牌橙 #FF6A00 / 抖音 = 品牌黑(随主题前景色:亮黑暗白)
//   B站 = 品牌粉 #FB7299(蓝 #00AEEC 与 TikTok 青易混,取粉)/ YouTube = 品牌红 #FF0000
//   TikTok = 品牌青体系(霓虹青 #25F4EE 做底,文字取加深青保证浅色主题可读)。
// 抖音的红/青会与小红书红混淆,故用其最具辨识度的「黑」并随主题自适应,两个主题都清晰可读。
// 其余未列平台仍按 id 哈希从 PALETTE 取色。
const BRAND_TINT: Record<string, string> = {
  xhs: "bg-[#FF2442]/10 text-[#E11D48] dark:bg-[#FF2442]/20 dark:text-[#FF7A90]",
  kuaishou: "bg-[#FF6A00]/10 text-[#C2410C] dark:bg-[#FF6A00]/20 dark:text-[#FF9A4D]",
  douyin: "bg-foreground/10 text-foreground",
  bilibili: "bg-[#FB7299]/10 text-[#D6336C] dark:bg-[#FB7299]/20 dark:text-[#FB85A6]",
  tiktok: "bg-[#25F4EE]/15 text-[#0E7490] dark:bg-[#25F4EE]/20 dark:text-[#34E8E0]",
  youtube: "bg-[#FF0000]/10 text-[#DC2626] dark:bg-[#FF0000]/20 dark:text-[#FF5C5C]",
};
const BRAND_SOLID: Record<string, string> = {
  xhs: "bg-[#FF2442] text-white",
  kuaishou: "bg-[#FF6A00] text-white",
  douyin: "bg-foreground text-background",
  bilibili: "bg-[#FB7299] text-white",
  tiktok: "bg-[#25F4EE] text-black",
  youtube: "bg-[#FF0000] text-white",
};

// 图表用品牌 HEX(SVG stroke/fill 需具体颜色)。抖音品牌黑在深色图表上不可见,
// 故图表用其品牌青 #25F4EE(深浅背景都清晰);TikTok 为与抖音图表青区分,用加深青 #00A2B8;
// 未列平台返回 null,由调用方回退自有调色板。
const BRAND_HEX: Record<string, string> = {
  xhs: "#FF2442",
  kuaishou: "#FF6A00",
  douyin: "#25F4EE",
  bilibili: "#FB7299",
  tiktok: "#00A2B8",
  youtube: "#FF0000",
};

// 简单字符串哈希(FNV-1a 变体),把任意 id 稳定映射到 0..PALETTE.length
function hashToIndex(id: string): number {
  let h = 2166136261;
  for (let i = 0; i < id.length; i += 1) {
    h ^= id.charCodeAt(i);
    h = Math.imul(h, 16777619);
  }
  return Math.abs(h) % PALETTE.length;
}

/** 按 platform id 取标签底色 className;三大平台用官方主色调,其余哈希取色,空 id 回退中性色 */
export function platformClass(id: string): string {
  if (!id) return "bg-muted text-muted-foreground";
  return BRAND_TINT[id] ?? PALETTE[hashToIndex(id)];
}

/** 任意文本标签的稳定底色 className(行业等动态枚举共用,同名同色);空值回退中性色 */
export function labelBadgeClass(text: string): string {
  if (!text) return "bg-muted text-muted-foreground";
  return PALETTE[hashToIndex(text)];
}

/** 实心版平台标 className(实底白字);三大平台用官方主色调,用于叠在封面等彩色背景上需要高对比时 */
export function platformSolidClass(id: string): string {
  if (!id) return "bg-foreground/80 text-background";
  return BRAND_SOLID[id] ?? SOLID_PALETTE[hashToIndex(id)];
}

/** 平台筛选 chip 的 className(全局统一):选中=品牌实色,未选=品牌浅色淡显。各页平台筛选共用,色彩一致。 */
export function platformChipClass(id: string, active: boolean): string {
  const base =
    "cursor-pointer rounded-md border border-transparent px-3 py-1 text-xs font-medium transition-all";
  return active
    ? `${base} ${platformSolidClass(id)} shadow-sm`
    : `${base} ${platformClass(id)} opacity-70 hover:opacity-100`;
}

/** 图表(趋势线 / 环形图)用的平台品牌 HEX;未列平台返回 null,调用方回退自有调色板。 */
export function platformColorHex(id: string): string | null {
  return BRAND_HEX[id] ?? null;
}

/** 内容详情页链接(点封面跳转);平台不支持或缺 id 返回 null,由调用方回退直链 */
export function contentDetailUrl(
  platform: string,
  contentId: string | null | undefined,
): string | null {
  if (!contentId) return null;
  switch (platform) {
    case "douyin":
      return `https://www.douyin.com/video/${contentId}`;
    case "kuaishou":
      return `https://www.kuaishou.com/short-video/${contentId}`;
    case "bilibili":
      return `https://www.bilibili.com/video/${contentId}`;
    case "tiktok":
      // 用户名段填占位 `_`,TikTok 按视频 id 重定向到规范地址
      return `https://www.tiktok.com/@_/video/${contentId}`;
    case "youtube":
      return `https://www.youtube.com/watch?v=${contentId}`;
    default:
      return null;
  }
}

/** 作者主页链接(点头像跳转);平台不支持或缺 uid 返回 null。
 *  platformId 是平台号(@handle 等,作者档案的 platformId 字段):TikTok 主页只能用
 *  @handle 拼(uid 是纯数字拼不出),有则传入,缺省时 TikTok 返回 null 走调用方回退 */
export function authorProfileUrl(
  platform: string,
  uid: string | null | undefined,
  platformId?: string | null,
): string | null {
  if (platform === "tiktok") {
    return platformId ? `https://www.tiktok.com/@${platformId}` : null;
  }
  if (!uid) return null;
  switch (platform) {
    case "douyin":
      return `https://www.douyin.com/user/${uid}`;
    case "kuaishou":
      return `https://www.kuaishou.com/profile/${uid}`;
    case "bilibili":
      return `https://space.bilibili.com/${uid}`;
    case "youtube":
      return `https://www.youtube.com/channel/${uid}`;
    default:
      return null;
  }
}

// ===================== 平台展示顺序 =====================
// 全局统一的平台排序:所有列出平台的地方(各库筛选 chip / 采集任务平台选择 / 看板 / 平台管理)
// 都按此顺序展示,保证跨页面一致。未列入的平台排到末尾,彼此保持原有相对顺序。
const PLATFORM_ORDER: string[] = [
  "douyin", // 抖音
  "xhs", // 小红书
  "kuaishou", // 快手
  "tiktok", // TikTok
  "bilibili", // B站
  "youtube", // YouTube
];

/** 平台 id → 中文展示名(标准名)。平台管理里自定义改名以各页面内 platformName(读配置)为准;
 *  此处用于对话资产文案等拿不到平台配置的场景。未知 id 原样返回。 */
const PLATFORM_LABELS: Record<string, string> = {
  douyin: "抖音",
  xhs: "小红书",
  kuaishou: "快手",
  tiktok: "TikTok",
  bilibili: "B站",
  youtube: "YouTube",
};

export function platformLabel(id: string): string {
  return PLATFORM_LABELS[id] ?? id;
}

/** 平台 id → 作者「用户编码」的平台叫法(复制提示 / tooltip 用)。
 *  抖音=抖音号,小红书=小红书号……未知平台回退通用「用户编码」。 */
const PLATFORM_UID_LABELS: Record<string, string> = {
  douyin: "抖音号",
  xhs: "小红书号",
  kuaishou: "快手号",
  tiktok: "TikTok ID",
  bilibili: "B站 UID",
  youtube: "频道 ID",
};

export function platformUidLabel(id: string): string {
  return PLATFORM_UID_LABELS[id] ?? "用户编码";
}

/** 平台 id 的展示排序权重;未列入的平台返回末位权重,排到最后。 */
export function platformOrder(id: string): number {
  const index = PLATFORM_ORDER.indexOf(id);
  return index === -1 ? PLATFORM_ORDER.length : index;
}

/** 按平台规范顺序排序;getId 从元素取平台 id。Array.sort 稳定,未列平台保持原相对顺序。 */
export function sortByPlatform<T>(list: T[], getId: (item: T) => string): T[] {
  return [...list].sort(
    (a, b) => platformOrder(getId(a)) - platformOrder(getId(b)),
  );
}
