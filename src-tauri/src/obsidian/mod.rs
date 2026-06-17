//! Obsidian 同步:把采集内容渲染为 Markdown 写入用户 vault。
//!
//! 目录结构 `{vault}/Veltrix/{行业}/{平台}/{日期}/`(三级目录,日期 YYYYMMDD,按内容所属任务的行业 + 平台 + 采集日,
//! 缺失各级自动递归创建),文件名为内容ID(同 ID 覆盖=更新 / 不存在=新增)。
//! 媒体(封面 / 音频 / 图文多图)复制到日期目录下的 `assets/`,Markdown 用相对引用。
//! 每用户各自 vault;同步执行后由调用方记录 content_synced_users(谁同步了该条)。

use std::path::Path;

use chrono::Local;
use serde_json::Value;
use veltrix_core::db::entity::{comment as comment_entity, content as content_entity};
use veltrix_core::error::{CrawlerError, Result};

/// vault 下的内容根目录与资源子目录。
const VELTRIX_SUBDIR: &str = "Veltrix";
const ASSETS_SUBDIR: &str = "assets";
/// 行业为空时的归档目录前缀。
const UNCLASSIFIED: &str = "未分类";
/// 图文图片瀑布流网格:每行图数 + 单图限宽(px)。wiki 嵌入同行并排,逼近瀑布流效果。
const IMAGES_PER_ROW: usize = 3;
const IMAGE_WIDTH: u32 = 180;
/// 文件名非法字符(Windows 保留 + 路径分隔符)。
const ILLEGAL: &[char] = &['/', '\\', ':', '*', '?', '"', '<', '>', '|'];

fn sanitize(raw: &str) -> String {
    let cleaned: String = raw
        .chars()
        .map(|c| if ILLEGAL.contains(&c) { '_' } else { c })
        .collect();
    let trimmed = cleaned.trim().trim_matches('.');
    if trimmed.is_empty() {
        "unknown".to_string()
    } else {
        trimmed.to_string()
    }
}

/// 平台 id → 中文名(导出属性用中文,未知平台保留原值)。
fn platform_cn(p: &str) -> &str {
    match p {
        "douyin" => "抖音",
        "xhs" => "小红书",
        "kuaishou" => "快手",
        "bilibili" => "B站",
        "tiktok" => "TikTok",
        "youtube" => "YouTube",
        other => other,
    }
}

/// 内容形态 → 中文。
fn kind_cn(k: &str) -> &str {
    match k {
        "video" => "视频",
        "image" => "图文",
        "article" => "文章",
        other => other,
    }
}

