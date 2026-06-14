//! TikTok 平台适配器。
//!
//! 接口与抖音同源(aweme 体系)但 Web 端字段命名不同:综合搜索
//! (`/api/search/general/full/`)结果在 `data[]`,每项 `{type:1, item:{...}}`;视频 tab
//! (`/api/search/item/full/`)在 `item_list[]`/`itemList[]`,两种形态都兼容。item 为
//! camelCase(`createTime`/`stats.diggCount` 等)。X-Bogus/msToken 由页面自己签,拦截模式天然绕过。
//!
//! 评论(`/api/comment/list/`)沿用旧 aweme snake_case 结构(`cid`/`digg_count`/`user`);
//! 评论项虽含 aweme_id,但与其他平台保持一致:所属内容 id 由采集上下文(`ctx.keyword`)传入。
//!
//! ⚠️ `video.playAddr` 直链下载由 CDN 校验会话 Cookie,reqwest 无 Cookie 拉取可能 403——
//! 失败会走素材失败标记 + 补偿重试,不阻塞采集主体。字段名需本机抓包核对(需代理可用)。

use crate::adapter::{FetchContext, FetchOutput, PlatformAdapter};
use crate::model::{Author, Comment, Content, ContentKind, Stats, TaskKind};
use async_trait::async_trait;
use chrono::Utc;
use serde_json::Value;
use veltrix_core::error::Result;

const PLATFORM_ID: &str = "tiktok";

#[derive(Default)]
pub struct TiktokAdapter;

impl TiktokAdapter {
    pub fn new() -> Self {
        Self
    }

    /// 数字字段容错:stats 是数字而 statsV2 是字符串,两种都收。
    fn num(value: Option<&Value>) -> Option<i64> {
        let v = value?;
        v.as_i64()
            .or_else(|| v.as_str().and_then(|s| s.parse::<i64>().ok()))
    }

    /// id 字段容错:可能是字符串或数字,统一成 String;空返回 None。
    fn as_string_opt(value: Option<&Value>) -> Option<String> {
        match value {
            Some(Value::String(s)) if !s.is_empty() => Some(s.clone()),
            Some(Value::Number(n)) => Some(n.to_string()),
            _ => None,
        }
    }

    /// 同一计数字段先取 stats(数字)再兜底 statsV2(字符串)。
    fn stat(item: &Value, key: &str) -> Option<i64> {
        Self::num(item.get("stats").and_then(|s| s.get(key)))
            .or_else(|| Self::num(item.get("statsV2").and_then(|s| s.get(key))))
    }

    fn str_field(value: Option<&Value>) -> Option<String> {
        value
            .and_then(Value::as_str)
            .map(str::to_string)
            .filter(|s| !s.is_empty())
    }

    /// 把单个搜索 item 解析为 Content;缺 id 视为无效返回 None。
    fn parse_item(item: &Value, collected_at: i64) -> Option<Content> {
        let content_id = Self::as_string_opt(item.get("id"))?;
        let video = item.get("video");

        // 图集帖(imagePost)按图文,否则按视频
        let image_urls = Self::parse_image_urls(item);
        let kind = if image_urls.is_empty() {
            ContentKind::Video
        } else {
            ContentKind::Image
        };

        let cover_url = video
            .and_then(|v| Self::str_field(v.get("cover")))
            .or_else(|| video.and_then(|v| Self::str_field(v.get("originCover"))))
            .or_else(|| image_urls.first().cloned());

        Some(Content {
            platform: PLATFORM_ID.to_string(),
            content_id,
            kind,
            title: None, // TikTok 无独立标题,正文在 desc
            desc: Self::str_field(item.get("desc")),
            // 传整个 item:作者基础信息在 author,粉丝/获赞等画像在兄弟节点 authorStats
            author: Self::parse_author(item),
            stats: Stats {
                like_count: Self::stat(item, "diggCount"),
                comment_count: Self::stat(item, "commentCount"),
                collect_count: Self::stat(item, "collectCount"),
                share_count: Self::stat(item, "shareCount"),
                play_count: Self::stat(item, "playCount"),
            },
            published_at: Self::num(item.get("createTime")),
            video_url: video
                .and_then(|v| Self::str_field(v.get("playAddr")))
                .or_else(|| video.and_then(|v| Self::str_field(v.get("downloadAddr")))),
            cover_url,
            image_urls,
            duration: video.and_then(|v| Self::num(v.get("duration"))).filter(|d| *d > 0),
            topics: Self::parse_topics(item),
            collected_at,
            extra: serde_json::json!({}),
        })
    }

