//! 分层配置系统。
//!
//! 核心诉求(用户明确要求):平台可配置、可管理、可扩展。
//! 因此平台不是写死的常量,而是一张 `platforms` 表,从 JSON 文件加载,
//! 支持运行时增 / 删 / 改 / 启停,新增平台无需重新编译。

use crate::error::{CrawlerError, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// 配置文件名。放常量避免散落字符串。
const CONFIG_FILE_NAME: &str = "veltrix-config.json";

/// 默认每平台并发请求数,偏保守以降低风控概率。
const DEFAULT_CONCURRENCY: u32 = 2;
/// 默认请求最小间隔(毫秒)。
const DEFAULT_MIN_INTERVAL_MS: u64 = 1200;
/// 默认失败重试次数。
const DEFAULT_MAX_RETRIES: u32 = 3;
/// 默认滚动加载轮数:模拟下滑触发分页接口。
const DEFAULT_SCROLL_ROUNDS: u32 = 5;
/// 默认每轮滚动后的等待(毫秒),给接口返回与页面渲染留出时间。
const DEFAULT_SCROLL_INTERVAL_MS: u64 = 1500;
/// 默认等待节点出现的超时(毫秒);超时即判定该 RPA 步骤失败。
const DEFAULT_WAIT_TIMEOUT_MS: u64 = 8000;
/// 默认拟人滚动分段数:一次翻页拆成多段小幅滚动,比一次到底更接近真人。
const DEFAULT_SCROLL_SEGMENTS: u32 = 4;
/// 默认数据库连接池上限。
const DEFAULT_DB_MAX_CONNECTIONS: u32 = 8;

/// 单平台限速与退避策略。各平台风控强度不同,逐平台可调。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RateLimitConfig {
    /// 同时在途请求数上限。
    pub concurrency: u32,
    /// 相邻请求最小间隔(毫秒)。
    pub min_interval_ms: u64,
    /// 单任务最大重试次数。
    pub max_retries: u32,
}

impl Default for RateLimitConfig {
    fn default() -> Self {
        Self {
            concurrency: DEFAULT_CONCURRENCY,
            min_interval_ms: DEFAULT_MIN_INTERVAL_MS,
            max_retries: DEFAULT_MAX_RETRIES,
        }
    }
}

/// 采集配置:描述「如何用关键词驱动页面 + 拦截哪些接口」。
///
/// RPA + 拦截模式下不再逆向签名,而是让真实页面自己发请求,
/// 我们注入脚本劫持 fetch/XHR,命中 `intercept_patterns` 的响应回传后由适配器解析。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CollectConfig {
    /// 搜索结果页 URL 模板,用 `{keyword}` 占位。多数平台可直接 URL 进搜索页,省去填表单。
    pub search_url_template: String,
    /// 内容详情页 URL 模板,用 `{id}` 占位(评论采集导航用)。空串表示该平台暂不支持详情页评论采集。
    #[serde(default)]
    pub detail_url_template: String,
    /// 作者主页 URL 模板,用 `{id}` 占位(作者画像补采导航用),小红书另需 `{token}` 占位。
    /// 空串表示该平台不支持主页画像补采(如抖音/TikTok 搜索响应已含完整画像,无需补采)。
    #[serde(default)]
    pub profile_url_template: String,
    /// 需拦截的接口 URL 特征(子串匹配)。页面发出的 fetch/XHR 命中任一即回传 Rust。
    #[serde(default)]
    pub intercept_patterns: Vec<String>,
    /// 平台专属 RPA 脚本相对路径(相对资源目录);为空时用内置滚动加载逻辑。
    pub rpa_script: Option<String>,
    /// 滚动加载轮数(模拟下滑触发分页);0 表示用默认值。
    #[serde(default = "default_scroll_rounds")]
    pub scroll_rounds: u32,
    /// 每轮滚动后的等待毫秒。
    #[serde(default = "default_scroll_interval_ms")]
    pub scroll_interval_ms: u64,
    /// 拟人 RPA 步骤序列。非空时取代内置「改 URL + 盲滚」逻辑,改为节点级模拟操作
    /// (在搜索框逐字输入、点击、等待结果、分段滚动等),更贴近真人行为以降低风控。
    #[serde(default)]
    pub rpa_steps: Vec<RpaStep>,
    /// 排序方式追加到搜索 URL 的参数名(如抖音 sort_type);空则不加排序参数。
    #[serde(default)]
    pub sort_query_key: String,
    /// sort_mode(synthetic/hottest/latest)→ 该参数值映射。
    #[serde(default)]
    pub sort_query_map: BTreeMap<String, String>,
    /// 发布时间追加到搜索 URL 的参数名(如抖音 publish_time)。
    #[serde(default)]
    pub time_query_key: String,
    /// time_range(any/1d/1w/6m)→ 该参数值映射。
    #[serde(default)]
    pub time_query_map: BTreeMap<String, String>,
    /// 「下一页」按钮文案。分页型结果页(如 B站)滚动不触发翻页,
    /// 非空时每轮滚动后按文案点击翻页;按钮不存在/置灰时点击为空操作,由停滞判定兜底结束。
    #[serde(default)]
    pub next_page_texts: Vec<String>,
    /// 安全验证弹窗的 CSS 选择器(querySelector)。采集窗口注入自检脚本,命中任一可见元素即
    /// 判定「弹出验证」→ 暂停滚动、等用户手动完成、弹窗消失自动恢复。真实选择器需本机抓包核对
    /// (如抖音 `.captcha_verify_container`、小红书滑块容器),空=不检测。
    #[serde(default)]
    pub verify_selectors: Vec<String>,
    /// 安全验证弹窗的文案特征(节点 textContent 子串)。作为选择器的补充识别手段
    /// (如「请完成安全验证」「拖动滑块」「向右滑动」)。空=不按文案检测。
    #[serde(default)]
    pub verify_texts: Vec<String>,
}

