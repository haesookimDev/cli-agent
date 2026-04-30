//! `/v1/workspaces/:wid/meetings` and `/v1/meetings/:mid/*` — async
//! collaboration thread between the user and one or more personas. v1
//! supports CRUD plus a `messages` append. Auto-rounds (where each listed
//! persona drafts a reply) are deferred.

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use chrono::Utc;
use serde::Deserialize;
use uuid::Uuid;

use crate::interface::api::{json_value, ApiState};
use crate::types::{Meeting, MeetingSpeakerKind, MeetingStatus, RunActionType};

#[derive(Debug, Deserialize)]
pub(crate) struct CreateMeetingBody {
    pub topic: String,
    #[serde(default)]
    pub participants: Vec<String>,
    #[serde(default)]
    pub session_id: Option<Uuid>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct PostMessageBody {
    pub content: String,
    #[serde(default)]
    pub speaker_kind: Option<String>,
    #[serde(default)]
    pub speaker_name: Option<String>,
}

pub(crate) async fn create_meeting_handler(
    State(state): State<ApiState>,
    Path(workspace_id): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    if let Err(err) = state.auth.verify_headers(&headers, body.as_ref()) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error": err.to_string()})),
        );
    }
    let req = match serde_json::from_slice::<CreateMeetingBody>(body.as_ref()) {
        Ok(v) => v,
        Err(err) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": err.to_string()})),
            );
        }
    };
    if req.topic.trim().is_empty() {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(serde_json::json!({"error": "topic must not be empty"})),
        );
    }
    let meeting = Meeting {
        id: Uuid::new_v4().to_string(),
        workspace_id: workspace_id.clone(),
        session_id: req.session_id,
        topic: req.topic.trim().to_string(),
        participants: req.participants,
        status: MeetingStatus::Open,
        created_by: "user".to_string(),
        created_at: Utc::now(),
        closed_at: None,
    };
    if let Err(err) = state
        .orchestrator
        .memory()
        .store()
        .insert_meeting(&meeting)
        .await
    {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": err.to_string()})),
        );
    }
    if let Some(session_id) = req.session_id {
        // Surface the meeting on the run trace (when a session is bound)
        // so the UI can stitch threads to runs without a join query.
        state
            .orchestrator
            .record_action_event(
                Uuid::nil(),
                session_id,
                RunActionType::MeetingStarted,
                Some("user"),
                Some(meeting.id.as_str()),
                None,
                serde_json::json!({
                    "meeting_id": meeting.id,
                    "topic": meeting.topic,
                    "participants": meeting.participants,
                }),
            )
            .await;
    }
    (StatusCode::CREATED, Json(json_value(meeting)))
}

pub(crate) async fn list_workspace_meetings_handler(
    State(state): State<ApiState>,
    Path(workspace_id): Path<String>,
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
        .memory()
        .store()
        .list_workspace_meetings(&workspace_id, 100)
        .await
    {
        Ok(list) => (StatusCode::OK, Json(json_value(list))),
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": err.to_string()})),
        ),
    }
}

pub(crate) async fn get_meeting_handler(
    State(state): State<ApiState>,
    Path(meeting_id): Path<String>,
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
        .memory()
        .store()
        .get_meeting(&meeting_id)
        .await
    {
        Ok(Some(m)) => {
            let messages = state
                .orchestrator
                .memory()
                .store()
                .list_meeting_messages(&meeting_id, 500)
                .await
                .unwrap_or_default();
            (
                StatusCode::OK,
                Json(serde_json::json!({"meeting": m, "messages": messages})),
            )
        }
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": "meeting not found"})),
        ),
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": err.to_string()})),
        ),
    }
}

pub(crate) async fn post_meeting_message_handler(
    State(state): State<ApiState>,
    Path(meeting_id): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    if let Err(err) = state.auth.verify_headers(&headers, body.as_ref()) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error": err.to_string()})),
        );
    }
    let req = match serde_json::from_slice::<PostMessageBody>(body.as_ref()) {
        Ok(v) => v,
        Err(err) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": err.to_string()})),
            );
        }
    };
    if req.content.trim().is_empty() {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(serde_json::json!({"error": "content must not be empty"})),
        );
    }
    let speaker_kind = match req.speaker_kind.as_deref().unwrap_or("user") {
        "user" => MeetingSpeakerKind::User,
        "persona" => MeetingSpeakerKind::Persona,
        "system" => MeetingSpeakerKind::System,
        other => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "error": format!("invalid speaker_kind: {other}")
                })),
            );
        }
    };
    let speaker_name = req
        .speaker_name
        .clone()
        .unwrap_or_else(|| match speaker_kind {
            MeetingSpeakerKind::User => "user".to_string(),
            MeetingSpeakerKind::System => "system".to_string(),
            MeetingSpeakerKind::Persona => "persona".to_string(),
        });
    match state
        .orchestrator
        .memory()
        .store()
        .append_meeting_message(&meeting_id, speaker_kind, &speaker_name, &req.content)
        .await
    {
        Ok(id) => (
            StatusCode::CREATED,
            Json(serde_json::json!({"id": id})),
        ),
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": err.to_string()})),
        ),
    }
}

pub(crate) async fn close_meeting_handler(
    State(state): State<ApiState>,
    Path(meeting_id): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    if let Err(err) = state.auth.verify_headers(&headers, body.as_ref()) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error": err.to_string()})),
        );
    }
    if let Err(err) = state
        .orchestrator
        .memory()
        .store()
        .close_meeting(&meeting_id)
        .await
    {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": err.to_string()})),
        );
    }
    (
        StatusCode::OK,
        Json(serde_json::json!({"closed": meeting_id})),
    )
}
