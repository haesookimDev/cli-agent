use std::net::SocketAddr;
use std::time::Duration;

use std::sync::Arc;

use async_stream::stream;
use axum::body::Bytes;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, Method, StatusCode};
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use axum::response::{Html, IntoResponse};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use futures::{SinkExt, stream::StreamExt};
use serde::{Deserialize, Serialize};
use tower_http::cors::{Any, CorsLayer};
use uuid::Uuid;

use crate::orchestrator::Orchestrator;
use crate::terminal::TerminalManager;
use crate::types::{RunRequest, TaskProfile};
use crate::webhook::AuthManager;

const DASHBOARD_HTML: &str = include_str!("dashboard.html");
const WEB_CLIENT_HTML: &str = include_str!("web_client.html");

#[derive(Clone)]
pub struct ApiState {
    pub orchestrator: Orchestrator,
    pub auth: std::sync::Arc<AuthManager>,
    pub terminal: TerminalManager,
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

#[derive(Debug, Deserialize)]
struct SaveWorkflowRequest {
    name: String,
    #[serde(default)]
    description: String,
}

#[derive(Debug, Deserialize)]
struct ExecuteWorkflowRequest {
    parameters: Option<serde_json::Value>,
    session_id: Option<Uuid>,
}

#[derive(Debug, Deserialize)]
struct CreateWorkflowRequest {
    name: String,
    #[serde(default)]
    description: String,
    graph_template: Option<crate::types::WorkflowGraphTemplate>,
    parameters: Option<Vec<crate::types::WorkflowParameter>>,
}

pub fn router(state: ApiState) -> Router {
    Router::new()
        .route("/dashboard", get(dashboard_handler))
        .route("/web-client", get(web_client_handler))
        .route(
            "/v1/sessions",
            post(create_session_handler).get(list_sessions_handler),
        )
        .route("/v1/sessions/:session_id", get(get_session_handler).delete(delete_session_handler))
        .route(
            "/v1/sessions/:session_id/runs",
            get(list_session_runs_handler),
        )
        .route(
            "/v1/sessions/:session_id/messages",
            get(list_session_messages_handler),
        )
        .route("/v1/runs/active", get(list_active_runs_handler))
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
        .route("/v1/mcp/tools", get(list_mcp_tools_handler))
        .route("/v1/mcp/tools/call", post(call_mcp_tool_handler))
        .route("/v1/mcp/servers", get(list_mcp_servers_handler))
        .route(
            "/v1/settings",
            get(get_settings_handler).patch(update_settings_handler),
        )
        .route("/v1/models", get(list_models_handler))
        .route("/v1/models/:model_id/toggle", post(toggle_model_handler))
        .route(
            "/v1/providers/:provider_name/toggle",
            post(toggle_provider_handler),
        )
        .route(
            "/v1/schedules",
            post(create_schedule_handler).get(list_schedules_handler),
        )
        .route(
            "/v1/schedules/:schedule_id",
            get(get_schedule_handler)
                .patch(update_schedule_handler)
                .delete(delete_schedule_handler),
        )
        .route(
            "/v1/workflows",
            post(create_workflow_handler).get(list_workflows_handler),
        )
        .route(
            "/v1/workflows/:workflow_id",
            get(get_workflow_handler).delete(delete_workflow_handler),
        )
        .route(
            "/v1/workflows/:workflow_id/execute",
            post(execute_workflow_handler),
        )
        .route(
            "/v1/runs/:run_id/save-workflow",
            post(save_workflow_from_run_handler),
        )
        .route(
            "/v1/terminal/sessions",
            post(create_terminal_handler).get(list_terminals_handler),
        )
        .route(
            "/v1/terminal/sessions/:id",
            delete(kill_terminal_handler),
        )
        .route(
            "/v1/terminal/sessions/:id/ws",
            get(terminal_ws_handler),
        )
        .with_state(state)
}

pub async fn serve(
    addr: SocketAddr,
    state: ApiState,
    gateway_router: Option<Router>,
) -> anyhow::Result<()> {
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods([
            Method::GET,
            Method::POST,
            Method::PATCH,
            Method::DELETE,
            Method::OPTIONS,
        ])
        .allow_headers(Any);

    let mut app = router(state).layer(cors);
    if let Some(gw) = gateway_router {
        app = app.merge(gw);
    }
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

async fn web_client_handler() -> Html<&'static str> {
    Html(WEB_CLIENT_HTML)
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

async fn delete_session_handler(
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

async fn list_session_messages_handler(
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
        Ok(messages) => (
            StatusCode::OK,
            Json(serde_json::to_value(messages).unwrap()),
        ),
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": err.to_string()})),
        ),
    }
}

async fn list_active_runs_handler(
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
    (StatusCode::OK, Json(serde_json::to_value(active).unwrap()))
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
        workflow_id: None,
        workflow_params: None,
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

// --- MCP ---

async fn list_mcp_tools_handler(
    State(state): State<ApiState>,
) -> impl IntoResponse {
    let tools = state.orchestrator.list_mcp_tools().await;
    (StatusCode::OK, Json(serde_json::json!(tools)))
}

async fn list_mcp_servers_handler(
    State(state): State<ApiState>,
) -> impl IntoResponse {
    let servers = state.orchestrator.list_mcp_servers();
    (StatusCode::OK, Json(serde_json::json!(servers)))
}

#[derive(Debug, Deserialize)]
struct CallToolRequest {
    tool_name: String,
    #[serde(default = "default_empty_object")]
    arguments: serde_json::Value,
}

fn default_empty_object() -> serde_json::Value {
    serde_json::json!({})
}

async fn call_mcp_tool_handler(
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

    let req = match serde_json::from_slice::<CallToolRequest>(body.as_ref()) {
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
        .call_mcp_tool(&req.tool_name, req.arguments)
        .await
    {
        Ok(result) => (
            StatusCode::OK,
            Json(serde_json::json!({
                "tool_name": result.tool_name,
                "succeeded": result.succeeded,
                "content": result.content,
                "error": result.error,
                "duration_ms": result.duration_ms,
            })),
        ),
        Err(err) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": err.to_string()})),
        ),
    }
}

// --- Settings & Models ---

async fn get_settings_handler(
    State(state): State<ApiState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(err) = state.auth.verify_headers(&headers, &[]) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error": err.to_string()})),
        );
    }
    let settings = state.orchestrator.get_settings();
    (StatusCode::OK, Json(serde_json::to_value(settings).unwrap()))
}

