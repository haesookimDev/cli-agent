//! `/v1/webhooks/*` endpoints — endpoint registration and delivery control.

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use serde::Deserialize;

use crate::interface::api::{json_value, ApiState};

#[derive(Debug, Deserialize)]
pub(crate) struct DeliveryListQuery {
    pub limit: Option<usize>,
    pub dead_letter: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RegisterWebhookRequest {
    pub url: String,
    pub events: Vec<String>,
    pub secret: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct TestWebhookRequest {
    pub event: String,
    pub payload: serde_json::Value,
}

pub(crate) async fn register_webhook_handler(
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

    let req = match serde_json::from_slice::<RegisterWebhookRequest>(body.as_ref()) {
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
        .register_webhook(req.url.as_str(), req.events.as_slice(), req.secret.as_str())
        .await
    {
        Ok(endpoint) => (StatusCode::CREATED, Json(json_value(endpoint))),
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": err.to_string()})),
        ),
    }
}

pub(crate) async fn list_webhooks_handler(
    State(state): State<ApiState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(err) = state.auth.verify_headers(&headers, &[]) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error": err.to_string()})),
        );
    }

    match state.orchestrator.list_webhooks().await {
        Ok(endpoints) => (StatusCode::OK, Json(json_value(endpoints))),
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": err.to_string()})),
        ),
    }
}

pub(crate) async fn list_webhook_deliveries_handler(
    State(state): State<ApiState>,
    Query(query): Query<DeliveryListQuery>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(err) = state.auth.verify_headers(&headers, &[]) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error": err.to_string()})),
        );
    }
    let limit = query.limit.unwrap_or(100).clamp(1, 500);
    let dead_letter_only = query.dead_letter.unwrap_or(false);

    match state
        .orchestrator
        .list_webhook_deliveries(dead_letter_only, limit)
        .await
    {
        Ok(deliveries) => (StatusCode::OK, Json(json_value(deliveries))),
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": err.to_string()})),
        ),
    }
}

pub(crate) async fn retry_webhook_delivery_handler(
    State(state): State<ApiState>,
    Path(delivery_id): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    if let Err(err) = state.auth.verify_headers(&headers, body.as_ref()) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error": err.to_string()})),
        );
    }

    let delivery_id = match delivery_id.parse::<i64>() {
        Ok(v) => v,
        Err(err) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": err.to_string()})),
            );
        }
    };

    match state.orchestrator.retry_webhook_delivery(delivery_id).await {
        Ok(()) => (
            StatusCode::OK,
            Json(serde_json::json!({"status": "requeued"})),
        ),
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": err.to_string()})),
        ),
    }
}

pub(crate) async fn test_webhook_handler(
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

    let req = match serde_json::from_slice::<TestWebhookRequest>(body.as_ref()) {
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
        .dispatch_webhook_event(req.event.as_str(), req.payload)
        .await
    {
        Ok(_) => (
            StatusCode::OK,
            Json(serde_json::json!({"status": "queued"})),
        ),
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": err.to_string()})),
        ),
    }
}
