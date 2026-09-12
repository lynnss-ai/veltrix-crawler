//! B站(bilibili)平台适配器。
//!
//! 搜索走视频 tab(`GET /x/web-interface/wbi/search/type?search_type=video`),结果在
//! `data.result[]`,每项即视频详情(`bvid`/`title`/`pic`/`play` 等);综合 tab
//! (`wbi/search/all/v2`)的 `data.result[]` 是按 `result_type` 分组的嵌套结构,两种形态都兼容。
//! 请求上的 WBI 签名由页面自己完成,拦截模式天然绕过。
//!
//! 评论走 `GET /x/v2/reply/wbi/main?oid={aid}`,一级评论在 `data.replies[]`(置顶在
//! `data.top_replies[]`);评论项不含 bvid,所属内容 id 由采集上下文(`ctx.keyword`)传入。
//!
//! 搜索响应不含视频流地址,详情页通过 playurl / SSR 的 `__playinfo__` 补取。
//! 优先使用带音频的 durl;只有 DASH 时分别保留视频轨和音频轨,媒体层再统一转码。
//!
//! ⚠️ 字段名基于 Web 端公开结构整理,真实结构需本机 `bun tauri dev` 抓包核对后微调。

use crate::adapter::{FetchContext, FetchOutput, PlatformAdapter};
use crate::model::{Author, Comment, Content, ContentKind, Stats, TaskKind};
use async_trait::async_trait;
use chrono::Utc;
use serde_json::Value;
use veltrix_core::error::Result;

const PLATFORM_ID: &str = "bilibili";
const DETAIL_PATH: &str = "/x/player/";

#[derive(Default)]
pub struct BilibiliAdapter;

impl BilibiliAdapter {
    pub fn new() -> Self {
        Self
    }

    /// 数字字段容错:计数可能是数字或字符串("--" 等非数字字符串返回 None)。
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

    /// 协议相对地址归一:B站图片直链多为 `//i1.hdslb.com/...`,补 https 才能下载。
    fn normalize_url(raw: &str) -> Option<String> {
        let s = raw.trim();
        if s.is_empty() {
            return None;
        }
        if let Some(rest) = s.strip_prefix("//") {
            return Some(format!("https://{rest}"));
        }
        Some(s.to_string())
    }

    /// 去掉搜索标题里的关键词高亮标记(`<em class="keyword">`)等 HTML 标签并反转义常见实体。
    fn strip_html(raw: &str) -> String {
        let mut out = String::with_capacity(raw.len());
        let mut in_tag = false;
        for c in raw.chars() {
            match c {
                '<' => in_tag = true,
                '>' => in_tag = false,
                _ if !in_tag => out.push(c),
                _ => {}
            }
        }
        out.replace("&amp;", "&")
            .replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace("&quot;", "\"")
            .replace("&#39;", "'")
    }

    /// 搜索接口的时长是 "MM:SS" / "HH:MM:SS" 字符串,解析为秒;不可解析返回 None。
    fn parse_clock(value: Option<&Value>) -> Option<i64> {
        let text = value?.as_str()?.trim();
        if text.is_empty() {
            return None;
        }
        let mut total: i64 = 0;
        for part in text.split(':') {
            total = total * 60 + part.trim().parse::<i64>().ok()?;
        }
        (total > 0).then_some(total)
    }

