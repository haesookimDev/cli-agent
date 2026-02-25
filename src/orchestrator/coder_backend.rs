use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use chrono::Utc;
use dashmap::DashMap;
use tokio::io::AsyncBufReadExt;
use tokio::process::Command;
use uuid::Uuid;

use crate::memory::MemoryManager;
use crate::router::ModelRouter;
use crate::types::{CoderBackendKind, CoderOutputChunk, CoderSessionResult};

/// Trait for coder execution backends.
#[async_trait]
pub trait CoderBackend: Send + Sync {
    fn kind(&self) -> CoderBackendKind;

    async fn run(
        &self,
        task: &str,
        context: &str,
        working_dir: &Path,
        on_chunk: Arc<dyn Fn(CoderOutputChunk) + Send + Sync>,
    ) -> anyhow::Result<CoderSessionResult>;
}

// --- LlmCoderBackend ---

pub struct LlmCoderBackend {
    router: Arc<ModelRouter>,
}

impl LlmCoderBackend {
    pub fn new(router: Arc<ModelRouter>) -> Self {
        Self { router }
    }
}

#[async_trait]
impl CoderBackend for LlmCoderBackend {
    fn kind(&self) -> CoderBackendKind {
        CoderBackendKind::Llm
    }

    async fn run(
        &self,
        task: &str,
        context: &str,
        _working_dir: &Path,
        _on_chunk: Arc<dyn Fn(CoderOutputChunk) + Send + Sync>,
    ) -> anyhow::Result<CoderSessionResult> {
        let prompt = if context.is_empty() {
            task.to_string()
        } else {
            format!("{context}\n\n{task}")
        };
        let constraints = crate::router::RoutingConstraints::for_profile(
            crate::types::TaskProfile::Coding,
        );
        let (_decision, inference) = self
            .router
            .infer(crate::types::TaskProfile::Coding, &prompt, &constraints)
            .await?;

        Ok(CoderSessionResult {
            output: inference.output,
            exit_code: 0,
            files_changed: vec![],
            duration_ms: 0,
        })
    }
}

// --- ClaudeCodeBackend ---

pub struct ClaudeCodeBackend {
    command: String,
    args: Vec<String>,
    timeout: Duration,
}

impl ClaudeCodeBackend {
    pub fn new(command: String, args: Vec<String>, timeout: Duration) -> Self {
        Self {
            command,
            args,
            timeout,
        }
    }
}

#[async_trait]
impl CoderBackend for ClaudeCodeBackend {
    fn kind(&self) -> CoderBackendKind {
        CoderBackendKind::ClaudeCode
    }

    async fn run(
        &self,
        task: &str,
        _context: &str,
        working_dir: &Path,
        on_chunk: Arc<dyn Fn(CoderOutputChunk) + Send + Sync>,
    ) -> anyhow::Result<CoderSessionResult> {
        let session_id = Uuid::new_v4().to_string();
        let started = Instant::now();

        let mut cmd = Command::new(&self.command);
        cmd.arg("-p")
            .arg(task)
            .arg("--output-format")
            .arg("stream-json")
            .args(&self.args)
            .current_dir(working_dir)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());

        let mut child = cmd.spawn()?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow::anyhow!("failed to capture stdout"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| anyhow::anyhow!("failed to capture stderr"))?;

        let sid = session_id.clone();
        let chunk_fn = on_chunk.clone();
        let stdout_handle = tokio::spawn(async move {
            let reader = tokio::io::BufReader::new(stdout);
            let mut lines = reader.lines();
            let mut collected = String::new();
            while let Ok(Some(line)) = lines.next_line().await {
                collected.push_str(&line);
                collected.push('\n');
                chunk_fn(CoderOutputChunk {
                    session_id: sid.clone(),
                    stream: "stdout".to_string(),
                    content: line,
                    timestamp: Utc::now(),
                });
            }
            collected
        });

        let sid2 = session_id.clone();
        let chunk_fn2 = on_chunk;
        let stderr_handle = tokio::spawn(async move {
            let reader = tokio::io::BufReader::new(stderr);
            let mut lines = reader.lines();
            while let Ok(Some(line)) = lines.next_line().await {
                chunk_fn2(CoderOutputChunk {
                    session_id: sid2.clone(),
                    stream: "stderr".to_string(),
                    content: line,
                    timestamp: Utc::now(),
                });
            }
        });

