use std::io::{Read, Write};
use std::sync::Arc;

use chrono::{DateTime, Utc};
use dashmap::DashMap;
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use uuid::Uuid;

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
    /// Read end: receive bytes from the PTY (output to browser).
    /// Wrapped in Option so it can be taken once when the WS reader loop starts.
    pub reader: Arc<Mutex<Option<Box<dyn Read + Send>>>>,
    /// PTY master handle for resize operations.
    pub master: Arc<Mutex<Box<dyn MasterPty + Send>>>,
    /// Child process handle.
    pub child: Arc<Mutex<Box<dyn Child + Send + Sync>>>,
    /// Optional link to an orchestrator run.
    pub run_id: Option<Uuid>,
    pub session_id: Option<Uuid>,
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

        let mut cmd = CommandBuilder::new(command);
        cmd.args(args);

        let child = pair.slave.spawn_command(cmd)?;
        let writer = pair.master.take_writer()?;
        let reader = pair.master.try_clone_reader()?;

        let id = Uuid::new_v4().to_string();
        let session = Arc::new(TerminalSession {
            id: id.clone(),
            command: command.to_string(),
            args: args.to_vec(),
            created_at: Utc::now(),
            cols,
            rows,
            writer: Arc::new(Mutex::new(writer)),
            reader: Arc::new(Mutex::new(Some(reader))),
            master: Arc::new(Mutex::new(pair.master)),
            child: Arc::new(Mutex::new(child)),
            run_id,
            session_id,
        });

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
    pub fn resize(&self, id: &str, cols: u16, rows: u16) -> anyhow::Result<()> {
        let session = self
            .sessions
            .get(id)
            .ok_or_else(|| anyhow::anyhow!("terminal session not found"))?;
        let master = session.master.blocking_lock();
        master.resize(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })?;
        Ok(())
    }

    /// Kill a session and remove it.
    pub fn kill(&self, id: &str) -> anyhow::Result<()> {
        if let Some((_, session)) = self.sessions.remove(id) {
            let mut child = session.child.blocking_lock();
            child.kill()?;
        }
        Ok(())
    }

    /// Remove from map only (called from async context after WS closes).
    pub fn remove(&self, id: &str) {
        self.sessions.remove(id);
    }
}
