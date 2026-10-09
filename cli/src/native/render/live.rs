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

/// Reports two independent signals: `local` (the page lives where only this
/// machine can reach it) and `chart` (it draws something a serialized DOM
/// cannot reproduce). Either may be `null`.
const LIVE_PROBE_JS: &str = r#"(() => {
  const loc = window.location;
  let local = null;
  if (loc.protocol === 'file:') {
    local = 'local-file';
  } else {
    const host = (loc.hostname || '').replace(/^\[|\]$/g, '');
    if (host === 'localhost' || host.endsWith('.localhost') || host === '::1' ||
        /^127\./.test(host) || /^10\./.test(host) || /^192\.168\./.test(host) ||
        /^172\.(1[6-9]|2\d|3[01])\./.test(host) || /^169\.254\./.test(host)) {
      local = 'local-server';
    }
  }
  // Hidden canvases (text measurement, captchas, tracking) draw nothing the
  // user sees, so they are no reason to reload the page.
  const shown = (el) => {
    for (let node = el; node && node.nodeType === 1; node = node.parentElement) {
      if (node.hidden) return false;
      try {
        const style = getComputedStyle(node);
        if (style.display === 'none' || style.visibility === 'hidden') return false;
      } catch (e) {}
    }
    return true;
  };
  let chart = null;
  // Any visible canvas, whatever its size: when a chart library fails under
  // Lightpanda its canvas never grows past the placeholder it started as.
  if (Array.from(document.querySelectorAll('canvas')).some(shown)) chart = 'canvas';
  else if (document.querySelector('.apexcharts-canvas, .apexcharts-svg')) chart = 'layout-chart';
  else if ((window.google && window.google.visualization) ||
      document.querySelector('script[src*="gstatic.com/charts"]')) chart = 'layout-chart';
  return { local, chart };
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

/// What the probe found on the Lightpanda page.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LiveProbe {
    /// `local-file` or `local-server`.
    pub local: Option<LiveReason>,
    /// `canvas` or `layout-chart`.
    pub chart: Option<LiveReason>,
}

impl LiveProbe {
    /// Why the capture must reload the page in a local Chrome, if it must.
    ///
    /// Charts always need it: no serialized DOM carries canvas pixels. Local
    /// pages only need it for a remote renderer, which cannot reach them. A
    /// local renderer reaches them already and paints the session's own DOM,
    /// keeping typed form values and client-side state a reload would lose.
    pub fn reason(self, remote_renderer: bool) -> Option<LiveReason> {
        if remote_renderer {
            self.local.or(self.chart)
        } else {
            self.chart
        }
    }
}

/// Probes the Lightpanda page. A failed probe finds nothing, so a capture
/// never breaks because the probe did.
pub async fn probe(ctx: &CaptureContext<'_>) -> LiveProbe {
    let Ok(value) = super::evaluate_value(ctx, LIVE_PROBE_JS).await else {
        return LiveProbe::default();
    };
    let field = |name: &str| {
        value
            .get(name)
            .and_then(Value::as_str)
            .and_then(LiveReason::parse)
    };
    LiveProbe {
        local: field("local"),
        chart: field("chart"),
    }
}

/// Explains a live capture that was needed but could not run, for the
/// command's warning.
pub fn missed_warning(reason: LiveReason) -> String {
    match reason {
        LiveReason::LocalFile | LiveReason::LocalServer => {
            "The remote renderer cannot reach this local page, so the capture may be incomplete. Install Chrome (`agent-browser install --with-chrome`) for live captures, or use --screenshot-renderer chrome.".to_string()
        }
        LiveReason::Canvas | LiveReason::LayoutChart => {
            "This page draws charts that only a live capture shows, and no local Chrome is installed, so they may be blank. Install Chrome (`agent-browser install --with-chrome`) or use --engine chrome.".to_string()
        }
    }
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

    #[test]
    fn local_pages_only_go_live_for_a_remote_renderer() {
        let local = LiveProbe {
            local: Some(LiveReason::LocalServer),
            chart: None,
        };
        assert_eq!(local.reason(true), Some(LiveReason::LocalServer));
        assert_eq!(local.reason(false), None);

        let file = LiveProbe {
            local: Some(LiveReason::LocalFile),
            chart: None,
        };
        assert_eq!(file.reason(false), None);
    }

    #[test]
    fn charts_go_live_for_every_renderer() {
        let chart = LiveProbe {
            local: None,
            chart: Some(LiveReason::Canvas),
        };
        assert_eq!(chart.reason(false), Some(LiveReason::Canvas));
        assert_eq!(chart.reason(true), Some(LiveReason::Canvas));

        // A local chart page reports the local reason to a remote renderer.
        let both = LiveProbe {
            local: Some(LiveReason::LocalServer),
            chart: Some(LiveReason::Canvas),
        };
        assert_eq!(both.reason(true), Some(LiveReason::LocalServer));
        assert_eq!(both.reason(false), Some(LiveReason::Canvas));
        assert_eq!(LiveProbe::default().reason(true), None);
    }
}