/// 登录态真实检测配置。在可见登录窗口内注入自检脚本周期性判断是否真的登录成功:
/// 命中「已登录」特征 → 实时置账号 active(列表即时变绿);关窗时仍清晰处于「未登录」
/// (登录 CTA 仍在)→ 置 invalid;检测不确定则回退「关窗即视为已登录」的乐观行为(不误伤)。
///
/// ⚠️ 选择器 / 文案为开箱起点,真实页面结构需本机抓包核对后调整(同 search/rpa 待校准点)。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct LoginCheckConfig {
    /// 「已登录」DOM 特征选择器:命中任一即视为已登录(如顶栏用户头像)。
    #[serde(default)]
    pub logged_in_selectors: Vec<String>,
    /// 「未登录」按钮文案:页面存在可见、文本等于其一的可点元素即视为未登录(如「登录」)。
    #[serde(default)]
    pub logged_out_texts: Vec<String>,
    /// 非 httpOnly 的登录态 Cookie 名:document.cookie 含任一即视为已登录(辅助信号)。
    #[serde(default)]
    pub login_cookie_names: Vec<String>,
}

impl LoginCheckConfig {
    /// 是否配置了任何检测信号;全空时跳过真实检测,沿用乐观行为。
    pub fn is_enabled(&self) -> bool {
        !self.logged_in_selectors.is_empty()
            || !self.logged_out_texts.is_empty()
            || !self.login_cookie_names.is_empty()
    }
}

