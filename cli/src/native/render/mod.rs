//! Visual screenshots for engines without a layout engine (Lightpanda).
//!
//! Lightpanda's own `Page.captureScreenshot` only rasterizes page text. To get
//! a real screenshot the daemon serializes the live Lightpanda DOM (scripts
//! stripped, form state preserved), collects the page cookies and viewport,
//! and hands that [`RenderRequest`] to a Chrome-based renderer. The renderer
//! can run in-process (a lazily launched headless Chrome) or remotely as a
//! shared `agent-browser renderer serve` deployment.
//!
//! Renderer selection (`--screenshot-renderer`, `AGENT_BROWSER_SCREENSHOT_RENDERER`):
//! - `auto` (default): local Chrome when installed, otherwise Lightpanda's text render
//! - `chrome`: local Chrome, error when it is not installed
//! - `native`: Lightpanda's own text-only render
//! - `http(s)://host[:port]`: remote renderer service

pub mod chrome;
pub mod remote;
pub mod server;

use std::collections::HashMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::Mutex;

use super::cdp::client::CdpClient;
use super::element::{resolve_element_object_id, RefMap};
use super::screenshot::{self, ScreenshotAnnotation, ScreenshotOptions, ScreenshotResult};

pub const RENDERER_ENV: &str = "AGENT_BROWSER_SCREENSHOT_RENDERER";
pub const RENDERER_TOKEN_ENV: &str = "AGENT_BROWSER_RENDERER_TOKEN";

/// Marks the element a selector screenshot clips to.
pub(crate) const TARGET_ATTR: &str = "data-agent-browser-target";
/// Marks annotated elements with their snapshot ref (e.g. `e3`).
pub(crate) const REF_ATTR: &str = "data-agent-browser-ref";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RendererMode {
    Auto,
    Chrome,
    Native,
    Remote(String),
}

impl RendererMode {
    pub fn parse(value: &str) -> Result<Self, String> {
        let trimmed = value.trim();
        match trimmed.to_ascii_lowercase().as_str() {
            "" | "auto" => Ok(Self::Auto),
            "chrome" | "local" => Ok(Self::Chrome),
            "native" | "lightpanda" => Ok(Self::Native),
            lower if lower.starts_with("http://") || lower.starts_with("https://") => {
                Ok(Self::Remote(trimmed.trim_end_matches('/').to_string()))
            }
            _ => Err(format!(
                "Invalid screenshot renderer '{}'. Use auto, chrome, native, or an http(s):// renderer URL",
                trimmed
            )),
        }
    }

