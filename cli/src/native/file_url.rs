//! `file://` pages on Lightpanda.
//!
//! Lightpanda refuses every scheme but http(s), so a `file://` navigation is
//! rewritten to a reserved host, `http://file.agent-browser.invalid/<path>`,
//! and the daemon answers that host's requests from disk through CDP `Fetch`.
//! The `.invalid` TLD never resolves, so nothing leaves the machine even if
//! interception is off.
//!
//! Every file of every opened folder shares that one origin, so a page could
//! `fetch()` any other file the host serves. Requests are therefore limited
//! to the folders of the files the session opened explicitly (and their
//! subfolders, after resolving `..` and symlinks); `--allow-file-access`
//! lifts the limit, as it lets Chrome's `file://` pages read other files.
//!
//! The page itself sees the http URL (`location.protocol` is `http:`); the
//! CLI shows the `file://` form in `open`, `get url`, and `tab list`.

use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use base64::Engine as _;
use serde_json::{json, Value};

use super::cdp::client::CdpClient;

/// Reserved host that stands in for `file://` on Lightpanda.
pub const HOST: &str = "file.agent-browser.invalid";
const ORIGIN: &str = "http://file.agent-browser.invalid";
/// Larger files are refused rather than base64-encoded into one CDP message.
const MAX_FILE_BYTES: u64 = 64 * 1024 * 1024;

/// Folders the session may serve, shared with the `Fetch` handler and the
/// screenshot renderer.
#[derive(Clone, Default)]
pub struct FileUrls {
    inner: Arc<RwLock<Mounts>>,
}

#[derive(Default)]
struct Mounts {
    roots: Vec<PathBuf>,
    allow_any: bool,
}

impl std::fmt::Debug for FileUrls {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mounts = self.read();
        f.debug_struct("FileUrls")
            .field("roots", &mounts.roots)
            .field("allow_any", &mounts.allow_any)
            .finish()
    }
}

/// What the reserved host answers for one request.
#[derive(Debug, PartialEq, Eq)]
pub struct FileResponse {
    pub status: u16,
    pub content_type: &'static str,
    pub body: Vec<u8>,
}

