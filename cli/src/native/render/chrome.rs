//! Chrome-backed renderer: loads a serialized document at its original URL
//! in an isolated browser context and captures it with the regular screenshot
//! pipeline (clip, full page, JPEG, annotations).

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use serde_json::{json, Value};
use tokio::sync::{mpsc, Semaphore};
use tokio::time::Instant;

use super::{RenderRequest, RenderResponse, REF_ATTR, TARGET_ATTR};
use crate::native::cdp::chrome::{launch_chrome, ChromeProcess, LaunchOptions};
use crate::native::cdp::client::CdpClient;
use crate::native::cdp::types::CdpEvent;
use crate::native::element::RefMap;
use crate::native::screenshot::{self, ScreenshotOptions};

const DEFAULT_LOAD_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_LOAD_TIMEOUT: Duration = Duration::from_secs(60);
/// Grace period for late layout (web fonts, images) after the load event.
const SETTLE_DELAY: Duration = Duration::from_millis(100);

pub struct ChromeRenderer {
    process: ChromeProcess,
    client: Arc<CdpClient>,
    slots: Arc<Semaphore>,
}

impl ChromeRenderer {
    /// Launches a dedicated headless Chrome. `executable_path` overrides discovery.
    pub async fn launch(executable_path: Option<String>) -> Result<Self, String> {
        Self::launch_with_concurrency(executable_path, 1).await
    }

    pub async fn launch_with_concurrency(
        executable_path: Option<String>,
        concurrency: usize,
    ) -> Result<Self, String> {
        let options = LaunchOptions {
            headless: true,
            executable_path,
            hide_scrollbars: true,
            webmcp: false,
            ..LaunchOptions::default()
        };
        let process = tokio::task::spawn_blocking(move || launch_chrome(&options))
            .await
            .map_err(|e| format!("Renderer Chrome launch task failed: {}", e))?
            .map_err(|e| format!("Failed to launch Chrome for screenshot rendering: {}", e))?;
        let client = Arc::new(CdpClient::connect(&process.ws_url).await?);
        Ok(Self {
            process,
            client,
            slots: Arc::new(Semaphore::new(concurrency.max(1))),
        })
    }

    pub fn is_alive(&mut self) -> bool {
        !self.process.has_exited()
    }

    pub async fn shutdown(&mut self) {
        let _ = self
            .client
            .send_command_no_params("Browser.close", None)
            .await;
        self.client.close().await;
        self.process.kill();
    }

    pub async fn render(&self, request: &RenderRequest) -> Result<RenderResponse, String> {
        let _slot = self
            .slots
            .acquire()
            .await
            .map_err(|_| "Renderer is shutting down".to_string())?;

        let context = self
            .client
            .send_command(
                "Target.createBrowserContext",
                Some(json!({ "disposeOnDetach": true })),
                None,
            )
            .await?;
        let context_id = context
            .get("browserContextId")
            .and_then(|v| v.as_str())
            .ok_or("Target.createBrowserContext returned no id")?
            .to_string();

        let result = self.render_in_context(&context_id, request).await;

        let _ = self
            .client
            .send_command(
                "Target.disposeBrowserContext",
                Some(json!({ "browserContextId": context_id })),
                None,
            )
            .await;
        result
    }

    async fn render_in_context(
        &self,
        context_id: &str,
        request: &RenderRequest,
    ) -> Result<RenderResponse, String> {
        let client = &self.client;
        let target = client
            .send_command(
                "Target.createTarget",
                Some(json!({ "url": "about:blank", "browserContextId": context_id })),
                None,
            )
            .await?;
        let target_id = target
            .get("targetId")
            .and_then(|v| v.as_str())
            .ok_or("Target.createTarget returned no targetId")?;
        let attached = client
            .send_command(
                "Target.attachToTarget",
                Some(json!({ "targetId": target_id, "flatten": true })),
                None,
            )
            .await?;
        let session_id = attached
            .get("sessionId")
            .and_then(|v| v.as_str())
            .ok_or("Target.attachToTarget returned no sessionId")?
            .to_string();

        let mut events = client.subscribe_session(&session_id);
        let result = self.render_page(&session_id, &mut events, request).await;
        client.unsubscribe_session(&session_id);
        result
    }