    /// 图集帖图片:item.imagePost.images[].imageURL.urlList 取首个直链。视频帖为空。
    fn parse_image_urls(item: &Value) -> Vec<String> {
        item.get("imagePost")
            .and_then(|p| p.get("images"))
            .and_then(Value::as_array)
            .map(|arr| {
                arr.iter()
                    .filter_map(|img| {
                        img.get("imageURL")
                            .and_then(|u| u.get("urlList"))
                            .and_then(Value::as_array)
                            .and_then(|list| list.iter().find_map(|u| u.as_str()))
                            .filter(|s| !s.is_empty())
                            .map(str::to_string)
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// 话题:优先结构化的 challenges[].title,兜底 textExtra[].hashtagName;统一加 # 前缀。
    fn parse_topics(item: &Value) -> Vec<String> {
        let from = |key: &str, name_key: &str| -> Vec<String> {
            item.get(key)
                .and_then(Value::as_array)
                .map(|arr| {
                    arr.iter()
                        .filter_map(|t| t.get(name_key).and_then(Value::as_str))
                        .filter(|n| !n.is_empty())
                        .map(|n| {
                            if n.starts_with('#') {
                                n.to_string()
                            } else {
                                format!("#{n}")
                            }
                        })
                        .collect()
                })
                .unwrap_or_default()
        };
        let topics = from("challenges", "title");
        if !topics.is_empty() {
            return topics;
        }
        from("textExtra", "hashtagName")
    }

    /// 作者解析:基础信息在 item.author,粉丝/关注/获赞画像在兄弟节点 item.authorStats
    /// (statsV2 形态是字符串计数,一并兜底)。extra 键名须与 authors 表映射约定一致
    /// (unique_id → platform_id,total_favorited → 获赞总数,见 author_to_active)。
    fn parse_author(item: &Value) -> Author {
        let Some(a) = item.get("author") else {
            return Author::default();
        };
        let author_stat = |key: &str| -> Option<i64> {
            Self::num(item.get("authorStats").and_then(|s| s.get(key)))
                .or_else(|| Self::num(item.get("authorStatsV2").and_then(|s| s.get(key))))
        };
        Author {
            platform: PLATFORM_ID.to_string(),
            uid: Self::as_string_opt(a.get("id")).unwrap_or_default(),
            nickname: a
                .get("nickname")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            avatar: Self::str_field(a.get("avatarLarger"))
                .or_else(|| Self::str_field(a.get("avatarMedium")))
                .or_else(|| Self::str_field(a.get("avatarThumb"))),
            signature: Self::str_field(a.get("signature")),
            follower_count: author_stat("followerCount"),
            following_count: author_stat("followingCount"),
            // unique_id 即 @handle,主页地址要靠它拼(uid 是纯数字拼不出主页)
            extra: serde_json::json!({
                "unique_id": a.get("uniqueId").and_then(Value::as_str),
                "total_favorited": author_stat("heartCount").or_else(|| author_stat("heart")),
            }),
        }
    }

    /// 把单条评论解析为 Comment;缺 cid 视为无效返回 None。本期只采一级评论。
    fn parse_comment(item: &Value, content_id: &str, collected_at: i64) -> Option<Comment> {
        let comment_id = Self::as_string_opt(item.get("cid"))?;
        let user = item.get("user");
        Some(Comment {
            platform: PLATFORM_ID.to_string(),
            content_id: content_id.to_string(),
            comment_id,
            parent_id: None,
            author: Author {
                platform: PLATFORM_ID.to_string(),
                uid: user
                    .and_then(|u| Self::as_string_opt(u.get("uid")))
                    .unwrap_or_default(),
                nickname: user
                    .and_then(|u| u.get("nickname"))
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                // 旧 aweme 结构:头像在 avatar_thumb.url_list[]
                avatar: user
                    .and_then(|u| u.get("avatar_thumb"))
                    .and_then(|t| t.get("url_list"))
                    .and_then(Value::as_array)
                    .and_then(|list| list.iter().find_map(|u| u.as_str()))
                    .filter(|s| !s.is_empty())
                    .map(str::to_string),
                signature: None,
                follower_count: None,
                following_count: None,
                extra: serde_json::json!({
                    "unique_id": user.and_then(|u| u.get("unique_id")).and_then(Value::as_str),
                }),
            },
            text: item
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            like_count: Self::num(item.get("digg_count")),
            reply_count: Self::num(item.get("reply_comment_total")),
            created_at: Self::num(item.get("create_time")),
            collected_at,
            extra: serde_json::json!({}),
        })
    }

    /// 解析搜索响应为内容列表(comments 恒空)。兼容综合搜索(data[].item)与
    /// 视频 tab(item_list[]/itemList[])两种形态。
    fn parse_search(ctx: &FetchContext) -> FetchOutput {
        let collected_at = Utc::now().timestamp();
        let mut contents = Vec::new();
        for resp in &ctx.responses {
            let Ok(root) = serde_json::from_str::<Value>(&resp.body) else {
                continue;
            };
            if let Some(entries) = root.get("data").and_then(Value::as_array) {
                for entry in entries {
                    // 综合搜索混排用户/直播等结果,只认带 item 的视频/图集项
                    let Some(item) = entry.get("item") else {
                        continue;
                    };
                    if let Some(content) = Self::parse_item(item, collected_at) {
                        contents.push(content);
                    }
                }
            }
            for key in ["item_list", "itemList"] {
                let Some(items) = root.get(key).and_then(Value::as_array) else {
                    continue;
                };
                for item in items {
                    if let Some(content) = Self::parse_item(item, collected_at) {
                        contents.push(content);
                    }
                }
            }
        }
        FetchOutput {
            contents,
            comments: Vec::new(),
            authors: Vec::new(),
        }
    }

    /// 解析评论响应为评论列表(contents 恒空)。
    fn parse_comments(ctx: &FetchContext) -> FetchOutput {
        let collected_at = Utc::now().timestamp();
        // 评论场景下采集上下文的 keyword 即所属内容 id
        let content_id = ctx.keyword.as_str();
        let mut comments = Vec::new();
        for resp in &ctx.responses {
            let Ok(root) = serde_json::from_str::<Value>(&resp.body) else {
                continue;
            };
            let Some(items) = root.get("comments").and_then(Value::as_array) else {
                continue;
            };
            for item in items {
                if let Some(comment) = Self::parse_comment(item, content_id, collected_at) {
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
impl PlatformAdapter for TiktokAdapter {
    fn id(&self) -> &str {
        PLATFORM_ID
    }

    fn supports(&self, kind: &TaskKind) -> bool {
        matches!(kind, TaskKind::Search | TaskKind::Comments)
    }

    async fn parse(&self, kind: &TaskKind, ctx: &FetchContext) -> Result<FetchOutput> {
        let output = match kind {
            TaskKind::Comments => Self::parse_comments(ctx),
            _ => Self::parse_search(ctx),
        };
        Ok(output)
    }
}