impl FileUrls {
    fn read(&self) -> std::sync::RwLockReadGuard<'_, Mounts> {
        self.inner.read().unwrap_or_else(|e| e.into_inner())
    }

    fn write(&self) -> std::sync::RwLockWriteGuard<'_, Mounts> {
        self.inner.write().unwrap_or_else(|e| e.into_inner())
    }

    /// `--allow-file-access`: serve any path, not only opened folders.
    pub fn set_allow_any(&self, allow: bool) {
        self.write().allow_any = allow;
    }

    /// Forgets the opened folders, when the session's browser closes.
    pub fn clear(&self) {
        self.write().roots.clear();
    }

    /// A file was opened, so the reserved host needs interception.
    pub fn is_active(&self) -> bool {
        !self.read().roots.is_empty()
    }

    /// Allows the folder of `file_url` (or the folder itself) and returns the
    /// reserved-host URL to navigate to. Fails when the file does not exist.
    pub fn mount(&self, file_url: &str) -> Result<String, String> {
        let parsed = url::Url::parse(file_url).map_err(|e| format!("Invalid file URL: {}", e))?;
        let path = parsed
            .to_file_path()
            .map_err(|_| format!("Unsupported file URL (only local paths): {}", file_url))?;
        let canonical = std::fs::canonicalize(&path)
            .map_err(|_| format!("File not found: {}", path.display()))?;
        let root = if canonical.is_dir() {
            canonical
        } else {
            canonical
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or(canonical)
        };
        let mut mounts = self.write();
        if !mounts.roots.contains(&root) {
            mounts.roots.push(root);
        }
        to_reserved_url(file_url).ok_or_else(|| format!("Unsupported file URL: {}", file_url))
    }

    /// Reads the file a reserved-host URL names, within the allowed folders.
    pub async fn resolve(&self, url: &str) -> FileResponse {
        let Some(path) = to_file_url(url)
            .and_then(|file| url::Url::parse(&file).ok())
            .and_then(|file| file.to_file_path().ok())
        else {
            return error_response(400, "Bad request");
        };
        let Ok(canonical) = tokio::fs::canonicalize(&path).await else {
            return error_response(404, &format!("File not found: {}", path.display()));
        };
        let allowed = {
            let mounts = self.read();
            mounts.allow_any || mounts.roots.iter().any(|root| canonical.starts_with(root))
        };
        if !allowed {
            return error_response(
                403,
                &format!(
                    "{} is outside the folders opened in this session. Open a file in that folder, or launch with --allow-file-access.",
                    path.display()
                ),
            );
        }
        if canonical.is_dir() {
            return directory_listing(&canonical).await;
        }
        match tokio::fs::metadata(&canonical).await {
            Ok(meta) if meta.len() > MAX_FILE_BYTES => {
                return error_response(413, &format!("{} is larger than 64 MB", path.display()))
            }
            Ok(_) => {}
            Err(_) => return error_response(404, &format!("File not found: {}", path.display())),
        }
        match tokio::fs::read(&canonical).await {
            Ok(body) => FileResponse {
                status: 200,
                content_type: content_type(&canonical),
                body,
            },
            Err(e) => error_response(403, &format!("Cannot read {}: {}", path.display(), e)),
        }
    }

    /// Answers a paused request for the reserved host from disk. Returns
    /// false, leaving the request alone, for any other URL.
    pub async fn serve(
        &self,
        client: &CdpClient,
        session_id: &str,
        request_id: &str,
        url: &str,
    ) -> bool {
        if !is_reserved_url(url) {
            return false;
        }
        let response = self.resolve(url).await;
        let _ = client
            .send_command(
                "Fetch.fulfillRequest",
                Some(json!({
                    "requestId": request_id,
                    "responseCode": response.status,
                    "responseHeaders": [
                        { "name": "Content-Type", "value": response.content_type },
                    ],
                    "body": base64::engine::general_purpose::STANDARD.encode(&response.body),
                })),
                Some(session_id),
            )
            .await;
        true
    }
}

/// `Fetch.enable` pattern for the reserved host.
pub fn fetch_pattern() -> Value {
    json!({ "urlPattern": format!("{}/*", ORIGIN) })
}

pub fn is_file_url(url: &str) -> bool {
    url.get(..5)
        .is_some_and(|scheme| scheme.eq_ignore_ascii_case("file:"))
}

fn is_reserved_url(url: &str) -> bool {
    url::Url::parse(url)
        .is_ok_and(|u| u.scheme() == "http" && u.host_str() == Some(HOST) && u.port().is_none())
}

/// `file:///a/b.html?q#f` -> `http://file.agent-browser.invalid/a/b.html?q#f`.
pub fn to_reserved_url(file_url: &str) -> Option<String> {
    let parsed = url::Url::parse(file_url).ok()?;
    if parsed.scheme() != "file" || parsed.host_str().is_some_and(|h| h != "localhost") {
        return None;
    }
    Some(format!("{}{}", ORIGIN, path_query_fragment(&parsed)))
}

/// The inverse of [`to_reserved_url`]; `None` for any other URL.
pub fn to_file_url(url: &str) -> Option<String> {
    let parsed = url::Url::parse(url).ok()?;
    is_reserved_url(url).then(|| format!("file://{}", path_query_fragment(&parsed)))
}

/// The URL to show the user: the `file://` form of a reserved-host URL.
pub fn display_url(url: &str) -> String {
    to_file_url(url).unwrap_or_else(|| url.to_string())
}

/// Rewrites the `url` field of a response (and of each entry in `tabs`).
pub fn display_urls_in(value: &mut Value) {
    if let Some(url) = value.get("url").and_then(Value::as_str) {
        if let Some(file) = to_file_url(url) {
            value["url"] = json!(file);
        }
    }
    if let Some(tabs) = value.get_mut("tabs").and_then(Value::as_array_mut) {
        tabs.iter_mut().for_each(display_urls_in);
    }
}

