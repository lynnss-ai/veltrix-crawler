// Tauri IPC 数据类型(DTO 接口):与 src-tauri/commands 的 #[derive(Serialize)] 结构逐字段对应。
// 从 api.ts 拆出;api.ts 经 `export * from "./api-types"` 再导出,各页面导入路径不变。

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
  // 海外平台音频拉流代理:空=自动探测本机代理、"off"=关闭直连、其余为代理 URL
  proxy: string;
}

export interface AppConfig {
  platforms: Record<string, PlatformConfig>;
  database: DatabaseConfig;
  report: unknown;
  media: MediaConfig;
  // 意向分析配置(后端 snake_case,与 database/media 一致)
  intent: {
    api_url: string;
    model: string;
    intent_prompt: string;
    batch_size: number;
  };
  // 语音转写配置(后端 snake_case,与 intent 一致)
  transcription: {
    // ASR 厂商 code(mimo / glm,与后端 provider 预设对应)
    provider: string;
    api_url: string;
    model: string;
    // 转写并发数(同时在飞的 ASR 请求数)
    concurrency: number;
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

// 模型能力 code(与后端 llm/provider.rs::MODEL_CAPABILITIES 逐一对应):
// 对话 / 图片(视觉) / 音频 / 视频 / 工具调用(function calling)。
export type ModelCapability = "text" | "vision" | "audio" | "video" | "tools";

// 单个模型 = 名称 + 能力集合。各智能体据能力挑模型:对话/角色要 text,coding/rpa 要 tools。
export interface ModelSpec {
  name: string;
  capabilities: ModelCapability[];
}

// 模型厂商
export interface ProviderDto {
  id: string;
  code: string;
  name: string;
  apiUrl: string;
  apiKey: string;
  models: ModelSpec[];
}

// 角色模型:杂活(分类/摘要/套用)可单独配便宜模型,主任务仍走会话模型。
// 各字段值为 "providerId::model" 串或空串(空=回退会话模型)。
export interface RoleModelConfig {
  classifyModel: string;
  summaryModel: string;
  applyModel: string;
}

// AI 对话:会话
export interface ConversationView {
  id: string;
  title: string;
  providerId: string;
  model: string;
  // 场景类型:chat / coding / rpa(决定页面布局与发送走哪个 Agent)
  agentType: string;
  createdAt: number;
  updatedAt: number;
  // 是否归档(归档会话不在「最近对话」与对话页展示,仅对话记录页可见)
  archived: boolean;
  // 编程 Agent 分步计划(JSON 数组 [{title,done}] 字符串;空=无计划)。Plan 产出、Act 按其执行并勾选
  planTodos: string;
}

// AI 对话:上传附件(发送时传给后端)
export interface ChatAttachment {
  name: string;
  mime: string;
  data: string; // base64,无 data url 前缀
}

// AI 对话:历史消息里的一个附件(图片缩略图 + 文件 chip 渲染用)
export interface MessageAttachment {
  name: string;
  mime: string;
  // 后端落盘的本地绝对路径(图片);convertFileSrc 读取,空/缺则只展示文件名
  path?: string;
  // 乐观消息内联 base64(无 path 时即时预览用,无 data url 前缀)
  data?: string;
}

// AI 对话:消息
export interface ChatMessageView {
  id: number;
  conversationId: string;
  role: "user" | "assistant" | "tool";
  content: string;
  // 工具往返(Agent 场景):assistant 的工具调用(JSON 字符串)/ tool 结果归属
  toolCalls?: string | null;
  toolCallId?: string | null;
  toolName?: string | null;
  // user 消息附件(图片 + 文件);无附件为空数组 / 缺省
  attachments?: MessageAttachment[];
  // assistant 思考过程(模型推理内容);仅推理型模型非空,前端折叠展示
  reasoning?: string | null;
  // 用户反馈(点赞/点踩):like / dislike / null;加载会话时水合到前端点赞态
  feedback: string | null;
  createdAt: number;
}

// 浏览器 / RPA Agent:内嵌 webview 拦截到的一条接口响应(右栏拦截面板用)
export interface NetworkEntryView {
  url: string;
  body: string;
}

// 编程 Agent:开发服务器(预览-开发服务器模式)状态
export interface DevServerStatus {
  running: boolean;
  port: number | null;
  command: string;
  logs: string[];
  // dev server 是全局单实例,此字段标其归属会话,供前端按 activeId 隔离(切会话不串台)
  conversationId: string;
}

// 编程 Agent:一个回退版本(git 检查点;每轮任务前的工作区快照)
export interface CheckpointView {
  hash: string;
  time: number; // 提交时间(unix 秒)
  message: string; // 该轮任务标签
}

// 编程 Agent:某版本里单个文件的改动
export interface CheckpointFileDiff {
  status: "added" | "modified" | "deleted" | "renamed" | string;
  path: string;
  additions: number;
  deletions: number;
  diff: string; // 该文件的 unified diff 正文
}

// 编程 Agent:某版本(检查点)的完整改动详情
export interface CheckpointDiffView {
  files: CheckpointFileDiff[];
}

// 编程 Agent:本地沙盒状态(进程级隔离;每会话一个 Job / 进程组,首个编程动作惰性创建)
export interface SandboxConfigView {
  workspace: string; // 工作区根目录(每会话一个子目录)
  running: boolean; // 是否有沙盒进程在运行
  activeSessions: number; // 已创建的会话沙盒数\r
  memoryLimitMb: number; // 内存上限(MB,0=不限)\r
  netLimitKbps: number; // 出站网络限速(KB/s,0=不限)\r
  idleRecycleMinutes: number; // 空闲自动回收阈值(分钟,0=关闭)\r
  cpuLimitPercent: number; // CPU 上限(%,0=不限)\r
  maxProcesses: number; // 进程数上限(0=不限)\r
  ioLimitKbps: number; // 磁盘 IO 限速(KB/s,0=不限)
}

// 沙盒配置写入入参(set_sandbox_config 单对象参数;0=不限/关闭)
export interface SandboxConfigInput {
  memoryLimitMb: number;
  netLimitKbps: number;
  idleRecycleMinutes: number;
  cpuLimitPercent: number;
  maxProcesses: number;
  ioLimitKbps: number;
}

// 沙盒资源占用(Job 会计聚合;累计值,非实时百分比);无沙盒进程时 running=false、其余空
export interface SandboxStatsView {
  running: boolean;
  cpuPerc: string; // 累计 CPU 时间,如 "12.3s"
  memUsage: string; // 峰值内存,如 "45.6 MB"
  memPerc: string; // 存活进程数,如 "3 进程"\r
  storageBytes: number; // 全部沙盒存储目录占用合计(字节;与 running 无关)\r
  memLimitBytes: number | null; // 内存上限(字节,null=不限)\r
  netLimitKbps: number; // 出站网络限速(KB/s,0=不限)
  cpuLimitPercent: number; // CPU 上限(%,0=不限)
  maxProcesses: number; // 进程数上限(0=不限)
  ioLimitKbps: number; // 磁盘 IO 限速(KB/s,0=不限)
}

// 记忆分类:身份 / 偏好 / 项目 / 人际 / 习惯 / 其它(与后端 MEM_TYPES 对应)
export type MemoryType =
  | "identity"
  | "preference"
  | "project"
  | "relationship"
  | "habit"
  | "other";

// AI 对话:长期记忆(跨会话,按用户归属)
export interface ChatMemoryView {
  id: number;
  content: string;
  source: "auto" | "manual";
  enabled: boolean;
  /** 置顶:每轮恒注入,不参与相似度淘汰 */
  pinned: boolean;
  /** 分类 */
  memType: MemoryType;
  /** 重要度 1-5 */
  importance: number;
  /** 置信度 1-5 */
  confidence: number;
  /** 命中次数(被注入的累计次数) */
  hitCount: number;
  createdAt: number;
  updatedAt: number;
}

// 长期记忆的语义检索(embedding)配置;apiKey 不回传明文,只回 hasApiKey
export interface EmbeddingConfigView {
  apiUrl: string;
  model: string;
  hasApiKey: boolean;
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
  // 定向采集目标链接(视频链接 / 作者主页链接);空数组 = 关键词搜索任务,定向任务 keywords 恒空
  targetUrls: string[];
  trigger: "once-now" | "daily" | "watching";
  scheduledAt: string | null;
  watchIntervalMin: number | null;
  sortMode: "synthetic" | "hottest" | "latest" | "most_comment" | "most_collect" | "most_danmaku";
  timeRange: "any" | "1d" | "1w" | "6m";
  perKeywordLimit: number;
  minLikes: number;
  aiExtract: boolean;
  // 音频提取:视频下载并转 mp3 留存;AI 文案提取开启时隐含开启
  audioExtract: boolean;
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
  // 平台专属额外筛选(抖音:视频时长/搜索范围/内容形式),{维度id: 选中文案};{} = 全不限
  extraFilters: Record<string, string>;
  // 失败自动重试次数上限(0=不自动重试);失败后按 1/5/15 分钟指数退避重跑
  maxRetries: number;
  // 当前失败序列已自动重试次数(成功或手动重跑新序列后归零)
  retryCount: number;
  // 下次自动重试时间(unix 秒);null=未排期(未开重试/已耗尽/运行中)
  nextRetryAt: number | null;
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
  // 定向采集目标链接(视频链接 / 主页链接);省略/空 = 关键词搜索任务
  targetUrls?: string[];
  trigger: "once-now" | "daily" | "watching";
  scheduledAt?: string | null;
  watchIntervalMin?: number | null;
  sortMode: "synthetic" | "hottest" | "latest" | "most_comment" | "most_collect" | "most_danmaku";
  timeRange: "any" | "1d" | "1w" | "6m";
  perKeywordLimit: number;
  minLikes: number;
  aiExtract: boolean;
  // 音频提取:视频下载并转 mp3 留存;AI 文案提取开启时隐含开启
  audioExtract: boolean;
  // 评论采集相关(见 TaskView 同名字段说明)
  collectComments?: boolean;
  commentTimeRange?: "3d" | "7d" | "14d" | "any";
  commentLimit?: number;
  analyzeCommentIntent?: boolean;
  // 采集完成后自动同步内容到发起者(owner)的 Obsidian vault
  autoSyncObsidian?: boolean;
  // 平台专属额外筛选(抖音:视频时长/搜索范围/内容形式),{维度id: 选中文案};省略 = 全不限
  extraFilters?: Record<string, string>;
  // 失败自动重试次数上限(0=不自动重试;缺省视为 0)
  maxRetries?: number;
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
  // 是否被拉黑:命中黑名单的作者在采集时被排除、不抓
  isBlacklisted: boolean;
  firstCollectedAt: number;
  lastCollectedAt: number;
  // 该作者在库中的已采内容数
  contentCount: number;
  // 该作者内容覆盖的行业(去重;作者可跨多个行业)
  industries: string[];
}

// 作者画像补采结果汇总(对应后端 EnrichSummary)
// 全量库补采评论结果汇总(对应后端 RecollectCommentsSummary)
export interface RecollectCommentsSummary {
  requested: number;
  // 实际发起评论采集的视频数(排除评论数为 0 / 平台不支持等跳过项)
  attempted: number;
  succeeded: number;
  failed: number;
  skipped: number;
  // 本次入库的评论条数(按时间范围 / 单视频上限过滤后)
  comments: number;
  // 跳过 / 失败的逐条原因
  messages: string[];
}

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
  // 最新转写文本 / 失败原因(音频重试成功会顺带补转写,转写重试会更新两者)
  transcript: string | null;
  transcriptError: string | null;
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
  // 采集该内容时命中的关键词(从所属内容关联取;内容已删则为空串)
  keyword: string;
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

// 屏幕录制状态(对应后端 RecordingStatus)
export interface RecordingStatus {
  // 是否正在录制
  recording: boolean;
  // 开始时间(Unix 秒);未录制为 null
  startedAt: number | null;
  // 输出 MP4 路径;未录制为 null
  outputPath: string | null;
}

// ===================== 账单计费 =====================

export interface BillingOverview {
  totalTokens: number;
  totalPromptTokens: number;
  totalCompletionTokens: number;
  totalRequests: number;
  byModel: ModelUsage[];
  requestByModel: ModelRequestCount[];
  tokenTrendDates: string[];
  tokenTrendSeries: ModelTrendSeries[];
  requestTrendDates: string[];
  requestTrendSeries: ModelTrendSeries[];
}

export interface ModelUsage {
  model: string;
  promptTokens: number;
  completionTokens: number;
  totalTokens: number;
  lastRequestedAt: number;
}

export interface ModelRequestCount {
  model: string;
  count: number;
}

export interface ModelTrendSeries {
  model: string;
  values: number[];
}