/// 各平台内置登录检测配置(开箱起点)。登录 CTA 文案在中文平台间通用,
/// 海外平台(TikTok / YouTube)追加英文文案;「已登录」头像选择器尽量宽松地
/// 匹配顶栏头像;均需本机核对。
fn builtin_login_check(platform_id: &str) -> LoginCheckConfig {
    // 顶栏头像类元素(各平台命名不一,广撒网;含 data-e2e 与 class 含 avatar)
    let mut logged_in_selectors: Vec<String> = [
        "[data-e2e=\"live-avatar\"]",
        "[data-e2e=\"user-info\"]",
        ".avatar img",
        ".user-avatar img",
        "img[class*=\"avatar\" i]",
        "[class*=\"avatar\" i] img",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    // 未登录时页面会出现明显的登录入口文案
    let mut logged_out_texts: Vec<String> = [
        "登录",
        "登录/注册",
        "立即登录",
        "扫码登录",
        "手机号登录",
        "登录抖音",
        "登录小红书",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    match platform_id {
        "tiktok" => {
            logged_out_texts.extend(["Log in", "Log in to TikTok"].map(String::from));
        }
        "youtube" => {
            // YouTube 顶栏已登录头像按钮有稳定 id
            logged_in_selectors.push("button#avatar-btn".into());
            logged_out_texts.extend(["Sign in", "登录"].map(String::from));
        }
        _ => {}
    }
    LoginCheckConfig {
        logged_in_selectors,
        logged_out_texts,
        // 默认不依赖 Cookie(关键登录态多为 httpOnly 不可读);需要时按本机抓包补充
        login_cookie_names: Vec::new(),
    }
}

fn default_scroll_rounds() -> u32 {
    DEFAULT_SCROLL_ROUNDS
}

fn default_scroll_interval_ms() -> u64 {
    DEFAULT_SCROLL_INTERVAL_MS
}

fn default_wait_timeout_ms() -> u64 {
    DEFAULT_WAIT_TIMEOUT_MS
}

fn default_scroll_segments() -> u32 {
    DEFAULT_SCROLL_SEGMENTS
}

/// 拟人 RPA 单步。声明式描述「在页面上做什么」,由注入脚本按拟人节奏解释执行。
/// `selector` / `text` 中的 `{keyword}` 占位在执行前由本次采集关键词替换。
///
/// 设计取向:节奏由「节点状态 + 随机化」驱动,而非固定计时——真人是看到元素才动作,
/// 盲等固定时长恰是风控最易识别的机器特征。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "camelCase")]
pub enum RpaStep {
    /// 轮询等待节点出现,超时即判定该步失败(页面结构变化或被风控拦截)。
    WaitFor {
        selector: String,
        #[serde(default = "default_wait_timeout_ms")]
        timeout_ms: u64,
    },
    /// 拟人点击:滚动到可见 → hover → 随机停顿 → 派发完整鼠标事件序列。
    Click { selector: String },
    /// 拟人逐字输入:聚焦后按随机节奏键入,用原生 setter 兼容 React 受控组件。
    Type { selector: String, text: String },
    /// 在节点上派发回车键,触发搜索提交。
    PressEnter { selector: String },
    /// 拟人分段滚动:分多段小幅下滑触发分页,每段距离/间隔随机,偶尔回滚,段间停顿。
    Scroll {
        #[serde(default = "default_scroll_segments")]
        segments: u32,
    },
    /// 随机停顿,模拟阅读 / 思考节奏。
    Pause { min_ms: u64, max_ms: u64 },
}

/// 各平台内置拟人 RPA 步骤(v0 联调起点)。
///
/// ⚠️ 选择器(搜索框 placeholder、结果项 class)均为**推测值**,需本机 `bun tauri dev`
/// 打开真实页面用 DevTools 校准后修正;校准前可能 `waitFor` 超时。未列平台返回空 = 走旧逻辑。
fn default_rpa_steps(platform_id: &str) -> Vec<RpaStep> {
    match platform_id {
        // 小红书:首页搜索框逐字输入 → 回车 → 等结果列表 → 分段滚动翻页
        "xhs" => vec![
            RpaStep::WaitFor {
                selector: "#search-input".into(),
                timeout_ms: 10_000,
            },
            RpaStep::Click {
                selector: "#search-input".into(),
            },
            RpaStep::Type {
                selector: "#search-input".into(),
                text: "{keyword}".into(),
            },
            RpaStep::Pause {
                min_ms: 400,
                max_ms: 900,
            },
            RpaStep::PressEnter {
                selector: "#search-input".into(),
            },
            RpaStep::WaitFor {
                // 搜索结果页特有的频道栏(全部/图文/视频/用户),首页没有,可靠判断「已进入结果页」
                selector: "#channel-container".into(),
                timeout_ms: 12_000,
            },
            // 作为最大滚动轮数上限;实际滚到底(内容不再增长)自动停
            RpaStep::Scroll { segments: 40 },
        ],
        _ => Vec::new(),
    }
}

/// 内置平台的搜索 URL 排序/时间参数映射(目前仅抖音 URL 直达支持;小红书走 RPA 不经此)。
/// 真实参数名/值需本机抓包核对(与 search_url / intercept_patterns 同属「骨架待抓包」)。
fn builtin_search_query(
    id: &str,
) -> (String, BTreeMap<String, String>, String, BTreeMap<String, String>) {
    let pair = |arr: &[(&str, &str)]| -> BTreeMap<String, String> {
        arr.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    };
    match id {
        // 抖音搜索 URL:sort_type 0综合/1最多点赞/2最新;publish_time 0不限/1一天/7一周/180半年
        "douyin" => (
            "sort_type".into(),
            pair(&[("synthetic", "0"), ("hottest", "1"), ("latest", "2")]),
            "publish_time".into(),
            pair(&[("any", "0"), ("1d", "1"), ("1w", "7"), ("6m", "180")]),
        ),
        // B站搜索 URL:order totalrank综合/click最多播放/pubdate最新发布;
        // 时间筛选走 pubtime_begin_s/end_s 绝对时间戳,静态映射表达不了,留空不支持
        "bilibili" => (
            "order".into(),
            pair(&[("synthetic", "totalrank"), ("hottest", "click"), ("latest", "pubdate")]),
            String::new(),
            BTreeMap::new(),
        ),
        _ => (String::new(), BTreeMap::new(), String::new(), BTreeMap::new()),
    }
}

/// 各平台「下一页」按钮文案(分页型结果页专用)。无限滚动平台返回空。
fn builtin_next_page_texts(id: &str) -> Vec<String> {
    match id {
        // B站搜索结果页是分页按钮而非无限滚动,滚动后需点「下一页」才请求下一页数据
        "bilibili" => vec!["下一页".into()],
        _ => Vec::new(),
    }
}

/// 各平台安全验证弹窗的检测特征 (选择器, 文案)。采集窗口据此识别「弹出验证」并暂停采集。
/// 这里是抓包起点骨架:选择器以各平台验证码容器常见类名为初值,真实值需本机
/// `bun run tauri dev` 触发风控后核对调整(代码与 search_url 等同属抓包依赖项)。
fn builtin_verify_signals(id: &str) -> (Vec<String>, Vec<String>) {
    match id {
        // 抖音 / TikTok:secsdk 验证码容器(滑块 / 点选)
        "douyin" | "tiktok" => (
            vec![
                ".captcha_verify_container".into(),
                "#captcha-verify-image".into(),
                ".captcha-verify-container".into(),
            ],
            vec!["完成安全验证".into(), "拖动下方滑块".into(), "向右滑动".into()],
        ),
        // 小红书:滑块验证容器
        "xhs" => (
            vec![".captcha-container".into(), ".red-captcha".into()],
            vec!["滑动验证".into(), "请完成安全验证".into(), "向右滑动".into()],
        ),
        // 快手:验证码弹层
        "kuaishou" => (
            vec![".captcha-dialog".into(), ".slide-verify".into()],
            vec!["拖动滑块".into(), "完成验证".into()],
        ),
        // 其余平台暂无骨架,留空(用户可在平台配置里补充后即时生效)
        _ => (Vec::new(), Vec::new()),
    }
}

/// 各平台作者主页 URL 模板(画像补采导航用),`{id}`=作者 uid,`{token}`=鉴权 token。
/// 仅「搜索响应缺画像」的平台需要:抖音/TikTok 搜索已含完整画像,返回空表示不支持补采。
fn builtin_profile_url(id: &str) -> &'static str {
    match id {
        // 小红书主页需 xsec_token(由最近一条该作者内容的 author_xsec_token 提供)
        "xhs" => "https://www.xiaohongshu.com/user/profile/{id}?xsec_token={token}&xsec_source=pc_search",
        "kuaishou" => "https://www.kuaishou.com/profile/{id}",
        "bilibili" => "https://space.bilibili.com/{id}",
        "youtube" => "https://www.youtube.com/channel/{id}",
        _ => "",
    }
}

/// 各平台作者主页画像接口拦截特征(并入该平台 intercept_patterns)。
/// 主页自己请求的用户信息接口,命中后由适配器 parse_profile 解析为画像。
fn builtin_profile_patterns(id: &str) -> Vec<&'static str> {
    match id {
        // 小红书:主页加载时请求用户基础信息(粉丝/关注/获赞在 interactions)
        "xhs" => vec!["/api/sns/web/v1/user/otherinfo"],
        // 快手:主页画像走 GraphQL(visionProfile);/graphql 已在搜索特征里,无需重复
        "kuaishou" => Vec::new(),
        // B站:空间页请求名片接口(粉丝在 card.fans / 关注 card.friend)
        "bilibili" => vec!["/x/web-interface/card"],
        // YouTube:频道页走 InnerTube browse(订阅数在 header)
        "youtube" => vec!["/youtubei/v1/browse"],
        _ => Vec::new(),
    }
}

