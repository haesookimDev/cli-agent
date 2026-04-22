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

#[derive(Debug, Deserialize)]
struct MemoryListQuery {
    limit: Option<usize>,
    query: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CreateSessionMemoryRequest {
    content: String,
    scope: Option<String>,
    importance: Option<f64>,
    source_ref: Option<String>,
}

#[derive(Debug, Deserialize)]
struct UpdateSessionMemoryRequest {
    content: Option<String>,
    scope: Option<String>,
    importance: Option<f64>,
}

#[derive(Debug, Deserialize)]
struct CreateGlobalMemoryRequest {
    topic: String,
    content: String,
    importance: Option<f64>,
}

#[derive(Debug, Deserialize)]
struct UpdateGlobalMemoryRequest {
    topic: Option<String>,
    content: Option<String>,
    importance: Option<f64>,
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
            get(list_session_memory_items_handler).post(create_session_memory_item_handler),
        )
        .route(
            "/v1/memory/items/:memory_id",
            patch(update_session_memory_item_handler).delete(delete_session_memory_item_handler),
        )
        .route(
            "/v1/memory/global/items",
            get(list_global_memory_items_handler).post(create_global_memory_item_handler),
        )
        .route(
            "/v1/memory/global/items/:knowledge_id",
            patch(update_global_memory_item_handler),
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
            post(create_terminal_handler).get(list_terminals_handler),
        )
        .route("/v1/terminal/sessions/:id", delete(kill_terminal_handler))
        .route("/v1/terminal/sessions/:id/ws", get(terminal_ws_handler))
        .route("/v1/cluster/members", get(list_cluster_members_handler))
        .route(
            "/v1/cluster/runs",
            post(submit_cluster_run_handler).get(list_cluster_runs_handler),
        )
        .route("/v1/cluster/runs/:cluster_run_id", get(get_cluster_run_handler))
        // Team & GitHub activity endpoints
        .route("/v1/team/members", get(list_team_members_handler))
        .route("/v1/github/activities", get(list_github_activities_handler))
        .route("/v1/github/activities/stats", get(github_activity_stats_handler))
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

async fn list_session_memory_items_handler(
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

async fn create_session_memory_item_handler(
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

async fn update_session_memory_item_handler(
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

async fn delete_session_memory_item_handler(
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

async fn list_global_memory_items_handler(
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

async fn create_global_memory_item_handler(
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

async fn update_global_memory_item_handler(
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

    let effective_session_id = if let Some(session_id) = req.session_id {
        Some(session_id)
    } else if let Some(run_id) = req.run_id {
        match state.orchestrator.get_run(run_id).await {
            Ok(Some(run)) => Some(run.session_id),
            _ => None,
        }
    } else {
        None
    };

    match state.terminal.spawn(
        &req.command,
        &req.args,
        req.cols,
        req.rows,
        req.run_id,
        effective_session_id,
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
    (StatusCode::OK, Json(json_value(list)))
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

    match state.terminal.kill(&id).await {
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

    ws.on_upgrade(move |socket| handle_terminal_ws(socket, session))
        .into_response()
}

async fn handle_terminal_ws(socket: WebSocket, session: Arc<crate::terminal::TerminalSession>) {
    use std::io::Write;

    let (mut ws_sender, mut ws_receiver) = socket.split();

    let writer = session.writer.clone();

    // Atomically subscribe AND snapshot scrollback under the same lock so
    // that no output is duplicated or lost between the snapshot and the live
    // event stream.
    let (scrollback_snapshot, mut events) = {
        let sb = session.scrollback.lock().unwrap_or_else(|e| e.into_inner());
        let rx = session.events.subscribe();
        (sb.clone(), rx)
    };

    // Replay buffered output so the client sees the prompt / prior output.
    if !scrollback_snapshot.is_empty() {
        if ws_sender
            .send(Message::Binary(scrollback_snapshot))
            .await
            .is_err()
        {
            return;
        }
    }

    // Session events → WS frames (with periodic pings for liveness).
    let mut send_handle = tokio::spawn(async move {
        let mut ping_interval = tokio::time::interval(std::time::Duration::from_secs(30));
        ping_interval.tick().await; // first tick is immediate, skip it
        loop {
            tokio::select! {
                event = events.recv() => {
                    match event {
                        Ok(crate::terminal::TerminalEvent::Output(data)) => {
                            if ws_sender.send(Message::Binary(data)).await.is_err() {
                                break;
                            }
                        }
                        Ok(crate::terminal::TerminalEvent::Exit(code)) => {
                            let _ = ws_sender
                                .send(Message::Text(
                                    serde_json::json!({"type": "exit", "code": code}).to_string(),
                                ))
                                .await;
                            break;
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    }
                }
                _ = ping_interval.tick() => {
                    if ws_sender.send(Message::Ping(vec![])).await.is_err() {
                        break;
                    }
                }
            }
        }
    });

    // Receive loop: WS → PTY stdin (with 90s timeout to detect dead connections).
    let master_for_resize = session.master.clone();
    let recv_timeout = std::time::Duration::from_secs(90);
    let mut recv_handle = tokio::spawn(async move {
        loop {
            let msg = match tokio::time::timeout(recv_timeout, ws_receiver.next()).await {
                Ok(Some(Ok(msg))) => msg,
                _ => break, // WS error, stream ended, or timeout
            };
            match msg {
                Message::Binary(data) => {
                    let writer = writer.clone();
                    let _ = tokio::task::spawn_blocking(move || {
                        let mut w = writer.blocking_lock();
                        let _ = w.write_all(&data);
                        let _ = w.flush();
                    })
                    .await;
                }
                Message::Text(text) => {
                    if let Ok(ctrl) = serde_json::from_str::<serde_json::Value>(&text) {
                        if ctrl.get("type").and_then(|v| v.as_str()) == Some("resize") {
                            let cols =
                                ctrl.get("cols").and_then(|v| v.as_u64()).unwrap_or(80) as u16;
                            let rows =
                                ctrl.get("rows").and_then(|v| v.as_u64()).unwrap_or(24) as u16;
                            let master = master_for_resize.clone();
                            let _ = tokio::task::spawn_blocking(move || {
                                let m = master.blocking_lock();
                                let _ = m.resize(portable_pty::PtySize {
                                    rows,
                                    cols,
                                    pixel_width: 0,
                                    pixel_height: 0,
                                });
                            })
                            .await;
                        }
                    }
                }
                Message::Close(_) => break,
                _ => {} // Pong and other frames
            }
        }
    });

    // Keep the WS alive until either side ends.
    tokio::select! {
        _ = &mut send_handle => {
            recv_handle.abort();
        }
        _ = &mut recv_handle => {
            send_handle.abort();
        }
    }
}

// ── Cluster handlers ──────────────────────────────────────────────────────────

async fn list_cluster_members_handler(
    State(state): State<ApiState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(e) = state.auth.verify_headers(&headers, &[]) {
        return (StatusCode::UNAUTHORIZED, Json(serde_json::json!({"error": e.to_string()}))).into_response();
    }
    let names = state.cluster.member_names();
    Json(serde_json::json!({ "members": names })).into_response()
}

async fn submit_cluster_run_handler(
    State(state): State<ApiState>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    if let Err(e) = state.auth.verify_headers(&headers, body.as_ref()) {
        return (StatusCode::UNAUTHORIZED, Json(serde_json::json!({"error": e.to_string()}))).into_response();
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

async fn list_cluster_runs_handler(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Query(q): Query<ListQuery>,
) -> impl IntoResponse {
    if let Err(e) = state.auth.verify_headers(&headers, &[]) {
        return (StatusCode::UNAUTHORIZED, Json(serde_json::json!({"error": e.to_string()}))).into_response();
    }
    let runs = state.cluster.list(q.limit.unwrap_or(20));
    Json(serde_json::json!({ "cluster_runs": runs })).into_response()
}

async fn get_cluster_run_handler(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Path(cluster_run_id): Path<Uuid>,
) -> impl IntoResponse {
    if let Err(e) = state.auth.verify_headers(&headers, &[]) {
        return (StatusCode::UNAUTHORIZED, Json(serde_json::json!({"error": e.to_string()}))).into_response();
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

// --- Team & GitHub Activity handlers ---

async fn list_team_members_handler(
    State(state): State<ApiState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(e) = state.auth.verify_headers(&headers, &[]) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error": e.to_string()})),
        );
    }

    // Load team agent definitions from agents/team/ directory.
    let team_dir = std::path::Path::new("agents/team");
    let members = crate::agents::agent_loader::load_agents_from_dir(team_dir).await;

    let result: Vec<serde_json::Value> = members
        .into_iter()
        .map(|def| {
            let mut val = serde_json::json!({
                "name": def.name,
                "description": def.description,
                "role": def.role.to_string(),
                "task_profile": def.task_profile.to_string(),
                "capabilities": def.capabilities,
            });
            if let Some(persona) = &def.persona {
                val["persona"] = serde_json::to_value(persona).unwrap_or_default();
            }
            val
        })
        .collect();

    (StatusCode::OK, Json(serde_json::json!(result)))
}

#[derive(Debug, Deserialize)]
struct GitHubActivityQuery {
    persona: Option<String>,
    run_id: Option<String>,
    limit: Option<i64>,
}

async fn list_github_activities_handler(
    State(state): State<ApiState>,
    Query(query): Query<GitHubActivityQuery>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(e) = state.auth.verify_headers(&headers, &[]) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error": e.to_string()})),
        );
    }

    let limit = query.limit.unwrap_or(100).clamp(1, 500);
    match state
        .orchestrator
        .memory()
        .store()
        .list_github_activities(query.persona.as_deref(), query.run_id.as_deref(), limit)
        .await
    {
        Ok(activities) => (StatusCode::OK, Json(serde_json::json!(activities))),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        ),
    }
}

async fn github_activity_stats_handler(
    State(state): State<ApiState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(e) = state.auth.verify_headers(&headers, &[]) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error": e.to_string()})),
        );
    }

    match state
        .orchestrator
        .memory()
        .store()
        .github_activity_stats()
        .await
    {
        Ok(stats) => (StatusCode::OK, Json(serde_json::json!(stats))),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        ),
    }
}
