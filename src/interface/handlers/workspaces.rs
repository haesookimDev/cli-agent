//! `/v1/workspaces/*` — workspace CRUD plus session-scoped file storage.
//! Files are uploaded as raw bytes via `?path=` query rather than multipart
//! to keep the v1 surface small; multipart can land alongside the notes
//! editor in Phase D.

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use serde::Deserialize;
use uuid::Uuid;

use crate::interface::api::{json_value, ApiState};
use crate::orchestrator::workspaces::CreateWorkspaceArgs;
use crate::types::{WorkspaceFileCreatedBy, WorkspaceKind};

#[derive(Debug, Deserialize)]
pub(crate) struct WorkspaceListQuery {
    pub kind: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct CreateWorkspaceBody {
    pub name: String,
    pub kind: String,
    #[serde(default)]
    pub root_path: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct UpdateWorkspaceBody {
    #[serde(default)]
    pub name: Option<String>,
    /// `description: null` clears the field; missing key leaves it alone.
    #[serde(default, deserialize_with = "deserialize_double_option")]
    pub description: Option<Option<String>>,
}

fn deserialize_double_option<'de, D>(
    deserializer: D,
) -> Result<Option<Option<String>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Some(Option::<String>::deserialize(deserializer)?))
}

#[derive(Debug, Deserialize)]
pub(crate) struct FileQuery {
    pub path: Option<String>,
    pub session_id: Option<String>,
    pub prefix: Option<String>,
}

fn parse_kind(raw: &str) -> Result<WorkspaceKind, (StatusCode, Json<serde_json::Value>)> {
    WorkspaceKind::parse(raw).ok_or_else(|| {
        (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "kind must be 'general' or 'team'"})),
        )
    })
}

pub(crate) async fn list_workspaces_handler(
    State(state): State<ApiState>,
    Query(query): Query<WorkspaceListQuery>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(err) = state.auth.verify_headers(&headers, &[]) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error": err.to_string()})),
        );
    }
    if let Some(ref k) = query.kind {
        if WorkspaceKind::parse(k).is_none() {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": "kind must be 'general' or 'team'"})),
            );
        }
    }
    match state
        .orchestrator
        .list_workspaces(query.kind.as_deref())
        .await
    {
        Ok(list) => (StatusCode::OK, Json(json_value(list))),
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": err.to_string()})),
        ),
    }
}

pub(crate) async fn create_workspace_handler(
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
    let req = match serde_json::from_slice::<CreateWorkspaceBody>(body.as_ref()) {
        Ok(v) => v,
        Err(err) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": err.to_string()})),
            );
        }
    };
    let kind = match parse_kind(&req.kind) {
        Ok(k) => k,
        Err(resp) => return resp,
    };
    match state
        .orchestrator
        .create_workspace(CreateWorkspaceArgs {
            name: req.name,
            kind,
            root_path: req.root_path,
            description: req.description,
        })
        .await
    {
        Ok(ws) => (StatusCode::CREATED, Json(json_value(ws))),
        Err(err) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": err.to_string()})),
        ),
    }
}

pub(crate) async fn get_workspace_handler(
    State(state): State<ApiState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(err) = state.auth.verify_headers(&headers, &[]) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error": err.to_string()})),
        );
    }
    match state.orchestrator.get_workspace(&id).await {
        Ok(Some(ws)) => (StatusCode::OK, Json(json_value(ws))),
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": "workspace not found"})),
        ),
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": err.to_string()})),
        ),
    }
}

pub(crate) async fn update_workspace_handler(
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
    let patch = match serde_json::from_slice::<UpdateWorkspaceBody>(body.as_ref()) {
        Ok(v) => v,
        Err(err) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": err.to_string()})),
            );
        }
    };
    let description_arg: Option<Option<&str>> = patch
        .description
        .as_ref()
        .map(|inner| inner.as_deref());
    if let Err(err) = state
        .orchestrator
        .update_workspace(&id, patch.name.as_deref(), description_arg)
        .await
    {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": err.to_string()})),
        );
    }
    match state.orchestrator.get_workspace(&id).await {
        Ok(Some(ws)) => (StatusCode::OK, Json(json_value(ws))),
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": "workspace not found"})),
        ),
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": err.to_string()})),
        ),
    }
}

