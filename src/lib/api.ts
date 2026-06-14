// Tauri IPC 命令的前端封装与类型定义,集中一处便于各页面复用。
import { invoke } from "@tauri-apps/api/core";
import { sortByPlatform } from "@/lib/platforms";

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
  // 意向分析配置(后端 snake_case,与 database/media 一致)
  intent: {
    provider_id: string;
    model: string;
    prompt_id: string;
    batch_size: number;
  };
  // 语音转写配置(后端 snake_case,与 intent 一致)
  transcription: {
    provider_id: string;
    model: string;
  };
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
  // 初始化创建的超级管理员:前端禁止禁用 / 改数据级别
  isSuperAdmin: boolean;
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

// AI 对话:会话
export interface ConversationView {
  id: string;
  title: string;
  providerId: string;
  model: string;
  createdAt: number;
  updatedAt: number;
}

// AI 对话:上传附件(发送时传给后端)
export interface ChatAttachment {
  name: string;
  mime: string;
  data: string; // base64,无 data url 前缀
}

// AI 对话:消息
export interface ChatMessageView {
  id: number;
  conversationId: string;
  role: "user" | "assistant";
  content: string;
  createdAt: number;
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

// 采集任务
export interface KeywordStat {
  keyword: string;
  contentCount: number;
  commentCount: number;
}

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
  // 评论采集:开启后按下列规则抓评论;关闭时其余字段无意义
  collectComments?: boolean;
  // 评论发布时间范围:3d / 7d / 14d / any(不限)
  commentTimeRange?: "3d" | "7d" | "14d" | "any";
  // 单视频评论抓取上限,0 表示不限
  commentLimit?: number;
  // 评论意图分析:采集全部完成后用 AI 提取意向客户评论(依赖 collectComments)
  analyzeCommentIntent?: boolean;
  status:
    | "pending"
    | "running"
    | "collecting_comments"
    | "analyzing_comments"
    | "downloading_media"
    | "paused"
    | "completed"
    | "failed"
    | "cancelled";
  progress: number;
  // 素材下载进度(downloading_media 阶段有效):总数 / 已处理数(成功+失败均计)
  mediaTotal: number;
  mediaDone: number;
  // 评论采集进度(collecting_comments 阶段有效):待采视频总数 / 已采视频数
  commentVideoTotal: number;
  commentVideoDone: number;
  contentCount: number;
  commentCount: number;
  startedAt: number | null;
  finishedAt: number | null;
  errorMessage: string | null;
  // 是否已归档(手动归档移入归档 tab;终止/失败不自动归档)
  archived: boolean;
  // 采集完成后自动同步内容到发起者 Obsidian vault
  autoSyncObsidian: boolean;
  owner: string;
  createdAt: number;
  updatedAt: number;
  // 各关键词「本次采集」统计;list_tasks 填充,task-progress 事件推送时为空数组
  keywordStats: KeywordStat[];
  // 累计采集总量(库里该任务去重后全部内容/评论数);list_tasks 填充,事件推送时为 0
  totalContents: number;
  totalComments: number;
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
  // 评论采集相关(见 TaskView 同名字段说明)
  collectComments?: boolean;
  commentTimeRange?: "3d" | "7d" | "14d" | "any";
  commentLimit?: number;
  analyzeCommentIntent?: boolean;
  // 采集完成后自动同步内容到发起者(owner)的 Obsidian vault
  autoSyncObsidian?: boolean;
}

export interface TaskStatusPatch {
  id: string;
  status: TaskView["status"];
  startedAt?: number | null;
  finishedAt?: number | null;
  archived?: boolean | null;
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
  // 封面本地绝对路径(下载成功后回写):前端本地优先显示,失败/无则回退外链
  coverPath: string | null;
  // 作者头像本地绝对路径(下载成功后回写)
  avatarPath: string | null;
  // 视频转出音频本地绝对路径(详情页播放用);null=非视频/未提取/旧数据未记录
  audioPath: string | null;
  // 视频语音转写文本(转写成功后回写),前端展示
  transcript: string | null;
  // 转写失败原因(区分未转写与失败)
  transcriptError: string | null;
  // 细粒度处理状态:视频下载 / 图文图片进度 / 评论采集 / 意向分析
  videoDownloaded: boolean | null;
  imageTotal: number | null;
  imageDone: number | null;
  commentCollected: boolean | null;
  intentAnalyzed: boolean | null;
  // 当前登录用户是否已把该内容同步到自己的 Obsidian
  syncedByMe: boolean;
}