    async fn render_page(
        &self,
        session_id: &str,
        events: &mut mpsc::Receiver<CdpEvent>,
        request: &RenderRequest,
    ) -> Result<RenderResponse, String> {
        let client = &self.client;
        let sid = Some(session_id);
        client.send_command_no_params("Page.enable", sid).await?;
        client
            .send_command(
                "Emulation.setDeviceMetricsOverride",
                Some(json!({
                    "width": request.viewport.width.clamp(1, 16_384),
                    "height": request.viewport.height.clamp(1, 16_384),
                    "deviceScaleFactor": request.viewport.device_scale_factor.clamp(0.1, 4.0),
                    "mobile": false,
                })),
                sid,
            )
            .await?;

        // Match the source page instead of the renderer host's OS theme.
        let color_scheme = match request.color_scheme.as_deref() {
            Some("dark") => "dark",
            _ => "light",
        };
        let _ = client
            .send_command(
                "Emulation.setEmulatedMedia",
                Some(json!({
                    "features": [{ "name": "prefers-color-scheme", "value": color_scheme }]
                })),
                sid,
            )
            .await;

        let cookies = cookie_params(&request.cookies);
        if !cookies.is_empty() {
            // A cookie Chrome rejects must not fail the whole render.
            let _ = client
                .send_command(
                    "Network.setCookies",
                    Some(json!({ "cookies": cookies })),
                    sid,
                )
                .await;
        }

        let timeout = request
            .timeout_ms
            .map(Duration::from_millis)
            .unwrap_or(DEFAULT_LOAD_TIMEOUT)
            .min(MAX_LOAD_TIMEOUT);

        if is_http_url(&request.url) {
            self.load_at_url(session_id, events, request, timeout)
                .await?;
        } else {
            self.load_as_content(session_id, events, request, timeout)
                .await?;
        }
        tokio::time::sleep(SETTLE_DELAY).await;

        if request.output == "pdf" {
            let mut params = request
                .pdf
                .clone()
                .filter(|p| p.is_object())
                .unwrap_or_else(|| json!({}));
            // The response carries the PDF inline; streams cannot cross the wire.
            params["transferMode"] = json!("ReturnAsBase64");
            let pdf = client
                .send_command("Page.printToPDF", Some(params), sid)
                .await?;
            let data = pdf
                .get("data")
                .and_then(|v| v.as_str())
                .ok_or("Renderer returned no PDF data")?;
            return Ok(RenderResponse {
                data: data.to_string(),
                annotations: Vec::new(),
            });
        }

        if !request.full_page && (request.scroll.x != 0.0 || request.scroll.y != 0.0) {
            let _ = client
                .send_command(
                    "Runtime.evaluate",
                    Some(json!({
                        "expression": format!("window.scrollTo({}, {})", request.scroll.x, request.scroll.y),
                    })),
                    sid,
                )
                .await;
        }

        let ref_map = if request.annotate {
            build_ref_map(client, session_id, request).await
        } else {
            RefMap::new()
        };

        let options = ScreenshotOptions {
            selector: request.target.then(|| format!("[{}]", TARGET_ATTR)),
            path: None,
            full_page: request.full_page,
            format: if request.format == "jpeg" {
                "jpeg".to_string()
            } else {
                "png".to_string()
            },
            quality: request.quality,
            annotate: request.annotate,
            output_dir: None,
        };
        let shot =
            screenshot::take_screenshot(client, session_id, &ref_map, &options, &HashMap::new())
                .await?;
        let annotations = shot
            .annotations
            .iter()
            .filter_map(|a| serde_json::to_value(a).ok())
            .collect();
        Ok(RenderResponse {
            data: shot.base64,
            annotations,
        })
    }

