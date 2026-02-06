use std::path::{Path, PathBuf};

use anyhow::Context;
use tokio::fs::{self, OpenOptions};
use tokio::io::AsyncWriteExt;
use uuid::Uuid;

use crate::types::{ReplayEvent, SessionEvent};

#[derive(Debug, Clone)]
pub struct SessionLogger {
    root: PathBuf,
}

impl SessionLogger {
    pub fn new(root: impl AsRef<Path>) -> Self {
        Self {
            root: root.as_ref().to_path_buf(),
        }
    }

    pub async fn append_event(&self, session_id: Uuid, event: &SessionEvent) -> anyhow::Result<()> {
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
            if line.trim().is_empty() {
                continue;
            }
            let event: SessionEvent = serde_json::from_str(line)
                .with_context(|| format!("failed to decode jsonl line {}", line_no + 1))?;
            events.push(ReplayEvent {
                line: line_no + 1,
                event,
            });
        }

        Ok(events)
    }

    pub fn path_for(&self, session_id: Uuid) -> PathBuf {
        self.root.join(format!("{}.jsonl", session_id))
    }
}

#[cfg(test)]
mod tests {
    use chrono::Utc;

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
}