    /// 把单个搜索结果项解析为 Content;缺内容 id(bvid 兜底 aid)视为无效返回 None。
    fn parse_video_item(item: &Value, collected_at: i64) -> Option<Content> {
        // 综合 tab 混排用户/番剧等结果,只认视频(有 bvid;type 字段存在时须为 video)
        if let Some(t) = item.get("type").and_then(Value::as_str) {
            if t != "video" {
                return None;
            }
        }
        let content_id = Self::as_string_opt(item.get("bvid"))
            .or_else(|| Self::as_string_opt(item.get("aid")))?;

        let title = item
            .get("title")
            .and_then(Value::as_str)
            .map(Self::strip_html)
            .filter(|s| !s.is_empty());

        Some(Content {
            platform: PLATFORM_ID.to_string(),
            content_id,
            kind: ContentKind::Video,
            title,
            desc: item
                .get("description")
                .and_then(Value::as_str)
                .map(str::to_string)
                .filter(|s| !s.is_empty()),
            author: Self::parse_search_author(item),
            stats: Stats {
                like_count: Self::num(item.get("like")),
                // 搜索结果的 review 即评论数;video_review 是弹幕数(入 extra)
                comment_count: Self::num(item.get("review")),
                collect_count: Self::num(item.get("favorites")),
                share_count: None,
                play_count: Self::num(item.get("play")),
            },
            published_at: Self::num(item.get("pubdate")),
            // DASH 流在详情页 playurl 接口且带防盗链,v1 不采视频/音频
            video_url: None,
            cover_url: item
                .get("pic")
                .and_then(Value::as_str)
                .and_then(Self::normalize_url),
            image_urls: Vec::new(),
            duration: Self::parse_clock(item.get("duration")),
            topics: Self::parse_topics(item),
            collected_at,
            extra: serde_json::json!({
                "aid": item.get("aid"),
                "typename": item.get("typename").and_then(Value::as_str),
                "danmaku": Self::num(item.get("video_review")),
            }),
        })
    }

