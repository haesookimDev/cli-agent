use std::net::SocketAddr;
use std::time::Duration;

use async_stream::stream;
use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use axum::response::{Html, IntoResponse};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::orchestrator::Orchestrator;
use crate::types::{RunRequest, TaskProfile};
use crate::webhook::AuthManager;

const DASHBOARD_HTML: &str = include_str!("dashboard.html");

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
struct TraceQuery {
    limit: Option<usize>,
}

#[derive(Debug, Deserialize)]
struct StreamQuery {
    poll_ms: Option<u64>,
    behavior: Option<bool>,
    behavior_limit: Option<usize>,
}

#[derive(Debug, Deserialize)]
struct ListQuery {
    limit: Option<usize>,
}

#[derive(Debug, Deserialize)]
struct CloneRunRequest {
    session_id: Option<Uuid>,
}

#[derive(Debug, Deserialize)]
struct DeliveryListQuery {
    limit: Option<usize>,
    dead_letter: Option<bool>,
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
        .route("/dashboard", get(dashboard_handler))
        .route(
            "/v1/sessions",
            post(create_session_handler).get(list_sessions_handler),
        )
        .route("/v1/sessions/:session_id", get(get_session_handler))
        .route(
            "/v1/sessions/:session_id/runs",
            get(list_session_runs_handler),
        )
        .route("/v1/runs", post(create_run_handler).get(list_runs_handler))
        .route("/v1/runs/:run_id", get(get_run_handler))
        .route("/v1/runs/:run_id/cancel", post(cancel_run_handler))
        .route("/v1/runs/:run_id/pause", post(pause_run_handler))
        .route("/v1/runs/:run_id/resume", post(resume_run_handler))
        .route("/v1/runs/:run_id/retry", post(retry_run_handler))
        .route("/v1/runs/:run_id/clone", post(clone_run_handler))
        .route("/v1/runs/:run_id/behavior", get(get_run_behavior_handler))
        .route("/v1/runs/:run_id/trace", get(get_run_trace_handler))
        .route("/v1/runs/:run_id/stream", get(stream_run_handler))
        .route(
            "/v1/webhooks/endpoints",
            post(register_webhook_handler).get(list_webhooks_handler),
        )
        .route(
            "/v1/webhooks/deliveries",
            get(list_webhook_deliveries_handler),
        )
        .route(
            "/v1/webhooks/deliveries/:delivery_id/retry",
            post(retry_webhook_delivery_handler),
        )
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
        Json(serde_json::to_value(CreateSessionResponse { session_id }).unwrap()),
    )
}

async fn dashboard_handler() -> Html<&'static str> {
    Html(DASHBOARD_HTML)
}

async fn list_sessions_handler(
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
        Ok(sessions) => (
            StatusCode::OK,
            Json(serde_json::to_value(sessions).unwrap()),
        ),
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": err.to_string()})),
        ),
    }
}

async fn get_session_handler(
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
        Ok(Some(summary)) => (StatusCode::OK, Json(serde_json::to_value(summary).unwrap())),
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

async fn list_session_runs_handler(
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
        Ok(runs) => (StatusCode::OK, Json(serde_json::to_value(runs).unwrap())),
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": err.to_string()})),
        ),
    }
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
            );
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

async fn list_runs_handler(
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
    match state.orchestrator.list_recent_runs(limit).await {
        Ok(runs) => (StatusCode::OK, Json(serde_json::to_value(runs).unwrap())),
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
            );
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

async fn cancel_run_handler(
    State(state): State<ApiState>,
    Path(run_id): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    if let Err(err) = state.auth.verify_headers(&headers, body.as_ref()) {
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
            );
        }
    };
    match state.orchestrator.cancel_run(run_id).await {
        Ok(cancelled) => (
            StatusCode::OK,
            Json(serde_json::json!({ "cancel_requested": cancelled })),
        ),
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": err.to_string()})),
        ),
    }
}

