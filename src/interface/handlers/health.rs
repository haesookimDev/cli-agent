//! `/health` and `/v1/health` — liveness + dependency readiness probe.
//!
//! Returns 200 with a JSON snapshot when every checked dependency responds.
//! Returns 503 with the same shape when any dependency is degraded so load
//! balancers and orchestration platforms can pull the instance out of
//! rotation without crashing it.

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use chrono::Utc;

use crate::interface::api::ApiState;

const PKG_VERSION: &str = env!("CARGO_PKG_VERSION");

pub(crate) async fn health_handler(State(state): State<ApiState>) -> impl IntoResponse {
    let started = std::time::Instant::now();

    let store = state.orchestrator.memory().store();
    let db_ok = sqlx::query_scalar::<_, i64>("SELECT 1")
        .fetch_one(store.pool())
        .await
        .map(|v| v == 1)
        .unwrap_or(false);

    let mcp_servers = state.orchestrator.mcp_server_count();
    let active_runs = state.orchestrator.active_run_count();

    let healthy = db_ok;
    let status_code = if healthy {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };

    let body = serde_json::json!({
        "status": if healthy { "ok" } else { "degraded" },
        "version": PKG_VERSION,
        "checked_at": Utc::now().to_rfc3339(),
        "elapsed_ms": started.elapsed().as_millis(),
        "checks": {
            "database": if db_ok { "ok" } else { "down" },
        },
        "metrics": {
            "active_runs": active_runs,
            "mcp_servers": mcp_servers,
        }
    });

    (status_code, Json(body))
}