    /// Serves the HTML at the original URL by intercepting the main document
    /// request, so relative URLs, cookies and same-origin rules all match.
    async fn load_at_url(
        &self,
        session_id: &str,
        events: &mut mpsc::Receiver<CdpEvent>,
        request: &RenderRequest,
        timeout: Duration,
    ) -> Result<(), String> {
        let client = &self.client;
        let sid = Some(session_id);
        client
            .send_command(
                "Fetch.enable",
                Some(json!({
                    "patterns": [{ "urlPattern": "*", "resourceType": "Document", "requestStage": "Request" }]
                })),
                sid,
            )
            .await?;

        let body = base64::engine::general_purpose::STANDARD.encode(request.html.as_bytes());
        let deadline = Instant::now() + timeout;
        let navigate =
            client.send_command("Page.navigate", Some(json!({ "url": request.url })), sid);
        tokio::pin!(navigate);

        let mut navigated = false;
        let mut served_main = false;
        let mut loaded = false;
        while !(navigated && loaded) {
            tokio::select! {
                result = &mut navigate, if !navigated => {
                    let result = result?;
                    if let Some(error) = result.get("errorText").and_then(|v| v.as_str()) {
                        return Err(format!("Renderer failed to load {}: {}", request.url, error));
                    }
                    navigated = true;
                }
                event = events.recv() => {
                    let Some(event) = event else {
                        return Err("Renderer page closed while loading".to_string());
                    };
                    match event.method.as_str() {
                        "Fetch.requestPaused" => {
                            let request_id = event.params.get("requestId").and_then(|v| v.as_str()).unwrap_or_default();
                            if !served_main {
                                served_main = true;
                                client.send_command(
                                    "Fetch.fulfillRequest",
                                    Some(json!({
                                        "requestId": request_id,
                                        "responseCode": 200,
                                        "responseHeaders": [{ "name": "Content-Type", "value": "text/html; charset=utf-8" }],
                                        "body": body,
                                    })),
                                    sid,
                                ).await?;
                            } else {
                                // Nested documents (iframes) load from the network.
                                let _ = client.send_command(
                                    "Fetch.continueRequest",
                                    Some(json!({ "requestId": request_id })),
                                    sid,
                                ).await;
                            }
                        }
                        "Page.loadEventFired" => loaded = true,
                        _ => {}
                    }
                }
                _ = tokio::time::sleep_until(deadline) => {
                    if navigated {
                        // Slow subresources: capture what has rendered so far.
                        break;
                    }
                    return Err(format!("Renderer timed out loading {}", request.url));
                }
            }
        }
        let _ = client.send_command_no_params("Fetch.disable", sid).await;
        Ok(())
    }

