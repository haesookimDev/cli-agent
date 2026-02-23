use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use chrono::Utc;
use dashmap::DashMap;
use futures::FutureExt;
use tokio::time::sleep;
use tracing::{error, info};
use uuid::Uuid;

use crate::agents::{AgentInput, AgentRegistry};
use crate::context::{ContextChunk, ContextKind, ContextManager, ContextScope};
use crate::mcp::McpRegistry;
use crate::memory::MemoryManager;
use crate::router::ModelRouter;
use crate::runtime::graph::{AgentNode, DependencyFailurePolicy, ExecutionGraph, ExecutionPolicy};
use crate::runtime::{
    AgentRuntime, EventSink, NodeExecutionResult, OnNodeCompletedFn, RunNodeFn, RuntimeEvent,
};
use crate::types::{
    AgentExecutionRecord, AgentRole, ChatMessage, ChatRole, McpToolDefinition, NodeTraceState,
    RunActionEvent, RunActionType, RunBehaviorActionCount, RunBehaviorLane, RunBehaviorSummary,
    RunBehaviorView, RunRecord, RunRequest, RunStatus, RunSubmission, RunTrace, RunTraceGraph,
    SessionEvent, SessionEventType, SubtaskPlan, TaskType, TraceEdge, WebhookDeliveryRecord,
    WebhookEndpoint, WorkflowTemplate,
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
    mcp: Arc<McpRegistry>,
    runs: Arc<DashMap<Uuid, RunRecord>>,
    controls: Arc<DashMap<Uuid, RunControl>>,
    workflow_graphs: Arc<DashMap<Uuid, ExecutionGraph>>,
    max_graph_depth: u8,
}

