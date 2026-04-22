//! `/v1/schedules` endpoints — cron-style scheduled runs.

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use uuid::Uuid;

use crate::interface::api::{ApiState, ListQuery};

pub(crate) async fn create_schedule_handler(
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

    let req = match serde_json::from_slice::<crate::types::CreateScheduleRequest>(body.as_ref()) {
        Ok(v) => v,
        Err(err) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": err.to_string()})),
            );
        }
    };

    match state.orchestrator.create_schedule(req).await {
        Ok(schedule) => (StatusCode::CREATED, Json(serde_json::json!(schedule))),
        Err(err) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": err.to_string()})),
        ),
    }
}

pub(crate) async fn list_schedules_handler(
    State(state): State<ApiState>,
    Query(q): Query<ListQuery>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(err) = state.auth.verify_headers(&headers, &[]) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error": err.to_string()})),
        );
    }

    let limit = q.limit.unwrap_or(100);
    match state.orchestrator.list_schedules(limit).await {
        Ok(list) => (StatusCode::OK, Json(serde_json::json!(list))),
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": err.to_string()})),
        ),
    }
}

pub(crate) async fn get_schedule_handler(
    State(state): State<ApiState>,
    Path(schedule_id): Path<Uuid>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(err) = state.auth.verify_headers(&headers, &[]) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error": err.to_string()})),
        );
    }

    match state.orchestrator.get_schedule(schedule_id).await {
        Ok(Some(schedule)) => (StatusCode::OK, Json(serde_json::json!(schedule))),
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": "schedule not found"})),
        ),
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": err.to_string()})),
        ),
    }
}

pub(crate) async fn update_schedule_handler(
    State(state): State<ApiState>,
    Path(schedule_id): Path<Uuid>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    if let Err(err) = state.auth.verify_headers(&headers, body.as_ref()) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error": err.to_string()})),
        );
    }

    let req = match serde_json::from_slice::<crate::types::UpdateScheduleRequest>(body.as_ref()) {
        Ok(v) => v,
        Err(err) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": err.to_string()})),
            );
        }
    };

    match state.orchestrator.update_schedule(schedule_id, req).await {
        Ok(Some(schedule)) => (StatusCode::OK, Json(serde_json::json!(schedule))),
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": "schedule not found"})),
        ),
        Err(err) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": err.to_string()})),
        ),
    }
}

pub(crate) async fn delete_schedule_handler(
    State(state): State<ApiState>,
    Path(schedule_id): Path<Uuid>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    if let Err(err) = state.auth.verify_headers(&headers, body.as_ref()) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error": err.to_string()})),
        );
    }

    match state.orchestrator.delete_schedule(schedule_id).await {
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