    /// For non-HTTP pages (about:blank, data:, file:) there is no origin to
    /// preserve, so the document is written directly into the blank page.
    async fn load_as_content(
        &self,
        session_id: &str,
        events: &mut mpsc::Receiver<CdpEvent>,
        request: &RenderRequest,
        timeout: Duration,
    ) -> Result<(), String> {
        let client = &self.client;
        let sid = Some(session_id);
        let tree = client
            .send_command_no_params("Page.getFrameTree", sid)
            .await?;
        let frame_id = tree
            .get("frameTree")
            .and_then(|t| t.get("frame"))
            .and_then(|f| f.get("id"))
            .and_then(|v| v.as_str())
            .ok_or("Renderer page has no main frame")?;
        client
            .send_command(
                "Page.setDocumentContent",
                Some(json!({ "frameId": frame_id, "html": request.html })),
                sid,
            )
            .await?;
        // setDocumentContent does not always emit a load event; poll readiness.
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            while events.try_recv().is_ok() {}
            let state = client
                .send_command(
                    "Runtime.evaluate",
                    Some(json!({ "expression": "document.readyState", "returnByValue": true })),
                    sid,
                )
                .await?;
            if state.pointer("/result/value").and_then(|v| v.as_str()) == Some("complete") {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        Ok(())
    }
}

/// Rebuilds the snapshot refs inside the renderer page from [`REF_ATTR`] markers.
async fn build_ref_map(client: &CdpClient, session_id: &str, request: &RenderRequest) -> RefMap {
    let mut ref_map = RefMap::new();
    let sid = Some(session_id);
    let roles: HashMap<&str, &super::RenderRef> = request
        .refs
        .iter()
        .map(|r| (r.ref_id.as_str(), r))
        .collect();

    let Ok(doc) = client
        .send_command("DOM.getDocument", Some(json!({ "depth": 0 })), sid)
        .await
    else {
        return ref_map;
    };
    let Some(root) = doc.pointer("/root/nodeId").and_then(|v| v.as_i64()) else {
        return ref_map;
    };
    let Ok(found) = client
        .send_command(
            "DOM.querySelectorAll",
            Some(json!({ "nodeId": root, "selector": format!("[{}]", REF_ATTR) })),
            sid,
        )
        .await
    else {
        return ref_map;
    };
    let node_ids: Vec<i64> = found
        .get("nodeIds")
        .and_then(|v| v.as_array())
        .map(|ids| ids.iter().filter_map(|v| v.as_i64()).collect())
        .unwrap_or_default();

    let described = futures_util::future::join_all(node_ids.iter().map(|node_id| {
        client.send_command("DOM.describeNode", Some(json!({ "nodeId": node_id })), sid)
    }))
    .await;

    for node in described.into_iter().flatten() {
        let Some(node) = node.get("node") else {
            continue;
        };
        let Some(backend_id) = node.get("backendNodeId").and_then(|v| v.as_i64()) else {
            continue;
        };
        let Some(ref_id) = attribute(node, REF_ATTR) else {
            continue;
        };
        if let Some(info) = roles.get(ref_id.as_str()) {
            ref_map.add(
                ref_id.clone(),
                Some(backend_id),
                &info.role,
                &info.name,
                None,
            );
        }
    }
    ref_map
}

fn attribute(node: &Value, name: &str) -> Option<String> {
    let attrs = node.get("attributes")?.as_array()?;
    attrs
        .chunks(2)
        .find(|pair| pair.first().and_then(|v| v.as_str()) == Some(name))
        .and_then(|pair| pair.get(1))
        .and_then(|v| v.as_str())
        .map(String::from)
}

fn is_http_url(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    lower.starts_with("http://") || lower.starts_with("https://")
}

/// Maps CDP `Network.Cookie` objects to `Network.CookieParam`.
fn cookie_params(cookies: &[Value]) -> Vec<Value> {
    cookies
        .iter()
        .filter_map(|cookie| {
            let name = cookie.get("name")?.as_str()?;
            let value = cookie.get("value")?.as_str()?;
            let mut param = json!({ "name": name, "value": value });
            for key in ["domain", "path", "secure", "httpOnly", "sameSite"] {
                if let Some(v) = cookie.get(key).filter(|v| !v.is_null()) {
                    param[key] = v.clone();
                }
            }
            if let Some(expires) = cookie.get("expires").and_then(|v| v.as_f64()) {
                if expires > 0.0 {
                    param["expires"] = json!(expires);
                }
            }
            param.get("domain")?;
            Some(param)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cookie_params_keep_supported_fields_only() {
        let cookies = vec![
            json!({
                "name": "sid", "value": "abc", "domain": ".example.com", "path": "/",
                "secure": true, "httpOnly": true, "sameSite": "Lax", "expires": -1,
                "size": 6, "session": true
            }),
            json!({ "name": "nodomain", "value": "x" }),
            json!({ "name": "exp", "value": "y", "domain": "a.test", "expires": 1900000000.0 }),
        ];
        let params = cookie_params(&cookies);
        assert_eq!(params.len(), 2);
        assert_eq!(params[0]["sameSite"], "Lax");
        assert!(params[0].get("expires").is_none());
        assert!(params[0].get("size").is_none());
        assert_eq!(params[1]["expires"], 1900000000.0);
    }

    #[test]
    fn reads_flat_attribute_pairs() {
        let node = json!({ "attributes": ["class", "x", REF_ATTR, "e7"] });
        assert_eq!(attribute(&node, REF_ATTR).as_deref(), Some("e7"));
        assert_eq!(attribute(&node, "id"), None);
    }

    #[test]
    fn only_http_urls_are_served_at_their_origin() {
        assert!(is_http_url("https://example.com"));
        assert!(is_http_url("HTTP://example.com"));
        assert!(!is_http_url("about:blank"));
        assert!(!is_http_url("data:text/html,hi"));
    }
}
