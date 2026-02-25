pub mod coder_backend;
pub mod prompt_composer;

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use chrono::Utc;
use dashmap::DashMap;
use futures::FutureExt;
use tokio::sync::mpsc;
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
    AgentExecutionRecord, AgentRole, ChatMessage, ChatRole, KnowledgeItem, McpToolDefinition,
    NodeTraceState, RunActionEvent, RunActionType, RunBehaviorActionCount, RunBehaviorLane,
    RunBehaviorSummary, RunBehaviorView, RunRecord, RunRequest, RunStatus, RunSubmission, RunTrace,
    RunTraceGraph, SessionEvent, SessionEventType, SessionMemoryItem, SubtaskPlan, TaskType,
    TraceEdge, WebhookDeliveryRecord, WebhookEndpoint, WorkflowTemplate,
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
    coder_manager: Arc<coder_backend::CoderSessionManager>,
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
        coder_manager: Arc<coder_backend::CoderSessionManager>,
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
            coder_manager,
        }
    }

    pub fn router(&self) -> &Arc<ModelRouter> {
        &self.router
    }

    pub async fn list_coder_sessions(
        &self,
        run_id: Uuid,
    ) -> anyhow::Result<Vec<serde_json::Value>> {
        self.coder_manager.list_sessions_for_run(run_id).await
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
                if let Ok(pk) =
                    serde_json::from_value::<ProviderKind>(serde_json::Value::String(name.clone()))
                {
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

    pub async fn list_session_memory_items(
        &self,
        session_id: Uuid,
        query_text: Option<&str>,
        limit: usize,
    ) -> anyhow::Result<Vec<SessionMemoryItem>> {
        self.memory
            .list_session_memory_items(session_id, query_text, limit)
            .await
    }

    pub async fn add_session_memory_item(
        &self,
        session_id: Uuid,
        scope: &str,
        content: &str,
        importance: f64,
        source_ref: Option<&str>,
    ) -> anyhow::Result<String> {
        self.memory
            .remember_long(session_id, scope, content, importance, source_ref)
            .await
    }

    pub async fn update_session_memory_item(
        &self,
        memory_id: &str,
        content: Option<&str>,
        importance: Option<f64>,
        scope: Option<&str>,
    ) -> anyhow::Result<bool> {
        self.memory
            .update_session_memory_item(memory_id, content, importance, scope)
            .await
    }

    pub async fn delete_session_memory_item(&self, memory_id: &str) -> anyhow::Result<bool> {
        self.memory.delete_session_memory_item(memory_id).await
    }

    pub async fn list_global_memory_items(
        &self,
        query_text: Option<&str>,
        limit: usize,
    ) -> anyhow::Result<Vec<KnowledgeItem>> {
        self.memory.list_knowledge_items(query_text, limit).await
    }

    pub async fn add_global_memory_item(
        &self,
        topic: &str,
        content: &str,
        importance: f64,
    ) -> anyhow::Result<String> {
        self.memory
            .insert_knowledge(topic, content, importance)
            .await
    }

    pub async fn update_global_memory_item(
        &self,
        knowledge_id: &str,
        topic: Option<&str>,
        content: Option<&str>,
        importance: Option<f64>,
    ) -> anyhow::Result<bool> {
        self.memory
            .update_knowledge_item(knowledge_id, topic, content, importance)
            .await
    }

    pub async fn submit_run(&self, req: RunRequest) -> anyhow::Result<RunSubmission> {
        let run_id = Uuid::new_v4();
        let session_id = req.session_id.unwrap_or_else(Uuid::new_v4);
        let mut req = req;
        req.session_id = Some(session_id);

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
                    let ts = run.finished_at.unwrap_or(run.created_at);
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
        let mcp_tool_names: Vec<String> = self
            .mcp
            .list_all_tools()
            .await
            .into_iter()
            .map(|t| t.name)
            .collect();
        // Resolve previous run context for continuation detection
        let (previous_task_type, previous_plan) = if Self::is_continuation_command(req.task.as_str()) {
            let session_runs = self
                .memory
                .list_session_runs(session_id, 5)
                .await
                .unwrap_or_default();
            match session_runs
                .iter()
                .find(|r| r.run_id != run_id && r.status == RunStatus::Succeeded)
            {
                Some(prev) => {
                    let prev_type = Self::classify_task_fallback(
                        prev.task.as_str(),
                        !mcp_server_names.is_empty(),
                    );
                    let prev_plan = prev
                        .outputs
                        .iter()
                        .find(|o| o.role == AgentRole::Planner && o.succeeded)
                        .map(|o| o.output.clone());
                    (Some(prev_type), prev_plan)
                }
                None => (None, None),
            }
        } else {
            (None, None)
        };

        let graph = match self.workflow_graphs.remove(&run_id).map(|(_, g)| g) {
            Some(g) => g,
            None => self
                .build_graph(
                    req.task.as_str(),
                    &mcp_server_names,
                    &mcp_tool_names,
                    previous_task_type,
                    previous_plan,
                )
                .await?,
        };
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
                if cancel_flag.load(Ordering::Relaxed) {
                    self.finish_run(run_id, RunStatus::Cancelled, node_results, None)
                        .await?;
                } else if node_results.iter().all(|r| r.succeeded) {
                    let (verified, reason) = self
                        .verify_completion(run_id, session_id, &req.task, &node_results)
                        .await;
                    let error_message = if verified {
                        None
                    } else {
                        Some(format!("Verification incomplete: {}", reason))
                    };
                    self.finish_run(run_id, RunStatus::Succeeded, node_results, error_message)
                        .await?;
                } else {
                    self.finish_run(run_id, RunStatus::Failed, node_results, None)
                        .await?;
                }
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
    ) -> (bool, String) {
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
            .map(|r| {
                format!(
                    "[{}] {}",
                    r.node_id,
                    r.output.chars().take(500).collect::<String>()
                )
            })
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

        (is_complete, reason)
    }

    fn has_explicit_remote_repo_reference(task: &str) -> bool {
        let lower = task.to_lowercase();
        let remote_markers = [
            "github",
            "gitlab",
            "bitbucket",
            "http://",
            "https://",
            "owner/repo",
            "repository url",
            "remote repo",
        ];
        remote_markers.iter().any(|kw| lower.contains(kw))
    }

    fn is_local_workspace_task(task: &str) -> bool {
        let lower = task.to_lowercase();
        let local_markers = [
            "로컬",
            "local",
            "workspace",
            "현재 폴더",
            "현재 디렉토리",
            "this project",
            "this repo",
            "프로젝트 폴더",
            "경로",
            "파일",
            "readme.md",
            "current directory",
            "current folder",
            "my project",
            "my repo",
            "working directory",
            "작성",
            "수정",
            "편집",
            "커밋",
        ];

        let has_path_like = lower.contains('/')
            && !lower.contains("http://")
            && !lower.contains("https://")
            && !lower.contains("github.com");

        (local_markers.iter().any(|kw| lower.contains(kw)) || has_path_like)
            && !Self::has_explicit_remote_repo_reference(lower.as_str())
    }

    fn looks_like_follow_up_task(task: &str) -> bool {
        let lower = task.trim().to_lowercase();
        let token_count = lower.split_whitespace().count();
        if token_count <= 5 {
            return true;
        }

        let follow_up_markers = [
            "그거",
            "이거",
            "저거",
            "거기",
            "로컬에",
            "local",
            "that",
            "it",
            "there",
            "맞아",
            "응",
            "계속",
            "이어서",
        ];
        follow_up_markers.iter().any(|kw| lower.contains(kw))
    }

    /// Detects explicit continuation/execution commands that should inherit
    /// the previous run's TaskType rather than being classified independently.
    fn is_continuation_command(task: &str) -> bool {
        let lower = task.trim().to_lowercase();
        // Must be short (no substantive new task content)
        if lower.split_whitespace().count() > 6 {
            return false;
        }
        let continuation_markers = [
            "실행해",
            "진행해",
            "계속해",
            "이어서",
            "다음 작업",
            "go ahead",
            "do it",
            "execute",
            "proceed",
            "continue",
            "run it",
            "carry on",
        ];
        continuation_markers.iter().any(|kw| lower.contains(kw))
    }

    fn build_memory_query(
        current_task: &str,
        recent_messages: &[(i64, String, String, String)],
    ) -> String {
        let current = current_task.trim();
        if current.is_empty() {
            return String::new();
        }

        if !Self::looks_like_follow_up_task(current) {
            return current.to_string();
        }

        let previous_user_message =
            recent_messages
                .iter()
                .rev()
                .find_map(|(_, role, content, _)| {
                    if role == "user" && content.trim() != current {
                        Some(content.trim().to_string())
                    } else {
                        None
                    }
                });

        match previous_user_message {
            Some(prev) if !prev.is_empty() => format!("{current} {prev}"),
            _ => current.to_string(),
        }
    }

    fn build_recent_history_chunks(
        recent_messages: &[(i64, String, String, String)],
        current_task: &str,
    ) -> Vec<ContextChunk> {
        let current = current_task.trim();
        let mut selected = Vec::new();

        for (_, role, content, _) in recent_messages.iter().rev() {
            if role != "user" {
                continue;
            }
            let trimmed = content.trim();
            if trimmed.is_empty() || trimmed == current {
                continue;
            }
            selected.push(trimmed.to_string());
            if selected.len() >= 4 {
                break;
            }
        }

        selected.reverse();
        selected
            .into_iter()
            .enumerate()
            .map(|(idx, content)| ContextChunk {
                id: format!("history-user-{idx}"),
                scope: ContextScope::SessionShared,
                kind: ContextKind::History,
                content: format!("Previous user message: {content}"),
                priority: 0.75 - (idx as f64 * 0.03),
            })
            .collect()
    }

    fn build_recent_run_summary(current_run_id: Uuid, runs: &[RunRecord]) -> Option<String> {
        let previous = runs
            .iter()
            .find(|run| run.run_id != current_run_id && !run.outputs.is_empty())?;

        let primary_output = previous
            .outputs
            .iter()
            .find(|o| o.role == AgentRole::Summarizer && !o.output.trim().is_empty())
            .or_else(|| {
                previous
                    .outputs
                    .iter()
                    .find(|o| !o.output.trim().is_empty())
            })?;

        Some(format!(
            "Previous run task: {}\nPrevious run outcome: {}",
            Self::trim_for_context(previous.task.as_str(), 220),
            Self::trim_for_context(primary_output.output.as_str(), 420)
        ))
    }

    fn trim_for_context(text: &str, max_chars: usize) -> String {
        let normalized = text.trim();
        if normalized.chars().count() <= max_chars {
            return normalized.to_string();
        }

        let head_len = max_chars / 2;
        let tail_len = max_chars.saturating_sub(head_len);
        let head = normalized.chars().take(head_len).collect::<String>();
        let tail = normalized
            .chars()
            .rev()
            .take(tail_len)
            .collect::<String>()
            .chars()
            .rev()
            .collect::<String>();
        format!("{head}\n... [trimmed] ...\n{tail}")
    }

    /// Strip markdown fences and try to extract text content from JSON wrappers.
    fn clean_llm_output(raw: &str) -> String {
        let mut text = raw.trim().to_string();

        // Strip markdown code fences
        if text.starts_with("```") {
            // Remove opening fence (with optional language tag)
            if let Some(end_of_first_line) = text.find('\n') {
                text = text[end_of_first_line + 1..].to_string();
            }
            if text.ends_with("```") {
                text = text[..text.len() - 3].to_string();
            }
            text = text.trim().to_string();
        }

        // If the output looks like a JSON object with common LLM response fields, extract content
        if text.starts_with('{') && text.ends_with('}') {
            if let Ok(obj) = serde_json::from_str::<serde_json::Value>(&text) {
                // Try common content field names
                for key in ["content", "answer", "response", "summary", "text", "message"] {
                    if let Some(val) = obj.get(key).and_then(|v| v.as_str()) {
                        return val.trim().to_string();
                    }
                }
            }
        }

        text
    }

    async fn classify_task(
        task: &str,
        mcp_server_names: &[String],
        mcp_tool_names: &[String],
        previous_task_type: Option<TaskType>,
        router: &ModelRouter,
    ) -> TaskType {
        // If this is a continuation command and we have session context, inherit
        if Self::is_continuation_command(task) {
            if let Some(prev_type) = previous_task_type {
                return prev_type;
            }
        }

        let has_mcp = !mcp_server_names.is_empty();
        let lower = task.to_lowercase();

        // Quick check: if MCP tools are available and task explicitly mentions a tool/server name
        if has_mcp {
            let matches_server = mcp_server_names
                .iter()
                .any(|name| lower.contains(&name.to_lowercase()));
            let matches_tool = mcp_tool_names.iter().any(|name| {
                let bare = name.rsplit('/').next().unwrap_or(name);
                lower.contains(&bare.to_lowercase())
            });
            if matches_server || matches_tool {
                return TaskType::ToolOperation;
            }
        }

        // LLM-based classification
        let tool_context = if has_mcp {
            format!(
                "\nAvailable MCP tools: [{}]",
                mcp_tool_names.iter().take(15).cloned().collect::<Vec<_>>().join(", ")
            )
        } else {
            String::new()
        };

        let classification_prompt = format!(
            "Classify the following user task into exactly ONE type.\n\n\
             Task types:\n\
             - simple_query: Questions or lookups that need no external action (e.g. greetings, factual Q&A, explanations)\n\
             - analysis: Tasks requiring data extraction, pattern analysis, or evaluation\n\
             - code_generation: Tasks involving writing, fixing, refactoring, or debugging code\n\
             - configuration: Tasks that CHANGE system settings (enable/disable features, switch models, update preferences)\n\
             - config_query: Tasks that ASK ABOUT current system state or settings without changing them (e.g. \"what backend is active?\", \"show current model\")\n\
             - tool_operation: Tasks requiring external tool calls (file I/O, API calls, MCP tools){tool_ctx}\n\
             - complex: Multi-step tasks spanning multiple categories\n\n\
             User task: \"{task}\"\n\n\
             Respond with ONLY the type name, nothing else.",
            tool_ctx = tool_context,
            task = task,
        );

        let constraints = crate::router::RoutingConstraints {
            quality_weight: 0.3,
            latency_budget_ms: 3_000,
            cost_budget: 0.01,
            min_context: 1_000,
            tool_call_weight: 0.0,
        };

        match router
            .infer(
                crate::types::TaskProfile::General,
                &classification_prompt,
                &constraints,
            )
            .await
        {
            Ok((_decision, result)) => {
                let output = result.output.trim().to_lowercase();
                match output.as_str() {
                    "simple_query" => TaskType::SimpleQuery,
                    "analysis" => TaskType::Analysis,
                    "code_generation" => TaskType::CodeGeneration,
                    "configuration" => TaskType::Configuration,
                    "config_query" => TaskType::ConfigQuery,
                    "tool_operation" => TaskType::ToolOperation,
                    "complex" => TaskType::Complex,
                    _ => Self::classify_task_fallback(task, has_mcp),
                }
            }
            Err(_) => Self::classify_task_fallback(task, has_mcp),
        }
    }

    /// Fast keyword-based fallback when LLM classification is unavailable.
    fn classify_task_fallback(task: &str, has_mcp: bool) -> TaskType {
        let lower = task.to_lowercase();

        let config_kw = ["setting", "config", "model", "provider", "backend"];
        if config_kw.iter().any(|kw| lower.contains(kw)) {
            return TaskType::Configuration;
        }
        if has_mcp {
            let tool_kw = ["mcp", "file", "repo", "commit", "branch", "search"];
            if tool_kw.iter().any(|kw| lower.contains(kw)) {
                return TaskType::ToolOperation;
            }
        }
        let code_kw = ["code", "implement", "function", "refactor", "bug", "fix", "debug"];
        if code_kw.iter().any(|kw| lower.contains(kw)) {
            return TaskType::CodeGeneration;
        }
        let analysis_kw = ["analyze", "analysis", "pattern", "compare", "evaluate"];
        if analysis_kw.iter().any(|kw| lower.contains(kw)) {
            return TaskType::Analysis;
        }
        if lower.split_whitespace().count() <= 8 {
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

    async fn build_system_state_snapshot(&self) -> String {
        let settings = self.get_settings();
        let coder_backend = self.coder_manager.default_backend_kind();
        let mcp_servers = self.mcp.server_descriptions();

        let persisted = self.memory.store().load_settings().await.ok().flatten();
        let (term_cmd, term_args, term_auto) = match persisted {
            Some(ref s) => (s.terminal_command.as_str(), &s.terminal_args, s.terminal_auto_spawn),
            None => (settings.terminal_command.as_str(), &settings.terminal_args, settings.terminal_auto_spawn),
        };

        let mcp_section = if mcp_servers.is_empty() {
            "  MCP servers: (none)".to_string()
        } else {
            let lines: Vec<String> = mcp_servers
                .iter()
                .map(|(name, desc)| format!("    - {}: {}", name, desc))
                .collect();
            format!("  MCP servers:\n{}", lines.join("\n"))
        };

        format!(
            "- Preferred model: {}\n\
             - Disabled models: [{}]\n\
             - Disabled providers: [{}]\n\
             - Coder backend: {}\n\
             - Terminal command: {} {}\n\
             - Terminal auto-spawn: {}\n\
             {}",
            settings.preferred_model.as_deref().unwrap_or("(auto)"),
            settings.disabled_models.join(", "),
            settings.disabled_providers.join(", "),
            coder_backend,
            term_cmd,
            term_args.join(" "),
            term_auto,
            mcp_section,
        )
    }

    async fn build_graph(
        &self,
        task: &str,
        mcp_server_names: &[String],
        mcp_tool_names: &[String],
        previous_task_type: Option<TaskType>,
        previous_plan: Option<String>,
    ) -> anyhow::Result<ExecutionGraph> {
        let task_type = Self::classify_task(
            task, mcp_server_names, mcp_tool_names, previous_task_type, &self.router,
        ).await;
        let mut graph = ExecutionGraph::new(self.max_graph_depth);

        // Planner instructions — include available tools when MCP servers are registered
        let planner_instructions = if !mcp_tool_names.is_empty() {
            let tool_summary = mcp_tool_names
                .iter()
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
            let local_first_hint = if Self::is_local_workspace_task(task) {
                "\n\nSTRICT LOCAL-FIRST POLICY:\n\
                 - You MUST plan filesystem/* tools for any task involving local files, directories, or project workspace contents.\n\
                 - NEVER plan github/* or remote API calls for reading/writing local files.\n\
                 - Only plan github/* when the user explicitly asks for remote repository operations (e.g., creating PRs, checking remote issues).\n\
                 - For file edits and commits on local projects, use filesystem/* tools (read, write, edit)."
            } else {
                ""
            };
            format!(
                "Plan work and constraints. Available MCP tools: [{}]. \
                 If the task requires external data or operations, plan to use these tools via the tool_call node.{}{}",
                tool_summary, guide, local_first_hint
            )
        } else {
            "Plan work and constraints.".to_string()
        };

        // Inject previous plan context for continuation commands
        let planner_instructions = if let Some(ref prev_plan) = previous_plan {
            format!(
                "{}\n\nPREVIOUS PLAN (from prior run in this session — continue execution from here):\n{}",
                planner_instructions,
                Self::trim_for_context(prev_plan, 800)
            )
        } else {
            planner_instructions
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
            TaskType::ConfigQuery => {
                // Read-only config query: plan → summarize with system state
                let state_snapshot = self.build_system_state_snapshot().await;
                let mut summarize = AgentNode::new(
                    "summarize",
                    AgentRole::Summarizer,
                    format!(
                        "Answer the user's question about system configuration using the information below.\n\n\
                         CURRENT SYSTEM STATE:\n{}\n\n\
                         Provide a clear, human-readable answer. Do not output raw JSON.",
                        state_snapshot
                    ),
                );
                summarize.dependencies = vec!["plan".to_string()];
                summarize.policy = Self::default_policy();
                graph.add_node(summarize)?;
            }
            TaskType::Configuration => {
                // Mutation: plan → config_manage → summarize with system state context
                let state_snapshot = self.build_system_state_snapshot().await;
                let mut config = AgentNode::new(
                    "config_manage",
                    AgentRole::ConfigManager,
                    format!(
                        "Execute the configuration changes requested by the user.\n\n\
                         CURRENT SYSTEM STATE (before changes):\n{}",
                        state_snapshot
                    ),
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
                summarize.dependencies = vec!["code".to_string(), "fallback_code".to_string()];
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
        let coder_manager = self.coder_manager.clone();

        Arc::new(move |node: AgentNode, deps: Vec<NodeExecutionResult>| {
            let agents = agents.clone();
            let router = router.clone();
            let memory = memory.clone();
            let context = context.clone();
            let req = req.clone();
            let mcp = mcp.clone();
            let event_sink = event_sink.clone();
            let coder_manager = coder_manager.clone();

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
                    let local_first_hint = if Self::is_local_workspace_task(req.task.as_str()) {
                        "\n\nSTRICT LOCAL-WORKSPACE RULE:\n\
                         - You MUST select filesystem/* tools for any operation on local files or directories.\n\
                         - NEVER select github/* tools for reading or modifying local project files.\n\
                         - github/* tools are ONLY for explicitly remote operations (PRs, issues, remote repo metadata).\n\
                         - If the planner mentions local file paths, you MUST use filesystem/* tools regardless of what other tools are available."
                    } else {
                        ""
                    };

                    // Phase B+C: Iterative tool calling loop
                    const MAX_TOOL_ITERATIONS: usize = 5;
                    let mut all_results: Vec<String> = Vec::new();
                    let mut total_tool_calls: usize = 0;
                    let mut any_failure = false;

                    for iteration in 0..MAX_TOOL_ITERATIONS {
                        let iteration_context = if all_results.is_empty() {
                            String::new()
                        } else {
                            format!(
                                "\n\nPREVIOUS TOOL RESULTS (iterations 0..{}):\n{}",
                                iteration - 1,
                                all_results.join("\n---\n")
                            )
                        };

                        let selection_prompt = format!(
                            "You are the tool caller agent.\n\n\
                             USER TASK:\n{}\n\n\
                             PLANNER OUTPUT:\n{}\n\n\
                             AVAILABLE MCP TOOLS:\n{}{}{}{}\n\n\
                             INSTRUCTIONS:\n\
                             - Select the NEXT tool call(s) needed to complete the task.\n\
                             - You may use results from previous iterations to inform arguments.\n\
                             - If the task requires sequential steps (e.g., read a value, then use it), \
                               select only the NEXT step's tool(s) in this iteration.\n\
                             - When ALL steps are complete and no more tools are needed, respond with exactly: DONE\n\n\
                             Respond ONLY with a JSON array of tool calls, OR the word DONE.\n\
                             Each element: {{\"tool_name\": \"server/tool_name\", \"arguments\": {{...}}}}",
                            req.task,
                            dep_outputs.join("\n---\n"),
                            tool_list_str,
                            server_guide,
                            local_first_hint,
                            iteration_context,
                        );

                        let optimized = context.optimize(vec![]);
                        let brief = context.build_structured_brief(
                            req.task.clone(),
                            vec!["select tools".to_string()],
                            vec![format!("node={}", node.id), format!("iteration={iteration}")],
                            vec![],
                        );

                        let selection_input = AgentInput {
                            task: req.task.clone(),
                            instructions: selection_prompt,
                            context: optimized,
                            dependency_outputs: dep_outputs.clone(),
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
                                    output: all_results.join("\n---\n"),
                                    duration_ms: started.elapsed().as_millis(),
                                    succeeded: false,
                                    error: Some(format!(
                                        "LLM tool selection failed at iteration {iteration}: {err}"
                                    )),
                                });
                            }
                        };

                        // Check for DONE signal
                        let trimmed_output = llm_output.trim();
                        if trimmed_output.eq_ignore_ascii_case("done")
                            || trimmed_output == "\"DONE\""
                            || trimmed_output == "\"done\""
                        {
                            break;
                        }

                        // Parse LLM response (strip markdown fences)
                        let json_str = trimmed_output
                            .strip_prefix("```json")
                            .or_else(|| trimmed_output.strip_prefix("```"))
                            .unwrap_or(trimmed_output)
                            .strip_suffix("```")
                            .unwrap_or(trimmed_output)
                            .trim();

                        let tool_calls = parse_tool_calls(json_str);

                        if tool_calls.is_empty() {
                            // If we have prior results, treat as implicit completion
                            if !all_results.is_empty() {
                                break;
                            }
                            // No prior results — this is a failure
                            return Ok(NodeExecutionResult {
                                node_id: node.id,
                                role: node.role,
                                model: "mcp:none".to_string(),
                                output: format!(
                                    "No executable tool calls were parsed.\nRaw selector output:\n{}",
                                    llm_output
                                ),
                                duration_ms: started.elapsed().as_millis(),
                                succeeded: false,
                                error: Some("No executable tool calls were parsed from LLM output".to_string()),
                            });
                        }

                        // Execute tool calls for this iteration
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
                                                "iteration": iteration,
                                            }),
                                        )
                                        .await;
                                    if !result.succeeded {
                                        any_failure = true;
                                    }
                                    all_results.push(format!(
                                        "[iter={}] [{}] {}: {}",
                                        iteration,
                                        if result.succeeded { "OK" } else { "ERR" },
                                        tool_name,
                                        result.content
                                    ));
                                }
                                Err(err) => {
                                    any_failure = true;
                                    all_results.push(format!(
                                        "[iter={}] [ERR] {}: {}",
                                        iteration, tool_name, err
                                    ));
                                }
                            }
                            total_tool_calls += 1;
                        }
                    }

                    // Final result assembly
                    let all_succeeded = !any_failure && total_tool_calls > 0;
                    let combined_output = all_results.join("\n---\n");

                    return Ok(NodeExecutionResult {
                        node_id: node.id,
                        role: node.role,
                        model: format!("mcp:{}", total_tool_calls),
                        output: combined_output,
                        duration_ms: started.elapsed().as_millis(),
                        succeeded: all_succeeded,
                        error: if all_succeeded {
                            None
                        } else if total_tool_calls == 0 {
                            Some("No tool calls were executed across all iterations".to_string())
                        } else {
                            Some("One or more tool calls failed".to_string())
                        },
                    });
                }

                // CLI coder path: delegate to CoderSessionManager for non-LLM backends
                if node.role == AgentRole::Coder {
                    let backend_kind = coder_manager.default_backend_kind();
                    if backend_kind != crate::types::CoderBackendKind::Llm {
                        let node_id_for_chunk = node.id.clone();
                        let event_sink_for_chunk = event_sink.clone();
                        let chunk_callback: Arc<dyn Fn(crate::types::CoderOutputChunk) + Send + Sync> =
                            Arc::new(move |chunk: crate::types::CoderOutputChunk| {
                                event_sink_for_chunk(RuntimeEvent::CoderOutputChunk {
                                    node_id: node_id_for_chunk.clone(),
                                    session_id: chunk.session_id.clone(),
                                    stream: chunk.stream.clone(),
                                    content: chunk.content.clone(),
                                });
                            });

                        event_sink(RuntimeEvent::CoderSessionStarted {
                            node_id: node.id.clone(),
                            session_id: String::new(),
                            backend: backend_kind.to_string(),
                        });

                        let context_str = deps
                            .iter()
                            .map(|d| format!("{}: {}", d.node_id, d.output))
                            .collect::<Vec<_>>()
                            .join("\n---\n");

                        let result = coder_manager
                            .run_session(
                                run_id,
                                &node.id,
                                backend_kind,
                                &req.task,
                                &context_str,
                                chunk_callback,
                            )
                            .await;

                        return match result {
                            Ok(session_result) => {
                                event_sink(RuntimeEvent::CoderSessionCompleted {
                                    node_id: node.id.clone(),
                                    session_id: String::new(),
                                    files_changed: session_result.files_changed.clone(),
                                    exit_code: session_result.exit_code,
                                });

                                Ok(NodeExecutionResult {
                                    node_id: node.id,
                                    role: node.role,
                                    model: format!("coder:{}", backend_kind),
                                    output: session_result.output,
                                    duration_ms: session_result.duration_ms,
                                    succeeded: session_result.exit_code == 0,
                                    error: if session_result.exit_code != 0 {
                                        Some(format!(
                                            "coder exited with code {}",
                                            session_result.exit_code
                                        ))
                                    } else {
                                        None
                                    },
                                })
                            }
                            Err(err) => Ok(NodeExecutionResult {
                                node_id: node.id,
                                role: node.role,
                                model: format!("coder:{}", backend_kind),
                                output: String::new(),
                                duration_ms: started.elapsed().as_millis(),
                                succeeded: false,
                                error: Some(err.to_string()),
                            }),
                        };
                    }
                }

                let dep_outputs = deps
                    .iter()
                    .map(|d| format!("{}: {}", d.node_id, d.output))
                    .collect::<Vec<_>>();

                let recent_messages = memory
                    .list_session_messages(session_id, 10)
                    .await
                    .unwrap_or_default();
                let mut recent_history_chunks =
                    Self::build_recent_history_chunks(&recent_messages, req.task.as_str());
                let memory_query = Self::build_memory_query(req.task.as_str(), &recent_messages);
                let memory_hits = memory
                    .retrieve(session_id, memory_query.as_str(), 8)
                    .await
                    .unwrap_or_default();
                let global_hits = memory
                    .search_knowledge(req.task.as_str(), 4)
                    .await
                    .unwrap_or_default();
                let recent_runs = memory.list_session_runs(session_id, 4).await.unwrap_or_default();
                let recent_run_summary = Self::build_recent_run_summary(run_id, &recent_runs);
                let local_workspace_task = Self::is_local_workspace_task(req.task.as_str());

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

                if local_workspace_task {
                    chunks.push(ContextChunk {
                        id: "hint-local-workspace".to_string(),
                        scope: ContextScope::SessionShared,
                        kind: ContextKind::Instructions,
                        content: "Task likely targets local workspace files. Prefer filesystem tools and local paths first; use remote repository tools only when explicitly requested.".to_string(),
                        priority: 0.93,
                    });
                }

                for (idx, output) in dep_outputs.iter().enumerate() {
                    chunks.push(ContextChunk {
                        id: format!("dep-{idx}"),
                        scope: ContextScope::SessionShared,
                        kind: ContextKind::History,
                        content: output.clone(),
                        priority: 0.8,
                    });
                }

                chunks.append(&mut recent_history_chunks);

                if let Some(summary) = recent_run_summary {
                    chunks.push(ContextChunk {
                        id: "history-prev-run".to_string(),
                        scope: ContextScope::SessionShared,
                        kind: ContextKind::History,
                        content: summary,
                        priority: 0.78,
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

                for (knowledge_id, topic, content, importance) in global_hits {
                    chunks.push(ContextChunk {
                        id: format!("kb:{knowledge_id}"),
                        scope: ContextScope::GlobalShared,
                        kind: ContextKind::Retrieval,
                        content: format!("knowledge[{topic}]: {content}"),
                        priority: importance.clamp(0.2, 1.0),
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
                let token_seq_counter = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
                let token_counter = token_seq_counter.clone();
                let on_token: crate::router::TokenCallback = Arc::new(move |token: &str| {
                    let seq = token_counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    token_sink(RuntimeEvent::NodeTokenChunk {
                        node_id: token_node_id.clone(),
                        role: token_role,
                        token: token.to_string(),
                        token_seq: seq,
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

    fn build_on_completed_fn(
        &self,
        run_id: Uuid,
        session_id: Uuid,
        task: String,
    ) -> OnNodeCompletedFn {
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

                // Emit TerminalSuggested when Coder node completes via LLM backend
                if node.role == AgentRole::Coder
                    && result.succeeded
                    && !result.model.starts_with("coder:")
                {
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
        let token_memory = memory.clone();
        let (token_tx, mut token_rx) = mpsc::unbounded_channel::<(String, AgentRole, String)>();

        tokio::spawn(async move {
            while let Some((node_id, role, token)) = token_rx.recv().await {
                let _ = token_memory
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
            }
        });

        Arc::new(move |event: RuntimeEvent| {
            // Token chunks are high-frequency: store as action events only (skip timeline/session/webhook)
            if let RuntimeEvent::NodeTokenChunk {
                ref node_id,
                ref role,
                ref token,
                token_seq: _,
            } = event
            {
                let _ = token_tx.send((node_id.clone(), *role, token.clone()));
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
                    RuntimeEvent::NodeCompleted {
                        node_id,
                        role,
                        model,
                        duration_ms,
                        output_preview,
                        output_truncated,
                    } => serde_json::json!({
                        "node_id": node_id,
                        "role": role,
                        "phase": "completed",
                        "model": model,
                        "duration_ms": duration_ms,
                        "output_preview": output_preview,
                        "output_truncated": output_truncated,
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
                    RuntimeEvent::CoderSessionStarted { node_id, session_id, backend } => serde_json::json!({
                        "node_id": node_id,
                        "session_id": session_id,
                        "backend": backend,
                        "phase": "coder_session_started",
                    }),
                    RuntimeEvent::CoderOutputChunk { node_id, session_id, stream, content } => serde_json::json!({
                        "node_id": node_id,
                        "session_id": session_id,
                        "stream": stream,
                        "content": content,
                        "phase": "coder_output_chunk",
                    }),
                    RuntimeEvent::CoderFileChanged { node_id, session_id, file } => serde_json::json!({
                        "node_id": node_id,
                        "session_id": session_id,
                        "file": file,
                        "phase": "coder_file_changed",
                    }),
                    RuntimeEvent::CoderSessionCompleted { node_id, session_id, files_changed, exit_code } => serde_json::json!({
                        "node_id": node_id,
                        "session_id": session_id,
                        "files_changed": files_changed,
                        "exit_code": exit_code,
                        "phase": "coder_session_completed",
                    }),
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
                    | RuntimeEvent::CoderSessionStarted { .. }
                    | RuntimeEvent::CoderOutputChunk { .. }
                    | RuntimeEvent::CoderFileChanged { .. }
                    | RuntimeEvent::CoderSessionCompleted { .. }
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
                    RuntimeEvent::CoderSessionStarted { node_id, .. } => {
                        (RunActionType::CoderSessionStarted, Some(node_id.as_str()))
                    }
                    RuntimeEvent::CoderOutputChunk { node_id, .. } => {
                        (RunActionType::NodeTokenChunk, Some(node_id.as_str()))
                    }
                    RuntimeEvent::CoderFileChanged { node_id, .. } => {
                        (RunActionType::CoderSessionStarted, Some(node_id.as_str()))
                    }
                    RuntimeEvent::CoderSessionCompleted { node_id, .. } => {
                        (RunActionType::CoderSessionCompleted, Some(node_id.as_str()))
                    }
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
            .map(|n| {
                let output = if n.role == AgentRole::Summarizer {
                    Self::clean_llm_output(&n.output)
                } else {
                    n.output
                };
                AgentExecutionRecord {
                    node_id: n.node_id,
                    role: n.role,
                    model: n.model,
                    output,
                    duration_ms: n.duration_ms,
                    succeeded: n.succeeded,
                    error: n.error,
                }
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

    pub async fn list_workflows(&self, limit: usize) -> anyhow::Result<Vec<WorkflowTemplate>> {
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

    pub async fn list_schedules(
        &self,
        limit: usize,
    ) -> anyhow::Result<Vec<crate::types::CronSchedule>> {
        self.memory.list_schedules(limit).await
    }

    pub async fn get_schedule(
        &self,
        id: Uuid,
    ) -> anyhow::Result<Option<crate::types::CronSchedule>> {
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
            params.as_ref().map(|p| p.to_string()).unwrap_or_default()
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
                    let model = event
                        .payload
                        .get("model")
                        .and_then(|v| v.as_str())
                        .map(ToString::to_string);
                    let duration_ms = event
                        .payload
                        .get("duration_ms")
                        .and_then(|v| v.as_u64())
                        .map(|v| v as u128);
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
                    if node.model.is_none() {
                        node.model = model;
                    }
                    if node.duration_ms.is_none() {
                        node.duration_ms = duration_ms;
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
            | RunActionType::TerminalSuggested
            | RunActionType::CoderSessionStarted
            | RunActionType::CoderSessionCompleted => {}
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

fn parse_tool_calls(raw: &str) -> Vec<serde_json::Value> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }

    let mut parsed = Vec::<serde_json::Value>::new();

    if let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed) {
        collect_tool_calls(value, &mut parsed);
    } else {
        let stream = serde_json::Deserializer::from_str(trimmed).into_iter::<serde_json::Value>();
        for value in stream.flatten() {
            collect_tool_calls(value, &mut parsed);
        }
    }

    let mut seen = HashSet::<String>::new();
    let mut deduped = Vec::new();
    for call in parsed {
        let tool_name = call
            .get("tool_name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim();
        if tool_name.is_empty() {
            continue;
        }

        let key = serde_json::json!({
            "tool_name": tool_name,
            "arguments": call.get("arguments").cloned().unwrap_or(serde_json::json!({})),
        })
        .to_string();
        if seen.insert(key) {
            deduped.push(call);
        }
    }

    deduped
}

fn collect_tool_calls(value: serde_json::Value, out: &mut Vec<serde_json::Value>) {
    match value {
        serde_json::Value::Array(values) => {
            for item in values {
                collect_tool_calls(item, out);
            }
        }
        serde_json::Value::Object(map) => {
            if map.contains_key("tool_name") {
                out.push(serde_json::Value::Object(map));
            }
        }
        _ => {}
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

        let router = Arc::new(ModelRouter::new("http://127.0.0.1:8000", None, None, None));
        let mut coder_mgr = coder_backend::CoderSessionManager::new(
            memory.clone(),
            std::env::temp_dir(),
            crate::types::CoderBackendKind::Llm,
        );
        coder_mgr.register_backend(Arc::new(coder_backend::LlmCoderBackend::new(router.clone())));
        let orchestrator = Orchestrator::new(
            AgentRuntime::new(4),
            AgentRegistry::builtin(),
            router,
            memory,
            Arc::new(ContextManager::new(8_000)),
            webhook,
            6,
            Arc::new(McpRegistry::new()),
            Arc::new(coder_mgr),
        );

        let no_servers: Vec<String> = vec![];
        let no_tools: Vec<String> = vec![];

        // Complex/CodeGeneration task should have full graph
        let graph = orchestrator
            .build_graph(
                "implement a webhook integration endpoint with code",
                &no_servers,
                &no_tools,
                None,
                None,
            )
            .await
            .unwrap();
        assert!(graph.node("plan").is_some());
        assert!(graph.node("code").is_some());
        assert!(graph.node("webhook_validation").is_some());

        // SimpleQuery should have minimal graph
        let simple = orchestrator
            .build_graph("hello world", &no_servers, &no_tools, None, None)
            .await
            .unwrap();
        assert!(simple.node("plan").is_some());
        assert!(simple.node("summarize").is_some());
        assert!(simple.node("code").is_none());

        // Analysis task should have analyzer
        let analysis = orchestrator
            .build_graph(
                "analyze the performance patterns and evaluate metrics",
                &no_servers,
                &no_tools,
                None,
                None,
            )
            .await
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

    #[test]
    fn parse_tool_calls_handles_concatenated_arrays_and_dedupes() {
        let raw = r#"[{"tool_name":"filesystem/list_allowed_directories","arguments":{}}]
[{"tool_name":"filesystem/list_allowed_directories","arguments":{}}]
{"tool_name":"filesystem/read_text_file","arguments":{"path":"README.md"}}"#;

        let calls = parse_tool_calls(raw);
        assert_eq!(calls.len(), 2);
        assert_eq!(
            calls[0].get("tool_name").and_then(|v| v.as_str()),
            Some("filesystem/list_allowed_directories")
        );
        assert_eq!(
            calls[1].get("tool_name").and_then(|v| v.as_str()),
            Some("filesystem/read_text_file")
        );
    }

    #[test]
    fn parse_tool_calls_accepts_single_object() {
        let raw = r#"{"tool_name":"filesystem/list_directory","arguments":{"path":"."}}"#;
        let calls = parse_tool_calls(raw);
        assert_eq!(calls.len(), 1);
        assert_eq!(
            calls[0].get("tool_name").and_then(|v| v.as_str()),
            Some("filesystem/list_directory")
        );
    }
}
