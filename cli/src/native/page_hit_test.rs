//! Fork: hit test for browsers that do not implement `DOM.getNodeForLocation`.
//!
//! Upstream confirms every ref click by asking the browser which node would
//! receive a press at the click point (`click_point.rs`). Lightpanda, the
//! default engine here, has no layout engine and does not implement
//! `DOM.getNodeForLocation`, so without this fallback every `click @ref`
//! fails. Lightpanda delivers input events to the element that
//! `document.elementFromPoint` returns for the same point, so that call names
//! the receiver instead. It runs in an isolated world, where page script cannot
//! redefine `elementFromPoint`.
//!
//! The receiver is reported in the session's root frame. A ref inside an
//! iframe therefore gets the frame element as its receiver, and upstream's
//! frame check refuses the press, which fails closed.

use serde_json::Value;

use super::cdp::client::CdpClient;

/// Whether a CDP error says the browser does not implement
/// `DOM.getNodeForLocation`.
pub fn node_for_location_unsupported(error: &str) -> bool {
    error.contains("DOM.getNodeForLocation")
        && (error.contains("wasn't found")
            || error.contains("Method not found")
            || error.contains("-32601"))
}

/// Backend node id and frame id of the element at a viewport point of the
/// session's root frame, as `document.elementFromPoint` reports it.
pub async fn element_at_point(
    client: &CdpClient,
    session: &str,
    x: f64,
    y: f64,
) -> Result<(i64, String), String> {
    let tree = client
        .send_command_no_params("Page.getFrameTree", Some(session))
        .await?;
    let frame_id = tree
        .get("frameTree")
        .and_then(|tree| tree.get("frame"))
        .and_then(|frame| frame.get("id"))
        .and_then(Value::as_str)
        .ok_or("the page frame tree names no root frame")?
        .to_string();
    let world = client
        .send_command(
            "Page.createIsolatedWorld",
            Some(serde_json::json!({
                "frameId": frame_id,
                "worldName": "agent-browser-click",
                "grantUniveralAccess": false,
            })),
            Some(session),
        )
        .await?;
    let context_id = world
        .get("executionContextId")
        .and_then(Value::as_i64)
        .ok_or("the browser created no isolated context")?;
    let hit = client
        .send_command(
            "Runtime.evaluate",
            Some(serde_json::json!({
                "expression": hit_expression(x, y),
                "contextId": context_id,
                "returnByValue": false,
            })),
            Some(session),
        )
        .await?;
    let object_id = hit
        .get("result")
        .and_then(|result| result.get("objectId"))
        .and_then(Value::as_str)
        .ok_or("the page reports no element at the point")?;
    let described = client
        .send_command(
            "DOM.describeNode",
            Some(serde_json::json!({ "objectId": object_id })),
            Some(session),
        )
        .await?;
    let backend_node_id = described
        .get("node")
        .and_then(|node| node.get("backendNodeId"))
        .and_then(Value::as_i64)
        .ok_or("the browser named no node")?;
    Ok((backend_node_id, frame_id))
}

fn hit_expression(x: f64, y: f64) -> String {
    format!("document.elementFromPoint({}, {})", x, y)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_a_missing_get_node_for_location() {
        assert!(node_for_location_unsupported(
            "CDP error (DOM.getNodeForLocation): 'DOM.getNodeForLocation' wasn't found"
        ));
        assert!(node_for_location_unsupported(
            "CDP error (DOM.getNodeForLocation): Method not found (-32601)"
        ));
    }

    #[test]
    fn other_hit_test_errors_still_refuse() {
        assert!(!node_for_location_unsupported(
            "CDP error (DOM.getNodeForLocation): No node found at given location"
        ));
        assert!(!node_for_location_unsupported(
            "CDP error (DOM.getContentQuads): 'DOM.getContentQuads' wasn't found"
        ));
    }

    #[test]
    fn hit_expression_uses_the_viewport_point() {
        assert_eq!(
            hit_expression(26.0, 138.0),
            "document.elementFromPoint(26, 138)"
        );
    }
}
