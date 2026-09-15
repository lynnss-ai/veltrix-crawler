//! 内网媒体文件服务。
//!
//! 访问前缀按当前网卡自动生成,/files 路由映射到媒体根目录。
//! 数据库里的本机绝对路径不会暴露给网络,路径穿越、符号链接越界和非媒体根目录文件均拒绝访问。

use axum::body::Body;
use axum::extract::State;
use axum::http::{header, Request, Response, StatusCode, Uri};
use axum::Router;
use std::path::{Path, PathBuf};
use tauri::{AppHandle, Manager};
use tokio::io::{AsyncReadExt, AsyncSeekExt, SeekFrom};
use tokio_util::io::ReaderStream;
use url::Url;

/// 内网媒体服务固定端口。前缀可省略端口,省略时按此端口拼接。
pub const DEFAULT_PORT: u16 = 8788;

/// 启动内网文件服务。固定路由不依赖网卡 IP,切换网络后重新获取前缀即可访问。
pub async fn serve(app: AppHandle) -> std::io::Result<()> {
    let addr = std::net::SocketAddr::from(([0, 0, 0, 0], DEFAULT_PORT));
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!("内网文件服务已启动: http://0.0.0.0:{DEFAULT_PORT}");
    axum::serve(
        listener,
        Router::new().fallback(handle_request).with_state(app),
    )
    .await
}

/// 校验并规范化设置中的访问前缀。端口固定为 DEFAULT_PORT,避免设置看似保存成功但服务实际不可达。
pub fn normalize_prefix(raw: &str) -> Result<String, String> {
    let value = raw.trim().trim_end_matches('/');
    if value.is_empty() {
        return Ok(String::new());
    }
    let mut parsed = Url::parse(value).map_err(|_| {
        "内网访问前缀必须是完整 URL,例如 http://192.168.1.10:8788/files".to_string()
    })?;
    if parsed.scheme() != "http" || parsed.host_str().is_none() {
        return Err("内网访问前缀必须使用 http,并包含主机地址".to_string());
    }
    if parsed.username() != ""
        || parsed.password().is_some()
        || parsed.query().is_some()
        || parsed.fragment().is_some()
    {
        return Err("内网访问前缀不能包含用户名、密码、查询参数或片段".to_string());
    }
    if parsed.port().is_some_and(|port| port != DEFAULT_PORT) {
        return Err(format!("内网文件服务端口固定为 {DEFAULT_PORT}"));
    }
    if parsed.port().is_none() {
        parsed
            .set_port(Some(DEFAULT_PORT))
            .map_err(|_| format!("内网文件服务端口固定为 {DEFAULT_PORT}"))?;
    }
    Ok(parsed.to_string().trim_end_matches('/').to_string())
}

/// 把数据库保存的本机素材路径(相对 media_root 的相对路径;兼容存量绝对路径)转换成可分享的内网文件 URL。
pub fn public_url_for_path(prefix: &str, media_root: &Path, local_path: &str) -> Option<String> {
    let prefix = normalize_prefix(prefix).ok()?;
    if prefix.is_empty() {
        return None;
    }
    let root = media_root.canonicalize().ok()?;
    let file = crate::media::resolve_media_path(media_root, local_path)
        .canonicalize()
        .ok()?;
    if !file.starts_with(&root) || !file.is_file() {
        return None;
    }
    let relative = file.strip_prefix(&root).ok()?;
    let mut url = Url::parse(&prefix).ok()?;
    let mut segments = url.path_segments_mut().ok()?;
    segments.pop_if_empty();
    for component in relative.components() {
        let value = component.as_os_str().to_str()?;
        if value.is_empty() || value == "." || value == ".." {
            return None;
        }
        segments.push(value);
    }
    drop(segments);
    Some(url.to_string())
}

