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

pub mod cookies;
pub mod filter_locate;
pub mod native_intercept;
pub mod pool;
pub mod script_eval;

use veltrix_core::config::RpaStep;
use veltrix_core::error::{CrawlerError, Result};
use serde::Serialize;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use tauri::{AppHandle, Emitter};
use tokio::sync::mpsc::Sender;
use tokio::sync::oneshot;

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
                // 只接受仍开启的会话:已结束(被取走)的会话若用 entry 重建,
                // 迟到的回传会留下永远无人取走的缓冲,长期运行累积成内存泄漏
                match sessions.get_mut(&session_id) {
                    Some(buf) => buf.push(InterceptedResponse { url, body }),
                    None => tracing::debug!(session_id, "会话已结束,丢弃迟到的拦截回传"),
                }
            }
            Err(_) => tracing::warn!(session_id, "拦截通道锁异常,丢弃一条回传"),
        }
    }

    /// 非破坏性查看会话当前已拦截的响应(clone),供采集中途判断进度,不结束会话。
    /// 与 `take_session` 区别:不移除,会话仍可继续累积。锁异常时返回空。
    pub fn peek_session(&self, session_id: u64) -> Vec<InterceptedResponse> {
        self.sessions
            .lock()
            .ok()
            .and_then(|sessions| sessions.get(&session_id).cloned())
            .unwrap_or_default()
    }

    /// 非破坏性查看会话当前已拦截的响应条数(不加锁,无 clone),供进度轮询用。
    /// 替代 `peek_session(session_id).len()` 的整份克隆热点(每轮 O(n²) 内存搬运)。
    /// 锁异常时返回 0。
    pub fn session_len(&self, session_id: u64) -> usize {
        self.sessions
            .lock()
            .ok()
            .and_then(|sessions| sessions.get(&session_id).map(|v| v.len()))
            .unwrap_or(0)
    }

    /// 非破坏性倒查会话内首个 URL 命中 `url_pattern` 的响应体(锁内按引用查找,
    /// 只克隆命中那一条)。供「看最新一条接口的字段」场景,避免 peek_session 整份克隆
    /// (每条是整页 JSON,滚动循环每轮调用会成 O(轮数×总字节) 热点)。
    pub fn find_session_body_rev(&self, session_id: u64, url_pattern: &str) -> Option<String> {
        self.sessions.lock().ok().and_then(|sessions| {
            sessions.get(&session_id).and_then(|buf| {
                buf.iter()
                    .rev()
                    .find(|r| r.url.contains(url_pattern))
                    .map(|r| r.body.clone())
            })
        })
    }

    /// 非破坏性倒查会话内首个 URL 命中 `url_pattern` 的请求 URL(只克隆命中那一条)。
    /// 供「拿真实请求 URL 当参数模板」场景(如评论 API 直采取 msToken / device 参数)。
    pub fn find_session_url_rev(&self, session_id: u64, url_pattern: &str) -> Option<String> {
        self.sessions.lock().ok().and_then(|sessions| {
            sessions.get(&session_id).and_then(|buf| {
                buf.iter()
                    .rev()
                    .find(|r| r.url.contains(url_pattern))
                    .map(|r| r.url.clone())
            })
        })
    }

    /// 非破坏性查看会话自 `offset` 起的新增响应,返回 (新增响应, 当前总条数) 供调用方推进游标。
    /// 滚动循环每轮调用:会话缓冲只追加,只克隆增量——此前整份 clone 随轮数增长成 O(n²) 热点
    /// (响应体是整页 JSON)。锁异常时返回空增量且总数保持 offset,游标原地不动、下轮重试。
    pub fn peek_session_from(
        &self,
        session_id: u64,
        offset: usize,
    ) -> (Vec<InterceptedResponse>, usize) {
        self.sessions
            .lock()
            .ok()
            .and_then(|sessions| {
                sessions.get(&session_id).map(|buf| {
                    let fresh = buf.get(offset..).map(<[_]>::to_vec).unwrap_or_default();
                    (fresh, buf.len())
                })
            })
            .unwrap_or((Vec::new(), offset))
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

/// 一次 RPA 运行的执行结果,由页面脚本经 `rpa_done` 回传。
#[derive(Debug, Clone)]
pub struct RpaOutcome {
    pub ok: bool,
    /// 失败步骤下标;成功为 -1。
    pub failed_step: i64,
    pub message: String,
}

/// RPA 运行通道:为每次拟人 RPA 运行分配 run_id,并以 oneshot 等待页面回传结果。
///
/// 与持续推送的 [`InterceptChannel`] 不同,一次运行只回传一次结果(成功/失败),故用
/// oneshot;接收端因超时被 drop 后,迟到的 `complete` 安全忽略。run_id 区分并发的多账号运行。
#[derive(Default)]
pub struct RpaChannel {
    seq: AtomicU64,
    /// run_id -> 结果发送端。
    pending: Mutex<HashMap<u64, oneshot::Sender<RpaOutcome>>>,
}

impl RpaChannel {
    pub fn new() -> Self {
        Self::default()
    }

    /// 开启一次运行,返回 run_id 与结果接收端。
    pub fn open_run(&self) -> Result<(u64, oneshot::Receiver<RpaOutcome>)> {
        let run_id = self.seq.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending
            .lock()
            .map_err(|_| CrawlerError::Sign("RPA 通道锁异常".into()))?
            .insert(run_id, tx);
        Ok((run_id, rx))
    }

    /// 页面回传一次运行结果。run_id 未登记或已完成(超时)则忽略。
    pub fn complete(&self, run_id: u64, outcome: RpaOutcome) {
        if let Ok(mut pending) = self.pending.lock() {
            if let Some(tx) = pending.remove(&run_id) {
                // 接收端已 drop(超时)时 send 返回 Err,忽略即可
                let _ = tx.send(outcome);
            }
        }
    }

    /// 放弃一次运行(等待方超时后调用):页面 ack 永不回传时,
    /// 不清理会让发送端条目在表里永久残留,长期运行累积泄漏。
    pub fn cancel(&self, run_id: u64) {
        if let Ok(mut pending) = self.pending.lock() {
            pending.remove(&run_id);
        }
    }
}

// 注:浏览器 Agent 早期的「动作 + 回读」请求-响应通道(AgentActionChannel / AgentActionOutcome)
// 已废弃——回读改走 WebView2 ExecuteScript(见 webview::script_eval),不再依赖页面 invoke 回传。

/// 采集中断控制:HUD「结束」按钮经 `stop_collect` 命令登记 session_id 与 task_id,
/// 采集循环每轮 / 关键词切换时检查到即**优雅停止**(保留已采内容、作为正常完成),而非报错中断。
#[derive(Default)]
pub struct CollectControl {
    /// 被请求停止的 session_id 集合(无 task 的联调单采用此兜底)。
    stopping: Mutex<std::collections::HashSet<u64>>,
    /// 被请求停止的 task_id 集合。会话(session_id)按关键词刷新,在「两个关键词之间」点结束
    /// 会落到上一个已结束的会话上而漏判;按任务登记则跨关键词稳定,关键词循环切下个词前据此终止。
    stopping_tasks: Mutex<std::collections::HashSet<String>>,
    /// 当前检测到安全验证弹窗的 session_id 集合(采集窗口自检脚本经 `report_collect_verify` 写入)。
    /// 采集循环每轮检查到即暂停滚动,等弹窗消失(用户手动完成)再恢复。
    verifying: Mutex<std::collections::HashSet<u64>>,
    /// 评论 API 直采的完成结果(session_id → 结果 JSON 字符串),页内 JS 经 `comment_api_done` 写入,
    /// pool 侧轮询 `take_api_done` 取走。比 eval 回读可靠:mac 上 eval_json 是空实现。
    api_done: Mutex<std::collections::HashMap<u64, String>>,
    /// 全量库批量提取(批量转写 / 补采评论)的停止标记:无任务上下文,不能复用
    /// stopping_tasks;由 `cancel_library_extract` 命令置位,两个批量命令在逐条/逐批
    /// 循环里检查,命中即优雅收尾(已完成条目保留)。下一批开始前必须 clear 重置。
    library_batch_stopping: Mutex<bool>,
}

impl CollectControl {
    pub fn new() -> Self {
        Self::default()
    }

    /// 请求停止某会话。
    pub fn request_stop(&self, session_id: u64) {
        if let Ok(mut set) = self.stopping.lock() {
            set.insert(session_id);
        }
    }

    /// 该会话是否被请求停止。
    pub fn is_stopping(&self, session_id: u64) -> bool {
        self.stopping
            .lock()
            .map(|set| set.contains(&session_id))
            .unwrap_or(false)
    }

    /// 请求停止某任务(HUD「结束」按钮按 task_id 登记)。
    pub fn request_stop_task(&self, task_id: &str) {
        if let Ok(mut set) = self.stopping_tasks.lock() {
            set.insert(task_id.to_string());
        }
    }

    /// 该任务是否被请求停止。
    pub fn is_task_stopping(&self, task_id: &str) -> bool {
        self.stopping_tasks
            .lock()
            .map(|set| set.contains(task_id))
            .unwrap_or(false)
    }

    /// 清除某任务的停止标记。任务开始前重置,避免上一次运行的「结束」点击影响重跑。
    pub fn clear_task(&self, task_id: &str) {
        if let Ok(mut set) = self.stopping_tasks.lock() {
            set.remove(task_id);
        }
    }

    /// 设置某会话的「安全验证弹窗」状态:present=true 标记弹出,false 清除(弹窗已消失)。
    pub fn set_verifying(&self, session_id: u64, present: bool) {
        if let Ok(mut set) = self.verifying.lock() {
            if present {
                set.insert(session_id);
            } else {
                set.remove(&session_id);
            }
        }
    }

    /// 该会话当前是否有安全验证弹窗待处理。
    pub fn is_verifying(&self, session_id: u64) -> bool {
        self.verifying
            .lock()
            .map(|set| set.contains(&session_id))
            .unwrap_or(false)
    }

    /// 会话结束后清理标志,避免 session_id 复用时误判(停止标志 + 验证标志一并清)。
    pub fn clear(&self, session_id: u64) {
        if let Ok(mut set) = self.stopping.lock() {
            set.remove(&session_id);
        }
        if let Ok(mut set) = self.verifying.lock() {
            set.remove(&session_id);
        }
        if let Ok(mut m) = self.api_done.lock() {
            m.remove(&session_id);
        }
    }

    /// 页内评论直采脚本回传完成结果(经 `comment_api_done` 命令)。
    pub fn set_api_done(&self, session_id: u64, result: String) {
        if let Ok(mut m) = self.api_done.lock() {
            m.insert(session_id, result);
        }
    }

    /// 取走某会话的直采完成结果(取走即清,不重复消费)。
    pub fn take_api_done(&self, session_id: u64) -> Option<String> {
        self.api_done
            .lock()
            .ok()
            .and_then(|mut m| m.remove(&session_id))
    }

    /// 请求停止全量库批量提取(批量转写 / 补采评论)。
    pub fn request_stop_library_batch(&self) {
        if let Ok(mut f) = self.library_batch_stopping.lock() {
            *f = true;
        }
    }

    /// 全量库批量提取是否被请求停止。
    pub fn is_library_batch_stopping(&self) -> bool {
        self.library_batch_stopping
            .lock()
            .map(|f| *f)
            .unwrap_or(false)
    }

    /// 清除批量提取停止标记:下一批开始前重置,避免上次的「取消」影响新批次。
    pub fn clear_library_batch(&self) {
        if let Ok(mut f) = self.library_batch_stopping.lock() {
            *f = false;
        }
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
  window.__veltrixSeen = [];    // 调试:hook 看到的所有请求 URL(不只命中 patterns 的)
  window.__veltrixPushOk = 0;   // 调试:invoke 回传成功次数
  window.__veltrixPushErr = 0;  // 调试:invoke 回传失败次数(>0 且 Ok=0 = 桥被拒)

  // 页内 → Rust 控制信号桥:走 WebView 原生消息,不走 Tauri invoke——
  // 远程页面(平台站点)的 invoke 会被 ACL 拒("not allowed. Plugin not found")。
  // Windows: chrome.webview.postMessage(WebMessageReceived);mac: webkit.messageHandlers。
  // 返回 false = 两桥都不可用,调用方可退回 invoke 兜底。
  window.__veltrixSignal = function (kind, payload) {{
    try {{
      var o = payload || {{}};
      o.__veltrix = kind;
      if (window.chrome && window.chrome.webview && window.chrome.webview.postMessage) {{
        window.chrome.webview.postMessage(o);
        return true;
      }}
      if (window.webkit && window.webkit.messageHandlers && window.webkit.messageHandlers.veltrixNative) {{
        window.webkit.messageHandlers.veltrixNative.postMessage(JSON.stringify(o));
        return true;
      }}
    }} catch (e) {{}}
    return false;
  }};

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
      window.__veltrixPushOk++;
    }} catch (e) {{ window.__veltrixPushErr++; console.error('[veltrix] intercept bridge unavailable', e); }}
  }}
  var __veltrixApiSeen = {{}};
  function report(url, body) {{
    if (window.__veltrixSeen.length < 300) window.__veltrixSeen.push(url);
    // 诊断:页面实际调用的每个 /api/ 接口「路径」首次出现时打到 HUD,便于核对页面真实接口
    // (如小红书改版排查搜索/评论接口是否真的发出、路径是否变化)。按路径去重,不刷屏。
    // 仅顶层帧输出:initialization_script 在所有帧注入,字节安全 SDK 的沙箱 iframe 每发一次
    // 遥测(security.zijieapi.com/api/metrics/emit)都是新帧上下文,各自「首次出现」会刷屏;
    // 页面真实业务接口都在顶层帧发出,诊断价值不损失。
    // 已知遥测/埋点接口(无任何业务数据,逐页导航各报一次会淹没真实日志)直接跳过;
    // 注意抖音真实业务接口是 /aweme/v1/web/...,不含 /api/,本诊断本就看不到它们
    var __veltrixApiSkip = ['/api/metrics/emit', '/api/metrics/collect'];
    try {{
      var p = (url || '').split('?')[0];
      var skipped = __veltrixApiSkip.some(function (s) {{ return p.indexOf(s) !== -1; }});
      if (window === window.top && p.indexOf('/api/') !== -1 && !skipped && !__veltrixApiSeen[p]) {{
        __veltrixApiSeen[p] = 1;
        if (window.__veltrixHud && window.__veltrixHud.log) {{
          window.__veltrixHud.log({{ level: 'info', message: '🔎 接口 ' + p }});
        }}
      }}
    }} catch (e) {{}}
    if (matched(url)) emit(url, body);
  }}

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

