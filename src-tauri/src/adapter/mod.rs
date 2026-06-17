//! 平台适配器抽象与注册表。
//!
//! 这是「可扩展」架构的核心:核心层只认 `PlatformAdapter` trait 与字符串平台 ID,
//! 具体平台(抖音/小红书/快手/未来更多)各实现一个适配器并注册进 `AdapterRegistry`。
//! 新增平台不改调度、上报、模型等任何核心代码。
//!
//! 具体平台实现见阶段2;阶段0 仅固化接口与注册机制。

// 适配器系统为渐进接入的脚手架,部分接口/方法暂未接主流程
#![allow(dead_code)]

pub mod bilibili;
pub mod douyin;
pub mod kuaishou;
pub mod tiktok;
pub mod xhs;
pub mod youtube;

use veltrix_core::error::{CrawlerError, Result};
use crate::model::{Author, Comment, Content, TaskKind};
use crate::webview::InterceptedResponse;
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::Arc;

/// 适配器单次解析的产出。RPA 滚动已在一次会话内收集全量响应,故不再有分页游标。
#[derive(Debug, Default)]
pub struct FetchOutput {
    pub contents: Vec<Content>,
    pub comments: Vec<Comment>,
    /// 作者画像(仅 UserProfile 补采解析时填;搜索/评论解析恒空)。
    pub authors: Vec<Author>,
}

/// 一次解析调用的输入上下文。
///
/// RPA + 拦截模式下,数据来自页面自己发出的接口响应(由 WebView hook 拦截回传),
/// 适配器不再发请求,只负责把这批响应解析为统一模型。
pub struct FetchContext {
    /// 本次采集关键词。
    pub keyword: String,
    /// WebView 拦截到的接口响应集合(命中平台 `intercept_patterns` 的 fetch/XHR)。
    pub responses: Vec<InterceptedResponse>,
}

/// 平台适配器。每个平台实现本 trait 并以平台 ID 注册。
#[async_trait]
pub trait PlatformAdapter: Send + Sync {
    /// 平台 ID,需与配置表 key 一致。
    fn id(&self) -> &str;

    /// 是否支持某类采集任务。调度层据此提前拒绝不支持的任务。
    fn supports(&self, kind: &TaskKind) -> bool;

    /// 把本次采集拦截到的接口响应解析为统一模型。
    /// 保留 async:部分平台解析后可能需异步补取媒体直链。
    async fn parse(&self, kind: &TaskKind, ctx: &FetchContext) -> Result<FetchOutput>;
}

/// 适配器注册表。线程安全,克隆共享(内部 Arc)。
#[derive(Clone, Default)]
pub struct AdapterRegistry {
    adapters: HashMap<String, Arc<dyn PlatformAdapter>>,
}

impl AdapterRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// 注册一个平台适配器;同 ID 覆盖,便于热替换。
    pub fn register(&mut self, adapter: Arc<dyn PlatformAdapter>) {
        self.adapters.insert(adapter.id().to_string(), adapter);
    }

    /// 按平台 ID 取适配器;未注册返回明确错误。
    pub fn get(&self, platform_id: &str) -> Result<Arc<dyn PlatformAdapter>> {
        self.adapters
            .get(platform_id)
            .cloned()
            .ok_or_else(|| CrawlerError::UnknownPlatform(platform_id.to_string()))
    }

    /// 已注册平台 ID 列表,供前端展示「可用平台」。
    pub fn registered_ids(&self) -> Vec<String> {
        self.adapters.keys().cloned().collect()
    }
}

/// 从正文文本提取 #话题:遇 `#` 开始累积,遇下一个 `#` / 空白 / 结尾即截断成一个话题。
/// 能正确拆分相邻无空格的「#话题a#话题b」(平台正文里话题常连写)。保序去重。
/// 仅作平台结构化话题字段缺失时的兜底——正文中部的 `#` 可能误判,但话题惯例以 `#` 起、空格或连写分隔。
pub(crate) fn extract_hashtags(text: &str) -> Vec<String> {
    let mut topics: Vec<String> = Vec::new();
    // buf=Some 表示正处于一个话题内(已遇到 #);None 表示在话题外的普通正文
    let mut buf: Option<String> = None;
    let flush = |buf: &mut Option<String>, out: &mut Vec<String>| {
        if let Some(name) = buf.take() {
            if !name.is_empty() {
                let topic = format!("#{name}");
                if !out.contains(&topic) {
                    out.push(topic);
                }
            }
        }
    };
    for ch in text.chars() {
        if ch == '#' {
            flush(&mut buf, &mut topics);
            buf = Some(String::new());
        } else if ch.is_whitespace() {
            flush(&mut buf, &mut topics);
        } else if let Some(name) = buf.as_mut() {
            name.push(ch);
        }
    }
    flush(&mut buf, &mut topics);
    topics
}
