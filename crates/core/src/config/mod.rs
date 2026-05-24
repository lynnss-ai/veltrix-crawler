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
}

fn default_scroll_rounds() -> u32 {
    DEFAULT_SCROLL_ROUNDS
}

fn default_scroll_interval_ms() -> u64 {
    DEFAULT_SCROLL_INTERVAL_MS
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
            enable_audio_extract: false,
            ffmpeg_path: None,
            audio_format: "mp3".to_string(),
            output_dir: "media".to_string(),
        }
    }
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
        let cfg: AppConfig = serde_json::from_str(&text)
            .map_err(|e| CrawlerError::Config(format!("解析 {CONFIG_FILE_NAME} 失败: {e}")))?;
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

    /// 内置三平台骨架配置。仅为开箱即用的起点,具体接口/签名在阶段1、2 完善。
    fn builtin_default() -> Self {
        let mut platforms = BTreeMap::new();
        // 搜索 URL 模板与拦截特征为「开箱起点」,真实路径需本机 `bun tauri dev` 抓包核对后调整
        for (id, name, login_url, search_url, patterns) in [
            (
                "douyin",
                "抖音",
                "https://www.douyin.com/",
                "https://www.douyin.com/search/{keyword}",
                vec!["/aweme/v1/web/general/search/", "/aweme/v1/web/search/item/"],
            ),
            (
                "xhs",
                "小红书",
                "https://www.xiaohongshu.com/",
                "https://www.xiaohongshu.com/search_result?keyword={keyword}",
                vec!["/api/sns/web/v1/search/notes"],
            ),
            (
                "kuaishou",
                "快手",
                "https://www.kuaishou.com/",
                "https://www.kuaishou.com/search/video?searchKey={keyword}",
                vec!["/graphql"],
            ),
        ] {
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
                        intercept_patterns: patterns
                            .into_iter()
                            .map(str::to_string)
                            .collect(),
                        rpa_script: None,
                        scroll_rounds: DEFAULT_SCROLL_ROUNDS,
                        scroll_interval_ms: DEFAULT_SCROLL_INTERVAL_MS,
                    },
                    extra: serde_json::Value::Null,
                },
            );
        }
        Self {
            platforms,
            database: DatabaseConfig::default(),
            report: ReportConfig::default(),
            media: MediaConfig::default(),
        }
    }
}

/// 解析配置目录(应用数据目录),集中一处便于后续替换为 Tauri path API。
pub fn resolve_config_dir(base: &Path) -> PathBuf {
    base.join("veltrix-crawler")
}