/// Unix 秒 → 本地时区「YYYY-MM-DD HH:MM」。
fn fmt_ts(ts: i64) -> String {
    chrono::DateTime::from_timestamp(ts, 0)
        .map(|dt| dt.with_timezone(&Local).format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_default()
}

/// 秒数 → 「X分Y秒」/「X秒」。
fn fmt_duration(secs: i64) -> String {
    if secs >= 60 {
        format!("{}分{}秒", secs / 60, secs % 60)
    } else {
        format!("{secs}秒")
    }
}

/// YAML 字符串值:加引号并清掉换行 / 引号,避免破坏 frontmatter。
fn yaml_str(s: &str) -> String {
    format!(
        "\"{}\"",
        s.replace('\\', " ").replace('"', "'").replace(['\n', '\r'], " ")
    )
}

/// 已复制进 vault assets 的本地媒体相对路径,供 Markdown 引用。
struct RenderAssets {
    cover: Option<String>,
    audio: Option<String>,
    images: Vec<String>,
}

/// 渲染单条内容为 Markdown:中文 frontmatter(尽量全字段)+ 标题/封面/正文 + 图片/音频 + 转写 + 评论。
fn render_markdown(
    c: &content_entity::Model,
    comments: &[comment_entity::Model],
    industry: &str,
    assets: &RenderAssets,
) -> String {
    // 作者补充信息藏在 author_json(粉丝 / 签名 / 属地);平台特有字段在 extra(分享链接)
    let author: Value = serde_json::from_str(&c.author_json).unwrap_or(Value::Null);
    let follower = author
        .get("follower_count")
        .and_then(Value::as_i64)
        .filter(|&v| v > 0);
    let signature = author
        .get("signature")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty());
    let ip_location = author
        .get("extra")
        .and_then(|e| e.get("ip_location"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty());
    let extra: Value = serde_json::from_str(&c.extra).unwrap_or(Value::Null);
    let share_url = extra
        .get("share_url")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty());

    let mut md = String::from("---\n");
    md.push_str(&format!("平台: {}\n", platform_cn(&c.platform)));
    md.push_str(&format!("类型: {}\n", kind_cn(&c.kind)));
    md.push_str(&format!("内容ID: {}\n", yaml_str(&c.content_id)));
    md.push_str(&format!("作者: {}\n", yaml_str(&c.author_nickname)));
    if !c.author_uid.is_empty() {
        md.push_str(&format!("作者ID: {}\n", yaml_str(&c.author_uid)));
    }
    if let Some(f) = follower {
        md.push_str(&format!("粉丝数: {f}\n"));
    }
    if let Some(s) = signature {
        md.push_str(&format!("作者签名: {}\n", yaml_str(s)));
    }
    if let Some(loc) = ip_location {
        md.push_str(&format!("属地: {}\n", yaml_str(loc)));
    }
    if !industry.trim().is_empty() {
        md.push_str(&format!("行业: {}\n", yaml_str(industry.trim())));
    }
    if !c.keyword.is_empty() {
        md.push_str(&format!("关键词: {}\n", yaml_str(&c.keyword)));
    }
    // 统计类数值:0 视为「接口未返回」的空信息,>0 才输出
    if let Some(v) = c.like_count.filter(|&v| v > 0) {
        md.push_str(&format!("点赞数: {v}\n"));
    }
    if let Some(v) = c.comment_count.filter(|&v| v > 0) {
        md.push_str(&format!("评论数: {v}\n"));
    }
    if let Some(v) = c.collect_count.filter(|&v| v > 0) {
        md.push_str(&format!("收藏数: {v}\n"));
    }
    if let Some(v) = c.share_count.filter(|&v| v > 0) {
        md.push_str(&format!("分享数: {v}\n"));
    }
    if let Some(v) = c.play_count.filter(|&v| v > 0) {
        md.push_str(&format!("播放数: {v}\n"));
    }
    if let Some(d) = c.duration.filter(|&v| v > 0) {
        md.push_str(&format!("时长: {}\n", fmt_duration(d)));
    }
    if let Some(p) = c.published_at {
        md.push_str(&format!("发布时间: {}\n", yaml_str(&fmt_ts(p))));
    }
    md.push_str(&format!("采集时间: {}\n", yaml_str(&fmt_ts(c.collected_at))));
    // 链接放笔记属性(Obsidian 属性面板里可点击跳转)
    if let Some(u) = share_url {
        md.push_str(&format!("原内容链接: {}\n", yaml_str(u)));
    }
    if let Some(u) = c.video_url.as_deref().filter(|s| !s.is_empty()) {
        md.push_str(&format!("视频链接: {}\n", yaml_str(u)));
    }
    if let Some(u) = c.cover_url.as_deref().filter(|s| !s.is_empty()) {
        md.push_str(&format!("封面链接: {}\n", yaml_str(u)));
    }
    // 标签用 Obsidian tags(YAML list,属性面板渲染为可点击胶囊);话题去 # 与空格以符合 tag 规范
    let topics: Vec<String> = serde_json::from_str(&c.topics).unwrap_or_default();
    let tags: Vec<String> = topics
        .iter()
        .map(|t| t.trim_start_matches('#').replace([' ', '"', '#'], ""))
        .filter(|t| !t.is_empty())
        .collect();
    if !tags.is_empty() {
        md.push_str("tags:\n");
        for tag in &tags {
            md.push_str(&format!("  - {tag}\n"));
        }
    }
    md.push_str("---\n");

    // 正文按「块」拼接:空块(无标题/封面/图片/音频/转写/评论)自动跳过,块间恰好一个空行,
    // 信息缺失时不留多余空行
    let mut blocks: Vec<String> = Vec::new();
    // 封面(图文首图已在下方图片网格里,只有非图文/视频才单独放封面大图)放最上方,居中显示
    // div + 前后空行:Obsidian 会渲染内部 wiki embed 并应用 text-align 居中
    if assets.images.is_empty() {
        if let Some(f) = &assets.cover {
            blocks.push(format!(
                "<div style=\"text-align:center\">\n\n![[{f}]]\n\n</div>"
            ));
        }
    }
    // 标题:有则紧贴在正文内容上方,用 callout 做样式标记,与正文区分开
    if let Some(t) = c.title.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        blocks.push(format!("> [!note] {}", t.replace('\n', " ")));
    }
    if let Some(d) = c.desc.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        blocks.push(d.to_string());
    }
    if !assets.images.is_empty() {
        // 瀑布流网格:wiki 嵌入,同行多图 Obsidian 会并排;每行 IMAGES_PER_ROW 张、限宽 IMAGE_WIDTH
        let grid = assets
            .images
            .chunks(IMAGES_PER_ROW)
            .map(|row| {
                row.iter()
                    .map(|f| format!("![[{f}|{IMAGE_WIDTH}]]"))
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .collect::<Vec<_>>()
            .join("\n");
        blocks.push(format!("## 图片\n\n{grid}"));
    }
    // 音频与语音文案合到一个「## 音频文案」标题下(音频嵌入在上,转写文本在下)
    let mut audio_text: Vec<String> = Vec::new();
    if let Some(f) = &assets.audio {
        audio_text.push(format!("![[{f}]]"));
    }
    if let Some(tr) = c.transcript.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        audio_text.push(tr.to_string());
    }
    if !audio_text.is_empty() {
        blocks.push(format!("## 音频文案\n\n{}", audio_text.join("\n\n")));
    }
    if !comments.is_empty() {
        let list = comments
            .iter()
            .map(|cm| {
                let intent = match cm.intent_level.as_deref() {
                    Some("high") => " `[高意向]`",
                    Some("medium") => " `[中意向]`",
                    Some("low") => " `[低意向]`",
                    _ => "",
                };
                // 评论者头像(远程 URL,小圆图)+ 发表时间
                let cm_author: Value = serde_json::from_str(&cm.author_json).unwrap_or(Value::Null);
                let avatar = cm_author
                    .get("avatar")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .map(|u| {
                        format!(
                            "<img src=\"{}\" width=\"20\" height=\"20\" style=\"border-radius:50%;vertical-align:middle\"> ",
                            u.replace('"', "")
                        )
                    })
                    .unwrap_or_default();
                let time = cm
                    .created_at
                    .map(|t| format!(" · {}", fmt_ts(t)))
                    .unwrap_or_default();
                format!(
                    "- {}**{}**{}{}: {}",
                    avatar,
                    cm.author_nickname.replace('\n', " "),
                    time,
                    intent,
                    cm.text.replace('\n', " ")
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        blocks.push(format!("## 评论({})\n\n{}", comments.len(), list));
    }

    if blocks.is_empty() {
        md.push('\n');
        md
    } else {
        format!("{md}\n{}\n", blocks.join("\n\n"))
    }
}

/// 复制本地文件到 assets 目录,成功返回文件名(供 wiki 嵌入 `![[文件名]]`,Obsidian 按 vault 内唯一文件名解析)。
/// 源文件不存在 / 复制失败返回 None。
async fn copy_asset(src: &str, assets: &Path, fname: &str) -> Option<String> {
    if !Path::new(src).exists() {
        return None;
    }
    if tokio::fs::copy(src, assets.join(fname)).await.is_ok() {
        Some(fname.to_string())
    } else {
        None
    }
}

/// 把单条内容同步到 vault:按「行业-采集日期」建目录,复制封面 / 音频 / 图文图片,写 Markdown。
/// 媒体复制失败不阻断(仅不引用该媒体);Markdown 写失败返回错误。
pub async fn sync_one(
    vault: &Path,
    c: &content_entity::Model,
    comments: &[comment_entity::Model],
    industry: &str,
) -> Result<()> {
    // 三级目录:行业 / 平台 / 日期(日期 YYYYMMDD;行业空归「未分类」);缺失的各级由 create_dir_all 递归自动创建。
    // 媒体与 md 同放日期目录下的 assets/
    let day = chrono::DateTime::from_timestamp(c.collected_at, 0)
        .map(|dt| dt.with_timezone(&Local).format("%Y%m%d").to_string())
        .unwrap_or_else(|| "unknown".into());
    let industry_name = if industry.trim().is_empty() {
        UNCLASSIFIED
    } else {
        industry.trim()
    };
    let base = vault
        .join(VELTRIX_SUBDIR)
        .join(sanitize(industry_name))
        .join(sanitize(platform_cn(&c.platform)))
        .join(sanitize(&day));
    let assets = base.join(ASSETS_SUBDIR);
    tokio::fs::create_dir_all(&assets)
        .await
        .map_err(|e| CrawlerError::Config(format!("创建 Obsidian 目录失败: {e}")))?;

    // 文件 / 资源前缀用内容ID(平台已是目录层级);同 ID 即同一个 .md,写入覆盖=更新,不存在=新增
    let prefix = sanitize(&c.content_id);

    // 封面
    let cover = match c.cover_path.as_deref() {
        Some(cp) => copy_asset(cp, &assets, &format!("{prefix}_cover.jpg")).await,
        None => None,
    };
    // 音频(视频转出的音频文件,保留原扩展名)
    let audio = match c.audio_path.as_deref() {
        Some(ap) => {
            let ext = Path::new(ap)
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("mp3");
            copy_asset(ap, &assets, &format!("{prefix}_audio.{ext}")).await
        }
        None => None,
    };
    // 图文本地图片:与封面同目录,media 侧文件名为 {封面前缀去 _cover}_img{idx}.jpg
    let mut images = Vec::new();
    if let Some(cp) = c.cover_path.as_deref() {
        let cover_path = Path::new(cp);
        if let (Some(dir), Some(stem)) = (
            cover_path.parent(),
            cover_path.file_stem().and_then(|s| s.to_str()),
        ) {
            if let Some(media_prefix) = stem.strip_suffix("_cover") {
                let count = c.image_done.or(c.image_total).unwrap_or(0).max(0);
                for idx in 0..count {
                    let src = dir.join(format!("{media_prefix}_img{idx}.jpg"));
                    if let Some(src_str) = src.to_str() {
                        if let Some(rel) =
                            copy_asset(src_str, &assets, &format!("{prefix}_img{idx}.jpg")).await
                        {
                            images.push(rel);
                        }
                    }
                }
            }
        }
    }

    let render_assets = RenderAssets {
        cover,
        audio,
        images,
    };
    let md = render_markdown(c, comments, industry, &render_assets);
    tokio::fs::write(base.join(format!("{prefix}.md")), md)
        .await
        .map_err(|e| CrawlerError::Config(format!("写 Markdown 失败: {e}")))?;
    Ok(())
}
