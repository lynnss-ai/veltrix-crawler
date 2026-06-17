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

pub mod native_intercept;
pub mod pool;

use veltrix_core::config::RpaStep;
use veltrix_core::error::{CrawlerError, Result};
use serde::Serialize;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use tauri::{AppHandle, Emitter};
use tokio::sync::mpsc::UnboundedSender;
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

/// 采集中断控制:HUD「结束」按钮经 `stop_collect` 命令登记 session_id,
/// 采集循环每轮检查到即**优雅停止**(保留已采内容、作为正常完成),而非报错中断。
#[derive(Default)]
pub struct CollectControl {
    /// 被请求停止的 session_id 集合。
    stopping: Mutex<std::collections::HashSet<u64>>,
    /// 当前检测到安全验证弹窗的 session_id 集合(采集窗口自检脚本经 `report_collect_verify` 写入)。
    /// 采集循环每轮检查到即暂停滚动,等弹窗消失(用户手动完成)再恢复。
    verifying: Mutex<std::collections::HashSet<u64>>,
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
  function report(url, body) {{
    if (window.__veltrixSeen.length < 300) window.__veltrixSeen.push(url);
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

  function visible(el) {
    if (!el || el.offsetParent === null) return false;
    var r = el.getBoundingClientRect();
    return r.width > 0 && r.height > 0;
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
  function present() { return bySelector() || byText() || byLocation(); }

  function tick() {
    var p = present();
    if (p !== last) {
      last = p;
      try {
        window.__TAURI_INTERNALS__.invoke('report_collect_verify', {
          sessionId: window.__veltrixVerifySession, present: p
        });
      } catch (e) {}
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
    format!(
        "(function () {{ var id = encodeURIComponent('{id_esc}'); \
         var token = encodeURIComponent('{token_esc}'); \
         window.location.assign('{tpl}'.replace('{{id}}', id).replace('{{token}}', token)); }})();"
    )
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
    var t = (nodes[i].textContent || '').trim();
    for (var j = 0; j < LABELS.length; j++) {
      if (t === LABELS[j]) {
        try {
          nodes[i].scrollIntoView({ block: 'center' });
          var r = nodes[i].getBoundingClientRect();
          var o = { bubbles: true, clientX: r.left + r.width / 2, clientY: r.top + r.height / 2 };
          nodes[i].dispatchEvent(new MouseEvent('mousedown', o));
          nodes[i].dispatchEvent(new MouseEvent('mouseup', o));
          nodes[i].dispatchEvent(new MouseEvent('click', o));
        } catch (e) {}
        return;
      }
    }
  }
})();"#;
    TEMPLATE.replace("__LABELS__", &labels_json)
}

// ===================== 浏览器 Agent 动作脚本(MVP:只发动作,不读结果) =====================
//
// 为何与采集的 build_*_eval 分开:浏览器 Agent 用独立 "agent" 窗口、不绑登录态/不注入采集 HUD;
// 且 Tauri eval 是 fire-and-forget(取不到返回值),MVP 阶段只「发出动作」不回读 DOM——
// 取 DOM / 截图需仿 RpaChannel 新建回传 channel(留待后续)。

/// 构造「导航到指定 URL」脚本。url 经页面侧赋值给 location;只接受 http/https 由 Rust 侧先校验。
pub fn build_navigate_eval(url: &str) -> String {
    // 用 serde_json 生成安全的 JS 字符串字面量(完整转义换行/回车/引号等);
    // 手工 replace 会漏掉换行 → 单引号字符串跨行 SyntaxError 使整段 eval 失效。
    let url_lit = serde_json::to_string(url).unwrap_or_else(|_| "\"\"".to_string());
    format!("(function () {{ window.location.assign({url_lit}); }})();")
}

/// 构造「按 CSS 选择器点击元素」脚本。命中第一个匹配元素并派发鼠标事件(mousedown/up/click);
/// 找不到则空操作(MVP 不回读结果,失败不抛回 Rust)。
pub fn build_click_eval(selector: &str) -> String {
    let sel_json = serde_json::to_string(selector).unwrap_or_else(|_| "\"\"".to_string());
    const TEMPLATE: &str = r#"(function () {
  var SEL = __SEL__;
  if (!SEL) return;
  var el;
  try { el = document.querySelector(SEL); } catch (e) { return; }
  if (!el) return;
  try {
    el.scrollIntoView({ block: 'center' });
    var r = el.getBoundingClientRect();
    var o = { bubbles: true, clientX: r.left + r.width / 2, clientY: r.top + r.height / 2 };
    el.dispatchEvent(new MouseEvent('mousedown', o));
    el.dispatchEvent(new MouseEvent('mouseup', o));
    el.dispatchEvent(new MouseEvent('click', o));
  } catch (e) {}
})();"#;
    TEMPLATE.replace("__SEL__", &sel_json)
}

