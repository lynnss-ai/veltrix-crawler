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

export interface MediaConfig {
  enable_audio_extract: boolean;
  ffmpeg_path: string | null;
  audio_format: string;
  output_dir: string;
}

export interface AppConfig {
  platforms: Record<string, PlatformConfig>;
  database: DatabaseConfig;
  report: unknown;
  media: MediaConfig;
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

// 采集任务
export interface TaskView {
  id: string;
  name: string;
  industry: string;
  platform: string;
  keywords: string[];
  trigger: "once-now" | "daily" | "watching";
  scheduledAt: string | null;
  watchIntervalMin: number | null;
  sortMode: "synthetic" | "hottest" | "latest";
  timeRange: "any" | "1d" | "1w" | "6m";
  perKeywordLimit: number;
  minLikes: number;
  aiExtract: boolean;
  status: "pending" | "running" | "paused" | "completed" | "failed" | "cancelled";
  progress: number;
  contentCount: number;
  commentCount: number;
  startedAt: number | null;
  finishedAt: number | null;
  errorMessage: string | null;
  owner: string;
  createdAt: number;
  updatedAt: number;
}

export interface TaskInput {
  id: string;
  name: string;
  industry: string;
  platform: string;
  keywords: string[];
  trigger: "once-now" | "daily" | "watching";
  scheduledAt?: string | null;
  watchIntervalMin?: number | null;
  sortMode: "synthetic" | "hottest" | "latest";
  timeRange: "any" | "1d" | "1w" | "6m";
  perKeywordLimit: number;
  minLikes: number;
  aiExtract: boolean;
}

export interface TaskStatusPatch {
  id: string;
  status: TaskView["status"];
  startedAt?: number | null;
  finishedAt?: number | null;
}

// 全量库:采集落库的内容(对应后端 ContentView / contents 表)
export interface ContentView {
  id: string;
  taskId: string;
  platform: string;
  industry: string;
  contentId: string;
  keyword: string;
  kind: "video" | "image" | "article" | "unknown";
  title: string | null;
  desc: string | null;
  authorUid: string;
  authorNickname: string;
  authorAvatar: string | null;
  likeCount: number | null;
  commentCount: number | null;
  collectCount: number | null;
  shareCount: number | null;
  playCount: number | null;
  publishedAt: number | null;
  videoUrl: string | null;
  coverUrl: string | null;
  imageUrls: string[];
  duration: number | null;
  topics: string[];
  owner: string;
  collectedAt: number;
  // 素材下载状态:pending(待处理)/success(成功)/failed(失败);null=旧数据未跑过下载
  mediaStatus: "pending" | "success" | "failed" | null;
  // 音频是否提取成功(仅视频且开启提取时有意义)
  audioExtracted: boolean | null;
  // 素材失败原因(403 / ffmpeg 失败等)
  mediaError: string | null;
}

// 单条内容素材重试结果(对应后端 MediaStatusView)
export interface MediaStatusView {
  id: string;
  mediaStatus: "pending" | "success" | "failed" | null;
  audioExtracted: boolean | null;
  mediaError: string | null;
}

// 云端连接相关
export interface CloudConfigView {
  base_url: string;
  user_token: string | null;
  pc_token: string | null;
  device_id: string;
}

export interface CloudConnectionState {
  connected: boolean;
  paired: boolean;
  last_report_at: number | null;
  last_error: string | null;
}

export interface CloudPairView {
  code: string;
  manual_code: string;
  qr_payload: string;
  expires_in: number;
  base_url: string;
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
  setStoragePath: (path: string) =>
    invoke<void>("set_storage_path", { path }),
  saveTextFile: (path: string, content: string) =>
    invoke<void>("save_text_file", { path, content }),
  // 清空业务数据(任务/内容/评论 + 媒体文件);需当前用户密码二次校验
  clearBusinessData: (password: string) =>
    invoke<void>("clear_business_data", { password }),

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

  // 采集任务
  listTasks: () => invoke<TaskView[]>("list_tasks"),
  upsertTask: (input: TaskInput) => invoke<void>("upsert_task", { input }),
  updateTaskStatus: (patch: TaskStatusPatch) =>
    invoke<void>("update_task_status", { patch }),
  removeTask: (id: string) => invoke<void>("remove_task", { id }),
  // 启动任务采集:后端选账号 + 后台遍历关键词(自动开窗 + 拟人 RPA),立即返回
  runTask: (taskId: string) => invoke<void>("run_task", { taskId }),
  // 全量库:列出采集落库的全部内容(按采集时间倒序)
  listContents: () => invoke<ContentView[]>("list_contents"),
  // 删除一条采集内容
  removeContent: (id: string) => invoke<void>("remove_content", { id }),
  // 失败重试:重跑单条内容的素材下载(注意:视频直链可能已过期,仍会 403)
  retryContentMedia: (id: string) =>
    invoke<MediaStatusView>("retry_content_media", { id }),

  // 云端连接(远程控制)
  cloudGetConfig: () => invoke<CloudConfigView>("cloud_get_config"),
  cloudGetStatus: () => invoke<CloudConnectionState>("cloud_get_status"),
  cloudSaveBaseUrl: (baseUrl: string) =>
    invoke<void>("cloud_save_base_url", { baseUrl }),
  cloudLogin: (username: string, password: string) =>
    invoke<void>("cloud_login", { username, password }),
  cloudPairInit: () => invoke<CloudPairView>("cloud_pair_init"),
  cloudDisconnect: () => invoke<void>("cloud_disconnect"),
};

// Unix 秒 -> 本地时间字符串;0 视为「未使用过」
export function formatTimestamp(ts: number): string {
  if (!ts) return "—";
  return new Date(ts * 1000).toLocaleString();
}
