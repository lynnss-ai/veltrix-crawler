// Tauri IPC 命令的前端封装与类型定义,集中一处便于各页面复用。
import { invoke } from "@tauri-apps/api/core";

export interface PlatformConfig {
  id: string;
  name: string;
  enabled: boolean;
  login_url: string;
  // 其余后端字段(collect/rate_limit 等)透传,前端按需读取
  [key: string]: unknown;
}

export interface AccountView {
  id: string;
  platform: string;
  label: string;
  cookie: string;
  status: string;
  risk_count: number;
  cooldown_until: number;
  last_used_at: number;
  created_at: number;
  // 编码 / 备注 / 归属用户:后端账号表补字段后返回,当前可能为空
  code?: string;
  remark?: string;
  owner?: string;
}

export interface CollectResult {
  intercepted: number;
  urls: string[];
  contents: unknown[];
  comments: unknown[];
}

export interface DatabaseConfig {
  url: string;
  max_connections: number;
}

export interface AppConfig {
  platforms: Record<string, PlatformConfig>;
  database: DatabaseConfig;
  report: unknown;
  media: unknown;
}

export interface AccountInput {
  id: string;
  platform: string;
  label: string;
  cookie: string;
  code?: string;
  remark?: string;
  owner?: string;
}

// 用户(列表视图,不含密码)
export interface UserView {
  id: string;
  username: string;
  email: string;
  nickname: string;
  avatar: string;
  remark: string;
  status: string;
  dataScope: string;
  createdAt: number;
  updatedAt: number;
}

// 用户提交(新建必填 password,编辑留空表示不改)
export interface UserInput {
  id: string;
  username: string;
  password: string;
  email: string;
  nickname: string;
  avatar: string;
  remark: string;
  status: string;
  dataScope: string;
}

// 模型厂商
export interface ProviderDto {
  id: string;
  code: string;
  name: string;
  apiUrl: string;
  apiKey: string;
  models: string;
}

// 提示词
export interface PromptDto {
  id: string;
  code: string;
  name: string;
  content: string;
}

// 客户(列表视图,tags 为数组)
export interface CustomerView {
  id: string;
  code: string;
  name: string;
  phone: string;
  email: string;
  company: string;
  position: string;
  wechat: string;
  industry: string;
  tags: string[];
  source: string;
  status: string;
  owner: string;
  remark: string;
  createdAt: number;
  updatedAt: number;
}

// 客户提交(无时间字段,后端补)
export interface CustomerInput {
  id: string;
  code: string;
  name: string;
  phone: string;
  email: string;
  company: string;
  position: string;
  wechat: string;
  industry: string;
  tags: string[];
  source: string;
  status: string;
  owner: string;
  remark: string;
}

// 行业类别
export interface IndustryView {
  id: string;
  code: string;
  name: string;
  remark: string;
  createdAt: number;
  updatedAt: number;
}

export interface IndustryInput {
  id: string;
  code: string;
  name: string;
  remark: string;
}

// 关键词
export interface KeywordDto {
  id: string;
  industryId: string;
  word: string;
}

// 平台 API(列表视图)
export interface ApiView {
  id: string;
  platformId: string;
  name: string;
  url: string;
  remark: string;
  createdAt: number;
}

// 平台 API 提交(无时间字段,后端补 createdAt)
export interface ApiInput {
  id: string;
  platformId: string;
  name: string;
  url: string;
  remark: string;
}

