//! Agent harness — sidecar layer that wraps `AgentRegistry::run_role`
//! with a session-aware lifecycle (TODO 9-1, sidecar slice).
//!
//! The harness does NOT yet replace the direct `agents.run_role(...)` calls
//! in build_run_node_fn — that is TODO 9-6 and would be a breaking change
//! to the orchestrator's execution path. Instead, this skeleton:
//!
//! 1. Tracks active "agent sessions" (one per spawn) with parent/child
//!    relationships, accumulated context, and rolling token usage
//! 2. Exposes a small typed API (spawn / send / status / terminate /
//!    session_tree) that mirrors what TODO 9-2 SubAgentManager and
//!    TODO 9-4 HarnessMetrics will read from
//! 3. Lives behind `Arc<AgentHarness>` so multiple call sites can share it
//!
//! The first real consumer is `metrics.rs` (TODO 9-4): it samples
//! `harness.metrics()` to populate the dashboard widget. Once the harness
//! has been observed in production for a release cycle and the API has
//! settled, TODO 9-6 will route node execution through it.

pub mod metrics;

use std::sync::Arc;
use std::time::Instant;

use anyhow::anyhow;
use dashmap::DashMap;
use uuid::Uuid;

use crate::agents::{AgentInput, AgentOutput, AgentRegistry};
use crate::router::ModelRouter;
use crate::types::{AgentRole, TokenUsage};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentSessionStatus {
    /// Session is created but no inference has run yet.
    Idle,
    /// An inference call is in flight.
    Running,
    /// At least one successful inference completed; session can be
    /// re-used (`send`) or terminated.
    Ready,
    /// Last inference returned an error. Subsequent `send` calls will
    /// surface the failure.
    Failed(String),
    /// `terminate` was called.
    Terminated,
}

#[derive(Debug)]
pub struct AgentSession {
    pub session_id: String,
    pub agent_role: AgentRole,
    pub created_at: Instant,
    pub status: AgentSessionStatus,
    /// Cumulative usage observed across every `send`/`spawn`. None when no
    /// provider returned a usage block yet.
    pub total_usage: Option<TokenUsage>,
    pub iteration_count: u32,
    pub parent_session: Option<String>,
    pub child_sessions: Vec<String>,
    /// Last AgentOutput produced. Useful for `send` so callers can replay
    /// the most recent answer without going through memory.
    pub last_output: Option<AgentOutput>,
}

impl AgentSession {
    fn new(role: AgentRole, parent: Option<String>) -> Self {
        Self {
            session_id: Uuid::new_v4().to_string(),
            agent_role: role,
            created_at: Instant::now(),
            status: AgentSessionStatus::Idle,
            total_usage: None,
            iteration_count: 0,
            parent_session: parent,
            child_sessions: Vec::new(),
            last_output: None,
        }
    }

    fn record_output(&mut self, output: &AgentOutput) {
        self.iteration_count = self.iteration_count.saturating_add(1);
        self.status = AgentSessionStatus::Ready;
        if let Some(u) = output.usage {
            self.total_usage = Some(match self.total_usage {
                Some(prev) => prev.merge(&u),
                None => u,
            });
        }
        self.last_output = Some(output.clone());
    }
}

#[derive(Debug, Clone)]
pub struct SessionTree {
    pub session_id: String,
    pub agent_role: AgentRole,
    pub status: AgentSessionStatus,
    pub total_usage: Option<TokenUsage>,
    pub iteration_count: u32,
    pub children: Vec<SessionTree>,
}

pub struct AgentHarness {
    registry: AgentRegistry,
    router: Arc<ModelRouter>,
    sessions: Arc<DashMap<String, AgentSession>>,
}

impl AgentHarness {
    pub fn new(registry: AgentRegistry, router: Arc<ModelRouter>) -> Arc<Self> {
        Arc::new(Self {
            registry,
            router,
            sessions: Arc::new(DashMap::new()),
        })
    }

