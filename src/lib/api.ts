// Tauri IPC 命令的前端封装(api 对象);数据类型(DTO)定义见 api-types.ts,本文件一并再导出供各页面复用。
import { invoke } from "@tauri-apps/api/core";
import { sortByPlatform } from "@/lib/platforms";
import type { PlatformConfig, AccountView, CollectResult, AppConfig, AccountInput, UserView, UserInput, ProviderDto, RoleModelConfig, ConversationView, ChatAttachment, ChatMessageView, CheckpointView, CheckpointDiffView, NetworkEntryView, DevServerStatus, SandboxConfigView, SandboxStatsView, SandboxConfigInput, ChatMemoryView, EmbeddingConfigView, PromptDto, CustomerView, CustomerInput, IndustryView, IndustryInput, KeywordDto, TaskView, TaskInput, TaskStatusPatch, AuthorView, EnrichSummary, RecollectCommentsSummary, ContentDetailView, MediaStatusView, CommentView, TaskRunView, RunDataView, CollectLogEntry, DashboardOverview, CloudConfigView, CloudConnectionState, CloudPairView, RecordingStatus, BillingOverview, ContentListQuery, CommentListQuery, ContentListResult, CommentListResult, ContentLibraryStats, IndustryCount, TableMigrationView } from "./api-types";
export * from "./api-types";

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
  // 复制局域网远程连接串(仅 PG):需当前用户密码二次校验,主机替换为本机局域网 IP
  getRemoteDatabaseUrl: (password: string) =>
    invoke<string>("get_remote_database_url", { password }),
  // SQLite → PG 一键迁移:目标自动建表,幂等(已存在的行跳过),源库只读
  migrateSqliteToPg: (targetUrl: string) =>
    invoke<TableMigrationView[]>("migrate_sqlite_to_pg", { targetUrl }),
  setStoragePath: (path: string) =>
    invoke<void>("set_storage_path", { path }),
  // 智能体可编辑附加规范(kind: coding/computer/rpa);下一轮对话注入生效,无需重启
  getAgentGuidelines: (kind: string) =>
    invoke<string>("get_agent_guidelines", { kind }),
  setAgentGuidelines: (kind: string, text: string) =>
    invoke<void>("set_agent_guidelines", { kind, text }),
  // 保存语音转写配置(系统设置「语音转写」)
  setTranscriptionConfig: (provider: string, apiUrl: string, model: string, apiKey: string, concurrency: number) =>
    invoke<void>("set_transcription_config", { provider, apiUrl, model, apiKey, concurrency }),
  // 保存海外平台音频拉流代理(系统设置「网络代理」):空=自动探测、"off"=关闭、其余=代理 URL
  setMediaProxy: (proxy: string) => invoke<void>("set_media_proxy", { proxy }),
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
  // 角色模型:读取 / 保存杂活角色的便宜模型映射(空值=回退会话模型)
  getRoleModels: () => invoke<RoleModelConfig>("get_role_models"),
  setRoleModels: (config: RoleModelConfig) =>
    invoke<void>("set_role_models", { config }),
  // 保存意向分析配置(系统设置「意向分析」)
  setIntentConfig: (
    apiUrl: string,
    model: string,
    intentPrompt: string,
    batchSize: number,
    apiKey: string,
  ) =>
    invoke<void>("set_intent_config", {
      apiUrl,
      model,
      intentPrompt,
      batchSize,
      apiKey,
    }),
  saveTextFile: (path: string, content: string) =>
    invoke<void>("save_text_file", { path, content }),
  saveBinaryFile: (path: string, contentBase64: string) =>
    invoke<void>("save_binary_file", { path, contentBase64 }),
  // 保存对话框 + 写文件:返回本地绝对路径(已保存)/ null(用户取消)
  saveTextDialog: (content: string, fileName: string) =>
    invoke<string | null>("save_text_dialog", { content, fileName }),
  saveBinaryDialog: (contentBase64: string, fileName: string) =>
    invoke<string | null>("save_binary_dialog", { contentBase64, fileName }),
  // 下载历史:查文件是否仍存在 + 打开文件所在目录(存在则选中)
  pathExists: (path: string) => invoke<boolean>("path_exists", { path }),
  revealPath: (path: string) => invoke<void>("reveal_path", { path }),
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

  // 个人中心:当前登录用户自助修改密码 / 资料
  changePassword: (oldPassword: string, newPassword: string) =>
    invoke<void>("change_password", { oldPassword, newPassword }),
  updateProfile: (nickname: string, email: string, avatar: string, remark: string) =>
    invoke<UserView>("update_profile", { nickname, email, avatar, remark }),

  // AI 对话
  listConversations: () =>
    invoke<ConversationView[]>("list_conversations"),
  createConversation: (
    id: string,
    providerId: string,
    model: string,
    agentType?: string,
  ) =>
    invoke<ConversationView>("create_conversation", {
      id,
      providerId,
      model,
      agentType: agentType ?? null,
    }),
  renameConversation: (id: string, title: string) =>
    invoke<void>("rename_conversation", { id, title }),
  archiveConversation: (id: string, archived: boolean) =>
    invoke<void>("archive_conversation", { id, archived }),
  updateConversationModel: (id: string, providerId: string, model: string) =>
    invoke<ConversationView>("update_conversation_model", {
      id,
      providerId,
      model,
    }),
  deleteConversation: (id: string) =>
    invoke<void>("delete_conversation", { id }),
  // 本会话滚动摘要(「本会话记忆」查看/编辑)
  getConversationSummary: (conversationId: string) =>
    invoke<string>("get_conversation_summary", { conversationId }),
  updateConversationSummary: (conversationId: string, summary: string) =>
    invoke<void>("update_conversation_summary", { conversationId, summary }),
  // 编程 Agent:发送消息(驱动 ReAct 循环,过程经 "agent-step" 事件推进度,resolve 返回最终回复)
  // mode:plan(只调研出方案)/ act(亲自动手执行),缺省 act
  sendCodingMessage: (conversationId: string, content: string, mode?: string) =>
    invoke<ChatMessageView>("send_coding_message", { conversationId, content, mode }),
  // 编程 Agent:请求停止该会话正在自主续航的循环(下一步检查点优雅收尾)
  stopCodingAgent: (conversationId: string) =>
    invoke<void>("stop_coding_agent", { conversationId }),
  // 对话 Agent:请求停止该会话正在进行的流式输出
  stopChatAgent: (conversationId: string) =>
    invoke<void>("stop_chat_agent", { conversationId }),
  // 更新消息反馈(点赞/点踩)
  updateMessageFeedback: (messageId: number, feedback: string | null) =>
    invoke<void>("update_message_feedback", { messageId, feedback }),
  // 浏览器 / RPA Agent:驱动 navigate/click/type/read_page/wait_for/get_network 的 ReAct 循环(动作可回读结果)
  sendBrowserMessage: (conversationId: string, content: string) =>
    invoke<ChatMessageView>("send_browser_message", { conversationId, content }),
  // 电脑操作 Agent(GUI:desktop/ocr/uia 看屏 + 鼠标键盘 + 控件)的 ReAct 循环
  sendComputerMessage: (conversationId: string, content: string) =>
    invoke<ChatMessageView>("send_computer_message", { conversationId, content }),
  // 本机助手 Agent(fs/system/shell:文件 / 进程 / 终端)的 ReAct 循环;危险确认复用 resolveAgentConfirm
  sendLocalMessage: (conversationId: string, content: string) =>
    invoke<ChatMessageView>("send_local_message", { conversationId, content }),
  // 统一编排器(默认对话):需要时把 coding/rpa/computer/local 当工具委派,子智能体结果回灌本会话
  sendOrchestratorMessage: (conversationId: string, content: string) =>
    invoke<ChatMessageView>("send_orchestrator_message", { conversationId, content }),
  // 危险操作确认回执:approved=true 放行后端执行,false 拒绝(后端把「已拒绝」回灌模型)
  resolveAgentConfirm: (confirmId: number, approved: boolean) =>
    invoke<void>("resolve_agent_confirm", { confirmId, approved }),
  // 截当前桌面屏幕,返回 PNG 的 data URL(电脑操作 Agent 右栏预览用);target 空=主屏
  captureDesktopScreenshot: (target?: string) =>
    invoke<string>("capture_desktop_screenshot", { target: target ?? null }),
  // 屏幕录制:打开悬浮条(不录制)/ 取消 / 开始(最小化主窗口 + ffmpeg 录全屏)/ 停止 / 查状态
  openRecordingOverlay: () => invoke<void>("open_recording_overlay"),
  cancelRecordingOverlay: () => invoke<void>("cancel_recording_overlay"),
  // 仅录视频(不录音频)
  startScreenRecording: () =>
    invoke<RecordingStatus>("start_screen_recording"),
  stopScreenRecording: () => invoke<RecordingStatus>("stop_screen_recording"),
  getRecordingStatus: () => invoke<RecordingStatus>("get_recording_status"),
  // 录屏停止后:把视频以附件方式加入指定对话(引用本地路径),返回新消息
  attachRecordingMessage: (conversationId: string, path: string) =>
    invoke<ChatMessageView>("attach_recording_message", { conversationId, path }),
  // 按右栏 DOM 区域(逻辑坐标,相对主窗口客户区)定位内嵌 Agent webview;未创建则静默忽略
  setAgentWebviewBounds: (
    conversationId: string,
    x: number,
    y: number,
    width: number,
    height: number,
  ) =>
    invoke<void>("set_agent_webview_bounds", { conversationId, x, y, width, height }),
  // 显示某会话的内嵌 Agent webview(进入/返回 RPA 页时)
  showAgentWebview: (conversationId: string) =>
    invoke<void>("show_agent_webview", { conversationId }),
  // 隐藏某会话的内嵌 Agent webview(切会话/弹模态时)
  hideAgentWebview: (conversationId: string) =>
    invoke<void>("hide_agent_webview", { conversationId }),
  // 隐藏全部内嵌 Agent webview(离开 RPA 工作区时,防原生层盖住其它页面)
  hideAllAgentWebviews: () => invoke<void>("hide_all_agent_webviews"),
  // 读取该会话拦截到的接口响应(实时增量另走 agent-network 事件);可选 url 子串过滤
  getAgentNetwork: (conversationId: string, urlContains?: string) =>
    invoke<NetworkEntryView[]>("get_agent_network", {
      conversationId,
      urlContains: urlContains ?? null,
    }),
  // 工作区路径:传 conversationId 返回该会话目录,否则返回根目录
  getCodingWorkspace: (conversationId?: string) =>
    invoke<string>("get_coding_workspace", {
      conversationId: conversationId ?? null,
    }),
  setCodingWorkspace: (path: string) =>
    invoke<void>("set_coding_workspace", { path }),
  // 编程 Agent:开发服务器(常驻进程,预览-开发服务器模式)
  startDevServer: (conversationId: string, command: string) =>
    invoke<void>("start_dev_server", { conversationId, command }),
  stopDevServer: () => invoke<void>("stop_dev_server"),
  getDevServerStatus: () =>
    invoke<DevServerStatus>("get_dev_server_status"),
  // 用户在终端直接执行一条命令(该会话工作区 / 沙盒内,超时),返回输出文本
  runWorkspaceCommand: (conversationId: string, command: string) =>
    invoke<string>("run_workspace_command", { conversationId, command }),
  // 回退:丢弃本轮 Agent 的文件改动,回到最近检查点(发送前状态)
  checkpointRollback: (conversationId: string) =>
    invoke<string>("checkpoint_rollback", { conversationId }),
  // 版本管理:列出检查点历史 / 取某版本改动详情 / 回退到指定版本(git reset)
  listCodingCheckpoints: (conversationId: string) =>
    invoke<CheckpointView[]>("list_coding_checkpoints", { conversationId }),
  getCheckpointDiff: (conversationId: string, hash: string) =>
    invoke<CheckpointDiffView>("get_checkpoint_diff", { conversationId, hash }),
  rollbackToCheckpoint: (conversationId: string, hash: string) =>
    invoke<string>("rollback_to_checkpoint", { conversationId, hash }),
  // 文件面板:列出工作区真实文件树 / 读取某文件内容
  listWorkspaceFiles: (conversationId: string) =>
    invoke<string[]>("list_workspace_files", { conversationId }),
  readWorkspaceFile: (conversationId: string, path: string) =>
    invoke<string>("read_workspace_file", { conversationId, path }),
  writeWorkspaceFile: (conversationId: string, path: string, content: string) =>
    invoke<void>("write_workspace_file", { conversationId, path, content }),
  // 编程沙盒(本地进程沙盒:Job Object / 进程组)状态与生命周期
  getSandboxConfig: () => invoke<SandboxConfigView>("get_sandbox_config"),
  // 沙盒资源占用(Job 会计:累计 CPU / 峰值内存 / 存活进程数 / 存储占用)
  getSandboxStats: () => invoke<SandboxStatsView>("get_sandbox_stats"),
  // 沙盒限额配置(内存/网络/CPU/进程数/IO/空闲回收,单对象参数;0=不限/关闭);
  // 改了会 terminate 现存沙盒,下次动作按新配置惰性重建
  setSandboxConfig: (config: SandboxConfigInput) =>
    invoke<void>("set_sandbox_config", { config }),
  // 某沙盒的命令审计日志(尾部 limit 条,JSON 行;无记录返回空)
  getSandboxAudit: (sandboxId: string, limit?: number) =>
    invoke<string[]>("get_sandbox_audit", { sandboxId, limit: limit ?? 50 }),
  // 清空该沙盒的存储目录(会删工作区全部文件;先杀进程,带根目录护栏)
  clearSandboxStorage: (sandboxId: string) =>
    invoke<void>("clear_sandbox_storage", { sandboxId }),
  // 停止全部沙盒进程(terminate 所有会话 Job;工作区文件保留,下次编程动作自动重建)
  sandboxStop: () => invoke<void>("sandbox_stop"),
  // 意图分类:首条消息判断该用哪个 Agent(chat/coding/rpa/computer/local),用于发送时自动切布局。
  // 混合策略:关键词命中直接返回(零延迟);仅关键词落到 chat 且像可执行任务时,后端才用所选模型做一次 LLM 兜底。
  classifyAgentType: (text: string, providerId?: string, model?: string) =>
    invoke<string>("classify_agent_type", {
      text,
      providerId: providerId ?? null,
      model: model ?? null,
    }),
  // 意图路由遥测:最近的路由决策(关键词命中 / 是否走 LLM 兜底 / 最终路由),供分析误路由
  listAgentRouteLogs: (limit?: number) =>
    invoke<
      {
        id: number;
        text: string;
        keywordRoute: string;
        llmUsed: boolean;
        llmRoute: string;
        finalRoute: string;
        model: string;
        owner: string;
        createdAt: number;
      }[]
    >("list_agent_route_logs", { limit: limit ?? null }),
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
  // 实时语音转写:定时发送音频片段(流式:增量经 asr-stream 事件返回,invoke 返回完整文本)
  transcribeAudioChunk: (audioBase64: string, format: string, requestId: string) =>
    invoke<string>("transcribe_audio_chunk", { audioBase64, format, requestId }),

  // AI 对话:长期记忆(跨会话,设置页「AI 记忆」管理)
  listChatMemories: () => invoke<ChatMemoryView[]>("list_chat_memories"),
  addChatMemory: (content: string, memType?: string, importance?: number) =>
    invoke<ChatMemoryView>("add_chat_memory", {
      content,
      memType: memType ?? null,
      importance: importance ?? null,
    }),
  updateChatMemory: (
    id: number,
    content: string,
    enabled: boolean,
    memType?: string,
    importance?: number,
  ) =>
    invoke<void>("update_chat_memory", {
      id,
      content,
      enabled,
      memType: memType ?? null,
      importance: importance ?? null,
    }),
  deleteChatMemory: (id: number) =>
    invoke<void>("delete_chat_memory", { id }),
  clearChatMemories: () => invoke<void>("clear_chat_memories"),
  getChatMemoryEnabled: () => invoke<boolean>("get_chat_memory_enabled"),
  setChatMemoryEnabled: (enabled: boolean) =>
    invoke<void>("set_chat_memory_enabled", { enabled }),
  // 置顶 / 取消置顶:置顶记忆每轮恒注入,不参与相似度淘汰
  setChatMemoryPinned: (id: number, pinned: boolean) =>
    invoke<void>("set_chat_memory_pinned", { id, pinned }),
  // 长期记忆的语义检索(embedding)配置:读取(apiKey 只回 hasApiKey)/ 保存(apiKey 留空=不改)
  getEmbeddingConfig: () =>
    invoke<EmbeddingConfigView>("get_embedding_config"),
  setEmbeddingConfig: (apiUrl: string, model: string, apiKey: string) =>
    invoke<void>("set_embedding_config", { apiUrl, model, apiKey }),
  // 把某条已采集内容(资产)的本地视觉素材读成 base64 附件。
  // coverOnly=true(图源=封面)只取封面;否则图文图片优先、无则退回封面。
  // indices 给出时仅取这些「本地图片排序位置」(逐张挑选用)
  buildContentAttachments: (
    contentId: string,
    coverOnly: boolean,
    indices?: number[],
  ) =>
    invoke<ChatAttachment[]>("build_content_attachments", {
      contentId,
      coverOnly,
      indices: indices ?? null,
    }),

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
  // 终止任务:登记停止标记,运行中的采集滚动 / 素材下载 / 语音转写各阶段据此中断
  // (仅改 DB 状态不会停运行体,必须显式登记;任务未在运行时登记无副作用)
  stopTask: (taskId: string) => invoke<void>("stop_collect", { taskId }),
  // 取消全量库批量提取(提取文案 / 提取评论):登记停止标记,后端逐条/逐批循环检查,
  // 已完成条目保留,未处理的不再继续(转写批间生效,评论在「下一个视频」前生效)
  cancelLibraryExtract: () => invoke<void>("cancel_library_extract"),
  // 全量库分页列表:筛选/排序下沉 SQL,limit/offset + total
  listContentsPage: (query: ContentListQuery) =>
    invoke<ContentListResult>("list_contents_page", { query }),
  // 评论库分页列表(同上)
  listCommentsPage: (query: CommentListQuery) =>
    invoke<CommentListResult>("list_comments_page", { query }),
  // 全量库「待转写 / 待提取评论」计数(与当前筛选口径一致)
  contentLibraryStats: (query: ContentListQuery) =>
    invoke<ContentLibraryStats>("content_library_stats", { query }),
  // 批量处理的目标内容 id 快照(点击瞬间与当前筛选口径一致)
  listBatchContentIds: (query: ContentListQuery, batch: "transcript" | "comments" | "audio") =>
    invoke<string[]>("list_batch_content_ids", { query, batch }),
  // 侧栏行业角标计数(忽略行业筛选自身,其余筛选同列表口径)
  contentIndustryCounts: (query: ContentListQuery) =>
    invoke<IndustryCount[]>("content_industry_counts", { query }),
  commentIndustryCounts: (query: CommentListQuery) =>
    invoke<IndustryCount[]>("comment_industry_counts", { query }),
  // 单条内容的评论列表(全量库详情右侧评论栏,按点赞倒序)
  listContentComments: (contentId: string) =>
    invoke<CommentView[]>("list_content_comments", { contentId }),
  // 采集日志:加载某任务的历史日志(任务详情页打开时回显,再接实时事件)
  listCollectLogs: (taskId: string) =>
    invoke<CollectLogEntry[]>("list_collect_logs", { taskId }),
  // 任务执行历史(每次运行一条)+ 某次运行的采集日志(按运行时间范围切分)
  listTaskRuns: (taskId: string) =>
    invoke<TaskRunView[]>("list_task_runs", { taskId }),
  listRunLogs: (runId: string) =>
    invoke<CollectLogEntry[]>("list_run_logs", { runId }),
  // 单次运行采集到的内容 + 评论(执行历史导出 Excel 用,按运行时间窗切分)
  listRunData: (runId: string) =>
    invoke<RunDataView>("list_run_data", { runId }),
  // 任务全部采集内容 + 评论(任务调度「更多 → 导出」Excel 用)
  listTaskData: (taskId: string) =>
    invoke<RunDataView>("list_task_data", { taskId }),
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
  // 失败重试:重跑单条内容的素材下载(注意:视频直链可能已过期,仍会 403);
  // 音频提取重试成功且尚无文案时会顺带补一次语音转写
  retryContentMedia: (id: string) =>
    invoke<MediaStatusView>("retry_content_media", { id }),
  // 文案转写失败重试:对已有音频的单条内容重跑语音转写
  retryContentTranscript: (id: string) =>
    invoke<MediaStatusView>("retry_content_transcript", { id }),
  // 批量转写:对指定 id(当前筛选列表中「有音频无文案」的条目)重跑语音转写,返回处理条数
  retryFailedTranscripts: (ids: string[]) =>
    invoke<number>("retry_failed_transcripts", { ids }),
  // 批量采集音频:对指定 id(当前筛选列表中「缺音频」的视频)重跑视频下载+音频提取,
  // 直链过期的会开采集窗口刷新直链后再试;并发与采集主链路素材下载一致(15 路),返回处理条数
  batchCollectAudios: (ids: string[]) =>
    invoke<number>("batch_collect_audios", { ids }),
  // 全量库补采评论:对选中内容按评论参数(时间范围 / 单视频上限 / 意向分析)重采一级评论
  recollectComments: (
    ids: string[],
    params: {
      commentTimeRange: string;
      commentLimit: number;
      analyzeIntent: boolean;
    },
  ) =>
    invoke<RecollectCommentsSummary>("recollect_comments", {
      ids,
      commentTimeRange: params.commentTimeRange,
      commentLimit: params.commentLimit,
      analyzeIntent: params.analyzeIntent,
    }),
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
  // 作者库的黑名单开关(按作者 id):加入黑名单后采集排除其内容
  setAuthorBlacklistedById: (id: string, blacklisted: boolean) =>
    invoke<void>("set_author_blacklisted_by_id", { id, blacklisted }),
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

  // 账单计费
  billingOverview: (start?: number, end?: number) =>
    invoke<BillingOverview>("billing_overview", {
      start: start ?? null,
      end: end ?? null,
    }),
};

