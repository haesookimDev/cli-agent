use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::Utc;
use dashmap::DashMap;
use futures::FutureExt;
use tokio::time::sleep;
use tracing::{error, info};
use uuid::Uuid;

use crate::agents::{AgentInput, AgentRegistry};
use crate::context::{ContextChunk, ContextKind, ContextManager, ContextScope};
use crate::memory::MemoryManager;
use crate::router::{ModelRouter, RoutingConstraints};
use crate::runtime::graph::{
    AgentNode, DependencyFailurePolicy, ExecutionGraph, ExecutionPolicy,
};
use crate::runtime::{AgentRuntime, EventSink, NodeExecutionResult, OnNodeCompletedFn, RunNodeFn, RuntimeEvent};
use crate::types::{
    AgentExecutionRecord, AgentRole, RunRecord, RunRequest, RunStatus, RunSubmission, SessionEvent,
    SessionEventType,
};
use crate::webhook::WebhookDispatcher;

#[derive(Clone)]
pub struct Orchestrator {
    runtime: AgentRuntime,
    agents: AgentRegistry,
    router: Arc<ModelRouter>,
    memory: Arc<MemoryManager>,
    context: Arc<ContextManager>,
    webhook: Arc<WebhookDispatcher>,
    runs: Arc<DashMap<Uuid, RunRecord>>,
    max_graph_depth: u8,
}

impl Orchestrator {
    pub fn new(
        runtime: AgentRuntime,
        agents: AgentRegistry,
        router: Arc<ModelRouter>,
        memory: Arc<MemoryManager>,
        context: Arc<ContextManager>,
        webhook: Arc<WebhookDispatcher>,
        max_graph_depth: u8,
    ) -> Self {
        Self {
            runtime,
            agents,
            router,
            memory,
            context,
            webhook,
            runs: Arc::new(DashMap::new()),
            max_graph_depth,
        }
    }

    pub async fn submit_run(&self, req: RunRequest) -> anyhow::Result<RunSubmission> {
        let run_id = Uuid::new_v4();
        let session_id = req.session_id.unwrap_or_else(Uuid::new_v4);

        self.memory.create_session(session_id).await?;

        let mut record = RunRecord::new_queued(run_id, session_id, req.task.clone(), req.profile);
        record
            .timeline
            .push(format!("{} run queued", Utc::now().to_rfc3339()));

        self.runs.insert(run_id, record.clone());
        self.memory.upsert_run(&record).await?;

        let this = self.clone();
        tokio::spawn(async move {
            if let Err(err) = this.execute_run(run_id, req).await {
                error!("run {run_id} execution crashed: {err}");
                let _ = this.mark_run_failed(run_id, err.to_string()).await;
            }
        });

        Ok(RunSubmission {
            run_id,
            session_id,
            status: RunStatus::Queued,
        })
    }

    pub async fn run_and_wait(
        &self,
        req: RunRequest,
        poll_interval: Duration,
    ) -> anyhow::Result<RunRecord> {
        let submission = self.submit_run(req).await?;

        loop {
            let maybe_run = self.get_run(submission.run_id).await?;
            if let Some(run) = maybe_run {
                if run.status.is_terminal() {
                    return Ok(run);
                }
            }
            sleep(poll_interval).await;
        }
    }

    pub async fn get_run(&self, run_id: Uuid) -> anyhow::Result<Option<RunRecord>> {
        if let Some(record) = self.runs.get(&run_id) {
            return Ok(Some(record.clone()));
        }
        self.memory.get_run(run_id).await
    }

    pub async fn list_recent_runs(&self, limit: usize) -> anyhow::Result<Vec<RunRecord>> {
        let mut runs = self.memory.list_recent_runs(limit).await?;
        for kv in self.runs.iter() {
            let run = kv.value().clone();
            if !runs.iter().any(|r| r.run_id == run.run_id) {
                runs.push(run);
            }
        }

        runs.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        runs.truncate(limit);
        Ok(runs)
    }

    pub async fn create_session(&self, session_id: Uuid) -> anyhow::Result<()> {
        self.memory.create_session(session_id).await
    }

    pub async fn replay_session(
        &self,
        session_id: Uuid,
    ) -> anyhow::Result<Vec<crate::types::ReplayEvent>> {
        self.memory.replay(session_id).await
    }

    pub async fn compact_session(&self, session_id: Uuid) -> anyhow::Result<()> {
        self.memory.compact_session(session_id).await
    }