    /// Resolves the renderer from the command, falling back to the daemon env.
    pub fn from_command(cmd: &Value) -> Result<Self, String> {
        match cmd.get("renderer").and_then(|v| v.as_str()) {
            Some(value) => Self::parse(value),
            None => match std::env::var(RENDERER_ENV) {
                Ok(value) => Self::parse(&value),
                Err(_) => Ok(Self::Auto),
            },
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RenderViewport {
    pub width: u32,
    pub height: u32,
    #[serde(default = "default_scale")]
    pub device_scale_factor: f64,
}

fn default_scale() -> f64 {
    1.0
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct RenderScroll {
    pub x: f64,
    pub y: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RenderRef {
    #[serde(rename = "ref")]
    pub ref_id: String,
    pub role: String,
    #[serde(default)]
    pub name: String,
}

/// Wire format shared by the in-process and remote renderers (`POST /v1/render`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RenderRequest {
    /// Page URL; the HTML is served at this URL so relative resources and
    /// cookies resolve against the original origin.
    pub url: String,
    /// Serialized document, including the doctype.
    pub html: String,
    /// Cookies for `url`, in CDP `Network.Cookie` shape.
    #[serde(default)]
    pub cookies: Vec<Value>,
    pub viewport: RenderViewport,
    #[serde(default)]
    pub scroll: RenderScroll,
    /// `light` or `dark`, as the source page resolved `prefers-color-scheme`.
    #[serde(default)]
    pub color_scheme: Option<String>,
    #[serde(default)]
    pub full_page: bool,
    #[serde(default = "default_format")]
    pub format: String,
    #[serde(default)]
    pub quality: Option<i32>,
    /// Clip to the element carrying [`TARGET_ATTR`].
    #[serde(default)]
    pub target: bool,
    /// Draw numbered boxes over the elements carrying [`REF_ATTR`].
    #[serde(default)]
    pub annotate: bool,
    #[serde(default)]
    pub refs: Vec<RenderRef>,
    /// Upper bound for subresource loading, in milliseconds.
    #[serde(default)]
    pub timeout_ms: Option<u64>,
}

fn default_format() -> String {
    "png".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RenderResponse {
    /// Base64-encoded image.
    pub data: String,
    #[serde(default)]
    pub annotations: Vec<Value>,
}

/// Daemon-side renderer state. The local Chrome is launched on first use and
/// kept until the session closes.
#[derive(Clone, Default)]
pub struct ScreenshotRenderer {
    local: Arc<Mutex<Option<chrome::ChromeRenderer>>>,
}

impl ScreenshotRenderer {
    pub async fn shutdown(&self) {
        if let Some(mut renderer) = self.local.lock().await.take() {
            renderer.shutdown().await;
        }
    }

    async fn render_local(&self, request: &RenderRequest) -> Result<RenderResponse, String> {
        let mut guard = self.local.lock().await;
        if guard.as_mut().is_some_and(|r| !r.is_alive()) {
            if let Some(mut dead) = guard.take() {
                dead.shutdown().await;
            }
        }
        if guard.is_none() {
            *guard = Some(chrome::ChromeRenderer::launch(None).await?);
        }
        let renderer = guard.as_ref().ok_or("Renderer unavailable")?;
        renderer.render(request).await
    }
}

/// Which renderer produced a screenshot, reported in the command response.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderedBy {
    Engine,
    LightpandaText,
    LocalChrome,
    Remote,
}

impl RenderedBy {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Engine => "engine",
            Self::LightpandaText => "lightpanda-text",
            Self::LocalChrome => "chrome",
            Self::Remote => "remote",
        }
    }
}

pub struct CaptureContext<'a> {
    pub client: &'a CdpClient,
    pub session_id: &'a str,
    pub ref_map: &'a RefMap,
    pub iframe_sessions: &'a HashMap<String, String>,
    pub engine: &'a str,
    /// `--allowed-domains` is active. A Chrome renderer would fetch page
    /// subresources outside the filter, so only the native render is allowed.
    pub domain_filter_active: bool,
    /// Bearer token for a remote renderer: the command's `rendererToken`
    /// (forwarded by the CLI from its environment) or the daemon env.
    pub renderer_token: Option<String>,
}

/// Token for a remote renderer: the command's value wins over the daemon env,
/// so changing `AGENT_BROWSER_RENDERER_TOKEN` takes effect without a restart.
pub fn renderer_token_from_command(cmd: &Value) -> Option<String> {
    cmd.get("rendererToken")
        .and_then(|v| v.as_str())
        .map(String::from)
        .or_else(|| std::env::var(RENDERER_TOKEN_ENV).ok())
        .filter(|t| !t.is_empty())
}

/// Takes a screenshot, routing Lightpanda sessions through a renderer.
pub async fn capture_screenshot(
    renderer: &ScreenshotRenderer,
    ctx: &CaptureContext<'_>,
    options: &ScreenshotOptions,
    mode: &RendererMode,
) -> Result<(ScreenshotResult, RenderedBy), String> {
    let native = || async {
        screenshot::take_screenshot(
            ctx.client,
            ctx.session_id,
            ctx.ref_map,
            options,
            ctx.iframe_sessions,
        )
        .await
    };

    if !ctx.engine.eq_ignore_ascii_case("lightpanda") {
        return Ok((native().await?, RenderedBy::Engine));
    }

    if ctx.domain_filter_active && !matches!(mode, RendererMode::Native | RendererMode::Auto) {
        return Err(
            "Chrome screenshot rendering is disabled while --allowed-domains is active, because the renderer would load page resources outside the filter. Use --screenshot-renderer native or --engine chrome."
                .to_string(),
        );
    }

    let target = match mode {
        RendererMode::Native => None,
        RendererMode::Auto if ctx.domain_filter_active => None,
        RendererMode::Remote(url) => Some((RenderedBy::Remote, url.as_str())),
        RendererMode::Chrome => {
            if super::cdp::chrome::find_chrome().is_none() {
                return Err(
                    "--screenshot-renderer chrome needs Chrome. Run `agent-browser install --with-chrome`, or use --screenshot-renderer native or a remote renderer URL."
                        .to_string(),
                );
            }
            Some((RenderedBy::LocalChrome, ""))
        }
        RendererMode::Auto => {
            super::cdp::chrome::find_chrome().map(|_| (RenderedBy::LocalChrome, ""))
        }
    };

    let Some((rendered_by, remote_url)) = target else {
        if options.format != "png" {
            return Err(
                "Lightpanda's text render only produces PNG. Use --screenshot-format png, or a Chrome renderer (--screenshot-renderer chrome or a renderer URL)."
                    .to_string(),
            );
        }
        return Ok((native().await?, RenderedBy::LightpandaText));
    };

    let request = build_render_request(ctx, options).await?;
    let response = match rendered_by {
        RenderedBy::Remote => {
            remote::render(remote_url, ctx.renderer_token.as_deref(), &request).await?
        }
        _ => renderer.render_local(&request).await?,
    };

    let annotations = response
        .annotations
        .iter()
        .filter_map(annotation_from_value)
        .collect();
    Ok((
        ScreenshotResult {
            base64: response.data,
            annotations,
        },
        rendered_by,
    ))
}

fn annotation_from_value(value: &Value) -> Option<ScreenshotAnnotation> {
    let bx = value.get("box")?;
    Some(ScreenshotAnnotation {
        ref_id: value.get("ref")?.as_str()?.to_string(),
        number: value.get("number")?.as_u64()?,
        role: value.get("role")?.as_str()?.to_string(),
        name: value.get("name").and_then(|v| v.as_str()).map(String::from),
        box_: screenshot::AnnotationBox {
            x: bx.get("x")?.as_i64()?,
            y: bx.get("y")?.as_i64()?,
            width: bx.get("width")?.as_i64()?,
            height: bx.get("height")?.as_i64()?,
        },
    })
}

/// Serializes the live document for a renderer. Runs in the page; returns
/// `{html, url, viewport, scroll}`. The clone drops scripts and inline event
/// handlers (the DOM already reflects their effects), copies live form state
/// into attributes, masks password values, and inlines CSSOM-only rules so
/// CSS-in-JS styles survive serialization.
const SERIALIZE_DOCUMENT_JS: &str = r#"(() => {
  const doc = document;
  const root = doc.documentElement;
  if (!root) return null;
  const clone = root.cloneNode(true);

