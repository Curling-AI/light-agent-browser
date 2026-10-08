//! Client for a remote `agent-browser renderer serve` deployment.

use std::time::Duration;

use super::{RenderRequest, RenderResponse};

const REMOTE_TIMEOUT: Duration = Duration::from_secs(90);

/// Accepts a base URL (`https://renderer:9300`) or the full endpoint.
pub fn endpoint(base: &str) -> String {
    let base = base.trim_end_matches('/');
    if base.ends_with("/v1/render") {
        base.to_string()
    } else {
        format!("{}/v1/render", base)
    }
}

pub async fn render(
    base_url: &str,
    token: Option<&str>,
    request: &RenderRequest,
) -> Result<RenderResponse, String> {
    let client =
        crate::tls::apply_to_reqwest(reqwest::Client::builder(), &crate::tls::process_options())?
            .user_agent(format!("agent-browser/{}", env!("CARGO_PKG_VERSION")))
            .timeout(REMOTE_TIMEOUT)
            .connect_timeout(Duration::from_secs(10))
            .build()
            .map_err(|e| format!("Failed to create renderer HTTP client: {}", e))?;

    let url = endpoint(base_url);
    let mut builder = client.post(&url).json(request);
    if let Some(token) = token.filter(|t| !t.is_empty()) {
        builder = builder.bearer_auth(token);
    }

    let response = builder
        .send()
        .await
        .map_err(|e| format!("Screenshot renderer at {} is unreachable: {}", url, e))?;
    let status = response.status();
    let body = response
        .text()
        .await
        .map_err(|e| format!("Failed to read renderer response: {}", e))?;
    if !status.is_success() {
        let message = serde_json::from_str::<serde_json::Value>(&body)
            .ok()
            .and_then(|v| v.get("error").and_then(|e| e.as_str()).map(String::from))
            .unwrap_or(body);
        return Err(format!(
            "Screenshot renderer returned {}: {}",
            status, message
        ));
    }
    serde_json::from_str(&body).map_err(|e| format!("Invalid renderer response: {}", e))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_appends_render_path_once() {
        assert_eq!(endpoint("http://r:9300"), "http://r:9300/v1/render");
        assert_eq!(endpoint("http://r:9300/"), "http://r:9300/v1/render");
        assert_eq!(endpoint("http://r/v1/render"), "http://r/v1/render");
        assert_eq!(endpoint("https://r/base"), "https://r/base/v1/render");
    }
}
