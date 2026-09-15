//! AI 成片的镜头策划：DeepSeek-V4.1-Flash 只挑选已抽取的真实镜头编号，不生成虚构时间码。
//! 源文件及密钥始终留在 Rust 侧；发往模型的是限量低分辨率抽帧和用户写的创作目标。

use crate::commands::{current_user, AppState};
use base64::Engine;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::Path;
use tauri::State;
use veltrix_core::db::entity::{model_usage_record, provider};
use veltrix_core::error::{CrawlerError, Result};

const MODEL: &str = "deepseek-flash";
const MAX_SCENES: usize = 20;
const MAX_FRAME_BYTES: usize = 350_000;

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AiSceneInput {
    pub start: f64,
    pub end: f64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AiVideoPlanInput {
    pub input_path: String,
    pub scenes: Vec<AiSceneInput>,
    pub platform: String,
    pub goal: String,
    pub brief: String,
    pub target_duration: u32,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiVideoPlanView {
    pub selected_indices: Vec<usize>,
    pub title: String,
    pub rationale: String,
    pub model: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ModelPlan {
    selected_indices: Vec<usize>,
    title: String,
    rationale: String,
}

fn validate_input(input: &AiVideoPlanInput) -> Result<()> {
    if !Path::new(&input.input_path).is_file() {
        return Err(CrawlerError::Config("源视频不存在".into()));
    }
    if input.scenes.is_empty() || input.scenes.len() > MAX_SCENES {
        return Err(CrawlerError::Config(format!(
            "AI 策划镜头数量必须在 1～{MAX_SCENES} 之间"
        )));
    }
    if !matches!(
        input.platform.as_str(),
        "douyin" | "xhs" | "kuaishou" | "tiktok" | "shorts"
    ) {
        return Err(CrawlerError::Config("目标平台无效".into()));
    }
    if !matches!(
        input.goal.as_str(),
        "lead" | "reach" | "knowledge" | "repurpose"
    ) {
        return Err(CrawlerError::Config("内容目标无效".into()));
    }
    if !(3..=180).contains(&input.target_duration) || input.brief.chars().count() > 1000 {
        return Err(CrawlerError::Config("成片时长或补充要求无效".into()));
    }
    if input.scenes.iter().any(|scene| {
        !scene.start.is_finite()
            || !scene.end.is_finite()
            || scene.start < 0.0
            || scene.end - scene.start < 0.35
    }) {
        return Err(CrawlerError::Config("镜头区间无效".into()));
    }
    Ok(())
}

/// 拒绝模型越界、重复和空选择；时间区间完全由真实 FFmpeg 场景检测结果决定。
fn parse_plan(raw: &str, scene_count: usize) -> Result<ModelPlan> {
    let plan: ModelPlan = serde_json::from_str(raw.trim())
        .map_err(|_| CrawlerError::Config("DeepSeek 未返回可用的 JSON 剪辑方案".into()))?;
    let mut seen = std::collections::HashSet::new();
    if plan.selected_indices.is_empty()
        || plan.selected_indices.len() > scene_count
        || plan
            .selected_indices
            .iter()
            .any(|index| *index >= scene_count || !seen.insert(*index))
    {
        return Err(CrawlerError::Config("DeepSeek 返回了无效的镜头编号".into()));
    }
    Ok(plan)
}

fn extract_frames(program: &str, input: &AiVideoPlanInput) -> Result<Vec<String>> {
    let mut frames = Vec::with_capacity(input.scenes.len());
    for scene in &input.scenes {
        let time = scene.start + (scene.end - scene.start) / 2.0;
        let mut command = std::process::Command::new(program);
        crate::media::hide_console_window(&mut command);
        let output = command
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-nostdin",
                "-ss",
                &format!("{time:.3}"),
            ])
            .arg("-i")
            .arg(&input.input_path)
            .args([
                "-frames:v",
                "1",
                "-an",
                "-sn",
                "-dn",
                "-vf",
                "scale=320:-2:force_original_aspect_ratio=decrease",
                "-q:v",
                "6",
                "-f",
                "image2pipe",
                "-vcodec",
                "mjpeg",
                "pipe:1",
            ])
            .output()
            .map_err(|_| CrawlerError::Config("AI 策划抽帧失败，请检查 FFmpeg".into()))?;
        if !output.status.success()
            || output.stdout.is_empty()
            || output.stdout.len() > MAX_FRAME_BYTES
        {
            return Err(CrawlerError::Config("AI 策划抽帧未得到可用图片".into()));
        }
        frames.push(base64::engine::general_purpose::STANDARD.encode(output.stdout));
    }
    Ok(frames)
}

fn build_messages(input: &AiVideoPlanInput, frames: &[String]) -> Value {
    let system = "你是短视频剪辑策划。只根据提供的真实镜头截图和用户目标挑选镜头。\
                  截图不含声音，不能推断口播、人物身份、商品功效或镜头之外的事实。\
                  严格输出 JSON 对象：{\"selectedIndices\":[0,2],\"title\":\"...\",\"rationale\":\"...\"}。\
                  selectedIndices 只能引用输入中的镜头编号，不得编造时间码。\
                  选择约目标时长的镜头，尽量兼顾开头吸引力、画面变化与叙事顺序。";
    let goal_label = match input.goal.as_str() {
        "lead" => "获得咨询或销售线索",
        "reach" => "扩大曝光",
        "knowledge" => "知识表达",
        _ => "内容复用",
    };
    let platform_label = match input.platform.as_str() {
        "douyin" => "抖音",
        "xhs" => "小红书",
        "kuaishou" => "快手",
        "tiktok" => "TikTok",
        _ => "YouTube Shorts",
    };
    let mut blocks = vec![json!({
        "type": "text",
        "text": format!(
            "目标平台：{}；内容目标：{}；目标时长：{} 秒；补充要求：{}。\n请返回 JSON。",
            platform_label,
            goal_label,
            input.target_duration,
            input.brief.trim()
        )
    })];
    for (index, (scene, frame)) in input.scenes.iter().zip(frames).enumerate() {
        blocks.push(json!({
            "type": "text",
            "text": format!("镜头 {index}，源区间 {:.2}～{:.2} 秒：", scene.start, scene.end),
        }));
        blocks.push(json!({
            "type": "image_url",
            "image_url": {"url": format!("data:image/jpeg;base64,{frame}"), "detail": "low"},
        }));
    }
    json!([
        {"role": "system", "content": system},
        {"role": "user", "content": blocks}
    ])
}

#[tauri::command]
pub async fn creation_ai_plan(
    state: State<'_, AppState>,
    caller: tauri::WebviewWindow,
    input: AiVideoPlanInput,
) -> Result<AiVideoPlanView> {
    // 采集 WebView 加载不受信任的远程页面，不得触发付费模型调用。
    if caller.label() != "main" {
        return Err(CrawlerError::Config("AI 成片仅允许在主窗口使用".into()));
    }
    let me = current_user(&state).ok_or_else(|| CrawlerError::Config("未登录".into()))?;
    validate_input(&input)?;
    let configured = provider::Entity::find()
        .filter(provider::Column::Code.eq("deepseek"))
        .one(&state.db)
        .await
        .map_err(|_| CrawlerError::Config("读取 DeepSeek 厂商配置失败".into()))?
        .ok_or_else(|| {
            CrawlerError::Config("尚未配置 DeepSeek 厂商，请先在模型设置中添加".into())
        })?;
    if configured.api_key.trim().is_empty() {
        return Err(CrawlerError::Config("DeepSeek 密钥尚未配置".into()));
    }
    let program = {
        let config = super::lock_config(&state)?;
        config
            .media
            .ffmpeg_path
            .clone()
            .unwrap_or_else(|| "ffmpeg".into())
    };
    let frame_input = AiVideoPlanInput {
        input_path: input.input_path.clone(),
        scenes: input.scenes.clone(),
        platform: input.platform.clone(),
        goal: input.goal.clone(),
        brief: input.brief.clone(),
        target_duration: input.target_duration,
    };
    let frames =
        tauri::async_runtime::spawn_blocking(move || extract_frames(&program, &frame_input))
            .await
            .map_err(|_| CrawlerError::Config("AI 策划抽帧任务异常".into()))??;
    let outcome = crate::llm::chat::chat_completion(crate::llm::chat::ChatRequest {
        api_url: &configured.api_url,
        api_key: &configured.api_key,
        model: MODEL,
        messages: build_messages(&input, &frames),
        extra_body: Some(json!({
            "response_format": {"type": "json_object"},
            "thinking": {"type": "disabled"},
            "max_tokens": 1200
        })),
        timeout_secs: crate::llm::http::CHAT_TIMEOUT_SECS,
        retry_server_errors: true,
    })
    .await?;
    let _ = model_usage_record::Model::record(
        &state.db,
        MODEL,
        &configured.id,
        outcome.usage.prompt,
        outcome.usage.completion,
        "ai_video_plan",
        &me.name,
    )
    .await;
    let plan = parse_plan(&outcome.content, input.scenes.len())?;
    Ok(AiVideoPlanView {
        selected_indices: plan.selected_indices,
        title: plan.title.chars().take(80).collect(),
        rationale: plan.rationale.chars().take(500).collect(),
        model: MODEL.into(),
    })
}

#[cfg(test)]
mod tests {
    use super::parse_plan;

    #[test]
    fn rejects_out_of_range_or_duplicate_scene_ids() {
        assert!(parse_plan(
            r#"{"selectedIndices":[0,4],"title":"t","rationale":"r"}"#,
            4
        )
        .is_err());
        assert!(parse_plan(
            r#"{"selectedIndices":[1,1],"title":"t","rationale":"r"}"#,
            4
        )
        .is_err());
        assert!(parse_plan(r#"{"selectedIndices":[],"title":"t","rationale":"r"}"#, 4).is_err());
    }

    #[test]
    fn accepts_real_scene_ids_only() {
        let plan = parse_plan(
            r#"{"selectedIndices":[2,0],"title":"t","rationale":"r"}"#,
            3,
        )
        .unwrap();
        assert_eq!(plan.selected_indices, vec![2, 0]);
    }
}