/// 构造「向输入框写入文本」脚本。命中选择器对应的 input/textarea/contenteditable,聚焦后整体赋值
/// 并派发 input/change 事件(触发框架的受控更新);找不到则空操作。MVP 不逐字模拟,只整体写入。
pub fn build_type_eval(selector: &str, text: &str) -> String {
    let sel_json = serde_json::to_string(selector).unwrap_or_else(|_| "\"\"".to_string());
    let text_json = serde_json::to_string(text).unwrap_or_else(|_| "\"\"".to_string());
    const TEMPLATE: &str = r#"(function () {
  var SEL = __SEL__;
  var TEXT = __TEXT__;
  if (!SEL) return;
  var el;
  try { el = document.querySelector(SEL); } catch (e) { return; }
  if (!el) return;
  try {
    el.focus();
    if (el.isContentEditable) {
      el.textContent = TEXT;
    } else {
      el.value = TEXT;
    }
    el.dispatchEvent(new Event('input', { bubbles: true }));
    el.dispatchEvent(new Event('change', { bubbles: true }));
  } catch (e) {}
})();"#;
    TEMPLATE.replace("__SEL__", &sel_json).replace("__TEXT__", &text_json)
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
    var o = { bubbles: true, clientX: r.left + r.width / 2, clientY: r.top + r.height / 2 };
    el.dispatchEvent(new MouseEvent('mouseover', o));
    await sleep(rand(120, 350)); // hover 后短暂停顿再按下
    el.dispatchEvent(new MouseEvent('mousedown', o));
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
static LOG_SINK: OnceLock<UnboundedSender<CollectLog>> = OnceLock::new();

/// 初始化日志落库通道(进程启动时调用一次)。
pub fn init_log_sink(sender: UnboundedSender<CollectLog>) {
    let _ = LOG_SINK.set(sender);
}

