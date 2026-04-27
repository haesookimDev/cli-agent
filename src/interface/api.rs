use std::net::SocketAddr;

use axum::extract::DefaultBodyLimit;
use axum::http::{HeaderValue, Method};
use axum::response::Html;
use axum::routing::{delete, get, patch, post};
use axum::Router;
use serde::Deserialize;
use tower_http::cors::{Any, AllowOrigin, CorsLayer};
use uuid::Uuid;

use crate::interface::handlers;
use crate::interface::rate_limit::{self, RateLimitConfig, RateLimiter};
use crate::orchestrator::Orchestrator;
use crate::orchestrator::cluster::OrchestratorCluster;
use crate::terminal::TerminalManager;
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
pub(crate) struct ExecuteWorkflowRequest {
    pub parameters: Option<serde_json::Value>,
    pub session_id: Option<Uuid>,
}

pub fn router(state: ApiState) -> Router {
    Router::new()
        .route("/dashboard", get(dashboard_handler))
        .route("/web-client", get(web_client_handler))
        .route("/health", get(handlers::health::health_handler))
        .route("/v1/health", get(handlers::health::health_handler))
        .route(
            "/v1/sessions",
            post(handlers::sessions::create_session_handler)
                .get(handlers::sessions::list_sessions_handler),
        )
        .route(
            "/v1/sessions/:session_id",
            get(handlers::sessions::get_session_handler)
                .delete(handlers::sessions::delete_session_handler),
        )
        .route(
            "/v1/sessions/:session_id/runs",
            get(handlers::sessions::list_session_runs_handler),
        )
        .route(
            "/v1/sessions/:session_id/messages",
            get(handlers::sessions::list_session_messages_handler),
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
        .route("/v1/runs/active", get(handlers::runs::list_active_runs_handler))
        .route(
            "/v1/runs",
            post(handlers::runs::create_run_handler).get(handlers::runs::list_runs_handler),
        )
        .route("/v1/runs/:run_id", get(handlers::runs::get_run_handler))
        .route("/v1/runs/:run_id/cancel", post(handlers::runs::cancel_run_handler))
        .route("/v1/runs/:run_id/pause", post(handlers::runs::pause_run_handler))
        .route("/v1/runs/:run_id/resume", post(handlers::runs::resume_run_handler))
        .route("/v1/runs/:run_id/retry", post(handlers::runs::retry_run_handler))
        .route("/v1/runs/:run_id/clone", post(handlers::runs::clone_run_handler))
        .route(
            "/v1/runs/:run_id/behavior",
            get(handlers::runs::get_run_behavior_handler),
        )
        .route("/v1/runs/:run_id/trace", get(handlers::runs::get_run_trace_handler))
        .route("/v1/runs/:run_id/stream", get(handlers::runs::stream_run_handler))
        .route("/v1/runs/:run_id/ws", get(handlers::runs::ws_stream_run_handler))
        .route(
            "/v1/runs/:run_id/coder-sessions",
            get(handlers::runs::list_coder_sessions_handler),
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
    let cors = build_cors_layer();

    let limiter = RateLimiter::new(RateLimitConfig::from_env());
    let body_limit_bytes = std::env::var("CLI_AGENT_MAX_BODY_BYTES")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(2 * 1024 * 1024);
    let mut app = router(state)
        .layer(DefaultBodyLimit::max(body_limit_bytes))
        .layer(axum::middleware::from_fn_with_state(
            limiter,
            rate_limit::middleware,
        ))
        .layer(cors);
    if let Some(gw) = gateway_router {
        app = app.merge(gw);
    }
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

/// Build the CORS layer from `CLI_AGENT_ALLOWED_ORIGINS` (comma-separated
/// origin URLs). Setting the variable to `*` keeps the wildcard for local
/// development. When unset the default also opens up to any origin so first
/// boot still works, but production deploys are expected to set a strict
/// allowlist (e.g. `https://app.example.com`).
fn build_cors_layer() -> CorsLayer {
    let methods = [
        Method::GET,
        Method::POST,
        Method::PATCH,
        Method::DELETE,
        Method::OPTIONS,
    ];
    let raw = std::env::var("CLI_AGENT_ALLOWED_ORIGINS").unwrap_or_default();
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed == "*" {
        return CorsLayer::new()
            .allow_origin(Any)
            .allow_methods(methods)
            .allow_headers(Any);
    }

    let parsed: Vec<HeaderValue> = trimmed
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .filter_map(|s| match HeaderValue::from_str(s) {
            Ok(v) => Some(v),
            Err(e) => {
                tracing::warn!("ignoring invalid CORS origin '{s}': {e}");
                None
            }
        })
        .collect();

    if parsed.is_empty() {
        tracing::warn!(
            "CLI_AGENT_ALLOWED_ORIGINS set but no valid origins parsed — falling back to wildcard"
        );
        return CorsLayer::new()
            .allow_origin(Any)
            .allow_methods(methods)
            .allow_headers(Any);
    }

    tracing::info!(
        "CORS allowlist active with {count} origin(s)",
        count = parsed.len()
    );
    CorsLayer::new()
        .allow_origin(AllowOrigin::list(parsed))
        .allow_methods(methods)
        .allow_headers(Any)
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


async fn dashboard_handler() -> Html<&'static str> {
    Html(DASHBOARD_HTML)
}

async fn web_client_handler() -> Html<&'static str> {
    Html(WEB_CLIENT_HTML)
}