async fn handle_request(State(app): State<AppHandle>, request: Request<Body>) -> Response<Body> {
    if request.method() != axum::http::Method::GET && request.method() != axum::http::Method::HEAD {
        return response(StatusCode::METHOD_NOT_ALLOWED, Body::empty(), &[]);
    }
    let Some(state) = app.try_state::<crate::commands::AppState>() else {
        return response(StatusCode::SERVICE_UNAVAILABLE, Body::empty(), &[]);
    };
    let media_root = match state.config.lock() {
        Ok(cfg) => crate::media::media_root(&state.config_dir, &cfg.media),
        Err(_) => return response(StatusCode::SERVICE_UNAVAILABLE, Body::empty(), &[]),
    };
    let Some(relative) = relative_path("http://localhost:8788/files", request.uri()) else {
        return response(StatusCode::NOT_FOUND, Body::empty(), &[]);
    };
    let path = match safe_file_path(&media_root, &relative).await {
        Some(path) => path,
        // 惰性缩略图:请求 *_thumb.jpg 且文件不存在时,尝试从源图现生成(兼容存量数据)
        None => match lazy_thumbnail(&media_root, &relative).await {
            Some(path) => path,
            None => return response(StatusCode::NOT_FOUND, Body::empty(), &[]),
        },
    };
    let Ok(meta) = tokio::fs::metadata(&path).await else {
        return response(StatusCode::NOT_FOUND, Body::empty(), &[]);
    };
    let size = meta.len();
    let modified = meta.modified().ok();
    let mtime_secs = modified
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // 弱 ETag 按 (长度, mtime 秒) 派生:素材落盘后不会原地修改,长度 + 秒级 mtime 足够判变;
    // 用弱校验器是因为惰性缩略图等场景下同一 URL 不承诺字节级强一致
    let etag = format!("W/\"{size:x}-{mtime_secs:x}\"");
    let last_modified = modified.map(httpdate::fmt_http_date);
    // 协商缓存命中直接 304(不带 body),判断须在 Range 之前
    if is_not_modified(request.headers(), &etag, mtime_secs) {
        let mut builder = Response::builder()
            .status(StatusCode::NOT_MODIFIED)
            .header(header::ETAG, &etag)
            .header(header::CACHE_CONTROL, "public, max-age=3600");
        if let Some(value) = &last_modified {
            builder = builder.header(header::LAST_MODIFIED, value);
        }
        return builder
            .body(Body::empty())
            .unwrap_or_else(|_| Response::new(Body::empty()));
    }
    let content_type = content_type(&path);
    let range = request
        .headers()
        .get(header::RANGE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| parse_range(value, size));
    let (status, start, length, content_range) = match range {
        Some(Ok((start, end))) => (
            StatusCode::PARTIAL_CONTENT,
            start,
            end - start + 1,
            Some(format!("bytes {start}-{end}/{size}")),
        ),
        Some(Err(())) => {
            return Response::builder()
                .status(StatusCode::RANGE_NOT_SATISFIABLE)
                .header(header::CONTENT_RANGE, format!("bytes */{size}"))
                .body(Body::empty())
                .unwrap_or_else(|_| Response::new(Body::empty()));
        }
        None => (StatusCode::OK, 0, size, None),
    };
    let mut builder = Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, content_type)
        .header(header::CONTENT_LENGTH, length)
        .header(header::ACCEPT_RANGES, "bytes")
        .header(header::ETAG, &etag)
        .header(header::CACHE_CONTROL, "public, max-age=3600")
        .header("access-control-allow-origin", "*");
    if let Some(value) = &last_modified {
        builder = builder.header(header::LAST_MODIFIED, value);
    }
    if let Some(value) = content_range {
        builder = builder.header(header::CONTENT_RANGE, value);
    }
    if request.method() == axum::http::Method::HEAD {
        return builder
            .body(Body::empty())
            .unwrap_or_else(|_| Response::new(Body::empty()));
    }
    let Ok(mut file) = tokio::fs::File::open(&path).await else {
        return response(StatusCode::NOT_FOUND, Body::empty(), &[]);
    };
    if start > 0 && file.seek(SeekFrom::Start(start)).await.is_err() {
        return response(StatusCode::INTERNAL_SERVER_ERROR, Body::empty(), &[]);
    }
    let stream = ReaderStream::new(file.take(length));
    builder
        .body(Body::from_stream(stream))
        .unwrap_or_else(|_| Response::new(Body::empty()))
}

fn response(status: StatusCode, body: Body, headers: &[(&str, &str)]) -> Response<Body> {
    let mut builder = Response::builder().status(status);
    for (key, value) in headers {
        builder = builder.header(*key, *value);
    }
    builder
        .body(body)
        .unwrap_or_else(|_| Response::new(Body::empty()))
}

