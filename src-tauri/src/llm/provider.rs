//! 厂商预设 + 能力元数据(单一真相源)。
//!
//! `PROVIDER_PRESETS` 是后端唯一定义:厂商 code / 名称 / 默认 base url / 是否支持 ASR。
//! 前端通过 `list_provider_capabilities` 获取,不再各自硬编码厂商清单。
//! 新增厂商:在此加一项即可;若该厂商支持 ASR,还需在 `speech.rs` 的 match 加对应实现。

use serde::Serialize;

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
