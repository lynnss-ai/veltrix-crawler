//! 抖音滑块验证码自动处理(secsdk / rmc captcha)。
//!
//! 流程:读几何(滑块按钮 / 轨道 / 验证图 / DPR)→ 截图并裁切到验证图区域 → 发给视觉模型
//! (默认 MiMo `mimo-v2.5`,凡 providers 表里标注 vision 能力的模型均可)识别目标位置 →
//! enigo 真实 OS 级鼠标拖拽对齐。失败时回退手动等待,不影响原有采集流程。
//!
//! 关键:secsdk 会校验事件 isTrusted,合成 DOM 事件会被判伪;故必须用真实鼠标(enigo)。
//! 坐标换算见 `try_auto_solve`:getBoundingClientRect 给 CSS 像素,enigo 要物理屏幕像素,
//! 中间靠窗口 inner_position(物理) + scale_factor 转换。

use tauri::WebviewWindow;

/// 视觉模型配置(从 providers 表读取,默认取首个标注 vision 能力的模型)。
pub struct VisionProvider {
    pub api_url: String,
    pub api_key: String,
    pub model: String,
}

/// 拖拽距离过小(像素)即视为识别失败,回退手动,避免无意义微抖。
const MIN_DRAG_CSS: f64 = 5.0;

/// AI 识别目标位置的 prompt。要求返回「占图宽的比例」而非绝对像素:
/// 比例与缩放/DPI 无关,配合「已裁切到验证图」的截图最稳。
/// 覆盖传统缺口滑块、盾牌/爱心/星形等拼图;强调拼块只水平移动、需同一行匹配、排除干扰项。
const GAP_DETECT_PROMPT: &str = r#"这是一张验证码背景图(已裁切到验证图区域,只含验证图)。图里有一个白色 / 空心轮廓的"拼块"(常见星形、爱心、盾牌等),以及一个或多个暗色 / 阴影形状。拼块只能水平左右移动,需要被拖到与它形状、大小、旋转角度一致、且处于同一水平高度(同一行)的那个暗色目标上。

请注意：
- 可能有多个暗色形状作为干扰项,只选与白色拼块"同一水平高度、形状角度最吻合"的那个作为目标。
- 若是传统矩形缺口滑块,则那个缺口就是目标。

请输出目标位置的"水平中心 X",表示为占整张图片宽度的比例(0 到 1 的小数:最左=0,正中=0.5,最右=1)。

只回复这一个小数(保留两到三位),不要任何其它文字。无法判断时回复 0。"#;

/// 点击验证码「刷新」按钮换一张挑战图(自动重试前调用)。best-effort,合成点击即可。
pub const CAPTCHA_REFRESH_JS: &str = r#"(function(){
  var r = document.querySelector('.vc-captcha-refresh') ||
          document.querySelector('.captcha_verify_refresh') ||
          document.querySelector('[class*="refresh"]');
  if (r) { try { r.click(); } catch (e) {} }
})()"#;

/// 读取滑块按钮 / 轨道 / 验证图几何与当前已拖距离、设备像素比。一次回读全部,返回 JSON 或 null。
/// 坐标均为 CSS 像素(getBoundingClientRect,相对网页视口左上角)。
const FIND_SLIDER_JS: &str = r#"
(function () {
  function rect(el) {
    if (!el) return null;
    var r = el.getBoundingClientRect();
    if (r.width === 0 || r.height === 0) return null;
    return r;
  }
  // 1. 滑块按钮(拖拽手柄)
  var btn = document.querySelector('.captcha-slider-btn') ||
            document.querySelector('.secsdk-captcha-drag-icon') ||
            document.querySelector('.captcha_verify_slide--button button');
  var br = rect(btn);
  if (!br) return null;
  // 2. 滑块轨道(取可拖宽度)
  var track = (btn.closest && (btn.closest('.captcha-slider-box') ||
               btn.closest('.captcha_verify_slide--button'))) || null;
  var tr = rect(track);
  // 3. 验证背景图(裁切与比例换算的基准)
  var img = document.querySelector('#captcha_verify_image') ||
            document.querySelector('.captcha_verify_img--wrapper img') ||
            document.querySelector('.captcha-verify-image');
  var ir = rect(img);
  if (!ir) return null;
  // 4. 当前拖块 translateX(滑块已被拖动过的距离;支持负值)
  var dragger = (btn.closest && btn.closest('.dragger-item')) || btn.parentElement;
  var currentX = 0;
  if (dragger) {
    var m = (dragger.style.transform || '').match(/translateX\((-?\d+(?:\.\d+)?)px\)/);
    if (m) currentX = parseFloat(m[1]);
  }
  // 5. 可移动拼块当前水平中心占图宽的比例(拼块未必从最左 0 起始,
  //    如星形/爱心拼图初始就在偏左某处;拖拽距离要从它当前位置算起)
  var piece = document.querySelector('#captcha-verify_img_slide') ||
              document.querySelector('.captcha-verify-image-slide');
  var pr = rect(piece);
  var pieceFrac = (pr && ir.width > 0) ? ((pr.left + pr.width / 2 - ir.left) / ir.width) : 0;
  return JSON.stringify({
    btnX: Math.round(br.left + br.width / 2),
    btnY: Math.round(br.top + br.height / 2),
    trackW: Math.round(tr ? tr.width : ir.width),
    imgLeft: Math.round(ir.left),
    imgTop: Math.round(ir.top),
    imgW: Math.round(ir.width),
    imgH: Math.round(ir.height),
    currentX: Math.round(currentX),
    pieceFrac: pieceFrac,
    dpr: window.devicePixelRatio || 1
  });
})()
"#;

