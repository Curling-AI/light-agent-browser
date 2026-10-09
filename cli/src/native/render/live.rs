//! Live captures for pages a serialized-DOM render cannot reproduce.
//!
//! The regular renderer paints a script-free copy of the Lightpanda DOM. That
//! loses what only exists as pixels or depends on layout at paint time:
//! `<canvas>` charts (Chart.js, ECharts), charts that measure their own SVG
//! (ApexCharts, Google Charts), and pages whose resources only the agent's
//! machine can reach (`file://`, local servers), which a shared renderer
//! cannot load and must refuse anyway. For those, a short-lived local Chrome
//! opens the page itself with the session cookies, lets its scripts run,
//! captures, and exits. Nothing stays resident, unlike a Chrome engine
//! session.

use serde_json::{json, Value};

use super::chrome::ChromeRenderer;
use super::guard::RenderPolicy;
use super::{CaptureContext, RenderRequest, RenderResponse};
use crate::native::element::resolve_element_object_id;

/// Returns why the page needs a live capture, or `null`.
const LIVE_PROBE_JS: &str = r#"(() => {
  const loc = window.location;
  if (loc.protocol === 'file:') return 'local-file';
  const host = (loc.hostname || '').replace(/^\[|\]$/g, '');
  if (host === 'localhost' || host.endsWith('.localhost') || host === '::1' ||
      /^127\./.test(host) || /^10\./.test(host) || /^192\.168\./.test(host) ||
      /^172\.(1[6-9]|2\d|3[01])\./.test(host) || /^169\.254\./.test(host)) {
    return 'local-server';
  }
  // Any canvas, whatever its size: when a chart library fails under
  // Lightpanda its canvas never grows past the placeholder it started as.
  if (document.querySelector('canvas')) return 'canvas';
  if (document.querySelector('.apexcharts-canvas, .apexcharts-svg')) return 'layout-chart';
  if ((window.google && window.google.visualization) ||
      document.querySelector('script[src*="gstatic.com/charts"]')) return 'layout-chart';
  return null;
})()"#;

/// Builds a CSS path that finds the same element after Chrome reloads the page.
const CSS_PATH_FN: &str = r#"function () {
  const parts = [];
  for (let el = this; el && el.nodeType === 1 && el !== document.documentElement; el = el.parentElement) {
    if (el.id && document.querySelectorAll('#' + CSS.escape(el.id)).length === 1) {
      parts.unshift('#' + CSS.escape(el.id));
      return parts.join(' > ');
    }
    let index = 1;
    for (let sib = el.previousElementSibling; sib; sib = sib.previousElementSibling) {
      if (sib.tagName === el.tagName) index++;
    }
    parts.unshift(el.tagName.toLowerCase() + ':nth-of-type(' + index + ')');
  }
  parts.unshift('html');
  return parts.join(' > ');
}"#;

/// Why a capture went live. Reported as `rendererReason`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LiveReason {
    LocalFile,
    LocalServer,
    Canvas,
    LayoutChart,
}

impl LiveReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::LocalFile => "local-file",
            Self::LocalServer => "local-server",
            Self::Canvas => "canvas",
            Self::LayoutChart => "layout-chart",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "local-file" => Some(Self::LocalFile),
            "local-server" => Some(Self::LocalServer),
            "canvas" => Some(Self::Canvas),
            "layout-chart" => Some(Self::LayoutChart),
            _ => None,
        }
    }
}

/// Probes the Lightpanda page. A failed probe means "no", so a capture never
/// breaks because the probe did.
pub async fn live_reason(ctx: &CaptureContext<'_>) -> Option<LiveReason> {
    let value = super::evaluate_value(ctx, LIVE_PROBE_JS).await.ok()?;
    LiveReason::parse(value.as_str()?)
}

/// CSS path of the element `selector` points at in the Lightpanda page.
pub async fn css_path(ctx: &CaptureContext<'_>, selector: &str) -> Result<String, String> {
    let (object_id, session) = resolve_element_object_id(
        ctx.client,
        ctx.session_id,
        ctx.ref_map,
        selector,
        ctx.iframe_sessions,
    )
    .await?;
    if session != ctx.session_id {
        return Err("Element screenshots inside iframes are not supported".to_string());
    }
    let result = ctx
        .client
        .send_command(
            "Runtime.callFunctionOn",
            Some(json!({
                "objectId": object_id,
                "functionDeclaration": CSS_PATH_FN,
                "returnByValue": true,
            })),
            Some(ctx.session_id),
        )
        .await?;
    result
        .pointer("/result/value")
        .and_then(Value::as_str)
        .map(String::from)
        .ok_or_else(|| "Could not locate the element for a live capture".to_string())
}

/// Opens `request.url` in a dedicated Chrome, captures, and shuts it down.
pub async fn capture(request: &RenderRequest) -> Result<RenderResponse, String> {
    let mut chrome = ChromeRenderer::launch_with(None, 1, RenderPolicy::LOCAL).await?;
    let result = chrome.render(request).await;
    chrome.shutdown().await;
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reasons_round_trip() {
        for reason in [
            LiveReason::LocalFile,
            LiveReason::LocalServer,
            LiveReason::Canvas,
            LiveReason::LayoutChart,
        ] {
            assert_eq!(LiveReason::parse(reason.as_str()), Some(reason));
        }
        assert_eq!(LiveReason::parse("something-else"), None);
    }
}
