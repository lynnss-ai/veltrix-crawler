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
//! 取舍:搜索响应不含视频流地址(DASH 流在详情页 playurl 接口,带防盗链),v1 不做
//! 视频/音频下载,`video_url` 恒为 None,只采元数据 + 封面 + 头像 + 评论。
//!
//! ⚠️ 字段名基于 Web 端公开结构整理,真实结构需本机 `bun tauri dev` 抓包核对后微调。

use crate::adapter::{FetchContext, FetchOutput, PlatformAdapter};
use crate::model::{Author, Comment, Content, ContentKind, Stats, TaskKind};
use async_trait::async_trait;
use chrono::Utc;
use serde_json::Value;
use veltrix_core::error::Result;

const PLATFORM_ID: &str = "bilibili";

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
                avatar: card.get("face").and_then(Value::as_str).and_then(Self::normalize_url),
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