/// macOS 专用早期注入脚本:hook fetch / XHR,命中 `patterns` 的响应经
/// `webkit.messageHandlers.veltrixNative` 直接回传 Rust(对应 Windows 的原生拦截)。
///
/// 与 [`build_intercept_init_script`] 并存:后者走 Tauri invoke 兜底,二者结果在采集结束时
/// 合并、由适配器按 content_id 去重。`webkit.messageHandlers` 在任意页面恒可用、不受 Tauri
/// capabilities 影响,故作为 mac 主拦截通道。回传体为 `{"u":url,"b":body}` JSON 字符串。
pub fn build_native_intercept_init_script_mac(patterns: &[String]) -> String {
    let patterns_json = serde_json::to_string(patterns).unwrap_or_else(|_| "[]".to_string());
    format!(
        r#"(function () {{
  if (window.__veltrixMacHooked) return;
  window.__veltrixMacHooked = true;
  var PATTERNS = {patterns};
  function matched(u) {{
    if (!u) return false;
    for (var i = 0; i < PATTERNS.length; i++) {{
      if (u.indexOf(PATTERNS[i]) !== -1) return true;
    }}
    return false;
  }}
  function post(u, b) {{
    try {{
      if (window.webkit && window.webkit.messageHandlers && window.webkit.messageHandlers.veltrixNative) {{
        window.webkit.messageHandlers.veltrixNative.postMessage(JSON.stringify({{ u: u, b: b }}));
      }}
    }} catch (e) {{}}
  }}
  function report(u, b) {{ if (matched(u)) post(u, b); }}

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
    this.__veltrixMacUrl = url;
    return origOpen.apply(this, arguments);
  }};
  XMLHttpRequest.prototype.send = function () {{
    var self = this;
    this.addEventListener('load', function () {{
      try {{
        var t = (self.responseType === '' || self.responseType === 'text')
          ? self.responseText : JSON.stringify(self.response);
        report(self.__veltrixMacUrl, t);
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

/// 首屏 SSR 兜底合成响应的标记 URL 前缀。
/// 主页定向采集的首屏(约 18-24 条,含置顶)是 SSR 直出,可能根本不发作品列表 XHR
/// 或命中缓存拿不到 body,导致整页丢失。`build_ssr_first_screen_eval` 从页面 SSR 数据
/// 提取首屏作品,以该前缀的合成 URL 经 `intercept_push` 回传,随拦截响应一并交适配器解析。
pub const SSR_FALLBACK_URL_PREFIX: &str = "https://veltrix.local/ssr-first-screen";

/// 构造「首屏 SSR 兜底补采」注入脚本(主页定向采集,导航完成后 eval 一次)。
///
/// 提取顺序:`window._ROUTER_DATA` → `#RENDER_DATA`(decodeURIComponent 后 JSON.parse),
/// 递归找首个「含 aweme_list 且首项带 aweme_id」的对象;找到即以
/// `{SSR_FALLBACK_URL_PREFIX}{posts_pattern}` 为合成 URL 回传 `{"aweme_list":[...]}`
/// (URL 含作品接口特征,适配器按正常分页响应解析,下游按 aweme_id 去重合并)。
/// SSR 数据不可用则退化为扫 DOM 里 `/video/{id}` 链接,回传 `{prefix}/aweme-ids`
/// + `{"aweme_ids":[...]}`(仅 ID,采集侧记「首屏疑似丢失 N 条」,不逐条补采)。
/// 全程 try/catch 包裹,任何一步失败静默放弃,不影响页面。
pub fn build_ssr_first_screen_eval(session_id: u64, posts_pattern: &str) -> String {
    let items_url = serde_json::to_string(&format!("{SSR_FALLBACK_URL_PREFIX}{posts_pattern}"))
        .unwrap_or_else(|_| "\"\"".to_string());
    let ids_url = serde_json::to_string(&format!("{SSR_FALLBACK_URL_PREFIX}/aweme-ids"))
        .unwrap_or_else(|_| "\"\"".to_string());

    const TEMPLATE: &str = r#"(function () {
  try {
    var SID = __SID__;
    var ITEMS_URL = __ITEMS_URL__;
    var IDS_URL = __IDS_URL__;
    function push(url, body) {
      try {
        var p = window.__TAURI_INTERNALS__.invoke('intercept_push', { sessionId: SID, url: url, body: body });
        if (p && p.catch) p.catch(function () {});
      } catch (e) {}
    }
    // 递归找「含 aweme_list 数组且首项带 aweme_id」的对象;限制深度与节点数防失控
    var budget = 4000;
    function looksLikeAwemeList(v) {
      if (!v || typeof v !== 'object') return false;
      var l = v.aweme_list;
      if (!Array.isArray(l) || !l.length) return false;
      for (var i = 0; i < Math.min(l.length, 3); i++) {
        var it = l[i];
        var info = (it && typeof it === 'object' && it.aweme_info) ? it.aweme_info : it;
        if (info && (typeof info.aweme_id === 'string' || typeof info.aweme_id === 'number')) return true;
      }
      return false;
    }
    function findList(node, depth) {
      if (!node || typeof node !== 'object' || depth > 8 || budget <= 0) return null;
      budget--;
      if (looksLikeAwemeList(node)) return node.aweme_list;
      var keys = Object.keys(node);
      for (var i = 0; i < keys.length; i++) {
        var child;
        try { child = node[keys[i]]; } catch (e) { continue; }
        if (child && typeof child === 'object') {
          var r = findList(child, depth + 1);
          if (r) return r;
        }
      }
      return null;
    }
    var list = null;
    try { if (window._ROUTER_DATA) list = findList(window._ROUTER_DATA, 0); } catch (e) {}
    if (!list) {
      try {
        var el = document.getElementById('RENDER_DATA');
        if (el && el.textContent) {
          budget = 4000;
          list = findList(JSON.parse(decodeURIComponent(el.textContent)), 0);
        }
      } catch (e) {}
    }
    if (list && list.length) {
      push(ITEMS_URL, JSON.stringify({ aweme_list: list }));
      return;
    }
    // SSR 数据不可用:退化为扫 DOM 里 /video/{id} 链接,只回传作品 ID 列表
    var ids = [], seen = {};
    try {
      var as = document.querySelectorAll('a[href*="/video/"]');
      for (var i = 0; i < as.length; i++) {
        var m = /\/video\/(\d+)/.exec(as[i].getAttribute('href') || '');
        if (m && !seen[m[1]]) { seen[m[1]] = 1; ids.push(m[1]); }
      }
    } catch (e) {}
    if (ids.length) push(IDS_URL, JSON.stringify({ aweme_ids: ids }));
  } catch (e) {}
})();"#;

    TEMPLATE
        .replace("__SID__", &session_id.to_string())
        .replace("__ITEMS_URL__", &items_url)
        .replace("__IDS_URL__", &ids_url)
}

/// 验证弹窗上报命令名;与 Rust 端 `#[tauri::command] report_collect_verify` 对应。
pub const VERIFY_REPORT_COMMAND: &str = "report_collect_verify";

/// 构造「安全验证自检」注入脚本(采集窗口用,导航后 eval)。每隔 ~1.5s 检测当前页是否处于
/// 安全验证状态:命中验证弹窗选择器/文案,或当前 location 命中验证页 URL 特征(整页跳转到
/// 验证中心场景);状态翻转时经 `report_collect_verify` 回传 `{ sessionId, present }`,
/// 采集循环据此暂停 / 恢复。三者皆空时不安装(该平台未配置验证检测)。
pub fn build_verify_check_eval(
    session_id: u64,
    verify_selectors: &[String],
    verify_texts: &[String],
    verify_url_patterns: &[String],
) -> String {
    if verify_selectors.is_empty() && verify_texts.is_empty() && verify_url_patterns.is_empty() {
        return String::new();
    }
    let sel = serde_json::to_string(verify_selectors).unwrap_or_else(|_| "[]".to_string());
    let txt = serde_json::to_string(verify_texts).unwrap_or_else(|_| "[]".to_string());
    let url = serde_json::to_string(verify_url_patterns).unwrap_or_else(|_| "[]".to_string());

    const TEMPLATE: &str = r#"(function () {
  // 会话每次采集都更新(窗口复用),检测脚本只装一次定时器
  window.__veltrixVerifySession = __SESSION__;
  if (window.__veltrixVerifyCheck) return;
  window.__veltrixVerifyCheck = true;
  var SEL = __SEL__;
  var TXT = __TXT__;
  var URLP = __URL__;
  var last = null;
  // 本脚本经 window.eval 注入,只在顶层帧跑。验证码常渲染在跨域 iframe 里、顶层帧 querySelector
  // 抓不到 —— 故除本帧 DOM/文案/URL 外,再读 HUD 注入脚本(在所有帧运行)写入的子帧心跳时间戳兜底:
  // 子帧在 iframe 内看到验证码会 postMessage 到顶层,HUD 顶层监听把时间戳写到 window.__veltrixChildVerifyTs。
  var CHILD_TTL = 4000; // 与 HUD 跨帧心跳一致(ms):超时即认为子帧验证码已消失

  function visible(el) {
    if (!el) return false;
    // offsetParent 对 position:fixed 元素返回 null,不能据此判不可见
    // 改用 getBoundingClientRect + 计算样式判断
    var r = el.getBoundingClientRect();
    if (r.width === 0 || r.height === 0) return false;
    var st = getComputedStyle(el);
    return st.display !== 'none' && st.visibility !== 'hidden';
  }
  // 命中任一验证弹窗选择器(可见)
  function bySelector() {
    for (var i = 0; i < SEL.length; i++) {
      try { if (visible(document.querySelector(SEL[i]))) return true; } catch (e) {}
    }
    return false;
  }
  // 页面可见文本含任一验证文案(限可见、较短的节点,降低误命中)
  function byText() {
    if (!TXT.length) return false;
    var nodes = document.querySelectorAll('div,span,p,button,a,[role="dialog"]');
    for (var i = 0; i < nodes.length; i++) {
      if (!visible(nodes[i])) continue;
      var t = (nodes[i].textContent || '').trim();
      if (!t || t.length > 40) continue;
      for (var j = 0; j < TXT.length; j++) {
        if (t.indexOf(TXT[j]) !== -1) return true;
      }
    }
    return false;
  }
  // 当前 location 命中验证页 URL 特征(整页跳转到验证中心)
  function byLocation() {
    if (!URLP.length) return false;
    var href = '';
    try { href = (location.href || '').toLowerCase(); } catch (e) { return false; }
    for (var i = 0; i < URLP.length; i++) {
      if (href.indexOf(String(URLP[i]).toLowerCase()) !== -1) return true;
    }
    return false;
  }
  // 顶层判定 = 本帧可见 或 跨域子帧近 CHILD_TTL 内报过验证码心跳(HUD 跨帧桥写入)
  function present() {
    if (bySelector() || byText() || byLocation()) return true;
    try { if ((Date.now() - (window.__veltrixChildVerifyTs || 0)) < CHILD_TTL) return true; } catch (e) {}
    return false;
  }

  function tick() {
    var p = present();
    if (p !== last) {
      last = p;
      // 优先走 WebView 原生消息桥(远程页面 invoke 会被 ACL 拒)
      var payload = { sessionId: window.__veltrixVerifySession, present: p };
      if (!(window.__veltrixSignal && window.__veltrixSignal('verify', payload))) {
        try {
          window.__TAURI_INTERNALS__.invoke('report_collect_verify', payload);
        } catch (e) {}
      }
    }
  }
  setTimeout(tick, 1200);
  setInterval(tick, 1500);
})();"#;

    TEMPLATE
        .replace("__SESSION__", &session_id.to_string())
        .replace("__SEL__", &sel)
        .replace("__TXT__", &txt)
        .replace("__URL__", &url)
}

/// 登录命令名;与 Rust 端 `#[tauri::command] login_status_report` 对应。
pub const LOGIN_STATUS_COMMAND: &str = "login_status_report";

/// 构造「登录态自检」注入脚本(登录窗口用)。页面内每隔数秒判断登录态,
/// 结论变化时经 `login_status_report` 回传:`in`(已登录)/ `out`(明确未登录)。
///
/// 判定优先级:命中「已登录」DOM 特征 或 登录 Cookie → in;否则页面就绪且存在可见登录
/// CTA → out;其余(加载中 / 不确定)不回传,保持沉默,避免误判。
pub fn build_login_check_script(
    account_id: &str,
    logged_in_selectors: &[String],
    logged_out_texts: &[String],
    login_cookie_names: &[String],
) -> String {
    let account_json = serde_json::to_string(account_id).unwrap_or_else(|_| "\"\"".to_string());
    let in_sel = serde_json::to_string(logged_in_selectors).unwrap_or_else(|_| "[]".to_string());
    let out_text = serde_json::to_string(logged_out_texts).unwrap_or_else(|_| "[]".to_string());
    let cookies = serde_json::to_string(login_cookie_names).unwrap_or_else(|_| "[]".to_string());

    const TEMPLATE: &str = r#"(function () {
  if (window.__veltrixLoginCheck) return;
  window.__veltrixLoginCheck = true;
  var ACCOUNT = __ACCOUNT__;
  var IN_SEL = __IN_SEL__;
  var OUT_TEXT = __OUT_TEXT__;
  var COOKIES = __COOKIES__;
  var last = '';

  function visible(el) {
    if (!el || el.offsetParent === null) return false;
    var r = el.getBoundingClientRect();
    return r.width > 0 && r.height > 0;
  }
  // 命中任一「已登录」选择器(且元素可见)
  function hasLoggedIn() {
    for (var i = 0; i < IN_SEL.length; i++) {
      try { if (visible(document.querySelector(IN_SEL[i]))) return true; } catch (e) {}
    }
    return false;
  }
  // document.cookie 含任一登录 Cookie 名
  function hasLoginCookie() {
    if (!COOKIES.length) return false;
    var c = document.cookie || '';
    for (var i = 0; i < COOKIES.length; i++) {
      if (c.indexOf(COOKIES[i] + '=') !== -1) return true;
    }
    return false;
  }
  // 存在文本恰为登录 CTA、且可见的可点元素
  function hasLoginCta() {
    if (!OUT_TEXT.length) return false;
    var nodes = document.querySelectorAll('button,a,div,span,[role="button"]');
    for (var i = 0; i < nodes.length; i++) {
      var t = (nodes[i].textContent || '').trim();
      for (var j = 0; j < OUT_TEXT.length; j++) {
        if (t === OUT_TEXT[j] && visible(nodes[i])) return true;
      }
    }
    return false;
  }

  function verdict() {
    if (document.readyState !== 'complete') return '';
    if (hasLoggedIn() || hasLoginCookie()) return 'in';
    if (hasLoginCta()) return 'out';
    return ''; // 不确定:保持沉默
  }

  function tick() {
    var v = verdict();
    if (v && v !== last) {
      last = v;
      try {
        window.__TAURI_INTERNALS__.invoke('login_status_report', { accountId: ACCOUNT, status: v });
      } catch (e) {}
    }
  }
  setTimeout(tick, 1500);   // 给首屏渲染留时间再首检
  setInterval(tick, 2500);  // 持续自检,登录/登出即时反馈
})();"#;

    TEMPLATE
        .replace("__ACCOUNT__", &account_json)
        .replace("__IN_SEL__", &in_sel)
        .replace("__OUT_TEXT__", &out_text)
        .replace("__COOKIES__", &cookies)
}

/// 构造单轮滚动脚本:滚到底部以触发平台的分页加载接口。
///
/// RPA 的「翻页」由 Rust 端循环调用本脚本 + 间隔等待驱动,而非一段长脚本,
/// 这样每轮之间可受 `scroll_interval_ms` 控制节奏,降低风控概率。
pub fn build_scroll_eval() -> String {
    "window.scrollTo(0, document.body.scrollHeight);".to_string()
}

/// 评论区专用滚动脚本:整页 scrollTo 只能覆盖「评论在视频下方」的布局;
/// 抖音等平台的详情页存在「评论在右侧面板」布局,评论列表是内部滚动容器,
/// 整页滚动滚不动它。此脚本先按评论标记元素(data-e2e/class/id 含 comment)
/// 向上找最近的可滚祖先并把它滚到底(附带派发 WheelEvent,兼容只认滚轮的懒加载),
/// 再整页 scrollTo 兜底,两种布局都能触发评论分页。
pub fn build_comment_scroll_eval() -> String {
    r#"(function () {
  function isScrollable(el) {
    if (!el || el === document.body || el === document.documentElement) return false;
    try {
      var st = getComputedStyle(el);
      return /(auto|scroll|overlay)/.test(st.overflowY) && el.scrollHeight > el.clientHeight + 60;
    } catch (e) { return false; }
  }
  function findCommentScroller() {
    var markers = document.querySelectorAll('[data-e2e*="comment"], [class*="comment" i], [id*="comment" i]');
    for (var i = 0; i < markers.length; i++) {
      var cur = markers[i], depth = 0;
      while (cur && depth < 12) {
        if (isScrollable(cur)) return cur;
        cur = cur.parentElement; depth++;
      }
    }
    return null;
  }
  try {
    var sc = findCommentScroller();
    if (sc) {
      var r = sc.getBoundingClientRect();
      var opt = {
        bubbles: true, cancelable: true, deltaY: 800, deltaMode: 0,
        clientX: r.left + r.width / 2, clientY: r.top + Math.min(r.height / 2, 300)
      };
      sc.dispatchEvent(new WheelEvent('wheel', opt));
      sc.scrollTop = sc.scrollHeight;
      sc.dispatchEvent(new Event('scroll', { bubbles: true }));
    }
  } catch (e) {}
  window.scrollTo(0, document.body.scrollHeight);
})();"#
        .to_string()
}

