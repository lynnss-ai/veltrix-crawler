//! YouTube 平台适配器。
//!
//! 数据来自 InnerTube 接口:搜索分页 `POST /youtubei/v1/search`,评论随观看页
//! `POST /youtubei/v1/next`。响应是层级极深、随实验灰度变化的 Renderer 树,逐层取值
//! 太脆——这里改为**递归收集目标节点**(`videoRenderer` / `commentEntityPayload` /
//! 旧版 `commentRenderer`),只解析认识的节点,结构变化时最多漏新形态、不会整批失败。
//!
//! 已知限制(v1 接受):
//! - 首屏搜索结果内嵌在页面 `ytInitialData`(不走 XHR)采不到,滚动分页可采;
//! - 计数是本地化文本("1.2万次观看"/"1,234 views"),尽力解析,失败置 None;
//! - 发布/评论时间是相对文本("3 天前")无法还原时间戳,原文存 extra;
//! - 视频流地址有签名混淆(n-throttling),不采视频/音频,`video_url` 恒为 None。
//!
//! ⚠️ 节点字段基于 Web 端公开结构整理,需本机抓包核对(需代理可用)。

use crate::adapter::{FetchContext, FetchOutput, PlatformAdapter};
use crate::model::{Author, Comment, Content, ContentKind, Stats, TaskKind};
use async_trait::async_trait;
use chrono::Utc;
use serde_json::Value;
use veltrix_core::error::Result;

const PLATFORM_ID: &str = "youtube";

#[derive(Default)]
pub struct YoutubeAdapter;

impl YoutubeAdapter {
    pub fn new() -> Self {
        Self
    }