// 作者库视图(对应后端 AuthorView):authors 表 + 已采内容数聚合
export interface AuthorView {
  id: string;
  owner: string;
  platform: string;
  uid: string;
  nickname: string;
  avatar: string | null;
  // 平台号(抖音号等)
  platformId: string | null;
  signature: string | null;
  followerCount: number | null;
  followingCount: number | null;
  totalFavorited: number | null;
  location: string | null;
  isMonitored: boolean;
  firstCollectedAt: number;
  lastCollectedAt: number;
  // 该作者在库中的已采内容数
  contentCount: number;
  // 该作者内容覆盖的行业(去重;作者可跨多个行业)
  industries: string[];
}

// 作者画像补采结果汇总(对应后端 EnrichSummary)
export interface EnrichSummary {
  requested: number;
  updated: number;
  skipped: number;
  failed: number;
  // 跳过 / 失败的逐条原因
  messages: string[];
}

// 内容详情里的作者扩展信息 + 作者维度聚合(对应后端 AuthorDetail)
export interface AuthorDetail {
  uid: string;
  nickname: string;
  avatar: string | null;
  avatarPath: string | null;
  platformId: string | null;
  shortId: string | null;
  signature: string | null;
  followerCount: number | null;
  followingCount: number | null;
  totalFavorited: number | null;
  location: string | null;
  videoCount: number;
  commentCount: number;
  firstCollectedAt: number | null;
  lastPublishedAt: number | null;
  lastCollectedAt: number | null;
  isMonitored: boolean;
}

// 全量库内容详情(对应后端 ContentDetailView)
export interface ContentDetailView {
  content: ContentView;
  author: AuthorDetail;
}

// 单条内容素材重试结果(对应后端 MediaStatusView)
export interface MediaStatusView {
  id: string;
  mediaStatus: "pending" | "success" | "failed" | null;
  audioExtracted: boolean | null;
  mediaError: string | null;
}

// 评论库:采集落库的评论(对应后端 CommentView / comments 表)
export interface CommentView {
  id: string;
  taskId: string;
  platform: string;
  contentId: string;
  commentId: string;
  parentId: string | null;
  authorUid: string;
  authorNickname: string;
  authorAvatar: string | null;
  authorUniqueId: string | null;
  industry: string;
  text: string;
  likeCount: number | null;
  replyCount: number | null;
  createdAt: number | null;
  owner: string;
  collectedAt: number;
  // AI 意向等级:high / medium / low / none;null=未分析
  intentLevel: "high" | "medium" | "low" | "none" | null;
  intentReason: string | null;
  // 所属内容(list_comments 关联 contents 填;内容已删则为 null)
  contentTitle: string | null;
  contentKind: string | null;
  contentCoverUrl: string | null;
  contentCoverPath: string | null;
  // 内容作者(视频/图文创作者,区别于评论者 author*)
  contentAuthorNickname: string | null;
  contentAuthorAvatar: string | null;
}

// 采集日志条目(对应后端 collect-log 事件 / list_collect_logs)
export interface TaskRunView {
  id: string;
  taskId: string;
  startedAt: number;
  finishedAt: number | null;
  status: "running" | "completed" | "failed" | "cancelled";
  // 本次新增内容 / 评论数(排除重复采到的已有)
  contentDelta: number;
  commentDelta: number;
  errorMessage: string | null;
}

export interface CollectLogEntry {
  taskId: string;
  ts: number;
  level: "info" | "warn" | "error";
  message: string;
  // 富条目(内容/评论);普通日志无此字段,前端按 message 纯文本渲染
  entry?: {
    kind: "content" | "comment";
    seq: number;
    avatar: string | null;
    nickname: string;
    title: string;
    contentKind?: string | null;
  };
}

