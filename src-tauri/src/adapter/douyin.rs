//! 抖音平台适配器。
//!
//! 解析网页搜索接口 `/aweme/v1/web/general/search/single/` 的响应:
//! 响应体 `data` 数组每项含 `aweme_info`(视频/图文详情),抽取为统一 Content 模型。
//! 评论走独立的一级评论接口 `/aweme/v1/web/comment/list/`,由 `TaskKind::Comments` 分流解析(只采一级)。
//!
//! 解析全程用 `serde_json::Value` 按需取值(而非强类型反序列化),
//! 任一字段缺失/改名只丢该字段,不会拖垮整条乃至整批解析。

use crate::adapter::{FetchContext, FetchOutput, PlatformAdapter};
use crate::model::{Author, Comment, Content, ContentKind, Stats, TaskKind};
use async_trait::async_trait;
use chrono::Utc;
use serde_json::Value;
use veltrix_core::error::Result;

const PLATFORM_ID: &str = "douyin";
/// 搜索接口 URL 特征,与平台配置 intercept_patterns 对应。
const SEARCH_PATH: &str = "/aweme/v1/web/general/search/";
/// 一级评论接口 URL 特征,与平台配置 intercept_patterns 对应。
const COMMENT_PATH: &str = "/aweme/v1/web/comment/list/";

#[derive(Default)]
pub struct DouyinAdapter;

impl DouyinAdapter {
    pub fn new() -> Self {
        Self
    }

    /// 把单条 aweme_info 解析为 Content;缺 aweme_id 视为非内容卡(用户卡/相关词),返回 None。
    fn parse_aweme(info: &Value, collected_at: i64) -> Option<Content> {
        let content_id = info.get("aweme_id").and_then(Value::as_str)?.to_string();
        if content_id.is_empty() {
            return None;
        }

        // 有非空 images 即图文,否则按视频
        let image_urls = Self::parse_image_urls(info.get("images"));
        let kind = if image_urls.is_empty() {
            ContentKind::Video
        } else {
            ContentKind::Image
        };

        let video_url = info
            .get("video")
            .and_then(|v| v.get("play_addr"))
            .and_then(|a| a.get("url_list"))
            .and_then(Value::as_array)
            .and_then(|l| l.first())
            .and_then(Value::as_str)
            .map(str::to_string);

        // 封面:视频取 video.cover(退而求 origin_cover),图文用首图,保证两类内容都有封面可下
        let cover_url = if image_urls.is_empty() {
            Self::parse_video_cover(info.get("video"))
        } else {
            image_urls.first().cloned()
        };

        // 视频时长:video.duration 为毫秒,统一存秒;图文无 video 则为 None
        let duration = info
            .get("video")
            .and_then(|v| v.get("duration"))
            .and_then(Value::as_i64)
            .map(|ms| ms / 1000);

        // 话题先提取,再从正文 desc 里剥离掉(抖音 desc 自带 #话题),
        // 让 desc 只留纯正文、topics 单列存储,前端无需再切。
        let topics = Self::parse_topics(info);
        let desc = info
            .get("desc")
            .and_then(Value::as_str)
            .map(|raw| {
                // 按长度降序剥离,避免「#上海」误伤「#上海迪士尼」这类前缀重叠的长话题
                let mut ordered = topics.clone();
                ordered.sort_by_key(|t| std::cmp::Reverse(t.chars().count()));
                let mut text = raw.to_string();
                for topic in &ordered {
                    text = text.replace(topic.as_str(), "");
                }
                // 保留换行分段:仅折叠每行内话题剥离后残留的连续空白,不把多行压成一行
                text.lines()
                    .map(|line| line.split_whitespace().collect::<Vec<_>>().join(" "))
                    .collect::<Vec<_>>()
                    .join("\n")
                    .trim()
                    .to_string()
            })
            .filter(|s| !s.is_empty());

        Some(Content {
            platform: PLATFORM_ID.to_string(),
            content_id,
            kind,
            title: None, // 抖音无独立标题,正文在 desc
            desc,
            author: Self::parse_author(info.get("author")),
            stats: Self::parse_stats(info.get("statistics")),
            published_at: info.get("create_time").and_then(Value::as_i64),
            video_url,
            cover_url,
            image_urls,
            duration,
            topics,
            collected_at,
            extra: Self::parse_extra(info),
        })
    }

