//! 快手平台适配器。
//!
//! 快手 Web 端搜索实测走 REST(`POST /rest/v/search/feed`),响应 `feeds[]` 直接在
//! 根级(不是 GraphQL 的 `data.visionSearchPhoto.feeds`),每项含 `photo`(内容详情)与
//! `author`;视频直链是 `photo.photoUrls[]`(数组,每项 `{cdn,url}`),封面 `photo.coverUrl`
//! 为字符串。评论走 GraphQL(`POST /graphql`,operationName `commentListQuery`),一级评论在
//! `data.visionCommentList.rootCommentsV2`(旧 `rootComments` 已迁空);评论项不含 photoId,
//! 所属内容 id 由采集上下文(`ctx.keyword`)传入。搜索/评论按响应体字段(`feeds` /
//! `visionCommentList`)区分,共用一套拦截特征(`/rest/v/search/feed` + `/graphql`)。
//!
//! ⚠️ 解析全程 `serde_json::Value` 按需取值,单条脏数据只跳过、不拖垮整批;为兼容历史
//! 结构,关键路径都保留旧字段兜底。

use crate::adapter::{FetchContext, FetchOutput, PlatformAdapter};
use crate::model::{Author, Comment, Content, ContentKind, Stats, TaskKind};
use async_trait::async_trait;
use chrono::Utc;
use serde_json::Value;
use veltrix_core::error::Result;

const PLATFORM_ID: &str = "kuaishou";
/// 毫秒判定阈值:大于该值视为毫秒级时间戳,统一转秒。
const MS_THRESHOLD: i64 = 1_000_000_000_000;

#[derive(Default)]
pub struct KuaishouAdapter;

impl KuaishouAdapter {
    pub fn new() -> Self {
        Self
    }

    /// 数字字段容错:快手 GraphQL 的计数可能是数字或字符串。
    fn num(value: Option<&Value>) -> Option<i64> {
        let v = value?;
        v.as_i64()
            .or_else(|| v.as_str().and_then(|s| s.parse::<i64>().ok()))
    }

    /// id 字段容错:可能是字符串或数字,统一成 String;空返回空串。
    fn as_string(value: Option<&Value>) -> String {
        match value {
            Some(Value::String(s)) => s.clone(),
            Some(Value::Number(n)) => n.to_string(),
            _ => String::new(),
        }
    }

    /// 同 as_string,但空串归一为 None(用于必填 id 判空跳过)。
    fn as_string_opt(value: Option<&Value>) -> Option<String> {
        let s = Self::as_string(value);
        if s.is_empty() {
            None
        } else {
            Some(s)
        }
    }

    /// 毫秒/秒兼容:超过阈值视为毫秒转秒。
    fn to_secs(v: Option<i64>) -> Option<i64> {
        v.map(|t| if t > MS_THRESHOLD { t / 1000 } else { t })
    }

    /// 把单个 feed(含 photo + author)解析为 Content;缺 photo.id 视为无效返回 None。
    fn parse_feed(feed: &Value, collected_at: i64) -> Option<Content> {
        // 兼容 feed.photo 包裹与 feed 本身即详情两种形态
        let photo = feed.get("photo").unwrap_or(feed);
        let content_id = Self::as_string_opt(
            photo.get("id").or_else(|| photo.get("photoId")),
        )?;

        let video_url = Self::first_video_url(photo);
        // 图集图片(快手图文);视频内容通常为空
        let image_urls = Self::parse_image_urls(photo);
        // 有视频直链按视频,否则若有图片按图文,再否则未知
        let kind = if video_url.is_some() {
            ContentKind::Video
        } else if !image_urls.is_empty() {
            ContentKind::Image
        } else {
            ContentKind::Unknown
        };

        let cover_url = photo
            .get("coverUrl")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .or_else(|| image_urls.first().cloned());

        Some(Content {
            platform: PLATFORM_ID.to_string(),
            content_id,
            kind,
            title: None, // 快手无独立标题,正文在 caption
            desc: photo
                .get("caption")
                .and_then(Value::as_str)
                .map(str::to_string)
                .filter(|s| !s.is_empty()),
            author: Self::parse_author(feed.get("author")),
            stats: Self::parse_stats(photo),
            published_at: Self::to_secs(Self::num(photo.get("timestamp"))),
            video_url,
            cover_url,
            image_urls,
            duration: Self::duration_secs(photo),
            topics: Self::parse_topics(feed),
            collected_at,
            extra: serde_json::json!({
                "expTag": photo.get("expTag").and_then(Value::as_str),
            }),
        })
    }

