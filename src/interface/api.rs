use std::net::SocketAddr;

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::orchestrator::Orchestrator;
use crate::types::{RunRequest, TaskProfile};
use crate::webhook::AuthManager;

#[derive(Clone)]
pub struct ApiState {
    pub orchestrator: Orchestrator,
    pub auth: std::sync::Arc<AuthManager>,
}

#[derive(Debug, Deserialize)]
struct CreateSessionRequest {
    session_id: Option<Uuid>,
}

#[derive(Debug, Serialize)]
struct CreateSessionResponse {
    session_id: Uuid,
}

#[derive(Debug, Deserialize)]
struct CreateRunRequest {
    task: String,
    #[serde(default)]
    profile: Option<TaskProfile>,
    session_id: Option<Uuid>,
}

#[derive(Debug, Deserialize)]
struct RegisterWebhookRequest {
    url: String,
    events: Vec<String>,
    secret: String,
}

#[derive(Debug, Deserialize)]
struct TestWebhookRequest {
    event: String,
    payload: serde_json::Value,
}

pub fn router(state: ApiState) -> Router {
    Router::new()
        .route("/v1/sessions", post(create_session_handler))
        .route("/v1/runs", post(create_run_handler))
        .route("/v1/runs/:run_id", get(get_run_handler))
        .route("/v1/webhooks/endpoints", post(register_webhook_handler))
        .route("/v1/webhooks/test", post(test_webhook_handler))
        .with_state(state)
}

pub async fn serve(addr: SocketAddr, state: ApiState) -> anyhow::Result<()> {
    let app = router(state);
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

async fn create_session_handler(
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
                )
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
        Json(serde_json::to_value(CreateSessionResponse { session_id }).unwrap()),
    )
}

async fn create_run_handler(
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

    let req = match serde_json::from_slice::<CreateRunRequest>(body.as_ref()) {
        Ok(v) => v,
        Err(err) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": err.to_string()})),
            )
        }
    };

    let run_req = RunRequest {
        task: req.task,
        profile: req.profile.unwrap_or(TaskProfile::General),
        session_id: req.session_id,
    };

    match state.orchestrator.submit_run(run_req).await {
        Ok(submission) => (
            StatusCode::ACCEPTED,
            Json(serde_json::to_value(submission).unwrap()),
        ),
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": err.to_string()})),
        ),
    }
}

async fn get_run_handler(
    State(state): State<ApiState>,
    Path(run_id): Path<String>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(err) = state.auth.verify_headers(&headers, &[]) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error": err.to_string()})),
        );
    }

    let run_id = match Uuid::parse_str(run_id.as_str()) {
        Ok(v) => v,
        Err(err) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": err.to_string()})),
            )
        }
    };

    match state.orchestrator.get_run(run_id).await {
        Ok(Some(run)) => (StatusCode::OK, Json(serde_json::to_value(run).unwrap())),
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": "run not found"})),
        ),
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": err.to_string()})),
        ),
    }
}

async fn register_webhook_handler(
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
            )
        }
    };

    match state
        .orchestrator
        .register_webhook(req.url.as_str(), req.events.as_slice(), req.secret.as_str())
        .await
    {
        Ok(endpoint) => (
            StatusCode::CREATED,
            Json(serde_json::to_value(endpoint).unwrap()),
        ),
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": err.to_string()})),
        ),
    }
}

async fn test_webhook_handler(
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
            )
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