/// 非 Windows(主要是 macOS)的「真实滚轮」对等实现:向**内容最高的可滚容器**派发
/// 一个 `WheelEvent` 并直接抬高 scrollTop,触发只认滚轮事件的页面(快手 / 小红书等)的
/// 懒加载。Windows 走窗口消息级 `WM_MOUSEWHEEL`;mac 无需辅助功能权限、后台窗口也能滚,
/// 但合成事件的可信度低于真实硬件滚轮,属当前可用的最佳近似(已标注待本机实测校准)。
pub fn build_wheel_eval() -> String {
    r#"(function () {
  function findScroller() {
    var docEl = document.scrollingElement || document.documentElement || document.body;
    var best = docEl, bestH = docEl ? docEl.scrollHeight : 0;
    var all = document.querySelectorAll('*');
    for (var i = 0; i < all.length; i++) {
      var el = all[i];
      try {
        var st = getComputedStyle(el);
        if (/(auto|scroll)/.test(st.overflowY) && el.scrollHeight > el.clientHeight + 100 && el.scrollHeight > bestH) {
          bestH = el.scrollHeight; best = el;
        }
      } catch (e) {}
    }
    return best;
  }
  try {
    var sc = findScroller();
    var r = sc.getBoundingClientRect ? sc.getBoundingClientRect() : { left: 0, top: 0, width: 0, height: 0 };
    var opt = {
      bubbles: true, cancelable: true, deltaY: 600, deltaMode: 0,
      clientX: r.left + r.width / 2, clientY: r.top + Math.min(r.height / 2, 300)
    };
    sc.dispatchEvent(new WheelEvent('wheel', opt));
    if (typeof sc.scrollTop === 'number') sc.scrollTop += 600;
    sc.dispatchEvent(new Event('scroll', { bubbles: true }));
    window.dispatchEvent(new Event('scroll'));
  } catch (e) {}
})();"#
        .to_string()
}

/// 构造「按关键词导航到搜索结果页」的脚本。
///
/// keyword 在页面侧用 `encodeURIComponent` 编码,避免中文 / 特殊字符破坏 URL;
/// `assign` 触发一次正常导航,使 `initialization_script` 在新页面重新挂载 hook。
pub fn build_search_eval(template: &str, keyword: &str, extra_query: &str) -> String {
    let tpl = template.replace('\\', "\\\\").replace('\'', "\\'");
    let kw = keyword.replace('\\', "\\\\").replace('\'', "\\'");
    let extra = extra_query.replace('\\', "\\\\").replace('\'', "\\'");
    format!(
        "(function () {{ var kw = encodeURIComponent('{kw}'); \
         var url = '{tpl}'.replace('{{keyword}}', kw); \
         var extra = '{extra}'; \
         if (extra) {{ url += (url.indexOf('?') >= 0 ? '&' : '?') + extra; }} \
         window.location.assign(url); }})();"
    )
}

/// 构造「按内容 ID 导航到详情页」的脚本(评论采集用)。`{id}` 替换为内容 ID,
/// `{token}` 替换为鉴权 token(小红书 xsec_token;抖音无此占位,传空即可)。
///
/// 值经 `encodeURIComponent` 编码;`assign` 触发正常导航,使拦截 hook 在详情页重新挂载。
pub fn build_detail_eval(template: &str, id: &str, token: &str) -> String {
    let tpl = template.replace('\\', "\\\\").replace('\'', "\\'");
    let id_esc = id.replace('\\', "\\\\").replace('\'', "\\'");
    let token_esc = token.replace('\\', "\\\\").replace('\'', "\\'");
    // split/join 做全局替换:抖音「主页模态」详情模板里 {id} 出现两次(modal_id={id}&vid={id}),
    // String.replace 只换首个会漏掉第二个,导致 URL 残留 vid={id} 而打不开。
    format!(
        "(function () {{ var id = encodeURIComponent('{id_esc}'); \
         var token = encodeURIComponent('{token_esc}'); \
         window.location.assign('{tpl}'.split('{{id}}').join(id).split('{{token}}').join(token)); }})();"
    )
}

/// 构造「详情页 SSR 数据回传」脚本(抖音直链刷新的第二通道):视频页 /video/{id} 的
/// RENDER_DATA / _ROUTER_DATA 内嵌完整 aweme_detail(含 video.play_addr / music.play_url),
/// JS 直读 DOM 即可拿到,完全不经过 WebView2 GetContent——规避「拦截器空 stream 丢包」导致
/// 直链刷新整批失败的根因。递归找到目标 aweme_id 的详情对象后,包成 {aweme_detail: ...}、
/// 以合成详情接口 URL 经 intercept_sink_push 命令回传 sink(Tauri invoke,与 intercept_push
/// 同通道;chrome.webview.postMessage 在采集窗口实测不送达),适配器按 ContentDetail 原路径解析。
pub fn build_detail_ssr_eval(label: &str, content_id: &str) -> String {
    let id_json = serde_json::to_string(content_id).unwrap_or_else(|_| "\"\"".to_string());
    let label_json = serde_json::to_string(label).unwrap_or_else(|_| "\"\"".to_string());
    // 必报结果:命中(__ssr=hit)带详情 JSON;未命中(__ssr=miss)带诊断位
    // (rd=RENDER_DATA 是否存在,router=_ROUTER_DATA 是否存在)——此前 miss 静默 return,
    // 无法区分「SSR 无详情」与「脚本没执行 / invoke 没送达」
    format!(
        r#"(function(){{
  var target = {id}, label = {label};
  var hasRD = false, hasRouter = false;
  function report(mark, body, diag) {{
    try {{
      window.__TAURI_INTERNALS__.invoke('intercept_sink_push', {{
        label: label,
        url: 'https://www.douyin.com/aweme/v1/web/aweme/detail/?aweme_id=' + target + '&__ssr=' + mark + '&' + diag,
        body: body
      }});
    }} catch (e) {{}}
  }}
  function walk(o, d) {{
    if (!o || typeof o !== 'object' || d > 14) return null;
    if (o.aweme_detail && typeof o.aweme_detail === 'object') return o.aweme_detail;
    if (o.aweme_id != null && String(o.aweme_id) === target && o.video) return o;
    for (var k in o) {{ var r = walk(o[k], d + 1); if (r) return r; }}
    return null;
  }}
  var detail = null;
  try {{
    var el = document.getElementById('RENDER_DATA');
    hasRD = !!el;
    if (el) detail = walk(JSON.parse(decodeURIComponent(el.textContent)), 0);
  }} catch (e) {{}}
  try {{ hasRouter = !!window._ROUTER_DATA; }} catch (e) {{}}
  if (!detail && hasRouter) {{ try {{ detail = walk(window._ROUTER_DATA, 0); }} catch (e) {{}} }}
  var diag = 'rd=' + (hasRD ? 1 : 0) + '&router=' + (hasRouter ? 1 : 0);
  if (detail && detail.video) {{
    report('hit', JSON.stringify({{ aweme_detail: detail }}), diag);
  }} else {{
    report('miss', '', diag);
  }}
}})();"#,
        id = id_json,
        label = label_json
    )
}

