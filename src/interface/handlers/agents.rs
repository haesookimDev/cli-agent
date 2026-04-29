//! `/v1/agents/*` endpoints — runtime control of the AgentRegistry
//! (Phase 12 / TODO 9-5 / F5).
//!
//! Currently exposes a single POST that re-reads the agents directory
//! configured via `Orchestrator::set_agents_dir`. Auto file-watching
//! (notify crate) is a follow-up; this handler covers the manual case
//! ("operator edited a YAML, ping the server") without adding deps.

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::Json;

use crate::interface::api::ApiState;

pub(crate) async fn reload_handler(
    State(state): State<ApiState>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    if let Err(err) = state.auth.verify_headers(&headers, body.as_ref()) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error": err.to_string()})),
        );
    }
    match state.orchestrator.reload_agents().await {
        Ok(count) => (
            StatusCode::OK,
            Json(serde_json::json!({"status": "reloaded", "agent_count": count})),
        ),
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": err.to_string()})),
        ),
    }
}
