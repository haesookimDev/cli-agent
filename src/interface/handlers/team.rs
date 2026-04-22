//! `/v1/team/*` and `/v1/github/activities*` endpoints — agent persona
//! listing and GitHub activity trace.

use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use serde::Deserialize;

use crate::interface::api::ApiState;

#[derive(Debug, Deserialize)]
pub(crate) struct GitHubActivityQuery {
    pub persona: Option<String>,
    pub run_id: Option<String>,
    pub limit: Option<i64>,
}

pub(crate) async fn list_team_members_handler(
    State(_state): State<ApiState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(e) = _state.auth.verify_headers(&headers, &[]) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error": e.to_string()})),
        );
    }

    // Load team agent definitions from agents/team/ directory.
    let team_dir = std::path::Path::new("agents/team");
    let members = crate::agents::agent_loader::load_agents_from_dir(team_dir).await;

    let result: Vec<serde_json::Value> = members
        .into_iter()
        .map(|def| {
            let mut val = serde_json::json!({
                "name": def.name,
                "description": def.description,
                "role": def.role.to_string(),
                "task_profile": def.task_profile.to_string(),
                "capabilities": def.capabilities,
            });
            if let Some(persona) = &def.persona {
                val["persona"] = serde_json::to_value(persona).unwrap_or_default();
            }
            val
        })
        .collect();

    (StatusCode::OK, Json(serde_json::json!(result)))
}

pub(crate) async fn list_github_activities_handler(
    State(state): State<ApiState>,
    Query(query): Query<GitHubActivityQuery>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(e) = state.auth.verify_headers(&headers, &[]) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error": e.to_string()})),
        );
    }

    let limit = query.limit.unwrap_or(100).clamp(1, 500);
    match state
        .orchestrator
        .memory()
        .store()
        .list_github_activities(query.persona.as_deref(), query.run_id.as_deref(), limit)
        .await
    {
        Ok(activities) => (StatusCode::OK, Json(serde_json::json!(activities))),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        ),
    }
}

pub(crate) async fn github_activity_stats_handler(
    State(state): State<ApiState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(e) = state.auth.verify_headers(&headers, &[]) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error": e.to_string()})),
        );
    }

    match state
        .orchestrator
        .memory()
        .store()
        .github_activity_stats()
        .await
    {
        Ok(stats) => (StatusCode::OK, Json(serde_json::json!(stats))),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        ),
    }
}
