//! `/v1/skills/*` endpoints — listing, fetching, executing, reloading skills.

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::Json;

use crate::interface::api::{ApiState, ExecuteWorkflowRequest};

pub(crate) async fn list_skills_handler(State(state): State<ApiState>) -> impl IntoResponse {
    let skills = state.orchestrator.list_skills();
    (StatusCode::OK, Json(serde_json::json!(skills)))
}

pub(crate) async fn get_skill_handler(
    State(state): State<ApiState>,
    Path(skill_id): Path<String>,
) -> impl IntoResponse {
    match state.orchestrator.get_skill(&skill_id) {
        Some(skill) => (StatusCode::OK, Json(serde_json::json!(skill))),
        None => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": "skill not found"})),
        ),
    }
}

pub(crate) async fn execute_skill_handler(
    State(state): State<ApiState>,
    Path(skill_id): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    if let Err(err) = state.auth.verify_headers(&headers, body.as_ref()) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error": err.to_string()})),
        );
    }

    let req = serde_json::from_slice::<ExecuteWorkflowRequest>(body.as_ref()).unwrap_or(
        ExecuteWorkflowRequest {
            parameters: None,
            session_id: None,
        },
    );

    match state
        .orchestrator
        .execute_workflow(&skill_id, req.parameters, req.session_id)
        .await
    {
        Ok(submission) => (StatusCode::OK, Json(serde_json::json!(submission))),
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": err.to_string()})),
        ),
    }
}

pub(crate) async fn reload_skills_handler(
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

    // Reload from default skills directory if configured
    let skills_dir = std::env::var("SKILLS_DIR").ok();
    match skills_dir {
        Some(dir) => {
            state
                .orchestrator
                .reload_skills(std::path::Path::new(&dir))
                .await;
            let count = state.orchestrator.list_skills().len();
            (StatusCode::OK, Json(serde_json::json!({"reloaded": count})))
        }
        None => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "SKILLS_DIR not configured"})),
        ),
    }
}
