//! Inter-session message bus (TODO 9-3 / D4).
//!
//! Use case (per the user's prompt — "서브에이전트와의 통신을 주로 사용할거 같아"):
//! sub-agents need a way to push status updates and short queries back up to
//! their parent (or sideways across siblings) without serializing through the
//! orchestrator's run loop. The bus is intentionally narrow:
//!
//! - In-process only — no cross-host delivery
//! - tokio::sync::mpsc per session, capped queue depth
//! - No persistence — messages live for the duration of the run
//!
//! The agent execution path (`run_role_stream`) doesn't yet read from the
//! bus. This module exposes the primitives so future iterations can wire
//! receive into a node's `AgentInput.context` or surface inbox events on
//! the trace UI without reshaping the data model again.

use std::sync::Arc;
use std::time::SystemTime;

use anyhow::anyhow;
use dashmap::DashMap;
use tokio::sync::mpsc;

const DEFAULT_QUEUE_CAP: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MessageKind {
    /// Sub-agent → parent: a question that expects a response.
    Query,
    /// Parent → sub-agent: corrective steer-back.
    Feedback,
    /// Sub-agent → sibling: hand-off / ask-for-help.
    Delegation,
    /// Anyone → anyone: status / progress beat (no expected reply).
    Status,
}

#[derive(Debug, Clone)]
pub struct AgentMessage {
    pub from_session: String,
    pub to_session: String,
    pub kind: MessageKind,
    pub body: String,
    pub sent_at: SystemTime,
}

impl AgentMessage {
    pub fn new(
        from: impl Into<String>,
        to: impl Into<String>,
        kind: MessageKind,
        body: impl Into<String>,
    ) -> Self {
        Self {
            from_session: from.into(),
            to_session: to.into(),
            kind,
            body: body.into(),
            sent_at: SystemTime::now(),
        }
    }
}

#[derive(Debug)]
struct Mailbox {
    tx: mpsc::Sender<AgentMessage>,
    rx: tokio::sync::Mutex<mpsc::Receiver<AgentMessage>>,
}

#[derive(Debug, Default)]
pub struct MessageBus {
    boxes: DashMap<String, Arc<Mailbox>>,
    queue_cap: usize,
}

impl MessageBus {
    pub fn new() -> Self {
        Self {
            boxes: DashMap::new(),
            queue_cap: DEFAULT_QUEUE_CAP,
        }
    }

    pub fn with_queue_cap(cap: usize) -> Self {
        Self {
            boxes: DashMap::new(),
            queue_cap: cap.max(1),
        }
    }

    /// Ensure a mailbox exists for `session_id`. Idempotent — calling twice
    /// is a no-op so spawning agents can register without coordinating.
    pub fn ensure_mailbox(&self, session_id: &str) {
        if self.boxes.contains_key(session_id) {
            return;
        }
        let (tx, rx) = mpsc::channel(self.queue_cap);
        self.boxes.insert(
            session_id.to_string(),
            Arc::new(Mailbox {
                tx,
                rx: tokio::sync::Mutex::new(rx),
            }),
        );
    }

    /// Send a message into `to_session`'s mailbox. Returns Err if the target
    /// has no mailbox or its queue is full (the latter usually means the
    /// receiver is stuck — caller decides whether to retry or drop).
    pub async fn send(&self, msg: AgentMessage) -> anyhow::Result<()> {
        self.ensure_mailbox(msg.to_session.as_str());
        let mailbox = self
            .boxes
            .get(msg.to_session.as_str())
            .ok_or_else(|| anyhow!("mailbox `{}` missing", msg.to_session))?
            .clone();
        mailbox
            .tx
            .send(msg)
            .await
            .map_err(|_| anyhow!("mailbox closed"))
    }

    /// Try to read the next pending message for `session_id` without
    /// blocking. Returns None when the mailbox is empty.
    pub async fn try_recv(&self, session_id: &str) -> Option<AgentMessage> {
        let mailbox = self.boxes.get(session_id)?.clone();
        let mut rx = mailbox.rx.lock().await;
        rx.try_recv().ok()
    }

    /// Drain every pending message for `session_id` into a Vec.
    pub async fn drain(&self, session_id: &str) -> Vec<AgentMessage> {
        let mut out = Vec::new();
        let Some(mailbox) = self.boxes.get(session_id).map(|kv| kv.clone()) else {
            return out;
        };
        let mut rx = mailbox.rx.lock().await;
        while let Ok(msg) = rx.try_recv() {
            out.push(msg);
        }
        out
    }

    /// Drop the mailbox when a session terminates. Pending messages are
    /// dropped too — the receiver is gone.
    pub fn close(&self, session_id: &str) {
        self.boxes.remove(session_id);
    }

    pub fn len(&self) -> usize {
        self.boxes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.boxes.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn send_then_drain_round_trips_messages_in_order() {
        let bus = MessageBus::new();
        bus.ensure_mailbox("dest");
        bus.send(AgentMessage::new("a", "dest", MessageKind::Status, "first"))
            .await
            .unwrap();
        bus.send(AgentMessage::new("a", "dest", MessageKind::Status, "second"))
            .await
            .unwrap();
        let drained = bus.drain("dest").await;
        assert_eq!(drained.len(), 2);
        assert_eq!(drained[0].body, "first");
        assert_eq!(drained[1].body, "second");
    }

    #[tokio::test]
    async fn ensure_mailbox_is_idempotent() {
        let bus = MessageBus::new();
        bus.ensure_mailbox("a");
        bus.ensure_mailbox("a");
        assert_eq!(bus.len(), 1);
    }

    #[tokio::test]
    async fn try_recv_returns_none_on_empty_mailbox() {
        let bus = MessageBus::new();
        assert!(bus.try_recv("nope").await.is_none());
        bus.ensure_mailbox("here");
        assert!(bus.try_recv("here").await.is_none());
    }

    #[tokio::test]
    async fn close_removes_mailbox_and_drops_pending() {
        let bus = MessageBus::new();
        bus.ensure_mailbox("dest");
        bus.send(AgentMessage::new("x", "dest", MessageKind::Query, "hi"))
            .await
            .unwrap();
        bus.close("dest");
        // After close: drain returns empty and len = 0.
        let drained = bus.drain("dest").await;
        assert!(drained.is_empty());
        assert!(bus.is_empty());
    }

    #[tokio::test]
    async fn full_queue_returns_err() {
        let bus = MessageBus::with_queue_cap(1);
        bus.ensure_mailbox("dest");
        bus.send(AgentMessage::new("a", "dest", MessageKind::Status, "1"))
            .await
            .unwrap();
        // Second send blocks because the queue is full (cap=1, no consumer).
        // Use a short timeout to keep the test fast.
        let res = tokio::time::timeout(
            std::time::Duration::from_millis(50),
            bus.send(AgentMessage::new("a", "dest", MessageKind::Status, "2")),
        )
        .await;
        assert!(res.is_err(), "expected the second send to block");
    }
}
