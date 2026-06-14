//! 小红书平台适配器。
//!
//! 解析网页搜索接口 `/api/sns/web/v1/search/notes` 的响应(明文 JSON):
//! `data.items[]` 每项 `model_type=note` 含 `note_card`(笔记详情),抽取为统一 Content。
//! `model_type=hot_query`(大家都在搜)等非笔记项跳过。
//!
//! 搜索接口的局限(联调须知):**只给**标题/封面/作者/互动数,**不含**正文、
//! 视频无水印直链(video 类型也只有封面)、话题标签、视频时长。故 desc/video_url/
//! topics/duration 均为空,需要时得另走笔记详情接口。互动数为字符串需转 i64,
//! 发布时间是 `corner_tag_info` 里 `MM-DD`(当年)/`YYYY-MM-DD` 文本。

use crate::adapter::{FetchContext, FetchOutput, PlatformAdapter};
use crate::model::{Author, Comment, Content, ContentKind, Stats, TaskKind};
use async_trait::async_trait;
use chrono::{Datelike, NaiveDate, Utc};
use serde_json::Value;
use veltrix_core::error::Result;

const PLATFORM_ID: &str = "xhs";
/// 搜索接口 URL 特征,与平台配置 intercept_patterns 对应。
const SEARCH_PATH: &str = "/api/sns/web/v1/search/notes";
/// 一级评论接口 URL 特征;真实路径需本机抓包核对。子评论接口路径不同(comment/sub/page)不会命中。
const COMMENT_PATH: &str = "/api/sns/web/v2/comment/page";
/// 作者主页用户信息接口 URL 特征(画像补采);真实路径需本机抓包核对。
const PROFILE_PATH: &str = "/api/sns/web/v1/user/otherinfo";

#[derive(Default)]
pub struct XhsAdapter;

impl XhsAdapter {
    pub fn new() -> Self {
        Self
    }

    /// 把单个 item 解析为 Content;非笔记(hot_query 等)或缺 id 返回 None。
    fn parse_item(item: &Value, collected_at: i64) -> Option<Content> {
        // 只认笔记卡;其余 model_type(hot_query/广告等)跳过
        if item.get("model_type").and_then(Value::as_str) != Some("note") {
            return None;
        }
        let content_id = item.get("id").and_then(Value::as_str)?.to_string();
        if content_id.is_empty() {
            return None;
        }
        let card = item.get("note_card")?;

        // type=video 为视频,其余(normal)按图文
        let kind = if card.get("type").and_then(Value::as_str) == Some("video") {
            ContentKind::Video
        } else {
            ContentKind::Image
        };

        let image_urls = Self::parse_images(card.get("image_list"));
        // 封面:cover.url_default,缺失退回首图
        let cover_url = card
            .get("cover")
            .and_then(|c| c.get("url_default"))
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| image_urls.first().cloned());

        Some(Content {
            platform: PLATFORM_ID.to_string(),
            content_id,
            kind,
            // 小红书 display_title 即标题/摘要,放 title;搜索接口无正文 desc
            title: card
                .get("display_title")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(str::to_string),
            desc: None,
            author: Self::parse_author(card.get("user")),
            stats: Self::parse_stats(card.get("interact_info")),
            published_at: Self::parse_published(card),
            // 搜索接口不含视频直链 / 时长 / 话题
            video_url: None,
            cover_url,
            image_urls,
            duration: None,
            topics: Vec::new(),
            collected_at,
            extra: serde_json::json!({
                // 笔记/作者的 xsec_token:打开详情/主页需要,先留存
                "xsec_token": item.get("xsec_token").and_then(Value::as_str),
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

    /// 取每张图的直链:优先 `WB_DFT`(默认大图),否则取 info_list 首个。
    fn parse_images(image_list: Option<&Value>) -> Vec<String> {
        image_list
            .and_then(Value::as_array)
            .map(|arr| {
                arr.iter()
                    .filter_map(|img| {
                        let infos = img.get("info_list").and_then(Value::as_array)?;
                        infos
                            .iter()
                            .find(|i| {
                                i.get("image_scene").and_then(Value::as_str) == Some("WB_DFT")
                            })
                            .or_else(|| infos.first())
                            .and_then(|i| i.get("url").and_then(Value::as_str))
                            .map(str::to_string)
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// 发布时间:`corner_tag_info` 里 `type=publish_time` 的 text,
    /// 形如 `MM-DD`(当年)或 `YYYY-MM-DD`,解析为当天 0 点的 Unix 秒。
    fn parse_published(card: &Value) -> Option<i64> {
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
                if let Some(content) = Self::parse_item(item, collected_at) {
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

    /// 把单条评论解析为 Comment;缺 id 返回 None。只采一级评论,parent_id 恒为 None。
    fn parse_comment(item: &Value, collected_at: i64) -> Option<Comment> {
        let comment_id = item.get("id").and_then(Value::as_str)?.to_string();
        if comment_id.is_empty() {
            return None;
        }
        let content_id = item
            .get("note_id")
            .and_then(Value::as_str)
            .unwrap_or_default()
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
            TaskKind::Search | TaskKind::Comments | TaskKind::UserProfile
        )
    }

    async fn parse(&self, kind: &TaskKind, ctx: &FetchContext) -> Result<FetchOutput> {
        // 按任务类型分流:评论任务解析一级评论,画像补采解析主页,其余按搜索笔记解析
        let output = match kind {
            TaskKind::Comments => Self::parse_comments(ctx),
            TaskKind::UserProfile => Self::parse_profile(ctx),
            _ => Self::parse_search(ctx),
        };
        Ok(output)
    }
}
