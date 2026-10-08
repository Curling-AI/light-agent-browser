//! `agent-browser renderer serve`: a standalone HTTP service that renders
//! [`RenderRequest`]s with a shared headless Chrome, so a fleet of Lightpanda
//! agents can take visual screenshots without bundling Chrome.
//!
//! Endpoints:
//! - `GET /healthz` → `{"ok": true}`
//! - `POST /v1/render` → [`RenderResponse`] (bearer token required when configured)

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::RwLock;

use super::chrome::ChromeRenderer;
use super::guard::RenderPolicy;
use super::RenderRequest;

const MAX_HEADER_BYTES: usize = 64 * 1024;
const DEFAULT_MAX_BODY_BYTES: usize = 32 * 1024 * 1024;
const READ_TIMEOUT: Duration = Duration::from_secs(30);
const DEFAULT_RECYCLE_AFTER: u64 = 200;
/// Largest viewport side a caller may ask for. Bounds the memory one request
/// can make Chrome allocate for a capture.
const MAX_VIEWPORT_SIDE: u32 = 4096;
const MAX_DEVICE_SCALE_FACTOR: f64 = 2.0;

#[derive(Debug, Clone)]
pub struct ServeOptions {
    pub host: String,
    pub port: u16,
    pub token: Option<String>,
    pub concurrency: usize,
    pub executable_path: Option<String>,
    pub max_body_bytes: usize,
    /// Relaunch Chrome after this many renders, so a long-lived process
    /// never accumulates state from earlier callers.
    pub recycle_after: u64,
    /// Listen on a non-loopback address without a token. Off by default:
    /// requests carry page cookies.
    pub allow_unauthenticated: bool,
}

impl Default for ServeOptions {
    fn default() -> Self {
        Self {
            host: "127.0.0.1".to_string(),
            port: 9300,
            token: None,
            concurrency: 4,
            executable_path: None,
            max_body_bytes: DEFAULT_MAX_BODY_BYTES,
            recycle_after: DEFAULT_RECYCLE_AFTER,
            allow_unauthenticated: false,
        }
    }
}

/// Parses `renderer serve` arguments. Unset values fall back to
/// `AGENT_BROWSER_RENDERER_*` env vars, then to defaults.
pub fn parse_serve_args(args: &[String]) -> Result<ServeOptions, String> {
    let mut options = ServeOptions::default();
    if let Ok(host) = std::env::var("AGENT_BROWSER_RENDERER_HOST") {
        options.host = host;
    }
    if let Ok(port) = std::env::var("AGENT_BROWSER_RENDERER_PORT") {
        options.port = parse_number(&port, "AGENT_BROWSER_RENDERER_PORT")?;
    }
    if let Ok(n) = std::env::var("AGENT_BROWSER_RENDERER_CONCURRENCY") {
        options.concurrency = parse_number(&n, "AGENT_BROWSER_RENDERER_CONCURRENCY")?;
    }
    if let Ok(n) = std::env::var("AGENT_BROWSER_RENDERER_RECYCLE_AFTER") {
        options.recycle_after = parse_number(&n, "AGENT_BROWSER_RENDERER_RECYCLE_AFTER")?;
    }
    options.token = std::env::var(super::RENDERER_TOKEN_ENV)
        .ok()
        .filter(|t| !t.is_empty());
    options.executable_path = std::env::var("AGENT_BROWSER_EXECUTABLE_PATH")
        .ok()
        .filter(|p| !p.is_empty());

    let mut i = 0;
    while i < args.len() {
        let flag = args[i].as_str();
        if flag == "--allow-unauthenticated" {
            options.allow_unauthenticated = true;
            i += 1;
            continue;
        }
        let value = || {
            args.get(i + 1)
                .cloned()
                .ok_or_else(|| format!("{} requires a value", flag))
        };
        match flag {
            "--host" => options.host = value()?,
            "--port" => options.port = parse_number(&value()?, "--port")?,
            "--concurrency" => options.concurrency = parse_number(&value()?, "--concurrency")?,
            "--executable-path" => options.executable_path = Some(value()?),
            "--recycle-after" => {
                options.recycle_after = parse_number(&value()?, "--recycle-after")?
            }
            "--max-body-mb" => {
                let mb: usize = parse_number(&value()?, "--max-body-mb")?;
                options.max_body_bytes = mb.max(1) * 1024 * 1024;
            }
            other => return Err(format!("Unknown renderer serve option: {}", other)),
        }
        i += 2;
    }
    if options.concurrency == 0 {
        return Err("--concurrency must be at least 1".to_string());
    }
    if options.recycle_after == 0 {
        return Err("--recycle-after must be at least 1".to_string());
    }
    Ok(options)
}

