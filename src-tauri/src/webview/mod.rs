//! 可见 WebView + Rust↔页面 拦截桥接。
//!
//! 采集模式(RPA + 接口拦截):不再逆向签名、不自己拼 API,而是
//! 在**可见** WebView 内打开搜索结果页,注入脚本劫持 `fetch` / `XMLHttpRequest`,
//! 把命中目标 URL 特征的接口响应经 IPC 回传 Rust,再交由适配器解析。
//!
//! 关于注入时序(重要,运行时联调须知):
//! `initialization_script` 会在**每次页面导航**时最早期执行,因此把「平台级拦截特征」
//! 编译进该脚本以尽早挂上 hook;而「本次采集会话 ID」是动态的,导航完成后再用 `eval`
//! 调用 `__veltrixSetSession` 注入。为防止页面在 session 注入前就发出首批搜索请求导致漏抓,
//! hook 命中后先压入页内缓冲,`__veltrixSetSession` 时连同缓冲一并回放上报。
//!
//! 对**外部页面**(如 douyin.com)能否调用 `window.__TAURI_INTERNALS__.invoke`,
//! 取决于 Tauri `capabilities` 是否对该窗口放行 `core:default`,需本机 `bun tauri dev` 验证。

// 拦截响应部分字段待解析链路接入,暂保留
#![allow(dead_code)]

pub mod pool;

use veltrix_core::error::{CrawlerError, Result};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

/// 一条被拦截的接口响应。`body` 为响应文本(通常是 JSON),由适配器解析。
#[derive(Debug, Clone)]
pub struct InterceptedResponse {
    pub url: String,
    pub body: String,
}

/// 拦截通道:按采集会话汇集页面回传的接口响应。
///
/// 与「签名一问一答」不同,拦截是**持续推送**:一次采集会触发多个分页接口,
/// 全部累积到该会话缓冲,RPA 跑完后由调度方一次取走交给适配器。
#[derive(Default)]
pub struct InterceptChannel {
    seq: AtomicU64,
    /// session_id -> 已拦截响应列表。
    sessions: Mutex<HashMap<u64, Vec<InterceptedResponse>>>,
}

impl InterceptChannel {
    pub fn new() -> Self {
        Self::default()
    }

    /// 开启一次采集会话,返回 session_id。
    pub fn open_session(&self) -> Result<u64> {
        let session_id = self.seq.fetch_add(1, Ordering::Relaxed);
        self.sessions
            .lock()
            .map_err(|_| CrawlerError::Sign("拦截通道锁异常".into()))?
            .insert(session_id, Vec::new());
        Ok(session_id)
    }

    /// 页面回传一条命中的接口响应。锁异常时丢弃本条并告警,不阻塞页面。
    pub fn push(&self, session_id: u64, url: String, body: String) {
        match self.sessions.lock() {
            Ok(mut sessions) => {
                // 用 entry 兜底:即便 session 已被取走或未登记,也不致丢失到 panic
                sessions
                    .entry(session_id)
                    .or_default()
                    .push(InterceptedResponse { url, body });
            }
            Err(_) => tracing::warn!(session_id, "拦截通道锁异常,丢弃一条回传"),
        }
    }

    /// 结束会话并取走全部已拦截响应。锁异常时返回空,由调度方按空结果处理。
    pub fn take_session(&self, session_id: u64) -> Vec<InterceptedResponse> {
        self.sessions
            .lock()
            .ok()
            .and_then(|mut sessions| sessions.remove(&session_id))
            .unwrap_or_default()
    }
}