    pub async fn vacuum_memory(&self) -> anyhow::Result<()> {
        self.memory.vacuum().await
    }

    pub async fn register_webhook(
        &self,
        url: &str,
        events: &[String],
        secret: &str,
    ) -> anyhow::Result<crate::types::WebhookEndpoint> {
        self.memory.register_webhook(url, events, secret).await
    }

    pub async fn dispatch_webhook_event(
        &self,
        event: &str,
        payload: serde_json::Value,
    ) -> anyhow::Result<()> {
        self.webhook.dispatch(event, payload).await
    }

    async fn execute_run(&self, run_id: Uuid, req: RunRequest) -> anyhow::Result<()> {
        let session_id = req.session_id.unwrap_or_else(Uuid::new_v4);
        self.set_running(run_id).await?;

        self.memory
            .append_event(SessionEvent {
                session_id,
                run_id: Some(run_id),
                event_type: SessionEventType::UserMessage,
                timestamp: Utc::now(),
                payload: serde_json::json!({ "text": req.task }),
            })
            .await?;

        self.webhook
            .dispatch(
                "run.started",
                serde_json::json!({
                    "run_id": run_id,
                    "session_id": session_id,
                    "profile": req.profile,
                }),
            )
            .await?;

        let graph = self.build_graph(req.task.as_str())?;

        let event_sink = self.build_event_sink(run_id, session_id);
        let run_node = self.build_run_node_fn(run_id, session_id, req.clone());
        let on_complete = self.build_on_completed_fn(run_id, req.task.clone());

        let outputs = self
            .runtime
            .execute_graph(graph, run_node, on_complete, Some(event_sink))
            .await;

        match outputs {
            Ok(node_results) => {
                let final_status = if node_results.iter().all(|r| r.succeeded) {
                    RunStatus::Succeeded
                } else {
                    RunStatus::Failed
                };

                self.finish_run(run_id, final_status, node_results, None).await?;
            }
            Err(err) => {
                self.finish_run(run_id, RunStatus::Failed, vec![], Some(err.to_string()))
                    .await?;
            }
        }

        Ok(())
    }

    fn build_graph(&self, task: &str) -> anyhow::Result<ExecutionGraph> {
        let mut graph = ExecutionGraph::new(self.max_graph_depth);

        let mut plan = AgentNode::new("plan", AgentRole::Planner, "Plan work and constraints.");
        plan.policy = ExecutionPolicy {
            max_parallelism: 1,
            retry: 1,
            timeout_ms: 30_000,
            circuit_breaker: 3,
            on_dependency_failure: DependencyFailurePolicy::FailFast,
            fallback_node: None,
        };

        let mut extract = AgentNode::new(
            "extract",
            AgentRole::Extractor,
            "Extract structured facts from user task and planner output.",
        );
        extract.dependencies = vec!["plan".to_string()];
        extract.policy = ExecutionPolicy {
            max_parallelism: 2,
            retry: 1,
            timeout_ms: 20_000,
            circuit_breaker: 3,
            on_dependency_failure: DependencyFailurePolicy::ContinueOnError,
            fallback_node: None,
        };

        let mut context_probe = AgentNode::new(
            "context_probe",
            AgentRole::Extractor,
            "Probe memory and context windows for high-value retrieval candidates.",
        );
        context_probe.dependencies = vec!["plan".to_string()];
        context_probe.policy = ExecutionPolicy {
            max_parallelism: 2,
            retry: 1,
            timeout_ms: 15_000,
            circuit_breaker: 3,
            on_dependency_failure: DependencyFailurePolicy::ContinueOnError,
            fallback_node: None,
        };

        let mut code = AgentNode::new(
            "code",
            AgentRole::Coder,
            "Generate implementation-level output from extracted and contextualized plan.",
        );
        code.dependencies = vec!["extract".to_string(), "context_probe".to_string()];
        code.policy = ExecutionPolicy {
            max_parallelism: 2,
            retry: 2,
            timeout_ms: 60_000,
            circuit_breaker: 2,
            on_dependency_failure: DependencyFailurePolicy::FailFast,
            fallback_node: Some("fallback_code".to_string()),
        };

        let mut fallback_code = AgentNode::new(
            "fallback_code",
            AgentRole::Fallback,
            "Recover from coding node failures using robust conservative strategy.",
        );
        fallback_code.dependencies = vec!["code".to_string()];
        fallback_code.policy = ExecutionPolicy {
            max_parallelism: 1,
            retry: 1,
            timeout_ms: 25_000,
            circuit_breaker: 3,
            on_dependency_failure: DependencyFailurePolicy::ContinueOnError,
            fallback_node: None,
        };

        let mut summarize = AgentNode::new(
            "summarize",
            AgentRole::Summarizer,
            "Summarize outcomes and produce checkpoint summary for context compaction.",
        );
        summarize.dependencies = vec!["code".to_string(), "fallback_code".to_string()];
        summarize.policy = ExecutionPolicy {
            max_parallelism: 1,
            retry: 1,
            timeout_ms: 20_000,
            circuit_breaker: 3,
            on_dependency_failure: DependencyFailurePolicy::ContinueOnError,
            fallback_node: None,
        };

        graph.add_node(plan)?;
        graph.add_node(extract)?;
        graph.add_node(context_probe)?;
        graph.add_node(code)?;
        graph.add_node(fallback_code)?;
        graph.add_node(summarize)?;

        if task.to_lowercase().contains("webhook") {
            let mut webhook = AgentNode::new(
                "webhook_validation",
                AgentRole::Extractor,
                "Validate webhook contract and integration constraints.",
            );
            webhook.dependencies = vec!["plan".to_string()];
            webhook.depth = 1;
            webhook.policy = ExecutionPolicy {
                max_parallelism: 1,
                retry: 1,
                timeout_ms: 12_000,
                circuit_breaker: 3,
                on_dependency_failure: DependencyFailurePolicy::ContinueOnError,
                fallback_node: None,
            };
            graph.add_node(webhook)?;
        }

        Ok(graph)
    }

