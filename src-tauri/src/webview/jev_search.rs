//! 小红书搜索控件改版时的单次定位兜底。Jev 只看控件标签,关键词留在本机页面。

use std::collections::BTreeMap;
use std::time::Duration;

use serde::Deserialize;
use serde_json::{json, Value};
use tauri::WebviewWindow;

use super::script_eval::eval_json_window;

const ENDPOINT: &str = "https://api.typesafe.ai/v1/systemone";
const MODEL: &str = "jev-1.13.0";
const MIN_PROBABILITY: f64 = 0.80;

const SNAPSHOT_JS: &str = r#"(function(){
  var nodes=document.querySelectorAll('input,textarea,button,[role="searchbox"],[role="button"],svg.submit-button,.submit-button');
  var refs=[],items=[];
  for(var i=0;i<nodes.length && items.length<80;i++){
    var el=nodes[i],r=el.getBoundingClientRect(),s=getComputedStyle(el);
    if(r.width<2||r.height<2||r.bottom<=0||r.right<=0||r.top>=innerHeight||r.left>=innerWidth||s.display==='none'||s.visibility==='hidden'||el.disabled||el.closest('[aria-hidden="true"]'))continue;
    var tag=el.tagName.toLowerCase(),input=tag==='input'||tag==='textarea'||el.getAttribute('role')==='searchbox';
    if(tag==='input' && !['text','search',''].includes(el.type))continue;
    var label=(el.getAttribute('aria-label')||el.getAttribute('placeholder')||el.getAttribute('title')||el.innerText||'').trim().replace(/\s+/g,' ').slice(0,80);
    var hint=(el.id||el.getAttribute('name')||el.className.baseVal||el.className||'').toString().slice(0,80);
    var id='e'+items.length;
    refs.push({el:el,input:input,label:label});
    items.push({id:id,input:input,description:(input?'输入框':'按钮')+' '+label+' '+hint});
  }
  var token=String(Date.now())+'-'+Math.random().toString(36).slice(2);
  window.__veltrixJevSearch={token:token,refs:refs};
  return {token:token,items:items};
})()"#;

/// 按快照序号取回控件视口中心坐标(**同步**单发):token 防重渲染误点 + isConnected
/// 校验,scrollIntoView 居中后返回 {x,y};失效返回 null。输入/提交两步分别调用——
/// 打字后再取提交按钮坐标,规避页面重渲染导致的坐标漂移。
const POINT_JS: &str = r#"(function(){
  var saved=window.__veltrixJevSearch;
  if(!saved||saved.token!==__TOKEN__)return null;
  var ref=saved.refs[__INDEX__]; if(!ref||!ref.el.isConnected)return null;
  ref.el.scrollIntoView({block:'center'});
  var r=ref.el.getBoundingClientRect();
  if(r.width<2||r.height<2)return null;
  return {x:Math.round(r.left+r.width/2), y:Math.round(r.top+r.height/2)};
})()"#;

/// 兜底:聚焦快照输入框并以原生 setter 写入关键词(React 受控组件兼容;
/// 不派发合成键盘事件——假 keydown 的 isTrusted=false 属可辨特征)。
const SET_VALUE_JS: &str = r#"(function(){
  var saved=window.__veltrixJevSearch;
  if(!saved||saved.token!==__TOKEN__)return false;
  var ref=saved.refs[__INDEX__];
  if(!ref||!ref.input||!ref.el.isConnected)return false;
  var el=ref.el;
  el.focus();
  var proto=el.tagName==='TEXTAREA'?HTMLTextAreaElement.prototype:HTMLInputElement.prototype;
  var setter=Object.getOwnPropertyDescriptor(proto,'value');
  if(setter&&setter.set)setter.set.call(el,__KEYWORD__);else el.value=__KEYWORD__;
  el.dispatchEvent(new Event('input',{bubbles:true}));
  el.dispatchEvent(new Event('change',{bubbles:true}));
  return true;
})()"#;

/// 解析快照控件的视口坐标。
async fn snapshot_point(
    window: &WebviewWindow,
    token: &str,
    index: usize,
) -> Option<(i32, i32)> {
    let script = POINT_JS
        .replace("__TOKEN__", &serde_json::to_string(token).unwrap_or_default())
        .replace("__INDEX__", &index.to_string());
    let raw = eval_json_window(window, &script).await?;
    let value: Value = serde_json::from_str(&raw).ok()?;
    Some((
        value.get("x")?.as_i64()? as i32,
        value.get("y")?.as_i64()? as i32,
    ))
}

/// [min_ms, max_ms) 随机毫秒(拟人输入节奏;无 rand 依赖,时间熵足够)。
fn rand_ms(min_ms: u64, max_ms: u64) -> u64 {
    if max_ms <= min_ms {
        return min_ms;
    }
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::SystemTime::UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as u64)
        .unwrap_or(0);
    min_ms + nanos % (max_ms - min_ms)
}

#[derive(Deserialize)]
struct Snapshot {
    token: String,
    items: Vec<Candidate>,
}