fn path_query_fragment(url: &url::Url) -> String {
    let mut out = url.path().to_string();
    if let Some(query) = url.query() {
        out.push('?');
        out.push_str(query);
    }
    if let Some(fragment) = url.fragment() {
        out.push('#');
        out.push_str(fragment);
    }
    out
}

fn error_response(status: u16, message: &str) -> FileResponse {
    FileResponse {
        status,
        content_type: "text/html; charset=utf-8",
        body: format!(
            "<!doctype html><html><head><title>{status}</title></head><body><h1>{status}</h1><p>{}</p></body></html>",
            html_escape(message)
        )
        .into_bytes(),
    }
}

async fn directory_listing(dir: &Path) -> FileResponse {
    let mut names = Vec::new();
    if let Ok(mut entries) = tokio::fs::read_dir(dir).await {
        while let Ok(Some(entry)) = entries.next_entry().await {
            let is_dir = entry.file_type().await.is_ok_and(|t| t.is_dir());
            names.push((entry.file_name().to_string_lossy().into_owned(), is_dir));
        }
    }
    names.sort();
    let base = url::Url::from_directory_path(dir)
        .ok()
        .and_then(|u| to_reserved_url(u.as_str()))
        .unwrap_or_else(|| format!("{}/", ORIGIN));
    let items: String = names
        .iter()
        .map(|(name, is_dir)| {
            let slash = if *is_dir { "/" } else { "" };
            format!(
                "<li><a href=\"{base}{}{slash}\">{}{slash}</a></li>",
                urlencoding::encode(name),
                html_escape(name)
            )
        })
        .collect();
    let title = html_escape(&dir.display().to_string());
    FileResponse {
        status: 200,
        content_type: "text/html; charset=utf-8",
        body: format!(
            "<!doctype html><html><head><title>Index of {title}</title></head><body><h1>Index of {title}</h1><ul>{items}</ul></body></html>"
        )
        .into_bytes(),
    }
}