    fn build_run_node_fn(&self, run_id: Uuid, session_id: Uuid, req: RunRequest) -> RunNodeFn {
        let agents = self.agents.clone();
        let router = self.router.clone();
        let memory = self.memory.clone();
        let context = self.context.clone();

        Arc::new(move |node: AgentNode, deps: Vec<NodeExecutionResult>| {
            let agents = agents.clone();
            let router = router.clone();
            let memory = memory.clone();
            let context = context.clone();
            let req = req.clone();

            async move {
                let started = Instant::now();

                let dep_outputs = deps
                    .iter()
                    .map(|d| format!("{}: {}", d.node_id, d.output))
                    .collect::<Vec<_>>();

                let memory_hits = memory
                    .retrieve(session_id, req.task.as_str(), 8)
                    .await
                    .unwrap_or_default();

                let mut chunks = vec![
                    ContextChunk {
                        id: "sys-1".to_string(),
                        scope: ContextScope::GlobalShared,
                        kind: ContextKind::System,
                        content: "You are one node in a multi-agent orchestrated DAG. Keep outputs machine-friendly.".to_string(),
                        priority: 1.0,
                    },
                    ContextChunk {
                        id: format!("inst-{}", node.id),
                        scope: ContextScope::AgentPrivate,
                        kind: ContextKind::Instructions,
                        content: node.instructions.clone(),
                        priority: 0.95,
                    },
                ];

                for (idx, output) in dep_outputs.iter().enumerate() {
                    chunks.push(ContextChunk {
                        id: format!("dep-{idx}"),
                        scope: ContextScope::SessionShared,
                        kind: ContextKind::History,
                        content: output.clone(),
                        priority: 0.8,
                    });
                }

                for hit in memory_hits {
                    chunks.push(ContextChunk {
                        id: hit.id,
                        scope: ContextScope::SessionShared,
                        kind: ContextKind::Retrieval,
                        content: hit.content,
                        priority: hit.score.clamp(0.2, 1.0),
                    });
                }

                let optimized = context.optimize(chunks);
                let brief = context.build_structured_brief(
                    req.task.clone(),
                    vec![
                        "respect dependency graph".to_string(),
                        "respect token budget".to_string(),
                        "prefer deterministic output".to_string(),
                    ],
                    vec![
                        format!("task_profile={}", req.profile),
                        format!("node={}", node.id),
                        format!("run_id={run_id}"),
                    ],
                    vec![format!("session_id={session_id}"), format!("run_id={run_id}")],
                );

                let input = AgentInput {
                    task: req.task.clone(),
                    instructions: node.instructions,
                    context: optimized,
                    dependency_outputs: dep_outputs,
                    brief,
                };

                let run = agents.run_role(node.role, input, router.clone()).await;
                match run {
                    Ok(output) => {
                        memory
                            .remember_short(
                                session_id,
                                output.content.clone(),
                                0.6,
                                Duration::from_secs(20 * 60),
                            )
                            .await;

                        let _ = memory
                            .remember_long(
                                session_id,
                                "agent_output",
                                output.content.as_str(),
                                0.65,
                                Some(node.id.as_str()),
                            )
                            .await;

                        Ok(NodeExecutionResult {
                            node_id: node.id,
                            role: node.role,
                            model: output.model,
                            output: output.content,
                            duration_ms: started.elapsed().as_millis(),
                            succeeded: true,
                            error: None,
                        })
                    }
                    Err(err) => Ok(NodeExecutionResult {
                        node_id: node.id,
                        role: node.role,
                        model: "unavailable".to_string(),
                        output: String::new(),
                        duration_ms: started.elapsed().as_millis(),
                        succeeded: false,
                        error: Some(err.to_string()),
                    }),
                }
            }
            .boxed()
        })
    }