/// 单个平台的完整配置。新增平台 = 往 `platforms` 里加一项 + 注册同名适配器。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlatformConfig {
    /// 平台唯一 ID,需与注册的适配器 key 一致(如 "douyin" / "xhs" / "kuaishou")。
    pub id: String,
    /// 展示名。
    pub name: String,
    /// 是否启用。停用后调度器跳过该平台任务,但配置保留。
    pub enabled: bool,
    /// 登录页地址:用户在可见 WebView 内完成登录,登录态随该账号 profile 持久化。
    pub login_url: String,
    /// 该平台目标账号数量上限,0 表示不限。
    #[serde(default)]
    pub max_accounts: u32,
    #[serde(default)]
    pub rate_limit: RateLimitConfig,
    #[serde(default)]
    pub collect: CollectConfig,
    /// 登录态真实检测配置(登录窗口内自检);全空则跳过检测、沿用乐观行为。
    #[serde(default)]
    pub login_check: LoginCheckConfig,
    /// 平台特有的扩展配置,适配器自行解释,核心层不感知。
    #[serde(default)]
    pub extra: serde_json::Value,
}

/// 远程上报配置。具体后端规格待用户提供,先做成可插拔占位。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ReportConfig {
    /// 上报方式:目前预留 "http";后续可扩展 "database" 等。
    pub kind: String,
    /// 远程接收端点。
    pub endpoint: Option<String>,
    /// 鉴权 token(从环境变量或安全存储注入,不硬编码进配置文件)。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auth_token: Option<String>,
    /// 单次上报批量条数。
    #[serde(default = "default_batch_size")]
    pub batch_size: usize,
}