fn html_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn content_type(path: &Path) -> &'static str {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    match ext.as_str() {
        "html" | "htm" | "xhtml" => "text/html; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "js" | "mjs" | "cjs" => "text/javascript; charset=utf-8",
        "json" | "map" => "application/json",
        "txt" | "md" | "log" | "csv" => "text/plain; charset=utf-8",
        "xml" => "application/xml",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "avif" => "image/avif",
        "ico" => "image/x-icon",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "otf" => "font/otf",
        "wasm" => "application/wasm",
        "pdf" => "application/pdf",
        "mp4" => "video/mp4",
        "webm" => "video/webm",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_round_trip() {
        let file = "file:///tmp/a%20b/page.html?x=1#top";
        let reserved = to_reserved_url(file).unwrap();
        assert_eq!(
            reserved,
            "http://file.agent-browser.invalid/tmp/a%20b/page.html?x=1#top"
        );
        assert_eq!(to_file_url(&reserved).as_deref(), Some(file));
        assert_eq!(display_url(&reserved), file);
        assert_eq!(display_url("https://example.com/"), "https://example.com/");
        assert_eq!(
            to_reserved_url("file://localhost/tmp/x.html").as_deref(),
            Some("http://file.agent-browser.invalid/tmp/x.html")
        );
    }

    #[test]
    fn rejects_other_urls() {
        assert!(to_reserved_url("file://server/share/x.html").is_none());
        assert!(to_reserved_url("https://example.com/").is_none());
        assert!(to_file_url("http://file.agent-browser.invalid:8080/x").is_none());
        assert!(to_file_url("https://file.agent-browser.invalid/x").is_none());
        assert!(to_file_url("http://example.com/x").is_none());
        assert!(is_file_url("FILE:///x"));
        assert!(!is_file_url("https://x"));
    }

    #[test]
    fn display_rewrites_tabs() {
        let mut value = json!({
            "tabs": [
                { "url": "http://file.agent-browser.invalid/tmp/a.html" },
                { "url": "https://example.com/" },
            ]
        });
        display_urls_in(&mut value);
        assert_eq!(value["tabs"][0]["url"], "file:///tmp/a.html");
        assert_eq!(value["tabs"][1]["url"], "https://example.com/");
    }

    fn reserved(path: &Path) -> String {
        to_reserved_url(url::Url::from_file_path(path).unwrap().as_str()).unwrap()
    }

    #[tokio::test]
    async fn serves_only_opened_folders() {
        let dir = tempfile::tempdir().unwrap();
        let site = dir.path().join("site");
        let other = dir.path().join("other");
        std::fs::create_dir_all(site.join("sub")).unwrap();
        std::fs::create_dir_all(&other).unwrap();
        std::fs::write(site.join("index.html"), "<h1>hi</h1>").unwrap();
        std::fs::write(site.join("sub/app.js"), "1").unwrap();
        std::fs::write(other.join("secret.txt"), "secret").unwrap();

        let files = FileUrls::default();
        assert!(!files.is_active());
        let index = url::Url::from_file_path(site.join("index.html")).unwrap();
        let reserved_index = files.mount(index.as_str()).unwrap();
        assert!(files.is_active());

        let page = files.resolve(&reserved_index).await;
        assert_eq!(page.status, 200);
        assert_eq!(page.content_type, "text/html; charset=utf-8");
        assert_eq!(page.body, b"<h1>hi</h1>");

        let script = files.resolve(&reserved(&site.join("sub/app.js"))).await;
        assert_eq!(script.status, 200);
        assert_eq!(script.content_type, "text/javascript; charset=utf-8");

        let outside = files.resolve(&reserved(&other.join("secret.txt"))).await;
        assert_eq!(outside.status, 403);

        // `..` in the URL resolves before the folder check.
        let dotted = format!(
            "{}/../other/secret.txt",
            reserved_index.trim_end_matches("/index.html")
        );
        assert_eq!(files.resolve(&dotted).await.status, 403);

        let missing = files.resolve(&reserved(&site.join("missing.css"))).await;
        assert_eq!(missing.status, 404);

        files.set_allow_any(true);
        let allowed = files.resolve(&reserved(&other.join("secret.txt"))).await;
        assert_eq!(allowed.status, 200);
        assert_eq!(allowed.body, b"secret");

        files.clear();
        assert!(!files.is_active());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn symlinks_cannot_escape_the_folder() {
        let dir = tempfile::tempdir().unwrap();
        let site = dir.path().join("site");
        std::fs::create_dir_all(&site).unwrap();
        std::fs::write(site.join("index.html"), "x").unwrap();
        std::fs::write(dir.path().join("secret.txt"), "secret").unwrap();
        std::os::unix::fs::symlink(dir.path().join("secret.txt"), site.join("link.txt")).unwrap();

        let files = FileUrls::default();
        files
            .mount(
                url::Url::from_file_path(site.join("index.html"))
                    .unwrap()
                    .as_str(),
            )
            .unwrap();
        assert_eq!(
            files
                .resolve(&reserved(&site.join("link.txt")))
                .await
                .status,
            403
        );
    }

    #[tokio::test]
    async fn lists_directories() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("sub dir")).unwrap();
        std::fs::write(dir.path().join("a.html"), "x").unwrap();
        let files = FileUrls::default();
        let root = url::Url::from_directory_path(dir.path()).unwrap();
        let reserved_root = files.mount(root.as_str()).unwrap();
        let listing = files.resolve(&reserved_root).await;
        assert_eq!(listing.status, 200);
        let html = String::from_utf8(listing.body).unwrap();
        assert!(html.contains("a.html"), "{html}");
        assert!(html.contains("sub%20dir/"), "{html}");
    }

    #[test]
    fn mount_fails_for_missing_files() {
        let files = FileUrls::default();
        let err = files
            .mount("file:///definitely/not/here/agent-browser.html")
            .unwrap_err();
        assert!(err.contains("File not found"), "{err}");
        assert!(!files.is_active());
    }
}
