//! `/v1/mcp/*` endpoints — list MCP tools/servers and invoke tools.

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use serde::Deserialize;

use crate::interface::api::ApiState;

#[derive(Debug, Deserialize)]
pub(crate) struct CallToolRequest {
    pub tool_name: String,
    #[serde(default = "default_empty_object")]
    pub arguments: serde_json::Value,
}

fn default_empty_object() -> serde_json::Value {
    serde_json::json!({})
}

pub(crate) async fn list_mcp_tools_handler(State(state): State<ApiState>) -> impl IntoResponse {
    let tools = state.orchestrator.list_mcp_tools().await;
    (StatusCode::OK, Json(serde_json::json!(tools)))
}

pub(crate) async fn list_mcp_servers_handler(State(state): State<ApiState>) -> impl IntoResponse {
    let servers = state.orchestrator.list_mcp_servers();
    (StatusCode::OK, Json(serde_json::json!(servers)))
}

pub(crate) async fn call_mcp_tool_handler(
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