fn parse_number<T: std::str::FromStr>(value: &str, name: &str) -> Result<T, String> {
    value
        .trim()
        .parse()
        .map_err(|_| format!("{} must be a number, got '{}'", name, value))
}

struct ServerState {
    renderer: RwLock<ChromeRenderer>,
    options: ServeOptions,
    renders: AtomicU64,
}

async fn launch(options: &ServeOptions) -> Result<ChromeRenderer, String> {
    ChromeRenderer::launch_with(
        options.executable_path.clone(),
        options.concurrency,
        RenderPolicy::SERVICE,
    )
    .await
}

pub async fn run(options: ServeOptions) -> Result<(), String> {
    if options.token.is_none() && !is_loopback(&options.host) && !options.allow_unauthenticated {
        return Err(format!(
            "Refusing to serve on {} without a token: render requests carry page cookies. Set {} or pass --allow-unauthenticated",
            options.host,
            super::RENDERER_TOKEN_ENV
        ));
    }
    let renderer = launch(&options).await?;
    let listener = TcpListener::bind((options.host.as_str(), options.port))
        .await
        .map_err(|e| format!("Failed to bind {}:{}: {}", options.host, options.port, e))?;
    let addr = listener
        .local_addr()
        .map_err(|e| format!("Failed to read listener address: {}", e))?;
    println!("agent-browser renderer listening on http://{}", addr);
    if options.token.is_none() && !is_loopback(&options.host) {
        eprintln!(
            "{} renderer is reachable from the network without a token; set {}",
            crate::color::warning_indicator(),
            super::RENDERER_TOKEN_ENV
        );
    }

    let state = Arc::new(ServerState {
        renderer: RwLock::new(renderer),
        options,
        renders: AtomicU64::new(0),
    });

    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let Ok((stream, _)) = accepted else { continue };
                let state = state.clone();
                tokio::spawn(async move {
                    let _ = handle_connection(stream, state).await;
                });
            }
            _ = tokio::signal::ctrl_c() => break,
        }
    }
    state.renderer.write().await.shutdown().await;
    Ok(())
}

fn is_loopback(host: &str) -> bool {
    matches!(host, "127.0.0.1" | "localhost" | "::1")
}

struct HttpRequest {
    method: String,
    path: String,
    authorization: Option<String>,
    body: Vec<u8>,
}

async fn handle_connection(mut stream: TcpStream, state: Arc<ServerState>) -> Result<(), String> {
    let request =
        match tokio::time::timeout(READ_TIMEOUT, read_request(&mut stream, &state.options)).await {
            Ok(Ok(request)) => request,
            Ok(Err((status, message))) => {
                return write_json(&mut stream, status, &json!({ "error": message })).await
            }
            Err(_) => {
                return write_json(&mut stream, 408, &json!({ "error": "Request timeout" })).await
            }
        };

    let path = request.path.split('?').next().unwrap_or("");
    match (request.method.as_str(), path) {
        ("GET", "/healthz") => write_json(&mut stream, 200, &json!({ "ok": true })).await,
        ("POST", "/v1/render") => {
            if !authorized(
                state.options.token.as_deref(),
                request.authorization.as_deref(),
            ) {
                return write_json(&mut stream, 401, &json!({ "error": "Unauthorized" })).await;
            }
            let render_request: RenderRequest = match serde_json::from_slice(&request.body) {
                Ok(r) => bounded(r),
                Err(e) => {
                    return write_json(
                        &mut stream,
                        400,
                        &json!({ "error": format!("Invalid render request: {}", e) }),
                    )
                    .await
                }
            };
            match render(&state, &render_request).await {
                Ok(response) => write_json(&mut stream, 200, &json!(response)).await,
                Err(e) => write_json(&mut stream, 500, &json!({ "error": e })).await,
            }
        }
        _ => write_json(&mut stream, 404, &json!({ "error": "Not found" })).await,
    }
}