/// 构造「评论 API 直采」注入脚本(抖音):借页面自己的签名函数(`window.byted_acrawler`,
/// webmssdk 挂载,跟随抖音改版自动是最新版,无需逆向 a_bogus 算法)对翻页请求签名,
/// 在页面上下文直接 fetch 评论接口分页拉取。拉到的响应会被页内 fetch hook 按既有特征
/// 自动回传(与滚动采集同通道,适配器解析 / 入库零改动)。
/// `jobs` 为 1~2 个 (aweme_id, template_url):双路时交错翻页并发采集(同账号窗口
/// 数据目录带锁开不出第二个窗口、脚本又互相接管,页内多路 fetch 是唯一并发姿势)。
/// `template_url` 取该视频会话内已拦截的真实 comment/list 请求:msToken / device 等
/// 公共参数全带着,脚本只换 cursor 重签。传空串 = 无模板模式:脚本用标准公共参数
/// (+ 页面 navigator / cookie 现取 msToken)+ `aweme_id` 凭空构造请求,签名机制不变。
/// 完成(或失败 / 被中止)后经 `comment_api_done` 命令回传结果 JSON(jobs 数组逐视频汇总)。
pub fn build_comment_api_collect_eval(
    session_id: u64,
    jobs: &[(&str, &str)],
    limit: usize,
    max_pages: u32,
    ms_token: &str,
    fp: &str,
) -> String {
    // serde 序列化为 JS 字符串字面量,免手工转义
    let jobs_json = serde_json::to_string(
        &jobs
            .iter()
            .map(|(id, tpl)| serde_json::json!({ "awemeId": id, "templateUrl": tpl }))
            .collect::<Vec<_>>(),
    )
    .unwrap_or_else(|_| "[]".to_string());
    let ms_token_json = serde_json::to_string(ms_token).unwrap_or_else(|_| "\"\"".to_string());
    let fp_json = serde_json::to_string(fp).unwrap_or_else(|_| "\"\"".to_string());
    format!(
        r#"(function () {{
  // 代际接管:新脚本注入即接替,旧脚本发现代际过期后静默退出(finish 里不再回传)。
  // 不能用 Running 标志挡注入:旧脚本若卡在 5s 重试眠里,新注入会被挡下、整视频白等 90s
  var GEN = (window.__veltrixCommentApiGen || 0) + 1;
  window.__veltrixCommentApiGen = GEN;
  window.__veltrixCommentApiRunning = true;
  window.__veltrixCommentApiAbort = false;
  var CFG = {{ sessionId: {session_id}, jobs: {jobs_json}, limit: {limit}, count: 20, maxPages: {max_pages}, msToken: {ms_token_json}, fp: {fp_json} }};

  function hud(level, msg) {{
    try {{ if (window.__veltrixHud && window.__veltrixHud.log) window.__veltrixHud.log({{ level: level, message: msg }}); }} catch (e) {{}}
  }}
  function sleep(ms) {{ return new Promise(function (r) {{ setTimeout(r, ms); }}); }}
  function getCookie(name) {{
    var m = document.cookie.match(new RegExp('(?:^|; )' + name + '=([^;]*)'));
    return m ? decodeURIComponent(m[1]) : '';
  }}

  // 无模板模式:标准公共参数 + 页面环境现取(msToken / 屏幕 / UA),业务参数只认 aweme_id。
  // a_bogus 对整个查询串现签,参数集与页面真实请求等价,无需面板先发出 comment/list。
  function buildStdUrl(awemeId) {{
    var u = new URL('https://www.douyin.com/aweme/v1/web/comment/list/');
    var sp = u.searchParams;
    var chromeVer = (navigator.userAgent.match(/Chrome\/([\d.]+)/) || [])[1] || '';
    sp.set('device_platform', 'webapp');
    sp.set('aid', '6383');
    sp.set('channel', 'channel_pc_web');
    sp.set('aweme_id', awemeId);
    sp.set('item_type', '0');
    sp.set('pc_client_type', '1');
    sp.set('version_code', '290100');
    sp.set('version_name', '29.1.0');
    sp.set('cookie_enabled', 'true');
    sp.set('screen_width', String(window.screen.width || 1920));
    sp.set('screen_height', String(window.screen.height || 1080));
    sp.set('browser_language', navigator.language || 'zh-CN');
    sp.set('browser_platform', navigator.platform || 'Win32');
    sp.set('browser_name', 'Chrome');
    sp.set('browser_version', chromeVer);
    sp.set('browser_online', 'true');
    sp.set('engine_name', 'Blink');
    sp.set('engine_version', chromeVer);
    sp.set('os_name', 'Windows');
    sp.set('os_version', '10');
    sp.set('cpu_core_num', String(navigator.hardwareConcurrency || 8));
    sp.set('device_memory', String(navigator.deviceMemory || 8));
    sp.set('platform', 'PC');
    sp.set('downlink', String((navigator.connection && navigator.connection.downlink) || 10));
    // msToken 优先用 Rust 侧 CookieManager 读到的值(含 HttpOnly);拿不到再退回
    // document.cookie 现取。verifyFp/fp(s_v_web_id)同理——页面真实评论请求都带
    // 这组指纹参数,凭空构造缺它们更容易被风控直接回 HTML 验证页
    sp.set('msToken', CFG.msToken || getCookie('msToken'));
    var fpv = CFG.fp || getCookie('s_v_web_id');
    if (fpv) {{ sp.set('verifyFp', fpv); sp.set('fp', fpv); }}
    return u;
  }}

  // 每视频的请求 URL 骨架:优先该视频会话内拦截到的真实 comment/list 请求当模板
  // (公共参数 / 指纹全带着);无模板用标准参数 + 页面环境现取构造。signKey 挂在 URL 对象上复用
  function makeUrl(job) {{
    var u;
    try {{ u = job.templateUrl ? new URL(job.templateUrl) : buildStdUrl(job.awemeId); }}
    catch (e) {{ return null; }}
    var sp = u.searchParams;
    // 业务参数以当前视频为准:模板可能携带来自其他视频的 aweme_id(同窗口历史拦截),
    // 必须强制覆盖,否则会把别的视频的评论拉进来
    sp.set('aweme_id', job.awemeId);
    // 签名参数名跟页面真实请求走(新版 a_bogus / 旧版 X-Bogus),旧值删掉、每页重签
    var signKey = sp.has('a_bogus') ? 'a_bogus' : (sp.has('X-Bogus') ? 'X-Bogus' : 'a_bogus');
    sp.delete('a_bogus'); sp.delete('X-Bogus');
    u.signKey = signKey;
    return u;
  }}

  // 找页面自带的请求签名函数;返回形态兼容字符串(纯签名值)与对象({{ a_bogus: v }} / {{ 'X-Bogus': v }})
  function findSigner() {{
    var ac = window.byted_acrawler;
    if (!ac) return null;
    var fns = [];
    if (typeof ac.frontierSign === 'function') fns.push(ac.frontierSign);
    if (typeof ac.sign === 'function') fns.push(ac.sign);
    if (!fns.length) return null;
    return function (url) {{
      for (var i = 0; i < fns.length; i++) {{
        try {{
          var r = fns[i].call(ac, {{ url: url }});
          if (typeof r === 'string' && r) return {{ str: r }};
          if (r && typeof r === 'object') return {{ obj: r }};
        }} catch (e) {{}}
      }}
      return null;
    }};
  }}

  function finish(r) {{
    // 代际过期(已被下批脚本接管):静默退出——回传会污染下批的完成判定
    if (window.__veltrixCommentApiGen !== GEN) return;
    window.__veltrixCommentApiRunning = false;
    // 结果同时留在 window 上:Rust 侧每轮 eval 回读兜底——信号桥(postMessage / invoke)
    // 在部分环境会静默丢失(页内已收尾,Rust 干等 90s 停滞看门狗),回读不依赖任何桥
    try {{ window.__veltrixCommentApiResult = JSON.stringify(r); }} catch (e) {{}}
    // 收尾留痕:区分「脚本没结束」与「结束了但 comment_api_done 回传丢失」
    var n = (r.jobs && r.jobs.length) ? r.jobs.length : 0;
    hud('info', '🏁 直采脚本收尾 · ' + n + ' 个视频' + (r.error ? ' · error=' + r.error : ''));
    // 回传走 WebView 原生消息桥(不受远程页面 ACL 限制);桥不可用才退回 invoke 兜底
    var sent = window.__veltrixSignal && window.__veltrixSignal('api_done', {{ sessionId: CFG.sessionId, result: JSON.stringify(r) }});
    if (!sent) {{
      try {{
        var p = window.__TAURI_INTERNALS__.invoke('comment_api_done', {{ sessionId: CFG.sessionId, result: JSON.stringify(r) }});
        if (p && p.catch) p.catch(function (e) {{ hud('warn', '⚠️ 直采结果回传失败: ' + e); }});
      }} catch (e) {{ hud('warn', '⚠️ 直采结果回传异常: ' + e); }}
    }}
  }}

  // result 提到 main 外:未捕获异常(resp.text() 拒绝、页面冻结等)时兜底回传,
  // 否则脚本静默死掉,Rust 侧只能死等超时、日志里看不到任何原因
  var result = {{ used: false, error: null, jobs: [] }};

  // 单页连续失败重试上限(5s/次,≈2.5 分钟):覆盖人工解滑块的耗时;
  // 超限才判定该视频直采失败
  var MAX_RETRY = 30;

  // 每视频翻一页。返回 'ok'=推进 / 'done'=该视频到底 / 'fail'=该视频失败 /
  // 'hard'=风控硬拒(整批停止)/ 'retry'=5s 后重试同页 / 'aborted'=中止。
  // 视频维度的状态(页数 / 累计 / 到底 / 失败)都挂在 job 上,主循环按 job 交错推进
  async function fetchPage(job, signer) {{
    if (window.__veltrixCommentApiAbort || window.__veltrixCommentApiGen !== GEN) return 'aborted';
    if (!job.env) {{
      job.env = makeUrl(job);
      if (!job.env) {{ job.failed = true; job.error = 'bad-template'; return 'fail'; }}
    }}
    var sp = job.env.searchParams;
    sp.set('cursor', String(job.cursor));
    sp.set('count', String(CFG.count));
    var unsigned = job.env.origin + job.env.pathname + '?' + sp.toString();
    var sig = signer(unsigned);
    if (!sig) {{ job.failed = true; job.error = 'sign-failed'; return 'fail'; }}
    // 首页留痕签名形态:frontierSign/sign 返回的键名随 webmssdk 版本变化,
    // 若不再是 a_bogus/X-Bogus 而签了别的键,服务端必拒——日志里一眼可辨
    if (job.pages === 0 && job.retries === 0) {{
      hud('info', '🔑 视频 ' + job.awemeId.slice(-6) + ' 首页签名形态: ' + (sig.str ? 'string(len=' + sig.str.length + ')' : 'obj[' + Object.keys(sig.obj).join(',') + ']'));
    }}
    var signed = unsigned + '&' + (sig.str
      ? job.env.signKey + '=' + encodeURIComponent(sig.str)
      : Object.keys(sig.obj).map(function (k) {{ return k + '=' + encodeURIComponent(sig.obj[k]); }}).join('&'));
    // 失败重试:风控(滑块 / 静默吞请求)由用户在窗口里手动解除,解除后 5s 重试即恢复,
    // 不因单次失败放弃整个视频。fetch 加 15s 超时,防止验证挂起请求永不返回
    var resp = null, text = null, json = null, failReason = null;
    try {{
      var ctrl = new AbortController();
      var timer = setTimeout(function () {{ ctrl.abort(); }}, 15000);
      // 响应缓慢探针:>5s 未返回即留痕——区分「fetch 被挂起」与「脚本根本没发出请求」
      var slowTimer = setTimeout(function () {{
        hud('warn', '⏳ 视频 ' + job.awemeId.slice(-6) + ' 第 ' + (job.pages + 1) + ' 页响应缓慢(>5s 未返回,可能被风控挂起)…');
      }}, 5000);
      try {{ resp = await fetch(signed, {{ credentials: 'include', signal: ctrl.signal }}); }}
      finally {{ clearTimeout(timer); clearTimeout(slowTimer); }}
      text = await resp.text();
      try {{
        json = JSON.parse(text);
      }} catch (pe) {{
        // 非 JSON 响应:风控 / WAF 常直接回 HTML 验证页(HTTP 200、不跳转,
        // 下面的 blocked-redirect 检不到)。同参数重试必败,判硬拒立即收尾;
        // 并把状态码 + content-type + 页面标题打进日志,一眼区分滑块页 /
        // WAF 拦截页 / 网关错误页
        var head = (text || '').replace(/\s+/g, ' ');
        if (head.charAt(0) === '<') {{
          // title 可能在头部 160 字符之外,放大取样范围再匹配
          var tm = head.slice(0, 4000).match(/<title[^>]*>([^<]*)<\/title>/i);
          var ct = '';
          try {{ ct = resp.headers.get('content-type') || ''; }} catch (he) {{}}
          // 带上重定向标记与最终 URL 路径:区分「WAF 直回验证页」与「被 302 到落地页」
          var finalPath = '';
          try {{ finalPath = new URL(resp.url).pathname; }} catch (ue) {{}}
          failReason = 'blocked-html(http-' + resp.status + (ct ? ' ' + ct : '') +
            (resp.redirected ? ', redirected→' + finalPath : '') +
            (tm ? ', title=' + tm[1] : ', ' + head.slice(0, 80)) + ')';
        }} else {{
          failReason = 'bad-json(http-' + resp.status + ', ' + head.slice(0, 80) + ')';
        }}
      }}
    }} catch (e) {{
      failReason = (e && e.name === 'AbortError') ? 'timeout' : String(e);
    }}
    // 风控硬拒:被 302 到搜索页/验证页(resp.url 离开接口路径;后台 fetch 不渲染滑块,
    // 同参数重试必败)——立即收尾交回 Rust 补发重试,不占 30×5s 重试空等
    if (resp && resp.redirected && resp.url.indexOf('/aweme/v1/') === -1) failReason = 'blocked-redirect(http-' + resp.status + ')';
    if (!failReason && resp && !resp.ok) failReason = 'http-' + resp.status;
    if (!failReason && !json) failReason = 'bad-json';
    if (!failReason && json.status_code !== undefined && json.status_code !== 0) {{
      failReason = 'status-' + json.status_code + (json.status_msg ? ': ' + json.status_msg : '');
    }}
    // 首页空评论的区分:接口返回 total=0 = 该视频真的没有评论(或全被平台过滤),
    // 正常结束、不重试;total>0 或缺省但列表空 = 风控静默吞的概率高,按失败重试。
    // 后续页空则视为正常到底,不重试
    if (!failReason && job.pages === 0 && !(json.comments || []).length) {{
      var declared = (typeof json.total === 'number') ? json.total : -1;
      if (declared === 0) {{ job.noComments = true; job.done = true; job.used = true; return 'done'; }}
      failReason = 'empty-first-page';
    }}
    if (failReason) {{
      // 风控硬拒不重试:直接收尾(job.failed → Rust 侧按整批结果判定 / 补发重试)。
      // blocked-html 同理:服务端已明确回验证/拦截页,同参数重试必败,白等 15×5s
      if (failReason.indexOf('blocked-redirect') === 0 || failReason.indexOf('blocked-html') === 0) {{
        job.failed = true; job.error = failReason; return 'hard';
      }}
      job.retries++;
      if (job.retries > MAX_RETRY) {{ job.failed = true; job.error = 'retry-exhausted: ' + failReason; return 'fail'; }}
      hud('warn', '⏳ 视频 ' + job.awemeId.slice(-6) + ' 第 ' + (job.pages + 1) + ' 页请求失败(' + failReason + ')· 5s 后重试 ' + job.retries + '/' + MAX_RETRY + '(若弹出滑块请在窗口中完成)');
      await sleep(5000);
      return 'retry';
    }}
    job.retries = 0;
    var items = json.comments || [];
    job.comments += items.length;
    job.pages++;
    hud('info', '⚡ 直采 · 视频 ' + job.awemeId.slice(-6) + ' 第 ' + job.pages + ' 页 +' + items.length + ' 条(累计 ' + job.comments + ')');
    if (items.length === 0) {{ job.done = true; job.used = job.comments > 0; return 'done'; }}
    if (CFG.limit > 0 && job.comments >= CFG.limit) {{ job.done = true; job.used = true; return 'done'; }}
    // 不足一页即到底:has_more 不可靠(实测 3 条评论也标 has_more=1,
    // 再翻页请求会被静默吞掉、白等停滞看门狗)
    if (items.length < CFG.count) {{ job.done = true; job.used = true; return 'done'; }}
    if (!json.has_more) {{ job.done = true; job.used = true; return 'done'; }}
    job.cursor = (typeof json.cursor === 'number') ? json.cursor : (job.cursor + items.length);
    return 'ok';
  }}

  async function main() {{
    // signer 最多等 10s:脚本注入时页面 SDK(webmssdk)可能还没挂载完
    var signer = null;
    for (var si = 0; si < 20; si++) {{
      if (window.__veltrixCommentApiAbort || window.__veltrixCommentApiGen !== GEN) {{ result.error = 'aborted'; return finish(result); }}
      signer = findSigner();
      if (signer) break;
      await sleep(500);
    }}
    if (!signer) {{ result.error = 'no-signer'; return finish(result); }}
    // 诊断留痕:确认签名来源 / 窗口所在页面 / msToken、ttwid 是否在——直采被风控时
    // 第一眼就能区分「签名环境缺失」与「请求被服务端拦」
    var sigSrc = (window.byted_acrawler && typeof window.byted_acrawler.frontierSign === 'function') ? 'frontierSign' : 'sign';
    var msTok = CFG.msToken || getCookie('msToken');
    var fpv = CFG.fp || getCookie('s_v_web_id');
    hud('info', '🔏 直采签名就绪(' + sigSrc + ')· 页面 ' + location.host + location.pathname +
      ' · 并发 ' + CFG.jobs.length + ' 路 · msToken=' + (msTok ? '有' : '无') + ' · fp=' + (fpv ? '有' : '无') +
      ' · ttwid=' + (getCookie('ttwid') ? '有' : '无'));
    // 每视频初始化翻页状态(env 惰性构建:makeUrl 在首翻时执行)
    for (var j = 0; j < CFG.jobs.length; j++) {{
      CFG.jobs[j].cursor = 0;
      CFG.jobs[j].pages = 0;
      CFG.jobs[j].comments = 0;
      CFG.jobs[j].retries = 0;
      CFG.jobs[j].done = false;
      CFG.jobs[j].failed = false;
      CFG.jobs[j].used = false;
      CFG.jobs[j].noComments = false;
      CFG.jobs[j].error = null;
      CFG.jobs[j].env = null;
    }}
    // 双路并发:每轮对每个未完成视频各翻一页交错推进;一路风控硬拒即整批停止
    // (同窗口同环境,另一路大概率同命;已完成视频的结果保留在 jobs 里回传),
    // 由 Rust 侧判定整批成败与补发重试
    var hardStop = false;
    while (true) {{
      if (window.__veltrixCommentApiAbort || window.__veltrixCommentApiGen !== GEN) {{ result.error = 'aborted'; break; }}
      var anyActive = false;
      for (var j = 0; j < CFG.jobs.length; j++) {{
        var job = CFG.jobs[j];
        if (job.done || job.failed) continue;
        anyActive = true;
        var r = await fetchPage(job, signer);
        if (r === 'hard' || r === 'aborted') {{ if (r === 'hard') hardStop = true; break; }}
        // 'retry' / 'fail' / 'done' / 'ok' 均已由 job 状态承载,继续下一路
      }}
      if (hardStop) break;
      if (window.__veltrixCommentApiAbort || window.__veltrixCommentApiGen !== GEN) break;
      if (!anyActive) break;
      // 拟人翻页间隔:直采特征明显,自觉压风控;双路每轮翻 2 页,间隔相应放宽,
      // 交错后的请求频率与单路相当
      if (CFG.jobs.length > 1) {{ await sleep(1000 + Math.floor(Math.random() * 1000)); }}
      else {{ await sleep(700 + Math.floor(Math.random() * 900)); }}
    }}
    // 汇总:每个视频独立成一条结果(Rust 侧逐视频判定 / 日志),整体 used = 至少一路成功
    var jobs = [];
    for (var j = 0; j < CFG.jobs.length; j++) {{
      var job = CFG.jobs[j];
      jobs.push({{ awemeId: job.awemeId, used: !!job.used, pages: job.pages, comments: job.comments, noComments: !!job.noComments, error: job.error || null }});
    }}
    result.jobs = jobs;
    result.used = false;
    for (var j = 0; j < jobs.length; j++) if (jobs[j].used) result.used = true;
    if (!result.used && !result.error) {{
      for (var j = 0; j < jobs.length; j++) if (jobs[j].error) {{ result.error = jobs[j].error; break; }}
      if (!result.error) result.error = 'empty';
    }}
    finish(result);
  }}
  main().catch(function (e) {{
    result.error = 'exception: ' + (e && e.message ? e.message : String(e));
    finish(result);
  }});
}})();"#,
        session_id = session_id,
        jobs_json = jobs_json,
        limit = limit,
        max_pages = max_pages,
    )
}


