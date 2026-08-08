//! 筛选面板文案定位:按可见文案在页面里查元素并回读其视口中心(CSS 像素),
//! 供采集结果页「展开筛选浮层 + 点选排序/时间/额外筛选」的 RPA 点击定位使用。
//!
//! 只回读坐标、不在此点击——由上层换算成 webview 客户区物理坐标后,用 PostMessage 向渲染
//! 子窗口发鼠标消息点击(绕过抖音 secsdk 的 isTrusted 校验,且不依赖真实光标 / 前台)。

use tauri::WebviewWindow;

/// 按文案查元素并回读其视口中心(CSS 像素)。匹配规则同 build_select_eval(精确 textContent,
/// 跳过 aria-hidden 诱饵与零尺寸),命中即滚入视野后返回中心点;无命中返回 null。
const FIND_BY_TEXT_JS: &str = r#"(function(){
  var LABELS = __LABELS__;
  if (!LABELS.length) return null;
  var nodes = document.querySelectorAll('button,a,span,div,li,[role="tab"],[role="button"]');
  for (var i=0;i<nodes.length;i++){
    var el=nodes[i];
    var t=(el.textContent||'').trim();
    var hit=false; for(var j=0;j<LABELS.length;j++){ if(t===LABELS[j]){hit=true;break;} }
    if(!hit) continue;
    if(el.closest && el.closest('[aria-hidden="true"]')) continue;
    var r=el.getBoundingClientRect();
    if(r.width<1||r.height<1) continue;
    try{ el.scrollIntoView({block:'center'}); }catch(e){}
    r=el.getBoundingClientRect();
    // 直接返回对象(不要 JSON.stringify):ExecuteScript 会再序列化一次,返回字符串会被双重编码导致解析失败
    return {x:Math.round(r.left+r.width/2), y:Math.round(r.top+r.height/2)};
  }
  return null;
})()"#;

#[derive(serde::Deserialize)]
struct ClickPoint {
    x: i32,
    y: i32,
}

/// 按文案回读目标元素的**视口中心(CSS 像素)**;命中返回 Some((cssX, cssY)),未找到返回 None。
/// 不在此点击——由上层换算成 webview 客户区物理坐标后,用 PostMessage 向渲染子窗口发鼠标消息点击。
pub async fn locate_by_labels(window: &WebviewWindow, labels: &[String]) -> Option<(i32, i32)> {
    let (raw, pt) = locate_by_labels_raw(window, labels).await;
    if pt.is_none() {
        tracing::warn!(
            "定位文案:页面未找到 {labels:?}(浮层未展开 / 文案不符 / 回读为空 {raw:?})"
        );
    }
    pt
}

/// 静默定位:同 locate_by_labels,但未命中不打告警——供「浮层就绪轮询 / 重试预检」
/// 高频调用,避免每轮轮询都刷一条 warn。
pub async fn locate_by_labels_quiet(
    window: &WebviewWindow,
    labels: &[String],
) -> Option<(i32, i32)> {
    locate_by_labels_raw(window, labels).await.1
}

/// 定位实现:返回 (原始回读文本, 解析出的坐标),告警与否由包装层决定。
async fn locate_by_labels_raw(
    window: &WebviewWindow,
    labels: &[String],
) -> (Option<String>, Option<(i32, i32)>) {
    use crate::webview::script_eval::eval_json;
    if labels.is_empty() {
        return (None, None);
    }
    let labels_json = serde_json::to_string(labels).unwrap_or_else(|_| "[]".into());
    let js = FIND_BY_TEXT_JS.replace("__LABELS__", &labels_json);
    let raw = eval_json(window.as_ref(), &js).await;
    // 优先按对象解析;若取回的是被双重 JSON 编码的字符串,先剥一层再解析(兼容两种回读形态)
    let pt = raw.as_deref().and_then(|s| {
        serde_json::from_str::<ClickPoint>(s).ok().or_else(|| {
            serde_json::from_str::<String>(s)
                .ok()
                .and_then(|inner| serde_json::from_str::<ClickPoint>(&inner).ok())
        })
    });
    (raw, pt.map(|p| (p.x, p.y)))
}