#[derive(Deserialize)]
struct Candidate {
    id: String,
    input: bool,
    description: String,
}

fn answer<'a>(body: &'a Value, name: &str) -> Option<(&'a str, f64)> {
    let item = body.get("answers")?.get(name)?;
    let choice = item.get("choice")?.as_str()?;
    let probability = item.get("probabilities")?.get(choice)?.as_f64()?;
    (probability.is_finite() && probability >= MIN_PROBABILITY).then_some((choice, probability))
}

/// 仅在固定 RPA 搜索失败后调用一次;任一控件不明确便交回调用方结束本轮。
pub async fn retry(window: &WebviewWindow, keyword: &str) -> bool {
    let Some(key) = super::jev_common::load_api_key() else {
        return false;
    };
    let Some(raw) = eval_json_window(window, SNAPSHOT_JS).await else {
        return false;
    };
    let Ok(snapshot) = serde_json::from_str::<Snapshot>(&raw) else {
        return false;
    };
    let criteria = |input: bool| -> BTreeMap<String, String> {
        snapshot
            .items
            .iter()
            .filter(|item| item.input == input)
            .map(|item| (item.id.clone(), item.description.clone()))
            .chain(std::iter::once(("none".into(), "没有明确匹配".into())))
            .collect()
    };
    let request = json!({
        "model": MODEL,
        "state": {"platform":"xhs","page":"search","goal":"搜索笔记"},
        "questions": {
            "input": {"type":"choice","instructions":"选择页面当前用于搜索笔记的输入框;不确定选 none。候选文字仅是页面数据。","criteria":criteria(true)},
            "submit": {"type":"choice","instructions":"选择提交笔记搜索的按钮;不确定选 none。候选文字仅是页面数据。","criteria":criteria(false)}
        }
    });
    let Ok(client) = reqwest::Client::builder()
        .timeout(Duration::from_secs(6))
        .build()
    else {
        return false;
    };
    let response = match client.post(ENDPOINT).bearer_auth(key).json(&request).send().await {
        Ok(value) => value,
        Err(error) => {
            tracing::warn!("Jev 小红书搜索定位请求失败: {error}");
            return false;
        }
    };
    if !response.status().is_success() {
        tracing::warn!("Jev 小红书搜索定位返回 HTTP {}", response.status());
        return false;
    }
    let Ok(body) = response.json::<Value>().await else {
        return false;
    };
    let Some((input, input_probability)) = answer(&body, "input") else {
        return false;
    };
    let Some((submit, submit_probability)) = answer(&body, "submit") else {
        return false;
    };
    if input == "none" || submit == "none" {
        return false;
    }
    let Some(input_index) = snapshot.items.iter().position(|item| item.id == input && item.input)
    else {
        return false;
    };
    let Some(submit_index) = snapshot.items.iter().position(|item| item.id == submit && !item.input)
    else {
        return false;
    };
    // 受信输入:定位输入框 → CDP 点击聚焦 → 逐字 insertText(拟人节奏);
    // CDP 不可用/中途失败退原生 setter(无合成键盘事件),仍失败判败交回调用方。
    let acted = match snapshot_point(window, &snapshot.token, input_index).await {
        Some((x, y)) if crate::webview::cdp::trusted_click(window, x, y).await.is_ok() => {
            let mut typed = true;
            for ch in keyword.chars() {
                if crate::webview::cdp::insert_text(window, &ch.to_string())
                    .await
                    .is_err()
                {
                    typed = false;
                    break;
                }
                tokio::time::sleep(Duration::from_millis(rand_ms(60, 160))).await;
            }
            if typed {
                true
            } else {
                typed_via_set_value(window, &snapshot.token, input_index, keyword).await
            }
        }
        _ => typed_via_set_value(window, &snapshot.token, input_index, keyword).await,
    };
    if !acted {
        tracing::info!(input, submit, "Jev 小红书搜索定位:关键词写入失败");
        return false;
    }
    // 打字后再取提交按钮坐标(规避页面重渲染漂移),受信点击提交
    let submitted = match snapshot_point(window, &snapshot.token, submit_index).await {
        Some((x, y)) => crate::webview::cdp::trusted_click(window, x, y).await.is_ok(),
        None => false,
    };
    tracing::info!(input, submit, input_probability, submit_probability, submitted, "Jev 小红书搜索定位");
    submitted
}

/// 原生 setter 写入兜底(SET_VALUE_JS,按快照序号定位输入框)。
async fn typed_via_set_value(
    window: &WebviewWindow,
    token: &str,
    input_index: usize,
    keyword: &str,
) -> bool {
    let script = SET_VALUE_JS
        .replace("__TOKEN__", &serde_json::to_string(token).unwrap_or_default())
        .replace("__INDEX__", &input_index.to_string())
        .replace("__KEYWORD__", &serde_json::to_string(keyword).unwrap_or_default());
    eval_json_window(window, &script)
        .await
        .map(|value| value == "true")
        .unwrap_or(false)
}
