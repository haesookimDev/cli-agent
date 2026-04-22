//! `/v1/cluster/*` endpoints — cluster member list + cluster-wide run control.

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use uuid::Uuid;

use crate::interface::api::{ApiState, ListQuery};
use crate::types::ClusterRunRequest;

pub(crate) async fn list_cluster_members_handler(
    State(state): State<ApiState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(e) = state.auth.verify_headers(&headers, &[]) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error": e.to_string()})),
        )
            .into_response();
    }
    let names = state.cluster.member_names();
    Json(serde_json::json!({ "members": names })).into_response()
}

pub(crate) async fn submit_cluster_run_handler(
    State(state): State<ApiState>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    if let Err(e) = state.auth.verify_headers(&headers, body.as_ref()) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error": e.to_string()})),
        )
            .into_response();
    }
    let req: ClusterRunRequest = match serde_json::from_slice(body.as_ref()) {
        Ok(r) => r,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": e.to_string()})),
            )
                .into_response();
        }
    };
    match state.cluster.submit(req).await {
        Ok(record) => (StatusCode::CREATED, Json(serde_json::json!(record))).into_response(),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

pub(crate) async fn list_cluster_runs_handler(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Query(q): Query<ListQuery>,
) -> impl IntoResponse {
    if let Err(e) = state.auth.verify_headers(&headers, &[]) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error": e.to_string()})),
        )
            .into_response();
    }
    let runs = state.cluster.list(q.limit.unwrap_or(20));
    Json(serde_json::json!({ "cluster_runs": runs })).into_response()
}

pub(crate) async fn get_cluster_run_handler(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Path(cluster_run_id): Path<Uuid>,
) -> impl IntoResponse {
    if let Err(e) = state.auth.verify_headers(&headers, &[]) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error": e.to_string()})),
        )
            .into_response();
    }
    match state.cluster.get(cluster_run_id) {
        Some(record) => Json(serde_json::json!(record)).into_response(),
        None => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": "cluster run not found"})),
        )
            .into_response(),
    }
}