fn relative_path(prefix: &str, uri: &Uri) -> Option<String> {
    let prefix = normalize_prefix(prefix).ok()?;
    if prefix.is_empty() {
        return None;
    }
    let prefix_url = Url::parse(&prefix).ok()?;
    let prefix_path = prefix_url.path().trim_end_matches('/');
    let request_path = uri.path();
    let relative = if prefix_path.is_empty() {
        request_path.strip_prefix('/')?
    } else if request_path == prefix_path {
        return None;
    } else {
        request_path.strip_prefix(prefix_path)?.strip_prefix('/')?
    };
    // 浏览器会编码中文、空格和 #。按路径段解码一次,解码后仍拒绝分隔符与路径穿越。
    let parts = relative
        .split('/')
        .map(|part| {
            percent_encoding::percent_decode_str(part)
                .decode_utf8()
                .ok()
                .map(|s| s.into_owned())
        })
        .collect::<Option<Vec<_>>>()?;
    if parts.iter().any(|part| {
        part.is_empty() || part == "." || part == ".." || part.contains(['/', '\\', '\0', ':'])
    }) {
        return None;
    }
    Some(parts.join("/"))
}

async fn safe_file_path(root: &Path, relative: &str) -> Option<PathBuf> {
    let root = tokio::fs::canonicalize(root).await.ok()?;
    let candidate = root.join(relative.replace('/', &std::path::MAIN_SEPARATOR.to_string()));
    let canonical = tokio::fs::canonicalize(candidate).await.ok()?;
    if canonical.starts_with(&root) && canonical.is_file() {
        Some(canonical)
    } else {
        None
    }
}

/// 惰性缩略图:请求路径以 `_thumb.jpg` 结尾且目标文件不存在时,推导源文件
/// (去掉 `_thumb` 后缀,按常见图片扩展名探测),现生成缩略图后返回;
/// 生成失败或源图超体积上限时回源直接返回源文件,保证前端仍能显示。
async fn lazy_thumbnail(root: &Path, relative: &str) -> Option<PathBuf> {
    const THUMB_SUFFIX: &str = "_thumb.jpg";
    if !relative.to_ascii_lowercase().ends_with(THUMB_SUFFIX) {
        return None;
    }
    let base = &relative[..relative.len() - THUMB_SUFFIX.len()];
    for ext in ["jpg", "jpeg", "png", "webp"] {
        let candidate = format!("{base}.{ext}");
        let Some(source) = safe_file_path(root, &candidate).await else {
            continue;
        };
        return Some(
            crate::thumbnail::ensure(source.clone())
                .await
                .unwrap_or(source),
        );
    }
    None
}

/// 协商缓存判定:If-None-Match 命中(弱比较,兼容 `*` 与客户端回传时去/加 W/ 前缀)
/// 或 If-Modified-Since 不早于文件 mtime 时返回 true(httpdate 只有秒粒度,mtime 先截断到秒再比)。
fn is_not_modified(headers: &axum::http::HeaderMap, etag: &str, mtime_secs: u64) -> bool {
    if let Some(value) = headers
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
    {
        let bare = etag.trim_start_matches("W/");
        let matched = value.split(',').any(|token| {
            let token = token.trim();
            token == "*" || token == etag || token.trim_start_matches("W/") == bare
        });
        if matched {
            return true;
        }
    }
    if let Some(value) = headers
        .get(header::IF_MODIFIED_SINCE)
        .and_then(|v| v.to_str().ok())
    {
        if let Ok(since) = httpdate::parse_http_date(value) {
            let since_secs = since
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            return since_secs >= mtime_secs;
        }
    }
    false
}

fn parse_range(value: &str, size: u64) -> Option<Result<(u64, u64), ()>> {
    let value = value.strip_prefix("bytes=")?;
    if value.contains(',') || size == 0 {
        return Some(Err(()));
    }
    let (start, end) = value.split_once('-')?;
    if start.is_empty() {
        let suffix: u64 = end.parse().ok()?;
        if suffix == 0 {
            return Some(Err(()));
        }
        let length = suffix.min(size);
        return Some(Ok((size - length, size - 1)));
    }
    let start: u64 = start.parse().ok()?;
    if start >= size {
        return Some(Err(()));
    }
    let end = if end.is_empty() {
        size - 1
    } else {
        end.parse::<u64>().ok()?.min(size - 1)
    };
    if start > end {
        Some(Err(()))
    } else {
        Some(Ok((start, end)))
    }
}