  const formSel = 'input,textarea,select option';
  const live = root.querySelectorAll(formSel);
  const copies = clone.querySelectorAll(formSel);
  for (let i = 0; i < live.length && i < copies.length; i++) {
    const l = live[i], c = copies[i];
    const tag = l.tagName.toLowerCase();
    try {
      if (tag === 'option') {
        if (l.selected) c.setAttribute('selected', ''); else c.removeAttribute('selected');
      } else if (tag === 'textarea') {
        c.textContent = l.value || '';
      } else {
        const type = (l.getAttribute('type') || '').toLowerCase();
        if (type === 'checkbox' || type === 'radio') {
          if (l.checked) c.setAttribute('checked', ''); else c.removeAttribute('checked');
        } else if (type === 'password') {
          c.setAttribute('value', '*'.repeat((l.value || '').length));
        } else if (type !== 'file') {
          c.setAttribute('value', l.value == null ? '' : String(l.value));
        }
      }
    } catch (e) {}
  }

  try {
    const liveStyles = root.querySelectorAll('style');
    const cloneStyles = clone.querySelectorAll('style');
    for (let i = 0; i < liveStyles.length && i < cloneStyles.length; i++) {
      const sheet = liveStyles[i].sheet;
      if (!sheet || !sheet.cssRules) continue;
      if ((liveStyles[i].textContent || '').trim() !== '') continue;
      const text = Array.from(sheet.cssRules).map(r => r.cssText).join('\n');
      if (text) cloneStyles[i].textContent = text;
    }
    if (doc.adoptedStyleSheets && doc.adoptedStyleSheets.length) {
      const extra = doc.createElement('style');
      extra.textContent = doc.adoptedStyleSheets
        .map(s => Array.from(s.cssRules || []).map(r => r.cssText).join('\n'))
        .join('\n');
      (clone.querySelector('head') || clone).appendChild(extra);
    }
  } catch (e) {}

