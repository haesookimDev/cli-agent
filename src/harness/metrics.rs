//! HarnessMetrics — observability snapshot for the AgentHarness (TODO 9-4).
//!
//! Counters are derived on-demand by walking `harness.sessions()` so the
//! metrics surface stays in lock-step with the harness state without
//! needing a separate update path. Cheap enough to call from a `/v1/harness/metrics`
//! handler or a periodic dashboard pull.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::harness::{AgentHarness, AgentSessionStatus};
use crate::types::{AgentRole, TokenUsage};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RoleStats {
    pub active: u32,
    pub completed: u32,
    pub failed: u32,
    pub iterations: u32,
    pub tokens: TokenUsage,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct HarnessMetrics {
    pub active_sessions: u32,
    pub idle_sessions: u32,
    pub completed_sessions: u32,
    pub failed_sessions: u32,
    pub terminated_sessions: u32,
    pub total_iterations: u32,
    pub total_tokens: TokenUsage,
    /// Maximum nesting depth observed (root = 0). Useful for detecting
    /// runaway sub-agent spawning.
    pub sub_agent_depth: u32,
    /// Per-role rollups keyed by AgentRole's snake_case Display string.
    pub per_role: HashMap<String, RoleStats>,
}

impl HarnessMetrics {
    pub fn snapshot(harness: &AgentHarness) -> Self {
        let sessions = harness.sessions();
        let mut metrics = HarnessMetrics::default();
        let mut roots: Vec<String> = Vec::new();
        for entry in sessions.iter() {
            let s = entry.value();
            match &s.status {
                AgentSessionStatus::Idle => metrics.idle_sessions += 1,
                AgentSessionStatus::Running => metrics.active_sessions += 1,
                AgentSessionStatus::Ready => metrics.completed_sessions += 1,
                AgentSessionStatus::Failed(_) => metrics.failed_sessions += 1,
                AgentSessionStatus::Terminated => metrics.terminated_sessions += 1,
            }
            metrics.total_iterations =
                metrics.total_iterations.saturating_add(s.iteration_count);
            if let Some(u) = s.total_usage {
                metrics.total_tokens = metrics.total_tokens.merge(&u);
            }
            if s.parent_session.is_none() {
                roots.push(s.session_id.clone());
            }
            let role_key = role_key(s.agent_role);
            let stats = metrics.per_role.entry(role_key).or_default();
            stats.iterations = stats.iterations.saturating_add(s.iteration_count);
            if let Some(u) = s.total_usage {
                stats.tokens = stats.tokens.merge(&u);
            }
            match &s.status {
                AgentSessionStatus::Running => stats.active += 1,
                AgentSessionStatus::Ready => stats.completed += 1,
                AgentSessionStatus::Failed(_) => stats.failed += 1,
                _ => {}
            }
        }

        metrics.sub_agent_depth = roots
            .iter()
            .map(|r| max_depth(harness, r, 0))
            .max()
            .unwrap_or(0);

        metrics
    }
}

fn max_depth(harness: &AgentHarness, session_id: &str, depth: u32) -> u32 {
    let Some(s) = harness.sessions().get(session_id) else {
        return depth;
    };
    if s.child_sessions.is_empty() {
        return depth;
    }
    let children: Vec<String> = s.child_sessions.clone();
    drop(s);
    children
        .iter()
        .map(|c| max_depth(harness, c, depth + 1))
        .max()
        .unwrap_or(depth)
}

fn role_key(role: AgentRole) -> String {
    format!("{role:?}").to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::AgentRegistry;
    use crate::harness::{AgentHarness, AgentSession};
    use crate::router::ModelRouter;
    use std::sync::Arc;

    fn make_harness() -> Arc<AgentHarness> {
        let router = Arc::new(ModelRouter::new(
            "http://127.0.0.1:1",
            None,
            None,
            None,
            None,
        ));
        AgentHarness::new(AgentRegistry::builtin(), router)
    }

    #[test]
    fn empty_harness_yields_zero_snapshot() {
        let h = make_harness();
        let m = HarnessMetrics::snapshot(&h);
        assert_eq!(m.active_sessions, 0);
        assert_eq!(m.completed_sessions, 0);
        assert_eq!(m.total_iterations, 0);
    }

    #[test]
    fn snapshot_counts_sessions_per_status_and_role() {
        let h = make_harness();
        // root: ready (Coder, 2 iterations, 100 tokens)
        let mut root = AgentSession::new(AgentRole::Coder, None);
        root.iteration_count = 2;
        root.total_usage = Some(TokenUsage {
            input_tokens: 60,
            output_tokens: 40,
        });
        root.status = AgentSessionStatus::Ready;
        let root_id = root.session_id.clone();
        h.sessions().insert(root_id.clone(), root);

        // child: failed (Reviewer)
        let mut child = AgentSession::new(AgentRole::Reviewer, Some(root_id.clone()));
        child.status = AgentSessionStatus::Failed("nope".to_string());
        let child_id = child.session_id.clone();
        h.sessions().insert(child_id.clone(), child);
        h.sessions()
            .get_mut(&root_id)
            .unwrap()
            .child_sessions
            .push(child_id);

        let m = HarnessMetrics::snapshot(&h);
        assert_eq!(m.completed_sessions, 1);
        assert_eq!(m.failed_sessions, 1);
        assert_eq!(m.total_iterations, 2);
        assert_eq!(m.total_tokens.input_tokens, 60);
        assert_eq!(m.total_tokens.output_tokens, 40);
        assert_eq!(m.sub_agent_depth, 1);

        let coder = m.per_role.get("coder").expect("coder bucket");
        assert_eq!(coder.completed, 1);
        assert_eq!(coder.iterations, 2);

        let reviewer = m.per_role.get("reviewer").expect("reviewer bucket");
        assert_eq!(reviewer.failed, 1);
    }
}
