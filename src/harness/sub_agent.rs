//! SubAgentManager — turns Planner SubtaskPlan output into a tracked subtree
//! of harness sessions (TODO 9-2 / D3).
//!
//! `completion.rs::build_on_completed_fn` already handles the SubtaskPlan →
//! AgentNode injection that grows the runtime DAG. This module sits next to
//! that path and registers each subtask as a child harness session of the
//! parent node, so HarnessMetrics + session_tree reflect parent → child
//! relationships at the harness layer too.
//!
//! Today this module is *additive*: it records hierarchy and aggregates
//! results. The actual inference for each subtask still runs through the
//! orchestrator's normal build_run_node_fn → run_role_stream path; the
//! harness sessions created here become the lookup targets for
//! `record_output` / `record_error` on subsequent calls.

use std::sync::Arc;

use anyhow::anyhow;
use uuid::Uuid;

use crate::harness::{AgentHarness, AgentSessionStatus};
use crate::types::{AgentRole, SubtaskDefinition, SubtaskPlan};

/// Manager that wraps the harness for SubtaskPlan-driven sub-agent spawning.
///
/// Lives on `Orchestrator` so both `completion.rs` (when injecting subtask
/// nodes into the graph) and the future Phase 11-D message bus can register
/// children against the same harness.
#[derive(Clone)]
pub struct SubAgentManager {
    harness: Arc<AgentHarness>,
}

impl SubAgentManager {
    pub fn new(harness: Arc<AgentHarness>) -> Self {
        Self { harness }
    }

    pub fn harness(&self) -> &Arc<AgentHarness> {
        &self.harness
    }

    /// Spawn one harness session for each subtask in `plan`, attached as a
    /// child of `parent_session_id`. Returns `(subtask_id → harness_session_id)`
    /// pairs in input order so callers can map back when the subtask actually
    /// executes.
    pub fn spawn_from_plan(
        &self,
        parent_session_id: &str,
        plan: &SubtaskPlan,
    ) -> Vec<(String, String)> {
        plan.subtasks
            .iter()
            .map(|s| {
                let session_id = self
                    .harness
                    .create_session(s.agent_role, Some(parent_session_id));
                (s.id.clone(), session_id)
            })
            .collect()
    }

    /// Convenience: spawn a single child session for a one-off subtask.
    pub fn spawn_child(
        &self,
        parent_session_id: &str,
        role: AgentRole,
    ) -> String {
        self.harness.create_session(role, Some(parent_session_id))
    }

    /// Aggregate the contents (last_output) of every direct child of
    /// `parent_session_id`. Used by recovery/continuation paths that want to
    /// summarize the most recent fan-out result.
    pub fn collect_child_outputs(&self, parent_session_id: &str) -> Vec<ChildOutput> {
        let parent_clone = match self.harness.sessions().get(parent_session_id) {
            Some(s) => s.child_sessions.clone(),
            None => return Vec::new(),
        };
        parent_clone
            .iter()
            .filter_map(|child_id| {
                self.harness.sessions().get(child_id).and_then(|kv| {
                    kv.last_output.as_ref().map(|out| ChildOutput {
                        session_id: child_id.clone(),
                        agent_role: kv.agent_role,
                        content: out.content.clone(),
                        status: kv.status.clone(),
                    })
                })
            })
            .collect()
    }

    /// Notify the harness that one of `parent_session_id`'s children failed.
    /// Returns a recovery hint based on how many siblings already failed —
    /// the orchestrator's recovery loop reads this to decide whether to keep
    /// going or escalate.
    pub fn classify_child_failure(
        &self,
        parent_session_id: &str,
        child_session_id: &str,
        error: &str,
    ) -> Result<RecoveryAction, anyhow::Error> {
        self.harness.record_error(child_session_id, error);
        let siblings = self
            .harness
            .sessions()
            .get(parent_session_id)
            .map(|kv| kv.child_sessions.clone())
            .ok_or_else(|| anyhow!("parent session `{parent_session_id}` not registered"))?;
        let mut total = 0usize;
        let mut failed = 0usize;
        for s in &siblings {
            if let Some(kv) = self.harness.sessions().get(s) {
                total += 1;
                if matches!(kv.status, AgentSessionStatus::Failed(_)) {
                    failed += 1;
                }
            }
        }
        Ok(if failed >= total {
            RecoveryAction::AbortParent
        } else if failed * 2 >= total {
            RecoveryAction::EscalateToPlanner
        } else {
            RecoveryAction::Continue
        })
    }

    /// Materialized hierarchy snapshot for UI / metrics surfaces.
    pub fn hierarchy(&self, root_session_id: &str) -> Option<HierarchyNode> {
        let entry = self.harness.sessions().get(root_session_id)?;
        let children: Vec<HierarchyNode> = entry
            .child_sessions
            .iter()
            .filter_map(|c| self.hierarchy(c))
            .collect();
        Some(HierarchyNode {
            session_id: entry.session_id.clone(),
            agent_role: entry.agent_role,
            status: entry.status.clone(),
            iteration_count: entry.iteration_count,
            children,
        })
    }
}