/// 滑块几何回读结果(字段名与 FIND_SLIDER_JS 的 JSON 一致,camelCase 经 rename 映射)。
#[derive(serde::Deserialize)]
struct SliderInfo {
    #[serde(rename = "btnX")]
    btn_x: i32,
    #[serde(rename = "btnY")]
    btn_y: i32,
    #[serde(rename = "trackW")]
    track_w: i32,
    #[serde(rename = "imgLeft")]
    img_left: i32,
    #[serde(rename = "imgTop")]
    img_top: i32,
    #[serde(rename = "imgW")]
    img_w: i32,
    #[serde(rename = "imgH")]
    img_h: i32,
    #[serde(default, rename = "currentX")]
    current_x: i32,
    /// 可移动拼块当前水平中心占图宽比例(0~1);找不到拼块元素时为 0(按从最左起算)。
    #[serde(default, rename = "pieceFrac")]
    piece_frac: f64,
    #[serde(default = "default_dpr")]
    dpr: f64,
}

fn default_dpr() -> f64 {
    1.0
}

/// 把整窗 PNG 裁切到验证图区域。截图为物理像素,故按 dpr 把 CSS 矩形放大后裁。
/// 越界做夹取保护;失败返回 None。
fn crop_to_captcha_image(png: &[u8], info: &SliderInfo) -> Option<Vec<u8>> {
    use image::GenericImageView;

    let img = image::load_from_memory_with_format(png, image::ImageFormat::Png).ok()?;
    let (full_w, full_h) = img.dimensions();
    let dpr = if info.dpr > 0.0 { info.dpr } else { 1.0 };

    let x = (info.img_left as f64 * dpr).max(0.0) as u32;
    let y = (info.img_top as f64 * dpr).max(0.0) as u32;
    let mut w = (info.img_w as f64 * dpr).round() as u32;
    let mut h = (info.img_h as f64 * dpr).round() as u32;
    if w == 0 || h == 0 || x >= full_w || y >= full_h {
        return None;
    }
    // 防越界:裁切区不得超出图像边界
    w = w.min(full_w - x);
    h = h.min(full_h - y);

    let cropped = img.crop_imm(x, y, w, h);
    let mut out = std::io::Cursor::new(Vec::new());
    cropped.write_to(&mut out, image::ImageFormat::Png).ok()?;
    Some(out.into_inner())
}

/// 从模型回复里取首个浮点数并归一化为 (0,1) 比例。
/// 兼容模型把比例写成百分数(如 "62" / "62%" → 0.62)。无法解析或越界返回 None。
fn parse_fraction(reply: &str) -> Option<f32> {
    let cleaned: String = reply
        .chars()
        .map(|c| if c.is_ascii_digit() || c == '.' { c } else { ' ' })
        .collect();
    let mut value: f32 = cleaned
        .split_whitespace()
        .find_map(|t| t.parse::<f32>().ok())?;
    // 1~100 视为百分数,折算成比例
    if value > 1.0 && value <= 100.0 {
        value /= 100.0;
    }
    if value > 0.0 && value < 1.0 {
        Some(value)
    } else {
        None
    }
}