  clone.querySelectorAll('script,noscript,meta[http-equiv="refresh" i]').forEach(n => n.remove());
  clone.querySelectorAll('*').forEach(el => {
    for (const attr of Array.from(el.attributes)) {
      if (attr.name.toLowerCase().startsWith('on')) el.removeAttribute(attr.name);
    }
  });

  let doctype = '';
  const dt = doc.doctype;
  if (dt) {
    doctype = '<!DOCTYPE ' + dt.name +
      (dt.publicId ? ' PUBLIC "' + dt.publicId + '"' : '') +
      (!dt.publicId && dt.systemId ? ' SYSTEM' : '') +
      (dt.systemId ? ' "' + dt.systemId + '"' : '') + '>';
  }

  return {
    html: doctype + clone.outerHTML,
    url: String(location.href),
    viewport: {
      width: Math.round(window.innerWidth || 1280),
      height: Math.round(window.innerHeight || 720),
      deviceScaleFactor: window.devicePixelRatio || 1,
    },
    scroll: { x: window.scrollX || 0, y: window.scrollY || 0 },
    colorScheme: (() => {
      try { return window.matchMedia('(prefers-color-scheme: dark)').matches ? 'dark' : 'light'; }
      catch (e) { return 'light'; }
    })(),
  };
})()"#;

async fn build_render_request(
    ctx: &CaptureContext<'_>,
    options: &ScreenshotOptions,
) -> Result<RenderRequest, String> {
    let mut target = false;
    if let Some(selector) = options.selector.as_deref() {
        let (object_id, session) = resolve_element_object_id(
            ctx.client,
            ctx.session_id,
            ctx.ref_map,
            selector,
            ctx.iframe_sessions,
        )
        .await?;
        if session != ctx.session_id {
            return Err(
                "Element screenshots inside iframes are not supported by the screenshot renderer"
                    .to_string(),
            );
        }
        set_marker(ctx, &object_id, TARGET_ATTR, "").await?;
        target = true;
    }

    let mut refs = Vec::new();
    if options.annotate {
        refs = mark_refs(ctx).await;
    }

    let serialized = evaluate_value(ctx, SERIALIZE_DOCUMENT_JS).await;
    // Markers must not leak into the live page, whatever happened above.
    let _ = evaluate_value(
        ctx,
        &format!(
            "document.querySelectorAll('[{t}],[{r}]').forEach(el => {{ el.removeAttribute('{t}'); el.removeAttribute('{r}'); }})",
            t = TARGET_ATTR,
            r = REF_ATTR
        ),
    )
    .await;
    let serialized = serialized?;
    if serialized.is_null() {
        return Err("The page has no document to render".to_string());
    }

    let cookies = ctx
        .client
        .send_command("Network.getCookies", None, Some(ctx.session_id))
        .await
        .ok()
        .and_then(|v| v.get("cookies").and_then(|c| c.as_array()).cloned())
        .unwrap_or_default();

    Ok(RenderRequest {
        url: serialized
            .get("url")
            .and_then(|v| v.as_str())
            .unwrap_or("about:blank")
            .to_string(),
        html: serialized
            .get("html")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string(),
        cookies,
        viewport: serde_json::from_value(serialized.get("viewport").cloned().unwrap_or_default())
            .unwrap_or(RenderViewport {
                width: 1280,
                height: 720,
                device_scale_factor: 1.0,
            }),
        scroll: serde_json::from_value(serialized.get("scroll").cloned().unwrap_or_default())
            .unwrap_or_default(),
        color_scheme: serialized
            .get("colorScheme")
            .and_then(|v| v.as_str())
            .map(String::from),
        full_page: options.full_page,
        format: options.format.clone(),
        quality: options.quality,
        target,
        annotate: options.annotate,
        refs,
        timeout_ms: None,
    })
}

/// Tags every main-frame ref with [`REF_ATTR`]; refs that fail to resolve are skipped.
async fn mark_refs(ctx: &CaptureContext<'_>) -> Vec<RenderRef> {
    let entries: Vec<_> = ctx
        .ref_map
        .entries_sorted()
        .into_iter()
        .filter(|(_, entry)| entry.frame_id.is_none() && entry.backend_node_id.is_some())
        .collect();

    let marks = entries.iter().map(|(ref_id, entry)| async move {
        let resolved = ctx
            .client
            .send_command(
                "DOM.resolveNode",
                Some(json!({
                    "backendNodeId": entry.backend_node_id,
                    "objectGroup": "agent-browser-render",
                })),
                Some(ctx.session_id),
            )
            .await
            .ok()?;
        let object_id = resolved.get("object")?.get("objectId")?.as_str()?;
        set_marker(ctx, object_id, REF_ATTR, ref_id).await.ok()?;
        Some(RenderRef {
            ref_id: ref_id.clone(),
            role: entry.role.clone(),
            name: entry.name.clone(),
        })
    });
    futures_util::future::join_all(marks)
        .await
        .into_iter()
        .flatten()
        .collect()
}