/// 构造注入到页面的早期拦截脚本(作为 `initialization_script`)。
///
/// `patterns` 为该平台需拦截的接口 URL 特征(子串)。脚本在页面最早期挂上
/// `fetch` / `XHR` hook,命中特征的响应在 session 未就绪时先缓冲,就绪后回放上报。
pub fn build_intercept_init_script(patterns: &[String]) -> String {
    // 用 serde 序列化为 JS 数组字面量,避免手工拼接引号出错
    let patterns_json = serde_json::to_string(patterns).unwrap_or_else(|_| "[]".to_string());
    format!(
        r#"(function () {{
  if (window.__veltrixHooked) return;
  window.__veltrixHooked = true;
  var PATTERNS = {patterns};
  window.__veltrixSession = null;
  window.__veltrixBuf = [];

  function matched(url) {{
    if (!url) return false;
    for (var i = 0; i < PATTERNS.length; i++) {{
      if (url.indexOf(PATTERNS[i]) !== -1) return true;
    }}
    return false;
  }}
  function emit(url, body) {{
    var s = window.__veltrixSession;
    if (s === null) {{ window.__veltrixBuf.push({{ url: url, body: body }}); return; }}
    try {{
      window.__TAURI_INTERNALS__.invoke('intercept_push', {{ sessionId: s, url: url, body: body }});
    }} catch (e) {{ console.error('[veltrix] intercept bridge unavailable', e); }}
  }}
  function report(url, body) {{ if (matched(url)) emit(url, body); }}

  window.__veltrixSetSession = function (s) {{
    window.__veltrixSession = s;
    var buf = window.__veltrixBuf;
    window.__veltrixBuf = [];
    for (var i = 0; i < buf.length; i++) emit(buf[i].url, buf[i].body);
  }};

  var origFetch = window.fetch;
  if (origFetch) {{
    window.fetch = function () {{
      var args = arguments;
      var url = (args[0] && args[0].url) ? args[0].url : String(args[0]);
      return origFetch.apply(this, args).then(function (resp) {{
        try {{ resp.clone().text().then(function (t) {{ report(url, t); }}).catch(function () {{}}); }} catch (e) {{}}
        return resp;
      }});
    }};
  }}

  var origOpen = XMLHttpRequest.prototype.open;
  var origSend = XMLHttpRequest.prototype.send;
  XMLHttpRequest.prototype.open = function (method, url) {{
    this.__veltrixUrl = url;
    return origOpen.apply(this, arguments);
  }};
  XMLHttpRequest.prototype.send = function () {{
    var self = this;
    this.addEventListener('load', function () {{
      try {{
        var t = (self.responseType === '' || self.responseType === 'text')
          ? self.responseText : JSON.stringify(self.response);
        report(self.__veltrixUrl, t);
      }} catch (e) {{}}
    }});
    return origSend.apply(this, arguments);
  }};
}})();"#,
        patterns = patterns_json,
    )
}

/// 构造「设置会话 ID 并回放缓冲」的注入脚本。导航到搜索页后调用。
pub fn build_set_session_eval(session_id: u64) -> String {
    format!("window.__veltrixSetSession && window.__veltrixSetSession({session_id});")
}

/// 构造单轮滚动脚本:滚到底部以触发平台的分页加载接口。
///
/// RPA 的「翻页」由 Rust 端循环调用本脚本 + 间隔等待驱动,而非一段长脚本,
/// 这样每轮之间可受 `scroll_interval_ms` 控制节奏,降低风控概率。
pub fn build_scroll_eval() -> String {
    "window.scrollTo(0, document.body.scrollHeight);".to_string()
}

/// 构造「按关键词导航到搜索结果页」的脚本。
///
/// keyword 在页面侧用 `encodeURIComponent` 编码,避免中文 / 特殊字符破坏 URL;
/// `assign` 触发一次正常导航,使 `initialization_script` 在新页面重新挂载 hook。
pub fn build_search_eval(template: &str, keyword: &str) -> String {
    let tpl = template.replace('\\', "\\\\").replace('\'', "\\'");
    let kw = keyword.replace('\\', "\\\\").replace('\'', "\\'");
    format!(
        "(function () {{ var kw = encodeURIComponent('{kw}'); \
         window.location.assign('{tpl}'.replace('{{keyword}}', kw)); }})();"
    )
}