/// 截图裁切后发给视觉模型,返回目标位置占图宽的比例(0~1)。失败返回 None。
async fn detect_gap_fraction(
    window: &WebviewWindow,
    provider: &VisionProvider,
    info: &SliderInfo,
) -> Option<f32> {
    use crate::webview::screenshot::capture_png;

    // 截图前同步隐藏 HUD,避免浮层进入截图;不主动恢复——验证码仍在,
    // 由页面侧 auto-avoid 在验证结束后统一恢复(见 build_hud_init_script)。
    let _ = window.eval("var h=document.getElementById('veltrix-hud');if(h)h.style.display='none';");
    tokio::time::sleep(std::time::Duration::from_millis(120)).await; // 等一帧重绘

    let png_bytes = capture_png(window).await?;
    tracing::info!("滑块AI:截图成功 {} bytes", png_bytes.len());

    let cropped = match crop_to_captcha_image(&png_bytes, info) {
        Some(c) => c,
        None => {
            tracing::warn!("滑块AI:裁切验证图失败,改用整图");
            png_bytes
        }
    };

    use base64::Engine;
    let b64 = base64::engine::general_purpose::STANDARD.encode(&cropped);
    let data_url = format!("data:image/png;base64,{b64}");

    let messages = serde_json::json!([
        {
            "role": "user",
            "content": [
                { "type": "text", "text": GAP_DETECT_PROMPT },
                { "type": "image_url", "image_url": { "url": &data_url } }
            ]
        }
    ]);

    let endpoint = format!("{}/chat/completions", provider.api_url.trim_end_matches('/'));
    let body = serde_json::json!({
        "model": &provider.model,
        "messages": messages,
        "max_tokens": 50,
        "temperature": 0.0,
    });

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|e| tracing::warn!("滑块AI:HTTP 客户端创建失败: {e}"))
        .ok()?;

    let resp = client
        .post(&endpoint)
        .bearer_auth(&provider.api_key)
        .json(&body)
        .send()
        .await
        .map_err(|e| tracing::warn!("滑块AI:请求失败: {e}"))
        .ok()?;

    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        // 按字符截断:按字节切片会在多字节 UTF-8(如中文错误体)中间 panic
        tracing::warn!(
            "滑块AI:模型返回 {status}: {}",
            text.chars().take(200).collect::<String>()
        );
        return None;
    }

    let payload: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| tracing::warn!("滑块AI:解析响应失败: {e}"))
        .ok()?;

    let reply = payload["choices"][0]["message"]["content"]
        .as_str()
        .unwrap_or("")
        .trim()
        .to_string();
    tracing::info!("滑块AI:模型回复 '{reply}'");

    let frac = parse_fraction(&reply);
    match frac {
        Some(f) => tracing::info!("滑块AI:目标比例 {f:.3}"),
        None => tracing::warn!("滑块AI:解析比例失败或越界"),
    }
    frac
}

