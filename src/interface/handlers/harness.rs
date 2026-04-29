//! `/v1/harness/*` endpoints — observability for the AgentHarness layer
//! (Phase 11-E / TODO 9-4).
//!
//! Currently exposes a single GET that returns a snapshot of
//! `HarnessMetrics`. Cheap enough that frontends can poll on a 1–2s tick.

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::Json;

use crate::harness::metrics::HarnessMetrics;
use crate::interface::api::{json_value, ApiState};

pub(crate) async fn get_metrics_handler(
    State(state): State<ApiState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(err) = state.auth.verify_headers(&headers, &[]) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error": err.to_string()})),
        );
    }
    let snapshot = HarnessMetrics::snapshot(state.orchestrator.harness.as_ref());
    (StatusCode::OK, Json(json_value(snapshot)))
}