fn content_type(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "jpg" | "jpeg" => "image/jpeg",
        "png" => "image/png",
        "webp" => "image/webp",
        "gif" => "image/gif",
        "avif" => "image/avif",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "m4a" => "audio/mp4",
        "mp4" => "video/mp4",
        "webm" => "video/webm",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::{is_not_modified, normalize_prefix, parse_range, relative_path};
    use axum::http::Uri;

    #[test]
    fn prefix_validation_keeps_fixed_port_and_trims_slash() {
        assert_eq!(
            normalize_prefix(" http://192.168.1.2:8788/files/ ").unwrap(),
            "http://192.168.1.2:8788/files"
        );
        assert_eq!(
            normalize_prefix("http://192.168.1.2/files").unwrap(),
            "http://192.168.1.2:8788/files"
        );
        assert!(normalize_prefix("http://192.168.1.2:9000/files").is_err());
        assert!(normalize_prefix("https://192.168.1.2:8788/files").is_err());
        assert!(normalize_prefix("http://user:pass@192.168.1.2:8788/files").is_err());
    }

    #[test]
    fn relative_path_rejects_prefix_collision_and_traversal() {
        assert_eq!(
            relative_path(
                "http://192.168.1.2:8788/files",
                &Uri::from_static("/files/douyin/a.jpg")
            ),
            Some("douyin/a.jpg".to_string())
        );
        assert_eq!(
            relative_path(
                "http://192.168.1.2:8788/files",
                &Uri::from_static("/files2/a.jpg")
            ),
            None
        );
        assert_eq!(
            relative_path(
                "http://192.168.1.2:8788/files",
                &Uri::from_static("/files/../secret")
            ),
            None
        );
    }

    #[test]
    fn range_parser_supports_open_and_suffix_ranges() {
        assert_eq!(parse_range("bytes=10-19", 100), Some(Ok((10, 19))));
        assert_eq!(parse_range("bytes=90-", 100), Some(Ok((90, 99))));
        assert_eq!(parse_range("bytes=-10", 100), Some(Ok((90, 99))));
        assert_eq!(parse_range("bytes=100-", 100), Some(Err(())));
    }

    #[test]
    fn file_paths_decode_names_without_allowing_encoded_traversal() {
        let prefix = "http://192.168.1.2:8788/files";
        assert_eq!(
            relative_path(prefix, &Uri::from_static("/files/xhs/%E5%9B%BE%20%231.jpg")),
            Some("xhs/图 #1.jpg".into())
        );
        assert_eq!(
            relative_path(prefix, &Uri::from_static("/files/100%25.jpg")),
            Some("100%.jpg".into())
        );
        for path in [
            "/files/%2e%2e/secret",
            "/files/a%2fb.jpg",
            "/files/a%5cb.jpg",
            "/files/a%00.jpg",
            "/files/a%3ab.jpg",
        ] {
            assert_eq!(relative_path(prefix, &path.parse().unwrap()), None);
        }
    }

    #[test]
    fn conditional_headers_trigger_not_modified() {
        use axum::http::{header, HeaderMap, HeaderValue};
        let etag = "W/\"3e8-64a1b2c3\"";
        let mtime = 0x64a1b2c3u64;
        let mut headers = HeaderMap::new();
        assert!(!is_not_modified(&headers, etag, mtime));
        headers.insert(
            header::IF_NONE_MATCH,
            HeaderValue::from_static("W/\"3e8-64a1b2c3\""),
        );
        assert!(is_not_modified(&headers, etag, mtime));
        // 客户端回传裸值(去 W/ 前缀)也应弱比较命中
        headers.insert(
            header::IF_NONE_MATCH,
            HeaderValue::from_static("\"3e8-64a1b2c3\""),
        );
        assert!(is_not_modified(&headers, etag, mtime));
        headers.insert(
            header::IF_NONE_MATCH,
            HeaderValue::from_static("W/\"3e8-00000000\""),
        );
        assert!(!is_not_modified(&headers, etag, mtime));
        // If-Modified-Since 等于 / 晚于 mtime 均 304;早于则重新下发
        headers.remove(header::IF_NONE_MATCH);
        let since =
            httpdate::fmt_http_date(std::time::UNIX_EPOCH + std::time::Duration::from_secs(mtime));
        headers.insert(
            header::IF_MODIFIED_SINCE,
            HeaderValue::from_str(&since).unwrap(),
        );
        assert!(is_not_modified(&headers, etag, mtime));
        let stale = httpdate::fmt_http_date(
            std::time::UNIX_EPOCH + std::time::Duration::from_secs(mtime - 1),
        );
        headers.insert(
            header::IF_MODIFIED_SINCE,
            HeaderValue::from_str(&stale).unwrap(),
        );
        assert!(!is_not_modified(&headers, etag, mtime));
    }
}
