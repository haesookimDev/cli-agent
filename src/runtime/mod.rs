pub mod graph;

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use futures::future::BoxFuture;
use tokio::task::JoinSet;
use tokio::time::timeout;
use tracing::{debug, warn};

use crate::runtime::graph::{AgentNode, DependencyFailurePolicy, ExecutionGraph, NodeStatus};
use crate::types::AgentRole;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct NodeExecutionResult {
    pub node_id: String,
    pub role: AgentRole,
    pub model: String,
    pub output: String,
    pub duration_ms: u128,
    pub succeeded: bool,
    pub error: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeEvent {
    NodeStarted {
        node_id: String,
        role: AgentRole,
    },
    NodeCompleted {
        node_id: String,
        role: AgentRole,
    },
    NodeFailed {
        node_id: String,
        role: AgentRole,
        error: String,
    },
    NodeSkipped {
        node_id: String,
        reason: String,
    },
    DynamicNodeAdded {
        node_id: String,
        from_node: String,
    },
    GraphCompleted,
}

pub type RunNodeFn = Arc<
    dyn Fn(
            AgentNode,
            Vec<NodeExecutionResult>,
        ) -> BoxFuture<'static, anyhow::Result<NodeExecutionResult>>
        + Send
        + Sync,
>;

pub type OnNodeCompletedFn = Arc<
    dyn Fn(AgentNode, NodeExecutionResult) -> BoxFuture<'static, anyhow::Result<Vec<AgentNode>>>
        + Send
        + Sync,
>;

pub type EventSink = Arc<dyn Fn(RuntimeEvent) + Send + Sync>;

#[derive(Debug, Clone)]
pub struct AgentRuntime {
    global_max_parallelism: usize,
}

impl AgentRuntime {
    pub fn new(global_max_parallelism: usize) -> Self {
        Self {
            global_max_parallelism: global_max_parallelism.max(1),
        }
    }

    pub async fn execute_graph(
        &self,
        mut graph: ExecutionGraph,
        run_node: RunNodeFn,
        on_completed: OnNodeCompletedFn,
        on_event: Option<EventSink>,
    ) -> anyhow::Result<Vec<NodeExecutionResult>> {
        let mut outputs: HashMap<String, NodeExecutionResult> = HashMap::new();
        let mut running: HashSet<String> = HashSet::new();
        let mut running_by_role: HashMap<AgentRole, usize> = HashMap::new();
        let mut role_failures: HashMap<AgentRole, u8> = HashMap::new();
        let mut join_set: JoinSet<(AgentNode, anyhow::Result<NodeExecutionResult>)> =
            JoinSet::new();

        loop {
            let mut launched_any = false;

            while running.len() < self.global_max_parallelism {
                let next = self.select_next_node(&mut graph, &running, &running_by_role)?;
                let Some(node) = next else {
                    break;
                };

                graph.set_status(node.id.as_str(), NodeStatus::Running);
                running.insert(node.id.clone());
                *running_by_role.entry(node.role).or_insert(0) += 1;

                if let Some(sink) = &on_event {
                    sink(RuntimeEvent::NodeStarted {
                        node_id: node.id.clone(),
                        role: node.role,
                    });
                }

                let dep_outputs: Vec<NodeExecutionResult> = node
                    .dependencies
                    .iter()
                    .filter_map(|dep| outputs.get(dep).cloned())
                    .collect();

                let run_node_fn = run_node.clone();
                let node_clone = node.clone();
                join_set.spawn(async move {
                    let result =
                        execute_node_with_policy(node_clone.clone(), dep_outputs, run_node_fn)
                            .await;
                    (node_clone, result)
                });
                launched_any = true;
            }

            if running.is_empty() && !launched_any {
                if graph.all_terminal() {
                    break;
                }

                let progressed = resolve_blocked_nodes(&mut graph, &on_event);
                if !progressed {
                    return Err(anyhow::anyhow!(
                        "graph reached deadlock: unresolved dependencies or inconsistent state"
                    ));
                }
                continue;
            }

            if let Some(joined) = join_set.join_next().await {
                let (node, result) = joined.map_err(anyhow::Error::from)?;
                running.remove(node.id.as_str());
                if let Some(count) = running_by_role.get_mut(&node.role) {
                    *count = count.saturating_sub(1);
                }

                match result {
                    Ok(ok) if ok.succeeded => {
                        graph.set_status(node.id.as_str(), NodeStatus::Succeeded);
                        outputs.insert(node.id.clone(), ok.clone());

                        if let Some(sink) = &on_event {
                            sink(RuntimeEvent::NodeCompleted {
                                node_id: node.id.clone(),
                                role: node.role,
                            });
                        }

                        let dynamic_nodes = on_completed(node.clone(), ok.clone()).await?;
                        for dynamic in dynamic_nodes {
                            let dynamic_id = dynamic.id.clone();
                            graph.add_node(dynamic)?;
                            if let Some(sink) = &on_event {
                                sink(RuntimeEvent::DynamicNodeAdded {
                                    node_id: dynamic_id,
                                    from_node: node.id.clone(),
                                });
                            }
                        }
                    }
                    Ok(failed) => {
                        graph.set_status(node.id.as_str(), NodeStatus::Failed);
                        outputs.insert(node.id.clone(), failed.clone());

                        if let Some(err) = &failed.error {
                            if let Some(sink) = &on_event {
                                sink(RuntimeEvent::NodeFailed {
                                    node_id: node.id.clone(),
                                    role: node.role,
                                    error: err.clone(),
                                });
                            }
                        }

                        if let Some(fallback_id) = &node.policy.fallback_node {
                            debug!("node {} failed, forcing fallback {}", node.id, fallback_id);
                            graph.force_ready(fallback_id);
                        }

                        let entry = role_failures.entry(node.role).or_insert(0);
                        *entry = entry.saturating_add(1);
                        if node.policy.circuit_breaker > 0 && *entry >= node.policy.circuit_breaker
                        {
                            warn!("circuit breaker opened for role {}", node.role);
                            graph.mark_role_pending_as_skipped(node.role);
                        }
                    }
                    Err(err) => {
                        graph.set_status(node.id.as_str(), NodeStatus::Failed);
                        let failed = NodeExecutionResult {
                            node_id: node.id.clone(),
                            role: node.role,
                            model: "unknown".to_string(),
                            output: String::new(),
                            duration_ms: 0,
                            succeeded: false,
                            error: Some(err.to_string()),
                        };
                        outputs.insert(node.id.clone(), failed.clone());

                        if let Some(sink) = &on_event {
                            sink(RuntimeEvent::NodeFailed {
                                node_id: node.id.clone(),
                                role: node.role,
                                error: err.to_string(),
                            });
                        }

                        if let Some(fallback_id) = &node.policy.fallback_node {
                            graph.force_ready(fallback_id);
                        }
                    }
                }
            }
        }

        if let Some(sink) = &on_event {
            sink(RuntimeEvent::GraphCompleted);
        }

        let mut all = outputs.into_values().collect::<Vec<_>>();
        all.sort_by(|a, b| a.node_id.cmp(&b.node_id));
        Ok(all)
    }

    fn select_next_node(
        &self,
        graph: &mut ExecutionGraph,
        running: &HashSet<String>,
        running_by_role: &HashMap<AgentRole, usize>,
    ) -> anyhow::Result<Option<AgentNode>> {
        let mut pending = graph.pending_nodes();
        pending.sort_by(|a, b| a.id.cmp(&b.id));

        for node in pending {
            if running.contains(node.id.as_str()) {
                continue;
            }

            let role_limit = node.policy.max_parallelism.max(1);
            let role_running = running_by_role.get(&node.role).copied().unwrap_or(0);
            if role_running >= role_limit {
                continue;
            }

            match is_runnable(graph, &node) {
                DepResolution::Ready => {
                    graph.clear_forced_ready(node.id.as_str());
                    return Ok(Some(node));
                }
                DepResolution::Blocked => continue,
                DepResolution::FailFast(reason) => {
                    graph.set_status(node.id.as_str(), NodeStatus::Skipped);
                    return Err(anyhow::anyhow!(
                        "node {} skipped due to fail-fast dependency policy: {}",
                        node.id,
                        reason
                    ));
                }
                DepResolution::Fallback(reason) => {
                    graph.set_status(node.id.as_str(), NodeStatus::Skipped);
                    if let Some(fallback) = &node.policy.fallback_node {
                        graph.force_ready(fallback);
                    }
                    debug!("node {} skipped: {}", node.id, reason);
                }
            }
        }

        Ok(None)
    }
}

#[derive(Debug)]
enum DepResolution {
    Ready,
    Blocked,
    FailFast(String),
    Fallback(String),
}

fn is_runnable(graph: &ExecutionGraph, node: &AgentNode) -> DepResolution {
    if graph.is_forced_ready(node.id.as_str()) {
        return DepResolution::Ready;
    }

    let deps = graph.dependencies(node);
    for (dep_id, dep_state) in deps {
        match dep_state {
            NodeStatus::Succeeded => {}
            NodeStatus::Pending | NodeStatus::Running => return DepResolution::Blocked,
            NodeStatus::Failed | NodeStatus::Skipped => match node.policy.on_dependency_failure {
                DependencyFailurePolicy::ContinueOnError => {}
                DependencyFailurePolicy::FailFast => {
                    return DepResolution::FailFast(format!(
                        "dependency {dep_id} state is {dep_state:?}"
                    ));
                }
                DependencyFailurePolicy::FallbackNode => {
                    return DepResolution::Fallback(format!(
                        "dependency {dep_id} state is {dep_state:?}"
                    ));
                }
            },
        }
    }

    DepResolution::Ready
}

fn resolve_blocked_nodes(graph: &mut ExecutionGraph, on_event: &Option<EventSink>) -> bool {
    let mut progressed = false;
    let pending = graph.pending_nodes();

    for node in pending {
        let deps = graph.dependencies(&node);
        let has_unresolved = deps.iter().any(|(_, s)| !s.is_terminal());
        if has_unresolved {
            continue;
        }

        let has_failed_deps = deps
            .iter()
            .any(|(_, s)| matches!(s, NodeStatus::Failed | NodeStatus::Skipped));
        if !has_failed_deps {
            continue;
        }

        match node.policy.on_dependency_failure {
            DependencyFailurePolicy::ContinueOnError => {
                graph.force_ready(node.id.as_str());
                progressed = true;
            }
            DependencyFailurePolicy::FailFast => {
                graph.set_status(node.id.as_str(), NodeStatus::Skipped);
                progressed = true;
                if let Some(sink) = on_event {
                    sink(RuntimeEvent::NodeSkipped {
                        node_id: node.id.clone(),
                        reason: "dependency failed (fail-fast)".to_string(),
                    });
                }
            }
            DependencyFailurePolicy::FallbackNode => {
                graph.set_status(node.id.as_str(), NodeStatus::Skipped);
                progressed = true;
                if let Some(fallback_id) = &node.policy.fallback_node {
                    graph.force_ready(fallback_id);
                }
                if let Some(sink) = on_event {
                    sink(RuntimeEvent::NodeSkipped {
                        node_id: node.id.clone(),
                        reason: "dependency failed (fallback-node)".to_string(),
                    });
                }
            }
        }
    }

    progressed
}

async fn execute_node_with_policy(
    node: AgentNode,
    deps: Vec<NodeExecutionResult>,
    run_node: RunNodeFn,
) -> anyhow::Result<NodeExecutionResult> {
    let mut last_error: Option<anyhow::Error> = None;

    for _attempt in 0..=node.policy.retry {
        let fut = (run_node)(node.clone(), deps.clone());
        let result = timeout(Duration::from_millis(node.policy.timeout_ms), fut).await;

        match result {
            Ok(Ok(output)) => {
                if output.succeeded {
                    return Ok(output);
                }
                last_error =
                    Some(anyhow::anyhow!(output.error.clone().unwrap_or_else(|| {
                        "node execution reported failure".to_string()
                    })));
            }
            Ok(Err(err)) => last_error = Some(err),
            Err(_) => {
                last_error = Some(anyhow::anyhow!(
                    "node {} timed out after {}ms",
                    node.id,
                    node.policy.timeout_ms
                ));
            }
        }
    }

    Err(last_error.unwrap_or_else(|| anyhow::anyhow!("node execution failed")))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Instant;

    use futures::FutureExt;

    use super::*;
    use crate::runtime::graph::{
        AgentNode, DependencyFailurePolicy, ExecutionGraph, ExecutionPolicy,
    };
    use crate::types::AgentRole;

    #[tokio::test]
    async fn executes_parallel_dependencies() {
        let runtime = AgentRuntime::new(4);
        let mut graph = ExecutionGraph::new(4);

        let mut a = AgentNode::new("a", AgentRole::Planner, "a");
        a.policy = ExecutionPolicy {
            max_parallelism: 2,
            ..ExecutionPolicy::default()
        };

        let mut b = AgentNode::new("b", AgentRole::Extractor, "b");
        b.policy = ExecutionPolicy {
            max_parallelism: 2,
            ..ExecutionPolicy::default()
        };

        let mut c = AgentNode::new("c", AgentRole::Summarizer, "c");
        c.dependencies = vec!["a".to_string(), "b".to_string()];

        graph.add_node(a).unwrap();
        graph.add_node(b).unwrap();
        graph.add_node(c).unwrap();

        let started = Arc::new(AtomicUsize::new(0));
        let run_node: RunNodeFn = {
            let started = started.clone();
            Arc::new(move |node, _deps| {
                let started = started.clone();
                async move {
                    started.fetch_add(1, Ordering::SeqCst);
                    tokio::time::sleep(Duration::from_millis(if node.id == "c" { 5 } else { 60 }))
                        .await;
                    Ok(NodeExecutionResult {
                        node_id: node.id,
                        role: node.role,
                        model: "mock".to_string(),
                        output: "ok".to_string(),
                        duration_ms: 1,
                        succeeded: true,
                        error: None,
                    })
                }
                .boxed()
            })
        };

        let on_completed: OnNodeCompletedFn =
            Arc::new(move |_node, _res| async { Ok(vec![]) }.boxed());

        let now = Instant::now();
        let results = runtime
            .execute_graph(graph, run_node, on_completed, None)
            .await
            .unwrap();

        assert_eq!(results.len(), 3);
        assert!(now.elapsed() < Duration::from_millis(160));
        assert_eq!(started.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn fallback_node_is_triggered() {
        let runtime = AgentRuntime::new(2);
        let mut graph = ExecutionGraph::new(4);

        let mut primary = AgentNode::new("primary", AgentRole::Coder, "primary");
        primary.policy = ExecutionPolicy {
            fallback_node: Some("fallback".to_string()),
            ..ExecutionPolicy::default()
        };

        let mut fallback = AgentNode::new("fallback", AgentRole::Fallback, "fallback");
        fallback.dependencies = vec!["primary".to_string()];
        fallback.policy = ExecutionPolicy {
            on_dependency_failure: DependencyFailurePolicy::ContinueOnError,
            ..ExecutionPolicy::default()
        };

        graph.add_node(primary).unwrap();
        graph.add_node(fallback).unwrap();

        let run_node: RunNodeFn = Arc::new(move |node, _deps| {
            async move {
                if node.id == "primary" {
                    Ok(NodeExecutionResult {
                        node_id: node.id,
                        role: node.role,
                        model: "mock".to_string(),
                        output: String::new(),
                        duration_ms: 0,
                        succeeded: false,
                        error: Some("boom".to_string()),
                    })
                } else {
                    Ok(NodeExecutionResult {
                        node_id: node.id,
                        role: node.role,
                        model: "mock".to_string(),
                        output: "fallback-ok".to_string(),
                        duration_ms: 0,
                        succeeded: true,
                        error: None,
                    })
                }
            }
            .boxed()
        });

        let on_completed: OnNodeCompletedFn =
            Arc::new(move |_node, _res| async { Ok(vec![]) }.boxed());

        let results = runtime
            .execute_graph(graph, run_node, on_completed, None)
            .await
            .unwrap();

        let fallback_result = results.iter().find(|r| r.node_id == "fallback");
        assert!(fallback_result.is_some());
        assert!(fallback_result.unwrap().succeeded);
    }
}