    /// 话题标签:抖音把正文里的 #话题 结构化在 `text_extra[].hashtag_name`,
    /// 比正则切 desc 更准(能拿到完整话题名、不误伤普通 # 文本)。统一加 # 前缀。
    fn parse_topics(info: &Value) -> Vec<String> {
        info.get("text_extra")
            .and_then(Value::as_array)
            .map(|arr| {
                arr.iter()
                    .filter_map(|e| e.get("hashtag_name").and_then(Value::as_str))
                    .filter(|name| !name.is_empty())
                    .map(|name| format!("#{name}"))
                    .collect()
            })
            .unwrap_or_default()
    }

    fn parse_author(value: Option<&Value>) -> Author {
        let Some(a) = value else {
            return Author::default();
        };
        // sec_uid 比 uid 稳定(uid 可能被隐藏),优先用
        let uid = a
            .get("sec_uid")
            .and_then(Value::as_str)
            .or_else(|| a.get("uid").and_then(Value::as_str))
            .unwrap_or_default()
            .to_string();
        let avatar = a
            .get("avatar_thumb")
            .and_then(|av| av.get("url_list"))
            .and_then(Value::as_array)
            .and_then(|l| l.first())
            .and_then(Value::as_str)
            .map(str::to_string);
        Author {
            platform: PLATFORM_ID.to_string(),
            uid,
            nickname: a
                .get("nickname")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            avatar,
            signature: a.get("signature").and_then(Value::as_str).map(str::to_string),
            follower_count: a.get("follower_count").and_then(Value::as_i64),
            following_count: a.get("following_count").and_then(Value::as_i64),
            extra: serde_json::json!({
                "unique_id": a.get("unique_id").and_then(Value::as_str),
                "uid": a.get("uid").and_then(Value::as_str),
                // 属地 / 作者获赞总数:搜索响应若带则存,供内容详情展示(缺失为 null)
                "ip_location": a.get("ip_location").and_then(Value::as_str),
                "total_favorited": a.get("total_favorited").and_then(Value::as_i64),
            }),
        }
    }

    fn parse_stats(value: Option<&Value>) -> Stats {
        let Some(s) = value else {
            return Stats::default();
        };
        let get = |key: &str| s.get(key).and_then(Value::as_i64);
        Stats {
            like_count: get("digg_count"),
            comment_count: get("comment_count"),
            collect_count: get("collect_count"),
            share_count: get("share_count"),
            play_count: get("play_count"),
        }
    }

    /// 视频封面:优先 cover,缺失时退回 origin_cover;均取 url_list 首个直链。
    fn parse_video_cover(video: Option<&Value>) -> Option<String> {
        let video = video?;
        let pick = |key: &str| {
            video
                .get(key)
                .and_then(|c| c.get("url_list"))
                .and_then(Value::as_array)
                .and_then(|l| l.first())
                .and_then(Value::as_str)
                .map(str::to_string)
        };
        pick("cover").or_else(|| pick("origin_cover"))
    }