fn default_batch_size() -> usize {
    50
}

/// 媒体处理(视频转音频)配置。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MediaConfig {
    /// 是否启用视频转音频。
    /// **已废弃**:视频是否转音频现由任务级 `ai_extract`(AI 文案提取)控制,
    /// 本字段不再被 media::process_content 读取,仅为兼容旧配置文件保留。
    pub enable_audio_extract: bool,
    /// ffmpeg 可执行文件路径;为空时按 sidecar / 系统 PATH 查找。
    pub ffmpeg_path: Option<String>,
    /// 输出音频格式(如 "mp3" / "wav")。
    pub audio_format: String,
    /// 媒体与中间文件输出目录。
    pub output_dir: String,
}

impl Default for MediaConfig {
    fn default() -> Self {
        Self {
            // 默认开启视频转音频(下载视频 → ffmpeg 转音频 → 删视频),符合「只留音频」诉求
            enable_audio_extract: true,
            ffmpeg_path: None,
            audio_format: "mp3".to_string(),
            output_dir: "media".to_string(),
        }
    }
}

/// 评论意向分析配置(系统设置「意向分析」)。只存对 providers/prompts 表的 id 引用 +
/// 模型名 + 批大小;api_key 等敏感信息仍存数据库,不落配置文件(安全规范)。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CommentIntentConfig {
    /// 模型厂商 id(providers.id 逻辑引用),空表示未配置。
    #[serde(default)]
    pub provider_id: String,
    /// 所选模型(provider.models 之一)。
    #[serde(default)]
    pub model: String,
    /// 提示词 id(prompts.id 逻辑引用)。
    #[serde(default)]
    pub prompt_id: String,
    /// 单批送入大模型的评论条数;<=0 时调用方回退默认值。
    #[serde(default)]
    pub batch_size: i32,
}

