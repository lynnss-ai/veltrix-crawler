//! 厂商预设 + 能力元数据(单一真相源)。
//!
//! `PROVIDER_PRESETS` 是后端唯一定义:厂商 code / 名称 / 默认 base url / 是否支持 ASR。
//! 前端通过 `list_provider_capabilities` 获取,不再各自硬编码厂商清单。
//! 新增厂商:在此加一项即可;若该厂商支持 ASR,还需在 `speech.rs` 的 match 加对应实现。

use serde::{Deserialize, Serialize};

/// 厂商预设(编译期常量,单一真相源)。
struct ProviderPreset {
    code: &'static str,
    name: &'static str,
    /// OpenAI 兼容 base url 默认值(用户可在表单改;MiMo 为官方接口)。
    api_url: &'static str,
    /// 是否支持语音识别(ASR)。
    asr: bool,
}

const PROVIDER_PRESETS: &[ProviderPreset] = &[
    ProviderPreset {
        code: "deepseek",
        name: "DeepSeek",
        api_url: "https://api.deepseek.com/chat/completions",
        asr: false,
    },
    ProviderPreset {
        code: "qwen",
        name: "千问 Qwen",
        api_url: "https://dashscope.aliyuncs.com/compatible-mode/v1",
        asr: false,
    },
    ProviderPreset {
        code: "mimo",
        name: "小米 MiMo",
        api_url: "https://api.xiaomimimo.com/v1",
        asr: true,
    },
    ProviderPreset {
        code: "glm",
        name: "智谱 GLM",
        api_url: "https://open.bigmodel.cn/api/paas/v4",
        asr: false,
    },
    ProviderPreset {
        code: "minimax",
        name: "MiniMax",
        api_url: "https://api.minimaxi.com/v1",
        asr: false,
    },
];

/// 是否支持语音识别(ASR)。从预设查;新增 ASR 厂商只需改 PROVIDER_PRESETS + speech.rs。
pub fn provider_supports_asr(code: &str) -> bool {
    PROVIDER_PRESETS.iter().any(|p| p.code == code && p.asr)
}

/// 暴露给前端的厂商预设 + 能力:供新增厂商下拉(code/name/apiUrl)与按 ASR 过滤。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderCapability {
    pub code: String,
    pub name: String,
    pub api_url: String,
    pub chat: bool,
    pub asr: bool,
}

/// 返回全部厂商预设 + 能力。
pub fn all_capabilities() -> Vec<ProviderCapability> {
    PROVIDER_PRESETS
        .iter()
        .map(|p| ProviderCapability {
            code: p.code.to_string(),
            name: p.name.to_string(),
            api_url: p.api_url.to_string(),
            chat: true, // 当前 5 家全部 OpenAI 兼容
            asr: p.asr,
        })
        .collect()
}

// ===================== 模型能力(模型级,非厂商级) =====================

/// 模型能力 code(单一真相源)。前端 `settings-meta.ts::MODEL_CAPABILITIES` 与此逐一对应,
/// 负责各 code 的中文标签与图标。新增能力维度:此处加一项 + 前端加对应展示项。
/// 顺序即规范展示/存储顺序(parse 时按此重排去重)。
pub const MODEL_CAPABILITIES: &[&str] = &["text", "vision", "audio", "video", "tools"];

/// 单个模型 = 名称 + 能力集合。`providers.models` 列以此数组的 JSON 存储。
/// 能力让各智能体「按需挑模型」:对话/角色要 text,coding/rpa 要 tools(function calling),
/// 多模态场景要 vision/audio/video。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelSpec {
    pub name: String,
    #[serde(default)]
    pub capabilities: Vec<String>,
}

/// 解析 `providers.models` 列。
/// - 新格式:`[{"name":"gpt-4o","capabilities":["text","vision"]}]` JSON 数组;
/// - 旧格式(兼容旧库):多行文本(每行一个模型名),整体降级为「仅对话(text)」能力。
///   解析顺带清洗:去空名、丢弃未知能力 code、按 MODEL_CAPABILITIES 顺序去重、能力为空兜底 text。
pub fn parse_models(raw: &str) -> Vec<ModelSpec> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }
    // 新格式:JSON 数组(以 '[' 开头)。解析失败则继续走旧格式兜底,避免脏数据丢全部模型。
    if trimmed.starts_with('[') {
        if let Ok(list) = serde_json::from_str::<Vec<ModelSpec>>(trimmed) {
            return list
                .into_iter()
                .filter_map(|m| {
                    let name = m.name.trim().to_string();
                    if name.is_empty() {
                        return None;
                    }
                    Some(ModelSpec {
                        name,
                        capabilities: normalize_capabilities(&m.capabilities),
                    })
                })
                .collect();
        }
    }
    // 旧格式:多行文本,每行一个模型名,默认仅对话能力。
    trimmed
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(|name| ModelSpec {
            name: name.to_string(),
            capabilities: vec!["text".to_string()],
        })
        .collect()
}

/// 规范化能力集合:只保留已知 code、按 MODEL_CAPABILITIES 顺序去重;为空兜底 text。
fn normalize_capabilities(input: &[String]) -> Vec<String> {
    let normalized: Vec<String> = MODEL_CAPABILITIES
        .iter()
        .filter(|cap| input.iter().any(|c| c == *cap))
        .map(|c| c.to_string())
        .collect();
    if normalized.is_empty() {
        vec!["text".to_string()]
    } else {
        normalized
    }
}

/// 序列化模型列表为 `providers.models` 列存储用 JSON;失败兜底空数组串(不致写坏列)。
pub fn serialize_models(models: &[ModelSpec]) -> String {
    serde_json::to_string(models).unwrap_or_else(|_| "[]".to_string())
}