/// 激活右侧详情面板的「评论」tab。抖音「主页模态」详情(/user/{sec_uid}?modal_id=)默认可能停在
/// 「详情」tab,评论列表与 comment/list 请求要切到「评论」tab 才加载。找文本以「评论」开头的短元素
/// (tab 文案如「评论」/「评论 16」,排除评论正文的长文本)派发全套 pointer+mouse 点击。
/// 兜底:无「评论」文案 tab 的布局(沉浸式播放器 modal,评论入口是动作栏图标按钮,
/// data-e2e="feed-comment-icon",真机实测 2026-08)直接点图标开面板——面板开出后「评论」tab 默认已激活。
/// best-effort:找不到 / 已在评论 tab 均无害(文案 tab 优先,图标仅作兜底,避免面板已开时误点图标反而关掉)。
pub fn build_comment_tab_eval() -> String {
    r#"(function () {
  // 评论面板已开(「全部评论」标志在)直接 no-op:图标点击是开关语义,重复点会把面板关掉
  if (document.body.innerText.indexOf('全部评论') !== -1) return true;
  function fireClick(el) {
    var r = el.getBoundingClientRect();
    var o = { bubbles: true, cancelable: true, clientX: r.left + r.width / 2, clientY: r.top + r.height / 2 };
    try { el.dispatchEvent(new PointerEvent('pointerover', o)); } catch (e) {}
    el.dispatchEvent(new MouseEvent('mouseover', o));
    try { el.dispatchEvent(new PointerEvent('pointerdown', o)); } catch (e) {}
    el.dispatchEvent(new MouseEvent('mousedown', o));
    try { el.dispatchEvent(new PointerEvent('pointerup', o)); } catch (e) {}
    el.dispatchEvent(new MouseEvent('mouseup', o));
    el.dispatchEvent(new MouseEvent('click', o));
  }
  var nodes = document.querySelectorAll('[role="tab"],[class*="tab" i],span,div,li');
  for (var i = 0; i < nodes.length; i++) {
    var el = nodes[i];
    var t = (el.textContent || '').trim();
    // tab 文案:以「评论」开头且整体很短(后面最多跟数量);长文本是评论正文/容器,排除
    if (t.length > 8 || !/^评论(\s|\(|（|\d|$)/.test(t)) continue;
    if (el.closest && el.closest('[aria-hidden="true"]')) continue;
    var r = el.getBoundingClientRect();
    if (r.width < 1 || r.height < 1) continue;
    fireClick(el);
    return true;
  }
  // 兜底:沉浸式播放器布局没有「评论」文案 tab,点动作栏评论图标开面板
  var icon = document.querySelector('[data-e2e="feed-comment-icon"]');
  if (icon) {
    var ir = icon.getBoundingClientRect();
    if (ir.width > 0 && ir.height > 0) {
      fireClick(icon);
      return true;
    }
  }
  return false;
})()"#
        .to_string()
}

/// 构造「按文案点击元素」的脚本(排序 / 时间筛选用)。在可点击元素里找 textContent
/// 精确等于任一 label 的,派发鼠标事件点击第一个匹配。用文案而非 class 选择器:更稳
/// (class 易变)、且无需逐平台抓包。labels 为空时不做任何操作(综合/不限即默认)。
pub fn build_select_eval(labels: &[String]) -> String {
    let labels_json = serde_json::to_string(labels).unwrap_or_else(|_| "[]".to_string());
    const TEMPLATE: &str = r#"(function () {
  var LABELS = __LABELS__;
  if (!LABELS.length) return;
  var nodes = document.querySelectorAll('button,a,span,div,li,[role="tab"],[role="button"]');
  for (var i = 0; i < nodes.length; i++) {
    var el = nodes[i];
    var t = (el.textContent || '').trim();
    var hit = false;
    for (var j = 0; j < LABELS.length; j++) { if (t === LABELS[j]) { hit = true; break; } }
    if (!hit) continue;
    // 跳过 aria-hidden 的装饰/诱饵层(小红书在每个筛选项上叠了不可见同名代理 data-hp-*,点它无效)及零尺寸元素
    if (el.closest && el.closest('[aria-hidden="true"]')) continue;
    var r = el.getBoundingClientRect();
    if (r.width < 1 || r.height < 1) continue;
    try {
      el.scrollIntoView({ block: 'center' });
      var o = { bubbles: true, clientX: r.left + r.width / 2, clientY: r.top + r.height / 2 };
      // 先派发 hover/move:抖音/小红书「筛选」等下拉靠 hover(React/Vue 的 mouseenter←mouseover)展开,只点不 hover 展不开。
      // pointer 全套(down/up)必须带:抖音筛选项的点击处理挂在 pointer 事件上,只发 mouse 合成事件点不中(浮层开着但选项不生效)。
      try { el.dispatchEvent(new PointerEvent('pointerover', o)); } catch (e) {}
      el.dispatchEvent(new MouseEvent('mouseover', o));
      el.dispatchEvent(new MouseEvent('mousemove', o));
      try { el.dispatchEvent(new PointerEvent('pointerdown', o)); } catch (e) {}
      el.dispatchEvent(new MouseEvent('mousedown', o));
      try { el.dispatchEvent(new PointerEvent('pointerup', o)); } catch (e) {}
      el.dispatchEvent(new MouseEvent('mouseup', o));
      el.dispatchEvent(new MouseEvent('click', o));
    } catch (e) {}
    return;
  }
})();"#;
    TEMPLATE.replace("__LABELS__", &labels_json)
}

// ===================== 浏览器 Agent 动作脚本(同步 IIFE 返回对象,供 ExecuteScript 回读) =====================
//
// 为何与采集的 build_*_eval 分开:浏览器 Agent 用内嵌主窗口的 "agent" 子 webview、不绑登录态/不注入采集 HUD。
// 回读机制:这些脚本是**同步 IIFE 返回一个对象**,由 Rust 侧 `script_eval::eval_json`(WebView2
// ExecuteScript)把返回对象序列化成 JSON 取回——**不走页面 invoke**,故任意域名都能回读
// (原 invoke 回传受 capabilities 远程白名单限制,在非白名单域调不通)。
//
// 关键约束:ExecuteScript **不 await Promise**,故脚本必须同步返回。
// - click/type 在派发事件后**同一同步轮**返回结果,早于点击引发的异步导航拆毁上下文,故结果不丢;
// - navigate 用 assign 拆上下文,回读交给随后单独 eval 的 probe;
// - 「等元素出现」改由 Rust 多次调用 `build_agent_exists_eval` 轮询(替代页面内 setTimeout 轮询)。

/// read_page 默认回读的可见交互元素上限(防止超大页面塞爆上下文)。
pub const AGENT_READ_ELEMENT_CAP: usize = 40;

/// 构造「导航到指定 URL」脚本(fire-and-forget:assign 会拆毁上下文,回读交给随后的 probe)。
/// url 只接受 http/https,由 Rust 侧先校验。
pub fn build_navigate_eval(url: &str) -> String {
    // 用 serde_json 生成安全的 JS 字符串字面量(完整转义换行/回车/引号等);
    // 手工 replace 会漏掉换行 → 单引号字符串跨行 SyntaxError 使整段 eval 失效。
    let url_lit = serde_json::to_string(url).unwrap_or_else(|_| "\"\"".to_string());
    format!("(function () {{ window.location.assign({url_lit}); }})();")
}

/// 构造「探测当前页面」脚本:返回 {url,title,readyState}。
/// 用于 navigate 之后(等导航 settle 后单独 eval 一次拿到落地页信息)。
pub fn build_agent_probe_eval() -> String {
    "(function(){try{return {url:location.href,title:document.title,readyState:document.readyState};}catch(e){return {error:String(e)};}})();".to_string()
}

/// 构造「按 CSS 选择器点击元素」脚本:命中即派发鼠标事件并**同步**返回 {matched,tag,text};
/// 未命中返回 {matched:false}。同步返回早于点击引发的异步导航,故结果不会因跳转丢失。
pub fn build_agent_click_eval(selector: &str) -> String {
    let sel_json = serde_json::to_string(selector).unwrap_or_else(|_| "\"\"".to_string());
    const TEMPLATE: &str = r#"(function () {
  var SEL = __SEL__;
  var el = null;
  try { el = document.querySelector(SEL); } catch (e) { return { error: '非法选择器: ' + String(e) }; }
  if (!el) return { matched: false };
  try {
    el.scrollIntoView({ block: 'center' });
    var r = el.getBoundingClientRect();
    var o = { bubbles: true, clientX: r.left + r.width / 2, clientY: r.top + r.height / 2 };
    el.dispatchEvent(new MouseEvent('mousedown', o));
    el.dispatchEvent(new MouseEvent('mouseup', o));
    el.dispatchEvent(new MouseEvent('click', o));
    return { matched: true, tag: el.tagName, text: (el.textContent || '').trim().slice(0, 60) };
  } catch (e) { return { error: String(e) }; }
})();"#;
    TEMPLATE.replace("__SEL__", &sel_json)
}

/// 构造「向输入框写入文本」脚本:命中 input/textarea/contenteditable,聚焦整体赋值并派发
/// input/change(触发框架受控更新),返回 {matched,tag};未命中返回 {matched:false}。
pub fn build_agent_type_eval(selector: &str, text: &str) -> String {
    let sel_json = serde_json::to_string(selector).unwrap_or_else(|_| "\"\"".to_string());
    let text_json = serde_json::to_string(text).unwrap_or_else(|_| "\"\"".to_string());
    const TEMPLATE: &str = r#"(function () {
  var SEL = __SEL__;
  var TEXT = __TEXT__;
  var el = null;
  try { el = document.querySelector(SEL); } catch (e) { return { error: '非法选择器: ' + String(e) }; }
  if (!el) return { matched: false };
  try {
    el.focus();
    if (el.isContentEditable) { el.textContent = TEXT; }
    else {
      var proto = el.tagName === 'TEXTAREA' ? window.HTMLTextAreaElement.prototype : window.HTMLInputElement.prototype;
      var desc = Object.getOwnPropertyDescriptor(proto, 'value');
      if (desc && desc.set) { desc.set.call(el, TEXT); } else { el.value = TEXT; }
    }
    el.dispatchEvent(new Event('input', { bubbles: true }));
    el.dispatchEvent(new Event('change', { bubbles: true }));
    return { matched: true, tag: el.tagName };
  } catch (e) { return { error: String(e) }; }
})();"#;
    TEMPLATE
        .replace("__SEL__", &sel_json)
        .replace("__TEXT__", &text_json)
}

/// 构造「单次检查元素是否存在」脚本:返回 {matched,visible,text}。
/// 「等元素出现」由 Rust 侧按间隔多次调用本脚本轮询实现(ExecuteScript 不 await Promise,
/// 故不在页面内 setTimeout 轮询)。
pub fn build_agent_exists_eval(selector: &str) -> String {
    let sel_json = serde_json::to_string(selector).unwrap_or_else(|_| "\"\"".to_string());
    const TEMPLATE: &str = r#"(function () {
  var SEL = __SEL__;
  var el = null;
  try { el = document.querySelector(SEL); } catch (e) { return { error: '非法选择器: ' + String(e) }; }
  if (!el) return { matched: false };
  var r = el.getBoundingClientRect();
  return { matched: true, visible: (r.width > 0 && r.height > 0), text: (el.textContent || '').trim().slice(0, 60) };
})();"#;
    TEMPLATE.replace("__SEL__", &sel_json)
}