/// 把日志副本送入落库通道;通道未初始化 / 已关闭时静默忽略,不影响采集。
fn persist_log(log: &CollectLog) {
    if let Some(sink) = LOG_SINK.get() {
        let _ = sink.send(log.clone());
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

/// 给指定账号采集窗口的 HUD 追加一条日志;窗口已关 / 不存在则静默忽略。
/// 供 commands 在 pool collect 返回后(入库完成等)向 HUD 补充提示。
pub fn hud_log(app: &AppHandle, platform: &str, account_id: &str, level: &str, message: &str) {
    use tauri::Manager;
    if let Some(win) = app.get_webview_window(&pool::window_label(platform, account_id)) {
        let _ = win.eval(&build_hud_log_eval(level, message));
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

/// 构造「更新 HUD 状态条」的 eval 脚本。running 控制状态点颜色/呼吸。
pub fn build_hud_status_eval(text: &str, running: bool) -> String {
    let text_json = serde_json::to_string(text).unwrap_or_else(|_| "\"\"".to_string());
    format!("window.__veltrixHud&&window.__veltrixHud.status({text_json},{running});")
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
  var DEFAULT_KW = '日志';
  var SID_KEY = '__veltrix_hud_sid';
  // 状态色:绿=正常采集 / 红=遇到问题(风控、错误)/ 灰=空闲
  var COLOR_OK = '#22c55e', COLOR_ERR = '#ef4444', COLOR_IDLE = '#9ca3af';
  // 记住最近一次状态色,收起时据此画发光环
  var lastColor = COLOR_IDLE, lastGlow = false;

  // currentKeyword:后端经 beginKeyword 标记的「正在采集」关键字;activeTab:HUD 当前查看的 tab。
  // 二者都落 sessionStorage,因为 legacy 翻页是整页导航,脚本会重跑、闭包变量会丢。
  var currentKeyword = '';
  var activeTab = '';
  try { currentKeyword = sessionStorage.getItem(CUR_KEY) || ''; } catch (e) {}
  try { activeTab = sessionStorage.getItem(TAB_KEY) || ''; } catch (e) {}

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
    root.style.cssText = 'position:fixed;left:0;right:0;bottom:0;z-index:2147483647;height:33vh;background:rgba(17,24,39,.95);color:#e5e7eb;font:12px/1.55 system-ui,-apple-system,sans-serif;border-top:1px solid rgba(255,255,255,.14);box-shadow:0 -8px 24px rgba(0,0,0,.45);overflow:hidden;display:flex;flex-direction:column;pointer-events:auto;';
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
      // 只复制当前 tab(关键字)的日志
      var logs = getLogs().filter(function (it) { return (it.keyword || DEFAULT_KW) === activeTab; });
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
      var sid = null;
      try { sid = sessionStorage.getItem(SID_KEY); } catch (err) {}
      if (sid === null || sid === '') return;
      try {
        window.__TAURI_INTERNALS__.invoke('stop_collect', { sessionId: Number(sid) });
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

    // HUD 固定为底部栏(占满宽度、底部 1/3 高),不再恢复拖动位置

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

    // 选定初始 tab:上次查看的 → 正在采集的 → 第一个
    var kws = keywordsOf(getLogs());
    if (!activeTab || kws.indexOf(activeTab) < 0) {
      activeTab = currentKeyword || kws[0] || '';
    }
    renderTabs();
    renderBody();
    applyCollapsed(isCollapsed());
    setStateColor(isRunning() ? COLOR_OK : COLOR_IDLE, isRunning());
    updateStopBtn();
    return root;
  }

  function isCollapsed() {
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
      // 展开:恢复成完整面板(尺寸与初始 cssText 保持一致)
      if (head) head.style.display = 'flex';
      if (icon) icon.style.display = 'none';
      if (body) body.style.display = '';
      if (tabs) tabs.style.display = keywordsOf(getLogs()).length < 2 ? 'none' : 'flex';
      // 展开:恢复成底部固定栏(占满宽度、底部 1/3 高)
      root.style.left = '0';
      root.style.right = '0';
      root.style.top = 'auto';
      root.style.bottom = '0';
      root.style.width = 'auto';
      root.style.height = '33vh';
      root.style.maxHeight = '';
      root.style.border = 'none';
      root.style.borderTop = '1px solid rgba(255,255,255,.14)';
      root.style.boxShadow = '0 -8px 24px rgba(0,0,0,.45)';
    }
  }
  function setCollapsed(collapsed) {
    try { sessionStorage.setItem(COLLAPSE_KEY, collapsed ? '1' : '0'); } catch (e) {}
    applyCollapsed(collapsed);
  }

  function renderTabs() {
    var tabs = document.getElementById('veltrix-hud-tabs');
    if (!tabs) return;
    var kws = keywordsOf(getLogs());
    tabs.innerHTML = '';
    for (var i = 0; i < kws.length; i++) {
      (function (kw) {
        var on = kw === activeTab;
        var t = document.createElement('span');
        t.textContent = kw;
        t.style.cssText = 'padding:3px 9px;border-radius:6px 6px 0 0;cursor:pointer;white-space:nowrap;font-size:11px;' + (on ? 'background:rgba(255,255,255,.10);color:#e5e7eb;' : 'color:#9ca3af;');
        t.addEventListener('click', function () {
          activeTab = kw;
          try { sessionStorage.setItem(TAB_KEY, kw); } catch (e) {}
          renderTabs();
          renderBody();
        });
        tabs.appendChild(t);
      })(kws[i]);
    }
    tabs.style.display = (isCollapsed() || kws.length < 2) ? 'none' : 'flex';
  }

  function renderBody() {
    var body = document.getElementById('veltrix-hud-logs');
    if (!body) return;
    body.innerHTML = '';
    var logs = getLogs().filter(function (it) { return (it.keyword || DEFAULT_KW) === activeTab; });
    for (var i = 0; i < logs.length; i++) appendLine(logs[i]);
    body.scrollTop = body.scrollHeight;
  }

  function appendLine(item) {
    var body = document.getElementById('veltrix-hud-logs');
    if (!body) return;
    var line = document.createElement('div');
    var color = item.level === 'error' ? '#f87171' : (item.level === 'warn' ? '#fbbf24' : '#9ca3af');
    line.style.cssText = 'white-space:pre-wrap;word-break:break-all;color:' + color + ';';
    line.textContent = (item.time || '') + '  ' + (item.message || '');
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
  // 统一设置状态色:同时作用于标题栏状态点与收起图标;glow 控制辉光
  function setStateColor(c, glow) {
    lastColor = c; lastGlow = glow;
    var d = document.getElementById('veltrix-hud-dot');
    if (d) {
      d.style.background = c;
      d.style.boxShadow = glow ? '0 0 6px ' + c : 'none';
    }
    // 收起态整块填充状态色(白色波形不变),一眼可辨绿/红/灰
    var icon = document.getElementById('veltrix-hud-icon');
    if (icon) icon.style.background = c;
    // 收起时整块外发光,远比细图标醒目
    var root = document.getElementById('veltrix-hud');
    if (root && isCollapsed()) {
      root.style.boxShadow = (glow ? '0 0 14px ' + c + ',' : '') + '0 4px 16px rgba(0,0,0,.5)';
    }
  }

  window.__veltrixHud = {
    // 后端每轮采集前调用,后续 log 自动归到该关键字 tab 并切到它
    beginKeyword: function (kw) {
      kw = kw || DEFAULT_KW;
      currentKeyword = kw;
      activeTab = kw;
      try { sessionStorage.setItem(CUR_KEY, kw); } catch (e) {}
      try { sessionStorage.setItem(TAB_KEY, kw); } catch (e) {}
      ensureRoot();
      renderTabs();
      renderBody();
      applyCollapsed(isCollapsed());
    },
    log: function (item) {
      item = item || {};
      item.time = new Date().toLocaleTimeString();
      item.keyword = item.keyword || currentKeyword || DEFAULT_KW;
      ensureRoot();
      var hadTab = keywordsOf(getLogs()).indexOf(item.keyword) >= 0;
      try {
        var saved = getLogs();
        saved.push(item);
        if (saved.length > 400) saved = saved.slice(-400);
        sessionStorage.setItem(KEY, JSON.stringify(saved));
      } catch (e) {}
      if (!hadTab) renderTabs();
      if (item.keyword === activeTab) appendLine(item);
      // warn/error 视为「遇到问题」置红;info 恢复正常态(采集中绿 / 空闲灰)
      if (item.level === 'error' || item.level === 'warn') {
        setStateColor(COLOR_ERR, true);
      } else {
        setStateColor(isRunning() ? COLOR_OK : COLOR_IDLE, isRunning());
      }
    },
    status: function (text, running) {
      ensureRoot();
      var s = document.getElementById('veltrix-hud-status');
      if (s && text) s.textContent = text;
      try { sessionStorage.setItem(RUN_KEY, running ? '1' : '0'); } catch (e) {}
      setStateColor(running ? COLOR_OK : COLOR_IDLE, running);
      updateStopBtn();
    },
    // 绑定当前采集会话 id(供「结束」按钮回传);整页导航会丢 window 变量,故存 sessionStorage
    bindSession: function (sid) {
      try { sessionStorage.setItem(SID_KEY, String(sid)); } catch (e) {}
      ensureRoot();
      var b = document.getElementById('veltrix-hud-stop');
      if (b) { b.textContent = '结束'; b.style.pointerEvents = ''; }
      updateStopBtn();
    }
  };

  if (document.readyState === 'loading') {
    document.addEventListener('DOMContentLoaded', ensureRoot);
  } else {
    ensureRoot();
  }
})();"#
        .to_string()
}