    /// 话题:搜索结果 `tag` 为逗号分隔字符串,统一加 # 前缀。
    fn parse_topics(item: &Value) -> Vec<String> {
        item.get("tag")
            .and_then(Value::as_str)
            .map(|raw| {
                raw.split(',')
                    .map(str::trim)
                    .filter(|t| !t.is_empty())
                    .map(|t| {
                        if t.starts_with('#') {
                            t.to_string()
                        } else {
                            format!("#{t}")
                        }
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// playurl 的 URL 字段同时存在驼峰/下划线版本,并允许从备用地址兜底。
    fn stream_url(item: &Value) -> Option<String> {
        ["url", "baseUrl", "base_url"]
            .iter()
            .find_map(|key| item.get(*key).and_then(Value::as_str))
            .or_else(|| {
                ["backupUrl", "backup_url"].iter().find_map(|key| {
                    item.get(*key)
                        .and_then(Value::as_array)
                        .and_then(|urls| urls.first())
                        .and_then(Value::as_str)
                })
            })
            .and_then(Self::normalize_url)
    }

    fn first_stream(items: Option<&Value>) -> Option<String> {
        items
            .and_then(Value::as_array)
            .and_then(|items| items.iter().find_map(Self::stream_url))
    }

    /// 详情响应可能拆成 view 元数据与 playurl 播放信息两条,也可能由 SSR 通道合并回传。
    fn parse_detail(ctx: &FetchContext) -> FetchOutput {
        let mut view: Option<Value> = None;
        let mut play: Option<Value> = None;
        let mut tags: Option<Value> = None;
        for resp in &ctx.responses {
            let Ok(root) = serde_json::from_str::<Value>(&resp.body) else {
                continue;
            };
            let data = root.get("data").unwrap_or(&root);
            if let Some(value) = data.get("view") {
                view = Some(value.clone());
                tags = data.get("tags").cloned();
            } else if data.get("bvid").is_some() && data.get("title").is_some() {
                view = Some(data.clone());
            }
            if let Some(value) = data.get("play") {
                play = Some(value.clone());
            } else if data.get("dash").is_some() || data.get("durl").is_some() {
                play = Some(data.clone());
            }
        }

        let Some(play) = play else {
            return FetchOutput::default();
        };
        let view = view.unwrap_or_else(|| serde_json::json!({ "bvid": ctx.keyword }));
        let content_id =
            Self::as_string_opt(view.get("bvid")).unwrap_or_else(|| ctx.keyword.clone());
        let dash = play.get("dash");
        let combined_url = Self::first_stream(play.get("durl"));
        let video_url = combined_url
            .clone()
            .or_else(|| Self::first_stream(dash.and_then(|value| value.get("video"))));
        let audio_source_url = Self::first_stream(dash.and_then(|value| value.get("audio")))
            .or_else(|| {
                Self::first_stream(
                    dash.and_then(|value| value.get("dolby"))
                        .and_then(|value| value.get("audio")),
                )
            })
            .or_else(|| {
                dash.and_then(|value| value.get("flac"))
                    .and_then(|value| value.get("audio"))
                    .and_then(Self::stream_url)
            });
        let owner = view.get("owner");
        let stat = view.get("stat");
        let desc = view
            .get("desc")
            .or_else(|| view.get("description"))
            .and_then(Value::as_str)
            .map(str::to_string)
            .filter(|value| !value.is_empty());
        let topics = tags
            .as_ref()
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| item.get("tag_name").and_then(Value::as_str))
                    .filter(|name| !name.is_empty())
                    .map(|name| format!("#{name}"))
                    .collect()
            })
            .unwrap_or_default();
        let mut extra = serde_json::json!({
            "aid": view.get("aid"),
            "cid": view.get("cid"),
            "quality": play.get("quality"),
        });
        if let Some(url) = audio_source_url {
            extra["audio_source_url"] = Value::String(url);
        }

        let content = Content {
            platform: PLATFORM_ID.to_string(),
            content_id,
            kind: ContentKind::Video,
            title: view
                .get("title")
                .and_then(Value::as_str)
                .map(str::to_string)
                .filter(|value| !value.is_empty()),
            desc,
            author: Author {
                platform: PLATFORM_ID.to_string(),
                uid: Self::as_string_opt(owner.and_then(|value| value.get("mid")))
                    .unwrap_or_default(),
                nickname: owner
                    .and_then(|value| value.get("name"))
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                avatar: owner
                    .and_then(|value| value.get("face"))
                    .and_then(Value::as_str)
                    .and_then(Self::normalize_url),
                signature: None,
                follower_count: None,
                following_count: None,
                extra: serde_json::json!({}),
            },
            stats: Stats {
                like_count: Self::num(stat.and_then(|value| value.get("like"))),
                comment_count: Self::num(stat.and_then(|value| value.get("reply"))),
                collect_count: Self::num(stat.and_then(|value| value.get("favorite"))),
                share_count: Self::num(stat.and_then(|value| value.get("share"))),
                play_count: Self::num(stat.and_then(|value| value.get("view"))),
            },
            published_at: Self::num(view.get("pubdate")),
            video_url,
            cover_url: view
                .get("pic")
                .and_then(Value::as_str)
                .and_then(Self::normalize_url),
            image_urls: Vec::new(),
            duration: Self::num(view.get("duration")),
            topics,
            collected_at: Utc::now().timestamp(),
            extra,
        };
        FetchOutput {
            contents: vec![content],
            comments: Vec::new(),
            authors: Vec::new(),
        }
    }

    /// 搜索结果项里的 UP 主信息:`author`(昵称)/`mid`(uid)/`upic`(头像,协议相对)。
    /// 粉丝数/签名搜索响应不含(在 `/x/web-interface/card?mid=` 卡片接口,需进作者主页
    /// 或额外请求才有)——平台响应限制,作者档案先建到这三个字段,画像字段留空。
    fn parse_search_author(item: &Value) -> Author {
        Author {
            platform: PLATFORM_ID.to_string(),
            uid: Self::as_string_opt(item.get("mid")).unwrap_or_default(),
            nickname: item
                .get("author")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            avatar: item
                .get("upic")
                .and_then(Value::as_str)
                .and_then(Self::normalize_url),
            signature: None,
            follower_count: None,
            following_count: None,
            extra: serde_json::json!({}),
        }
    }

    /// 把单条一级评论(replies/top_replies 项)解析为 Comment;缺 rpid 视为无效返回 None。
    /// content_id 由采集上下文传入(评论项只有数字 oid,与任务侧的 bvid 不同体系)。
    fn parse_reply(item: &Value, content_id: &str, collected_at: i64) -> Option<Comment> {
        let comment_id = Self::as_string_opt(item.get("rpid"))?;
        let member = item.get("member");
        Some(Comment {
            platform: PLATFORM_ID.to_string(),
            content_id: content_id.to_string(),
            comment_id,
            parent_id: None, // 主列表均为一级评论(root=0);本期不采楼中楼
            author: Author {
                platform: PLATFORM_ID.to_string(),
                uid: Self::as_string_opt(member.and_then(|m| m.get("mid")))
                    .or_else(|| Self::as_string_opt(item.get("mid")))
                    .unwrap_or_default(),
                nickname: member
                    .and_then(|m| m.get("uname"))
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                avatar: member
                    .and_then(|m| m.get("avatar"))
                    .and_then(Value::as_str)
                    .and_then(Self::normalize_url),
                signature: member
                    .and_then(|m| m.get("sign"))
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .filter(|s| !s.is_empty()),
                follower_count: None,
                following_count: None,
                extra: serde_json::json!({}),
            },
            text: item
                .get("content")
                .and_then(|c| c.get("message"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            like_count: Self::num(item.get("like")),
            reply_count: Self::num(item.get("rcount")),
            created_at: Self::num(item.get("ctime")),
            collected_at,
            extra: serde_json::json!({}),
        })
    }

    /// 解析搜索响应为内容列表(comments 恒空)。兼容两种形态:
    /// 视频 tab `data.result[]` 直接是视频项;综合 tab 项带 `result_type`,视频在其 `data[]`。
    fn parse_search(ctx: &FetchContext) -> FetchOutput {
        let collected_at = Utc::now().timestamp();
        let mut contents = Vec::new();
        for resp in &ctx.responses {
            let Ok(root) = serde_json::from_str::<Value>(&resp.body) else {
                continue;
            };
            let Some(result) = root
                .get("data")
                .and_then(|d| d.get("result"))
                .and_then(Value::as_array)
            else {
                continue;
            };
            for entry in result {
                if let Some(group) = entry.get("result_type") {
                    // 综合 tab 的分组形态:只取视频组
                    if group.as_str() != Some("video") {
                        continue;
                    }
                    let Some(items) = entry.get("data").and_then(Value::as_array) else {
                        continue;
                    };
                    for item in items {
                        if let Some(content) = Self::parse_video_item(item, collected_at) {
                            contents.push(content);
                        }
                    }
                } else if let Some(content) = Self::parse_video_item(entry, collected_at) {
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

    /// 解析作者主页画像(authors 仅一条;contents/comments 恒空)。
    /// 空间页请求 `/x/web-interface/card?mid=`,画像在 `data.card`:粉丝 `fans`、关注
    /// `friend`、签名 `sign`、头像 `face`、昵称 `name`。card 不含获赞总数,留空。
    /// uid 用导航时的 mid(ctx.keyword),不依赖响应避免错配。
    fn parse_profile(ctx: &FetchContext) -> FetchOutput {
        let uid = ctx.keyword.clone();
        let mut authors = Vec::new();
        for resp in &ctx.responses {
            let Ok(root) = serde_json::from_str::<Value>(&resp.body) else {
                continue;
            };
            let Some(card) = root.get("data").and_then(|d| d.get("card")) else {
                continue;
            };
            authors.push(Author {
                platform: PLATFORM_ID.to_string(),
                uid: uid.clone(),
                nickname: card
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                avatar: card
                    .get("face")
                    .and_then(Value::as_str)
                    .and_then(Self::normalize_url),
                signature: card
                    .get("sign")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .filter(|s| !s.is_empty()),
                follower_count: Self::num(card.get("fans")),
                following_count: Self::num(card.get("friend")),
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

    /// 解析评论响应为评论列表(contents 恒空)。置顶评论(top_replies)一并采集。
    fn parse_comments(ctx: &FetchContext) -> FetchOutput {
        let collected_at = Utc::now().timestamp();
        // 评论场景下采集上下文的 keyword 即所属内容 id(bvid)
        let content_id = ctx.keyword.as_str();
        let mut comments = Vec::new();
        for resp in &ctx.responses {
            let Ok(root) = serde_json::from_str::<Value>(&resp.body) else {
                continue;
            };
            let Some(data) = root.get("data") else {
                continue;
            };
            for key in ["top_replies", "replies"] {
                let Some(items) = data.get(key).and_then(Value::as_array) else {
                    continue;
                };
                for item in items {
                    if let Some(comment) = Self::parse_reply(item, content_id, collected_at) {
                        comments.push(comment);
                    }
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
impl PlatformAdapter for BilibiliAdapter {
    fn id(&self) -> &str {
        PLATFORM_ID
    }

    fn supports(&self, kind: &TaskKind) -> bool {
        matches!(
            kind,
            TaskKind::Search | TaskKind::Comments | TaskKind::UserProfile | TaskKind::ContentDetail
        )
    }

    fn detail_pattern(&self) -> Option<&str> {
        Some(DETAIL_PATH)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::webview::InterceptedResponse;

    #[tokio::test]
    async fn parses_detail_with_separate_dash_audio() {
        let body = serde_json::json!({
            "data": {
                "view": {
                    "bvid": "BV1test", "aid": 10, "cid": 20, "title": "标题",
                    "desc": "正文", "pic": "//i0.hdslb.com/cover.jpg", "duration": 66,
                    "owner": { "mid": 30, "name": "作者", "face": "//i0.hdslb.com/avatar.jpg" },
                    "stat": { "view": 100, "like": 20, "reply": 3, "favorite": 4, "share": 5 }
                },
                "tags": [{ "tag_name": "旅行" }],
                "play": {
                    "quality": 80,
                    "dash": {
                        "video": [{ "baseUrl": "https://cdn/video.m4s" }],
                        "audio": [{ "base_url": "https://cdn/audio.m4s" }]
                    }
                }
            }
        });
        let ctx = FetchContext {
            keyword: "BV1test".into(),
            responses: vec![InterceptedResponse {
                url: "detail".into(),
                body: body.to_string(),
            }],
        };
        let output = BilibiliAdapter::new()
            .parse(&TaskKind::ContentDetail, &ctx)
            .await
            .unwrap();
        let content = &output.contents[0];
        assert_eq!(content.video_url.as_deref(), Some("https://cdn/video.m4s"));
        assert_eq!(content.extra["audio_source_url"], "https://cdn/audio.m4s");
        assert_eq!(content.topics, vec!["#旅行"]);
        assert_eq!(content.stats.like_count, Some(20));
    }

    #[tokio::test]
    async fn prefers_combined_durl_for_saved_video() {
        let body = serde_json::json!({
            "data": { "view": { "bvid": "BV2test" }, "play": {
                "durl": [{ "url": "https://cdn/combined.mp4" }],
                "dash": { "video": [{ "baseUrl": "https://cdn/video.m4s" }] }
            }}
        });
        let ctx = FetchContext {
            keyword: "BV2test".into(),
            responses: vec![InterceptedResponse {
                url: "detail".into(),
                body: body.to_string(),
            }],
        };
        let output = BilibiliAdapter::new()
            .parse(&TaskKind::ContentDetail, &ctx)
            .await
            .unwrap();
        assert_eq!(
            output.contents[0].video_url.as_deref(),
            Some("https://cdn/combined.mp4")
        );
    }
}