// 数据概览(对应后端 dashboard_overview)
export interface PlatformCount {
  platform: string;
  count: number;
}
export interface PlatformSeries {
  platform: string;
  counts: number[];
  contents: number[];
  comments: number[];
}
export interface IntentDistribution {
  high: number;
  medium: number;
  low: number;
  none: number;
}
export interface TodayPlatform {
  platform: string;
  contents: number;
  comments: number;
}
export interface TodayStat {
  contents: number;
  comments: number;
  contentsDelta: number;
  commentsDelta: number;
  byPlatform: TodayPlatform[];
}
export interface TaskStatusStat {
  running: number;
  pending: number;
  completedToday: number;
  failed: number;
}
export interface HotContent {
  title: string;
  platform: string;
  author: string;
  likeCount: number;
  commentCount: number;
}
export interface MediaStat {
  success: number;
  pending: number;
  failed: number;
}
export interface KeywordCount {
  keyword: string;
  count: number;
}
export interface DashboardOverview {
  contentTotal: number;
  commentTotal: number;
  intentTotal: number;
  contentByPlatform: PlatformCount[];
  contentVideo: number;
  contentImage: number;
  commentByPlatform: PlatformCount[];
  commentVideo: number;
  commentImage: number;
  intentByPlatform: PlatformCount[];
  intentVideo: number;
  intentImage: number;
  trendDates: string[];
  trendSeries: PlatformSeries[];
  intentDistribution: IntentDistribution;
  today: TodayStat;
  taskStatus: TaskStatusStat;
  hotContents: HotContent[];
  mediaStats: MediaStat;
  topKeywords: KeywordCount[];
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
  // 平台列表统一按规范顺序返回(抖音/小红书/快手/TikTok/B站/YouTube),各页筛选与选择器据此展示
  listPlatforms: () =>
    invoke<PlatformConfig[]>("list_platforms").then((list) =>
      sortByPlatform(list, (p) => p.id),
    ),
  upsertPlatform: (platform: PlatformConfig) =>
    invoke<void>("upsert_platform", { platform }),
  removePlatform: (id: string) => invoke<boolean>("remove_platform", { id }),
  getAppConfig: () => invoke<AppConfig>("get_app_config"),
  getDatabaseSize: () => invoke<number>("get_database_size"),
  getDataDir: () => invoke<string>("get_data_dir"),
  // 当前生效的素材存储根目录(绝对路径,默认/相对会补全为完整路径)
  getMediaRoot: () => invoke<string>("get_media_root"),
  getDatabasePath: () => invoke<string | null>("get_database_path"),
  testDatabaseConnection: (url: string) =>
    invoke<void>("test_database_connection", { url }),
  setDatabaseConfig: (url: string, maxConnections: number) =>
    invoke<void>("set_database_config", { url, maxConnections }),
  setStoragePath: (path: string) =>
    invoke<void>("set_storage_path", { path }),
  // 保存语音转写配置(系统设置「语音转写」)
  setTranscriptionConfig: (providerId: string, model: string) =>
    invoke<void>("set_transcription_config", { providerId, model }),
  // 各厂商能力(chat / asr),供「语音转写」按 ASR 能力过滤厂商下拉
  listProviderCapabilities: () =>
    invoke<
      {
        code: string;
        name: string;
        apiUrl: string;
        chat: boolean;
        asr: boolean;
      }[]
    >("list_provider_capabilities"),
  // 保存意向分析配置(系统设置「意向分析」)
  setIntentConfig: (
    providerId: string,
    model: string,
    promptId: string,
    batchSize: number,
  ) =>
    invoke<void>("set_intent_config", {
      providerId,
      model,
      promptId,
      batchSize,
    }),
  saveTextFile: (path: string, content: string) =>
    invoke<void>("save_text_file", { path, content }),
  // 清空业务数据(任务/内容/评论 + 媒体文件);需当前用户密码二次校验
  // 清空业务数据;clearMedia=true 时连同存储路径下的媒体资源文件一并删除
  clearBusinessData: (password: string, clearMedia: boolean) =>
    invoke<void>("clear_business_data", { password, clearMedia }),

  listAccounts: (platform: string) =>
    invoke<AccountView[]>("list_accounts", { platform }),
  upsertAccount: (account: AccountInput) =>
    invoke<void>("upsert_account", { account }),
  removeAccount: (platform: string, accountId: string) =>
    invoke<boolean>("remove_account", { platform, accountId }),
  // 清空登录状态:关窗 + 删除该账号 WebView 登录数据,并置失效(需重新登录)
  clearAccountLogin: (platform: string, accountId: string) =>
    invoke<void>("clear_account_login", { platform, accountId }),
  openLoginWindow: (platform: string, accountId: string, accountLabel: string) =>
    invoke<void>("open_login_window", { platform, accountId, accountLabel }),

  startCollect: (platform: string, keyword: string, accountId: string) =>
    invoke<CollectResult>("start_collect", { platform, keyword, accountId }),

  // 鉴权 / 初始化
  hasUsers: () => invoke<boolean>("has_users"),
  // 校验本地恢复的登录态:用户仍有效返回最新 dataScope,无效(清库/删除/禁用)返回 null
  verifySessionUser: (username: string) =>
    invoke<string | null>("verify_session_user", { username }),
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

