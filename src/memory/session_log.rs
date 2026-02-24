use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Context;
use dashmap::DashMap;
use tokio::fs::{self, OpenOptions};
use tokio::io::AsyncWriteExt;
use tokio::sync::Mutex;
use uuid::Uuid;

use crate::types::{ReplayEvent, SessionEvent};

#[derive(Debug, Clone)]
pub struct SessionLogger {
    root: PathBuf,
    append_locks: Arc<DashMap<Uuid, Arc<Mutex<()>>>>,
}

impl SessionLogger {
    pub fn new(root: impl AsRef<Path>) -> Self {
        Self {
            root: root.as_ref().to_path_buf(),
            append_locks: Arc::new(DashMap::new()),
        }
    }

    pub async fn append_event(&self, session_id: Uuid, event: &SessionEvent) -> anyhow::Result<()> {
        let lock = self
            .append_locks
            .entry(session_id)
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone();
        let _guard = lock.lock().await;

        fs::create_dir_all(&self.root)
            .await
            .with_context(|| format!("failed to create session root {}", self.root.display()))?;

        let path = self.path_for(session_id);
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .await
            .with_context(|| format!("failed to open session log {}", path.display()))?;

        let line = serde_json::to_string(event)?;
        file.write_all(line.as_bytes()).await?;
        file.write_all(b"\n").await?;
        file.flush().await?;
        Ok(())
    }

    pub async fn replay(&self, session_id: Uuid) -> anyhow::Result<Vec<ReplayEvent>> {
        let path = self.path_for(session_id);
        if !path.exists() {
            return Ok(vec![]);
        }

        let content = fs::read_to_string(&path)
            .await
            .with_context(|| format!("failed to read session log {}", path.display()))?;

        let mut events = Vec::new();
        for (line_no, line) in content.lines().enumerate() {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }

            let mut parsed_any = false;
            let mut stream =
                serde_json::Deserializer::from_str(trimmed).into_iter::<SessionEvent>();
            while let Some(item) = stream.next() {
                let event =
                    item.with_context(|| format!("failed to decode jsonl line {}", line_no + 1))?;
                events.push(ReplayEvent {
                    line: line_no + 1,
                    event,
                });
                parsed_any = true;
            }

            if !parsed_any {
                anyhow::bail!("failed to decode jsonl line {}", line_no + 1);
            }
        }

        Ok(events)
    }

    pub fn path_for(&self, session_id: Uuid) -> PathBuf {
        self.root.join(format!("{}.jsonl", session_id))
    }

    pub async fn delete(&self, session_id: Uuid) -> anyhow::Result<()> {
        let path = self.path_for(session_id);
        if !path.exists() {
            return Ok(());
        }
        fs::remove_file(&path)
            .await
            .with_context(|| format!("failed to remove session log {}", path.display()))?;
        self.append_locks.remove(&session_id);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use tokio::fs;

    use super::*;
    use crate::types::{SessionEvent, SessionEventType};

    #[tokio::test]
    async fn append_and_replay_roundtrip() {
        let tmp_root = std::env::temp_dir().join(format!("agent-session-log-{}", Uuid::new_v4()));
        let logger = SessionLogger::new(tmp_root.as_path());
        let session_id = Uuid::new_v4();

        let event = SessionEvent {
            session_id,
            run_id: Some(Uuid::new_v4()),
            event_type: SessionEventType::UserMessage,
            timestamp: Utc::now(),
            payload: serde_json::json!({"hello": "world"}),
        };

        logger.append_event(session_id, &event).await.unwrap();
        let replayed = logger.replay(session_id).await.unwrap();

        assert_eq!(replayed.len(), 1);
        assert_eq!(replayed[0].event.event_type, SessionEventType::UserMessage);
    }

    #[tokio::test]
    async fn replay_handles_concatenated_json_values_on_same_line() {
        let tmp_root = std::env::temp_dir().join(format!("agent-session-log-{}", Uuid::new_v4()));
        let logger = SessionLogger::new(tmp_root.as_path());
        let session_id = Uuid::new_v4();
        let path = logger.path_for(session_id);

        fs::create_dir_all(tmp_root).await.unwrap();

        let event_a = SessionEvent {
            session_id,
            run_id: Some(Uuid::new_v4()),
            event_type: SessionEventType::UserMessage,
            timestamp: Utc::now(),
            payload: serde_json::json!({"text": "first"}),
        };
        let event_b = SessionEvent {
            session_id,
            run_id: Some(Uuid::new_v4()),
            event_type: SessionEventType::RunCompleted,
            timestamp: Utc::now(),
            payload: serde_json::json!({"status": "ok"}),
        };

        let line = format!(
            "{}{}\n",
            serde_json::to_string(&event_a).unwrap(),
            serde_json::to_string(&event_b).unwrap()
        );
        fs::write(path, line).await.unwrap();

        let replayed = logger.replay(session_id).await.unwrap();
        assert_eq!(replayed.len(), 2);
        assert_eq!(replayed[0].event.event_type, SessionEventType::UserMessage);
        assert_eq!(replayed[1].event.event_type, SessionEventType::RunCompleted);
    }
}