/// 构造「读取页面」脚本:返回 url / title / readyState、可见交互元素清单(给每个元素打上
/// `data-veltrix-id` 并以 `[data-veltrix-id="N"]` 作为可靠选择器,供后续 click/type 精确命中)、
/// 以及正文摘要(截断)。`cap` 限制元素数量。这是 Agent「看清页面再动手」的核心工具。
pub fn build_agent_read_eval(cap: usize) -> String {
    const TEMPLATE: &str = r#"(function () {
  var CAP = __CAP__;
  try {
    function vis(el) {
      if (!el || el.offsetParent === null) return false;
      var r = el.getBoundingClientRect();
      return r.width > 0 && r.height > 0;
    }
    var nodes = document.querySelectorAll('a,button,input,textarea,select,[role="button"],[role="link"],[role="tab"],[onclick]');
    var items = [];
    for (var i = 0; i < nodes.length && items.length < CAP; i++) {
      var el = nodes[i];
      if (!vis(el)) continue;
      var tag = el.tagName.toLowerCase();
      var label = (el.getAttribute('aria-label') || el.getAttribute('placeholder') || el.value || el.textContent || '').replace(/\s+/g, ' ').trim().slice(0, 50);
      var vid = String(items.length);
      try { el.setAttribute('data-veltrix-id', vid); } catch (e) {}
      items.push({ id: vid, tag: tag, type: (el.getAttribute('type') || ''), text: label, selector: '[data-veltrix-id="' + vid + '"]' });
    }
    var bodyText = (document.body ? (document.body.innerText || '') : '').replace(/\s+/g, ' ').trim().slice(0, 1500);
    return { url: location.href, title: document.title, readyState: document.readyState, elements: items, text: bodyText };
  } catch (e) { return { error: String(e) }; }
})();"#;
    TEMPLATE.replace("__CAP__", &cap.to_string())
}

/// 注入脚本里回传 RPA 执行结果的命令名;与 Rust 端 `#[tauri::command] rpa_done` 对应。
pub const RPA_DONE_COMMAND: &str = "rpa_done";

/// 构造「拟人 RPA 步骤执行器」注入脚本。
///
/// `steps` 序列化为 JS 数组后,在页面内 async 自驱动执行:逐字输入、hover→点击、
/// 轮询等待节点、分段随机滚动、随机停顿——节奏由节点状态 + 随机化驱动而非固定计时,
/// 以贴近真人、降低风控。整段跑完(或某步失败)经 `rpa_done` 回传成败,Rust 据此编排。
///
/// 用占位替换而非 `format!`,规避脚本内大量 `{}` 的转义噪声;`__STEPS__` / `__KW__`
/// 不会作为合法标识符出现在脚本中,替换安全。keyword 的 `{keyword}` 占位在页面侧替换。
pub fn build_human_rpa_script(steps: &[RpaStep], keyword: &str, run_id: u64) -> String {
    let steps_json = serde_json::to_string(steps).unwrap_or_else(|_| "[]".to_string());
    let kw_json = serde_json::to_string(keyword).unwrap_or_else(|_| "\"\"".to_string());

    const TEMPLATE: &str = r#"(function () {
  var STEPS = __STEPS__;
  var KW = __KW__;
  // 手动结束中断标志:HUD「结束」按钮会置 window.__veltrixAbort=true(同窗口共享),
  // 本脚本的步骤循环与滚动循环每轮检查它即时退出。开跑先复位,避免窗口复用/SPA 下残留上次的 true。
  try { window.__veltrixAbort = false; } catch (e) {}

  function rand(a, b) { return a + Math.random() * (b - a); }
  function sleep(ms) { return new Promise(function (r) { setTimeout(r, ms); }); }
  function subst(s) { return (s == null ? '' : String(s)).split('{keyword}').join(KW); }

  // 轮询等待节点出现;命中或超时(返回 null)后 resolve
  function waitFor(sel, timeout) {
    return new Promise(function (resolve) {
      var start = Date.now();
      (function poll() {
        var el = document.querySelector(sel);
        if (el) return resolve(el);
        if (Date.now() - start > timeout) return resolve(null);
        setTimeout(poll, rand(180, 360));
      })();
    });
  }

  // React 受控组件:必须用原生 value setter 再派发 input,框架才感知到输入
  function setNativeValue(el, value) {
    var proto = el.tagName === 'TEXTAREA'
      ? window.HTMLTextAreaElement.prototype : window.HTMLInputElement.prototype;
    var desc = Object.getOwnPropertyDescriptor(proto, 'value');
    if (desc && desc.set) { desc.set.call(el, value); } else { el.value = value; }
    el.dispatchEvent(new Event('input', { bubbles: true }));
  }

  async function typeHuman(el, text) {
    el.focus();
    for (var i = 0; i < text.length; i++) {
      var ch = text[i];
      el.dispatchEvent(new KeyboardEvent('keydown', { bubbles: true, key: ch }));
      setNativeValue(el, text.slice(0, i + 1));
      el.dispatchEvent(new KeyboardEvent('keyup', { bubbles: true, key: ch }));
      await sleep(rand(80, 200)); // 逐字随机节奏,模拟打字
    }
    el.dispatchEvent(new Event('change', { bubbles: true }));
  }

  async function clickHuman(el) {
    el.scrollIntoView({ block: 'center' });
    await sleep(rand(150, 400));
    var r = el.getBoundingClientRect();
    var o = { bubbles: true, cancelable: true, clientX: r.left + r.width / 2, clientY: r.top + r.height / 2 };
    // 派发 pointer + mouse 全套:小红书等 Vue 组件的点击处理常挂在 pointer 事件上,
    // 只发 mouse 合成事件触发不了搜索(表现为「关键词输入完没点击/点了没反应」)。
    try { el.dispatchEvent(new PointerEvent('pointerover', o)); } catch (e) {}
    el.dispatchEvent(new MouseEvent('mouseover', o));
    el.dispatchEvent(new MouseEvent('mousemove', o));
    await sleep(rand(120, 350)); // hover 后短暂停顿再按下
    try { el.dispatchEvent(new PointerEvent('pointerdown', o)); } catch (e) {}
    el.dispatchEvent(new MouseEvent('mousedown', o));
    try { el.dispatchEvent(new PointerEvent('pointerup', o)); } catch (e) {}
    el.dispatchEvent(new MouseEvent('mouseup', o));
    el.dispatchEvent(new MouseEvent('click', o));
  }

  function pressEnter(el) {
    el.focus();
    var ev = { bubbles: true, key: 'Enter', code: 'Enter', keyCode: 13, which: 13 };
    el.dispatchEvent(new KeyboardEvent('keydown', ev));
    el.dispatchEvent(new KeyboardEvent('keyup', ev));
  }

  // 找主滚动容器:整页 + 所有内部可滚容器里,取「内容最高」的那个(= 主内容区,
  // 避免误选某个小的内部滚动容器导致很快「到底」)。
  function findMainScroller() {
    var docEl = document.scrollingElement || document.documentElement;
    var best = docEl, bestH = docEl ? docEl.scrollHeight : 0;
    var all = document.querySelectorAll('*');
    for (var i = 0; i < all.length; i++) {
      var el = all[i];
      var st = getComputedStyle(el);
      if (/(auto|scroll)/.test(st.overflowY) && el.scrollHeight > el.clientHeight + 100) {
        if (el.scrollHeight > bestH) { bestH = el.scrollHeight; best = el; }
      }
    }
    return best;
  }

  // maxRounds 为最大轮数上限;持续滚动直到内容高度连续多轮不再增长(真·到底)才停。
  // 多管齐下触发懒加载:scrollBy + 把末尾元素滚入视口(命中 IntersectionObserver 哨兵) + 派发 scroll 事件。
  async function scrollHuman(maxRounds) {
    var scroller = findMainScroller();
    var lastHeight = 0, stagnant = 0;
    for (var i = 0; i < maxRounds; i++) {
      if (window.__veltrixAbort) break; // 手动结束:立即停止滚动翻页
      scroller.scrollBy({ top: rand(600, 1100) });
      var kids = scroller.children;
      if (kids && kids.length) {
        try { kids[kids.length - 1].scrollIntoView({ block: 'end' }); } catch (e) {}
      }
      // 兼容「监听 scroll 事件才加载」的页面
      scroller.dispatchEvent(new Event('scroll', { bubbles: true }));
      window.dispatchEvent(new Event('scroll'));
      await sleep(rand(1000, 2000)); // 等懒加载补内容

      var h = scroller.scrollHeight;
      if (h <= lastHeight + 10) {
        stagnant++;
        if (stagnant >= 6) break; // 更有耐心:连续 6 轮不涨才认为到底
        await sleep(rand(1000, 2000)); // 没涨就多等,给慢加载机会
      } else {
        stagnant = 0;
      }
      lastHeight = h;
      if (Math.random() < 0.2) { // 偶尔回滚一点,更像人
        scroller.scrollBy({ top: -rand(80, 200) });
        await sleep(rand(300, 700));
      }
    }
  }

  function done(ok, idx, msg) {
    try {
      // 失败时附带当前 URL,日志可看出卡在首页/登录页/结果页哪一步
      var detail = ok ? (msg || '') : ((msg || '') + ' @ ' + location.href);
      window.__TAURI_INTERNALS__.invoke('rpa_done', { runId: __RUNID__, ok: ok, failedStep: idx, message: detail });
    } catch (e) { console.error('[veltrix] rpa_done bridge unavailable', e); }
  }

  (async function run() {
    for (var i = 0; i < STEPS.length; i++) {
      if (window.__veltrixAbort) return done(false, i, '已手动结束'); // 手动结束:中止后续步骤
      var s = STEPS[i];
      try {
        if (s.action === 'waitFor') {
          if (!await waitFor(subst(s.selector), s.timeoutMs || 8000)) {
            return done(false, i, 'waitFor 超时: ' + s.selector);
          }
        } else if (s.action === 'click') {
          var ec = await waitFor(subst(s.selector), 5000);
          if (!ec) return done(false, i, 'click 节点缺失: ' + s.selector);
          await clickHuman(ec);
        } else if (s.action === 'type') {
          var et = await waitFor(subst(s.selector), 5000);
          if (!et) return done(false, i, 'type 节点缺失: ' + s.selector);
          await typeHuman(et, subst(s.text));
        } else if (s.action === 'pressEnter') {
          var ep = await waitFor(subst(s.selector), 5000);
          if (!ep) return done(false, i, 'pressEnter 节点缺失: ' + s.selector);
          pressEnter(ep);
        } else if (s.action === 'scroll') {
          await scrollHuman(s.segments || 4);
        } else if (s.action === 'pause') {
          await sleep(rand(s.minMs || 300, s.maxMs || 800));
        }
        await sleep(rand(200, 600)); // 步骤间自然间隔
      } catch (e) {
        return done(false, i, String(e));
      }
    }
    done(true, -1, '');
  })();
})();"#;

    TEMPLATE
        .replace("__STEPS__", &steps_json)
        .replace("__KW__", &kw_json)
        .replace("__RUNID__", &run_id.to_string())
}

// ---- 采集日志:窗口内 HUD 浮层 + 前端事件 ----

/// 前端监听的采集日志事件名;TaskDetailPage 据此订阅并按 task_id 过滤展示。
pub const COLLECT_LOG_EVENT: &str = "collect-log";

/// 采集条目富信息(内容/评论)。前端日志面板据此渲染头像 + 昵称 + 标题 + 序号 + 类型。
/// HUD 浮层为纯文本,不消费本字段(只显示 message)。
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CollectEntry {
    /// 条目类型:"content"(视频/图文)| "comment"(评论)。
    pub kind: String,
    /// 任务内序号(从 1 递增)。
    pub seq: i64,
    /// 作者头像 URL。
    pub avatar: Option<String>,
    pub nickname: String,
    /// 内容标题/正文 或 评论文本(已截断)。
    pub title: String,
    /// 内容形态 video / image;评论为 None。
    pub content_kind: Option<String>,
}

/// 一条采集日志。同一条既经 `app.emit` 推给前端面板,也经窗口 HUD 实时展示。
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CollectLog {
    pub task_id: String,
    /// 产生时间(Unix 秒)。
    pub ts: i64,
    /// 级别:info / warn / error,前端与 HUD 按级别着色。
    pub level: String,
    pub message: String,
    /// 采集条目富信息(内容/评论);普通日志为 None。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entry: Option<CollectEntry>,
}

/// 采集日志落库通道。lib.rs setup 初始化后,emit 时把日志副本发到此处由后台 writer 落库。
static LOG_SINK: OnceLock<Sender<CollectLog>> = OnceLock::new();

/// 初始化日志落库通道(进程启动时调用一次)。改用有界通道,
/// 采集高峰期发送阻塞提供背压,防止无界通道内存无限增长。
pub fn init_log_sink(sender: Sender<CollectLog>) {
    let _ = LOG_SINK.set(sender);
}

/// 把日志副本送入落库通道;通道未初始化 / 已满 / 已关闭时静默忽略,不影响采集。
fn persist_log(log: &CollectLog) {
    if let Some(sink) = LOG_SINK.get() {
        // try_send 非阻塞:通道满时丢弃日志(优先保障采集吞吐,不因日志 IO 阻塞采集)
        let _ = sink.try_send(log.clone());
    }
}

/// 向前端推送一条采集日志并落库持久化。emit 失败仅忽略(无前端监听时不应影响采集)。
pub fn emit_collect_log(app: &AppHandle, task_id: &str, level: &str, message: impl Into<String>) {
    let log = CollectLog {
        task_id: task_id.to_string(),
        ts: chrono::Utc::now().timestamp(),
        level: level.to_string(),
        message: message.into(),
        entry: None,
    };
    persist_log(&log);
    let _ = app.emit(COLLECT_LOG_EVENT, log);
}

/// 给指定采集窗口的 HUD 追加一条日志;窗口已关 / 不存在则静默忽略。
/// 供 commands 在 pool collect 返回后(入库完成等)向 HUD 补充提示。
/// task_id 非空时按任务级 label 定位(采集窗口按「平台+账号+任务」唯一);
/// 素材下载阶段窗口已主动关闭,该阶段的 HUD 日志找不到窗口静默丢弃属预期(仍经 emit_collect_log 落库推前端)。
pub fn hud_log(app: &AppHandle, platform: &str, account_id: &str, task_id: Option<&str>, level: &str, message: &str) {
    use tauri::Manager;
    if let Some(win) = app.get_webview_window(&pool::task_window_label(platform, account_id, task_id)) {
        let _ = win.eval(build_hud_log_eval(level, message));
    }
}

