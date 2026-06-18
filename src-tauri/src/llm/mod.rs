//! 大模型能力封装:chat(意向分析)、speech(语音转写)。
//!
//! 设计:5 家厂商 chat 全 OpenAI 兼容,统一走 `chat::chat_completion`;
//! 语音识别按 provider code 分发(目前仅小米 MiMo),经 `speech::transcribe`。
//! 统一超时 + 指数退避重试在 `http`,稳定高可用;能力元数据在 `provider`,便于扩展。

pub mod agent;
pub mod chat;
pub mod embedding;
pub mod http;
pub mod intent;
pub mod provider;
pub mod role;
pub mod speech;

// 保持对外路径稳定(commands 现有调用零改动):crate::llm::analyze_intent 等
pub use intent::{analyze_intent, IntentRequest, IntentVerdict};
pub use provider::{all_capabilities, ProviderCapability};
pub use role::AgentRole;
pub use speech::{transcribe, TranscribeRequest};
