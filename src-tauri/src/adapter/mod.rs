//! 平台适配器抽象与注册表。
//!
//! 这是「可扩展」架构的核心:核心层只认 `PlatformAdapter` trait 与字符串平台 ID,
//! 具体平台(抖音/小红书/快手/未来更多)各实现一个适配器并注册进 `AdapterRegistry`。
//! 新增平台不改调度、上报、模型等任何核心代码。
//!
//! 具体平台实现见阶段2;阶段0 仅固化接口与注册机制。

// 适配器系统为渐进接入的脚手架,部分接口/方法暂未接主流程
#![allow(dead_code)]

pub mod douyin;
pub mod xhs;

use veltrix_core::error::{CrawlerError, Result};
use crate::model::{Comment, Content, TaskKind};
use crate::webview::InterceptedResponse;
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::Arc;

/// 适配器单次解析的产出。RPA 滚动已在一次会话内收集全量响应,故不再有分页游标。
#[derive(Debug, Default)]
pub struct FetchOutput {
    pub contents: Vec<Content>,
    pub comments: Vec<Comment>,
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