/// 推送一条「采集条目」富日志(内容/评论),供前端日志面板渲染头像 + 昵称 + 标题 + 序号。
/// message 仍填一句纯文本兜底(HUD 与不支持富渲染处显示)。
pub fn emit_collect_entry(
    app: &AppHandle,
    task_id: &str,
    message: impl Into<String>,
    entry: CollectEntry,
) {
    let log = CollectLog {
        task_id: task_id.to_string(),
        ts: chrono::Utc::now().timestamp(),
        level: "info".to_string(),
        message: message.into(),
        entry: Some(entry),
    };
    persist_log(&log);
    let _ = app.emit(COLLECT_LOG_EVENT, log);
}

/// 构造「更新 HUD 一条日志」的 eval 脚本。时间由页面侧生成,避免跨端时钟差。
pub fn build_hud_log_eval(level: &str, message: &str) -> String {
    let payload = serde_json::json!({ "level": level, "message": message });
    format!("window.__veltrixHud&&window.__veltrixHud.log({payload});")
}

/// 构造「更新 HUD 状态条」的 eval 脚本。running 控制状态点颜色/呼吸;
/// 收起态视觉由 JS 按 running 推断(true=运行中绿 / false=已停止灰)。
pub fn build_hud_status_eval(text: &str, running: bool) -> String {
    let text_json = serde_json::to_string(text).unwrap_or_else(|_| "\"\"".to_string());
    format!("window.__veltrixHud&&window.__veltrixHud.status({text_json},{running});")
}

/// 同 build_hud_status_eval,但显式指定收起态视觉状态(覆盖按 running 的推断):
/// `state` ∈ "running"(运行中·绿)/ "error"(异常或需处理·红)/ "stopped"(已停止·灰)。
/// 用于「异常结束」「等待安全验证」等需要在最小化图标上明确区分的场景。
pub fn build_hud_status_eval_state(text: &str, running: bool, state: &str) -> String {
    let text_json = serde_json::to_string(text).unwrap_or_else(|_| "\"\"".to_string());
    let state_json = serde_json::to_string(state).unwrap_or_else(|_| "\"\"".to_string());
    format!("window.__veltrixHud&&window.__veltrixHud.status({text_json},{running},{state_json});")
}

/// 构造「切到关键字 tab」的 eval 脚本。每轮采集前调用,使后续日志按关键字分组到独立 tab。
pub fn build_hud_keyword_eval(keyword: &str) -> String {
    let kw_json = serde_json::to_string(keyword).unwrap_or_else(|_| "\"\"".to_string());
    format!("window.__veltrixHud&&window.__veltrixHud.beginKeyword({kw_json});")
}

/// 构造「绑定当前采集会话 id」的 eval 脚本,供 HUD「结束」按钮回传以停止本次采集。
pub fn build_hud_session_eval(session_id: u64) -> String {
    format!("window.__veltrixHud&&window.__veltrixHud.bindSession({session_id});")
}

/// 构造「绑定当前任务 id」的 eval 脚本。task_id 跨关键词稳定,「结束」按钮按它停止整任务,
/// 避免会话(session_id)随关键词刷新时,在关键词空档点结束落到旧会话上而漏判。
pub fn build_hud_task_eval(task_id: &str) -> String {
    let tid_json = serde_json::to_string(task_id).unwrap_or_else(|_| "\"\"".to_string());
    format!("window.__veltrixHud&&window.__veltrixHud.bindTask({tid_json});")
}