/// 语音转写配置(系统设置「语音转写」)。只存 providers 表 id 引用 + 模型名;
/// api_key 等敏感信息仍存数据库,不落配置文件(安全规范)。目前仅支持 ASR 的厂商(小米 MiMo)。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TranscriptionConfig {
    /// 模型厂商 id(providers.id 逻辑引用),空表示未配置。
    #[serde(default)]
    pub provider_id: String,
    /// 所选 ASR 模型(provider.models 之一,如 mimo-v2.5-asr)。
    #[serde(default)]
    pub model: String,
}

/// 数据库配置。运行时二选一:连接串决定后端(SQLite / PostgreSQL)。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DatabaseConfig {
    /// 连接串;为空时回退到应用数据目录下的本地 SQLite 文件。
    /// 安全规范:PG 含密码时**不要写在配置文件**,改设环境变量 `VELTRIX_DATABASE_URL`,连接时优先读取。
    #[serde(default)]
    pub url: String,
    /// 连接池最大连接数。
    #[serde(default = "default_db_max_connections")]
    pub max_connections: u32,
}

fn default_db_max_connections() -> u32 {
    DEFAULT_DB_MAX_CONNECTIONS
}

impl Default for DatabaseConfig {
    fn default() -> Self {
        Self {
            url: String::new(),
            max_connections: DEFAULT_DB_MAX_CONNECTIONS,
        }
    }
}

/// 顶层应用配置。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AppConfig {
    /// 平台表,key 为平台 ID。用 BTreeMap 保证序列化顺序稳定、便于人工管理。
    pub platforms: BTreeMap<String, PlatformConfig>,
    #[serde(default)]
    pub database: DatabaseConfig,
    #[serde(default)]
    pub report: ReportConfig,
    #[serde(default)]
    pub media: MediaConfig,
    #[serde(default)]
    pub intent: CommentIntentConfig,
    #[serde(default)]
    pub transcription: TranscriptionConfig,
}

impl AppConfig {
    /// 从配置目录加载;文件不存在时返回内置默认配置(含三平台骨架),
    /// 这样首次启动即可用,用户再按需在前端增删改。
    pub fn load_or_default(config_dir: &Path) -> Result<Self> {
        let path = config_dir.join(CONFIG_FILE_NAME);
        if !path.exists() {
            return Ok(Self::builtin_default());
        }
        let text = std::fs::read_to_string(&path)?;
        let mut cfg: AppConfig = serde_json::from_str(&text)
            .map_err(|e| CrawlerError::Config(format!("解析 {CONFIG_FILE_NAME} 失败: {e}")))?;
        // 兼容旧配置文件:补全内置平台后续新增的关键字段(detail_url_template / 内置拦截特征)。
        // 「文件已存在则只读文件」会让老用户拿不到新增的内置配置(如评论采集所需),这里启动时补齐。
        cfg.merge_builtin_platform_defaults();
        Ok(cfg)
    }

    /// 持久化配置,供前端「平台管理」保存改动后调用。
    pub fn save(&self, config_dir: &Path) -> Result<()> {
        std::fs::create_dir_all(config_dir)?;
        let path = config_dir.join(CONFIG_FILE_NAME);
        let text = serde_json::to_string_pretty(self)?;
        std::fs::write(path, text)?;
        Ok(())
    }

    /// 取启用中的平台配置;未知或停用平台返回明确错误,便于调度层跳过。
    pub fn platform(&self, id: &str) -> Result<&PlatformConfig> {
        match self.platforms.get(id) {
            Some(p) if p.enabled => Ok(p),
            Some(_) => Err(CrawlerError::Config(format!("平台已停用: {id}"))),
            None => Err(CrawlerError::UnknownPlatform(id.to_string())),
        }
    }

    /// 增加或覆盖一个平台配置(前端「新增平台」入口)。
    pub fn upsert_platform(&mut self, cfg: PlatformConfig) {
        self.platforms.insert(cfg.id.clone(), cfg);
    }

    /// 删除一个平台配置。
    pub fn remove_platform(&mut self, id: &str) -> bool {
        self.platforms.remove(id).is_some()
    }