    fn build_on_completed_fn(&self, run_id: Uuid, task: String) -> OnNodeCompletedFn {
        Arc::new(move |node: AgentNode, result: NodeExecutionResult| {
            let task = task.clone();
            async move {
                let mut dynamic_nodes = Vec::new();

                if node.role == AgentRole::Planner
                    && result.succeeded
                    && task.split_whitespace().count() > 20
                    && node.depth < 5
                {
                    let mut dynamic = AgentNode::new(
                        format!("dynamic_checkpoint_{run_id}"),
                        AgentRole::Summarizer,
                        "Create checkpoint summary to reduce context pressure.",
                    );
                    dynamic.dependencies = vec![node.id];
                    dynamic.depth = node.depth + 1;
                    dynamic.policy = ExecutionPolicy {
                        max_parallelism: 1,
                        retry: 1,
                        timeout_ms: 12_000,
                        circuit_breaker: 2,
                        on_dependency_failure: DependencyFailurePolicy::ContinueOnError,
                        fallback_node: None,
                    };
                    dynamic_nodes.push(dynamic);
                }

                Ok(dynamic_nodes)
            }
            .boxed()
        })
    }

    fn build_event_sink(&self, run_id: Uuid, session_id: Uuid) -> EventSink {
        let runs = self.runs.clone();
        let memory = self.memory.clone();
        let webhook = self.webhook.clone();

        Arc::new(move |event: RuntimeEvent| {
            if let Some(mut entry) = runs.get_mut(&run_id) {
                entry
                    .timeline
                    .push(format!("{} {:?}", Utc::now().to_rfc3339(), event));
            }

            let memory = memory.clone();
            let webhook = webhook.clone();
            let event_clone = event.clone();
            tokio::spawn(async move {
                let payload = match &event_clone {
                    RuntimeEvent::NodeStarted { node_id, role } => serde_json::json!({
                        "node_id": node_id,
                        "role": role,
                        "phase": "started"
                    }),
                    RuntimeEvent::NodeCompleted { node_id, role } => serde_json::json!({
                        "node_id": node_id,
                        "role": role,
                        "phase": "completed"
                    }),
                    RuntimeEvent::NodeFailed {
                        node_id,
                        role,
                        error,
                    } => serde_json::json!({
                        "node_id": node_id,
                        "role": role,
                        "phase": "failed",
                        "error": error
                    }),
                    RuntimeEvent::NodeSkipped { node_id, reason } => serde_json::json!({
                        "node_id": node_id,
                        "phase": "skipped",
                        "reason": reason
                    }),
                    RuntimeEvent::DynamicNodeAdded { node_id, from_node } => serde_json::json!({
                        "node_id": node_id,
                        "phase": "dynamic_added",
                        "from": from_node
                    }),
                    RuntimeEvent::GraphCompleted => serde_json::json!({ "phase": "graph_completed" }),
                };

                let event_type = match event_clone {
                    RuntimeEvent::NodeStarted { .. } => SessionEventType::AgentStarted,
                    RuntimeEvent::NodeCompleted { .. } => SessionEventType::AgentOutput,
                    RuntimeEvent::NodeFailed { .. } => SessionEventType::RunFailed,
                    RuntimeEvent::NodeSkipped { .. }
                    | RuntimeEvent::DynamicNodeAdded { .. }
                    | RuntimeEvent::GraphCompleted => SessionEventType::RunProgress,
                };

                let _ = memory
                    .append_event(SessionEvent {
                        session_id,
                        run_id: Some(run_id),
                        event_type,
                        timestamp: Utc::now(),
                        payload: payload.clone(),
                    })
                    .await;

                let _ = webhook.dispatch("run.progress", payload).await;
            });
        })
    }

