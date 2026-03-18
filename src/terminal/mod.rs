use std::io::{Read, Write};
use std::sync::Arc;

use chrono::{DateTime, Utc};
use dashmap::DashMap;
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, broadcast};
use uuid::Uuid;

#[derive(Debug, Clone)]
pub enum TerminalEvent {
    Output(Vec<u8>),
    Exit(i32),
}

/// Max scrollback buffer size (64 KB). Older output is trimmed from the front.
const MAX_SCROLLBACK: usize = 64 * 1024;

/// One live terminal session backed by a PTY.
pub struct TerminalSession {
    pub id: String,
    pub command: String,
    pub args: Vec<String>,
    pub created_at: DateTime<Utc>,
    pub cols: u16,
    pub rows: u16,
    /// Write end: send bytes into the PTY (keyboard input from browser).
    pub writer: Arc<Mutex<Box<dyn Write + Send>>>,
    /// Broadcasts PTY output and lifecycle events to any attached web terminals.
    pub events: broadcast::Sender<TerminalEvent>,
    /// PTY master handle for resize operations.
    pub master: Arc<Mutex<Box<dyn MasterPty + Send>>>,
    /// Child process handle.
    pub child: Arc<Mutex<Box<dyn Child + Send + Sync>>>,
    /// Optional link to an orchestrator run.
    pub run_id: Option<Uuid>,
    pub session_id: Option<Uuid>,
    /// Scrollback buffer so new WS connections can replay missed output.
    /// Uses std::sync::Mutex because the reader thread is a plain OS thread.
    pub scrollback: Arc<std::sync::Mutex<Vec<u8>>>,
}

/// Lightweight serializable info for REST list endpoint.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TerminalSessionInfo {
    pub id: String,
    pub command: String,
    pub args: Vec<String>,
    pub created_at: DateTime<Utc>,
    pub cols: u16,
    pub rows: u16,
    pub run_id: Option<Uuid>,
    pub session_id: Option<Uuid>,
}

/// Manages all active terminal sessions. In-memory only.
#[derive(Clone)]
pub struct TerminalManager {
    sessions: Arc<DashMap<String, Arc<TerminalSession>>>,
}

impl TerminalManager {
    pub fn new() -> Self {
        Self {
            sessions: Arc::new(DashMap::new()),
        }
    }

    /// Spawn a new PTY session. Returns the session ID.
    pub fn spawn(
        &self,
        command: &str,
        args: &[String],
        cols: u16,
        rows: u16,
        run_id: Option<Uuid>,
        session_id: Option<Uuid>,
    ) -> anyhow::Result<String> {
        let pty_system = native_pty_system();
        let pair = pty_system.openpty(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })?;

        let resolved_command = crate::command_resolver::resolve_command_path(command);
        let resolved_command_str = resolved_command.to_string_lossy().into_owned();
        let mut cmd = CommandBuilder::new(&resolved_command_str);
        cmd.args(args);

        let child = pair.slave.spawn_command(cmd)?;
        let writer = pair.master.take_writer()?;
        let reader = pair.master.try_clone_reader()?;
        let (events, _) = broadcast::channel(256);
        let scrollback: Arc<std::sync::Mutex<Vec<u8>>> =
            Arc::new(std::sync::Mutex::new(Vec::new()));

        let id = Uuid::new_v4().to_string();
        let session = Arc::new(TerminalSession {
            id: id.clone(),
            command: resolved_command_str,
            args: args.to_vec(),
            created_at: Utc::now(),
            cols,
            rows,
            writer: Arc::new(Mutex::new(writer)),
            events: events.clone(),
            master: Arc::new(Mutex::new(pair.master)),
            child: Arc::new(Mutex::new(child)),
            run_id,
            session_id,
            scrollback: scrollback.clone(),
        });

        let child_for_reader = session.child.clone();
        let sessions_for_cleanup = self.sessions.clone();
        let id_for_cleanup = id.clone();

        let thread_name = format!("terminal-reader-{}", &id[..8]);
        std::thread::Builder::new()
            .name(thread_name)
            .spawn(move || {
                let mut reader = reader;
                let mut buf = [0u8; 4096];
                loop {
                    match reader.read(&mut buf) {
                        Ok(0) => break,
                        Ok(n) => {
                            let data = buf[..n].to_vec();
                            // Hold scrollback lock while appending AND broadcasting
                            // so that a new WS subscriber cannot slip in between.
                            let mut sb = scrollback.lock().unwrap();
                            sb.extend_from_slice(&data);
                            if sb.len() > MAX_SCROLLBACK {
                                let excess = sb.len() - MAX_SCROLLBACK;
                                sb.drain(..excess);
                            }
                            let _ = events.send(TerminalEvent::Output(data));
                            drop(sb);
                        }
                        Err(_) => break,
                    }
                }
                // Collect real exit code from the child process.
                let code = match child_for_reader.blocking_lock().wait() {
                    Ok(status) => if status.success() { 0 } else { 1 },
                    Err(_) => -1,
                };
                let _ = events.send(TerminalEvent::Exit(code));
                // Auto-remove dead session from the manager.
                sessions_for_cleanup.remove(&id_for_cleanup);
            })
            .map_err(|err| anyhow::anyhow!("failed to spawn terminal reader thread: {err}"))?;

        self.sessions.insert(id.clone(), session);
        Ok(id)
    }

    /// Get a session by ID.
    pub fn get(&self, id: &str) -> Option<Arc<TerminalSession>> {
        self.sessions.get(id).map(|v| v.value().clone())
    }

    /// List all active sessions (metadata only).
    pub fn list(&self) -> Vec<TerminalSessionInfo> {
        self.sessions
            .iter()
            .map(|entry| {
                let s = entry.value();
                TerminalSessionInfo {
                    id: s.id.clone(),
                    command: s.command.clone(),
                    args: s.args.clone(),
                    created_at: s.created_at,
                    cols: s.cols,
                    rows: s.rows,
                    run_id: s.run_id,
                    session_id: s.session_id,
                }
            })
            .collect()
    }

    /// Resize a session's PTY.
    pub async fn resize(&self, id: &str, cols: u16, rows: u16) -> anyhow::Result<()> {
        let session = self
            .sessions
            .get(id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| anyhow::anyhow!("terminal session not found"))?;
        tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
            let master = session.master.blocking_lock();
            master.resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })?;
            Ok(())
        })
        .await
        .map_err(|err| anyhow::anyhow!("terminal resize task failed: {err}"))??;
        Ok(())
    }

    /// Kill a session and remove it.
    pub async fn kill(&self, id: &str) -> anyhow::Result<()> {
        if let Some((_, session)) = self.sessions.remove(id) {
            tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
                let mut child = session.child.blocking_lock();
                child.kill()?;
                Ok(())
            })
            .await
            .map_err(|err| anyhow::anyhow!("terminal kill task failed: {err}"))??;
        }
        Ok(())
    }

    /// Remove from map only (called from async context after WS closes).
    pub fn remove(&self, id: &str) {
        self.sessions.remove(id);
    }
}