async fn update_settings_handler(
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

    let patch = match serde_json::from_slice::<crate::types::SettingsPatch>(body.as_ref()) {
        Ok(v) => v,
        Err(err) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": err.to_string()})),
            );
        }
    };

    state.orchestrator.update_settings(patch).await;
    let settings = state.orchestrator.get_settings();
    (StatusCode::OK, Json(serde_json::to_value(settings).unwrap()))
}

async fn list_models_handler(
    State(state): State<ApiState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(err) = state.auth.verify_headers(&headers, &[]) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error": err.to_string()})),
        );
    }

    let router = state.orchestrator.router();
    let preferred = router.preferred_model();
    let models: Vec<serde_json::Value> = router
        .catalog()
        .iter()
        .map(|spec| {
            serde_json::json!({
                "spec": spec,
                "enabled": !router.is_model_disabled(&spec.model_id),
                "is_preferred": preferred.as_deref() == Some(spec.model_id.as_str()),
            })
        })
        .collect();

    (StatusCode::OK, Json(serde_json::json!(models)))
}

async fn toggle_model_handler(
    State(state): State<ApiState>,
    Path(model_id): Path<String>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(err) = state.auth.verify_headers(&headers, &[]) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error": err.to_string()})),
        );
    }

    let router = state.orchestrator.router();
    let currently_disabled = router.is_model_disabled(&model_id);
    router.set_model_disabled(&model_id, !currently_disabled);
    (
        StatusCode::OK,
        Json(serde_json::json!({
            "model_id": model_id,
            "enabled": currently_disabled,
        })),
    )
}

async fn toggle_provider_handler(
    State(state): State<ApiState>,
    Path(provider_name): Path<String>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(err) = state.auth.verify_headers(&headers, &[]) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error": err.to_string()})),
        );
    }

    let pk = match serde_json::from_value::<crate::router::ProviderKind>(
        serde_json::Value::String(provider_name.clone()),
    ) {
        Ok(v) => v,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": "unknown provider"})),
            );
        }
    };

    let router = state.orchestrator.router();
    let currently_disabled = router.disabled_providers().contains(&pk);
    router.set_provider_disabled(pk, !currently_disabled);
    (
        StatusCode::OK,
        Json(serde_json::json!({
            "provider": provider_name,
            "enabled": currently_disabled,
        })),
    )
}

// --- Workflows ---

async fn list_workflows_handler(
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

async fn get_workflow_handler(
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

async fn create_workflow_handler(
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
    };

    match state.orchestrator.memory().save_workflow(&template).await {
        Ok(_) => (StatusCode::CREATED, Json(serde_json::json!(template))),
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": err.to_string()})),
        ),
    }
}