    /// Spawn a new session and immediately run a single inference. Returns
    /// the session id; the caller can fetch the output via `last_output`
    /// or call `send` for follow-up turns.
    pub async fn spawn(
        &self,
        role: AgentRole,
        input: AgentInput,
        parent: Option<&str>,
    ) -> anyhow::Result<String> {
        let session = AgentSession::new(role, parent.map(str::to_string));
        let id = session.session_id.clone();

        // Wire parent/child link before running so even if the inference
        // fails, the topology is recorded.
        if let Some(parent_id) = parent {
            if let Some(mut parent_entry) = self.sessions.get_mut(parent_id) {
                parent_entry.child_sessions.push(id.clone());
            }
        }
        self.sessions.insert(id.clone(), session);

        // Mark Running, run, then update status.
        if let Some(mut entry) = self.sessions.get_mut(&id) {
            entry.status = AgentSessionStatus::Running;
        }
        let result = self
            .registry
            .run_role(role, input, self.router.clone(), None)
            .await;
        if let Some(mut entry) = self.sessions.get_mut(&id) {
            match &result {
                Ok(output) => entry.record_output(output),
                Err(err) => {
                    entry.status = AgentSessionStatus::Failed(err.to_string());
                }
            }
        }
        result.map(|_| id)
    }

    /// Run a follow-up inference on an existing session. The harness
    /// doesn't yet thread accumulated context automatically — callers
    /// build the next AgentInput themselves — but the iteration count
    /// and usage roll-up still update.
    pub async fn send(
        &self,
        session_id: &str,
        input: AgentInput,
    ) -> anyhow::Result<AgentOutput> {
        let role = self
            .sessions
            .get(session_id)
            .map(|s| s.agent_role)
            .ok_or_else(|| anyhow!("unknown session id `{session_id}`"))?;
        if let Some(mut entry) = self.sessions.get_mut(session_id) {
            entry.status = AgentSessionStatus::Running;
        }
        let result = self
            .registry
            .run_role(role, input, self.router.clone(), None)
            .await;
        if let Some(mut entry) = self.sessions.get_mut(session_id) {
            match &result {
                Ok(output) => entry.record_output(output),
                Err(err) => {
                    entry.status = AgentSessionStatus::Failed(err.to_string());
                }
            }
        }
        result
    }

    pub fn status(&self, session_id: &str) -> Option<AgentSessionStatus> {
        self.sessions.get(session_id).map(|s| s.status.clone())
    }

    /// Build a SessionTree rooted at `session_id`. Returns None if the
    /// id isn't registered.
    pub fn session_tree(&self, session_id: &str) -> Option<SessionTree> {
        let s = self.sessions.get(session_id)?;
        let children = s
            .child_sessions
            .iter()
            .filter_map(|child| self.session_tree(child))
            .collect();
        Some(SessionTree {
            session_id: s.session_id.clone(),
            agent_role: s.agent_role,
            status: s.status.clone(),
            total_usage: s.total_usage,
            iteration_count: s.iteration_count,
            children,
        })
    }

    pub async fn terminate(&self, session_id: &str) -> anyhow::Result<()> {
        let mut entry = self
            .sessions
            .get_mut(session_id)
            .ok_or_else(|| anyhow!("unknown session id `{session_id}`"))?;
        entry.status = AgentSessionStatus::Terminated;
        Ok(())
    }

    /// Register a session WITHOUT running inference. Used by the orchestrator
    /// when it has its own run_role_stream call path but still wants the
    /// harness to track lifecycle, usage, and tree structure.
    pub fn create_session(
        &self,
        role: AgentRole,
        parent: Option<&str>,
    ) -> String {
        let mut session = AgentSession::new(role, parent.map(str::to_string));
        session.status = AgentSessionStatus::Running;
        let id = session.session_id.clone();
        if let Some(parent_id) = parent {
            if let Some(mut parent_entry) = self.sessions.get_mut(parent_id) {
                parent_entry.child_sessions.push(id.clone());
            }
        }
        self.sessions.insert(id.clone(), session);
        id
    }