    fn parse_image_urls(value: Option<&Value>) -> Vec<String> {
        value
            .and_then(Value::as_array)
            .map(|imgs| {
                imgs.iter()
                    .filter_map(|img| {
                        img.get("url_list")
                            .and_then(Value::as_array)
                            .and_then(|l| l.first())
                            .and_then(Value::as_str)
                            .map(str::to_string)
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// 保留少量平台特有字段,既不丢关键信息又不让库膨胀。
    fn parse_extra(info: &Value) -> Value {
        serde_json::json!({
            // share_url 在 share_info 下,非 aweme_info 顶层
            "share_url": info
                .get("share_info")
                .and_then(|s| s.get("share_url"))
                .and_then(Value::as_str),
            "duration": info.get("video").and_then(|v| v.get("duration")).and_then(Value::as_i64),
            "aweme_type": info.get("aweme_type").and_then(Value::as_i64),
        })
    }

    /// 把单条评论解析为 Comment;缺 cid 视为无效返回 None。本期只采一级评论,parent_id 恒为 None。
    fn parse_comment(item: &Value, collected_at: i64) -> Option<Comment> {
        let comment_id = item.get("cid").and_then(Value::as_str)?.to_string();
        if comment_id.is_empty() {
            return None;
        }
        // 评论自带所属 aweme_id;缺失时留空,落库仍可按 comment_id 去重
        let content_id = item
            .get("aweme_id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        Some(Comment {
            platform: PLATFORM_ID.to_string(),
            content_id,
            comment_id,
            parent_id: None,
            author: Self::parse_author(item.get("user")),
            text: item
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            like_count: item.get("digg_count").and_then(Value::as_i64),
            reply_count: item.get("reply_comment_total").and_then(Value::as_i64),
            created_at: item.get("create_time").and_then(Value::as_i64),
            collected_at,
            // 保留 IP 属地等少量平台特有字段,便于后续意图分析
            extra: serde_json::json!({
                "ip_label": item.get("ip_label").and_then(Value::as_str),
            }),
        })
    }

    /// 解析搜索接口响应为内容列表(comments 恒空)。
    fn parse_search(ctx: &FetchContext) -> FetchOutput {
        let collected_at = Utc::now().timestamp();
        let mut contents = Vec::new();

        for resp in &ctx.responses {
            // 只认搜索接口;其余命中的接口(如评论)结构不同,跳过不报错
            if !resp.url.contains(SEARCH_PATH) {
                continue;
            }
            // 单条脏响应不拖垮整批
            let Ok(root) = serde_json::from_str::<Value>(&resp.body) else {
                continue;
            };
            // single 接口用 data;兼容旧版/推荐流的 aweme_list
            let items = root
                .get("data")
                .and_then(Value::as_array)
                .or_else(|| root.get("aweme_list").and_then(Value::as_array));
            let Some(items) = items else {
                continue;
            };
            for item in items {
                // data 每项把详情包在 aweme_info 里;aweme_list 每项本身即详情
                let info = item.get("aweme_info").unwrap_or(item);
                if let Some(content) = Self::parse_aweme(info, collected_at) {
                    contents.push(content);
                }
            }
        }

        FetchOutput {
            contents,
            comments: Vec::new(),
            authors: Vec::new(),
        }
    }

    /// 解析一级评论接口响应为评论列表(contents 恒空)。
    /// 只取一级评论接口,排除 URL 含 `reply` 的二级回复接口(本期只采一级)。
    fn parse_comments(ctx: &FetchContext) -> FetchOutput {
        let collected_at = Utc::now().timestamp();
        let mut comments = Vec::new();

        for resp in &ctx.responses {
            if !resp.url.contains(COMMENT_PATH) || resp.url.contains("reply") {
                continue;
            }
            let Ok(root) = serde_json::from_str::<Value>(&resp.body) else {
                continue;
            };
            let Some(items) = root.get("comments").and_then(Value::as_array) else {
                continue;
            };
            for item in items {
                if let Some(comment) = Self::parse_comment(item, collected_at) {
                    comments.push(comment);
                }
            }
        }

        FetchOutput {
            contents: Vec::new(),
            comments,
            authors: Vec::new(),
        }
    }
}

#[async_trait]
impl PlatformAdapter for DouyinAdapter {
    fn id(&self) -> &str {
        PLATFORM_ID
    }

    fn supports(&self, kind: &TaskKind) -> bool {
        matches!(kind, TaskKind::Search | TaskKind::Comments)
    }

    async fn parse(&self, kind: &TaskKind, ctx: &FetchContext) -> Result<FetchOutput> {
        // 按任务类型分流:评论任务解析一级评论,其余按搜索内容解析
        let output = match kind {
            TaskKind::Comments => Self::parse_comments(ctx),
            _ => Self::parse_search(ctx),
        };
        Ok(output)
    }
}