/// 用 enigo 做真实 OS 级鼠标拖拽(贝塞尔轨迹 + 随机抖动),坐标 / 距离均为物理屏幕像素。
fn enigo_drag(start_x: i32, start_y: i32, distance: u32) {
    use enigo::{Enigo, Mouse, Settings};
    use std::time::Duration;

    let mut enigo = match Enigo::new(&Settings::default()) {
        Ok(e) => e,
        Err(e) => {
            tracing::warn!("滑块:enigo 初始化失败: {e}");
            return;
        }
    };

    // 移动到滑块按钮位置
    if let Err(e) = enigo.move_mouse(start_x, start_y, enigo::Coordinate::Abs) {
        tracing::warn!("滑块:移动鼠标失败: {e}");
        return;
    }
    std::thread::sleep(Duration::from_millis(150));

    // 按下鼠标(不松开)
    if let Err(e) = enigo.button(enigo::Button::Left, enigo::Direction::Press) {
        tracing::warn!("滑块:按下鼠标失败: {e}");
        return;
    }
    std::thread::sleep(Duration::from_millis(80));

    // 贝塞尔轨迹拖拽
    let steps = 30 + (distance / 15) as usize;
    for i in 0..=steps {
        let t = i as f64 / steps as f64;
        // easeInOutCubic
        let progress = if t < 0.5 {
            4.0 * t * t * t
        } else {
            1.0 - (-2.0 * t + 2.0).powi(3) / 2.0
        };
        let x = start_x as f64 + distance as f64 * progress;
        // Y 轴轻微抖动
        let y_offset = ((t * 10.0).sin() * 2.0) as f64;
        let _ = enigo.move_mouse(x as i32, start_y + y_offset as i32, enigo::Coordinate::Abs);
        let delay = 10 + ((t * 7.0).sin() * 8.0) as u64;
        std::thread::sleep(Duration::from_millis(delay));
    }

    // 松开鼠标
    std::thread::sleep(Duration::from_millis(50));
    let _ = enigo.button(enigo::Button::Left, enigo::Direction::Release);
}

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
/// 不在此点击——由上层换算成 webview 客户区物理坐标后,用 PostMessage 向渲染子窗口发鼠标消息点击
/// (绕过抖音 secsdk 的 isTrusted 校验,且不依赖真实光标/前台,比 enigo 在本机更可靠)。
pub async fn locate_by_labels(window: &WebviewWindow, labels: &[String]) -> Option<(i32, i32)> {
    use crate::webview::script_eval::eval_json;
    if labels.is_empty() {
        return None;
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
    match pt {
        Some(p) => Some((p.x, p.y)),
        None => {
            tracing::warn!(
                "定位文案:页面未找到 {labels:?}(浮层未展开 / 文案不符 / 回读为空 {raw:?})"
            );
            None
        }
    }
}

/// 尝试自动完成滑块验证。返回 true 表示已触发拖拽(不代表验证通过,需上层复检)。
pub async fn try_auto_solve(window: &WebviewWindow, provider: &VisionProvider) -> bool {
    use crate::webview::script_eval::eval_json;

    tracing::info!("滑块:开始自动验证流程(模型 {})", provider.model);

    // 1. 读滑块 / 验证图几何
    let raw_json = eval_json(window.as_ref(), FIND_SLIDER_JS).await;
    tracing::info!(
        "滑块:FIND_SLIDER_JS 返回 {}",
        raw_json.as_deref().unwrap_or("None")
    );
    let info = match raw_json
        .as_deref()
        .and_then(|s| serde_json::from_str::<SliderInfo>(s).ok())
    {
        Some(i) if i.img_w > 0 && i.track_w > 0 => i,
        _ => {
            tracing::warn!("滑块:未找到滑块 / 验证图几何");
            return false;
        }
    };

    // 2. AI 识别目标位置比例
    let frac = match detect_gap_fraction(window, provider, &info).await {
        Some(f) => f,
        None => return false,
    };

    // 3. 目标滑块位移(CSS 像素,轨道坐标系):
    //    拼块随滑块在「验证图 ↔ 轨道」间等比例移动,故需移动的「图宽比例」=目标比例−拼块当前比例;
    //    换算到轨道:位移 = (目标比例 − 拼块当前比例) × 轨道宽(已拖距离 currentX 在此约去)。
    //    拼块当前比例已含 currentX,无需再减。找不到拼块元素时 piece_frac=0,退化为从最左起算。
    let move_frac = frac as f64 - info.piece_frac;
    let target_css = move_frac * info.track_w as f64;
    tracing::info!(
        "滑块:目标比例={frac:.3} 拼块当前比例={:.3} trackW={} → 需移动 {target_css:.1}px(CSS)",
        info.piece_frac,
        info.track_w
    );
    if target_css < MIN_DRAG_CSS {
        // 目标在拼块左侧或识别异常(本类拼图目标恒在右侧):跳过,回退手动
        tracing::warn!("滑块:目标位移过小或为负({target_css:.1}),跳过");
        return false;
    }

    // 4. CSS → 物理屏幕坐标:enigo 用物理像素,inner_position 给客户区物理左上角,
    //    scale_factor 把 CSS 像素折算成物理像素(Windows 上等于 devicePixelRatio)。
    let scale = window.scale_factor().unwrap_or(1.0);
    let origin = match window.inner_position() {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!("滑块:取窗口位置失败: {e}");
            return false;
        }
    };
    let start_x = origin.x + (info.btn_x as f64 * scale).round() as i32;
    let start_y = origin.y + (info.btn_y as f64 * scale).round() as i32;
    let dist = (target_css * scale).round() as u32;
    tracing::info!(
        "滑块:执行 enigo 拖拽 从物理({start_x},{start_y}) 距离{dist}px(scale={scale})"
    );
    enigo_drag(start_x, start_y, dist);
    tracing::info!("滑块:拖拽完成,等待验证结果");
    true
}
