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
use crate::model::{Author, Content, ContentKind, Stats, TaskKind};
use async_trait::async_trait;
use chrono::{Datelike, NaiveDate, Utc};
use serde_json::Value;
use veltrix_core::error::Result;

const PLATFORM_ID: &str = "xhs";
/// 搜索接口 URL 特征,与平台配置 intercept_patterns 对应。
const SEARCH_PATH: &str = "/api/sns/web/v1/search/notes";

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
}

#[async_trait]
impl PlatformAdapter for XhsAdapter {
    fn id(&self) -> &str {
        PLATFORM_ID
    }

    fn supports(&self, kind: &TaskKind) -> bool {
        matches!(kind, TaskKind::Search)
    }

    async fn parse(&self, _kind: &TaskKind, ctx: &FetchContext) -> Result<FetchOutput> {
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

        Ok(FetchOutput {
            contents,
            comments: Vec::new(),
        })
    }
}
