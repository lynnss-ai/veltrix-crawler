//! 小红书平台适配器。
//!
//! 解析网页搜索接口 `/search/notes` 的响应(改版后为 `/api/sns/web/v2/search/notes`,明文 JSON):
//! `data.items[]` 每项 `model_type=note` 含 `note_card`(笔记详情),抽取为统一 Content。
//! `model_type=hot_query`(大家都在搜)等非笔记项跳过。
//!
//! 搜索卡通常只给标题/封面/作者/互动数,正文、话题与视频流需由详情接口补齐。
//! 搜索解析仍按响应实际字段尽力提取;详情响应则解析完整 `note_card`,由采集流程逐条合并回库。
//! 视频无水印直链按 `note_card.video.media.stream` 解析。互动数为字符串需转 i64,
//! 发布时间兼容详情毫秒时间戳与搜索卡 `corner_tag_info` 日期文本。

use crate::adapter::{FetchContext, FetchOutput, PlatformAdapter};
use crate::model::{Author, Comment, Content, ContentKind, Stats, TaskKind};
use async_trait::async_trait;
use chrono::{Datelike, NaiveDate, Utc};
use serde_json::Value;
use veltrix_core::error::Result;

const PLATFORM_ID: &str = "xhs";
/// 搜索接口 URL 特征,与平台配置 intercept_patterns 对应。
/// 用版本无关子串 `/search/notes`:小红书改版后笔记搜索从 v1(edith)迁到 v2(so.xiaohongshu.com),
/// 路径 `/api/sns/web/v2/search/notes`;子串匹配同时兼容 v1/v2 与不同子域。
const SEARCH_PATH: &str = "/search/notes";
/// 一级评论接口 URL 特征;真实路径需本机抓包核对。子评论接口路径不同(comment/sub/page)不会命中。
const COMMENT_PATH: &str = "/api/sns/web/v2/comment/page";
/// 作者主页用户信息接口 URL 特征(画像补采);真实路径需本机抓包核对。
const PROFILE_PATH: &str = "/api/sns/web/v1/user/otherinfo";
/// 笔记详情(feed)接口 URL 特征:详情卡才含 `video.media.stream`,供补取视频直链。
const FEED_PATH: &str = "/api/sns/web/v1/feed";

#[derive(Default)]
pub struct XhsAdapter;

impl XhsAdapter {
    pub fn new() -> Self {
        Self
    }

