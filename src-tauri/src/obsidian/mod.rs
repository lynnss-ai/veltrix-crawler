//! Obsidian 同步:把采集内容渲染为 Markdown 写入用户 vault,封面复制到 vault 的 assets。
//!
//! 每用户各自 vault;同步执行后由调用方记录 content_synced_users(谁同步了该条)。

use std::path::Path;

use veltrix_core::db::entity::{comment as comment_entity, content as content_entity};
use veltrix_core::error::{CrawlerError, Result};

/// vault 下的内容子目录与资源子目录。
const VELTRIX_SUBDIR: &str = "Veltrix";
const ASSETS_SUBDIR: &str = "assets";
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

/// 渲染单条内容为 Markdown:frontmatter + 标题/封面/正文 + 转写 + 评论。
fn render_markdown(
    c: &content_entity::Model,
    comments: &[comment_entity::Model],
    cover_rel: Option<&str>,
) -> String {
    let mut md = String::new();
    md.push_str("---\n");
    md.push_str(&format!("platform: {}\n", c.platform));
    md.push_str(&format!("content_id: \"{}\"\n", c.content_id));
    md.push_str(&format!(
        "author: \"{}\"\n",
        c.author_nickname.replace(['\n', '"'], " ")
    ));
    md.push_str(&format!("kind: {}\n", c.kind));
    if let Some(l) = c.like_count {
        md.push_str(&format!("likes: {l}\n"));
    }
    md.push_str(&format!("collected_at: {}\n", c.collected_at));
    let topics: Vec<String> = serde_json::from_str(&c.topics).unwrap_or_default();
    if !topics.is_empty() {
        let tags = topics
            .iter()
            .map(|t| format!("\"{}\"", t.trim_start_matches('#').replace('"', "")))
            .collect::<Vec<_>>()
            .join(", ");
        md.push_str(&format!("tags: [{tags}]\n"));
    }
    md.push_str("---\n\n");

    if let Some(t) = c.title.as_deref().filter(|s| !s.is_empty()) {
        md.push_str(&format!("# {t}\n\n"));
    }
    if let Some(rel) = cover_rel {
        md.push_str(&format!("![cover]({rel})\n\n"));
    }
    if let Some(d) = c.desc.as_deref().filter(|s| !s.is_empty()) {
        md.push_str(d);
        md.push_str("\n\n");
    }
    if let Some(tr) = c.transcript.as_deref().filter(|s| !s.is_empty()) {
        md.push_str("## 语音文案\n\n");
        md.push_str(tr);
        md.push_str("\n\n");
    }
    if !comments.is_empty() {
        md.push_str(&format!("## 评论({})\n\n", comments.len()));
        for cm in comments {
            let intent = cm
                .intent_level
                .as_deref()
                .map(|l| format!(" `[{l}]`"))
                .unwrap_or_default();
            md.push_str(&format!(
                "- **{}**{}: {}\n",
                cm.author_nickname.replace('\n', " "),
                intent,
                cm.text.replace('\n', " ")
            ));
        }
        md.push('\n');
    }
    md
}

/// 把单条内容同步到 vault:复制封面到 assets,写 Markdown 文件。
/// 封面复制失败不阻断(仅不引用图片);Markdown 写失败返回错误。
pub async fn sync_one(
    vault: &Path,
    c: &content_entity::Model,
    comments: &[comment_entity::Model],
) -> Result<()> {
    let base = vault.join(VELTRIX_SUBDIR);
    let assets = base.join(ASSETS_SUBDIR);
    tokio::fs::create_dir_all(&assets)
        .await
        .map_err(|e| CrawlerError::Config(format!("创建 Obsidian 目录失败: {e}")))?;

    let prefix = sanitize(&format!("{}_{}", c.platform, c.content_id));
    // 封面本地存在则复制进 vault 的 assets,markdown 用相对引用
    let cover_rel = match c.cover_path.as_deref() {
        Some(cp) if Path::new(cp).exists() => {
            let fname = format!("{prefix}_cover.jpg");
            if tokio::fs::copy(cp, assets.join(&fname)).await.is_ok() {
                Some(format!("{ASSETS_SUBDIR}/{fname}"))
            } else {
                None
            }
        }
        _ => None,
    };

    let md = render_markdown(c, comments, cover_rel.as_deref());
    tokio::fs::write(base.join(format!("{prefix}.md")), md)
        .await
        .map_err(|e| CrawlerError::Config(format!("写 Markdown 失败: {e}")))?;
    Ok(())
}
