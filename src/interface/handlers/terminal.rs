//! `/v1/terminal/*` endpoints — PTY session create/list/kill + WS I/O.

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use futures::{stream::StreamExt, SinkExt};
use serde::Deserialize;
use uuid::Uuid;

use crate::interface::api::{json_value, ApiState, WsAuthQuery};

#[derive(Debug, Deserialize)]
pub(crate) struct CreateTerminalRequest {
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default = "default_cols")]
    pub cols: u16,
    #[serde(default = "default_rows")]
    pub rows: u16,
    pub run_id: Option<Uuid>,
    pub session_id: Option<Uuid>,
}

fn default_cols() -> u16 {
    80
}
fn default_rows() -> u16 {
    24
}

pub(crate) async fn create_terminal_handler(
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

pub(crate) async fn list_terminals_handler(
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

pub(crate) async fn kill_terminal_handler(
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

pub(crate) async fn terminal_ws_handler(
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