#[derive(Debug, Clone)]
struct RunControl {
    cancel_requested: Arc<AtomicBool>,
    pause_requested: Arc<AtomicBool>,
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
        mcp: Arc<McpRegistry>,
    ) -> Self {
        Self {
            runtime,
            agents,
            router,
            memory,
            context,
            webhook,
            mcp,
            runs: Arc::new(DashMap::new()),
            controls: Arc::new(DashMap::new()),
            workflow_graphs: Arc::new(DashMap::new()),
            max_graph_depth,
        }
    }

    pub fn router(&self) -> &Arc<ModelRouter> {
        &self.router
    }

    pub fn get_settings(&self) -> crate::types::AppSettings {
        crate::types::AppSettings {
            default_profile: crate::types::TaskProfile::General,
            preferred_model: self.router.preferred_model(),
            disabled_models: self.router.disabled_models(),
            disabled_providers: self
                .router
                .disabled_providers()
                .iter()
                .map(|p| p.to_string())
                .collect(),
            terminal_command: "claude".to_string(),
            terminal_args: vec![],
            terminal_auto_spawn: false,
        }
    }

    pub async fn update_settings(&self, patch: crate::types::SettingsPatch) {
        if let Some(preferred) = patch.preferred_model {
            self.router.set_preferred_model(preferred);
        }
        if let Some(disabled_models) = patch.disabled_models {
            // Reset all, then disable specified
            for spec in self.router.catalog() {
                self.router.set_model_disabled(&spec.model_id, false);
            }
            for model_id in &disabled_models {
                self.router.set_model_disabled(model_id, true);
            }
        }
        if let Some(disabled_providers) = patch.disabled_providers {
            use crate::router::ProviderKind;
            let all_providers = [
                ProviderKind::OpenAi,
                ProviderKind::Anthropic,
                ProviderKind::Gemini,
                ProviderKind::Vllm,
                ProviderKind::Mock,
            ];
            for p in &all_providers {
                self.router.set_provider_disabled(*p, false);
            }
            for name in &disabled_providers {
                if let Ok(pk) = serde_json::from_value::<ProviderKind>(
                    serde_json::Value::String(name.clone()),
                ) {
                    self.router.set_provider_disabled(pk, true);
                }
            }
        }

        // Build settings: start from current state, then merge terminal fields from DB + patch
        let mut settings = self.get_settings();

        // Load persisted terminal settings as baseline
        if let Ok(Some(persisted)) = self.memory.store().load_settings().await {
            settings.terminal_command = persisted.terminal_command;
            settings.terminal_args = persisted.terminal_args;
            settings.terminal_auto_spawn = persisted.terminal_auto_spawn;
        }

        // Apply terminal patch fields
        if let Some(cmd) = patch.terminal_command {
            settings.terminal_command = cmd;
        }
        if let Some(args) = patch.terminal_args {
            settings.terminal_args = args;
        }
        if let Some(auto) = patch.terminal_auto_spawn {
            settings.terminal_auto_spawn = auto;
        }

        if let Err(e) = self.memory.store().save_settings(&settings).await {
            error!("failed to persist settings: {e}");
        }
    }

    pub async fn load_persisted_settings(&self) {
        match self.memory.store().load_settings().await {
            Ok(Some(settings)) => {
                if let Some(preferred) = settings.preferred_model {
                    self.router.set_preferred_model(Some(preferred));
                }
                for model_id in &settings.disabled_models {
                    self.router.set_model_disabled(model_id, true);
                }
                for name in &settings.disabled_providers {
                    if let Ok(pk) = serde_json::from_value::<crate::router::ProviderKind>(
                        serde_json::Value::String(name.clone()),
                    ) {
                        self.router.set_provider_disabled(pk, true);
                    }
                }
                info!("restored persisted settings");
            }
            Ok(None) => {}
            Err(e) => {
                error!("failed to load persisted settings: {e}");
            }
        }
    }

    pub fn memory(&self) -> &Arc<MemoryManager> {
        &self.memory
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
        self.controls.insert(
            run_id,
            RunControl {
                cancel_requested: Arc::new(AtomicBool::new(false)),
                pause_requested: Arc::new(AtomicBool::new(false)),
            },
        );
        let run_id_text = run_id.to_string();
        self.record_action_event(
            run_id,
            session_id,
            RunActionType::RunQueued,
            Some("run"),
            Some(run_id_text.as_str()),
            None,
            serde_json::json!({
                "profile": req.profile,
                "task": req.task,
            }),
        )
        .await;

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

    pub async fn list_active_runs(&self) -> Vec<RunRecord> {
        let mut active = Vec::new();
        for kv in self.runs.iter() {
            let run = kv.value();
            if matches!(run.status, RunStatus::Running | RunStatus::Queued) {
                active.push(run.clone());
            }
        }
        active.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        active
    }

    pub async fn get_session_messages(
        &self,
        session_id: Uuid,
        limit: usize,
    ) -> anyhow::Result<Vec<ChatMessage>> {
        let mut messages = Vec::new();

        // Gather user messages from store
        let raw_msgs = self.memory.list_session_messages(session_id, limit).await?;
        for (id, role, content, created_at) in raw_msgs {
            let ts = chrono::DateTime::parse_from_rfc3339(&created_at)
                .map(|dt| dt.with_timezone(&chrono::Utc))
                .unwrap_or_else(|_| Utc::now());

            let chat_role = match role.as_str() {
                "user" => ChatRole::User,
                _ => ChatRole::System,
            };
            messages.push(ChatMessage {
                id: format!("msg:{id}"),
                session_id,
                run_id: None,
                role: chat_role,
                content,
                agent_role: None,
                model: None,
                timestamp: ts,
            });
        }

        // Gather agent outputs from runs in this session
        let runs = self.memory.list_session_runs(session_id, limit).await?;
        for run in &runs {
            for output in &run.outputs {
                if output.succeeded && !output.output.is_empty() {
                    let ts = run
                        .finished_at
                        .unwrap_or(run.created_at);
                    messages.push(ChatMessage {
                        id: format!("out:{}:{}", run.run_id, output.node_id),
                        session_id,
                        run_id: Some(run.run_id),
                        role: ChatRole::Agent,
                        content: output.output.clone(),
                        agent_role: Some(output.role),
                        model: Some(output.model.clone()),
                        timestamp: ts,
                    });
                }
            }
        }

        // Sort chronologically
        messages.sort_by(|a, b| a.timestamp.cmp(&b.timestamp));
        // Keep last `limit` messages
        if messages.len() > limit {
            messages = messages.split_off(messages.len() - limit);
        }
        Ok(messages)
    }

    pub async fn list_run_action_events(
        &self,
        run_id: Uuid,
        limit: usize,
    ) -> anyhow::Result<Vec<RunActionEvent>> {
        self.memory.list_run_action_events(run_id, limit).await
    }

    pub async fn list_run_action_events_since(
        &self,
        run_id: Uuid,
        after_seq: i64,
        limit: usize,
    ) -> anyhow::Result<Vec<RunActionEvent>> {
        self.memory
            .list_run_action_events_since(run_id, after_seq, limit)
            .await
    }

    pub async fn get_run_trace(
        &self,
        run_id: Uuid,
        limit: usize,
    ) -> anyhow::Result<Option<RunTrace>> {
        let Some(run) = self.get_run(run_id).await? else {
            return Ok(None);
        };
        let events = self.memory.list_run_action_events(run_id, limit).await?;
        let graph = build_trace_graph(&run, events.as_slice());
        Ok(Some(RunTrace {
            run_id: run.run_id,
            session_id: run.session_id,
            status: Some(run.status),
            events,
            graph,
        }))
    }

    pub async fn get_run_behavior(
        &self,
        run_id: Uuid,
        limit: usize,
    ) -> anyhow::Result<Option<RunBehaviorView>> {
        let Some(trace) = self.get_run_trace(run_id, limit).await? else {
            return Ok(None);
        };
        Ok(Some(build_behavior_view(&trace)))
    }

    pub async fn list_session_runs(
        &self,
        session_id: Uuid,
        limit: usize,
    ) -> anyhow::Result<Vec<RunRecord>> {
        let mut runs = self.memory.list_session_runs(session_id, limit).await?;
        for kv in self.runs.iter() {
            let run = kv.value().clone();
            if run.session_id == session_id && !runs.iter().any(|r| r.run_id == run.run_id) {
                runs.push(run);
            }
        }
        runs.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        runs.truncate(limit);
        Ok(runs)
    }

    pub async fn list_sessions(
        &self,
        limit: usize,
    ) -> anyhow::Result<Vec<crate::types::SessionSummary>> {
        self.memory.list_sessions(limit).await
    }

    pub async fn get_session(
        &self,
        session_id: Uuid,
    ) -> anyhow::Result<Option<crate::types::SessionSummary>> {
        self.memory.get_session(session_id).await
    }

    pub async fn delete_session(&self, session_id: Uuid) -> anyhow::Result<()> {
        self.memory.delete_session(session_id).await?;
        let run_ids = self
            .runs
            .iter()
            .filter(|kv| kv.value().session_id == session_id)
            .map(|kv| *kv.key())
            .collect::<Vec<_>>();

        for run_id in run_ids {
            self.runs.remove(&run_id);
        }
        Ok(())
    }

    pub async fn cancel_run(&self, run_id: Uuid) -> anyhow::Result<bool> {
        let Some(control) = self.controls.get(&run_id) else {
            return Ok(false);
        };
        control.cancel_requested.store(true, Ordering::Relaxed);
        drop(control);

        if let Some(mut run) = self.runs.get_mut(&run_id) {
            if run.status.is_terminal() {
                return Ok(false);
            }
            run.status = RunStatus::Cancelling;
            run.timeline
                .push(format!("{} cancel requested", Utc::now().to_rfc3339()));
            let session_id = run.session_id;
            let run_id_text = run_id.to_string();
            self.memory.upsert_run(&run).await?;
            self.record_action_event(
                run_id,
                session_id,
                RunActionType::RunCancelRequested,
                Some("run"),
                Some(run_id_text.as_str()),
                None,
                serde_json::json!({ "status": "cancelling" }),
            )
            .await;
            return Ok(true);
        }

        Ok(false)
    }

    pub async fn pause_run(&self, run_id: Uuid) -> anyhow::Result<bool> {
        let Some(control) = self.controls.get(&run_id) else {
            return Ok(false);
        };
        control.pause_requested.store(true, Ordering::Relaxed);
        drop(control);

        if let Some(mut run) = self.runs.get_mut(&run_id) {
            if run.status.is_terminal() || run.status == RunStatus::Paused {
                return Ok(false);
            }
            run.status = RunStatus::Paused;
            run.timeline
                .push(format!("{} pause requested", Utc::now().to_rfc3339()));
            let session_id = run.session_id;
            let run_id_text = run_id.to_string();
            self.memory.upsert_run(&run).await?;
            self.record_action_event(
                run_id,
                session_id,
                RunActionType::RunPauseRequested,
                Some("run"),
                Some(run_id_text.as_str()),
                None,
                serde_json::json!({ "status": "paused" }),
            )
            .await;
            return Ok(true);
        }

        Ok(false)
    }

    pub async fn resume_run(&self, run_id: Uuid) -> anyhow::Result<bool> {
        let Some(control) = self.controls.get(&run_id) else {
            return Ok(false);
        };
        control.pause_requested.store(false, Ordering::Relaxed);
        let cancelling = control.cancel_requested.load(Ordering::Relaxed);
        drop(control);

        if let Some(mut run) = self.runs.get_mut(&run_id) {
            if run.status.is_terminal() {
                return Ok(false);
            }
            run.status = if cancelling {
                RunStatus::Cancelling
            } else {
                RunStatus::Running
            };
            run.timeline
                .push(format!("{} resumed", Utc::now().to_rfc3339()));
            let session_id = run.session_id;
            let run_id_text = run_id.to_string();
            self.memory.upsert_run(&run).await?;
            self.record_action_event(
                run_id,
                session_id,
                RunActionType::RunResumed,
                Some("run"),
                Some(run_id_text.as_str()),
                None,
                serde_json::json!({
                    "status": if cancelling { "cancelling" } else { "running" }
                }),
            )
            .await;
            return Ok(true);
        }

        Ok(false)
    }

    pub async fn retry_run(&self, run_id: Uuid) -> anyhow::Result<RunSubmission> {
        let run = self
            .get_run(run_id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("run not found"))?;
        let req = RunRequest {
            task: run.task.clone(),
            profile: run.profile,
            session_id: Some(run.session_id),
            workflow_id: None,
            workflow_params: None,
        };
        self.submit_run(req).await
    }

    pub async fn clone_run(
        &self,
        run_id: Uuid,
        target_session: Option<Uuid>,
    ) -> anyhow::Result<RunSubmission> {
        let run = self
            .get_run(run_id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("run not found"))?;
        let req = RunRequest {
            task: run.task.clone(),
            profile: run.profile,
            session_id: target_session.or(Some(run.session_id)),
            workflow_id: None,
            workflow_params: None,
        };
        self.submit_run(req).await
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

    pub async fn list_webhooks(&self) -> anyhow::Result<Vec<WebhookEndpoint>> {
        self.memory.list_webhooks().await
    }

    pub async fn dispatch_webhook_event(
        &self,
        event: &str,
        payload: serde_json::Value,
    ) -> anyhow::Result<()> {
        self.webhook.dispatch(event, payload).await
    }

    pub async fn list_webhook_deliveries(
        &self,
        dead_letter_only: bool,
        limit: usize,
    ) -> anyhow::Result<Vec<WebhookDeliveryRecord>> {
        self.memory
            .list_webhook_deliveries(dead_letter_only, limit)
            .await
    }

    pub async fn retry_webhook_delivery(&self, delivery_id: i64) -> anyhow::Result<()> {
        let delivery = self
            .memory
            .get_webhook_delivery(delivery_id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("webhook delivery not found"))?;
        self.webhook
            .dispatch_to_endpoint(
                delivery.endpoint_id.as_str(),
                delivery.event.as_str(),
                delivery.payload,
            )
            .await
    }

    async fn record_action_event(
        &self,
        run_id: Uuid,
        session_id: Uuid,
        action: RunActionType,
        actor_type: Option<&str>,
        actor_id: Option<&str>,
        cause_event_id: Option<&str>,
        payload: serde_json::Value,
    ) {
        let _ = self
            .memory
            .append_run_action_event(
                run_id,
                session_id,
                action,
                actor_type,
                actor_id,
                cause_event_id,
                payload,
            )
            .await;
    }

    async fn execute_run(&self, run_id: Uuid, req: RunRequest) -> anyhow::Result<()> {
        let session_id = req.session_id.unwrap_or_else(Uuid::new_v4);
        let cancel_flag = self
            .controls
            .get(&run_id)
            .map(|c| c.cancel_requested.clone())
            .unwrap_or_else(|| Arc::new(AtomicBool::new(false)));
        let pause_flag = self
            .controls
            .get(&run_id)
            .map(|c| c.pause_requested.clone())
            .unwrap_or_else(|| Arc::new(AtomicBool::new(false)));
        self.set_running(run_id).await?;

        if cancel_flag.load(Ordering::Relaxed) {
            self.finish_run(run_id, RunStatus::Cancelled, vec![], None)
                .await?;
            return Ok(());
        }

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

        let mcp_server_names = self.mcp.server_names();
        let mcp_tool_names: Vec<String> = self.mcp.list_all_tools().await
            .into_iter().map(|t| t.name).collect();
        let graph = self
            .workflow_graphs
            .remove(&run_id)
            .map(|(_, g)| g)
            .unwrap_or_else(|| self.build_graph(req.task.as_str(), &mcp_server_names, &mcp_tool_names).unwrap());
        let graph_nodes = graph.nodes();
        self.record_action_event(
            run_id,
            session_id,
            RunActionType::GraphInitialized,
            Some("orchestrator"),
            Some("graph"),
            None,
            serde_json::json!({
                "nodes": graph_nodes.iter().map(|n| {
                    serde_json::json!({
                        "id": n.id.clone(),
                        "role": n.role,
                        "dependencies": n.dependencies.clone(),
                        "depth": n.depth,
                        "policy": {
                            "retry": n.policy.retry,
                            "timeout_ms": n.policy.timeout_ms,
                            "max_parallelism": n.policy.max_parallelism,
                            "on_dependency_failure": n.policy.on_dependency_failure,
                            "fallback_node": n.policy.fallback_node.clone(),
                        }
                    })
                }).collect::<Vec<_>>()
            }),
        )
        .await;
        self.record_action_event(
            run_id,
            session_id,
            RunActionType::WebhookDispatched,
            Some("webhook"),
            Some("run.started"),
            None,
            serde_json::json!({ "event": "run.started" }),
        )
        .await;

        let event_sink = self.build_event_sink(run_id, session_id);
        let run_node = self.build_run_node_fn(run_id, session_id, req.clone(), event_sink.clone());
        let on_complete = self.build_on_completed_fn(run_id, session_id, req.task.clone());

        let outputs = self
            .runtime
            .execute_graph(
                graph,
                run_node,
                on_complete,
                Some(event_sink),
                Some(Arc::new({
                    let cancel_flag = cancel_flag.clone();
                    move || cancel_flag.load(Ordering::Relaxed)
                })),
                Some(Arc::new({
                    let pause_flag = pause_flag.clone();
                    move || pause_flag.load(Ordering::Relaxed)
                })),
            )
            .await;

        match outputs {
            Ok(node_results) => {
                let final_status = if cancel_flag.load(Ordering::Relaxed) {
                    RunStatus::Cancelled
                } else if node_results.iter().all(|r| r.succeeded) {
                    // Verification loop: check if the request was fully satisfied
                    let verified = self
                        .verify_completion(run_id, session_id, &req.task, &node_results)
                        .await;
                    if verified {
                        RunStatus::Succeeded
                    } else {
                        RunStatus::Succeeded // Still mark as succeeded but log the gap
                    }
                } else {
                    RunStatus::Failed
                };

                self.finish_run(run_id, final_status, node_results, None)
                    .await?;
            }
            Err(err) => {
                self.finish_run(run_id, RunStatus::Failed, vec![], Some(err.to_string()))
                    .await?;
            }
        }

        Ok(())
    }

    async fn verify_completion(
        &self,
        run_id: Uuid,
        session_id: Uuid,
        original_task: &str,
        results: &[NodeExecutionResult],
    ) -> bool {
        // Record verification start
        self.record_action_event(
            run_id,
            session_id,
            RunActionType::VerificationStarted,
            Some("orchestrator"),
            Some("reviewer"),
            None,
            serde_json::json!({"task": original_task}),
        )
        .await;

        // Collect outputs from successful nodes
        let outputs_summary: String = results
            .iter()
            .filter(|r| r.succeeded)
            .map(|r| format!("[{}] {}", r.node_id, r.output.chars().take(500).collect::<String>()))
            .collect::<Vec<_>>()
            .join("\n---\n");

        // Run Reviewer agent
        let review_input = crate::agents::AgentInput {
            task: format!(
                "Original request: {}\n\nExecution results:\n{}\n\nDid the execution fully satisfy the original request? Answer COMPLETE if yes, or INCOMPLETE: <reason> if not.",
                original_task, outputs_summary
            ),
            instructions: "Review the execution results against the original request.".to_string(),
            context: crate::context::OptimizedContext::empty(),
            dependency_outputs: vec![],
            brief: crate::types::StructuredBrief {
                goal: format!("Verify completion of: {}", original_task),
                constraints: vec![],
                decisions: vec![],
                references: vec![],
            },
        };

        let review_result = self
            .agents
            .run_role(AgentRole::Reviewer, review_input, self.router.clone())
            .await;

        let (is_complete, reason) = match &review_result {
            Ok(output) => {
                let content = output.content.trim();
                if content.starts_with("COMPLETE") {
                    (true, "Verified complete".to_string())
                } else if content.starts_with("INCOMPLETE") {
                    let reason = content
                        .strip_prefix("INCOMPLETE:")
                        .unwrap_or(content)
                        .trim()
                        .to_string();
                    (false, reason)
                } else {
                    // Default to complete if output is ambiguous
                    (true, "Assumed complete (ambiguous review)".to_string())
                }
            }
            Err(e) => {
                // If reviewer fails, don't block — assume complete
                (true, format!("Reviewer unavailable: {e}"))
            }
        };

        // Record verification result
        self.record_action_event(
            run_id,
            session_id,
            RunActionType::VerificationComplete,
            Some("reviewer"),
            Some("orchestrator"),
            None,
            serde_json::json!({
                "complete": is_complete,
                "reason": reason,
            }),
        )
        .await;

        is_complete
    }

    fn classify_task(task: &str, mcp_server_names: &[String], mcp_tool_names: &[String]) -> TaskType {
        let lower = task.to_lowercase();

        // Configuration keywords
        let config_kw = [
            "toggle", "enable", "disable", "setting", "config", "model",
            "provider", "prefer",
        ];
        if config_kw.iter().any(|kw| lower.contains(kw)) {
            return TaskType::Configuration;
        }

        // Tool / MCP operation keywords
        let tool_kw = ["mcp", "tool call", "tool 호출", "도구"];
        if tool_kw.iter().any(|kw| lower.contains(kw)) {
            return TaskType::ToolOperation;
        }

        // If MCP servers are registered, check if task mentions server or tool names
        if !mcp_server_names.is_empty() {
            let matches_server = mcp_server_names.iter().any(|name| lower.contains(&name.to_lowercase()));
            if matches_server {
                return TaskType::ToolOperation;
            }

            // Check bare tool names (e.g. "search_repositories" from "github/search_repositories")
            let matches_tool = mcp_tool_names.iter().any(|name| {
                let bare = name.rsplit('/').next().unwrap_or(name);
                lower.contains(&bare.to_lowercase())
            });
            if matches_tool {
                return TaskType::ToolOperation;
            }

            // If tools are available and task involves file/project/repo operations
            let tool_action_kw = [
                "파일", "file", "read", "읽어", "열어", "프로젝트", "project",
                "repo", "repository", "디렉토리", "directory", "folder", "폴더",
                "commit", "커밋", "branch", "브랜치", "issue", "이슈", "pr",
                "search", "검색",
            ];
            if tool_action_kw.iter().any(|kw| lower.contains(kw)) {
                return TaskType::ToolOperation;
            }
        }

        // Code generation keywords
        let code_kw = [
            "code", "implement", "function", "class", "refactor", "write",
            "bug", "fix", "debug", "api", "endpoint",
        ];
        if code_kw.iter().any(|kw| lower.contains(kw)) {
            return TaskType::CodeGeneration;
        }

        // Analysis keywords
        let analysis_kw = [
            "analyze", "analysis", "pattern", "compare", "insight",
            "metric", "stat", "evaluate", "assess",
        ];
        if analysis_kw.iter().any(|kw| lower.contains(kw)) {
            return TaskType::Analysis;
        }

        // Simple query: short tasks without action verbs
        if lower.split_whitespace().count() <= 8
            && !lower.contains("and")
            && !lower.contains("then")
        {
            return TaskType::SimpleQuery;
        }

        TaskType::Complex
    }

    fn default_policy() -> ExecutionPolicy {
        ExecutionPolicy {
            max_parallelism: 1,
            retry: 1,
            timeout_ms: 120_000,
            circuit_breaker: 3,
            on_dependency_failure: DependencyFailurePolicy::ContinueOnError,
            fallback_node: None,
        }
    }

    fn build_graph(&self, task: &str, mcp_server_names: &[String], mcp_tool_names: &[String]) -> anyhow::Result<ExecutionGraph> {
        let task_type = Self::classify_task(task, mcp_server_names, mcp_tool_names);
        let mut graph = ExecutionGraph::new(self.max_graph_depth);

        // Planner instructions — include available tools when MCP servers are registered
        let planner_instructions = if !mcp_tool_names.is_empty() {
            let tool_summary = mcp_tool_names.iter()
                .take(30)
                .cloned()
                .collect::<Vec<_>>()
                .join(", ");
            let server_descs = self.mcp.server_descriptions();
            let guide = if !server_descs.is_empty() {
                let lines: Vec<String> = server_descs
                    .iter()
                    .map(|(name, desc)| format!("  - {}: {}", name, desc))
                    .collect();
                format!("\nServer guide:\n{}", lines.join("\n"))
            } else {
                String::new()
            };
            format!(
                "Plan work and constraints. Available MCP tools: [{}]. \
                 If the task requires external data or operations, plan to use these tools via the tool_call node.{}",
                tool_summary, guide
            )
        } else {
            "Plan work and constraints.".to_string()
        };

        let mut plan = AgentNode::new("plan", AgentRole::Planner, planner_instructions);
        plan.policy = ExecutionPolicy {
            timeout_ms: 120_000,
            on_dependency_failure: DependencyFailurePolicy::FailFast,
            ..Self::default_policy()
        };
        graph.add_node(plan)?;

        match task_type {
            TaskType::SimpleQuery => {
                // Lightweight: plan → summarize
                let mut summarize = AgentNode::new(
                    "summarize",
                    AgentRole::Summarizer,
                    "Summarize the planner output into a concise answer.",
                );
                summarize.dependencies = vec!["plan".to_string()];
                summarize.policy = Self::default_policy();
                graph.add_node(summarize)?;
            }
            TaskType::Analysis => {
                // plan → extract → analyze → summarize
                let mut extract = AgentNode::new(
                    "extract",
                    AgentRole::Extractor,
                    "Extract structured facts from user task and planner output.",
                );
                extract.dependencies = vec!["plan".to_string()];
                extract.policy = Self::default_policy();
                graph.add_node(extract)?;

                let mut analyze = AgentNode::new(
                    "analyze",
                    AgentRole::Analyzer,
                    "Analyze the extracted data, identify patterns and insights.",
                );
                analyze.dependencies = vec!["extract".to_string()];
                analyze.policy = ExecutionPolicy {
                    timeout_ms: 120_000,
                    ..Self::default_policy()
                };
                graph.add_node(analyze)?;

                let mut summarize = AgentNode::new(
                    "summarize",
                    AgentRole::Summarizer,
                    "Consolidate analysis results into a concise report.",
                );
                summarize.dependencies = vec!["analyze".to_string()];
                summarize.policy = Self::default_policy();
                graph.add_node(summarize)?;
            }
            TaskType::Configuration => {
                // plan → config_manage → summarize
                let mut config = AgentNode::new(
                    "config_manage",
                    AgentRole::ConfigManager,
                    "Execute the configuration changes requested by the user.",
                );
                config.dependencies = vec!["plan".to_string()];
                config.policy = ExecutionPolicy {
                    timeout_ms: 120_000,
                    ..Self::default_policy()
                };
                graph.add_node(config)?;

                let mut summarize = AgentNode::new(
                    "summarize",
                    AgentRole::Summarizer,
                    "Summarize the configuration changes made.",
                );
                summarize.dependencies = vec!["config_manage".to_string()];
                summarize.policy = Self::default_policy();
                graph.add_node(summarize)?;
            }
            TaskType::ToolOperation => {
                // plan → tool_call → summarize
                let mut tool = AgentNode::new(
                    "tool_call",
                    AgentRole::ToolCaller,
                    "Execute MCP tool calls as planned.",
                );
                tool.dependencies = vec!["plan".to_string()];
                tool.policy = ExecutionPolicy {
                    timeout_ms: 180_000,
                    retry: 2,
                    ..Self::default_policy()
                };
                graph.add_node(tool)?;

                let mut summarize = AgentNode::new(
                    "summarize",
                    AgentRole::Summarizer,
                    "Summarize tool execution results.",
                );
                summarize.dependencies = vec!["tool_call".to_string()];
                summarize.policy = Self::default_policy();
                graph.add_node(summarize)?;
            }
            TaskType::CodeGeneration | TaskType::Complex => {
                // Full graph: plan → extract + context_probe → code → fallback → summarize
                let mut extract = AgentNode::new(
                    "extract",
                    AgentRole::Extractor,
                    "Extract structured facts from user task and planner output.",
                );
                extract.dependencies = vec!["plan".to_string()];
                extract.policy = ExecutionPolicy {
                    max_parallelism: 2,
                    ..Self::default_policy()
                };
                graph.add_node(extract)?;

                let mut context_probe = AgentNode::new(
                    "context_probe",
                    AgentRole::Extractor,
                    "Probe memory and context windows for high-value retrieval candidates.",
                );
                context_probe.dependencies = vec!["plan".to_string()];
                context_probe.policy = ExecutionPolicy {
                    max_parallelism: 2,
                    timeout_ms: 60_000,
                    ..Self::default_policy()
                };
                graph.add_node(context_probe)?;

                let mut code = AgentNode::new(
                    "code",
                    AgentRole::Coder,
                    "Generate implementation-level output from extracted and contextualized plan.",
                );
                code.dependencies = vec!["extract".to_string(), "context_probe".to_string()];
                code.policy = ExecutionPolicy {
                    max_parallelism: 2,
                    retry: 2,
                    timeout_ms: 180_000,
                    circuit_breaker: 2,
                    on_dependency_failure: DependencyFailurePolicy::FailFast,
                    fallback_node: Some("fallback_code".to_string()),
                };
                graph.add_node(code)?;

                let mut fallback_code = AgentNode::new(
                    "fallback_code",
                    AgentRole::Fallback,
                    "Recover from coding node failures using robust conservative strategy.",
                );
                fallback_code.dependencies = vec!["code".to_string()];
                fallback_code.policy = Self::default_policy();
                graph.add_node(fallback_code)?;

                let mut summarize = AgentNode::new(
                    "summarize",
                    AgentRole::Summarizer,
                    "Summarize outcomes and produce checkpoint summary for context compaction.",
                );
                summarize.dependencies =
                    vec!["code".to_string(), "fallback_code".to_string()];
                summarize.policy = Self::default_policy();
                graph.add_node(summarize)?;

                // Conditional webhook validation
                if task.to_lowercase().contains("webhook") {
                    let mut webhook = AgentNode::new(
                        "webhook_validation",
                        AgentRole::Extractor,
                        "Validate webhook contract and integration constraints.",
                    );
                    webhook.dependencies = vec!["plan".to_string()];
                    webhook.depth = 1;
                    webhook.policy = ExecutionPolicy {
                        timeout_ms: 60_000,
                        ..Self::default_policy()
                    };
                    graph.add_node(webhook)?;
                }
            }
        }

        Ok(graph)
    }

    fn build_run_node_fn(
        &self,
        run_id: Uuid,
        session_id: Uuid,
        req: RunRequest,
        event_sink: EventSink,
    ) -> RunNodeFn {
        let agents = self.agents.clone();
        let router = self.router.clone();
        let memory = self.memory.clone();
        let context = self.context.clone();
        let mcp = self.mcp.clone();

        Arc::new(move |node: AgentNode, deps: Vec<NodeExecutionResult>| {
            let agents = agents.clone();
            let router = router.clone();
            let memory = memory.clone();
            let context = context.clone();
            let req = req.clone();
            let mcp = mcp.clone();
            let event_sink = event_sink.clone();

            async move {
                let started = Instant::now();

                // ToolCaller role: LLM-driven tool selection + MCP execution
                if node.role == AgentRole::ToolCaller {
                    // Phase A: Collect dependency outputs and available tools
                    let dep_outputs: Vec<String> = deps
                        .iter()
                        .map(|d| format!("{}: {}", d.node_id, d.output))
                        .collect();

                    let available_tools = mcp.list_all_tools().await;
                    let tool_list_str = available_tools
                        .iter()
                        .map(|t| {
                            format!(
                                "- {} : {} | schema: {}",
                                t.name,
                                t.description,
                                serde_json::to_string(&t.input_schema).unwrap_or_default()
                            )
                        })
                        .collect::<Vec<_>>()
                        .join("\n");

                    let server_descs = mcp.server_descriptions();
                    let server_guide = if !server_descs.is_empty() {
                        let lines: Vec<String> = server_descs
                            .iter()
                            .map(|(name, desc)| format!("- {}: {}", name, desc))
                            .collect();
                        format!("\n\nSERVER SELECTION GUIDE:\n{}", lines.join("\n"))
                    } else {
                        String::new()
                    };

                    // Phase B: LLM decides which tool(s) to call
                    let selection_prompt = format!(
                        "You are the tool caller agent.\n\n\
                         USER TASK:\n{}\n\n\
                         PLANNER OUTPUT:\n{}\n\n\
                         AVAILABLE MCP TOOLS:\n{}{}\n\n\
                         Based on the planner output, select which tool(s) to call.\n\
                         Respond ONLY with a JSON array. Each element: \
                         {{\"tool_name\": \"server/tool_name\", \"arguments\": {{...}}}}\n\
                         If no tool is needed, respond with [].",
                        req.task,
                        dep_outputs.join("\n---\n"),
                        tool_list_str,
                        server_guide,
                    );

                    let optimized = context.optimize(vec![]);
                    let brief = context.build_structured_brief(
                        req.task.clone(),
                        vec!["select tools".to_string()],
                        vec![format!("node={}", node.id)],
                        vec![],
                    );

                    let selection_input = AgentInput {
                        task: req.task.clone(),
                        instructions: selection_prompt,
                        context: optimized,
                        dependency_outputs: dep_outputs,
                        brief,
                    };

                    let selection_result = agents
                        .run_role(AgentRole::ToolCaller, selection_input, router.clone())
                        .await;

                    let llm_output = match selection_result {
                        Ok(output) => output.content,
                        Err(err) => {
                            return Ok(NodeExecutionResult {
                                node_id: node.id,
                                role: node.role,
                                model: "unavailable".to_string(),
                                output: String::new(),
                                duration_ms: started.elapsed().as_millis(),
                                succeeded: false,
                                error: Some(format!("LLM tool selection failed: {err}")),
                            });
                        }
                    };

                    // Phase C: Parse LLM response and execute MCP calls
                    let json_str = llm_output
                        .trim()
                        .strip_prefix("```json")
                        .or_else(|| llm_output.trim().strip_prefix("```"))
                        .unwrap_or(llm_output.trim())
                        .strip_suffix("```")
                        .unwrap_or(llm_output.trim())
                        .trim();

                    let tool_calls: Vec<serde_json::Value> =
                        serde_json::from_str(json_str).unwrap_or_default();

                    if tool_calls.is_empty() {
                        return Ok(NodeExecutionResult {
                            node_id: node.id,
                            role: node.role,
                            model: "mcp:none".to_string(),
                            output: llm_output,
                            duration_ms: started.elapsed().as_millis(),
                            succeeded: true,
                            error: None,
                        });
                    }

                    let mut results = Vec::new();
                    for call in &tool_calls {
                        let tool_name = call
                            .get("tool_name")
                            .and_then(|v| v.as_str())
                            .unwrap_or("unknown")
                            .to_string();
                        let arguments = call
                            .get("arguments")
                            .cloned()
                            .unwrap_or(serde_json::json!({}));
                        let arguments_snapshot = arguments.clone();

                        match mcp.call_tool(&tool_name, arguments).await {
                            Ok(result) => {
                                let _ = memory
                                    .append_run_action_event(
                                        run_id,
                                        session_id,
                                        RunActionType::McpToolCalled,
                                        Some("node"),
                                        Some(node.id.as_str()),
                                        None,
                                        serde_json::json!({
                                            "node_id": node.id,
                                            "tool_name": tool_name,
                                            "arguments": arguments_snapshot,
                                            "succeeded": result.succeeded,
                                            "content": result.content,
                                            "error": result.error,
                                            "duration_ms": result.duration_ms,
                                        }),
                                    )
                                    .await;
                                results.push(format!(
                                    "[{}] {}: {}",
                                    if result.succeeded { "OK" } else { "ERR" },
                                    tool_name,
                                    result.content
                                ));
                            }
                            Err(err) => {
                                results
                                    .push(format!("[ERR] {}: {}", tool_name, err));
                            }
                        }
                    }

                    let all_succeeded = results.iter().all(|r| r.starts_with("[OK]"));
                    let combined_output = results.join("\n---\n");

                    return Ok(NodeExecutionResult {
                        node_id: node.id,
                        role: node.role,
                        model: format!("mcp:{}", tool_calls.len()),
                        output: combined_output,
                        duration_ms: started.elapsed().as_millis(),
                        succeeded: all_succeeded,
                        error: if all_succeeded {
                            None
                        } else {
                            Some("one or more tool calls failed".to_string())
                        },
                    });
                }

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

                let token_node_id = node.id.clone();
                let token_role = node.role;
                let token_sink = event_sink.clone();
                let on_token: crate::router::TokenCallback = Arc::new(move |token: &str| {
                    token_sink(RuntimeEvent::NodeTokenChunk {
                        node_id: token_node_id.clone(),
                        role: token_role,
                        token: token.to_string(),
                    });
                });

                let run = agents
                    .run_role_stream(node.role, input, router.clone(), on_token)
                    .await;
                match run {
                    Ok(output) => {
                        let node_id = node.id.clone();
                        let role = node.role;
                        let model = output.model.clone();
                        let _ = memory
                            .append_run_action_event(
                                run_id,
                                session_id,
                                RunActionType::ModelSelected,
                                Some("node"),
                                Some(node_id.as_str()),
                                None,
                                serde_json::json!({
                                    "node_id": node_id.clone(),
                                    "role": role,
                                    "model": model.clone(),
                                }),
                            )
                            .await;

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
                                Some(node_id.as_str()),
                            )
                            .await;

                        Ok(NodeExecutionResult {
                            node_id,
                            role,
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

    fn build_on_completed_fn(&self, run_id: Uuid, session_id: Uuid, task: String) -> OnNodeCompletedFn {
        let memory = self.memory.clone();
        Arc::new(move |node: AgentNode, result: NodeExecutionResult| {
            let task = task.clone();
            let memory = memory.clone();
            async move {
                let mut dynamic_nodes = Vec::new();

                if node.role == AgentRole::Planner && result.succeeded && node.depth < 5 {
                    // Try to parse output as SubtaskPlan JSON
                    if let Ok(plan) = serde_json::from_str::<SubtaskPlan>(&result.output) {
                        if !plan.subtasks.is_empty() {
                            let _ = memory
                                .append_run_action_event(
                                    run_id,
                                    session_id,
                                    RunActionType::SubtaskPlanned,
                                    Some("node"),
                                    Some(node.id.as_str()),
                                    None,
                                    serde_json::json!({
                                        "subtask_count": plan.subtasks.len(),
                                        "subtasks": plan.subtasks.iter().map(|s| &s.id).collect::<Vec<_>>(),
                                    }),
                                )
                                .await;

                            for subtask in plan.subtasks {
                                let role = subtask.agent_role;
                                let instructions = if subtask.instructions.is_empty() {
                                    subtask.description.clone()
                                } else {
                                    subtask.instructions.clone()
                                };
                                let mut sub_node = AgentNode::new(
                                    subtask.id.clone(),
                                    role,
                                    instructions,
                                );
                                sub_node.dependencies = subtask.dependencies;
                                // If dependencies are empty, depend on the planner node
                                if sub_node.dependencies.is_empty() {
                                    sub_node.dependencies = vec![node.id.clone()];
                                }
                                sub_node.depth = node.depth + 1;
                                sub_node.policy = ExecutionPolicy {
                                    max_parallelism: 2,
                                    retry: 1,
                                    timeout_ms: 120_000,
                                    circuit_breaker: 3,
                                    on_dependency_failure: DependencyFailurePolicy::ContinueOnError,
                                    fallback_node: None,
                                };
                                dynamic_nodes.push(sub_node);
                            }
                            return Ok(dynamic_nodes);
                        }
                    }

                    // Fallback: existing checkpoint logic for long tasks
                    if task.split_whitespace().count() > 20 {
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
                            timeout_ms: 60_000,
                            circuit_breaker: 2,
                            on_dependency_failure: DependencyFailurePolicy::ContinueOnError,
                            fallback_node: None,
                        };
                        dynamic_nodes.push(dynamic);
                    }
                }

                // Emit TerminalSuggested when Coder node completes successfully
                if node.role == AgentRole::Coder && result.succeeded {
                    let _ = memory
                        .append_run_action_event(
                            run_id,
                            session_id,
                            RunActionType::TerminalSuggested,
                            Some("node"),
                            Some("code"),
                            None,
                            serde_json::json!({
                                "suggestion": "Coder agent completed. Open terminal to apply changes.",
                            }),
                        )
                        .await;
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
            // Token chunks are high-frequency: store as action events only (skip timeline/session/webhook)
            if let RuntimeEvent::NodeTokenChunk {
                ref node_id,
                ref role,
                ref token,
            } = event
            {
                let memory = memory.clone();
                let node_id = node_id.clone();
                let role = *role;
                let token = token.clone();
                tokio::spawn(async move {
                    let _ = memory
                        .append_run_action_event(
                            run_id,
                            session_id,
                            RunActionType::NodeTokenChunk,
                            Some("runtime"),
                            Some(node_id.as_str()),
                            None,
                            serde_json::json!({
                                "node_id": node_id,
                                "role": role,
                                "token": token,
                            }),
                        )
                        .await;
                });
                return;
            }

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
                    RuntimeEvent::DynamicNodeAdded {
                        node_id,
                        from_node,
                        role,
                        dependencies,
                    } => serde_json::json!({
                        "node_id": node_id,
                        "phase": "dynamic_added",
                        "from": from_node,
                        "role": role,
                        "dependencies": dependencies,
                    }),
                    RuntimeEvent::NodeTokenChunk { .. } => unreachable!(),
                    RuntimeEvent::GraphCompleted => {
                        serde_json::json!({ "phase": "graph_completed" })
                    }
                };

                let event_type = match event_clone {
                    RuntimeEvent::NodeStarted { .. } => SessionEventType::AgentStarted,
                    RuntimeEvent::NodeCompleted { .. } => SessionEventType::AgentOutput,
                    RuntimeEvent::NodeFailed { .. } => SessionEventType::RunFailed,
                    RuntimeEvent::NodeSkipped { .. }
                    | RuntimeEvent::DynamicNodeAdded { .. }
                    | RuntimeEvent::NodeTokenChunk { .. }
                    | RuntimeEvent::GraphCompleted => SessionEventType::RunProgress,
                };

                let (action, actor_id) = match &event_clone {
                    RuntimeEvent::NodeStarted { node_id, .. } => {
                        (RunActionType::NodeStarted, Some(node_id.as_str()))
                    }
                    RuntimeEvent::NodeCompleted { node_id, .. } => {
                        (RunActionType::NodeCompleted, Some(node_id.as_str()))
                    }
                    RuntimeEvent::NodeFailed { node_id, .. } => {
                        (RunActionType::NodeFailed, Some(node_id.as_str()))
                    }
                    RuntimeEvent::NodeSkipped { node_id, .. } => {
                        (RunActionType::NodeSkipped, Some(node_id.as_str()))
                    }
                    RuntimeEvent::DynamicNodeAdded { node_id, .. } => {
                        (RunActionType::DynamicNodeAdded, Some(node_id.as_str()))
                    }
                    RuntimeEvent::NodeTokenChunk { .. } => unreachable!(),
                    RuntimeEvent::GraphCompleted => (RunActionType::GraphCompleted, Some("graph")),
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

                let _ = memory
                    .append_run_action_event(
                        run_id,
                        session_id,
                        action,
                        Some("runtime"),
                        actor_id,
                        None,
                        payload.clone(),
                    )
                    .await;

                let _ = webhook.dispatch("run.progress", payload).await;
            });
        })
    }

    async fn set_running(&self, run_id: Uuid) -> anyhow::Result<()> {
        if let Some(mut entry) = self.runs.get_mut(&run_id) {
            let cancelling = self
                .controls
                .get(&run_id)
                .map(|control| control.cancel_requested.load(Ordering::Relaxed))
                .unwrap_or(false);
            let paused = self
                .controls
                .get(&run_id)
                .map(|control| control.pause_requested.load(Ordering::Relaxed))
                .unwrap_or(false);
            entry.status = if cancelling {
                RunStatus::Cancelling
            } else if paused {
                RunStatus::Paused
            } else {
                RunStatus::Running
            };
            entry.started_at = Some(Utc::now());
            entry
                .timeline
                .push(format!("{} run started", Utc::now().to_rfc3339()));
            let session_id = entry.session_id;
            let run_id_text = run_id.to_string();

            self.memory.upsert_run(&entry).await?;
            self.record_action_event(
                run_id,
                session_id,
                RunActionType::RunStarted,
                Some("run"),
                Some(run_id_text.as_str()),
                None,
                serde_json::json!({
                    "status": if cancelling {
                        "cancelling"
                    } else if paused {
                        "paused"
                    } else {
                        "running"
                    }
                }),
            )
            .await;
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
        let status_text = entry.status.to_string();
        entry.timeline.push(format!(
            "{} run finished ({})",
            Utc::now().to_rfc3339(),
            status_text
        ));
        let session_id = entry.session_id;
        let run_status = entry.status;
        let output_len = entry.outputs.len();
        let run_error = entry.error.clone();
        let run_id_text = run_id.to_string();

        self.memory.upsert_run(&entry).await?;
        self.record_action_event(
            run_id,
            session_id,
            RunActionType::RunFinished,
            Some("run"),
            Some(run_id_text.as_str()),
            None,
            serde_json::json!({
                "status": run_status,
                "outputs": output_len,
                "error": run_error,
            }),
        )
        .await;

        self.memory
            .append_event(SessionEvent {
                session_id,
                run_id: Some(entry.run_id),
                event_type: match entry.status {
                    RunStatus::Succeeded => SessionEventType::RunCompleted,
                    RunStatus::Cancelled => SessionEventType::RunCancelled,
                    _ => SessionEventType::RunFailed,
                },
                timestamp: Utc::now(),
                payload: serde_json::json!({
                    "status": entry.status,
                    "outputs": output_len,
                    "error": entry.error,
                }),
            })
            .await?;

        match entry.status {
            RunStatus::Succeeded => {
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
                self.record_action_event(
                    run_id,
                    session_id,
                    RunActionType::WebhookDispatched,
                    Some("webhook"),
                    Some("run.completed"),
                    None,
                    serde_json::json!({ "event": "run.completed" }),
                )
                .await;
            }
            RunStatus::Cancelled => {
                self.webhook
                    .dispatch(
                        "run.cancelled",
                        serde_json::json!({
                            "run_id": entry.run_id,
                            "session_id": entry.session_id,
                        }),
                    )
                    .await?;
                self.record_action_event(
                    run_id,
                    session_id,
                    RunActionType::WebhookDispatched,
                    Some("webhook"),
                    Some("run.cancelled"),
                    None,
                    serde_json::json!({ "event": "run.cancelled" }),
                )
                .await;
            }
            _ => {
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
                self.record_action_event(
                    run_id,
                    session_id,
                    RunActionType::WebhookDispatched,
                    Some("webhook"),
                    Some("run.failed"),
                    None,
                    serde_json::json!({ "event": "run.failed" }),
                )
                .await;
            }
        }

        self.controls.remove(&run_id);
        info!("run {} finished with status {}", run_id, entry.status);
        Ok(())
    }

    async fn mark_run_failed(&self, run_id: Uuid, error_message: String) -> anyhow::Result<()> {
        self.finish_run(run_id, RunStatus::Failed, vec![], Some(error_message))
            .await
    }

    // --- Workflow methods ---

    pub async fn save_workflow_from_run(
        &self,
        run_id: Uuid,
        name: String,
        description: String,
    ) -> anyhow::Result<WorkflowTemplate> {
        let events = self.memory.list_run_action_events(run_id, 500).await?;
        let graph_event = events
            .iter()
            .find(|e| e.action == RunActionType::GraphInitialized)
            .ok_or_else(|| anyhow::anyhow!("no graph found for run {run_id}"))?;

        let nodes_json = graph_event
            .payload
            .get("nodes")
            .cloned()
            .unwrap_or(serde_json::json!([]));

        let graph_template = crate::types::WorkflowGraphTemplate {
            nodes: serde_json::from_value(nodes_json).unwrap_or_default(),
        };

        let now = chrono::Utc::now();
        let template = WorkflowTemplate {
            id: Uuid::new_v4().to_string(),
            name,
            description,
            created_at: now,
            updated_at: now,
            source_run_id: Some(run_id),
            graph_template,
            parameters: Vec::new(),
        };

        self.memory.save_workflow(&template).await?;
        Ok(template)
    }

    pub async fn list_workflows(
        &self,
        limit: usize,
    ) -> anyhow::Result<Vec<WorkflowTemplate>> {
        self.memory.list_workflows(limit).await
    }

    pub async fn get_workflow(&self, id: &str) -> anyhow::Result<Option<WorkflowTemplate>> {
        self.memory.get_workflow(id).await
    }

    pub async fn delete_workflow(&self, id: &str) -> anyhow::Result<()> {
        self.memory.delete_workflow(id).await
    }

    // --- Cron Schedule methods ---

    pub async fn create_schedule(
        &self,
        req: crate::types::CreateScheduleRequest,
    ) -> anyhow::Result<crate::types::CronSchedule> {
        // Validate cron expression
        use std::str::FromStr;
        let parsed = cron::Schedule::from_str(&req.cron_expr)
            .map_err(|e| anyhow::anyhow!("invalid cron expression: {e}"))?;
        let next = parsed.upcoming(Utc).next();

        let schedule = crate::types::CronSchedule {
            id: Uuid::new_v4(),
            workflow_id: req.workflow_id,
            cron_expr: req.cron_expr,
            enabled: req.enabled,
            parameters: req.parameters,
            last_run_at: None,
            next_run_at: next,
            created_at: Utc::now(),
        };
        self.memory.create_schedule(&schedule).await?;
        Ok(schedule)
    }

    pub async fn list_schedules(&self, limit: usize) -> anyhow::Result<Vec<crate::types::CronSchedule>> {
        self.memory.list_schedules(limit).await
    }

    pub async fn get_schedule(&self, id: Uuid) -> anyhow::Result<Option<crate::types::CronSchedule>> {
        self.memory.get_schedule(id).await
    }

    pub async fn update_schedule(
        &self,
        id: Uuid,
        req: crate::types::UpdateScheduleRequest,
    ) -> anyhow::Result<Option<crate::types::CronSchedule>> {
        // If cron_expr changed, validate and recompute next_run_at
        let new_next = if let Some(ref expr) = req.cron_expr {
            use std::str::FromStr;
            let parsed = cron::Schedule::from_str(expr)
                .map_err(|e| anyhow::anyhow!("invalid cron expression: {e}"))?;
            Some(parsed.upcoming(Utc).next())
        } else {
            None
        };

        let params_ref = req.parameters.as_ref().map(|opt| opt.as_ref());

        self.memory
            .update_schedule(
                id,
                req.cron_expr.as_deref(),
                req.enabled,
                params_ref,
                new_next,
            )
            .await?;

        self.memory.get_schedule(id).await
    }

    pub async fn delete_schedule(&self, id: Uuid) -> anyhow::Result<()> {
        self.memory.delete_schedule(id).await
    }

    pub async fn execute_workflow(
        &self,
        workflow_id: &str,
        params: Option<serde_json::Value>,
        session_id: Option<Uuid>,
    ) -> anyhow::Result<RunSubmission> {
        let template = self
            .memory
            .get_workflow(workflow_id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("workflow not found: {workflow_id}"))?;

        let mut graph = ExecutionGraph::new(self.max_graph_depth);
        for wn in &template.graph_template.nodes {
            let role = wn.role;
            let instructions = wn.instructions.clone();
            let mut node = AgentNode::new(wn.id.clone(), role, instructions);
            node.dependencies = wn.dependencies.clone();
            if !wn.policy.is_null() {
                if let Ok(policy) = serde_json::from_value::<ExecutionPolicy>(wn.policy.clone()) {
                    node.policy = policy;
                }
            }
            graph.add_node(node)?;
        }

        let task = format!(
            "Workflow: {} ({})",
            template.name,
            params
                .as_ref()
                .map(|p| p.to_string())
                .unwrap_or_default()
        );

        let req = RunRequest {
            task,
            profile: crate::types::TaskProfile::General,
            session_id,
            workflow_id: Some(workflow_id.to_string()),
            workflow_params: params,
        };

        let run_id = Uuid::new_v4();
        let sid = req.session_id.unwrap_or_else(Uuid::new_v4);
        self.workflow_graphs.insert(run_id, graph);

        // We need to submit via the same path but with a pre-built graph
        // Re-implement submit_run inline to use the pre-stored graph
        self.memory.create_session(sid).await?;
        let mut record = RunRecord::new_queued(run_id, sid, req.task.clone(), req.profile);
        record
            .timeline
            .push(format!("{} run queued (workflow)", Utc::now().to_rfc3339()));
        self.runs.insert(run_id, record.clone());
        self.memory.upsert_run(&record).await?;
        self.controls.insert(
            run_id,
            RunControl {
                cancel_requested: Arc::new(AtomicBool::new(false)),
                pause_requested: Arc::new(AtomicBool::new(false)),
            },
        );
        let run_id_text = run_id.to_string();
        self.record_action_event(
            run_id,
            sid,
            RunActionType::RunQueued,
            Some("run"),
            Some(run_id_text.as_str()),
            None,
            serde_json::json!({
                "profile": req.profile,
                "task": req.task,
                "workflow_id": workflow_id,
            }),
        )
        .await;

        let this = self.clone();
        tokio::spawn(async move {
            if let Err(err) = this.execute_run(run_id, req).await {
                error!("run {run_id} execution crashed: {err}");
                let _ = this.mark_run_failed(run_id, err.to_string()).await;
            }
        });

        Ok(RunSubmission {
            run_id,
            session_id: sid,
            status: RunStatus::Queued,
        })
    }

    pub async fn list_mcp_tools(&self) -> Vec<McpToolDefinition> {
        self.mcp.list_all_tools().await
    }

    pub fn list_mcp_servers(&self) -> Vec<String> {
        self.mcp.server_names()
    }

    pub async fn call_mcp_tool(
        &self,
        name: &str,
        arguments: serde_json::Value,
    ) -> anyhow::Result<crate::types::McpToolCallResult> {
        self.mcp.call_tool(name, arguments).await
    }
}

fn build_trace_graph(run: &RunRecord, events: &[RunActionEvent]) -> RunTraceGraph {
    let mut nodes = HashMap::<String, NodeTraceState>::new();
    let mut edges = HashSet::<(String, String)>::new();
    let mut start_counts = HashMap::<String, u32>::new();

    for output in &run.outputs {
        nodes
            .entry(output.node_id.clone())
            .or_insert(NodeTraceState {
                node_id: output.node_id.clone(),
                role: Some(output.role),
                dependencies: Vec::new(),
                status: if output.succeeded {
                    "succeeded".to_string()
                } else {
                    "failed".to_string()
                },
                started_at: None,
                finished_at: None,
                duration_ms: Some(output.duration_ms),
                retries: 0,
                model: Some(output.model.clone()),
            });
    }

    for event in events {
        match event.action {
            RunActionType::GraphInitialized => {
                let Some(items) = event.payload.get("nodes").and_then(|v| v.as_array()) else {
                    continue;
                };
                for item in items {
                    let Some(node_id) = item.get("id").and_then(|v| v.as_str()) else {
                        continue;
                    };
                    let role = item
                        .get("role")
                        .and_then(|v| v.as_str())
                        .and_then(parse_agent_role);
                    let dependencies = item
                        .get("dependencies")
                        .and_then(|v| v.as_array())
                        .map(|arr| {
                            arr.iter()
                                .filter_map(|dep| dep.as_str().map(ToString::to_string))
                                .collect::<Vec<_>>()
                        })
                        .unwrap_or_default();

                    let node = nodes.entry(node_id.to_string()).or_insert(NodeTraceState {
                        node_id: node_id.to_string(),
                        role,
                        dependencies: dependencies.clone(),
                        status: "pending".to_string(),
                        started_at: None,
                        finished_at: None,
                        duration_ms: None,
                        retries: 0,
                        model: None,
                    });

                    if node.role.is_none() {
                        node.role = role;
                    }
                    if node.dependencies.is_empty() && !dependencies.is_empty() {
                        node.dependencies = dependencies.clone();
                    }
                    if node.status.is_empty() {
                        node.status = "pending".to_string();
                    }

                    for dep in dependencies {
                        edges.insert((dep, node_id.to_string()));
                    }
                }
            }
            RunActionType::NodeStarted => {
                if let Some(node_id) = event.payload.get("node_id").and_then(|v| v.as_str()) {
                    let role = event
                        .payload
                        .get("role")
                        .and_then(|v| v.as_str())
                        .and_then(parse_agent_role);
                    let node = nodes.entry(node_id.to_string()).or_insert(NodeTraceState {
                        node_id: node_id.to_string(),
                        role,
                        dependencies: Vec::new(),
                        status: "running".to_string(),
                        started_at: Some(event.timestamp),
                        finished_at: None,
                        duration_ms: None,
                        retries: 0,
                        model: None,
                    });

                    if node.started_at.is_none() {
                        node.started_at = Some(event.timestamp);
                    }
                    node.status = "running".to_string();
                    if node.role.is_none() {
                        node.role = role;
                    }
                    let starts = start_counts.entry(node_id.to_string()).or_insert(0);
                    *starts = starts.saturating_add(1);
                }
            }
            RunActionType::NodeCompleted => {
                if let Some(node_id) = event.payload.get("node_id").and_then(|v| v.as_str()) {
                    let role = event
                        .payload
                        .get("role")
                        .and_then(|v| v.as_str())
                        .and_then(parse_agent_role);
                    let node = nodes.entry(node_id.to_string()).or_insert(NodeTraceState {
                        node_id: node_id.to_string(),
                        role,
                        dependencies: Vec::new(),
                        status: "succeeded".to_string(),
                        started_at: None,
                        finished_at: Some(event.timestamp),
                        duration_ms: None,
                        retries: 0,
                        model: None,
                    });
                    node.status = "succeeded".to_string();
                    node.finished_at = Some(event.timestamp);
                    if node.role.is_none() {
                        node.role = role;
                    }
                }
            }
            RunActionType::NodeFailed => {
                if let Some(node_id) = event.payload.get("node_id").and_then(|v| v.as_str()) {
                    let role = event
                        .payload
                        .get("role")
                        .and_then(|v| v.as_str())
                        .and_then(parse_agent_role);
                    let node = nodes.entry(node_id.to_string()).or_insert(NodeTraceState {
                        node_id: node_id.to_string(),
                        role,
                        dependencies: Vec::new(),
                        status: "failed".to_string(),
                        started_at: None,
                        finished_at: Some(event.timestamp),
                        duration_ms: None,
                        retries: 0,
                        model: None,
                    });
                    node.status = "failed".to_string();
                    node.finished_at = Some(event.timestamp);
                    if node.role.is_none() {
                        node.role = role;
                    }
                }
            }
            RunActionType::NodeSkipped => {
                if let Some(node_id) = event.payload.get("node_id").and_then(|v| v.as_str()) {
                    let node = nodes.entry(node_id.to_string()).or_insert(NodeTraceState {
                        node_id: node_id.to_string(),
                        role: None,
                        dependencies: Vec::new(),
                        status: "skipped".to_string(),
                        started_at: None,
                        finished_at: Some(event.timestamp),
                        duration_ms: None,
                        retries: 0,
                        model: None,
                    });
                    node.status = "skipped".to_string();
                    node.finished_at = Some(event.timestamp);
                }
            }
            RunActionType::DynamicNodeAdded => {
                let Some(node_id) = event.payload.get("node_id").and_then(|v| v.as_str()) else {
                    continue;
                };
                let role = event
                    .payload
                    .get("role")
                    .and_then(|v| v.as_str())
                    .and_then(parse_agent_role);
                let dependencies = event
                    .payload
                    .get("dependencies")
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|dep| dep.as_str().map(ToString::to_string))
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();

                let node = nodes.entry(node_id.to_string()).or_insert(NodeTraceState {
                    node_id: node_id.to_string(),
                    role,
                    dependencies: dependencies.clone(),
                    status: "pending".to_string(),
                    started_at: None,
                    finished_at: None,
                    duration_ms: None,
                    retries: 0,
                    model: None,
                });
                if node.role.is_none() {
                    node.role = role;
                }
                if node.dependencies.is_empty() && !dependencies.is_empty() {
                    node.dependencies = dependencies.clone();
                }

                for dep in dependencies {
                    edges.insert((dep, node_id.to_string()));
                }
            }
            RunActionType::ModelSelected => {
                let node_id = event
                    .payload
                    .get("node_id")
                    .and_then(|v| v.as_str())
                    .or_else(|| event.actor_id.as_deref());
                if let Some(node_id) = node_id {
                    let model = event
                        .payload
                        .get("model")
                        .and_then(|v| v.as_str())
                        .map(ToString::to_string);
                    let role = event
                        .payload
                        .get("role")
                        .and_then(|v| v.as_str())
                        .and_then(parse_agent_role);

                    let node = nodes.entry(node_id.to_string()).or_insert(NodeTraceState {
                        node_id: node_id.to_string(),
                        role,
                        dependencies: Vec::new(),
                        status: "pending".to_string(),
                        started_at: None,
                        finished_at: None,
                        duration_ms: None,
                        retries: 0,
                        model: model.clone(),
                    });
                    node.model = model;
                    if node.role.is_none() {
                        node.role = role;
                    }
                }
            }
            RunActionType::RunQueued
            | RunActionType::RunStarted
            | RunActionType::RunCancelRequested
            | RunActionType::RunPauseRequested
            | RunActionType::RunResumed
            | RunActionType::GraphCompleted
            | RunActionType::RunFinished
            | RunActionType::WebhookDispatched
            | RunActionType::McpToolCalled
            | RunActionType::NodeTokenChunk
            | RunActionType::SubtaskPlanned
            | RunActionType::VerificationStarted
            | RunActionType::VerificationComplete
            | RunActionType::ReplanTriggered
            | RunActionType::TerminalSuggested => {}
        }
    }

    for output in &run.outputs {
        let node = nodes
            .entry(output.node_id.clone())
            .or_insert(NodeTraceState {
                node_id: output.node_id.clone(),
                role: Some(output.role),
                dependencies: Vec::new(),
                status: if output.succeeded {
                    "succeeded".to_string()
                } else {
                    "failed".to_string()
                },
                started_at: None,
                finished_at: None,
                duration_ms: Some(output.duration_ms),
                retries: 0,
                model: Some(output.model.clone()),
            });
        node.role = Some(output.role);
        node.model = Some(output.model.clone());
        node.duration_ms = Some(output.duration_ms);
        if output.succeeded {
            if node.status == "pending" || node.status == "running" {
                node.status = "succeeded".to_string();
            }
        } else {
            node.status = "failed".to_string();
        }
    }

    for (node_id, starts) in start_counts {
        if let Some(node) = nodes.get_mut(node_id.as_str()) {
            node.retries = starts.saturating_sub(1);
        }
    }

    let mut node_list = nodes.into_values().collect::<Vec<_>>();
    node_list.sort_by(|a, b| a.node_id.cmp(&b.node_id));

    let mut edge_list = edges
        .into_iter()
        .map(|(from, to)| TraceEdge { from, to })
        .collect::<Vec<_>>();
    edge_list
        .sort_by(|a, b| (a.from.as_str(), a.to.as_str()).cmp(&(b.from.as_str(), b.to.as_str())));

    let active_nodes = node_list
        .iter()
        .filter(|n| n.status == "running")
        .map(|n| n.node_id.clone())
        .collect::<Vec<_>>();
    let completed_nodes = node_list
        .iter()
        .filter(|n| n.status == "succeeded" || n.status == "skipped")
        .count();
    let failed_nodes = node_list.iter().filter(|n| n.status == "failed").count();

    RunTraceGraph {
        nodes: node_list,
        edges: edge_list,
        active_nodes,
        completed_nodes,
        failed_nodes,
    }
}

fn build_behavior_view(trace: &RunTrace) -> RunBehaviorView {
    let now = Utc::now();
    let mut window_start = trace.events.first().map(|e| e.timestamp);
    let mut window_end = trace.events.last().map(|e| e.timestamp);

    for node in &trace.graph.nodes {
        if let Some(started_at) = node.started_at {
            window_start = Some(match window_start {
                Some(current) => current.min(started_at),
                None => started_at,
            });
        }

        if let Some(estimated_end) = estimate_node_end(node, now) {
            window_end = Some(match window_end {
                Some(current) => current.max(estimated_end),
                None => estimated_end,
            });
        }
    }

    if let (Some(start), Some(end)) = (window_start, window_end) {
        if end < start {
            window_end = Some(start);
        }
    }

    let mut lanes = trace
        .graph
        .nodes
        .iter()
        .map(|node| {
            let start_offset_ms = match (window_start, node.started_at) {
                (Some(start), Some(node_start)) => Some(
                    node_start
                        .signed_duration_since(start)
                        .num_milliseconds()
                        .max(0),
                ),
                _ => None,
            };

            let end_offset_ms = match (window_start, estimate_node_end(node, now)) {
                (Some(start), Some(node_end)) => Some(
                    node_end
                        .signed_duration_since(start)
                        .num_milliseconds()
                        .max(0),
                ),
                _ => None,
            };

            let duration_ms = node
                .duration_ms
                .or_else(|| match (start_offset_ms, end_offset_ms) {
                    (Some(start_ms), Some(end_ms)) if end_ms >= start_ms => {
                        Some((end_ms - start_ms) as u128)
                    }
                    _ => None,
                });

            RunBehaviorLane {
                node_id: node.node_id.clone(),
                role: node.role,
                status: node.status.clone(),
                dependencies: node.dependencies.clone(),
                start_offset_ms,
                end_offset_ms,
                duration_ms,
                retries: node.retries,
                model: node.model.clone(),
            }
        })
        .collect::<Vec<_>>();

    lanes.sort_by(|a, b| {
        a.start_offset_ms
            .is_none()
            .cmp(&b.start_offset_ms.is_none())
            .then_with(|| {
                a.start_offset_ms
                    .unwrap_or(i64::MAX)
                    .cmp(&b.start_offset_ms.unwrap_or(i64::MAX))
            })
            .then_with(|| a.node_id.cmp(&b.node_id))
    });

    let mut action_counts = HashMap::<String, usize>::new();
    for event in &trace.events {
        let key = event.action.to_string();
        let next = action_counts.get(key.as_str()).copied().unwrap_or(0) + 1;
        action_counts.insert(key, next);
    }

    let mut action_mix = action_counts
        .into_iter()
        .map(|(action, count)| RunBehaviorActionCount { action, count })
        .collect::<Vec<_>>();
    action_mix.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.action.cmp(&b.action)));

    let total_duration_ms = match (window_start, window_end) {
        (Some(start), Some(end)) if end >= start => {
            Some(end.signed_duration_since(start).num_milliseconds().max(0) as u128)
        }
        _ => None,
    };
    let peak_parallelism = compute_peak_parallelism(lanes.as_slice());
    let (critical_path_nodes, critical_path_duration_ms) = compute_critical_path(lanes.as_slice());
    let (bottleneck_node_id, bottleneck_duration_ms) = compute_bottleneck(lanes.as_slice());

    RunBehaviorView {
        run_id: trace.run_id,
        session_id: trace.session_id,
        status: trace.status,
        window_start,
        window_end,
        active_nodes: trace.graph.active_nodes.clone(),
        lanes,
        action_mix,
        summary: RunBehaviorSummary {
            total_duration_ms,
            lane_count: trace.graph.nodes.len(),
            completed_nodes: trace.graph.completed_nodes,
            failed_nodes: trace.graph.failed_nodes,
            critical_path_nodes,
            critical_path_duration_ms,
            bottleneck_node_id,
            bottleneck_duration_ms,
            peak_parallelism,
        },
    }
}

fn compute_peak_parallelism(lanes: &[RunBehaviorLane]) -> usize {
    let mut points = Vec::<(i64, i32)>::new();

    for lane in lanes {
        let Some(start) = lane.start_offset_ms else {
            continue;
        };
        let end = lane
            .end_offset_ms
            .or_else(|| {
                lane.duration_ms
                    .map(|d| start.saturating_add(d.min(i64::MAX as u128) as i64))
            })
            .unwrap_or(start.saturating_add(1));
        let normalized_end = if end <= start {
            start.saturating_add(1)
        } else {
            end
        };

        points.push((start, 1));
        points.push((normalized_end, -1));
    }

    points.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));

    let mut current = 0_i32;
    let mut peak = 0_i32;
    for (_, delta) in points {
        current = (current + delta).max(0);
        peak = peak.max(current);
    }
    peak as usize
}

fn compute_critical_path(lanes: &[RunBehaviorLane]) -> (Vec<String>, u128) {
    if lanes.is_empty() {
        return (Vec::new(), 0);
    }

    let node_ids = lanes
        .iter()
        .map(|lane| lane.node_id.clone())
        .collect::<HashSet<_>>();
    let duration = lanes
        .iter()
        .map(|lane| (lane.node_id.clone(), lane.duration_ms.unwrap_or(0)))
        .collect::<HashMap<_, _>>();

    let mut indegree = lanes
        .iter()
        .map(|lane| (lane.node_id.clone(), 0_usize))
        .collect::<HashMap<_, _>>();
    let mut adjacency = HashMap::<String, Vec<String>>::new();

    for lane in lanes {
        for dep in &lane.dependencies {
            if !node_ids.contains(dep) {
                continue;
            }
            adjacency
                .entry(dep.clone())
                .or_default()
                .push(lane.node_id.clone());
            let next = indegree.get(lane.node_id.as_str()).copied().unwrap_or(0) + 1;
            indegree.insert(lane.node_id.clone(), next);
        }
    }

    for children in adjacency.values_mut() {
        children.sort();
        children.dedup();
    }

    let mut roots = indegree
        .iter()
        .filter_map(|(node, deg)| if *deg == 0 { Some(node.clone()) } else { None })
        .collect::<Vec<_>>();
    roots.sort();

    let mut queue = VecDeque::from(roots);
    let mut dist = duration.clone();
    let mut prev = HashMap::<String, String>::new();
    let mut visited = 0_usize;

    while let Some(node) = queue.pop_front() {
        visited = visited.saturating_add(1);
        let base = dist.get(node.as_str()).copied().unwrap_or(0);
        if let Some(children) = adjacency.get(node.as_str()) {
            for child in children {
                let child_weight = duration.get(child.as_str()).copied().unwrap_or(0);
                let candidate = base.saturating_add(child_weight);
                if candidate > dist.get(child.as_str()).copied().unwrap_or(0) {
                    dist.insert(child.clone(), candidate);
                    prev.insert(child.clone(), node.clone());
                }

                if let Some(entry) = indegree.get_mut(child.as_str()) {
                    *entry = entry.saturating_sub(1);
                    if *entry == 0 {
                        queue.push_back(child.clone());
                    }
                }
            }
        }
    }

    if visited != node_ids.len() {
        // Cycles should not happen for DAG runs; keep output stable with best-effort fallback.
        let mut longest = lanes
            .iter()
            .map(|lane| (lane.node_id.clone(), lane.duration_ms.unwrap_or(0)))
            .collect::<Vec<_>>();
        longest.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        if let Some((node, dur)) = longest.into_iter().next() {
            return (vec![node], dur);
        }
        return (Vec::new(), 0);
    }

    let mut best = dist.into_iter().collect::<Vec<_>>();
    best.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    let Some((tail, total)) = best.into_iter().next() else {
        return (Vec::new(), 0);
    };

    let mut path = Vec::<String>::new();
    let mut cursor = Some(tail);
    while let Some(node) = cursor {
        path.push(node.clone());
        cursor = prev.get(node.as_str()).cloned();
    }
    path.reverse();
    (path, total)
}

fn compute_bottleneck(lanes: &[RunBehaviorLane]) -> (Option<String>, Option<u128>) {
    let mut best_node: Option<String> = None;
    let mut best_dur: u128 = 0;
    let mut has_duration = false;

    for lane in lanes {
        let Some(dur) = lane.duration_ms else {
            continue;
        };
        has_duration = true;
        let replace = dur > best_dur
            || (dur == best_dur
                && best_node
                    .as_ref()
                    .map(|id| lane.node_id.as_str() < id.as_str())
                    .unwrap_or(true));
        if replace {
            best_node = Some(lane.node_id.clone());
            best_dur = dur;
        }
    }

    if has_duration {
        (best_node, Some(best_dur))
    } else {
        (None, None)
    }
}

fn estimate_node_end(
    node: &NodeTraceState,
    now: chrono::DateTime<Utc>,
) -> Option<chrono::DateTime<Utc>> {
    if let Some(finished_at) = node.finished_at {
        return Some(finished_at);
    }

    if let (Some(started_at), Some(duration_ms)) = (node.started_at, node.duration_ms) {
        let bounded = duration_ms.min(i64::MAX as u128) as i64;
        return Some(started_at + chrono::Duration::milliseconds(bounded));
    }

    if node.status == "running" {
        return node.started_at.or(Some(now));
    }

    None
}

fn parse_agent_role(value: &str) -> Option<AgentRole> {
    match value {
        "planner" => Some(AgentRole::Planner),
        "extractor" => Some(AgentRole::Extractor),
        "coder" => Some(AgentRole::Coder),
        "summarizer" => Some(AgentRole::Summarizer),
        "fallback" => Some(AgentRole::Fallback),
        "tool_caller" => Some(AgentRole::ToolCaller),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use chrono::Utc;

    use super::*;

    #[tokio::test]
    async fn graph_builder_respects_depth_and_dependencies() {
        let tmp = std::env::temp_dir().join(format!("agent-test-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&tmp).unwrap();

        let memory = Arc::new(
            MemoryManager::new(
                tmp.join("sessions"),
                format!("sqlite://{}", tmp.join("db.sqlite").display()).as_str(),
            )
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
            Arc::new(ModelRouter::new("http://127.0.0.1:8000", None, None, None)),
            memory,
            Arc::new(ContextManager::new(8_000)),
            webhook,
            6,
            Arc::new(McpRegistry::new()),
        );

        let no_servers: Vec<String> = vec![];
        let no_tools: Vec<String> = vec![];

        // Complex/CodeGeneration task should have full graph
        let graph = orchestrator
            .build_graph("implement a webhook integration endpoint with code", &no_servers, &no_tools)
            .unwrap();
        assert!(graph.node("plan").is_some());
        assert!(graph.node("code").is_some());
        assert!(graph.node("webhook_validation").is_some());

        // SimpleQuery should have minimal graph
        let simple = orchestrator.build_graph("hello world", &no_servers, &no_tools).unwrap();
        assert!(simple.node("plan").is_some());
        assert!(simple.node("summarize").is_some());
        assert!(simple.node("code").is_none());

        // Analysis task should have analyzer
        let analysis = orchestrator
            .build_graph("analyze the performance patterns and evaluate metrics", &no_servers, &no_tools)
            .unwrap();
        assert!(analysis.node("plan").is_some());
        assert!(analysis.node("analyze").is_some());
    }

    #[test]
    fn build_trace_graph_reconstructs_node_states() {
        let run_id = Uuid::new_v4();
        let session_id = Uuid::new_v4();
        let mut run = RunRecord::new_queued(
            run_id,
            session_id,
            "trace test".to_string(),
            crate::types::TaskProfile::General,
        );
        run.status = RunStatus::Succeeded;

        let events = vec![
            RunActionEvent {
                seq: 1,
                event_id: Uuid::new_v4().to_string(),
                run_id,
                session_id,
                timestamp: Utc::now(),
                action: RunActionType::GraphInitialized,
                actor_type: Some("orchestrator".to_string()),
                actor_id: Some("graph".to_string()),
                cause_event_id: None,
                payload: serde_json::json!({
                    "nodes": [
                        {"id":"plan","role":"planner","dependencies":[]},
                        {"id":"code","role":"coder","dependencies":["plan"]}
                    ]
                }),
            },
            RunActionEvent {
                seq: 2,
                event_id: Uuid::new_v4().to_string(),
                run_id,
                session_id,
                timestamp: Utc::now(),
                action: RunActionType::NodeStarted,
                actor_type: Some("runtime".to_string()),
                actor_id: Some("plan".to_string()),
                cause_event_id: None,
                payload: serde_json::json!({"node_id":"plan","role":"planner"}),
            },
            RunActionEvent {
                seq: 3,
                event_id: Uuid::new_v4().to_string(),
                run_id,
                session_id,
                timestamp: Utc::now(),
                action: RunActionType::NodeCompleted,
                actor_type: Some("runtime".to_string()),
                actor_id: Some("plan".to_string()),
                cause_event_id: None,
                payload: serde_json::json!({"node_id":"plan","role":"planner"}),
            },
            RunActionEvent {
                seq: 4,
                event_id: Uuid::new_v4().to_string(),
                run_id,
                session_id,
                timestamp: Utc::now(),
                action: RunActionType::NodeStarted,
                actor_type: Some("runtime".to_string()),
                actor_id: Some("code".to_string()),
                cause_event_id: None,
                payload: serde_json::json!({"node_id":"code","role":"coder"}),
            },
            RunActionEvent {
                seq: 5,
                event_id: Uuid::new_v4().to_string(),
                run_id,
                session_id,
                timestamp: Utc::now(),
                action: RunActionType::ModelSelected,
                actor_type: Some("node".to_string()),
                actor_id: Some("code".to_string()),
                cause_event_id: None,
                payload: serde_json::json!({"node_id":"code","role":"coder","model":"mock:model"}),
            },
            RunActionEvent {
                seq: 6,
                event_id: Uuid::new_v4().to_string(),
                run_id,
                session_id,
                timestamp: Utc::now(),
                action: RunActionType::NodeCompleted,
                actor_type: Some("runtime".to_string()),
                actor_id: Some("code".to_string()),
                cause_event_id: None,
                payload: serde_json::json!({"node_id":"code","role":"coder"}),
            },
        ];

        let graph = build_trace_graph(&run, events.as_slice());
        assert_eq!(graph.nodes.len(), 2);
        assert!(
            graph
                .edges
                .iter()
                .any(|e| e.from == "plan" && e.to == "code")
        );

        let code = graph
            .nodes
            .iter()
            .find(|n| n.node_id == "code")
            .expect("code node should exist");
        assert_eq!(code.status, "succeeded");
        assert_eq!(code.model.as_deref(), Some("mock:model"));
    }

    #[test]
    fn build_behavior_view_derives_lane_offsets_and_action_mix() {
        let run_id = Uuid::new_v4();
        let session_id = Uuid::new_v4();
        let mut run = RunRecord::new_queued(
            run_id,
            session_id,
            "behavior test".to_string(),
            crate::types::TaskProfile::General,
        );
        run.status = RunStatus::Failed;

        let t0 = Utc::now();
        let events = vec![
            RunActionEvent {
                seq: 1,
                event_id: Uuid::new_v4().to_string(),
                run_id,
                session_id,
                timestamp: t0,
                action: RunActionType::GraphInitialized,
                actor_type: Some("orchestrator".to_string()),
                actor_id: Some("graph".to_string()),
                cause_event_id: None,
                payload: serde_json::json!({
                    "nodes": [
                        {"id":"plan","role":"planner","dependencies":[]},
                        {"id":"code","role":"coder","dependencies":["plan"]}
                    ]
                }),
            },
            RunActionEvent {
                seq: 2,
                event_id: Uuid::new_v4().to_string(),
                run_id,
                session_id,
                timestamp: t0 + chrono::Duration::milliseconds(10),
                action: RunActionType::NodeStarted,
                actor_type: Some("runtime".to_string()),
                actor_id: Some("plan".to_string()),
                cause_event_id: None,
                payload: serde_json::json!({"node_id":"plan","role":"planner"}),
            },
            RunActionEvent {
                seq: 3,
                event_id: Uuid::new_v4().to_string(),
                run_id,
                session_id,
                timestamp: t0 + chrono::Duration::milliseconds(25),
                action: RunActionType::NodeCompleted,
                actor_type: Some("runtime".to_string()),
                actor_id: Some("plan".to_string()),
                cause_event_id: None,
                payload: serde_json::json!({"node_id":"plan","role":"planner"}),
            },
            RunActionEvent {
                seq: 4,
                event_id: Uuid::new_v4().to_string(),
                run_id,
                session_id,
                timestamp: t0 + chrono::Duration::milliseconds(30),
                action: RunActionType::NodeStarted,
                actor_type: Some("runtime".to_string()),
                actor_id: Some("code".to_string()),
                cause_event_id: None,
                payload: serde_json::json!({"node_id":"code","role":"coder"}),
            },
            RunActionEvent {
                seq: 5,
                event_id: Uuid::new_v4().to_string(),
                run_id,
                session_id,
                timestamp: t0 + chrono::Duration::milliseconds(50),
                action: RunActionType::NodeFailed,
                actor_type: Some("runtime".to_string()),
                actor_id: Some("code".to_string()),
                cause_event_id: None,
                payload: serde_json::json!({"node_id":"code","role":"coder","error":"boom"}),
            },
        ];

        let trace = RunTrace {
            run_id,
            session_id,
            status: Some(RunStatus::Failed),
            events: events.clone(),
            graph: build_trace_graph(&run, events.as_slice()),
        };

        let view = build_behavior_view(&trace);
        assert_eq!(view.run_id, run_id);
        assert_eq!(view.session_id, session_id);
        assert_eq!(view.lanes.len(), 2);
        assert!(view.window_start.is_some());
        assert!(view.window_end.is_some());

        let plan_lane = view
            .lanes
            .iter()
            .find(|l| l.node_id == "plan")
            .expect("plan lane should exist");
        let code_lane = view
            .lanes
            .iter()
            .find(|l| l.node_id == "code")
            .expect("code lane should exist");

        assert!(
            plan_lane.start_offset_ms.unwrap_or_default()
                <= code_lane.start_offset_ms.unwrap_or_default()
        );
        assert!(
            code_lane.end_offset_ms.unwrap_or_default()
                >= code_lane.start_offset_ms.unwrap_or_default()
        );

        let started_count = view
            .action_mix
            .iter()
            .find(|item| item.action == "node_started")
            .map(|item| item.count)
            .unwrap_or_default();
        let failed_count = view
            .action_mix
            .iter()
            .find(|item| item.action == "node_failed")
            .map(|item| item.count)
            .unwrap_or_default();

        assert_eq!(started_count, 2);
        assert_eq!(failed_count, 1);
        assert_eq!(view.summary.total_duration_ms, Some(50));
        assert_eq!(
            view.summary.critical_path_nodes,
            vec!["plan".to_string(), "code".to_string()]
        );
        assert_eq!(view.summary.critical_path_duration_ms, 35);
        assert_eq!(view.summary.bottleneck_node_id.as_deref(), Some("code"));
        assert_eq!(view.summary.bottleneck_duration_ms, Some(20));
        assert_eq!(view.summary.peak_parallelism, 1);
    }

    #[test]
    fn compute_peak_parallelism_counts_overlap() {
        let lanes = vec![
            RunBehaviorLane {
                node_id: "a".to_string(),
                role: None,
                status: "succeeded".to_string(),
                dependencies: vec![],
                start_offset_ms: Some(0),
                end_offset_ms: Some(20),
                duration_ms: Some(20),
                retries: 0,
                model: None,
            },
            RunBehaviorLane {
                node_id: "b".to_string(),
                role: None,
                status: "succeeded".to_string(),
                dependencies: vec![],
                start_offset_ms: Some(5),
                end_offset_ms: Some(25),
                duration_ms: Some(20),
                retries: 0,
                model: None,
            },
            RunBehaviorLane {
                node_id: "c".to_string(),
                role: None,
                status: "succeeded".to_string(),
                dependencies: vec![],
                start_offset_ms: Some(10),
                end_offset_ms: Some(30),
                duration_ms: Some(20),
                retries: 0,
                model: None,
            },
        ];

        assert_eq!(compute_peak_parallelism(lanes.as_slice()), 3);
    }
}