/// Clamps what a caller may make Chrome allocate.
fn bounded(mut request: RenderRequest) -> RenderRequest {
    request.viewport.width = request.viewport.width.clamp(1, MAX_VIEWPORT_SIDE);
    request.viewport.height = request.viewport.height.clamp(1, MAX_VIEWPORT_SIDE);
    request.viewport.device_scale_factor = request
        .viewport
        .device_scale_factor
        .clamp(0.1, MAX_DEVICE_SCALE_FACTOR);
    request
}

/// Renders, relaunching Chrome once if it died underneath the request, and
/// recycling it every `recycle_after` renders.
async fn render(
    state: &ServerState,
    request: &RenderRequest,
) -> Result<super::RenderResponse, String> {
    let first = state.renderer.read().await.render(request).await;
    let count = state.renders.fetch_add(1, Ordering::Relaxed) + 1;
    if count.is_multiple_of(state.options.recycle_after) {
        // The write lock waits for renders in flight, so none is cut short.
        let mut renderer = state.renderer.write().await;
        renderer.shutdown().await;
        *renderer = launch(&state.options).await?;
    }
    if first.is_ok() {
        return first;
    }
    let mut renderer = state.renderer.write().await;
    if renderer.is_alive() {
        return first;
    }
    renderer.shutdown().await;
    *renderer = launch(&state.options).await?;
    let renderer = tokio::sync::RwLockWriteGuard::downgrade(renderer);
    renderer.render(request).await
}

fn authorized(token: Option<&str>, header: Option<&str>) -> bool {
    let Some(token) = token else {
        return true;
    };
    let Some(presented) = header.and_then(|h| {
        h.strip_prefix("Bearer ")
            .or_else(|| h.strip_prefix("bearer "))
    }) else {
        return false;
    };
    constant_time_eq(token.trim().as_bytes(), presented.trim().as_bytes())
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

async fn read_request(
    stream: &mut TcpStream,
    options: &ServeOptions,
) -> Result<HttpRequest, (u16, String)> {
    let mut buffer = Vec::with_capacity(8192);
    let mut chunk = [0u8; 8192];
    let header_end = loop {
        let n = stream
            .read(&mut chunk)
            .await
            .map_err(|e| (400, format!("Read failed: {}", e)))?;
        if n == 0 {
            return Err((400, "Connection closed before headers".to_string()));
        }
        buffer.extend_from_slice(&chunk[..n]);
        if let Some(pos) = find_header_end(&buffer) {
            break pos;
        }
        if buffer.len() > MAX_HEADER_BYTES {
            return Err((431, "Request headers too large".to_string()));
        }
    };

    let head = String::from_utf8_lossy(&buffer[..header_end]).to_string();
    let mut lines = head.split("\r\n");
    let mut request_line = lines.next().unwrap_or("").split_whitespace();
    let method = request_line.next().unwrap_or("").to_string();
    let path = request_line.next().unwrap_or("").to_string();

    let mut content_length = 0usize;
    let mut authorization = None;
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim();
        match name.trim().to_ascii_lowercase().as_str() {
            "content-length" => {
                content_length = value
                    .parse()
                    .map_err(|_| (400, "Invalid Content-Length".to_string()))?
            }
            "authorization" => authorization = Some(value.to_string()),
            "transfer-encoding" if !value.eq_ignore_ascii_case("identity") => {
                return Err((
                    411,
                    "Chunked bodies are not supported; send Content-Length".to_string(),
                ))
            }
            _ => {}
        }
    }
    if content_length > options.max_body_bytes {
        return Err((413, "Request body too large".to_string()));
    }

    let mut body = buffer[header_end + 4..].to_vec();
    while body.len() < content_length {
        let n = stream
            .read(&mut chunk)
            .await
            .map_err(|e| (400, format!("Read failed: {}", e)))?;
        if n == 0 {
            return Err((400, "Connection closed before body".to_string()));
        }
        body.extend_from_slice(&chunk[..n]);
    }
    body.truncate(content_length);

    Ok(HttpRequest {
        method,
        path,
        authorization,
        body,
    })
}