    /// 话题:快手 feed.tags[].name(若存在);统一加 # 前缀。
    fn parse_topics(feed: &Value) -> Vec<String> {
        feed.get("tags")
            .and_then(Value::as_array)
            .map(|arr| {
                arr.iter()
                    .filter_map(|t| t.get("name").and_then(Value::as_str))
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
    }

    /// 取视频无水印直链。快手 REST 响应里直链是 `photoUrls`(数组,每项 `{cdn,url}`),
    /// 优先取首个可用 url;再兜底 H265 数组、历史字符串字段(`photoUrl`/`photoH265Url`)与
    /// 老接口的 `mainMvUrls[].url`。任一命中即返回,供后续拉流转音频。
    fn first_video_url(photo: &Value) -> Option<String> {
        for key in ["photoUrls", "photoH265Urls", "mainMvUrls"] {
            if let Some(url) = photo
                .get(key)
                .and_then(Value::as_array)
                .and_then(|arr| {
                    arr.iter()
                        .filter_map(|item| item.get("url").and_then(Value::as_str))
                        .find(|s| !s.is_empty())
                })
            {
                return Some(url.to_string());
            }
        }
        photo
            .get("photoUrl")
            .and_then(Value::as_str)
            .or_else(|| photo.get("photoH265Url").and_then(Value::as_str))
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    }

    /// 视频时长:快手 `photo.duration` 为毫秒,统一转秒(向下取整);非正值视为无。
    /// 不能复用 to_secs——其毫秒判定阈值(1e12)远大于时长,会把毫秒当秒。
    fn duration_secs(photo: &Value) -> Option<i64> {
        Self::num(photo.get("duration"))
            .map(|ms| ms / 1000)
            .filter(|secs| *secs > 0)
    }

    /// 图集图片:快手图文在 photo.imgUrls(直链数组,推测;抓包后微调)。视频内容为空。
    fn parse_image_urls(photo: &Value) -> Vec<String> {
        photo
            .get("imgUrls")
            .and_then(Value::as_array)
            .map(|arr| {
                arr.iter()
                    .filter_map(|u| u.as_str().filter(|s| !s.is_empty()).map(str::to_string))
                    .collect()
            })
            .unwrap_or_default()
    }

    fn parse_author(value: Option<&Value>) -> Author {
        let Some(a) = value else {
            return Author::default();
        };
        Author {
            platform: PLATFORM_ID.to_string(),
            uid: Self::as_string(a.get("id")),
            nickname: a
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            avatar: a
                .get("headerUrl")
                .and_then(Value::as_str)
                .or_else(|| a.get("headurl").and_then(Value::as_str))
                .map(str::to_string),
            signature: None,
            follower_count: None,
            following_count: None,
            extra: serde_json::json!({}),
        }
    }

    fn parse_stats(photo: &Value) -> Stats {
        Stats {
            like_count: Self::num(photo.get("likeCount")),
            // 快手搜索响应不含评论/分享数,缺失即 None;收藏数在 photo.collectCount
            comment_count: Self::num(photo.get("commentCount")),
            collect_count: Self::num(photo.get("collectCount")),
            share_count: Self::num(photo.get("shareCount")),
            play_count: Self::num(photo.get("viewCount")),
        }
    }

    /// 把单条评论解析为 Comment;缺 commentId 视为无效返回 None。本期只采一级评论。
    /// content_id 由采集上下文传入——评论项自身不含 photoId。
    fn parse_comment(item: &Value, content_id: &str, collected_at: i64) -> Option<Comment> {
        let comment_id = Self::as_string_opt(item.get("commentId"))?;
        Some(Comment {
            platform: PLATFORM_ID.to_string(),
            content_id: content_id.to_string(),
            comment_id,
            parent_id: None,
            author: Author {
                platform: PLATFORM_ID.to_string(),
                uid: Self::as_string(item.get("authorId")),
                nickname: item
                    .get("authorName")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                avatar: item.get("headurl").and_then(Value::as_str).map(str::to_string),
                signature: None,
                follower_count: None,
                following_count: None,
                extra: serde_json::json!({}),
            },
            text: item
                .get("content")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            like_count: Self::num(item.get("likedCount")),
            reply_count: Self::num(item.get("subCommentCount")),
            created_at: Self::to_secs(Self::num(item.get("timestamp"))),
            collected_at,
            extra: serde_json::json!({}),
        })
    }

    /// 解析搜索响应为内容列表(comments 恒空)。按响应体里的 visionSearchPhoto 识别。
    fn parse_search(ctx: &FetchContext) -> FetchOutput {
        let collected_at = Utc::now().timestamp();
        let mut contents = Vec::new();
        for resp in &ctx.responses {
            let Ok(root) = serde_json::from_str::<Value>(&resp.body) else {
                continue;
            };
            // REST(/rest/v/search/feed)的 feeds 在响应根级;兼容历史 GraphQL
            // 结构(data.visionSearchPhoto.feeds)作兜底。
            let feeds = root
                .get("feeds")
                .or_else(|| {
                    root.get("data")
                        .and_then(|d| d.get("visionSearchPhoto"))
                        .and_then(|v| v.get("feeds"))
                })
                .and_then(Value::as_array);
            let Some(feeds) = feeds else {
                continue;
            };
            for feed in feeds {
                if let Some(content) = Self::parse_feed(feed, collected_at) {
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

    /// 中文计数容错:"1234" / "1.2万" / "3.4亿" → i64;不可解析返回 None。
    fn parse_cn_count(text: &str) -> Option<i64> {
        let t = text.trim().replace(',', "");
        if t.is_empty() {
            return None;
        }
        let numeric: String = t
            .chars()
            .take_while(|c| c.is_ascii_digit() || *c == '.')
            .collect();
        if numeric.is_empty() {
            return None;
        }
        let base: f64 = numeric.parse().ok()?;
        let rest = &t[numeric.len()..];
        let mult = if rest.starts_with('万') {
            1e4
        } else if rest.starts_with('亿') {
            1e8
        } else {
            1.0
        };
        Some((base * mult) as i64)
    }

    /// 解析作者主页画像(authors 仅一条;contents/comments 恒空)。主页画像走 GraphQL
    /// `visionProfile`:`data.visionProfile.userProfile.{profile:{user_name,headurl,user_text},
    /// ownerCount:{fan,follow,photo_public}}`。fan/follow 可能是带万/亿的字符串。快手主页无
    /// 获赞总数/属地,留空。uid 用导航时的 userId(ctx.keyword)。
    fn parse_profile(ctx: &FetchContext) -> FetchOutput {
        let uid = ctx.keyword.clone();
        let mut authors = Vec::new();
        for resp in &ctx.responses {
            let Ok(root) = serde_json::from_str::<Value>(&resp.body) else {
                continue;
            };
            let Some(up) = root
                .get("data")
                .and_then(|d| d.get("visionProfile"))
                .and_then(|v| v.get("userProfile"))
            else {
                continue;
            };
            let profile = up.get("profile");
            let owner_count = up.get("ownerCount");
            let count = |key: &str| -> Option<i64> {
                let v = owner_count?.get(key)?;
                v.as_i64().or_else(|| v.as_str().and_then(Self::parse_cn_count))
            };
            let profile_str = |key: &str| {
                profile
                    .and_then(|p| p.get(key))
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
            };
            authors.push(Author {
                platform: PLATFORM_ID.to_string(),
                uid: uid.clone(),
                nickname: profile_str("user_name").unwrap_or_default(),
                avatar: profile_str("headurl"),
                signature: profile_str("user_text"),
                follower_count: count("fan"),
                following_count: count("follow"),
                extra: serde_json::json!({}),
            });
            break; // 一个主页只取一条画像
        }
        FetchOutput {
            contents: Vec::new(),
            comments: Vec::new(),
            authors,
        }
    }

    /// 解析内容详情(graphql `visionVideoDetail`)为单条内容,主要拿新鲜视频直链。
    /// 详情响应 `data.visionVideoDetail.photo`(结构同搜索 feed 的 photo);取不到直链则跳过。
    fn parse_detail(ctx: &FetchContext) -> FetchOutput {
        let collected_at = Utc::now().timestamp();
        let mut contents = Vec::new();
        for resp in &ctx.responses {
            let Ok(root) = serde_json::from_str::<Value>(&resp.body) else {
                continue;
            };
            let Some(photo) = root
                .get("data")
                .and_then(|d| d.get("visionVideoDetail"))
                .and_then(|v| v.get("photo"))
            else {
                continue;
            };
            let Some(content_id) =
                Self::as_string_opt(photo.get("id").or_else(|| photo.get("photoId")))
            else {
                continue;
            };
            let Some(video_url) = Self::first_video_url(photo) else {
                continue;
            };
            contents.push(Content {
                platform: PLATFORM_ID.to_string(),
                content_id,
                kind: ContentKind::Video,
                video_url: Some(video_url),
                collected_at,
                ..Default::default()
            });
        }
        FetchOutput {
            contents,
            comments: Vec::new(),
            authors: Vec::new(),
        }
    }

    /// 解析一级评论响应为评论列表(contents 恒空)。按响应体里的 visionCommentList 识别。
    fn parse_comments(ctx: &FetchContext) -> FetchOutput {
        let collected_at = Utc::now().timestamp();
        // 评论场景下采集上下文的 keyword 即所属内容 id(评论项自身不含 photoId)
        let content_id = ctx.keyword.as_str();
        let mut comments = Vec::new();
        for resp in &ctx.responses {
            let Ok(root) = serde_json::from_str::<Value>(&resp.body) else {
                continue;
            };
            let list = root.get("data").and_then(|d| d.get("visionCommentList"));
            // 快手已把一级评论迁到 rootCommentsV2,旧 rootComments 多为空;V2 优先、旧字段兜底。
            let items = list
                .and_then(|v| v.get("rootCommentsV2"))
                .and_then(Value::as_array)
                .filter(|arr| !arr.is_empty())
                .or_else(|| {
                    list.and_then(|v| v.get("rootComments"))
                        .and_then(Value::as_array)
                });
            let Some(items) = items else {
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
impl PlatformAdapter for KuaishouAdapter {
    fn id(&self) -> &str {
        PLATFORM_ID
    }

    fn supports(&self, kind: &TaskKind) -> bool {
        matches!(
            kind,
            TaskKind::Search | TaskKind::Comments | TaskKind::UserProfile | TaskKind::ContentDetail
        )
    }

    async fn parse(&self, kind: &TaskKind, ctx: &FetchContext) -> Result<FetchOutput> {
        let output = match kind {
            TaskKind::Comments => Self::parse_comments(ctx),
            TaskKind::UserProfile => Self::parse_profile(ctx),
            TaskKind::ContentDetail => Self::parse_detail(ctx),
            _ => Self::parse_search(ctx),
        };
        Ok(output)
    }
}
