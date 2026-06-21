//! OpenAI 兼容 embeddings:把文本批量转向量,供长期记忆按语义检索(RAG)。
//!
//! 与 chat 共用 http 客户端 / 重试;endpoint 幂等拼接 `/embeddings`(用户填完整 URL 时不重复拼)。
//! 本模块只做「文本 → 向量」;相似度计算在调用方——记忆条数有硬上限,内存暴力 cosine 足够快,
//! 不引入向量库(pgvector 留给未来大规模知识库)。

use serde_json::{json, Value};
use veltrix_core::error::{CrawlerError, Result};

use super::http;

/// 单次请求最多 embedding 的文本条数。各厂商批量上限不一(Qwen 兼容模式约 10/批),保守取 10。
const MAX_BATCH: usize = 10;

/// 拼接 embeddings endpoint;api_url 已含该路径(用户填了完整 URL)时不重复拼。
fn embeddings_endpoint(api_url: &str) -> String {
    http::join_endpoint(api_url, "/embeddings")
}

/// 把若干文本转成向量,顺序与输入一致。空输入返回空;任一批失败则整体返回 Err(调用方据此回退)。
pub async fn embed_texts(
    api_url: &str,
    api_key: &str,
    model: &str,
    inputs: &[String],
) -> Result<Vec<Vec<f32>>> {
    if api_url.trim().is_empty() || api_key.trim().is_empty() {
        return Err(CrawlerError::Config(
            "embedding 厂商未配置 api_url / api_key".into(),
        ));
    }
    if inputs.is_empty() {
        return Ok(Vec::new());
    }
    let endpoint = embeddings_endpoint(api_url);
    let client = http::shared_client(http::CHAT_TIMEOUT_SECS)?;
    let mut out: Vec<Vec<f32>> = Vec::with_capacity(inputs.len());
    for chunk in inputs.chunks(MAX_BATCH) {
        let body = json!({ "model": model, "input": chunk });
        let resp = http::send_with_retry(
            || client.post(&endpoint).bearer_auth(api_key).json(&body),
            "embeddings",
            true,
        )
        .await?;
        let payload: Value = resp
            .json()
            .await
            .map_err(|e| CrawlerError::Config(format!("解析 embeddings 响应失败: {e}")))?;
        let data = payload
            .get("data")
            .and_then(Value::as_array)
            .ok_or_else(|| CrawlerError::Config("embeddings 响应缺少 data".into()))?;
        if data.len() != chunk.len() {
            return Err(CrawlerError::Config(format!(
                "embeddings 返回数量不符:期望 {},实得 {}",
                chunk.len(),
                data.len()
            )));
        }
        // OpenAI 兼容:data 按输入顺序返回;直接顺序取 embedding 数组
        for item in data {
            let vec = item
                .get("embedding")
                .and_then(Value::as_array)
                .ok_or_else(|| CrawlerError::Config("embeddings 条目缺少 embedding".into()))?
                .iter()
                .map(|v| v.as_f64().unwrap_or(0.0) as f32)
                .collect::<Vec<f32>>();
            out.push(vec);
        }
    }
    Ok(out)
}

/// 余弦相似度;维度不一致 / 空向量 / 零向量一律返回 0(视为不相关,稳妥不 panic)。
pub fn cosine(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let mut dot = 0f32;
    let mut norm_a = 0f32;
    let mut norm_b = 0f32;
    for (x, y) in a.iter().zip(b.iter()) {
        dot += x * y;
        norm_a += x * x;
        norm_b += y * y;
    }
    if norm_a == 0.0 || norm_b == 0.0 {
        return 0.0;
    }
    dot / (norm_a.sqrt() * norm_b.sqrt())
}