    /// Record an externally-produced AgentOutput against an existing session
    /// (created via `create_session`). Updates iteration count + rolling
    /// usage; does not call any LLM.
    pub fn record_output(&self, session_id: &str, output: &AgentOutput) {
        if let Some(mut entry) = self.sessions.get_mut(session_id) {
            entry.record_output(output);
        }
    }

    /// Record an externally-produced error so HarnessMetrics counts the
    /// session as Failed (mirrors what `spawn` does on inference error).
    pub fn record_error(&self, session_id: &str, err: &str) {
        if let Some(mut entry) = self.sessions.get_mut(session_id) {
            entry.status = AgentSessionStatus::Failed(err.to_string());
        }
    }

    /// All sessions currently registered (any status). Useful for metrics
    /// and the SubAgentManager's eventual collect_results path.
    pub fn sessions(&self) -> &Arc<DashMap<String, AgentSession>> {
        &self.sessions
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn router() -> Arc<ModelRouter> {
        Arc::new(ModelRouter::new(
            "http://127.0.0.1:1",
            None,
            None,
            None,
            None,
        ))
    }

    fn input(task: &str) -> AgentInput {
        AgentInput {
            task: task.to_string(),
            instructions: String::new(),
            context: crate::context::OptimizedContext::default(),
            dependency_outputs: vec![],
            brief: crate::types::StructuredBrief::default(),
            working_dir: None,
        }
    }

    #[tokio::test]
    async fn spawn_records_session_metadata_even_when_inference_fails() {
        // No provider keys are set, so run_role will fail fast. We still
        // want the session row + parent/child wiring to land.
        let harness = AgentHarness::new(AgentRegistry::builtin(), router());
        let parent_id = harness
            .spawn(AgentRole::Planner, input("plan"), None)
            .await
            .err()
            .map(|_| "intentionally errored");
        // Even on error spawn returns an Err; the session is still recorded.
        assert!(parent_id.is_some());

        // Sessions DashMap should now contain exactly one entry whose
        // status is Failed.
        assert_eq!(harness.sessions().len(), 1);
        let only = harness
            .sessions()
            .iter()
            .next()
            .map(|kv| kv.value().status.clone())
            .unwrap();
        assert!(matches!(only, AgentSessionStatus::Failed(_)));
    }

    #[tokio::test]
    async fn unknown_session_send_errors() {
        let harness = AgentHarness::new(AgentRegistry::builtin(), router());
        let err = harness
            .send("nonexistent", input("hi"))
            .await
            .err()
            .expect("must error");
        assert!(err.to_string().contains("unknown session id"));
    }

    #[tokio::test]
    async fn terminate_marks_session_terminated() {
        let harness = AgentHarness::new(AgentRegistry::builtin(), router());
        // Manually insert a synthetic session so we don't rely on a real
        // inference round-trip.
        let session = AgentSession::new(AgentRole::Reviewer, None);
        let id = session.session_id.clone();
        harness.sessions().insert(id.clone(), session);

        harness.terminate(&id).await.unwrap();
        assert!(matches!(
            harness.status(&id).unwrap(),
            AgentSessionStatus::Terminated
        ));
    }

    #[test]
    fn session_tree_builds_recursively() {
        let harness = AgentHarness::new(AgentRegistry::builtin(), router());
        let root = AgentSession::new(AgentRole::Planner, None);
        let root_id = root.session_id.clone();
        harness.sessions().insert(root_id.clone(), root);

        let child = AgentSession::new(AgentRole::Coder, Some(root_id.clone()));
        let child_id = child.session_id.clone();
        harness.sessions().insert(child_id.clone(), child);
        harness
            .sessions()
            .get_mut(&root_id)
            .unwrap()
            .child_sessions
            .push(child_id.clone());

        let tree = harness.session_tree(&root_id).unwrap();
        assert_eq!(tree.session_id, root_id);
        assert_eq!(tree.children.len(), 1);
        assert_eq!(tree.children[0].session_id, child_id);
    }
}
