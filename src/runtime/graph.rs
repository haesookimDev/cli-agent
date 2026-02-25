use std::collections::{HashMap, HashSet};

use anyhow::{anyhow, bail};
use serde::{Deserialize, Serialize};

use crate::types::AgentRole;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeStatus {
    Pending,
    Running,
    Succeeded,
    Failed,
    Skipped,
}

impl NodeStatus {
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            NodeStatus::Succeeded | NodeStatus::Failed | NodeStatus::Skipped
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DependencyFailurePolicy {
    FailFast,
    ContinueOnError,
    FallbackNode,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionPolicy {
    pub max_parallelism: usize,
    pub retry: u8,
    pub timeout_ms: u64,
    pub circuit_breaker: u8,
    pub on_dependency_failure: DependencyFailurePolicy,
    pub fallback_node: Option<String>,
}

impl Default for ExecutionPolicy {
    fn default() -> Self {
        Self {
            max_parallelism: 1,
            retry: 1,
            timeout_ms: 120_000,
            circuit_breaker: 3,
            on_dependency_failure: DependencyFailurePolicy::FailFast,
            fallback_node: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentNode {
    pub id: String,
    pub role: AgentRole,
    pub instructions: String,
    pub dependencies: Vec<String>,
    pub policy: ExecutionPolicy,
    pub depth: u8,
    pub retry_context: Option<String>,
}

impl AgentNode {
    pub fn new(id: impl Into<String>, role: AgentRole, instructions: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            role,
            instructions: instructions.into(),
            dependencies: Vec::new(),
            policy: ExecutionPolicy::default(),
            depth: 0,
            retry_context: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ExecutionGraph {
    nodes: HashMap<String, AgentNode>,
    statuses: HashMap<String, NodeStatus>,
    forced_ready: HashSet<String>,
    /// Node IDs that are referenced as `fallback_node` by another node.
    /// These nodes should only run when explicitly activated via `force_ready`.
    fallback_only: HashSet<String>,
    max_depth: u8,
}

impl ExecutionGraph {
    pub fn new(max_depth: u8) -> Self {
        Self {
            nodes: HashMap::new(),
            statuses: HashMap::new(),
            forced_ready: HashSet::new(),
            fallback_only: HashSet::new(),
            max_depth,
        }
    }

    pub fn with_nodes(max_depth: u8, nodes: Vec<AgentNode>) -> anyhow::Result<Self> {
        let mut graph = Self::new(max_depth);
        graph.add_nodes(nodes)?;
        Ok(graph)
    }

    pub fn add_nodes(&mut self, nodes: Vec<AgentNode>) -> anyhow::Result<()> {
        for node in nodes {
            self.add_node(node)?;
        }
        Ok(())
    }

    pub fn add_node(&mut self, node: AgentNode) -> anyhow::Result<()> {
        if self.nodes.contains_key(&node.id) {
            bail!("duplicate node id {}", node.id);
        }
        if node.depth > self.max_depth {
            bail!(
                "node {} depth {} exceeds max depth {}",
                node.id,
                node.depth,
                self.max_depth
            );
        }

        for dep in &node.dependencies {
            if dep == &node.id {
                bail!("node {} cannot depend on itself", node.id);
            }
            if !self.nodes.contains_key(dep) {
                bail!(
                    "node {} dependency {} does not exist in graph",
                    node.id,
                    dep
                );
            }
        }

        // Track if this node is a fallback target for any existing node.
        if let Some(fb) = &node.policy.fallback_node {
            self.fallback_only.insert(fb.clone());
        }
        // Check if any existing node references this new node as its fallback.
        for existing in self.nodes.values() {
            if existing.policy.fallback_node.as_deref() == Some(&node.id) {
                self.fallback_only.insert(node.id.clone());
            }
        }

        self.nodes.insert(node.id.clone(), node.clone());
        self.statuses.insert(node.id.clone(), NodeStatus::Pending);

        if self.has_cycle() {
            self.nodes.remove(&node.id);
            self.statuses.remove(&node.id);
            return Err(anyhow!("adding node {} introduces cycle", node.id));
        }

        Ok(())
    }

    pub fn force_ready(&mut self, node_id: &str) {
        if self.nodes.contains_key(node_id) {
            self.forced_ready.insert(node_id.to_string());
        }
    }

    pub fn is_forced_ready(&self, node_id: &str) -> bool {
        self.forced_ready.contains(node_id)
    }

    pub fn clear_forced_ready(&mut self, node_id: &str) {
        self.forced_ready.remove(node_id);
    }

    /// Returns true if this node is only meant to run as a fallback (i.e. when
    /// the parent node that references it has failed and called `force_ready`).
    pub fn is_fallback_only(&self, node_id: &str) -> bool {
        self.fallback_only.contains(node_id)
    }

    pub fn nodes(&self) -> Vec<AgentNode> {
        self.nodes.values().cloned().collect()
    }

    pub fn node(&self, node_id: &str) -> Option<&AgentNode> {
        self.nodes.get(node_id)
    }

    pub fn status(&self, node_id: &str) -> Option<NodeStatus> {
        self.statuses.get(node_id).copied()
    }

    pub fn set_status(&mut self, node_id: &str, status: NodeStatus) {
        if let Some(entry) = self.statuses.get_mut(node_id) {
            *entry = status;
        }
    }

    pub fn statuses(&self) -> HashMap<String, NodeStatus> {
        self.statuses.clone()
    }

    pub fn pending_nodes(&self) -> Vec<AgentNode> {
        self.nodes
            .values()
            .filter(|node| matches!(self.status(node.id.as_str()), Some(NodeStatus::Pending)))
            .cloned()
            .collect()
    }

    pub fn all_terminal(&self) -> bool {
        self.statuses.values().all(NodeStatus::is_terminal)
    }

    pub fn dependencies(&self, node: &AgentNode) -> Vec<(String, NodeStatus)> {
        node.dependencies
            .iter()
            .filter_map(|dep| self.status(dep).map(|status| (dep.clone(), status)))
            .collect()
    }

    pub fn mark_role_pending_as_skipped(&mut self, role: AgentRole) {
        let targets = self
            .nodes
            .values()
            .filter(|node| {
                node.role == role
                    && matches!(self.status(node.id.as_str()), Some(NodeStatus::Pending))
            })
            .map(|node| node.id.clone())
            .collect::<Vec<_>>();

        for node_id in targets {
            self.set_status(node_id.as_str(), NodeStatus::Skipped);
        }
    }

    fn has_cycle(&self) -> bool {
        fn dfs(
            node_id: &str,
            graph: &ExecutionGraph,
            temp: &mut HashSet<String>,
            perm: &mut HashSet<String>,
        ) -> bool {
            if perm.contains(node_id) {
                return false;
            }
            if temp.contains(node_id) {
                return true;
            }

            temp.insert(node_id.to_string());
            if let Some(node) = graph.nodes.get(node_id) {
                for dep in &node.dependencies {
                    if dfs(dep, graph, temp, perm) {
                        return true;
                    }
                }
            }
            temp.remove(node_id);
            perm.insert(node_id.to_string());
            false
        }

        let mut temp = HashSet::new();
        let mut perm = HashSet::new();
        for node_id in self.nodes.keys() {
            if dfs(node_id, self, &mut temp, &mut perm) {
                return true;
            }
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::AgentRole;

    #[test]
    fn graph_rejects_cycles() {
        let mut graph = ExecutionGraph::new(4);

        let a = AgentNode::new("a", AgentRole::Planner, "A");
        graph.add_node(a).unwrap();

        let mut b = AgentNode::new("b", AgentRole::Coder, "B");
        b.dependencies = vec!["a".to_string()];
        graph.add_node(b).unwrap();

        let mut c = AgentNode::new("c", AgentRole::Summarizer, "C");
        c.dependencies = vec!["b".to_string()];
        graph.add_node(c).unwrap();

        let mut bad = AgentNode::new("bad", AgentRole::Fallback, "BAD");
        bad.dependencies = vec!["bad".to_string()];
        assert!(graph.add_node(bad).is_err());
    }
}