    async fn set_running(&self, run_id: Uuid) -> anyhow::Result<()> {
        if let Some(mut entry) = self.runs.get_mut(&run_id) {
            entry.status = RunStatus::Running;
            entry.started_at = Some(Utc::now());
            entry
                .timeline
                .push(format!("{} run started", Utc::now().to_rfc3339()));

            self.memory.upsert_run(&entry).await?;
            return Ok(());
        }

        Err(anyhow::anyhow!("run {run_id} not found"))
    }

    async fn finish_run(
        &self,
        run_id: Uuid,
        status: RunStatus,
        outputs: Vec<NodeExecutionResult>,
        error_message: Option<String>,
    ) -> anyhow::Result<()> {
        let Some(mut entry) = self.runs.get_mut(&run_id) else {
            return Err(anyhow::anyhow!("run {run_id} not found"));
        };

        let records = outputs
            .into_iter()
            .map(|n| AgentExecutionRecord {
                node_id: n.node_id,
                role: n.role,
                model: n.model,
                output: n.output,
                duration_ms: n.duration_ms,
                succeeded: n.succeeded,
                error: n.error,
            })
            .collect::<Vec<_>>();

        entry.status = status;
        entry.outputs = records;
        entry.error = error_message;
        entry.finished_at = Some(Utc::now());
        entry.timeline.push(format!(
            "{} run finished ({})",
            Utc::now().to_rfc3339(),
            entry.status
        ));

        self.memory.upsert_run(&entry).await?;

        self.memory
            .append_event(SessionEvent {
                session_id: entry.session_id,
                run_id: Some(entry.run_id),
                event_type: if entry.status == RunStatus::Succeeded {
                    SessionEventType::RunCompleted
                } else {
                    SessionEventType::RunFailed
                },
                timestamp: Utc::now(),
                payload: serde_json::json!({
                    "status": entry.status,
                    "outputs": entry.outputs.len(),
                    "error": entry.error,
                }),
            })
            .await?;

        if entry.status == RunStatus::Succeeded {
            self.webhook
                .dispatch(
                    "run.completed",
                    serde_json::json!({
                        "run_id": entry.run_id,
                        "session_id": entry.session_id,
                        "outputs": entry.outputs,
                    }),
                )
                .await?;
        } else {
            self.webhook
                .dispatch(
                    "run.failed",
                    serde_json::json!({
                        "run_id": entry.run_id,
                        "session_id": entry.session_id,
                        "error": entry.error,
                    }),
                )
                .await?;
        }

        info!("run {} finished with status {}", run_id, entry.status);
        Ok(())
    }

    async fn mark_run_failed(&self, run_id: Uuid, error_message: String) -> anyhow::Result<()> {
        self.finish_run(run_id, RunStatus::Failed, vec![], Some(error_message))
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn graph_builder_respects_depth_and_dependencies() {
        let tmp = std::env::temp_dir().join(format!("agent-test-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&tmp).unwrap();

        let memory = Arc::new(
            MemoryManager::new(tmp.join("sessions"), format!("sqlite://{}", tmp.join("db.sqlite").display()).as_str())
                .await
                .unwrap(),
        );
        let auth = Arc::new(crate::webhook::AuthManager::new("k", "s", 300));
        let webhook = Arc::new(crate::webhook::WebhookDispatcher::new(
            memory.clone(),
            auth,
            Duration::from_secs(1),
        ));

        let orchestrator = Orchestrator::new(
            AgentRuntime::new(4),
            AgentRegistry::builtin(),
            Arc::new(ModelRouter::new("http://127.0.0.1:11434")),
            memory,
            Arc::new(ContextManager::new(8_000)),
            webhook,
            6,
        );

        let graph = orchestrator.build_graph("test webhook task").unwrap();
        assert!(graph.node("plan").is_some());
        assert!(graph.node("code").is_some());
        assert!(graph.node("webhook_validation").is_some());
    }
}
