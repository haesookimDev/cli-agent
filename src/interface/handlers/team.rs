//! `/v1/team/*` and `/v1/github/activities*` endpoints — Virtual Dev Team
//! persona CRUD and GitHub activity trace.

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use serde::Deserialize;

use crate::agents::agent_loader;
use crate::agents::agent_loader::AgentDefinition;
use crate::interface::api::ApiState;

#[derive(Debug, Deserialize)]
pub(crate) struct GitHubActivityQuery {
    pub persona: Option<String>,
    pub run_id: Option<String>,
    pub limit: Option<i64>,
}

fn definition_to_json(def: &AgentDefinition) -> serde_json::Value {
    let mut val = serde_json::json!({
        "name": def.name,
        "description": def.description,
        "role": def.role.to_string(),
        "task_profile": def.task_profile.to_string(),
        "capabilities": def.capabilities,
        "system_prompt": def.system_prompt,
        "instructions": def.instructions,
    });
    if let Some(persona) = &def.persona {
        val["persona"] = serde_json::to_value(persona).unwrap_or_default();
    }
    val
}

/// Resolve the on-disk path for team YAML files. Returns 500-style error
/// when `set_agents_dir` was never called (i.e. CLI mode without a server
/// startup pre-condition).
fn team_dir(state: &ApiState) -> Result<std::path::PathBuf, (StatusCode, Json<serde_json::Value>)> {
    state.orchestrator.agents_dir().map(|d| d.join("team")).ok_or_else(|| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": "agents directory not configured"})),
        )
    })
}

pub(crate) async fn list_team_members_handler(
    State(state): State<ApiState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(e) = state.auth.verify_headers(&headers, &[]) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error": e.to_string()})),
        );
    }

    let result: Vec<serde_json::Value> = state
        .orchestrator
        .list_team_personas()
        .iter()
        .map(definition_to_json)
        .collect();

    (StatusCode::OK, Json(serde_json::json!(result)))
}

pub(crate) async fn get_team_member_handler(
    State(state): State<ApiState>,
    Path(name): Path<String>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(e) = state.auth.verify_headers(&headers, &[]) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error": e.to_string()})),
        );
    }

    match state.orchestrator.get_team_persona(&name) {
        Some(def) => (StatusCode::OK, Json(definition_to_json(&def))),
        None => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": "team member not found"})),
        ),
    }
}

fn parse_definition_body(
    body: &[u8],
) -> Result<AgentDefinition, (StatusCode, Json<serde_json::Value>)> {
    let def: AgentDefinition = serde_json::from_slice(body).map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": e.to_string()})),
        )
    })?;
    if def.name.trim().is_empty() {
        return Err((
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(serde_json::json!({"error": "name must not be empty"})),
        ));
    }
    if def.system_prompt.trim().is_empty() {
        return Err((
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(serde_json::json!({"error": "system_prompt must not be empty"})),
        ));
    }
    Ok(def)
}

async fn finalize_after_write(
    state: &ApiState,
    def: AgentDefinition,
    status: StatusCode,
) -> (StatusCode, Json<serde_json::Value>) {
    let reload_result = state.orchestrator.reload_agents().await;
    let mut response = definition_to_json(&def);
    if let serde_json::Value::Object(ref mut map) = response {
        map.insert(
            "reloaded".to_string(),
            serde_json::Value::Bool(reload_result.is_ok()),
        );
        if let Err(err) = reload_result {
            map.insert(
                "reload_error".to_string(),
                serde_json::Value::String(err.to_string()),
            );
        }
    }
    (status, Json(response))
}

pub(crate) async fn create_team_member_handler(
    State(state): State<ApiState>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    if let Err(e) = state.auth.verify_headers(&headers, body.as_ref()) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error": e.to_string()})),
        );
    }

    let def = match parse_definition_body(body.as_ref()) {
        Ok(d) => d,
        Err(resp) => return resp,
    };

    let dir = match team_dir(&state) {
        Ok(d) => d,
        Err(resp) => return resp,
    };

    let _guard = state.orchestrator.team_yaml_lock.lock().await;

    if state.orchestrator.get_team_persona(&def.name).is_some() {
        return (
            StatusCode::CONFLICT,
            Json(serde_json::json!({"error": "team member with that name already exists"})),
        );
    }

    if let Err(err) = agent_loader::save_agent_definition(&dir, &def).await {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": err.to_string()})),
        );
    }

    finalize_after_write(&state, def, StatusCode::CREATED).await
}

pub(crate) async fn update_team_member_handler(
    State(state): State<ApiState>,
    Path(name): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    if let Err(e) = state.auth.verify_headers(&headers, body.as_ref()) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error": e.to_string()})),
        );
    }

    let def = match parse_definition_body(body.as_ref()) {
        Ok(d) => d,
        Err(resp) => return resp,
    };

    if def.name != name {
        return (
            StatusCode::BAD_REQUEST,
            Json(
                serde_json::json!({"error": "body name does not match URL path name"}),
            ),
        );
    }

    let dir = match team_dir(&state) {
        Ok(d) => d,
        Err(resp) => return resp,
    };

    let _guard = state.orchestrator.team_yaml_lock.lock().await;

    if state.orchestrator.get_team_persona(&name).is_none() {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": "team member not found"})),
        );
    }

    if let Err(err) = agent_loader::save_agent_definition(&dir, &def).await {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": err.to_string()})),
        );
    }

    finalize_after_write(&state, def, StatusCode::OK).await
}

pub(crate) async fn delete_team_member_handler(
    State(state): State<ApiState>,
    Path(name): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    if let Err(e) = state.auth.verify_headers(&headers, body.as_ref()) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error": e.to_string()})),
        );
    }

    let dir = match team_dir(&state) {
        Ok(d) => d,
        Err(resp) => return resp,
    };

    let _guard = state.orchestrator.team_yaml_lock.lock().await;

    let removed = match agent_loader::delete_agent_definition_by_name(&dir, &name).await {
        Ok(r) => r,
        Err(err) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": err.to_string()})),
            );
        }
    };
    if !removed {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": "team member not found"})),
        );
    }

    let reload_result = state.orchestrator.reload_agents().await;
    (
        StatusCode::OK,
        Json(serde_json::json!({
            "deleted": name,
            "reloaded": reload_result.is_ok(),
            "reload_error": reload_result.err().map(|e| e.to_string()),
        })),
    )
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