        let status = tokio::time::timeout(self.timeout, child.wait())
            .await
            .map_err(|_| anyhow::anyhow!("coder session timed out after {:?}", self.timeout))?
            .map_err(|e| anyhow::anyhow!("process wait failed: {e}"))?;

        let stdout_output = stdout_handle.await.unwrap_or_default();
        let _ = stderr_handle.await;

        Ok(CoderSessionResult {
            output: stdout_output,
            exit_code: status.code().unwrap_or(-1),
            files_changed: vec![],
            duration_ms: started.elapsed().as_millis(),
        })
    }
}

// --- CodexBackend ---

pub struct CodexBackend {
    command: String,
    args: Vec<String>,
    timeout: Duration,
}

impl CodexBackend {
    pub fn new(command: String, args: Vec<String>, timeout: Duration) -> Self {
        Self {
            command,
            args,
            timeout,
        }
    }
}

#[async_trait]
impl CoderBackend for CodexBackend {
    fn kind(&self) -> CoderBackendKind {
        CoderBackendKind::Codex
    }

    async fn run(
        &self,
        task: &str,
        _context: &str,
        working_dir: &Path,
        on_chunk: Arc<dyn Fn(CoderOutputChunk) + Send + Sync>,
    ) -> anyhow::Result<CoderSessionResult> {
        let session_id = Uuid::new_v4().to_string();
        let started = Instant::now();

        let mut cmd = Command::new(&self.command);
        cmd.arg("--approval-mode")
            .arg("full-auto")
            .arg(task)
            .args(&self.args)
            .current_dir(working_dir)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());

        let mut child = cmd.spawn()?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow::anyhow!("failed to capture stdout"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| anyhow::anyhow!("failed to capture stderr"))?;

        let sid = session_id.clone();
        let chunk_fn = on_chunk.clone();
        let stdout_handle = tokio::spawn(async move {
            let reader = tokio::io::BufReader::new(stdout);
            let mut lines = reader.lines();
            let mut collected = String::new();
            while let Ok(Some(line)) = lines.next_line().await {
                collected.push_str(&line);
                collected.push('\n');
                chunk_fn(CoderOutputChunk {
                    session_id: sid.clone(),
                    stream: "stdout".to_string(),
                    content: line,
                    timestamp: Utc::now(),
                });
            }
            collected
        });

        let sid2 = session_id.clone();
        let chunk_fn2 = on_chunk;
        let stderr_handle = tokio::spawn(async move {
            let reader = tokio::io::BufReader::new(stderr);
            let mut lines = reader.lines();
            while let Ok(Some(line)) = lines.next_line().await {
                chunk_fn2(CoderOutputChunk {
                    session_id: sid2.clone(),
                    stream: "stderr".to_string(),
                    content: line,
                    timestamp: Utc::now(),
                });
            }
        });

        let status = tokio::time::timeout(self.timeout, child.wait())
            .await
            .map_err(|_| anyhow::anyhow!("coder session timed out after {:?}", self.timeout))?
            .map_err(|e| anyhow::anyhow!("process wait failed: {e}"))?;

        let stdout_output = stdout_handle.await.unwrap_or_default();
        let _ = stderr_handle.await;

        Ok(CoderSessionResult {
            output: stdout_output,
            exit_code: status.code().unwrap_or(-1),
            files_changed: vec![],
            duration_ms: started.elapsed().as_millis(),
        })
    }
}

// --- CoderSessionManager ---

struct CoderSessionState {
    _id: String,
    _run_id: Uuid,
    _node_id: String,
    _backend: CoderBackendKind,
    _working_dir: PathBuf,
    _started_at: Instant,
}

pub struct CoderSessionManager {
    backends: HashMap<CoderBackendKind, Arc<dyn CoderBackend>>,
    active_sessions: DashMap<String, CoderSessionState>,
    memory: Arc<MemoryManager>,
    default_working_dir: PathBuf,
    default_backend: CoderBackendKind,
}

impl CoderSessionManager {
    pub fn new(
        memory: Arc<MemoryManager>,
        default_working_dir: PathBuf,
        default_backend: CoderBackendKind,
    ) -> Self {
        Self {
            backends: HashMap::new(),
            active_sessions: DashMap::new(),
            memory,
            default_working_dir,
            default_backend,
        }
    }