#[derive(Debug, Clone)]
pub struct ChildOutput {
    pub session_id: String,
    pub agent_role: AgentRole,
    pub content: String,
    pub status: AgentSessionStatus,
}

/// One node of the SubAgentManager's hierarchy snapshot.
#[derive(Debug, Clone)]
pub struct HierarchyNode {
    pub session_id: String,
    pub agent_role: AgentRole,
    pub status: AgentSessionStatus,
    pub iteration_count: u32,
    pub children: Vec<HierarchyNode>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecoveryAction {
    /// Keep going — the failing child is a minority, others may still finish.
    Continue,
    /// At least half of the siblings failed; ask the Planner to redesign.
    EscalateToPlanner,
    /// Every sibling failed — bubble up and fail the parent.
    AbortParent,
}

/// Use this from `completion.rs` once a SubtaskPlan has been validated and
/// written into the dynamic graph: pair each subtask id with the harness
/// session id that will track its execution.
#[derive(Debug, Clone)]
pub struct PlannedSubtask {
    pub subtask_id: String,
    pub harness_session_id: String,
    pub agent_role: AgentRole,
}

impl PlannedSubtask {
    pub fn from_definition(def: &SubtaskDefinition, harness_session_id: String) -> Self {
        Self {
            subtask_id: def.id.clone(),
            harness_session_id,
            agent_role: def.agent_role,
        }
    }
}

/// Generate a unique synthetic id (currently unused; kept for future
/// scenarios where two subtasks happen to share an id but need distinct
/// harness sessions).
pub fn new_synthetic_id(prefix: &str) -> String {
    format!("{prefix}-{}", Uuid::new_v4().simple())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::AgentRegistry;
    use crate::router::ModelRouter;
    use std::sync::Arc;

    fn make_manager() -> SubAgentManager {
        let router = Arc::new(ModelRouter::new(
            "http://127.0.0.1:1",
            None,
            None,
            None,
            None,
        ));
        let harness = AgentHarness::new(AgentRegistry::builtin(), router);
        SubAgentManager::new(harness)
    }

    fn subtask(id: &str, role: AgentRole) -> SubtaskDefinition {
        SubtaskDefinition {
            id: id.to_string(),
            description: format!("desc {id}"),
            agent_role: role,
            dependencies: vec![],
            mcp_tools: vec![],
            instructions: format!("inst {id}"),
        }
    }

    #[test]
    fn spawn_from_plan_creates_a_child_per_subtask() {
        let mgr = make_manager();
        let parent = mgr.harness.create_session(AgentRole::Planner, None);
        let plan = SubtaskPlan {
            subtasks: vec![
                subtask("a", AgentRole::Coder),
                subtask("b", AgentRole::Reviewer),
            ],
        };
        let pairs = mgr.spawn_from_plan(&parent, &plan);
        assert_eq!(pairs.len(), 2);
        assert_eq!(pairs[0].0, "a");
        assert_eq!(pairs[1].0, "b");

        let parent_entry = mgr.harness.sessions().get(&parent).unwrap();
        assert_eq!(parent_entry.child_sessions.len(), 2);
    }

    #[test]
    fn classify_child_failure_returns_continue_then_escalate_then_abort() {
        let mgr = make_manager();
        let parent = mgr.harness.create_session(AgentRole::Planner, None);
        // 4 children; we'll fail them one by one to traverse the thresholds.
        let ids: Vec<String> = (0..4)
            .map(|_| mgr.spawn_child(&parent, AgentRole::Coder))
            .collect();

        // First failure: 1/4 → Continue
        let action = mgr
            .classify_child_failure(&parent, &ids[0], "boom")
            .unwrap();
        assert_eq!(action, RecoveryAction::Continue);

        // Second failure: 2/4 → EscalateToPlanner (>= half)
        let action = mgr
            .classify_child_failure(&parent, &ids[1], "boom")
            .unwrap();
        assert_eq!(action, RecoveryAction::EscalateToPlanner);

        // Third failure: 3/4 → still EscalateToPlanner
        let action = mgr
            .classify_child_failure(&parent, &ids[2], "boom")
            .unwrap();
        assert_eq!(action, RecoveryAction::EscalateToPlanner);

        // Fourth failure: 4/4 → AbortParent
        let action = mgr
            .classify_child_failure(&parent, &ids[3], "boom")
            .unwrap();
        assert_eq!(action, RecoveryAction::AbortParent);
    }

    #[test]
    fn hierarchy_walks_children_recursively() {
        let mgr = make_manager();
        let root = mgr.harness.create_session(AgentRole::Planner, None);
        let mid = mgr.spawn_child(&root, AgentRole::Coder);
        let _leaf_a = mgr.spawn_child(&mid, AgentRole::Reviewer);
        let _leaf_b = mgr.spawn_child(&mid, AgentRole::Validator);

        let h = mgr.hierarchy(&root).unwrap();
        assert_eq!(h.children.len(), 1);
        assert_eq!(h.children[0].children.len(), 2);
    }
}