    /// 用内置默认补全已有平台缺失的关键字段。只补不覆盖用户自定义:
    /// detail_url_template 仅在为空时填;intercept_patterns 并入内置缺失项(去重,保留用户已加的)。
    /// 这样新增内置配置字段对老配置文件也即时生效,无需用户删档重建。
    fn merge_builtin_platform_defaults(&mut self) {
        let builtin = Self::builtin_default();
        for (id, bp) in builtin.platforms {
            let Some(p) = self.platforms.get_mut(&id) else {
                // 缺失的内置平台整条补回,保证内置平台始终开箱可用
                // (被删 / 配置被清空后,启动即恢复;老用户升级后也能直接拿到新增的内置平台)。
                self.platforms.insert(id, bp);
                continue;
            };
            // 已有平台:补全缺失的关键字段(不覆盖用户已自定义的值)
            if p.name.is_empty() {
                p.name = bp.name.clone();
            }
            if p.login_url.is_empty() {
                p.login_url = bp.login_url.clone();
            }
            if p.collect.detail_url_template.is_empty() {
                p.collect.detail_url_template = bp.collect.detail_url_template.clone();
            }
            if p.collect.profile_url_template.is_empty() {
                p.collect.profile_url_template = bp.collect.profile_url_template.clone();
            }
            for pat in &bp.collect.intercept_patterns {
                if !p.collect.intercept_patterns.contains(pat) {
                    p.collect.intercept_patterns.push(pat.clone());
                }
            }
            // 补全搜索 URL 排序/时间参数映射(老配置缺失时)
            if p.collect.sort_query_key.is_empty() {
                p.collect.sort_query_key = bp.collect.sort_query_key.clone();
            }
            if p.collect.sort_query_map.is_empty() {
                p.collect.sort_query_map = bp.collect.sort_query_map.clone();
            }
            if p.collect.time_query_key.is_empty() {
                p.collect.time_query_key = bp.collect.time_query_key.clone();
            }
            if p.collect.time_query_map.is_empty() {
                p.collect.time_query_map = bp.collect.time_query_map.clone();
            }
            if p.collect.next_page_texts.is_empty() {
                p.collect.next_page_texts = bp.collect.next_page_texts.clone();
            }
        }
    }