pub(crate) async fn delete_workspace_handler(
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
    if let Err(err) = state.orchestrator.delete_workspace(&id).await {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": err.to_string()})),
        );
    }
    (StatusCode::OK, Json(serde_json::json!({"deleted": id})))
}

pub(crate) async fn list_workspace_sessions_handler(
    State(state): State<ApiState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(err) = state.auth.verify_headers(&headers, &[]) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error": err.to_string()})),
        );
    }
    match state.orchestrator.list_workspace_sessions(&id, 200).await {
        Ok(list) => (
            StatusCode::OK,
            Json(serde_json::json!({"session_ids": list})),
        ),
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": err.to_string()})),
        ),
    }
}

pub(crate) async fn list_workspace_files_handler(
    State(state): State<ApiState>,
    Path(id): Path<String>,
    Query(query): Query<FileQuery>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(err) = state.auth.verify_headers(&headers, &[]) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error": err.to_string()})),
        );
    }
    let session_id = match query.session_id.as_deref().map(Uuid::parse_str) {
        Some(Ok(u)) => Some(u),
        Some(Err(err)) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": err.to_string()})),
            );
        }
        None => None,
    };
    match state
        .orchestrator
        .list_workspace_files(&id, session_id, query.prefix.as_deref())
        .await
    {
        Ok(list) => (StatusCode::OK, Json(json_value(list))),
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": err.to_string()})),
        ),
    }
}

pub(crate) async fn upload_workspace_file_handler(
    State(state): State<ApiState>,
    Path(id): Path<String>,
    Query(query): Query<FileQuery>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    if let Err(err) = state.auth.verify_headers(&headers, body.as_ref()) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error": err.to_string()})),
        )
            .into_response();
    }
    let Some(path) = query.path.as_ref().filter(|p| !p.is_empty()) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "missing ?path"})),
        )
            .into_response();
    };
    let session_id = match query.session_id.as_deref().map(Uuid::parse_str) {
        Some(Ok(u)) => Some(u),
        Some(Err(err)) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": err.to_string()})),
            )
                .into_response();
        }
        None => None,
    };
    let mime = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    match state
        .orchestrator
        .upload_workspace_file(
            &id,
            path,
            session_id,
            body.to_vec(),
            mime,
            WorkspaceFileCreatedBy::User,
            None,
        )
        .await
    {
        Ok(file) => (StatusCode::CREATED, Json(json_value(file))).into_response(),
        Err(err) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": err.to_string()})),
        )
            .into_response(),
    }
}

pub(crate) async fn download_workspace_file_handler(
    State(state): State<ApiState>,
    Path(id): Path<String>,
    Query(query): Query<FileQuery>,
    headers: HeaderMap,
) -> axum::response::Response {
    if let Err(err) = state.auth.verify_headers(&headers, &[]) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error": err.to_string()})),
        )
            .into_response();
    }
    let Some(path) = query.path.as_ref().filter(|p| !p.is_empty()) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "missing ?path"})),
        )
            .into_response();
    };
    match state.orchestrator.read_workspace_file(&id, path).await {
        Ok(Some((meta, bytes))) => {
            let ct = meta.mime.unwrap_or_else(|| "application/octet-stream".to_string());
            let mut resp = (StatusCode::OK, bytes).into_response();
            if let Ok(value) = ct.parse() {
                resp.headers_mut().insert(header::CONTENT_TYPE, value);
            }
            resp
        }
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": "file not found"})),
        )
            .into_response(),
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": err.to_string()})),
        )
            .into_response(),
    }
}

pub(crate) async fn delete_workspace_file_handler(
    State(state): State<ApiState>,
    Path(id): Path<String>,
    Query(query): Query<FileQuery>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    if let Err(err) = state.auth.verify_headers(&headers, body.as_ref()) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error": err.to_string()})),
        );
    }
    let Some(path) = query.path.as_ref().filter(|p| !p.is_empty()) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "missing ?path"})),
        );
    };
    match state.orchestrator.delete_workspace_file(&id, path).await {
        Ok(true) => (StatusCode::OK, Json(serde_json::json!({"deleted": path}))),
        Ok(false) => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": "file not found"})),
        ),
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": err.to_string()})),
        ),
    }
}