fn find_header_end(buffer: &[u8]) -> Option<usize> {
    buffer.windows(4).position(|w| w == b"\r\n\r\n")
}

async fn write_json(
    stream: &mut TcpStream,
    status: u16,
    body: &serde_json::Value,
) -> Result<(), String> {
    let payload = body.to_string();
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        404 => "Not Found",
        408 => "Request Timeout",
        411 => "Length Required",
        413 => "Payload Too Large",
        431 => "Request Header Fields Too Large",
        _ => "Internal Server Error",
    };
    let response = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        status,
        reason,
        payload.len(),
        payload
    );
    stream
        .write_all(response.as_bytes())
        .await
        .map_err(|e| e.to_string())?;
    let _ = stream.shutdown().await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn parses_serve_flags() {
        let options = parse_serve_args(&args(&[
            "--host",
            "0.0.0.0",
            "--port",
            "8080",
            "--concurrency",
            "8",
            "--max-body-mb",
            "4",
        ]))
        .unwrap();
        assert_eq!(options.host, "0.0.0.0");
        assert_eq!(options.port, 8080);
        assert_eq!(options.concurrency, 8);
        assert_eq!(options.max_body_bytes, 4 * 1024 * 1024);
        assert_eq!(options.recycle_after, DEFAULT_RECYCLE_AFTER);
        assert!(!options.allow_unauthenticated);

        let options =
            parse_serve_args(&args(&["--allow-unauthenticated", "--recycle-after", "50"])).unwrap();
        assert!(options.allow_unauthenticated);
        assert_eq!(options.recycle_after, 50);
    }

    #[tokio::test]
    async fn refuses_network_listener_without_token() {
        let options = ServeOptions {
            host: "0.0.0.0".to_string(),
            ..ServeOptions::default()
        };
        let err = run(options).await.unwrap_err();
        assert!(err.contains("without a token"), "{err}");
    }

    #[test]
    fn requests_are_bounded() {
        let request: RenderRequest = serde_json::from_value(serde_json::json!({
            "url": "https://example.com",
            "html": "<p>x</p>",
            "viewport": { "width": 100000, "height": 0, "deviceScaleFactor": 9.0 }
        }))
        .unwrap();
        let request = bounded(request);
        assert_eq!(request.viewport.width, MAX_VIEWPORT_SIDE);
        assert_eq!(request.viewport.height, 1);
        assert_eq!(
            request.viewport.device_scale_factor,
            MAX_DEVICE_SCALE_FACTOR
        );
    }

    #[test]
    fn rejects_bad_serve_flags() {
        assert!(parse_serve_args(&args(&["--port", "abc"])).is_err());
        assert!(parse_serve_args(&args(&["--port"])).is_err());
        assert!(parse_serve_args(&args(&["--concurrency", "0"])).is_err());
        assert!(parse_serve_args(&args(&["--recycle-after", "0"])).is_err());
        assert!(parse_serve_args(&args(&["--bogus", "1"])).is_err());
    }

    #[test]
    fn token_auth_requires_matching_bearer() {
        assert!(authorized(None, None));
        assert!(authorized(Some("s3cret"), Some("Bearer s3cret")));
        assert!(!authorized(Some("s3cret"), Some("Bearer nope")));
        assert!(!authorized(Some("s3cret"), Some("Basic s3cret")));
        assert!(!authorized(Some("s3cret"), None));
    }

    #[test]
    fn finds_header_terminator() {
        assert_eq!(find_header_end(b"GET / HTTP/1.1\r\n\r\nbody"), Some(14));
        assert_eq!(find_header_end(b"partial\r\n"), None);
    }
}