async fn pause_run_handler(
    State(state): State<ApiState>,
    Path(run_id): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    if let Err(err) = state.auth.verify_headers(&headers, body.as_ref()) {
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
            );
        }
    };
    match state.orchestrator.pause_run(run_id).await {
        Ok(paused) => (
            StatusCode::OK,
            Json(serde_json::json!({ "paused": paused })),
        ),
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": err.to_string()})),
        ),
    }
}

async fn resume_run_handler(
    State(state): State<ApiState>,
    Path(run_id): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    if let Err(err) = state.auth.verify_headers(&headers, body.as_ref()) {
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
            );
        }
    };
    match state.orchestrator.resume_run(run_id).await {
        Ok(resumed) => (
            StatusCode::OK,
            Json(serde_json::json!({ "resumed": resumed })),
        ),
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": err.to_string()})),
        ),
    }
}

async fn retry_run_handler(
    State(state): State<ApiState>,
    Path(run_id): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    if let Err(err) = state.auth.verify_headers(&headers, body.as_ref()) {
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
            );
        }
    };
    match state.orchestrator.retry_run(run_id).await {
        Ok(sub) => (
            StatusCode::ACCEPTED,
            Json(serde_json::to_value(sub).unwrap()),
        ),
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": err.to_string()})),
        ),
    }
}

async fn clone_run_handler(
    State(state): State<ApiState>,
    Path(run_id): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    if let Err(err) = state.auth.verify_headers(&headers, body.as_ref()) {
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
            );
        }
    };

    let req = if body.is_empty() {
        CloneRunRequest { session_id: None }
    } else {
        match serde_json::from_slice::<CloneRunRequest>(body.as_ref()) {
            Ok(v) => v,
            Err(err) => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(serde_json::json!({"error": err.to_string()})),
                );
            }
        }
    };

    match state.orchestrator.clone_run(run_id, req.session_id).await {
        Ok(sub) => (
            StatusCode::ACCEPTED,
            Json(serde_json::to_value(sub).unwrap()),
        ),
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": err.to_string()})),
        ),
    }
}

