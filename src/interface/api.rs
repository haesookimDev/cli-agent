use std::net::SocketAddr;
use std::time::Duration;

use async_stream::stream;
use axum::body::Bytes;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, Method, StatusCode};
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use axum::response::{Html, IntoResponse};
use axum::routing::{delete, get, patch, post};
use axum::{Json, Router};
use futures::{SinkExt, stream::StreamExt};
use serde::{Deserialize, Serialize};
use tower_http::cors::{Any, CorsLayer};
use uuid::Uuid;

use crate::interface::handlers;
use crate::orchestrator::Orchestrator;
use crate::orchestrator::cluster::OrchestratorCluster;
use crate::terminal::TerminalManager;
use crate::types::{ClusterRunRequest, RunRequest, TaskProfile};
use crate::webhook::AuthManager;

const DASHBOARD_HTML: &str = include_str!("dashboard.html");
const WEB_CLIENT_HTML: &str = include_str!("web_client.html");

#[derive(Clone)]
pub struct ApiState {
    pub orchestrator: Orchestrator,
    pub auth: std::sync::Arc<AuthManager>,
    pub terminal: TerminalManager,
    pub cluster: OrchestratorCluster,
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
    repo_url: Option<String>,
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
pub(crate) struct ListQuery {
    pub limit: Option<usize>,
}

/// Authentication query params for WebSocket upgrade endpoints (headers are
/// not available on the initial upgrade request, so callers pass the same
/// values that would normally live in HMAC headers via the query string).
#[derive(Debug, Deserialize)]
pub(crate) struct WsAuthQuery {
    pub api_key: String,
    pub signature: String,
    pub timestamp: String,
    pub nonce: String,
}

#[derive(Debug, Deserialize)]
struct CloneRunRequest {
    session_id: Option<Uuid>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ExecuteWorkflowRequest {
    pub parameters: Option<serde_json::Value>,
    pub session_id: Option<Uuid>,
}

pub fn router(state: ApiState) -> Router {
    Router::new()
        .route("/dashboard", get(dashboard_handler))
        .route("/web-client", get(web_client_handler))
        .route(
            "/v1/sessions",
            post(create_session_handler).get(list_sessions_handler),
        )
        .route(
            "/v1/sessions/:session_id",
            get(get_session_handler).delete(delete_session_handler),
        )
        .route(
            "/v1/sessions/:session_id/runs",
            get(list_session_runs_handler),
        )
        .route(
            "/v1/sessions/:session_id/messages",
            get(list_session_messages_handler),
        )
        .route(
            "/v1/memory/sessions/:session_id/items",
            get(handlers::memory::list_session_memory_items_handler)
                .post(handlers::memory::create_session_memory_item_handler),
        )
        .route(
            "/v1/memory/items/:memory_id",
            patch(handlers::memory::update_session_memory_item_handler)
                .delete(handlers::memory::delete_session_memory_item_handler),
        )
        .route(
            "/v1/memory/global/items",
            get(handlers::memory::list_global_memory_items_handler)
                .post(handlers::memory::create_global_memory_item_handler),
        )
        .route(
            "/v1/memory/global/items/:knowledge_id",
            patch(handlers::memory::update_global_memory_item_handler),
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
        .route("/v1/runs/:run_id/ws", get(ws_stream_run_handler))
        .route(
            "/v1/runs/:run_id/coder-sessions",
            get(list_coder_sessions_handler),
        )
        .route(
            "/v1/webhooks/endpoints",
            post(handlers::webhooks::register_webhook_handler)
                .get(handlers::webhooks::list_webhooks_handler),
        )
        .route(
            "/v1/webhooks/deliveries",
            get(handlers::webhooks::list_webhook_deliveries_handler),
        )
        .route(
            "/v1/webhooks/deliveries/:delivery_id/retry",
            post(handlers::webhooks::retry_webhook_delivery_handler),
        )
        .route("/v1/webhooks/test", post(handlers::webhooks::test_webhook_handler))
        .route("/v1/mcp/tools", get(handlers::mcp::list_mcp_tools_handler))
        .route("/v1/mcp/tools/call", post(handlers::mcp::call_mcp_tool_handler))
        .route("/v1/mcp/servers", get(handlers::mcp::list_mcp_servers_handler))
        .route(
            "/v1/settings",
            get(handlers::settings::get_settings_handler)
                .patch(handlers::settings::update_settings_handler),
        )
        .route("/v1/models", get(handlers::settings::list_models_handler))
        .route("/v1/local-models", get(handlers::settings::list_vllm_models_handler))
        .route(
            "/v1/models/:model_id/toggle",
            post(handlers::settings::toggle_model_handler),
        )
        .route(
            "/v1/providers/:provider_name/toggle",
            post(handlers::settings::toggle_provider_handler),
        )
        .route(
            "/v1/schedules",
            post(handlers::schedules::create_schedule_handler)
                .get(handlers::schedules::list_schedules_handler),
        )
        .route(
            "/v1/schedules/:schedule_id",
            get(handlers::schedules::get_schedule_handler)
                .patch(handlers::schedules::update_schedule_handler)
                .delete(handlers::schedules::delete_schedule_handler),
        )
        .route(
            "/v1/workflows",
            post(handlers::workflows::create_workflow_handler)
                .get(handlers::workflows::list_workflows_handler),
        )
        .route(
            "/v1/workflows/:workflow_id",
            get(handlers::workflows::get_workflow_handler)
                .delete(handlers::workflows::delete_workflow_handler),
        )
        .route(
            "/v1/workflows/:workflow_id/execute",
            post(handlers::workflows::execute_workflow_handler),
        )
        .route(
            "/v1/runs/:run_id/save-workflow",
            post(handlers::workflows::save_workflow_from_run_handler),
        )
        .route("/v1/skills", get(handlers::skills::list_skills_handler))
        .route("/v1/skills/reload", post(handlers::skills::reload_skills_handler))
        .route("/v1/skills/:skill_id", get(handlers::skills::get_skill_handler))
        .route("/v1/skills/:skill_id/execute", post(handlers::skills::execute_skill_handler))
        .route(
            "/v1/terminal/sessions",
            post(handlers::terminal::create_terminal_handler)
                .get(handlers::terminal::list_terminals_handler),
        )
        .route(
            "/v1/terminal/sessions/:id",
            delete(handlers::terminal::kill_terminal_handler),
        )
        .route(
            "/v1/terminal/sessions/:id/ws",
            get(handlers::terminal::terminal_ws_handler),
        )
        .route(
            "/v1/cluster/members",
            get(handlers::cluster::list_cluster_members_handler),
        )
        .route(
            "/v1/cluster/runs",
            post(handlers::cluster::submit_cluster_run_handler)
                .get(handlers::cluster::list_cluster_runs_handler),
        )
        .route(
            "/v1/cluster/runs/:cluster_run_id",
            get(handlers::cluster::get_cluster_run_handler),
        )
        // Team & GitHub activity endpoints
        .route(
            "/v1/team/members",
            get(handlers::team::list_team_members_handler),
        )
        .route(
            "/v1/github/activities",
            get(handlers::team::list_github_activities_handler),
        )
        .route(
            "/v1/github/activities/stats",
            get(handlers::team::github_activity_stats_handler),
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

/// Serialize a response payload to `serde_json::Value`, logging and returning
/// an error object instead of panicking if serialization fails. Call sites
/// previously used `serde_json::to_value(x).unwrap()`, which could bring down
/// the server on a serialization error.
pub(crate) fn json_value<T: serde::Serialize>(value: T) -> serde_json::Value {
    serde_json::to_value(value).unwrap_or_else(|e| {
        tracing::error!("api response serialization failed: {e}");
        serde_json::json!({"error": format!("serialization failed: {e}")})
    })
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
        Json(json_value(CreateSessionResponse { session_id })),
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
            Json(json_value(sessions)),
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
        Ok(runs) => (StatusCode::OK, Json(json_value(runs))),
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
            Json(json_value(messages)),
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
    (StatusCode::OK, Json(json_value(active)))
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
        Ok(runs) => (StatusCode::OK, Json(json_value(runs))),
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
            Json(json_value(sub)),
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
            Json(json_value(sub)),
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

async fn list_coder_sessions_handler(
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

/// WebSocket equivalent of `stream_run_handler`.
/// Clients connect to `/v1/runs/:run_id/ws?api_key=...` and receive the same
/// `RunActionEvent` JSON frames that the SSE endpoint emits, but over a persistent
/// WebSocket connection — no polling delay, no SSE keep-alive overhead.
///
/// Message format (server → client): text frames, each containing a JSON object:
///   `{"type": "action_event", "data": <RunActionEvent>}`
///   `{"type": "run_terminal", "data": {"run_id": "...", "status": "..."}}`
///   `{"type": "error", "data": {"error": "..."}}`
async fn ws_stream_run_handler(
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