export const api = {
  listPlatforms: () => invoke<PlatformConfig[]>("list_platforms"),
  upsertPlatform: (platform: PlatformConfig) =>
    invoke<void>("upsert_platform", { platform }),
  removePlatform: (id: string) => invoke<boolean>("remove_platform", { id }),
  getAppConfig: () => invoke<AppConfig>("get_app_config"),
  getDatabaseSize: () => invoke<number>("get_database_size"),
  getDataDir: () => invoke<string>("get_data_dir"),
  getDatabasePath: () => invoke<string | null>("get_database_path"),
  testDatabaseConnection: (url: string) =>
    invoke<void>("test_database_connection", { url }),
  setDatabaseConfig: (url: string, maxConnections: number) =>
    invoke<void>("set_database_config", { url, maxConnections }),
  saveTextFile: (path: string, content: string) =>
    invoke<void>("save_text_file", { path, content }),

  listAccounts: (platform: string) =>
    invoke<AccountView[]>("list_accounts", { platform }),
  upsertAccount: (account: AccountInput) =>
    invoke<void>("upsert_account", { account }),
  removeAccount: (platform: string, accountId: string) =>
    invoke<boolean>("remove_account", { platform, accountId }),
  openLoginWindow: (platform: string, accountId: string) =>
    invoke<void>("open_login_window", { platform, accountId }),

  startCollect: (platform: string, keyword: string, accountId: string) =>
    invoke<CollectResult>("start_collect", { platform, keyword, accountId }),

  // 鉴权 / 初始化
  hasUsers: () => invoke<boolean>("has_users"),
  login: (username: string, password: string) =>
    invoke<UserView>("login", { username, password }),

  // 会话:后端「当前登录用户」(替代桌面端无 token 的鉴权上下文)
  setCurrentUser: (username: string, dataScope: string) =>
    invoke<void>("set_current_user", { username, dataScope }),
  clearCurrentUser: () => invoke<void>("clear_current_user"),

  // 用户管理
  listUsers: () => invoke<UserView[]>("list_users"),
  upsertUser: (user: UserInput) => invoke<void>("upsert_user", { user }),
  removeUser: (id: string) => invoke<void>("remove_user", { id }),
  resetUserPassword: (id: string, password: string) =>
    invoke<void>("reset_user_password", { id, password }),

  // 系统配置:模型厂商
  listProviders: () => invoke<ProviderDto[]>("list_providers"),
  upsertProvider: (provider: ProviderDto) =>
    invoke<void>("upsert_provider", { provider }),
  removeProvider: (id: string) => invoke<void>("remove_provider", { id }),

  // 系统配置:提示词
  listPrompts: () => invoke<PromptDto[]>("list_prompts"),
  upsertPrompt: (prompt: PromptDto) => invoke<void>("upsert_prompt", { prompt }),
  removePrompt: (id: string) => invoke<void>("remove_prompt", { id }),

  // 客户管理
  listCustomers: () => invoke<CustomerView[]>("list_customers"),
  upsertCustomer: (customer: CustomerInput) =>
    invoke<void>("upsert_customer", { customer }),
  removeCustomer: (id: string) => invoke<void>("remove_customer", { id }),

  // 行业类别
  listIndustries: () => invoke<IndustryView[]>("list_industries"),
  upsertIndustry: (industry: IndustryInput) =>
    invoke<void>("upsert_industry", { industry }),
  removeIndustry: (id: string) => invoke<void>("remove_industry", { id }),

  // 关键词
  listKeywords: (industryId: string) =>
    invoke<KeywordDto[]>("list_keywords", { industryId }),
  createKeywords: (industryId: string, words: string[]) =>
    invoke<void>("create_keywords", { industryId, words }),
  upsertKeyword: (keyword: KeywordDto) =>
    invoke<void>("upsert_keyword", { keyword }),
  removeKeyword: (id: string) => invoke<void>("remove_keyword", { id }),

  // 平台 API 子列表
  listApis: (platformId: string) =>
    invoke<ApiView[]>("list_apis", { platformId }),
  upsertApi: (item: ApiInput) => invoke<void>("upsert_api", { item }),
  removeApi: (id: string) => invoke<void>("remove_api", { id }),
};

// Unix 秒 -> 本地时间字符串;0 视为「未使用过」
export function formatTimestamp(ts: number): string {
  if (!ts) return "—";
  return new Date(ts * 1000).toLocaleString();
}