    pub fn register_backend(&mut self, backend: Arc<dyn CoderBackend>) {
        self.backends.insert(backend.kind(), backend);
    }

    pub fn default_backend_kind(&self) -> CoderBackendKind {
        self.default_backend
    }

    pub async fn run_session(
        &self,
        run_id: Uuid,
        node_id: &str,
        backend_kind: CoderBackendKind,
        task: &str,
        context: &str,
        on_chunk: Arc<dyn Fn(CoderOutputChunk) + Send + Sync>,
    ) -> anyhow::Result<CoderSessionResult> {
        let backend = self
            .backends
            .get(&backend_kind)
            .ok_or_else(|| anyhow::anyhow!("coder backend {:?} not registered", backend_kind))?;

        let session_id = Uuid::new_v4().to_string();
        let working_dir = self.default_working_dir.clone();

        // Record session start in DB
        let _ = self
            .memory
            .store()
            .insert_coder_session(
                &session_id,
                run_id,
                node_id,
                &backend_kind.to_string(),
                None,
                Some(working_dir.to_str().unwrap_or("")),
                "running",
            )
            .await;

        // Track in memory
        self.active_sessions.insert(
            session_id.clone(),
            CoderSessionState {
                _id: session_id.clone(),
                _run_id: run_id,
                _node_id: node_id.to_string(),
                _backend: backend_kind,
                _working_dir: working_dir.clone(),
                _started_at: Instant::now(),
            },
        );

        // Execute
        let result = backend.run(task, context, &working_dir, on_chunk).await;

        // Update DB and cleanup
        match &result {
            Ok(r) => {
                let files_json = serde_json::to_string(&r.files_changed).ok();
                let _ = self
                    .memory
                    .store()
                    .update_coder_session_completed(
                        &session_id,
                        "completed",
                        Some(r.exit_code),
                        files_json.as_deref(),
                    )
                    .await;
            }
            Err(_) => {
                let _ = self
                    .memory
                    .store()
                    .update_coder_session_completed(&session_id, "failed", None, None)
                    .await;
            }
        }

        self.active_sessions.remove(&session_id);
        result
    }

    pub async fn list_sessions_for_run(
        &self,
        run_id: Uuid,
    ) -> anyhow::Result<Vec<serde_json::Value>> {
        self.memory.store().list_coder_sessions_for_run(run_id).await
    }
}

/// Flush a byte buffer as a UTF-8 string, preserving incomplete codepoints.
pub fn flush_utf8_safe(buffer: &mut Vec<u8>) -> String {
    match std::str::from_utf8(buffer) {
        Ok(s) => {
            let out = s.to_string();
            buffer.clear();
            out
        }
        Err(e) => {
            let valid_up_to = e.valid_up_to();
            let out = std::str::from_utf8(&buffer[..valid_up_to])
                .unwrap()
                .to_string();
            *buffer = buffer[valid_up_to..].to_vec();
            out
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flush_utf8_safe_handles_complete_codepoints() {
        let mut buf = "hello".as_bytes().to_vec();
        assert_eq!(flush_utf8_safe(&mut buf), "hello");
        assert!(buf.is_empty());
    }

    #[test]
    fn flush_utf8_safe_preserves_incomplete_codepoints() {
        let full = "한".as_bytes(); // 3 bytes in UTF-8
        let mut buf = full[..2].to_vec();
        let result = flush_utf8_safe(&mut buf);
        assert_eq!(result, "");
        assert_eq!(buf.len(), 2);

        buf.push(full[2]);
        let result = flush_utf8_safe(&mut buf);
        assert_eq!(result, "한");
        assert!(buf.is_empty());
    }

    #[test]
    fn flush_utf8_safe_mixed_content() {
        let input = "abc한d";
        let bytes = input.as_bytes();
        // "abc" = 3 bytes, "한" = 3 bytes, "d" = 1 byte, total 7
        // Split at byte 5 (middle of "한")
        let mut buf = bytes[..5].to_vec();
        let result = flush_utf8_safe(&mut buf);
        assert_eq!(result, "abc");
        assert_eq!(buf.len(), 2);

        buf.extend_from_slice(&bytes[5..]);
        let result = flush_utf8_safe(&mut buf);
        assert_eq!(result, "한d");
        assert!(buf.is_empty());
    }
}