    /// 内置三平台骨架配置。仅为开箱即用的起点,具体接口/签名在阶段1、2 完善。
    fn builtin_default() -> Self {
        let mut platforms = BTreeMap::new();
        // 搜索 URL 模板与拦截特征为「开箱起点」,真实路径需本机 `bun tauri dev` 抓包核对后调整
        for (id, name, login_url, search_url, detail_url, patterns) in [
            (
                "douyin",
                "抖音",
                "https://www.douyin.com/",
                "https://www.douyin.com/search/{keyword}",
                // 详情页 URL 模板,{id}=aweme_id,评论采集导航用;真实路径需本机抓包核对
                "https://www.douyin.com/video/{id}",
                vec![
                    "/aweme/v1/web/general/search/",
                    "/aweme/v1/web/search/item/",
                    // 一级评论接口;真实路径需本机抓包核对
                    "/aweme/v1/web/comment/list/",
                ],
            ),
            (
                "xhs",
                "小红书",
                "https://www.xiaohongshu.com/",
                "https://www.xiaohongshu.com/search_result?keyword={keyword}",
                // 笔记详情页:{id}=note_id,{token}=xsec_token(详情页鉴权必需);真实格式需抓包核对
                "https://www.xiaohongshu.com/explore/{id}?xsec_token={token}&xsec_source=pc_search",
                vec![
                    "/api/sns/web/v1/search/notes",
                    // 一级评论接口;真实路径需抓包核对
                    "/api/sns/web/v2/comment/page",
                ],
            ),
            (
                "kuaishou",
                "快手",
                "https://www.kuaishou.com/",
                "https://www.kuaishou.com/search/video?searchKey={keyword}",
                // 详情页:{id}=photoId,评论采集导航用;真实路径需本机抓包核对
                "https://www.kuaishou.com/short-video/{id}",
                // 快手 Web 搜索实测走 REST(POST /rest/v/search/feed,feeds 在响应根级),
                // 不是 GraphQL。两者都拦:搜索命中 REST,评论若仍走 graphql 也能拦到,
                // 适配器按响应体字段(feeds / visionCommentList)区分。评论真实路径待抓包核对。
                vec!["/rest/v/search/feed", "/graphql"],
            ),
            (
                "bilibili",
                "B站",
                "https://www.bilibili.com/",
                // 视频 tab(只出视频,解析最干净);搜索页为分页按钮,翻页靠 next_page_texts 点击
                "https://search.bilibili.com/video?keyword={keyword}",
                // 详情页:{id}=bvid,评论采集导航用
                "https://www.bilibili.com/video/{id}",
                // 搜索前缀同时覆盖 wbi/search/type(视频 tab)与 wbi/search/all/v2(综合 tab);
                // /x/v2/reply 前缀覆盖评论的 wbi/main 与旧 main 变体。真实路径需本机抓包核对
                vec!["/x/web-interface/wbi/search/", "/x/web-interface/search/", "/x/v2/reply"],
            ),
            (
                "tiktok",
                "TikTok",
                "https://www.tiktok.com/",
                "https://www.tiktok.com/search?q={keyword}",
                // 详情页:用户名段填占位 `_`,TikTok 会按视频 id 重定向到规范地址;需本机核对
                "https://www.tiktok.com/@_/video/{id}",
                // 综合搜索 + 视频 tab 搜索 + 评论;接口结构与抖音同源(aweme 体系)。
                // ⚠️ 大陆网络访问不了 TikTok,需本机代理可用
                vec!["/api/search/general/full/", "/api/search/item/full/", "/api/comment/list/"],
            ),
            (
                "youtube",
                "YouTube",
                "https://www.youtube.com/",
                "https://www.youtube.com/results?search_query={keyword}",
                "https://www.youtube.com/watch?v={id}",
                // InnerTube:搜索分页走 /youtubei/v1/search,评论随观看页走 /youtubei/v1/next。
                // ⚠️ 首屏结果内嵌在页面 ytInitialData 不走 XHR,首批 ~20 条采不到(滚动分页可采);
                // 排序参数是 protobuf 编码的 sp=,静态映射表达不了,留空。需本机代理可用
                vec!["/youtubei/v1/search", "/youtubei/v1/next"],
            ),
        ] {
            let (sort_query_key, sort_query_map, time_query_key, time_query_map) =
                builtin_search_query(id);
            let (verify_selectors, verify_texts) = builtin_verify_signals(id);
            // 搜索/评论拦截特征 + 画像接口特征合并(去重),作者补采复用同一套拦截
            let mut all_patterns: Vec<String> =
                patterns.into_iter().map(str::to_string).collect();
            for p in builtin_profile_patterns(id) {
                if !all_patterns.iter().any(|x| x == p) {
                    all_patterns.push(p.to_string());
                }
            }
            platforms.insert(
                id.to_string(),
                PlatformConfig {
                    id: id.to_string(),
                    name: name.to_string(),
                    enabled: true,
                    login_url: login_url.to_string(),
                    max_accounts: 0,
                    rate_limit: RateLimitConfig::default(),
                    collect: CollectConfig {
                        search_url_template: search_url.to_string(),
                        detail_url_template: detail_url.to_string(),
                        profile_url_template: builtin_profile_url(id).to_string(),
                        intercept_patterns: all_patterns,
                        rpa_script: None,
                        scroll_rounds: DEFAULT_SCROLL_ROUNDS,
                        scroll_interval_ms: DEFAULT_SCROLL_INTERVAL_MS,
                        // 节点级拟人步骤:小红书已填 v0 起点,其余平台留空走内置滚动逻辑
                        rpa_steps: default_rpa_steps(id),
                        sort_query_key,
                        sort_query_map,
                        time_query_key,
                        time_query_map,
                        next_page_texts: builtin_next_page_texts(id),
                        verify_selectors,
                        verify_texts,
                    },
                    login_check: builtin_login_check(id),
                    extra: serde_json::Value::Null,
                },
            );
        }
        Self {
            platforms,
            database: DatabaseConfig::default(),
            report: ReportConfig::default(),
            media: MediaConfig::default(),
            intent: CommentIntentConfig::default(),
            transcription: TranscriptionConfig::default(),
        }
    }
}

/// 解析配置目录(应用数据目录),集中一处便于后续替换为 Tauri path API。
pub fn resolve_config_dir(base: &Path) -> PathBuf {
    base.join("veltrix-crawler")
}
