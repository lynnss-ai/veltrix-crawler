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
  // 封面图片 OCR 配置(后端 snake_case,与 transcription 一致)
  ocr: {
    // 厂商 code(当前固定 glm,走智谱 files/ocr 工具接口)
    provider: string;
    api_url: string;
    // OCR 并发数(同时在飞的识别请求数)
    concurrency: number;
    // 本地预判:调云端前先用系统 OCR 离线判断封面有无文字,无文字则跳过付费请求(仅 Windows 生效)
    local_precheck: boolean;
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
  // 指定采集账号(accounts.id);null = 按「最久未用」自动轮换
  accountId?: string | null;
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
  // 保留视频文件:视频落盘到 video/ 目录供自动发布(与音频提取独立)
  keepVideo: boolean;
  // 封面文字识别:采集完成后对封面图做 OCR,结果回写内容的 coverOcrText
  coverOcr: boolean;
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
  // 指定采集账号(accounts.id);省略/null = 按「最久未用」自动轮换
  accountId?: string | null;
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
  // 保留视频文件:视频落盘供自动发布;省略 = 关闭(与音频提取独立)
  keepVideo?: boolean;
  // 封面文字识别:采集完成后对封面图做 OCR;省略 = 关闭(需先在系统设置配置 OCR Key)
  coverOcr?: boolean;
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

// SQLite → PG 一键迁移的单表结果(read=源读取,written=实际写入;skipped=目标库无此表)
export interface TableMigrationView {
  table: string;
  read: number;
  written: number;
  skipped: boolean;
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
  imagePaths?: (string | null)[];
  xsecToken?: string | null;
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
  // 视频语音转写文本(转写成功后回写),前端展示;
  // 空串 "" = 已转写但未识别到语音(空文案标记),null = 未转写/转写失败
  transcript: string | null;
  // 转写失败原因(区分未转写与失败)
  transcriptError: string | null;
  // 封面图片 OCR 识别文本(识别成功后回写),前端展示;
  // 空串 "" = 已识别但封面无文字,null = 未识别/识别失败
  coverOcrText: string | null;
  // 封面 OCR 识别失败原因(区分未识别与失败)
  coverOcrError: string | null;
  // 细粒度处理状态:视频下载 / 图文图片进度 / 评论采集 / 意向分析
  videoDownloaded: boolean | null;
  imageTotal: number | null;
  imageDone: number | null;
  commentCollected: boolean | null;
  intentAnalyzed: boolean | null;
  // 当前登录用户是否已把该内容同步到自己的 Obsidian
  syncedByMe: boolean;
}

// 列表专用瘦身视图(对应后端 ContentListView,list_contents_page 返回):
// 相比 ContentView 剔除 transcript / coverOcrText 全文、imageUrls、imagePaths 等大块字段
// (瀑布流 48 条/批、表格单页可达 1000 条,整文过 IPC 是大库卡顿主因);
// 文案/封面文字只留三态 + ~100 字摘要,图集只留首图与张数。
// 需要全文的场景(详情弹窗 / 导出 Excel / 对话插入文案)走 getContentDetail / listContentsFull。
export interface ContentListView {
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
  // 图集首图 URL(封面缺失时的回退图源;替代旧 imageUrls[0] 用法)
  firstImageUrl: string | null;
  // 图集图片张数(瀑布流「N 图」角标等;替代旧 imageUrls.length 用法)
  imageCount: number;
  xsecToken?: string | null;
  duration: number | null;
  topics: string[];
  owner: string;
  collectedAt: number;
  // 素材下载状态:pending(待处理)/success(成功)/failed(失败);null=旧数据未跑过下载
  mediaStatus: "pending" | "success" | "failed" | null;
  audioExtracted: boolean | null;
  mediaError: string | null;
  coverPath: string | null;
  avatarPath: string | null;
  audioPath: string | null;
  // 转写三态:none=未转写/转写失败(可重试),empty=已转写但未识别到语音(空文案标记),has=有文案;
  // 语义与 ContentView.transcript 的 null/空串/非空 一一对应
  transcriptState: "none" | "empty" | "has";
  // 文案摘要(仅 has 时有值,超长截断补 …);全文走详情/导出接口
  transcriptPreview: string | null;
  transcriptError: string | null;
  // 封面 OCR 三态(口径同 transcriptState)
  coverOcrState: "none" | "empty" | "has";
  coverOcrPreview: string | null;
  coverOcrError: string | null;
  // 细粒度处理状态:视频下载 / 图文图片进度 / 评论采集 / 意向分析
  videoDownloaded: boolean | null;
  imageTotal: number | null;
  imageDone: number | null;
  commentCollected: boolean | null;
  intentAnalyzed: boolean | null;
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
  // 作者头像本地路径(相对 mediaRoot;未下载过为 null,前端本地优先、缺失回退 CDN)
  avatarPath: string | null;
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
  // 最新转写文本 / 失败原因(音频重试成功会顺带补转写,转写重试会更新两者);
  // transcript 空串 "" = 已转写但未识别到语音(空文案标记)
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

// 内容详情评论栏分页响应(对应后端 CommentPageView;按点赞数倒序游标分页)
export interface CommentPageView {
  items: CommentView[];
  // 该内容下评论总数(标题展示;与分页无关)
  total: number;
  // 下一页游标;null = 已到底(隐藏「加载更多」)
  nextCursor: string | null;
}

// 全量库内容分页查询参数(对应后端 ContentListQuery)。ids 模式(批量视图)忽略 limit/offset/sort。
export interface ContentListQuery {
  taskId?: string | null;
  keyword?: string | null;
  runStart?: number | null;
  runEnd?: number | null;
  search?: string | null;
  platform?: string | null;
  kinds?: string[];
  industry?: string | null;
  createdFrom?: number | null;
  createdTo?: number | null;
  publishedFrom?: number | null;
  publishedTo?: number | null;
  // image=本地封面路径非空(选择器口径);cover=本地/远程封面任一(内容库封面图源)
  imageSource?: "image" | "cover" | null;
  requireTranscript?: boolean | null;
  // 内容库额外展示小红书图文;正文来自详情数据,不要求语音转写
  includeXhsImages?: boolean | null;
  sortBy?: "collectedAt" | "publishedAt" | "mediaStatus" | null;
  sortDir?: "asc" | "desc" | null;
  limit: number;
  offset: number;
  ids?: string[] | null;
}

// 评论库分页查询参数(对应后端 CommentListQuery)
export interface CommentListQuery {
  taskId?: string | null;
  search?: string | null;
  platform?: string | null;
  kinds?: string[];
  industry?: string | null;
  intentLevels?: Array<"high" | "medium" | "low" | "none" | "unanalyzed">;
  createdFrom?: number | null;
  createdTo?: number | null;
  sortBy?: "collectedAt" | "createdAt" | "likeCount" | "intent" | null;
  sortDir?: "asc" | "desc" | null;
  limit: number;
  offset: number;
}

// 评论来源分组(评论库瀑布流;对应后端 CommentSourceGroup):
// 来源元信息由组内评论行的 content* 字段携带,comments 为后端截好的预览(点赞倒序)
export interface CommentSourceGroup {
  platform: string;
  contentId: string;
  // 当前筛选口径下该来源的评论总数
  commentCount: number;
  comments: CommentView[];
}

export interface CommentSourcePageResult {
  items: CommentSourceGroup[];
  // 来源总数(distinct platform + contentId)
  total: number;
}

// 分页列表返回包:条目 + 同筛选口径总数
export interface ContentListResult {
  items: ContentListView[];
  total: number;
}
export interface CommentListResult {
  items: CommentView[];
  total: number;
}

// 全量库「待转写 / 待提取评论」计数
export interface ContentLibraryStats {
  untranscribed: number;
  pendingComment: number;
  // 音频采集失败/缺失的视频数(素材失败且无音频文件),对应「采集音频」批量按钮
  missingAudio: number;
}

export interface IndustryCount {
  industry: string;
  count: number;
}

// 侧栏行业角标聚合结果(后端 content/comment_industry_counts):
// total = 「全部行业」总数(忽略行业筛选、含无行业内容),选中行业后不再随列表 total 变化
export interface IndustryCounts {
  total: number;
  industries: IndustryCount[];
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

// 单次运行导出数据(对应后端 list_run_data):运行时间窗内落库的内容 + 评论
export interface RunDataView {
  contents: ContentView[];
  comments: CommentView[];
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

// 可录制的显示器(对应后端 ScreenInfo)
export interface ScreenInfo {
  // 枚举下标(开始录制时按它回选)
  index: number;
  // 系统显示器名,仅展示参考
  name: string | null;
  width: number;
  height: number;
  // 是否主屏
  primary: boolean;
}

// 单屏预览缩略图(对应后端 ScreenPreview,平铺选屏用)
export interface ScreenPreview {
  index: number;
  // PNG 的 base64 data URL;空串 = 编码失败,前端按「无预览」占位
  dataUrl: string;
}

// 音频输入设备(对应后端 AudioDeviceInfo,录屏设置面板的音频选择器用)
export interface AudioDeviceInfo {
  // dshow 设备名(开始录制 / 音频测试按它回选)
  name: string;
  // 是否评分最高的推荐设备
  recommended: boolean;
}

// 屏幕录制状态(对应后端 RecordingStatus)
export interface RecordingStatus {
  // 是否正在录制
  recording: boolean;
  // 本次录制的屏幕下标;null = 全部屏幕
  screenIndex: number | null;
  // 是否在采麦克风(录制中展示用;启动后不可改)
  withMic: boolean;
  // 开始时间(Unix 秒);未录制为 null
  startedAt: number | null;
  // 输出 MP4 路径;未录制为 null
  outputPath: string | null;
  // 是否处于暂停态(分段已收尾,等「继续」起新段)
  paused: boolean;
  // 已录制的活跃秒数(不含暂停时段);悬浮窗计时以此为基准本地走秒
  elapsedSecs: number;
}

// ===================== 创作 =====================

// 视频剪辑片段(秒;对应后端 ClipSegment)。position = 时间轴(序列)位置,
// 音轨混音按它做 adelay 定位;缺省时后端回退按顺序串接
export interface ClipSegment {
  start: number;
  end: number;
  position?: number;
  // 片段自己的视频或音频素材路径;为空时使用工程主视频。
  inputPath?: string;
  // 视频片段画面变换;音频片段忽略该字段。
  transform?: VideoTransformInput;
  // 导出时仅关闭该视频片段的原声,用于多视频轨按轨静音。
  muteOriginal?: boolean;
  // 视频合成层级;数值越大越靠上。音频片段忽略。
  layer?: number;
  // 相对片段起点的画面关键帧；后端在相邻点之间线性插值。
  keyframes?: VideoKeyframeInput[];
  // 片段级混音参数。音量允许 0~2 倍，声像 -1=左 / 1=右；淡入淡出单位为秒。
  volume?: number;
  pan?: number;
  fadeIn?: number;
  fadeOut?: number;
  // 片段恒定播放速度，0.25~4；时间线时长 = 源区间时长 / speed。
  speed?: number;
  // 速度曲线节点：offset 为相对源片段起点的源时间秒数。
  speedCurve?: SpeedPointInput[];
  // 进入该片段时使用的独立转场；kind=none 可覆盖工程级默认转场。
  transitionIn?: TransitionInput;
}

export interface SpeedPointInput {
  id: string;
  offset: number;
  speed: number;
}

export interface VideoKeyframeInput {
  id: string;
  offset: number;
  scale: number;
  positionX: number;
  positionY: number;
  // 到达该关键帧前一段动画的缓动方式。
  easing?: "linear" | "easeIn" | "easeOut" | "easeInOut";
}

export interface VideoTransformInput {
  rotation?: 0 | 90 | 180 | 270;
  scale?: number;
  positionX?: number;
  positionY?: number;
  cropTop?: number;
  cropRight?: number;
  cropBottom?: number;
  cropLeft?: number;
  // 画中画透明度,0~1。
  opacity?: number;
  // 基础色彩调节。brightness/temperature 为 -1~1，其余为倍率。
  brightness?: number;
  contrast?: number;
  saturation?: number;
  temperature?: number;
  hue?: number;
  filter?: "none" | "vivid" | "cinema" | "warm" | "cool" | "mono";
}

// 剪辑轨道类型(对齐专业 NLE:视频 / 音频 / 字幕)
export type TrackType = "video" | "audio" | "text";

// 视频元信息(后端 ffmpeg -i 解析;<video> 元素拿不到的帧率 / 码率 / 编码走这里)
export interface VideoInfo {
  durationSecs: number;
  width: number;
  height: number;
  fps: number;
  videoCodec: string;
  audioCodec: string;
  bitrateKbps: number;
}

// 片段转场参数:none 显式覆盖全局转场为硬切；不传则继承工程全局设置。
export interface TransitionInput {
  kind: "none" | "dissolve" | "fade";
  durationSecs: number;
}

// 导出画质档位(后端映射 CRF:high=18 / medium=21 / low=27;流拷贝路径不适用)
export type ExportQuality = "high" | "medium" | "low";

// 导出分辨率档位:目标为「短边」像素,仅在源短边大于目标时缩放(不放大)
export type ExportResolution = "original" | "1080p" | "720p" | "480p";

// 后端 FFmpeg 真实处理进度事件；jobId 用于过滤同窗口或并发任务。
export interface CreationExportProgress {
  jobId: string;
  percent: number;
  stage: string;
  status: "queued" | "running" | "cancelling" | "completed" | "failed" | "cancelled";
  outputPath?: string;
  error?: string;
}

// DeepSeek-V4.1-Flash 只返回真实候选镜头的编号和策划理由；时间码由本地场景检测提供。
export interface AiVideoPlanView {
  selectedIndices: number[];
  title: string;
  rationale: string;
  model: string;
}

// 轨道上的片段:start/end 为源视频内区间;position 为时间轴(序列)位置(秒)——
// 缺省时视为等于 start(旧草稿迁移口径),拖动拼接 / 自动拼接改变的是 position;
// detachedFrom = 音轨分离来源视频片段 id(「合并音频」据此找回归属)
export interface TrackClip extends ClipSegment {
  id: string;
  // 多轨片段编组；同组片段共同选择、移动和删除。
  groupId?: string;
  position?: number;
  detachedFrom?: string;
  // 波形按源素材总时长换算;旧草稿缺省时回退工程主视频时长。
  sourceDuration?: number;
  // 字幕轨专用样式;position + 片段时长决定成片中的显示区间。
  text?: string;
  fontSize?: number;
  textColor?: string;
  textPosition?: "top" | "center" | "bottom";
  // 文字中心点在画布中的百分比坐标，以及顺时针旋转角度。
  textX?: number;
  textY?: number;
  textRotation?: number;
}

export interface TextOverlay {
  start: number;
  end: number;
  text: string;
  fontSize?: number;
  color?: string;
  position?: "top" | "center" | "bottom";
  x?: number;
  y?: number;
  rotation?: number;
}

// 剪辑轨道(草稿持久化用,前端模型,无后端表)
export interface EditorTrack {
  id: string;
  type: TrackType;
  name: string;
  clips: TrackClip[];
  // 轨道开关(随草稿持久化):锁定=禁增删片段/删轨;隐藏=不参与导出;静音=音频轨不混音
  locked?: boolean;
  hidden?: boolean;
  muted?: boolean;
}

// 剪辑历史条目(对应后端 ExportItem;导出目录扫描,文件即记录)
export interface ExportItem {
  // 文件名(clip-<时间戳>.mp4)
  name: string;
  // 绝对路径
  path: string;
  // 文件大小(字节)
  size: number;
  // 导出时间(Unix 秒)
  createdAt: number;
  // 成片媒体信息；存量文件探测失败时为 0。
  durationSecs: number;
  width: number;
  height: number;
}

// ===================== 发布服务 =====================

// 发布平台清单(对应后端 list_publish_platforms):独立于采集平台配置,只返回启用的平台
export interface PublishPlatformView {
  id: string;
  // 发布侧展示名(如「视频号助手」「小红书创作者平台」)
  name: string;
  loginUrl: string;
  enabled: boolean;
}

// 发布账号的分组(客户):直接复用 CRM 客户(运营 > 客户管理),展示名称 + 编码
export interface PublishCustomerView {
  id: string;
  name: string;
  // 客户编码(如 CUS-XXXX)
  code: string;
  // 该客户下挂的发布账号数
  accountCount: number;
  createdAt: number;
}

// 发布账号(对应后端 PublishAccountView)
export interface PublishAccountView {
  id: string;
  platform: string;
  // 所属客户(customers.id)
  categoryId: string | null;
  label: string;
  nickname: string;
  avatar: string;
  uid: string;
  // active 正常 / invalid 失效 / limited 受限 / disabled 停用
  status: "active" | "invalid" | "limited" | "disabled" | string;
  // 失效 / 受限原因(空串 = 无)
  failReason: string;
  code: string;
  lastLoginAt: number | null;
  lastPublishAt: number | null;
  // 今日已发布条数
  todayPublished: number;
  createdAt: number;
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
