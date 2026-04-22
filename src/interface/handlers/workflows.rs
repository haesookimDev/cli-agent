//! `/v1/workflows/*` endpoints — template CRUD and execution, plus
//! `save-workflow` which persists a template from a completed run.

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use serde::Deserialize;
use uuid::Uuid;

use crate::interface::api::{ApiState, ExecuteWorkflowRequest, ListQuery};

#[derive(Debug, Deserialize)]
pub(crate) struct CreateWorkflowRequest {
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub graph_template: Option<crate::types::WorkflowGraphTemplate>,
    pub parameters: Option<Vec<crate::types::WorkflowParameter>>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct SaveWorkflowRequest {
    pub name: String,
    #[serde(default)]
    pub description: String,
}

pub(crate) async fn list_workflows_handler(
    State(state): State<ApiState>,
    Query(q): Query<ListQuery>,
) -> impl IntoResponse {
    let limit = q.limit.unwrap_or(50).clamp(1, 200);
    match state.orchestrator.list_workflows(limit).await {
        Ok(workflows) => (StatusCode::OK, Json(serde_json::json!(workflows))),
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": err.to_string()})),
        ),
    }
}

pub(crate) async fn get_workflow_handler(
    State(state): State<ApiState>,
    Path(workflow_id): Path<String>,
) -> impl IntoResponse {
    match state.orchestrator.get_workflow(&workflow_id).await {
        Ok(Some(wf)) => (StatusCode::OK, Json(serde_json::json!(wf))),
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": "workflow not found"})),
        ),
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": err.to_string()})),
        ),
    }
}

pub(crate) async fn create_workflow_handler(
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

    let req = match serde_json::from_slice::<CreateWorkflowRequest>(body.as_ref()) {
        Ok(v) => v,
        Err(err) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": err.to_string()})),
            );
        }
    };

    let now = chrono::Utc::now();
    let template = crate::types::WorkflowTemplate {
        id: Uuid::new_v4().to_string(),
        name: req.name,
        description: req.description,
        created_at: now,
        updated_at: now,
        source_run_id: None,
        graph_template: req
            .graph_template
            .unwrap_or(crate::types::WorkflowGraphTemplate { nodes: Vec::new() }),
        parameters: req.parameters.unwrap_or_default(),
        source: Default::default(),
    };

    match state.orchestrator.memory().save_workflow(&template).await {
        Ok(_) => (StatusCode::CREATED, Json(serde_json::json!(template))),
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": err.to_string()})),
        ),
    }
}

pub(crate) async fn delete_workflow_handler(
    State(state): State<ApiState>,
    Path(workflow_id): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    if let Err(err) = state.auth.verify_headers(&headers, body.as_ref()) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error": err.to_string()})),
        );
    }

    match state.orchestrator.delete_workflow(&workflow_id).await {
        Ok(_) => (
            StatusCode::OK,
            Json(serde_json::json!({"status": "deleted"})),
        ),
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": err.to_string()})),
        ),
    }
}

pub(crate) async fn execute_workflow_handler(
    State(state): State<ApiState>,
    Path(workflow_id): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    if let Err(err) = state.auth.verify_headers(&headers, body.as_ref()) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error": err.to_string()})),
        );
    }

    let req = match serde_json::from_slice::<ExecuteWorkflowRequest>(body.as_ref()) {
        Ok(v) => v,
        Err(err) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": err.to_string()})),
            );
        }
    };

    match state
        .orchestrator
        .execute_workflow(&workflow_id, req.parameters, req.session_id)
        .await
    {
        Ok(submission) => (StatusCode::OK, Json(serde_json::json!(submission))),
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": err.to_string()})),
        ),
    }
}

pub(crate) async fn save_workflow_from_run_handler(
    State(state): State<ApiState>,
    Path(run_id): Path<Uuid>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    if let Err(err) = state.auth.verify_headers(&headers, body.as_ref()) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error": err.to_string()})),
        );
    }

    let req = match serde_json::from_slice::<SaveWorkflowRequest>(body.as_ref()) {
        Ok(v) => v,
        Err(err) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": err.to_string()})),
            );
        }
    };

    match state
        .orchestrator
        .save_workflow_from_run(run_id, req.name, req.description)
        .await
    {
        Ok(template) => (StatusCode::CREATED, Json(serde_json::json!(template))),
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": err.to_string()})),
        ),
    }
}