async fn set_marker(
    ctx: &CaptureContext<'_>,
    object_id: &str,
    attr: &str,
    value: &str,
) -> Result<(), String> {
    ctx.client
        .send_command(
            "Runtime.callFunctionOn",
            Some(json!({
                "objectId": object_id,
                "functionDeclaration": "function(name, value) { if (this.setAttribute) this.setAttribute(name, value); }",
                "arguments": [{ "value": attr }, { "value": value }],
                "returnByValue": true,
            })),
            Some(ctx.session_id),
        )
        .await
        .map(|_| ())
}

async fn evaluate_value(ctx: &CaptureContext<'_>, expression: &str) -> Result<Value, String> {
    let result = ctx
        .client
        .send_command(
            "Runtime.evaluate",
            Some(json!({ "expression": expression, "returnByValue": true })),
            Some(ctx.session_id),
        )
        .await?;
    if let Some(details) = result.get("exceptionDetails") {
        return Err(format!(
            "Failed to serialize page for rendering: {}",
            details
        ));
    }
    Ok(result
        .get("result")
        .and_then(|r| r.get("value"))
        .cloned()
        .unwrap_or(Value::Null))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_renderer_modes() {
        assert_eq!(RendererMode::parse("auto").unwrap(), RendererMode::Auto);
        assert_eq!(RendererMode::parse("").unwrap(), RendererMode::Auto);
        assert_eq!(RendererMode::parse("Chrome").unwrap(), RendererMode::Chrome);
        assert_eq!(RendererMode::parse("native").unwrap(), RendererMode::Native);
        assert_eq!(
            RendererMode::parse("https://renderer.svc:9300/").unwrap(),
            RendererMode::Remote("https://renderer.svc:9300".to_string())
        );
        assert!(RendererMode::parse("ftp://nope").is_err());
        assert!(RendererMode::parse("webkit").is_err());
    }

    #[test]
    fn command_token_wins_and_empty_is_ignored() {
        assert_eq!(
            renderer_token_from_command(&json!({ "rendererToken": "abc" })).as_deref(),
            Some("abc")
        );
        if std::env::var(RENDERER_TOKEN_ENV).is_err() {
            assert_eq!(
                renderer_token_from_command(&json!({ "rendererToken": "" })),
                None
            );
            assert_eq!(renderer_token_from_command(&json!({})), None);
        }
    }

    #[test]
    fn command_renderer_wins_over_env() {
        let cmd = json!({ "renderer": "native" });
        assert_eq!(
            RendererMode::from_command(&cmd).unwrap(),
            RendererMode::Native
        );
    }

    #[test]
    fn render_request_round_trips_with_defaults() {
        let request: RenderRequest = serde_json::from_value(json!({
            "url": "https://example.com/",
            "html": "<html></html>",
            "viewport": { "width": 800, "height": 600 }
        }))
        .unwrap();
        assert_eq!(request.format, "png");
        assert_eq!(request.viewport.device_scale_factor, 1.0);
        assert!(!request.full_page && !request.target && !request.annotate);
        let value = serde_json::to_value(&request).unwrap();
        assert_eq!(value["viewport"]["deviceScaleFactor"], 1.0);
        assert_eq!(value["fullPage"], false);
    }

    #[test]
    fn annotations_parse_from_renderer_json() {
        let value = json!({
            "ref": "e2", "number": 2, "role": "button", "name": "Go",
            "box": { "x": 1, "y": 2, "width": 3, "height": 4 }
        });
        let annotation = annotation_from_value(&value).unwrap();
        assert_eq!(annotation.ref_id, "e2");
        assert_eq!(annotation.box_.height, 4);
        assert!(annotation_from_value(&json!({ "ref": "e1" })).is_none());
    }
}
