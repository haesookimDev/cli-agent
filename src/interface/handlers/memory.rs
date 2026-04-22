//! `/v1/memory/*` endpoints — session and global memory item CRUD/search.

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use serde::Deserialize;
use uuid::Uuid;

use crate::interface::api::{json_value, ApiState};

#[derive(Debug, Deserialize)]
pub(crate) struct MemoryListQuery {
    pub limit: Option<usize>,
    pub query: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct CreateSessionMemoryRequest {
    pub content: String,
    pub scope: Option<String>,
    pub importance: Option<f64>,
    pub source_ref: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct UpdateSessionMemoryRequest {
    pub content: Option<String>,
    pub scope: Option<String>,
    pub importance: Option<f64>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct CreateGlobalMemoryRequest {
    pub topic: String,
    pub content: String,
    pub importance: Option<f64>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct UpdateGlobalMemoryRequest {
    pub topic: Option<String>,
    pub content: Option<String>,
    pub importance: Option<f64>,
}

pub(crate) async fn list_session_memory_items_handler(
    State(state): State<ApiState>,
    Path(session_id): Path<String>,
    Query(query): Query<MemoryListQuery>,
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
    let limit = query.limit.unwrap_or(100).clamp(1, 1000);
    let query_text = query
        .query
        .as_ref()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty());

    match state
        .orchestrator
        .list_session_memory_items(session_id, query_text, limit)
        .await
    {
        Ok(items) => (StatusCode::OK, Json(json_value(items))),
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": err.to_string()})),
        ),
    }
}

pub(crate) async fn create_session_memory_item_handler(
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

    let req = match serde_json::from_slice::<CreateSessionMemoryRequest>(body.as_ref()) {
        Ok(v) => v,
        Err(err) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": err.to_string()})),
            );
        }
    };

    let content = req.content.trim();
    if content.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "content is required"})),
        );
    }
    let scope = req
        .scope
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("manual_note");
    let importance = req.importance.unwrap_or(0.6).clamp(0.0, 1.0);
    let source_ref = req
        .source_ref
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());

    match state
        .orchestrator
        .add_session_memory_item(session_id, scope, content, importance, source_ref)
        .await
    {
        Ok(id) => (StatusCode::CREATED, Json(serde_json::json!({ "id": id }))),
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": err.to_string()})),
        ),
    }
}

pub(crate) async fn update_session_memory_item_handler(
    State(state): State<ApiState>,
    Path(memory_id): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    if let Err(err) = state.auth.verify_headers(&headers, body.as_ref()) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error": err.to_string()})),
        );
    }

    let req = match serde_json::from_slice::<UpdateSessionMemoryRequest>(body.as_ref()) {
        Ok(v) => v,
        Err(err) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": err.to_string()})),
            );
        }
    };

    let content = req
        .content
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty());
    let scope = req
        .scope
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty());
    let importance = req.importance.map(|v| v.clamp(0.0, 1.0));

    if content.is_none() && scope.is_none() && importance.is_none() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "no fields to update"})),
        );
    }

    match state
        .orchestrator
        .update_session_memory_item(
            memory_id.as_str(),
            content.as_deref(),
            importance,
            scope.as_deref(),
        )
        .await
    {
        Ok(true) => (
            StatusCode::OK,
            Json(serde_json::json!({"status": "updated"})),
        ),
        Ok(false) => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": "memory item not found"})),
        ),
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": err.to_string()})),
        ),
    }
}

pub(crate) async fn delete_session_memory_item_handler(
    State(state): State<ApiState>,
    Path(memory_id): Path<String>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(err) = state.auth.verify_headers(&headers, &[]) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error": err.to_string()})),
        );
    }

    match state
        .orchestrator
        .delete_session_memory_item(memory_id.as_str())
        .await
    {
        Ok(true) => (
            StatusCode::OK,
            Json(serde_json::json!({"status": "deleted"})),
        ),
        Ok(false) => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": "memory item not found"})),
        ),
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": err.to_string()})),
        ),
    }
}

pub(crate) async fn list_global_memory_items_handler(
    State(state): State<ApiState>,
    Query(query): Query<MemoryListQuery>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(err) = state.auth.verify_headers(&headers, &[]) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error": err.to_string()})),
        );
    }

    let limit = query.limit.unwrap_or(100).clamp(1, 1000);
    let query_text = query
        .query
        .as_ref()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty());

    match state
        .orchestrator
        .list_global_memory_items(query_text, limit)
        .await
    {
        Ok(items) => (StatusCode::OK, Json(json_value(items))),
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": err.to_string()})),
        ),
    }
}

pub(crate) async fn create_global_memory_item_handler(
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

    let req = match serde_json::from_slice::<CreateGlobalMemoryRequest>(body.as_ref()) {
        Ok(v) => v,
        Err(err) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": err.to_string()})),
            );
        }
    };

    let topic = req.topic.trim();
    let content = req.content.trim();
    if topic.is_empty() || content.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "topic and content are required"})),
        );
    }
    let importance = req.importance.unwrap_or(0.7).clamp(0.0, 1.0);

    match state
        .orchestrator
        .add_global_memory_item(topic, content, importance)
        .await
    {
        Ok(id) => (StatusCode::CREATED, Json(serde_json::json!({ "id": id }))),
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": err.to_string()})),
        ),
    }
}

pub(crate) async fn update_global_memory_item_handler(
    State(state): State<ApiState>,
    Path(knowledge_id): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    if let Err(err) = state.auth.verify_headers(&headers, body.as_ref()) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error": err.to_string()})),
        );
    }

    let req = match serde_json::from_slice::<UpdateGlobalMemoryRequest>(body.as_ref()) {
        Ok(v) => v,
        Err(err) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": err.to_string()})),
            );
        }
    };

    let topic = req
        .topic
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty());
    let content = req
        .content
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty());
    let importance = req.importance.map(|v| v.clamp(0.0, 1.0));

    if topic.is_none() && content.is_none() && importance.is_none() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "no fields to update"})),
        );
    }

    match state
        .orchestrator
        .update_global_memory_item(
            knowledge_id.as_str(),
            topic.as_deref(),
            content.as_deref(),
            importance,
        )
        .await
    {
        Ok(true) => (
            StatusCode::OK,
            Json(serde_json::json!({"status": "updated"})),
        ),
        Ok(false) => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": "global memory item not found"})),
        ),
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": err.to_string()})),
        ),
    }
}