/// 构造注入采集窗口的 HUD 浮层脚本(作为 `initialization_script`)。
///
/// 每次文档加载自动重建浮层,并从 `sessionStorage` 恢复历史日志,
/// 因此 legacy 路径的整页导航不会清空 HUD。脚本对页面只读、`pointer-events:none`,
/// 不干扰平台页面自身的交互与采集 hook。
pub fn build_hud_init_script() -> String {
    r#"(function () {
  if (window.__veltrixHudReady) return;
  window.__veltrixHudReady = true;
  var KEY = '__veltrix_hud_logs';
  var POS_KEY = '__veltrix_hud_pos';
  var COLLAPSE_KEY = '__veltrix_hud_collapsed';
  var CUR_KEY = '__veltrix_hud_cur';
  var TAB_KEY = '__veltrix_hud_tab';
  var RUN_KEY = '__veltrix_hud_running';
  var STATE_KEY = '__veltrix_hud_state';
  var DEFAULT_KW = '日志';
  var SID_KEY = '__veltrix_hud_sid';
  var TID_KEY = '__veltrix_hud_tid';
  var SEQ_KEY = '__veltrix_hud_seq';
  // 状态色:绿=正常运行中 / 红=异常或需处理 / 灰=已停止
  var COLOR_OK = '#22c55e', COLOR_ERR = '#ef4444', COLOR_IDLE = '#9ca3af';
  // 收起态三态视觉(颜色 + 图标 + 悬浮文案):最小化后一眼区分「运行中 / 异常 / 已停止」
  var STATE_META = {
    running: { color: COLOR_OK, glow: true, label: '正在采集',
      svg: '<svg width="26" height="26" viewBox="0 0 24 24" fill="none" stroke="white" stroke-width="2.4" stroke-linecap="round" stroke-linejoin="round"><path d="M3 12h4l3 7 4-14 3 7h4"/></svg>' },
    error: { color: COLOR_ERR, glow: true, label: '采集异常 / 需处理',
      svg: '<svg width="26" height="26" viewBox="0 0 24 24" fill="none" stroke="white" stroke-width="2.4" stroke-linecap="round" stroke-linejoin="round"><path d="M10.3 3.9 1.8 18a2 2 0 0 0 1.7 3h17a2 2 0 0 0 1.7-3L13.7 3.9a2 2 0 0 0-3.4 0z"/><path d="M12 9v4"/><path d="M12 17h.01"/></svg>' },
    stopped: { color: COLOR_IDLE, glow: false, label: '已停止 / 空闲',
      svg: '<svg width="22" height="22" viewBox="0 0 24 24" fill="white" stroke="none"><rect x="6" y="6" width="12" height="12" rx="2"/></svg>' }
  };
  // 记住最近一次状态色,收起时据此画发光环
  var lastColor = COLOR_IDLE, lastGlow = false;

  // 日志统一一个流,不再按关键字分 tab(用户要求):keyword 字段仍随日志保留,但不再参与分组
  var currentKeyword = '';
  var activeTab = DEFAULT_KW;

  function getLogs() {
    try { return JSON.parse(sessionStorage.getItem(KEY) || '[]'); } catch (e) { return []; }
  }
  // 按出现顺序提取去重关键字列表,作为 tab 顺序
  function keywordsOf(logs) {
    var seen = {}, list = [];
    for (var i = 0; i < logs.length; i++) {
      var k = logs[i].keyword || DEFAULT_KW;
      if (!seen[k]) { seen[k] = 1; list.push(k); }
    }
    return list;
  }

  function ensureRoot() {
    if (!document.body) return null;
    var root = document.getElementById('veltrix-hud');
    if (root) return root;
    root = document.createElement('div');
    root.id = 'veltrix-hud';
    root.style.cssText = 'position:fixed;right:12px;bottom:12px;width:50vw;z-index:2147483647;height:33vh;background:rgba(17,24,39,.95);color:#e5e7eb;font:12px/1.55 system-ui,-apple-system,sans-serif;border:1px solid rgba(255,255,255,.14);border-radius:10px;box-shadow:0 8px 28px rgba(0,0,0,.5);overflow:hidden;display:flex;flex-direction:column;pointer-events:auto;';
    var head = document.createElement('div');
    head.id = 'veltrix-hud-head';
    head.style.cssText = 'padding:8px 11px;font-weight:600;background:rgba(255,255,255,.06);display:flex;align-items:center;gap:7px;flex:0 0 auto;cursor:default;user-select:none;';
    var dot = document.createElement('span');
    dot.id = 'veltrix-hud-dot';
    dot.style.cssText = 'width:8px;height:8px;border-radius:50%;background:#9ca3af;flex:0 0 auto;';
    var title = document.createElement('span');
    title.textContent = 'HUD日志';
    title.style.cssText = 'flex:0 0 auto;font-weight:600;';
    var status = document.createElement('span');
    status.id = 'veltrix-hud-status';
    status.style.cssText = 'flex:1 1 auto;min-width:0;overflow:hidden;text-overflow:ellipsis;white-space:nowrap;font-weight:400;font-size:11px;color:#9ca3af;';
    head.appendChild(dot); head.appendChild(title); head.appendChild(status);

    var toggleBtn = document.createElement('span');
    toggleBtn.id = 'veltrix-hud-toggle';
    toggleBtn.setAttribute('data-hud-btn', '1');
    toggleBtn.textContent = '收起';
    toggleBtn.style.cssText = 'cursor:pointer;font-weight:400;font-size:11px;padding:1px 7px;border:1px solid rgba(255,255,255,.18);border-radius:5px;color:#cbd5e1;flex:0 0 auto;';
    toggleBtn.addEventListener('click', function (e) {
      e.stopPropagation();
      setCollapsed(true);
    });

    var copyBtn = document.createElement('span');
    copyBtn.setAttribute('data-hud-btn', '1');
    copyBtn.textContent = '复制';
    copyBtn.style.cssText = 'cursor:pointer;font-weight:400;font-size:11px;padding:1px 7px;border:1px solid rgba(255,255,255,.18);border-radius:5px;color:#cbd5e1;flex:0 0 auto;';
    copyBtn.addEventListener('click', function (e) {
      e.stopPropagation();
      // 统一单流:复制全部日志
      var logs = getLogs();
      var text = logs.map(function (it) { return (it.time || '') + '  ' + (it.message || ''); }).join('\n');
      if (navigator.clipboard && navigator.clipboard.writeText) {
        navigator.clipboard.writeText(text).then(function () {
          copyBtn.textContent = '已复制';
          setTimeout(function () { copyBtn.textContent = '复制'; }, 1200);
        }).catch(function () {});
      }
    });
    // 手动结束:仅采集中显示;点击通知后端优雅停止本次采集(保留已采内容,正常完成)
    var stopBtn = document.createElement('span');
    stopBtn.id = 'veltrix-hud-stop';
    stopBtn.setAttribute('data-hud-btn', '1');
    stopBtn.textContent = '结束';
    stopBtn.title = '手动结束本次采集(保留已采内容)';
    stopBtn.style.cssText = 'display:none;cursor:pointer;font-weight:400;font-size:11px;padding:1px 7px;border:1px solid rgba(239,68,68,.5);border-radius:5px;color:#fca5a5;flex:0 0 auto;';
    stopBtn.addEventListener('click', function (e) {
      e.stopPropagation();
      // 立即中断页面内 RPA 滚动(同窗口共享标志),不等 Rust 往返;Rust 停止信号另经 stop_collect 下发
      try { window.__veltrixAbort = true; } catch (err) {}
      var sid = null, tid = null;
      try { sid = sessionStorage.getItem(SID_KEY); } catch (err) {}
      try { tid = sessionStorage.getItem(TID_KEY); } catch (err) {}
      // 会话或任务任一可用即可停止;任务采集走 task_id(跨关键词稳定),联调单采无 task 走 session
      if ((sid === null || sid === '') && (tid === null || tid === '')) return;
      try {
        var payload = {};
        if (sid !== null && sid !== '') payload.sessionId = Number(sid);
        if (tid !== null && tid !== '') payload.taskId = tid;
        // 优先走 WebView 原生消息桥(远程页面 invoke 会被 ACL 拒)
        if (!(window.__veltrixSignal && window.__veltrixSignal('stop', payload))) {
          window.__TAURI_INTERNALS__.invoke('stop_collect', payload);
        }
      } catch (err) { console.error('[veltrix] stop_collect 调用失败', err); }
      stopBtn.textContent = '结束中…';
      stopBtn.style.pointerEvents = 'none';
    });
    head.appendChild(stopBtn); head.appendChild(toggleBtn); head.appendChild(copyBtn);

    // 多关键字时显示的 tab 条;单关键字隐藏
    var tabs = document.createElement('div');
    tabs.id = 'veltrix-hud-tabs';
    tabs.style.cssText = 'display:none;gap:4px;padding:6px 9px 0;overflow-x:auto;flex:0 0 auto;';

    var body = document.createElement('div');
    body.id = 'veltrix-hud-logs';
    body.style.cssText = 'padding:6px 11px 8px;overflow-y:auto;flex:1 1 auto;user-select:text;cursor:text;';

    // 收起态:整个浮层缩成一个图标,点击展开;图标颜色随采集状态(绿=正常/红=问题/灰=空闲)
    var icon = document.createElement('div');
    icon.id = 'veltrix-hud-icon';
    icon.title = '展开 HUD 日志';
    // 收起态整块填充状态色 + 白色波形图标,深色页面上也足够醒目
    icon.style.cssText = 'display:none;width:100%;height:100%;align-items:center;justify-content:center;cursor:pointer;background:#9ca3af;';
    icon.innerHTML = '<svg width="26" height="26" viewBox="0 0 24 24" fill="none" stroke="white" stroke-width="2.4" stroke-linecap="round" stroke-linejoin="round"><path d="M3 12h4l3 7 4-14 3 7h4"/></svg>';

    root.appendChild(head); root.appendChild(tabs); root.appendChild(body); root.appendChild(icon);
    document.body.appendChild(root);

    // HUD 为右下角浮动面板(宽度 1/2、高 1/3),默认展开,不恢复拖动位置

    // 拖动:按住标题栏或收起图标移动浮层(按钮除外),松手把位置存入 sessionStorage。
    // dragMoved 供图标的 click 判断:刚拖动过的那次点击不应触发展开。
    var dragMoved = false;
    (function () {
      var dragging = false, sx = 0, sy = 0, ox = 0, oy = 0;
      function onDown(e) {
        if (e.target.closest && e.target.closest('[data-hud-btn]')) return;
        var rect = root.getBoundingClientRect();
        root.style.left = rect.left + 'px';
        root.style.top = rect.top + 'px';
        root.style.right = 'auto';
        dragging = true; dragMoved = false; sx = e.clientX; sy = e.clientY; ox = rect.left; oy = rect.top;
        e.preventDefault();
      }
      // HUD 固定底部栏,禁用拖动(仅 icon 保留点击展开,不再绑 mousedown 拖动)
      document.addEventListener('mousemove', function (e) {
        if (!dragging) return;
        dragMoved = true;
        var nx = ox + (e.clientX - sx), ny = oy + (e.clientY - sy);
        nx = Math.max(0, Math.min(nx, window.innerWidth - root.offsetWidth));
        ny = Math.max(0, Math.min(ny, window.innerHeight - root.offsetHeight));
        root.style.left = nx + 'px';
        root.style.top = ny + 'px';
      });
      document.addEventListener('mouseup', function () {
        if (!dragging) return;
        dragging = false;
        try {
          sessionStorage.setItem(POS_KEY, JSON.stringify({ left: parseInt(root.style.left, 10), top: parseInt(root.style.top, 10) }));
        } catch (e) {}
      });
    })();

    // 点击收起图标展开(拖动结束的那次点击不触发)
    icon.addEventListener('click', function () {
      if (dragMoved) { dragMoved = false; return; }
      setCollapsed(false);
    });

    renderTabs();
    renderBody();
    applyCollapsed(isCollapsed());
    setHudState(currentState());
    updateStopBtn();
    applyCaptchaAvoid(); // 验证中心整页重注入时,若验证码已渲染则立刻避让,避免一帧闪现
    return root;
  }

  function isCollapsed() {
    // 默认展开;用户手动收起后(置 '1')本会话保持收起
    try { return sessionStorage.getItem(COLLAPSE_KEY) === '1'; } catch (e) { return false; }
  }
  function applyCollapsed(collapsed) {
    var root = document.getElementById('veltrix-hud');
    if (!root) return;
    var head = document.getElementById('veltrix-hud-head');
    var tabs = document.getElementById('veltrix-hud-tabs');
    var body = document.getElementById('veltrix-hud-logs');
    var icon = document.getElementById('veltrix-hud-icon');
    if (collapsed) {
      // 收起:藏掉标题栏 / tab / 日志,整体缩成方形图标
      if (head) head.style.display = 'none';
      if (tabs) tabs.style.display = 'none';
      if (body) body.style.display = 'none';
      if (icon) {
        icon.style.display = 'flex';
        icon.style.background = lastColor; // 收起即用当前状态色,绿/红/灰一眼可辨
      }
      root.style.left = 'auto';
      root.style.right = '12px';
      root.style.top = 'auto';
      root.style.bottom = '12px';
      root.style.width = '46px';
      root.style.height = '46px';
      root.style.maxHeight = '46px';
      root.style.borderTop = 'none';
      root.style.border = 'none'; // 收起态不要边框线,整块纯色更干净
      root.style.boxShadow = (lastGlow ? '0 0 14px ' + lastColor + ',' : '') + '0 4px 16px rgba(0,0,0,.5)';
    } else {
      if (head) head.style.display = 'flex';
      if (icon) icon.style.display = 'none';
      if (body) body.style.display = '';
      if (tabs) tabs.style.display = 'none'; // 统一单流:tab 条永不显示
      // 展开:右下角浮动面板,宽度为窗口的一半、高 1/3,带圆角与四边边框
      root.style.left = 'auto';
      root.style.right = '12px';
      root.style.top = 'auto';
      root.style.bottom = '12px';
      root.style.width = '50vw';
      root.style.height = '33vh';
      root.style.maxHeight = '';
      root.style.border = '1px solid rgba(255,255,255,.14)';
      root.style.borderTop = '1px solid rgba(255,255,255,.14)';
      root.style.borderRadius = '10px';
      root.style.boxShadow = '0 8px 28px rgba(0,0,0,.5)';
    }
  }
  function setCollapsed(collapsed) {
    try { sessionStorage.setItem(COLLAPSE_KEY, collapsed ? '1' : '0'); } catch (e) {}
    applyCollapsed(collapsed);
  }

  // 统一单流后不再渲染关键字 tab 条(保留函数壳:beginKeyword 仍会调用)
  function renderTabs() {
    var tabs = document.getElementById('veltrix-hud-tabs');
    if (tabs) tabs.style.display = 'none';
  }

  function renderBody() {
    var body = document.getElementById('veltrix-hud-logs');
    if (!body) return;
    body.innerHTML = '';
    // 统一单流:不过滤关键字,全部日志按时间顺序一个列表
    var logs = getLogs();
    for (var i = 0; i < logs.length; i++) appendLine(logs[i]);
    body.scrollTop = body.scrollHeight;
  }

  function appendLine(item) {
    var body = document.getElementById('veltrix-hud-logs');
    if (!body) return;
    var line = document.createElement('div');
    var color = item.level === 'error' ? '#f87171' : (item.level === 'warn' ? '#fbbf24' : '#9ca3af');
    line.style.cssText = 'white-space:nowrap;overflow:hidden;text-overflow:ellipsis;color:' + color + ';';
    line.textContent = (item.seq ? '#' + item.seq + ' ' : '') + (item.time || '') + '  ' + (item.message || '');
    body.appendChild(line);
    body.scrollTop = body.scrollHeight;
  }

  // 是否处于采集中(status running 落 sessionStorage,跨导航恢复)
  function isRunning() {
    try { return sessionStorage.getItem(RUN_KEY) === '1'; } catch (e) { return false; }
  }
  // 结束按钮仅采集中可见
  function updateStopBtn() {
    var b = document.getElementById('veltrix-hud-stop');
    if (b) b.style.display = isRunning() ? 'inline-block' : 'none';
  }
  // 读取当前持久化的三态(默认 stopped);非法值回落 stopped
  function currentState() {
    var v = null;
    try { v = sessionStorage.getItem(STATE_KEY); } catch (e) {}
    return STATE_META[v] ? v : 'stopped';
  }
  // 统一设置三态(running/error/stopped):标题栏状态点 + 收起图标(颜色 + 图标 + 悬浮文案)+ 发光环
  function setHudState(state) {
    if (!STATE_META[state]) state = 'stopped';
    try { sessionStorage.setItem(STATE_KEY, state); } catch (e) {}
    var m = STATE_META[state];
    lastColor = m.color; lastGlow = m.glow;
    var d = document.getElementById('veltrix-hud-dot');
    if (d) {
      d.style.background = m.color;
      d.style.boxShadow = m.glow ? '0 0 6px ' + m.color : 'none';
    }
    // 收起态:整块填色 + 对应图标(波形=运行 / 警告三角=异常 / 方块=停止)+ 悬浮文案
    var icon = document.getElementById('veltrix-hud-icon');
    if (icon) {
      icon.style.background = m.color;
      icon.innerHTML = m.svg;
      icon.title = m.label + '(点击展开 HUD 日志)';
    }
    // 收起时整块外发光,远比细图标醒目
    var root = document.getElementById('veltrix-hud');
    if (root && isCollapsed()) {
      root.style.boxShadow = (m.glow ? '0 0 14px ' + m.color + ',' : '') + '0 4px 16px rgba(0,0,0,.5)';
    }
  }

  // ── 风控验证码自动避让 ───────────────────────────────────────────
  // 抖音/字节 secsdk 验证码(滑块/盾牌/点选)弹出时,底部 HUD 栏会盖住验证弹窗下半部分,
  // 妨碍手动验证。后端的 wait_verify_cleared 也会隐藏 HUD,但它依赖响应/DOM 自检命中后才触发,
  // 弹窗刚渲染的空窗期盖不住(见用户实测:第 1 次翻页等待中弹窗已出 HUD 仍在)。这里在页面侧直接
  // 按验证码 DOM 是否在屏可见来避让,弹窗一出即隐藏、消失即恢复,不依赖后端时序。
  // 选择器取各平台 verify_selectors 的并集(抖音/TikTok/小红书/快手),确保是后端检测的超集,
  // 整页「验证中心」与页内 overlay 两种形态都能识别。
  var CAPTCHA_SELECTORS = [
    '#captcha_container', '#vc_captcha_box', '.vc-captcha-verify',
    '.captcha_verify_container', '#captcha-verify-image', '.captcha-verify-container',
    '.captcha-container', '.red-captcha',          // 小红书
    '.captcha-dialog', '.slide-verify'             // 快手
  ];
  // 本脚本经 initialization_script 注入,在「所有帧」运行(含跨域验证码 iframe)。
  // 验证码常在跨域 iframe 里:该帧能看到弹窗 DOM 却无法 invoke,顶层帧又跨域抓不到 iframe DOM。
  // 故由本脚本做跨帧桥:子帧看到验证码 → postMessage 给顶层;顶层把时间戳写到 window.__veltrixChildVerifyTs,
  // 供「HUD 避让」与「verify 自检(report_collect_verify)」共用,从而正确暂停采集 + 隐藏全宽 HUD。
  var CAPTCHA_MSG = 'veltrix-verify';
  var CAPTCHA_TTL = 4000; // 子帧心跳超时(ms)
  var hudIsTop = false;
  try { hudIsTop = (window.top === window.self); } catch (e) { hudIsTop = false; }
  if (hudIsTop) {
    try {
      window.addEventListener('message', function (ev) {
        var d = ev && ev.data;
        if (d && d.__veltrix === CAPTCHA_MSG) {
          try { window.__veltrixChildVerifyTs = Date.now(); } catch (e) {}
        }
      }, false);
    } catch (e) {}
  }
  // 本帧选择器命中(不含跨帧兜底)
  function localCaptchaHit() {
    for (var i = 0; i < CAPTCHA_SELECTORS.length; i++) {
      var el;
      try { el = document.querySelector(CAPTCHA_SELECTORS[i]); } catch (e) { continue; }
      if (!el) continue;
      // 0 尺寸 = display:none / 尚未渲染 / 已解除后残留,均视为不在屏
      var r = el.getBoundingClientRect();
      if (r.width > 0 && r.height > 0) return true;
    }
    return false;
  }
  function isCaptchaVisible() {
    if (localCaptchaHit()) return true;
    // 跨域子帧的验证码本帧抓不到 → 顶层靠子帧心跳兜底
    try { if (hudIsTop && (Date.now() - (window.__veltrixChildVerifyTs || 0)) < CAPTCHA_TTL) return true; } catch (e) {}
    return false;
  }
  // 仅托管「由本逻辑隐藏」的 HUD:验证码在屏→隐藏并记账;离屏后只恢复自己藏的那次。
  // 这样后端因 verify_texts/url 命中(本侧选择器可能漏)而隐藏的 HUD 不会被错误抢回显示。
  // 恢复用 flex(与初始 cssText 的 display:flex 一致),避免空串回落成 block 丢失弹性布局。
  var hudHiddenByCaptcha = false;
  function applyCaptchaAvoid() {
    // 子帧:本帧看到验证码就向顶层发心跳(顶层据此暂停采集 + 隐藏全宽 HUD)
    if (!hudIsTop && localCaptchaHit()) {
      try { window.top.postMessage({ __veltrix: CAPTCHA_MSG, present: true }, '*'); } catch (e) {}
    }
    var root = document.getElementById('veltrix-hud');
    if (!root) return;
    if (isCaptchaVisible()) {
      if (root.style.display !== 'none') { root.style.display = 'none'; hudHiddenByCaptcha = true; }
    } else if (hudHiddenByCaptcha) {
      hudHiddenByCaptcha = false;
      if (root.style.display === 'none') root.style.display = 'flex';
    }
  }
  function startCaptchaAvoid() {
    if (window.__veltrixCaptchaAvoid) return;
    window.__veltrixCaptchaAvoid = true;
    // 轮询而非 MutationObserver:抖音滚动期 DOM 高频变更,逐次 getBoundingClientRect 触发的
    // 强制重排代价高;400ms 定时检测开销可忽略,验证码显隐延迟也在可接受范围。
    setInterval(applyCaptchaAvoid, 400);
    applyCaptchaAvoid();
  }

  window.__veltrixHud = {
    // 后端每轮采集前调用。统一单流后不再按关键字切 tab:此处仅刷新显示
    beginKeyword: function (kw) {
      ensureRoot();
      renderBody();
      applyCollapsed(isCollapsed());
    },
    log: function (item) {
      item = item || {};
      // 序号:单调递增计数器存 sessionStorage,整页导航后延续(与日志列表同生命周期),
      // 便于在 HUD 上按序核对「打开 → 成功/未果 → 重试结果」的逐条链路
      var seq = 0;
      try {
        seq = (parseInt(sessionStorage.getItem(SEQ_KEY) || '0', 10) || 0) + 1;
        sessionStorage.setItem(SEQ_KEY, String(seq));
      } catch (e) {}
      item.seq = seq;
      // 完整年月日时分秒:采集常跨零点 / 长时间运行,单时分秒看不出是哪一天
      var dt = new Date();
      var p2 = function (n) { return (n < 10 ? '0' : '') + n; };
      item.time = dt.getFullYear() + '-' + p2(dt.getMonth() + 1) + '-' + p2(dt.getDate()) + ' ' + p2(dt.getHours()) + ':' + p2(dt.getMinutes()) + ':' + p2(dt.getSeconds());
      item.keyword = item.keyword || currentKeyword || DEFAULT_KW;
      ensureRoot();
      try {
        var saved = getLogs();
        saved.push(item);
        if (saved.length > 400) saved = saved.slice(-400);
        sessionStorage.setItem(KEY, JSON.stringify(saved));
      } catch (e) {}
      appendLine(item);
      // 收起态三态(运行/异常/停止)由后端 status() 显式驱动,单条日志不再改写状态色,
      // 避免一条 warn 把「正常运行中」误闪成异常、又被下一条 info 抹掉,导致状态不可信。
    },
    status: function (text, running, state) {
      ensureRoot();
      var s = document.getElementById('veltrix-hud-status');
      if (s && text) s.textContent = text;
      try { sessionStorage.setItem(RUN_KEY, running ? '1' : '0'); } catch (e) {}
      // state 未显式传入时按 running 推断:运行中=running / 已结束=stopped;异常由后端显式传 'error'
      setHudState(state || (running ? 'running' : 'stopped'));
      updateStopBtn();
    },
    // 绑定当前采集会话 id(供「结束」按钮回传);整页导航会丢 window 变量,故存 sessionStorage
    bindSession: function (sid) {
      try { sessionStorage.setItem(SID_KEY, String(sid)); } catch (e) {}
      ensureRoot();
      var b = document.getElementById('veltrix-hud-stop');
      if (b) { b.textContent = '结束'; b.style.pointerEvents = ''; }
      updateStopBtn();
    },
    // 绑定当前任务 id(供「结束」按钮按任务停止);跨关键词稳定,整页导航后从 sessionStorage 恢复
    bindTask: function (tid) {
      try { sessionStorage.setItem(TID_KEY, String(tid)); } catch (e) {}
    }
  };

  if (document.readyState === 'loading') {
    document.addEventListener('DOMContentLoaded', ensureRoot);
  } else {
    ensureRoot();
  }
  startCaptchaAvoid();
})();"#
        .to_string()
}
