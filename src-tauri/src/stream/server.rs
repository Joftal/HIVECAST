use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;

/// 只服务本机回环的 HLS 缓存目录。
///
/// 安全模型：
/// 1. 只绑定 127.0.0.1，端口随机；
/// 2. 路径前缀带一次性 token，会话结束即作废；
/// 3. 校验 Host，拒绝非回环访问；
/// 4. Origin 只放行 Tauri 自身与开发服务器两个来源。
pub struct StreamServer {
    token: String,
    port: u16,
    sessions: RwLock<HashMap<String, PathBuf>>,
}

impl StreamServer {
    pub async fn start() -> anyhow::Result<Arc<Self>> {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await?;
        let port = listener.local_addr()?.port();
        let token = uuid::Uuid::new_v4().simple().to_string();

        let server = Arc::new(Self {
            token,
            port,
            sessions: RwLock::new(HashMap::new()),
        });

        let app = router(server.clone());
        tokio::spawn(async move {
            if let Err(err) = axum::serve(listener, app).await {
                tracing::error!(error = %err, "loopback stream server stopped");
            }
        });

        tracing::info!(port, "loopback stream server listening on 127.0.0.1");
        Ok(server)
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    pub fn register(&self, session_id: &str, dir: PathBuf) {
        if let Ok(mut map) = self.sessions.write() {
            map.insert(session_id.to_string(), dir);
        }
    }

    /// 会话结束时调用；M0 的演示流常驻，M2 起每个槽位结束即注销。
    #[allow(dead_code)]
    pub fn unregister(&self, session_id: &str) {
        if let Ok(mut map) = self.sessions.write() {
            map.remove(session_id);
        }
    }

    /// 播放列表地址；token 放在路径里，前端拿不到其它会话。
    pub fn playlist_url(&self, session_id: &str) -> String {
        format!(
            "http://127.0.0.1:{port}/s/{token}/{session}/index.m3u8",
            port = self.port,
            token = self.token,
            session = session_id
        )
    }
}

fn router(state: Arc<StreamServer>) -> Router {
    Router::new()
        .route("/s/{token}/{session}/{*path}", get(serve_file).options(preflight))
        .with_state(state)
}

fn allowed_origin(origin: &str) -> bool {
    matches!(
        origin,
        "tauri://localhost"
            | "http://tauri.localhost"
            | "https://tauri.localhost"
            | "http://localhost:1420"
            | "http://127.0.0.1:1420"
    )
}

fn is_loopback_host(headers: &HeaderMap, port: u16) -> bool {
    let Some(host) = headers.get(header::HOST).and_then(|v| v.to_str().ok()) else {
        return false;
    };
    host == format!("127.0.0.1:{port}") || host == format!("localhost:{port}")
}

fn cors_headers(headers: &HeaderMap, origin: &str) -> HeaderMap {
    let mut out = HeaderMap::new();
    if let Ok(value) = HeaderValue::from_str(origin) {
        out.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, value);
    }
    out.insert(header::VARY, HeaderValue::from_static("Origin"));
    out.insert(header::ACCESS_CONTROL_ALLOW_METHODS, HeaderValue::from_static("GET, HEAD, OPTIONS"));
    out.insert(header::ACCESS_CONTROL_ALLOW_HEADERS, HeaderValue::from_static("Range"));
    out.insert(
        header::ACCESS_CONTROL_EXPOSE_HEADERS,
        HeaderValue::from_static("Content-Length, Content-Range, Accept-Ranges"),
    );
    let _ = headers;
    out
}

fn content_type(path: &str) -> &'static str {
    match path.rsplit('.').next().unwrap_or_default() {
        "m3u8" => "application/vnd.apple.mpegurl",
        "m4s" | "mp4" => "video/mp4",
        "ts" => "video/mp2t",
        _ => "application/octet-stream",
    }
}

/// 只允许这些扩展名的文件被读出，避免把缓存目录里的日志等文件暴露出去。
fn allowed_extension(path: &str) -> bool {
    matches!(
        path.rsplit('.').next().unwrap_or_default(),
        "m3u8" | "m4s" | "mp4" | "ts"
    )
}

fn safe_relative_path(path: &str) -> Option<&str> {
    if path.is_empty()
        || path.contains("..")
        || path.contains('\\')
        || path.starts_with('/')
        || path.contains("//")
    {
        return None;
    }
    let ok = path
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '/'));
    ok.then_some(path)
}

async fn preflight(State(state): State<Arc<StreamServer>>, headers: HeaderMap) -> Response {
    if !is_loopback_host(&headers, state.port) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let origin = headers
        .get(header::ORIGIN)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    if !allowed_origin(&origin) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let mut res = StatusCode::NO_CONTENT.into_response();
    res.headers_mut().extend(cors_headers(&headers, &origin));
    res
}

async fn serve_file(
    State(state): State<Arc<StreamServer>>,
    Path((token, session, path)): Path<(String, String, String)>,
    headers: HeaderMap,
) -> Response {
    if token != state.token {
        return StatusCode::NOT_FOUND.into_response();
    }
    if !is_loopback_host(&headers, state.port) {
        return StatusCode::FORBIDDEN.into_response();
    }

    let origin = headers
        .get(header::ORIGIN)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    if !origin.is_empty() && !allowed_origin(&origin) {
        return StatusCode::FORBIDDEN.into_response();
    }

    if !allowed_extension(&path) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let Some(relative) = safe_relative_path(&path) else {
        return StatusCode::BAD_REQUEST.into_response();
    };

    let Some(dir) = state
        .sessions
        .read()
        .ok()
        .and_then(|map| map.get(&session).cloned())
    else {
        return StatusCode::NOT_FOUND.into_response();
    };

    let full = dir.join(relative);
    let bytes = match tokio::fs::read(&full).await {
        Ok(bytes) => bytes,
        Err(_) => return StatusCode::NOT_FOUND.into_response(),
    };

    let mutable_playlist = relative.ends_with(".m3u8");
    let cache_control = if mutable_playlist {
        "no-store"
    } else {
        "public, max-age=30"
    };

    let mut res = Response::new(Body::from(bytes));
    let h = res.headers_mut();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type(relative)));
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static(cache_control));
    h.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    if !origin.is_empty() {
        h.extend(cors_headers(&headers, &origin));
    }
    res
}