    /// 递归收集树中所有键名为 `key` 的对象节点。深度优先;命中的节点不再向其内部递归
    /// (videoRenderer 内不会再嵌 videoRenderer,少走无谓的深层遍历)。
    fn collect_by_key<'a>(node: &'a Value, key: &str, out: &mut Vec<&'a Value>) {
        match node {
            Value::Object(map) => {
                for (k, v) in map {
                    if k == key {
                        out.push(v);
                    } else {
                        Self::collect_by_key(v, key, out);
                    }
                }
            }
            Value::Array(arr) => {
                for v in arr {
                    Self::collect_by_key(v, key, out);
                }
            }
            _ => {}
        }
    }

    /// 拼接 runs 文本(InnerTube 文本通用形态 `{runs:[{text}...]}`,兜底 `{simpleText}`)。
    fn text_of(value: Option<&Value>) -> Option<String> {
        let v = value?;
        if let Some(s) = v.get("simpleText").and_then(Value::as_str) {
            return (!s.is_empty()).then(|| s.to_string());
        }
        let runs = v.get("runs").and_then(Value::as_array)?;
        let joined: String = runs
            .iter()
            .filter_map(|r| r.get("text").and_then(Value::as_str))
            .collect();
        (!joined.is_empty()).then_some(joined)
    }

    /// 解析本地化计数文本:"1,234,567 views" / "1.2万次观看" / "3.4K"。
    /// 取前导数字(含千分位与小数点),按 万/亿/K/M/B 乘倍;无数字返回 None。
    fn parse_count_text(text: &str) -> Option<i64> {
        let cleaned = text.replace([',', ' '], "");
        let numeric: String = cleaned
            .chars()
            .take_while(|c| c.is_ascii_digit() || *c == '.')
            .collect();
        if numeric.is_empty() {
            return None;
        }
        let base: f64 = numeric.parse().ok()?;
        let rest = &cleaned[numeric.len()..];
        let multiplier: f64 = if rest.starts_with('万') {
            1e4
        } else if rest.starts_with('亿') {
            1e8
        } else if rest.starts_with('K') || rest.starts_with('k') {
            1e3
        } else if rest.starts_with('M') {
            1e6
        } else if rest.starts_with('B') {
            1e9
        } else {
            1.0
        };
        Some((base * multiplier) as i64)
    }

    /// "12:34" / "1:02:03" 形式的时长文本转秒。
    fn parse_clock(text: &str) -> Option<i64> {
        let mut total: i64 = 0;
        for part in text.trim().split(':') {
            total = total * 60 + part.trim().parse::<i64>().ok()?;
        }
        (total > 0).then_some(total)
    }

    /// 缩略图组里取最大一张(thumbnails 按尺寸升序,取末位)。
    fn last_thumbnail(value: Option<&Value>) -> Option<String> {
        value?
            .get("thumbnails")
            .and_then(Value::as_array)?
            .last()?
            .get("url")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    }

    /// 把单个 videoRenderer 解析为 Content;缺 videoId 视为无效返回 None。
    fn parse_video_renderer(node: &Value, collected_at: i64) -> Option<Content> {
        let content_id = node
            .get("videoId")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())?
            .to_string();

        // 频道主信息:ownerText.runs[0] 带昵称与 browseEndpoint(browseId=UC 频道 id,
        // canonicalBaseUrl="/@handle")。搜索结果不含订阅数,粉丝数采不到(平台响应限制)。
        let owner_run = node
            .get("ownerText")
            .and_then(|t| t.get("runs"))
            .and_then(Value::as_array)
            .and_then(|runs| runs.first());
        let browse = owner_run
            .and_then(|r| r.get("navigationEndpoint"))
            .and_then(|n| n.get("browseEndpoint"));
        // "/@handle" → "handle";存 extra.unique_id 映射到 authors 表平台号
        let handle = browse
            .and_then(|b| b.get("canonicalBaseUrl"))
            .and_then(Value::as_str)
            .and_then(|u| u.strip_prefix("/@"))
            .filter(|s| !s.is_empty());
        let author = Author {
            platform: PLATFORM_ID.to_string(),
            uid: browse
                .and_then(|b| b.get("browseId"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            nickname: owner_run
                .and_then(|r| r.get("text"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            avatar: Self::last_thumbnail(
                node.get("channelThumbnailSupportedRenderers")
                    .and_then(|c| c.get("channelThumbnailWithLinkRenderer"))
                    .and_then(|c| c.get("thumbnail")),
            ),
            signature: None,
            follower_count: None,
            following_count: None,
            extra: serde_json::json!({ "unique_id": handle }),
        };

        // 摘要新旧两个挂点:detailedMetadataSnippets[0].snippetText / descriptionSnippet
        let desc = node
            .get("detailedMetadataSnippets")
            .and_then(Value::as_array)
            .and_then(|arr| arr.first())
            .and_then(|s| Self::text_of(s.get("snippetText")))
            .or_else(|| Self::text_of(node.get("descriptionSnippet")));

        Some(Content {
            platform: PLATFORM_ID.to_string(),
            content_id,
            kind: ContentKind::Video,
            title: Self::text_of(node.get("title")),
            desc,
            author,
            stats: Stats {
                like_count: None, // 搜索结果不含点赞数
                comment_count: None,
                collect_count: None,
                share_count: None,
                play_count: Self::text_of(node.get("viewCountText"))
                    .as_deref()
                    .and_then(Self::parse_count_text),
            },
            // 相对时间("3 天前")无法还原时间戳,原文存 extra
            published_at: None,
            // 流地址有签名混淆,v1 不采视频/音频
            video_url: None,
            cover_url: Self::last_thumbnail(node.get("thumbnail")),
            image_urls: Vec::new(),
            duration: Self::text_of(node.get("lengthText"))
                .as_deref()
                .and_then(Self::parse_clock),
            topics: Vec::new(),
            collected_at,
            extra: serde_json::json!({
                "publishedTimeText": Self::text_of(node.get("publishedTimeText")),
            }),
        })
    }

    /// 新版评论节点(frameworkUpdates 里的 commentEntityPayload)解析为 Comment。
    /// 仅一级评论(replyLevel 0 或缺省);相对时间原文存 extra。
    fn parse_comment_payload(node: &Value, content_id: &str, collected_at: i64) -> Option<Comment> {
        let props = node.get("properties")?;
        if let Some(level) = props.get("replyLevel").and_then(Value::as_i64) {
            if level > 0 {
                return None;
            }
        }
        let comment_id = props
            .get("commentId")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())?
            .to_string();
        let author = node.get("author");
        let toolbar = node.get("toolbar");
        Some(Comment {
            platform: PLATFORM_ID.to_string(),
            content_id: content_id.to_string(),
            comment_id,
            parent_id: None,
            author: Author {
                platform: PLATFORM_ID.to_string(),
                uid: author
                    .and_then(|a| a.get("channelId"))
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                nickname: author
                    .and_then(|a| a.get("displayName"))
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                avatar: author
                    .and_then(|a| a.get("avatarThumbnailUrl"))
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string),
                signature: None,
                follower_count: None,
                following_count: None,
                extra: serde_json::json!({}),
            },
            text: props
                .get("content")
                .and_then(|c| c.get("content"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            like_count: toolbar
                .and_then(|t| t.get("likeCountNotliked"))
                .and_then(Value::as_str)
                .and_then(Self::parse_count_text),
            reply_count: toolbar
                .and_then(|t| t.get("replyCount"))
                .and_then(Value::as_str)
                .and_then(Self::parse_count_text),
            created_at: None,
            collected_at,
            extra: serde_json::json!({
                "publishedTime": props.get("publishedTime").and_then(Value::as_str),
            }),
        })
    }

    /// 旧版评论节点(commentRenderer)解析为 Comment,作为灰度兜底。
    fn parse_comment_renderer(node: &Value, content_id: &str, collected_at: i64) -> Option<Comment> {
        let comment_id = node
            .get("commentId")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())?
            .to_string();
        Some(Comment {
            platform: PLATFORM_ID.to_string(),
            content_id: content_id.to_string(),
            comment_id,
            parent_id: None,
            author: Author {
                platform: PLATFORM_ID.to_string(),
                uid: node
                    .get("authorEndpoint")
                    .and_then(|e| e.get("browseEndpoint"))
                    .and_then(|b| b.get("browseId"))
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                nickname: Self::text_of(node.get("authorText")).unwrap_or_default(),
                avatar: Self::last_thumbnail(node.get("authorThumbnail")),
                signature: None,
                follower_count: None,
                following_count: None,
                extra: serde_json::json!({}),
            },
            text: Self::text_of(node.get("contentText")).unwrap_or_default(),
            like_count: Self::text_of(node.get("voteCount"))
                .as_deref()
                .and_then(Self::parse_count_text),
            reply_count: node.get("replyCount").and_then(Value::as_i64),
            created_at: None,
            collected_at,
            extra: serde_json::json!({
                "publishedTime": Self::text_of(node.get("publishedTimeText")),
            }),
        })
    }

    /// 解析搜索响应为内容列表(comments 恒空)。Shorts(reelItemRenderer 等)本期不采。
    fn parse_search(ctx: &FetchContext) -> FetchOutput {
        let collected_at = Utc::now().timestamp();
        let mut contents = Vec::new();
        for resp in &ctx.responses {
            let Ok(root) = serde_json::from_str::<Value>(&resp.body) else {
                continue;
            };
            let mut nodes = Vec::new();
            Self::collect_by_key(&root, "videoRenderer", &mut nodes);
            for node in nodes {
                if let Some(content) = Self::parse_video_renderer(node, collected_at) {
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

    /// 解析频道主页画像(authors 仅一条;contents/comments 恒空)。频道页走 InnerTube
    /// `browse`,订阅数在 header 的 `subscriberCountText`("1.2M subscribers"/"1.2万位订阅者"),
    /// 昵称/头像在 `c4TabbedHeaderRenderer`。YouTube 不公开关注数/获赞总数,留空。
    /// uid 用导航时的 channelId(ctx.keyword)。结构随灰度多变,递归取节点容错。
    fn parse_profile(ctx: &FetchContext) -> FetchOutput {
        let uid = ctx.keyword.clone();
        let mut authors = Vec::new();
        for resp in &ctx.responses {
            let Ok(root) = serde_json::from_str::<Value>(&resp.body) else {
                continue;
            };
            let mut subs_nodes = Vec::new();
            Self::collect_by_key(&root, "subscriberCountText", &mut subs_nodes);
            let follower = subs_nodes
                .iter()
                .find_map(|n| Self::text_of(Some(*n)).as_deref().and_then(Self::parse_count_text));

            let mut header_nodes = Vec::new();
            Self::collect_by_key(&root, "c4TabbedHeaderRenderer", &mut header_nodes);
            let header = header_nodes.first();
            let nickname = header
                .and_then(|h| h.get("title"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let avatar = header.and_then(|h| Self::last_thumbnail(h.get("avatar")));

            // 全空说明响应非频道页(或结构已变):跳过,避免写入空画像覆盖已有数据
            if follower.is_none() && nickname.is_empty() && avatar.is_none() {
                continue;
            }
            authors.push(Author {
                platform: PLATFORM_ID.to_string(),
                uid: uid.clone(),
                nickname,
                avatar,
                signature: None,
                follower_count: follower,
                following_count: None,
                extra: serde_json::json!({}),
            });
            break; // 一个频道页只取一条画像
        }
        FetchOutput {
            contents: Vec::new(),
            comments: Vec::new(),
            authors,
        }
    }

    /// 解析评论响应为评论列表(contents 恒空)。新版 commentEntityPayload 为主,
    /// 旧版 commentRenderer 兜底;同批两形态并存时按 comment_id 由下游去重。
    fn parse_comments(ctx: &FetchContext) -> FetchOutput {
        let collected_at = Utc::now().timestamp();
        // 评论场景下采集上下文的 keyword 即所属内容 id(videoId)
        let content_id = ctx.keyword.as_str();
        let mut comments = Vec::new();
        for resp in &ctx.responses {
            let Ok(root) = serde_json::from_str::<Value>(&resp.body) else {
                continue;
            };
            let mut payloads = Vec::new();
            Self::collect_by_key(&root, "commentEntityPayload", &mut payloads);
            for node in payloads {
                if let Some(comment) = Self::parse_comment_payload(node, content_id, collected_at) {
                    comments.push(comment);
                }
            }
            let mut renderers = Vec::new();
            Self::collect_by_key(&root, "commentRenderer", &mut renderers);
            for node in renderers {
                if let Some(comment) = Self::parse_comment_renderer(node, content_id, collected_at)
                {
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
impl PlatformAdapter for YoutubeAdapter {
    fn id(&self) -> &str {
        PLATFORM_ID
    }

    fn supports(&self, kind: &TaskKind) -> bool {
        matches!(
            kind,
            TaskKind::Search | TaskKind::Comments | TaskKind::UserProfile
        )
    }

    async fn parse(&self, kind: &TaskKind, ctx: &FetchContext) -> Result<FetchOutput> {
        let output = match kind {
            TaskKind::Comments => Self::parse_comments(ctx),
            TaskKind::UserProfile => Self::parse_profile(ctx),
            _ => Self::parse_search(ctx),
        };
        Ok(output)
    }
}
