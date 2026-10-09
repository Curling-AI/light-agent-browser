//! Prometheus counters for `renderer serve`, exposed on `GET /metrics`.
//!
//! No caller label: a fleet can have tens of thousands of callers, and a label
//! per caller would turn every scrape into that many series. Who called is in
//! the per-request log line instead.

use std::fmt::Write as _;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

/// Upper bounds of the render duration histogram, in seconds. The last one
/// matches the render deadline.
const BUCKETS: [f64; 8] = [0.25, 0.5, 1.0, 2.0, 5.0, 10.0, 30.0, 85.0];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Ok,
    Error,
    BadRequest,
    Unauthorized,
    Throttled,
}

impl Outcome {
    const ALL: [Outcome; 5] = [
        Outcome::Ok,
        Outcome::Error,
        Outcome::BadRequest,
        Outcome::Unauthorized,
        Outcome::Throttled,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Outcome::Ok => "ok",
            Outcome::Error => "error",
            Outcome::BadRequest => "bad_request",
            Outcome::Unauthorized => "unauthorized",
            Outcome::Throttled => "throttled",
        }
    }

    fn index(self) -> usize {
        Outcome::ALL.iter().position(|o| *o == self).unwrap_or(0)
    }
}

#[derive(Default)]
pub struct Metrics {
    requests: [AtomicU64; 5],
    in_flight: AtomicU64,
    buckets: [AtomicU64; BUCKETS.len()],
    duration_count: AtomicU64,
    duration_micros: AtomicU64,
    chrome_launches: AtomicU64,
}

impl Metrics {
    pub fn record(&self, outcome: Outcome) {
        self.requests[outcome.index()].fetch_add(1, Ordering::Relaxed);
    }

    pub fn observe_render(&self, elapsed: Duration) {
        let seconds = elapsed.as_secs_f64();
        for (bound, bucket) in BUCKETS.iter().zip(&self.buckets) {
            if seconds <= *bound {
                bucket.fetch_add(1, Ordering::Relaxed);
            }
        }
        self.duration_count.fetch_add(1, Ordering::Relaxed);
        self.duration_micros
            .fetch_add(elapsed.as_micros() as u64, Ordering::Relaxed);
    }

    pub fn chrome_launched(&self) {
        self.chrome_launches.fetch_add(1, Ordering::Relaxed);
    }

    /// Counts a render as in flight until the guard drops.
    pub fn in_flight(&self) -> InFlight<'_> {
        self.in_flight.fetch_add(1, Ordering::Relaxed);
        InFlight(&self.in_flight)
    }

    pub fn render_text(&self) -> String {
        let load = |c: &AtomicU64| c.load(Ordering::Relaxed);
        let mut out = String::new();
        out.push_str("# HELP agent_browser_renderer_requests_total Render requests by outcome.\n");
        out.push_str("# TYPE agent_browser_renderer_requests_total counter\n");
        for outcome in Outcome::ALL {
            let _ = writeln!(
                out,
                "agent_browser_renderer_requests_total{{outcome=\"{}\"}} {}",
                outcome.as_str(),
                load(&self.requests[outcome.index()])
            );
        }
        out.push_str("# HELP agent_browser_renderer_in_flight Renders in progress.\n");
        out.push_str("# TYPE agent_browser_renderer_in_flight gauge\n");
        let _ = writeln!(
            out,
            "agent_browser_renderer_in_flight {}",
            load(&self.in_flight)
        );
        out.push_str("# HELP agent_browser_renderer_render_seconds Time spent rendering.\n");
        out.push_str("# TYPE agent_browser_renderer_render_seconds histogram\n");
        for (bound, bucket) in BUCKETS.iter().zip(&self.buckets) {
            let _ = writeln!(
                out,
                "agent_browser_renderer_render_seconds_bucket{{le=\"{}\"}} {}",
                bound,
                load(bucket)
            );
        }
        let count = load(&self.duration_count);
        let _ = writeln!(
            out,
            "agent_browser_renderer_render_seconds_bucket{{le=\"+Inf\"}} {}",
            count
        );
        let _ = writeln!(
            out,
            "agent_browser_renderer_render_seconds_sum {}",
            load(&self.duration_micros) as f64 / 1_000_000.0
        );
        let _ = writeln!(out, "agent_browser_renderer_render_seconds_count {}", count);
        out.push_str("# HELP agent_browser_renderer_chrome_launches_total Chrome launches, recycling and crash recovery included.\n");
        out.push_str("# TYPE agent_browser_renderer_chrome_launches_total counter\n");
        let _ = writeln!(
            out,
            "agent_browser_renderer_chrome_launches_total {}",
            load(&self.chrome_launches)
        );
        out
    }
}

pub struct InFlight<'a>(&'a AtomicU64);

impl Drop for InFlight<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_counters_and_cumulative_histogram() {
        let m = Metrics::default();
        m.record(Outcome::Ok);
        m.record(Outcome::Ok);
        m.record(Outcome::Throttled);
        m.observe_render(Duration::from_millis(300));
        m.observe_render(Duration::from_secs(3));
        m.chrome_launched();
        let guard = m.in_flight();
        let text = m.render_text();
        drop(guard);

        assert!(text.contains("agent_browser_renderer_requests_total{outcome=\"ok\"} 2"));
        assert!(text.contains("agent_browser_renderer_requests_total{outcome=\"throttled\"} 1"));
        assert!(text.contains("agent_browser_renderer_in_flight 1"));
        assert!(text.contains("agent_browser_renderer_render_seconds_bucket{le=\"0.25\"} 0"));
        assert!(text.contains("agent_browser_renderer_render_seconds_bucket{le=\"0.5\"} 1"));
        assert!(text.contains("agent_browser_renderer_render_seconds_bucket{le=\"5\"} 2"));
        assert!(text.contains("agent_browser_renderer_render_seconds_bucket{le=\"+Inf\"} 2"));
        assert!(text.contains("agent_browser_renderer_render_seconds_count 2"));
        assert!(text.contains("agent_browser_renderer_chrome_launches_total 1"));
        assert!(m
            .render_text()
            .contains("agent_browser_renderer_in_flight 0"));
    }
}
