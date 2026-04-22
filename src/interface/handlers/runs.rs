//! `/v1/runs/*` endpoints — run lifecycle (submit/cancel/pause/resume/retry/
//! clone), query (list/get/trace/behavior/coder-sessions), and live
//! streaming (SSE + WebSocket).

use std::time::Duration;

use async_stream::stream;
use axum::body::Bytes;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use axum::response::IntoResponse;
use axum::Json;
use futures::{stream::StreamExt, SinkExt};
use serde::Deserialize;
use uuid::Uuid;

use crate::interface::api::{json_value, ApiState, ListQuery, WsAuthQuery};
use crate::types::{RunRequest, TaskProfile};

#[derive(Debug, Deserialize)]
pub(crate) struct CreateRunRequest {
    pub task: String,
    #[serde(default)]
    pub profile: Option<TaskProfile>,
    pub session_id: Option<Uuid>,
    pub repo_url: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct TraceQuery {
    pub limit: Option<usize>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct StreamQuery {
    pub poll_ms: Option<u64>,
    pub behavior: Option<bool>,
    pub behavior_limit: Option<usize>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct CloneRunRequest {
    pub session_id: Option<Uuid>,
}

pub(crate) async fn list_active_runs_handler(
    State(state): State<ApiState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(err) = state.auth.verify_headers(&headers, &[]) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error": err.to_string()})),
        );
    }

    let active = state.orchestrator.list_active_runs().await;
    (StatusCode::OK, Json(json_value(active)))
}

pub(crate) async fn create_run_handler(
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
        workflow_id: None,
        workflow_params: None,
        repo_url: req.repo_url,
    };

    match state.orchestrator.submit_run(run_req).await {
        Ok(submission) => (
            StatusCode::ACCEPTED,
            Json(json_value(submission)),
        ),
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": err.to_string()})),
        ),
    }
}

pub(crate) async fn list_runs_handler(
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
        Ok(runs) => (StatusCode::OK, Json(json_value(runs))),
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": err.to_string()})),
        ),
    }
}

pub(crate) async fn get_run_handler(
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
        Ok(Some(run)) => (StatusCode::OK, Json(json_value(run))),
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

pub(crate) async fn cancel_run_handler(
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

pub(crate) async fn pause_run_handler(
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

pub(crate) async fn resume_run_handler(
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

pub(crate) async fn retry_run_handler(
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
            Json(json_value(sub)),
        ),
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": err.to_string()})),
        ),
    }
}

pub(crate) async fn clone_run_handler(
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
            Json(json_value(sub)),
        ),
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": err.to_string()})),
        ),
    }
}

pub(crate) async fn get_run_trace_handler(
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
        Ok(Some(trace)) => (StatusCode::OK, Json(json_value(trace))),
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

pub(crate) async fn list_coder_sessions_handler(
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

    match state.orchestrator.list_coder_sessions(run_id).await {
        Ok(sessions) => (
            StatusCode::OK,
            Json(serde_json::json!({ "sessions": sessions })),
        ),
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": err.to_string()})),
        ),
    }
}

pub(crate) async fn get_run_behavior_handler(
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
            Json(json_value(behavior)),
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

pub(crate) async fn stream_run_handler(
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

/// WebSocket equivalent of `stream_run_handler`.
/// Clients connect to `/v1/runs/:run_id/ws?api_key=...` and receive the same
/// `RunActionEvent` JSON frames that the SSE endpoint emits, but over a persistent
/// WebSocket connection — no polling delay, no SSE keep-alive overhead.
///
/// Message format (server → client): text frames, each containing a JSON object:
///   `{"type": "action_event", "data": <RunActionEvent>}`
///   `{"type": "run_terminal", "data": {"run_id": "...", "status": "..."}}`
///   `{"type": "error", "data": {"error": "..."}}`
pub(crate) async fn ws_stream_run_handler(
    State(state): State<ApiState>,
    Path(run_id): Path<String>,
    Query(auth_query): Query<WsAuthQuery>,
    ws: WebSocketUpgrade,
) -> impl IntoResponse {
    if let Err(err) = state.auth.verify_query_params(
        &auth_query.api_key,
        &auth_query.signature,
        &auth_query.timestamp,
        &auth_query.nonce,
        &[],
    ) {
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

    let orchestrator = state.orchestrator.clone();
    ws.on_upgrade(move |socket| handle_run_ws(socket, orchestrator, run_id))
        .into_response()
}

async fn handle_run_ws(
    socket: WebSocket,
    orchestrator: crate::orchestrator::Orchestrator,
    run_id: Uuid,
) {
    let (mut sender, _receiver) = socket.split();
    let mut last_seq: i64 = 0;

    loop {
        let events = match orchestrator
            .list_run_action_events_since(run_id, last_seq, 500)
            .await
        {
            Ok(e) => e,
            Err(err) => {
                let msg = serde_json::json!({"type": "error", "data": {"error": err.to_string()}});
                let _ = sender
                    .send(Message::Text(msg.to_string()))
                    .await;
                break;
            }
        };

        for event in &events {
            last_seq = event.seq;
            if let Ok(data) = serde_json::to_string(event) {
                let msg = serde_json::json!({"type": "action_event", "data": serde_json::from_str::<serde_json::Value>(&data).unwrap_or_default()});
                if sender
                    .send(Message::Text(msg.to_string()))
                    .await
                    .is_err()
                {
                    return;
                }
            }
        }

        match orchestrator.get_run(run_id).await {
            Ok(Some(run)) if run.status.is_terminal() => {
                let msg = serde_json::json!({
                    "type": "run_terminal",
                    "data": {"run_id": run.run_id, "status": run.status}
                });
                let _ = sender.send(Message::Text(msg.to_string())).await;
                break;
            }
            Ok(None) => {
                let msg = serde_json::json!({"type": "error", "data": {"error": "run_not_found"}});
                let _ = sender.send(Message::Text(msg.to_string())).await;
                break;
            }
            Err(err) => {
                let msg = serde_json::json!({"type": "error", "data": {"error": err.to_string()}});
                let _ = sender.send(Message::Text(msg.to_string())).await;
                break;
            }
            Ok(Some(_)) => {}
        }

        tokio::time::sleep(Duration::from_millis(150)).await;
    }
}