async fn get_run_trace_handler(
    State(state): State<ApiState>,
    Path(run_id): Path<String>,
    Query(query): Query<TraceQuery>,
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
            );
        }
    };

    let limit = query.limit.unwrap_or(5_000).clamp(1, 20_000);
    match state.orchestrator.get_run_trace(run_id, limit).await {
        Ok(Some(trace)) => (StatusCode::OK, Json(serde_json::to_value(trace).unwrap())),
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

async fn get_run_behavior_handler(
    State(state): State<ApiState>,
    Path(run_id): Path<String>,
    Query(query): Query<TraceQuery>,
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
            );
        }
    };
    let limit = query.limit.unwrap_or(2_000).clamp(1, 20_000);

    match state.orchestrator.get_run_behavior(run_id, limit).await {
        Ok(Some(behavior)) => (
            StatusCode::OK,
            Json(serde_json::to_value(behavior).unwrap()),
        ),
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

async fn stream_run_handler(
    State(state): State<ApiState>,
    Path(run_id): Path<String>,
    Query(query): Query<StreamQuery>,
    headers: HeaderMap,
) -> axum::response::Response {
    if let Err(err) = state.auth.verify_headers(&headers, &[]) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error": err.to_string()})),
        )
            .into_response();
    }

    let run_id = match Uuid::parse_str(run_id.as_str()) {
        Ok(v) => v,
        Err(err) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": err.to_string()})),
            )
                .into_response();
        }
    };

    let poll_ms = query.poll_ms.unwrap_or(500).clamp(200, 5_000);
    let include_behavior = query.behavior.unwrap_or(false);
    let behavior_limit = query.behavior_limit.unwrap_or(2_000).clamp(1, 20_000);
    let orchestrator = state.orchestrator.clone();

    let event_stream = stream! {
        let mut last_seq: i64 = 0;
        let mut idle_terminal_ticks = 0_u8;

        loop {
            let events_result = orchestrator
                .list_run_action_events_since(run_id, last_seq, 500)
                .await;

            let events = match events_result {
                Ok(events) => events,
                Err(err) => {
                    let payload = serde_json::json!({"error": err.to_string()});
                    yield Ok::<SseEvent, std::convert::Infallible>(
                        SseEvent::default().event("error").data(payload.to_string())
                    );
                    break;
                }
            };
            let had_new_events = !events.is_empty();

            for event in events {
                last_seq = event.seq;
                let payload = match serde_json::to_string(&event) {
                    Ok(v) => v,
                    Err(_) => "{\"error\":\"serialization_failed\"}".to_string(),
                };
                yield Ok::<SseEvent, std::convert::Infallible>(
                    SseEvent::default().event("action_event").data(payload),
                );
            }

            if include_behavior && had_new_events {
                if let Ok(Some(behavior)) = orchestrator.get_run_behavior(run_id, behavior_limit).await {
                    if let Ok(payload) = serde_json::to_string(&behavior) {
                        yield Ok::<SseEvent, std::convert::Infallible>(
                            SseEvent::default().event("behavior_snapshot").data(payload),
                        );
                    }
                }
            }

            match orchestrator.get_run(run_id).await {
                Ok(Some(run)) if run.status.is_terminal() => {
                    if include_behavior {
                        if let Ok(Some(behavior)) = orchestrator.get_run_behavior(run_id, behavior_limit).await {
                            if let Ok(payload) = serde_json::to_string(&behavior) {
                                yield Ok::<SseEvent, std::convert::Infallible>(
                                    SseEvent::default().event("behavior_snapshot").data(payload),
                                );
                            }
                        }
                    }
                    if last_seq > 0 {
                        idle_terminal_ticks = idle_terminal_ticks.saturating_add(1);
                        if idle_terminal_ticks >= 2 {
                            let payload = serde_json::json!({
                                "run_id": run.run_id,
                                "status": run.status,
                            });
                            yield Ok::<SseEvent, std::convert::Infallible>(
                                SseEvent::default().event("run_terminal").data(payload.to_string()),
                            );
                            break;
                        }
                    } else {
                        let payload = serde_json::json!({
                            "run_id": run.run_id,
                            "status": run.status,
                        });
                        yield Ok::<SseEvent, std::convert::Infallible>(
                            SseEvent::default().event("run_terminal").data(payload.to_string()),
                        );
                        break;
                    }
                }
                Ok(Some(_)) => {
                    idle_terminal_ticks = 0;
                }
                Ok(None) => {
                    yield Ok::<SseEvent, std::convert::Infallible>(
                        SseEvent::default().event("error").data("{\"error\":\"run_not_found\"}"),
                    );
                    break;
                }
                Err(err) => {
                    let payload = serde_json::json!({"error": err.to_string()});
                    yield Ok::<SseEvent, std::convert::Infallible>(
                        SseEvent::default().event("error").data(payload.to_string())
                    );
                    break;
                }
            }

            tokio::time::sleep(Duration::from_millis(poll_ms)).await;
        }
    };

    Sse::new(event_stream)
        .keep_alive(KeepAlive::new().interval(Duration::from_secs(10)))
        .into_response()
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
            );
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

async fn list_webhooks_handler(
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
        Ok(endpoints) => (
            StatusCode::OK,
            Json(serde_json::to_value(endpoints).unwrap()),
        ),
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": err.to_string()})),
        ),
    }
}

async fn list_webhook_deliveries_handler(
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
        Ok(deliveries) => (
            StatusCode::OK,
            Json(serde_json::to_value(deliveries).unwrap()),
        ),
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": err.to_string()})),
        ),
    }
}

async fn retry_webhook_delivery_handler(
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