  // AI 对话
  listConversations: () =>
    invoke<ConversationView[]>("list_conversations"),
  createConversation: (id: string, providerId: string, model: string) =>
    invoke<ConversationView>("create_conversation", { id, providerId, model }),
  renameConversation: (id: string, title: string) =>
    invoke<void>("rename_conversation", { id, title }),
  deleteConversation: (id: string) =>
    invoke<void>("delete_conversation", { id }),
  listChatMessages: (conversationId: string) =>
    invoke<ChatMessageView[]>("list_chat_messages", { conversationId }),
  sendChatMessage: (conversationId: string, content: string) =>
    invoke<ChatMessageView>("send_chat_message", { conversationId, content }),
  // 流式发送:增量经 "chat-stream" 事件推送,resolve 时返回完整 assistant 消息。
  // attachments:每项 { name, mime, data(base64,无 data url 前缀) },最多 10 个
  sendChatMessageStream: (
    conversationId: string,
    content: string,
    attachments: ChatAttachment[],
  ) =>
    invoke<ChatMessageView>("send_chat_message_stream", {
      conversationId,
      content,
      attachments,
    }),
  transcribeChatAudio: (audioBase64: string, format: string) =>
    invoke<string>("transcribe_chat_audio", { audioBase64, format }),

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
  // 评论库:列出采集落库的评论(task_id 可选,按任务过滤)
  listComments: (taskId?: string) =>
    invoke<CommentView[]>("list_comments", { taskId: taskId ?? null }),
  // 采集日志:加载某任务的历史日志(任务详情页打开时回显,再接实时事件)
  listCollectLogs: (taskId: string) =>
    invoke<CollectLogEntry[]>("list_collect_logs", { taskId }),
  // 任务执行历史(每次运行一条)+ 某次运行的采集日志(按运行时间范围切分)
  listTaskRuns: (taskId: string) =>
    invoke<TaskRunView[]>("list_task_runs", { taskId }),
  listRunLogs: (runId: string) =>
    invoke<CollectLogEntry[]>("list_run_logs", { runId }),
  // 数据概览(首页):全量库/评论库/意向客资 + 多平台采集趋势(start/end 为 Unix 秒区间,可选)
  dashboardOverview: (start?: number, end?: number) =>
    invoke<DashboardOverview>("dashboard_overview", {
      start: start ?? null,
      end: end ?? null,
    }),
  // 删除一条采集内容
  removeContent: (id: string) => invoke<void>("remove_content", { id }),
  // 批量删除采集内容(全量库多选),返回实际删除条数
  removeContents: (ids: string[]) =>
    invoke<number>("remove_contents", { ids }),
  // 失败重试:重跑单条内容的素材下载(注意:视频直链可能已过期,仍会 403)
  retryContentMedia: (id: string) =>
    invoke<MediaStatusView>("retry_content_media", { id }),
  // 失败任务补偿:按采集参数补做缺失的后处理(意向分析 / 素材下载 / 转写)
  compensateTask: (id: string) => invoke<void>("compensate_task", { id }),
  // ffmpeg 安装检测:用于「AI 文案提取」处按是否已装切换提示(已装隐藏下载引导)
  checkFfmpeg: () =>
    invoke<{ available: boolean; version: string | null }>("check_ffmpeg"),
  // Obsidian:配置当前用户 vault、读取、把内容同步进去
  setObsidianVault: (vaultPath: string) =>
    invoke<void>("set_obsidian_vault", { vaultPath }),
  getObsidianVault: () => invoke<string>("get_obsidian_vault"),
  syncContentsToObsidian: (ids: string[]) =>
    invoke<number>("sync_contents_to_obsidian", { ids }),
  // 全量库内容详情(作者扩展 + 聚合)
  getContentDetail: (id: string) =>
    invoke<ContentDetailView>("get_content_detail", { id }),
  // 作者监控开关(内容详情里的「监控状态」)
  setAuthorMonitored: (contentId: string, monitored: boolean) =>
    invoke<void>("set_author_monitored", { contentId, monitored }),
  // 作者库:列出作者档案(画像 + 聚合 + 监控)
  listAuthors: () => invoke<AuthorView[]>("list_authors"),
  // 作者库的监控开关(按作者 id)
  setAuthorMonitoredById: (id: string, monitored: boolean) =>
    invoke<void>("set_author_monitored_by_id", { id, monitored }),
  // 作者画像补采:逐个打开主页拦截画像接口,刷新粉丝/签名等档案(仅小红书/快手/B站/YouTube)
  enrichAuthors: (ids: string[]) =>
    invoke<EnrichSummary>("enrich_authors", { ids }),

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
