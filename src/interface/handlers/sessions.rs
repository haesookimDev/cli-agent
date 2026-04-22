//! `/v1/sessions/*` endpoints — session CRUD, run list, and message list.

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::interface::api::{json_value, ApiState, ListQuery};

#[derive(Debug, Deserialize)]
pub(crate) struct CreateSessionRequest {
    pub session_id: Option<Uuid>,
}

#[derive(Debug, Serialize)]
pub(crate) struct CreateSessionResponse {
    pub session_id: Uuid,
}

pub(crate) async fn create_session_handler(
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

    let req = if body.is_empty() {
        CreateSessionRequest { session_id: None }
    } else {
        match serde_json::from_slice::<CreateSessionRequest>(body.as_ref()) {
            Ok(v) => v,
            Err(err) => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(serde_json::json!({"error": err.to_string()})),
                );
            }
        }
    };

    let session_id = req.session_id.unwrap_or_else(Uuid::new_v4);
    if let Err(err) = state.orchestrator.create_session(session_id).await {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": err.to_string()})),
        );
    }

    (
        StatusCode::CREATED,
        Json(json_value(CreateSessionResponse { session_id })),
    )
}

pub(crate) async fn list_sessions_handler(
    State(state): State<ApiState>,
    Query(query): Query<ListQuery>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(err) = state.auth.verify_headers(&headers, &[]) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error": err.to_string()})),
        );
    }

    let limit = query.limit.unwrap_or(100).clamp(1, 500);
    match state.orchestrator.list_sessions(limit).await {
        Ok(sessions) => (StatusCode::OK, Json(json_value(sessions))),
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": err.to_string()})),
        ),
    }
}

pub(crate) async fn get_session_handler(
    State(state): State<ApiState>,
    Path(session_id): Path<String>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(err) = state.auth.verify_headers(&headers, &[]) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error": err.to_string()})),
        );
    }

    let session_id = match Uuid::parse_str(session_id.as_str()) {
        Ok(v) => v,
        Err(err) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": err.to_string()})),
            );
        }
    };

    match state.orchestrator.get_session(session_id).await {
        Ok(Some(summary)) => (StatusCode::OK, Json(json_value(summary))),
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": "session not found"})),
        ),
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": err.to_string()})),
        ),
    }
}

pub(crate) async fn delete_session_handler(
    State(state): State<ApiState>,
    Path(session_id): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    if let Err(err) = state.auth.verify_headers(&headers, body.as_ref()) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error": err.to_string()})),
        );
    }

    let session_id = match Uuid::parse_str(session_id.as_str()) {
        Ok(v) => v,
        Err(err) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": err.to_string()})),
            );
        }
    };

    match state.orchestrator.delete_session(session_id).await {
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

pub(crate) async fn list_session_runs_handler(
    State(state): State<ApiState>,
    Path(session_id): Path<String>,
    Query(query): Query<ListQuery>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(err) = state.auth.verify_headers(&headers, &[]) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error": err.to_string()})),
        );
    }

    let session_id = match Uuid::parse_str(session_id.as_str()) {
        Ok(v) => v,
        Err(err) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": err.to_string()})),
            );
        }
    };
    let limit = query.limit.unwrap_or(100).clamp(1, 500);

    match state
        .orchestrator
        .list_session_runs(session_id, limit)
        .await
    {
        Ok(runs) => (StatusCode::OK, Json(json_value(runs))),
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": err.to_string()})),
        ),
    }
}

pub(crate) async fn list_session_messages_handler(
    State(state): State<ApiState>,
    Path(session_id): Path<String>,
    Query(query): Query<ListQuery>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(err) = state.auth.verify_headers(&headers, &[]) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error": err.to_string()})),
        );
    }

    let session_id = match Uuid::parse_str(session_id.as_str()) {
        Ok(v) => v,
        Err(err) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": err.to_string()})),
            );
        }
    };
    let limit = query.limit.unwrap_or(200).clamp(1, 1000);

    match state
        .orchestrator
        .get_session_messages(session_id, limit)
        .await
    {
        Ok(messages) => (StatusCode::OK, Json(json_value(messages))),
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": err.to_string()})),
        ),
    }
}