async fn delete_workflow_handler(
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

async fn execute_workflow_handler(
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

async fn save_workflow_from_run_handler(
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

// --- Schedule handlers ---

async fn create_schedule_handler(
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

async fn list_schedules_handler(
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

async fn get_schedule_handler(
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

async fn update_schedule_handler(
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

async fn delete_schedule_handler(
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

// ─── Terminal handlers ───────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct CreateTerminalRequest {
    command: String,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default = "default_cols")]
    cols: u16,
    #[serde(default = "default_rows")]
    rows: u16,
    run_id: Option<Uuid>,
    session_id: Option<Uuid>,
}

fn default_cols() -> u16 {
    80
}
fn default_rows() -> u16 {
    24
}

async fn create_terminal_handler(
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

    let req: CreateTerminalRequest = match serde_json::from_slice(&body) {
        Ok(r) => r,
        Err(err) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": err.to_string()})),
            );
        }
    };

    match state.terminal.spawn(
        &req.command,
        &req.args,
        req.cols,
        req.rows,
        req.run_id,
        req.session_id,
    ) {
        Ok(id) => (StatusCode::OK, Json(serde_json::json!({"id": id}))),
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": err.to_string()})),
        ),
    }
}

async fn list_terminals_handler(
    State(state): State<ApiState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(err) = state.auth.verify_headers(&headers, &[]) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error": err.to_string()})),
        );
    }

    let list = state.terminal.list();
    (StatusCode::OK, Json(serde_json::to_value(list).unwrap()))
}

async fn kill_terminal_handler(
    State(state): State<ApiState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    if let Err(err) = state.auth.verify_headers(&headers, body.as_ref()) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error": err.to_string()})),
        );
    }

    match state.terminal.kill(&id) {
        Ok(_) => (
            StatusCode::OK,
            Json(serde_json::json!({"status": "killed"})),
        ),
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": err.to_string()})),
        ),
    }
}

#[derive(Debug, Deserialize)]
struct WsAuthQuery {
    api_key: String,
    signature: String,
    timestamp: String,
    nonce: String,
}

async fn terminal_ws_handler(
    State(state): State<ApiState>,
    Path(id): Path<String>,
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

    let session = match state.terminal.get(&id) {
        Some(s) => s,
        None => {
            return (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({"error": "terminal session not found"})),
            )
                .into_response();
        }
    };

    let terminal_mgr = state.terminal.clone();
    ws.on_upgrade(move |socket| handle_terminal_ws(socket, session, terminal_mgr))
        .into_response()
}

async fn handle_terminal_ws(
    socket: WebSocket,
    session: Arc<crate::terminal::TerminalSession>,
    manager: crate::terminal::TerminalManager,
) {
    use std::io::{Read, Write};

    let (mut ws_sender, mut ws_receiver) = socket.split();

    // Take the PTY reader (only one WS connection per session)
    let pty_reader = {
        let mut guard = session.reader.lock().await;
        match guard.take() {
            Some(r) => r,
            None => {
                let _ = ws_sender
                    .send(Message::Text(
                        serde_json::json!({"type": "error", "message": "already connected"})
                            .to_string(),
                    ))
                    .await;
                return;
            }
        }
    };

    let writer = session.writer.clone();
    let session_id_for_cleanup = session.id.clone();
    let reader_slot = session.reader.clone();

    // Bridge blocking PTY read to async via mpsc channel
    let (tx, mut rx) = tokio::sync::mpsc::channel::<Vec<u8>>(64);

    let read_handle = tokio::task::spawn_blocking(move || {
        let mut reader = pty_reader;
        let mut buf = [0u8; 4096];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    if tx.blocking_send(buf[..n].to_vec()).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
        // Return reader so it can be restored for reconnection
        reader
    });

    // Async task: mpsc → WS binary frames
    let send_handle = tokio::spawn(async move {
        while let Some(data) = rx.recv().await {
            if ws_sender.send(Message::Binary(data)).await.is_err() {
                break;
            }
        }
        let _ = ws_sender
            .send(Message::Text(
                serde_json::json!({"type": "exit", "code": 0}).to_string(),
            ))
            .await;
    });

    // Receive loop: WS → PTY stdin
    let master_for_resize = session.master.clone();
    let recv_handle = tokio::spawn(async move {
        while let Some(Ok(msg)) = ws_receiver.next().await {
            match msg {
                Message::Binary(data) => {
                    let mut w = writer.lock().await;
                    let _ = w.write_all(&data);
                    let _ = w.flush();
                }
                Message::Text(text) => {
                    if let Ok(ctrl) = serde_json::from_str::<serde_json::Value>(&text) {
                        if ctrl.get("type").and_then(|v| v.as_str()) == Some("resize") {
                            let cols =
                                ctrl.get("cols").and_then(|v| v.as_u64()).unwrap_or(80) as u16;
                            let rows =
                                ctrl.get("rows").and_then(|v| v.as_u64()).unwrap_or(24) as u16;
                            let m = master_for_resize.lock().await;
                            let _ = m.resize(portable_pty::PtySize {
                                rows,
                                cols,
                                pixel_width: 0,
                                pixel_height: 0,
                            });
                        }
                    }
                }
                Message::Close(_) => break,
                _ => {}
            }
        }
    });

    // Wait for either direction to finish
    tokio::select! {
        result = read_handle => {
            // Restore reader for potential reconnection
            if let Ok(reader) = result {
                let mut guard = reader_slot.lock().await;
                *guard = Some(reader);
            }
        }
        _ = recv_handle => {}
    }
    send_handle.abort();

    // Remove session if child exited
    manager.remove(&session_id_for_cleanup);
}