    /// 把搜索/详情 item 的 `note_card` 解析为完整 Content。
    /// 搜索列表须校验 model_type,避免把 hot_query / 广告误当笔记;详情 feed 可不带该字段。
    fn parse_item(item: &Value, collected_at: i64, require_note_type: bool) -> Option<Content> {
        if require_note_type && item.get("model_type").and_then(Value::as_str) != Some("note") {
            return None;
        }
        let card = item.get("note_card")?;
        let content_id = item
            .get("id")
            .or_else(|| card.get("note_id"))
            .and_then(Value::as_str)?
            .to_string();
        if content_id.is_empty() {
            return None;
        }

        // type=video 为视频,其余(normal)按图文
        let kind = if card.get("type").and_then(Value::as_str) == Some("video") {
            ContentKind::Video
        } else {
            ContentKind::Image
        };

        let image_urls = Self::parse_images(card.get("image_list"));
        // 封面:兼容搜索卡 url_default 与详情卡 info_list,缺失退回首图。
        let cover_url = card
            .get("cover")
            .and_then(Self::parse_image_url)
            .or_else(|| image_urls.first().cloned());

        Some(Content {
            platform: PLATFORM_ID.to_string(),
            content_id,
            kind,
            // 搜索卡多为 display_title,详情卡多为 title。
            title: card
                .get("title")
                .or_else(|| card.get("display_title"))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string),
            desc: card
                .get("desc")
                .or_else(|| card.get("description"))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string),
            author: Self::parse_author(card.get("user")),
            stats: Self::parse_stats(card.get("interact_info")),
            published_at: Self::parse_published(card),
            // 搜索卡通常不含直链/正文/话题;字段为空时采集流程会打开详情页补全。
            video_url: Self::parse_video_stream(card),
            cover_url,
            image_urls,
            duration: Self::parse_duration(card),
            topics: Self::parse_topics(card),
            collected_at,
            extra: serde_json::json!({
                // 笔记/作者的 xsec_token:打开详情/主页需要,先留存
                "xsec_token": item.get("xsec_token").and_then(Value::as_str)
                    .filter(|s| !s.trim().is_empty())
                    .or_else(|| card.get("xsec_token").and_then(Value::as_str)
                        .filter(|s| !s.trim().is_empty())),
                "author_xsec_token": card
                    .get("user")
                    .and_then(|u| u.get("xsec_token"))
                    .and_then(Value::as_str),
            }),
        })
    }

    fn parse_author(value: Option<&Value>) -> Author {
        let Some(u) = value else {
            return Author::default();
        };
        Author {
            platform: PLATFORM_ID.to_string(),
            uid: u
                .get("user_id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            nickname: u
                .get("nickname")
                .or_else(|| u.get("nick_name"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            avatar: u.get("avatar").and_then(Value::as_str).map(str::to_string),
            signature: None,
            follower_count: None,
            following_count: None,
            extra: Value::Null,
        }
    }

    fn parse_stats(value: Option<&Value>) -> Stats {
        let Some(s) = value else {
            return Stats::default();
        };
        // 互动数是字符串(如 "475"),转 i64
        let num = |key: &str| {
            s.get(key)
                .and_then(Value::as_str)
                .and_then(|v| v.parse::<i64>().ok())
                .or_else(|| s.get(key).and_then(Value::as_i64))
        };
        Stats {
            like_count: num("liked_count"),
            comment_count: num("comment_count"),
            collect_count: num("collected_count"),
            share_count: num("shared_count"),
            play_count: None,
        }
    }

    /// 单张图直链:优先 `WB_DFT`(默认大图),否则兼容 url_default / url_pre / info_list 首个。
    fn parse_image_url(image: &Value) -> Option<String> {
        let from_infos = || {
            let infos = image.get("info_list").and_then(Value::as_array)?;
            infos
                .iter()
                .find(|i| i.get("image_scene").and_then(Value::as_str) == Some("WB_DFT"))
                .or_else(|| infos.first())
                .and_then(|i| i.get("url").and_then(Value::as_str))
        };
        from_infos()
            .or_else(|| image.get("url_default").and_then(Value::as_str))
            .or_else(|| image.get("url_pre").and_then(Value::as_str))
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    }

    /// 取每张图的直链;详情图与搜索图字段形态不同,统一走 parse_image_url。
    fn parse_images(image_list: Option<&Value>) -> Vec<String> {
        image_list
            .and_then(Value::as_array)
            .map(|arr| arr.iter().filter_map(Self::parse_image_url).collect())
            .unwrap_or_default()
    }

    /// 详情卡话题在 tag_list/topic_list 中;少数响应只把 `#话题[话题]#` 写进正文,
    /// 因此结构化字段为空时再从正文兜底,并统一成 `#名称` 去重。
    fn parse_topics(card: &Value) -> Vec<String> {
        let mut topics = Vec::new();
        for key in ["tag_list", "topic_list"] {
            let Some(items) = card.get(key).and_then(Value::as_array) else {
                continue;
            };
            for item in items {
                let name = item
                    .get("name")
                    .or_else(|| item.get("title"))
                    .or_else(|| item.get("topic_name"))
                    .and_then(Value::as_str);
                Self::push_topic(&mut topics, name.unwrap_or_default());
            }
        }
        if topics.is_empty() {
            let desc = card
                .get("desc")
                .or_else(|| card.get("description"))
                .and_then(Value::as_str)
                .unwrap_or_default();
            for fragment in desc.split('#').skip(1) {
                let raw: String = fragment
                    .chars()
                    .take_while(|c| {
                        !c.is_whitespace()
                            && !matches!(
                                c,
                                ',' | '，' | '。' | '！' | '!' | '？' | '?' | ';' | '；'
                            )
                    })
                    .collect();
                Self::push_topic(&mut topics, &raw);
            }
        }
        topics
    }

    fn push_topic(topics: &mut Vec<String>, raw: &str) {
        let name = raw
            .trim()
            .trim_matches('#')
            .trim_end_matches("[话题]")
            .trim();
        if name.is_empty() {
            return;
        }
        let topic = format!("#{name}");
        if !topics.contains(&topic) {
            topics.push(topic);
        }
    }

    /// 视频时长字段随详情版本变化,兼容常见路径;大于一千按毫秒归一为秒。
    fn parse_duration(card: &Value) -> Option<i64> {
        let raw = [
            "/video/capa/duration",
            "/video/media/video/duration",
            "/video/duration",
            "/duration",
        ]
        .into_iter()
        .find_map(|path| {
            let value = card.pointer(path)?;
            value
                .as_i64()
                .or_else(|| value.as_str().and_then(|text| text.parse::<i64>().ok()))
        })?;
        Some(if raw >= 1_000 { raw / 1_000 } else { raw })
    }

    /// 视频无水印直链:`note_card.video.media.stream` 下按编码优先级 h264→h265→av1 取首个可用流,
    /// 每个流再多重兜底 `master_url`→`url`→`consumer.master_url`→`url_pre`。
    /// 注意:搜索接口的 note_card 通常不含 `video.media`(只给封面),需走笔记详情才有直链;取不到返回 None。
    fn parse_video_stream(card: &Value) -> Option<String> {
        let stream = card.get("video")?.get("media")?.get("stream")?;
        for codec in ["h264", "h265", "av1"] {
            let Some(items) = stream.get(codec).and_then(Value::as_array) else {
                continue;
            };
            for item in items {
                let url = item
                    .get("master_url")
                    .and_then(Value::as_str)
                    .or_else(|| item.get("url").and_then(Value::as_str))
                    .or_else(|| {
                        item.get("consumer")
                            .and_then(|c| c.get("master_url"))
                            .and_then(Value::as_str)
                    })
                    .or_else(|| item.get("url_pre").and_then(Value::as_str))
                    .filter(|s| !s.is_empty());
                if let Some(found) = url {
                    return Some(found.to_string());
                }
            }
        }
        None
    }

    /// 发布时间:`corner_tag_info` 里 `type=publish_time` 的 text,
    /// 形如 `MM-DD`(当年)或 `YYYY-MM-DD`,解析为当天 0 点的 Unix 秒。
    fn parse_published(card: &Value) -> Option<i64> {
        if let Some(raw) = card
            .get("time")
            .or_else(|| card.get("create_time"))
            .and_then(|v| {
                v.as_i64()
                    .or_else(|| v.as_str().and_then(|text| text.parse::<i64>().ok()))
            })
        {
            return Some(if raw > 1_000_000_000_000 {
                raw / 1_000
            } else {
                raw
            });
        }
        let text = card
            .get("corner_tag_info")
            .and_then(Value::as_array)?
            .iter()
            .find(|t| t.get("type").and_then(Value::as_str) == Some("publish_time"))
            .and_then(|t| t.get("text").and_then(Value::as_str))?;

        let parts: Vec<&str> = text.split('-').collect();
        let date = match parts.len() {
            3 => NaiveDate::from_ymd_opt(
                parts[0].parse::<i32>().ok()?,
                parts[1].parse::<u32>().ok()?,
                parts[2].parse::<u32>().ok()?,
            )?,
            2 => {
                // 「MM-DD」无年份:先按当年算,若得到未来日期(如 1 月采到 12-28 的去年笔记),
                // 回退到去年,避免发布时间晚于采集时间。
                let month = parts[0].parse::<u32>().ok()?;
                let day = parts[1].parse::<u32>().ok()?;
                let today = Utc::now().date_naive();
                let this_year = NaiveDate::from_ymd_opt(today.year(), month, day)?;
                if this_year > today {
                    NaiveDate::from_ymd_opt(today.year() - 1, month, day)?
                } else {
                    this_year
                }
            }
            _ => return None,
        };
        Some(date.and_hms_opt(0, 0, 0)?.and_utc().timestamp())
    }

    /// 解析搜索接口响应为笔记内容列表(comments 恒空)。
    fn parse_search(ctx: &FetchContext) -> FetchOutput {
        let collected_at = Utc::now().timestamp();
        let mut contents = Vec::new();
        for resp in &ctx.responses {
            if !resp.url.contains(SEARCH_PATH) {
                continue;
            }
            let Ok(root) = serde_json::from_str::<Value>(&resp.body) else {
                continue;
            };
            let Some(items) = root
                .get("data")
                .and_then(|d| d.get("items"))
                .and_then(Value::as_array)
            else {
                continue;
            };
            for item in items {
                if let Some(content) = Self::parse_item(item, collected_at, true) {
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

    /// 解析作者主页画像(authors 仅一条;contents/comments 恒空)。
    /// 主页请求 `/api/sns/web/v1/user/otherinfo`:基础信息在 `data.basic_info`
    /// (nickname/images头像/desc签名/red_id小红书号/ip_location),粉丝/关注/获赞与收藏在
    /// `data.interactions[]`(按 type=fans/follows/interaction 区分,count 为字符串可能带万/亿)。
    /// uid 用导航时的 user_id(ctx.keyword)。
    fn parse_profile(ctx: &FetchContext) -> FetchOutput {
        let uid = ctx.keyword.clone();
        let mut authors = Vec::new();
        for resp in &ctx.responses {
            if !resp.url.contains(PROFILE_PATH) {
                continue;
            }
            let Ok(root) = serde_json::from_str::<Value>(&resp.body) else {
                continue;
            };
            let Some(data) = root.get("data") else {
                continue;
            };
            let basic = data.get("basic_info");
            let basic_str = |key: &str| {
                basic
                    .and_then(|b| b.get(key))
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
            };
            let (mut follower, mut following, mut favorited) = (None, None, None);
            if let Some(arr) = data.get("interactions").and_then(Value::as_array) {
                for it in arr {
                    let count = it
                        .get("count")
                        .and_then(Value::as_str)
                        .and_then(Self::parse_cn_count)
                        .or_else(|| it.get("count").and_then(Value::as_i64));
                    match it.get("type").and_then(Value::as_str) {
                        Some("fans") => follower = count,
                        Some("follows") => following = count,
                        Some("interaction") => favorited = count,
                        _ => {}
                    }
                }
            }
            authors.push(Author {
                platform: PLATFORM_ID.to_string(),
                uid: uid.clone(),
                nickname: basic_str("nickname").unwrap_or_default(),
                avatar: basic_str("images"),
                signature: basic_str("desc"),
                follower_count: follower,
                following_count: following,
                extra: serde_json::json!({
                    "unique_id": basic_str("red_id"),
                    "ip_location": basic_str("ip_location"),
                    "total_favorited": favorited,
                }),
            });
            break; // 一个主页只取一条画像
        }
        FetchOutput {
            contents: Vec::new(),
            comments: Vec::new(),
            authors,
        }
    }

    /// 解析笔记详情(feed)接口响应为完整内容。图文与视频都保留,正文、话题、图集、
    /// 发布时间及视频流由调用方合并到搜索卡并回写数据库。
    fn parse_detail(ctx: &FetchContext) -> FetchOutput {
        let collected_at = Utc::now().timestamp();
        let mut contents = Vec::new();
        for resp in &ctx.responses {
            if !resp.url.contains(FEED_PATH) {
                continue;
            }
            let Ok(root) = serde_json::from_str::<Value>(&resp.body) else {
                continue;
            };
            let Some(items) = root
                .get("data")
                .and_then(|d| d.get("items"))
                .and_then(Value::as_array)
            else {
                continue;
            };
            for item in items {
                if let Some(content) = Self::parse_item(item, collected_at, false) {
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
    fn parse_comments(ctx: &FetchContext) -> FetchOutput {
        let collected_at = Utc::now().timestamp();
        let mut comments = Vec::new();
        for resp in &ctx.responses {
            if !resp.url.contains(COMMENT_PATH) {
                continue;
            }
            let Ok(root) = serde_json::from_str::<Value>(&resp.body) else {
                continue;
            };
            let Some(items) = root
                .get("data")
                .and_then(|d| d.get("comments"))
                .and_then(Value::as_array)
            else {
                continue;
            };
            for item in items {
                if let Some(comment) = Self::parse_comment(item, &ctx.keyword, collected_at) {
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

    /// 把单条评论解析为 Comment;缺 id 返回 None。只采一级评论,parent_id 恒为 None。
    fn parse_comment(
        item: &Value,
        fallback_content_id: &str,
        collected_at: i64,
    ) -> Option<Comment> {
        let comment_id = item.get("id").and_then(Value::as_str)?.to_string();
        if comment_id.is_empty() {
            return None;
        }
        let content_id = item
            .get("note_id")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .unwrap_or(fallback_content_id)
            .to_string();
        // 互动数小红书多为字符串
        let num = |key: &str| {
            item.get(key)
                .and_then(Value::as_str)
                .and_then(|v| v.parse::<i64>().ok())
                .or_else(|| item.get(key).and_then(Value::as_i64))
        };
        // create_time 小红书多为毫秒;>1e12 视为毫秒,统一转存秒
        let created_at = item.get("create_time").and_then(Value::as_i64).map(|t| {
            if t > 1_000_000_000_000 {
                t / 1000
            } else {
                t
            }
        });
        let user = item.get("user_info");
        let author = Author {
            platform: PLATFORM_ID.to_string(),
            uid: user
                .and_then(|u| u.get("user_id"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            nickname: user
                .and_then(|u| u.get("nickname"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            avatar: user
                .and_then(|u| u.get("image"))
                .and_then(Value::as_str)
                .map(str::to_string),
            signature: None,
            follower_count: None,
            following_count: None,
            extra: Value::Null,
        };
        Some(Comment {
            platform: PLATFORM_ID.to_string(),
            content_id,
            comment_id,
            parent_id: None,
            author,
            text: item
                .get("content")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            like_count: num("like_count"),
            reply_count: num("sub_comment_count"),
            created_at,
            collected_at,
            extra: serde_json::json!({
                "ip_location": item.get("ip_location").and_then(Value::as_str),
            }),
        })
    }
}

#[async_trait]
impl PlatformAdapter for XhsAdapter {
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
        // 按任务类型分流:评论解析一级评论,画像补采解析主页,详情补取解析视频直链,其余按搜索笔记解析
        let output = match kind {
            TaskKind::Comments => Self::parse_comments(ctx),
            TaskKind::UserProfile => Self::parse_profile(ctx),
            TaskKind::ContentDetail => Self::parse_detail(ctx),
            _ => Self::parse_search(ctx),
        };
        Ok(output)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::webview::InterceptedResponse;
    use serde_json::json;

    #[test]
    fn parses_complete_note_detail() {
        let ctx = FetchContext {
            keyword: "note-1".into(),
            responses: vec![InterceptedResponse {
                url: "https://edith.xiaohongshu.com/api/sns/web/v1/feed".into(),
                body: json!({
                    "data": {
                        "items": [{
                            "id": "note-1",
                            "note_card": {
                                "type": "normal",
                                "title": "完整标题",
                                "desc": "完整正文 #正文兜底[话题]#",
                                "time": 1_725_000_000_000_i64,
                                "user": {
                                    "user_id": "user-1",
                                    "nickname": "作者",
                                    "avatar": "https://img/avatar.jpg"
                                },
                                "interact_info": {
                                    "liked_count": "123",
                                    "comment_count": "8",
                                    "collected_count": "16",
                                    "shared_count": "2"
                                },
                                "image_list": [
                                    {"url_default": "https://img/1.jpg"},
                                    {"info_list": [
                                        {"image_scene": "WB_PRV", "url": "https://img/2-small.jpg"},
                                        {"image_scene": "WB_DFT", "url": "https://img/2.jpg"}
                                    ]},
                                    {"url_pre": "https://img/3.jpg"}
                                ],
                                "tag_list": [
                                    {"name": "旅行"},
                                    {"name": "云南"},
                                    {"name": "旅行"}
                                ]
                            }
                        }]
                    }
                })
                .to_string(),
            }],
        };

        let output = XhsAdapter::parse_detail(&ctx);
        assert_eq!(output.contents.len(), 1);
        let content = &output.contents[0];
        assert_eq!(content.content_id, "note-1");
        assert_eq!(content.title.as_deref(), Some("完整标题"));
        assert_eq!(content.desc.as_deref(), Some("完整正文 #正文兜底[话题]#"));
        assert_eq!(content.topics, vec!["#旅行", "#云南"]);
        assert_eq!(
            content.image_urls,
            vec![
                "https://img/1.jpg",
                "https://img/2.jpg",
                "https://img/3.jpg",
            ]
        );
        assert_eq!(content.published_at, Some(1_725_000_000));
    }

    #[test]
    fn parses_video_stream_for_shared_audio_pipeline() {
        let ctx = FetchContext {
            keyword: "video-1".into(),
            responses: vec![InterceptedResponse {
                url: "https://edith.xiaohongshu.com/api/sns/web/v1/feed".into(),
                body: json!({
                    "data": {
                        "items": [{
                            "id": "video-1",
                            "note_card": {
                                "type": "video",
                                "title": "视频笔记",
                                "video": {
                                    "media": {
                                        "stream": {
                                            "h264": [{
                                                "master_url": "https://sns-video-hw.xhscdn.com/video.mp4"
                                            }]
                                        }
                                    }
                                },
                                "image_list": [{"url_default": "https://img/cover.jpg"}]
                            }
                        }]
                    }
                }).to_string(),
            }],
        };

        let output = XhsAdapter::parse_detail(&ctx);
        assert_eq!(output.contents.len(), 1);
        let content = &output.contents[0];
        assert!(matches!(content.kind, ContentKind::Video));
        assert_eq!(
            content.video_url.as_deref(),
            Some("https://sns-video-hw.xhscdn.com/video.mp4")
        );
    }

    #[test]
    fn extracts_topics_from_description_when_structured_tags_are_absent() {
        let card = json!({
            "desc": "周末出发 #丽江[话题]# #旅行攻略[话题]#，走起"
        });
        assert_eq!(XhsAdapter::parse_topics(&card), vec!["#丽江", "#旅行攻略"]);
    }

    #[test]
    fn comment_uses_context_note_id_when_response_omits_it() {
        let ctx = FetchContext {
            keyword: "note-from-context".into(),
            responses: vec![InterceptedResponse {
                url: "https://edith.xiaohongshu.com/api/sns/web/v2/comment/page".into(),
                body: json!({
                    "data": { "comments": [{
                        "id": "comment-1",
                        "content": "评论正文",
                        "user_info": { "user_id": "user-1", "nickname": "用户" }
                    }] }
                })
                .to_string(),
            }],
        };
        let output = XhsAdapter::parse_comments(&ctx);
        assert_eq!(output.comments.len(), 1);
        assert_eq!(output.comments[0].content_id, "note-from-context");
    }
}
