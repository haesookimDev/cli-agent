pub mod cluster;
pub mod coder_backend;
pub mod completion;
pub mod context_builder;
pub mod git_manager;
pub mod graph_builder;
pub mod helpers;
pub mod interactive;
pub mod node_executor;
pub mod prompt_composer;
pub mod repo_analyzer;
pub mod run_manager;
pub mod settings;
pub mod skill_loader;
pub mod skill_router;
pub mod task_classifier;
pub mod tool_augment;
pub mod validator;

use helpers::{
    clean_llm_output, parse_agent_role,
};

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use chrono::Utc;
use dashmap::DashMap;
use tracing::{error, info};
use uuid::Uuid;

use crate::agents::AgentRegistry;
use crate::context::ContextManager;
use crate::mcp::McpRegistry;
use crate::memory::MemoryManager;
use crate::router::ModelRouter;
use crate::runtime::graph::ExecutionGraph;
use crate::runtime::{AgentRuntime, NodeExecutionResult};
use crate::session_workspace::SessionWorkspaceManager;
use crate::types::{
    AgentExecutionRecord, AgentRole, ChatMessage, ChatRole, KnowledgeItem, McpToolDefinition,
    NodeTraceState, RepoAnalysis, RepoAnalysisConfig, RunActionEvent, RunActionType,
    RunBehaviorActionCount, RunBehaviorLane, RunBehaviorSummary, RunBehaviorView, RunRecord,
    RunRequest, RunStatus, RunSubmission, RunTrace, RunTraceGraph, SessionEvent, SessionEventType,
    SessionMemoryItem, TraceEdge, ValidationConfig, WebhookDeliveryRecord, WebhookEndpoint,
    WorkflowTemplate,
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
    validation_config: Arc<ValidationConfig>,
    repo_analysis_config: Arc<RepoAnalysisConfig>,
    repo_analyses: Arc<DashMap<Uuid, RepoAnalysis>>,
    skills: Arc<DashMap<String, WorkflowTemplate>>,
    session_workspace: SessionWorkspaceManager,
}

#[derive(Debug, Clone)]
struct RunControl {
    cancel_requested: Arc<AtomicBool>,
    pause_requested: Arc<AtomicBool>,
}

const MAX_COMPLETION_CONTINUATIONS: u8 = 2;
/// How many times the orchestrator may redesign the workflow after a stage failure
/// before giving up and marking the run as Failed.
const MAX_FAILURE_RETRIES: u8 = 2;
const DYNAMIC_SUBTASK_MAX_PARALLELISM: usize = 4;
/// Hard cap on dynamic nodes a single Planner output may generate to prevent OOM.
const MAX_DYNAMIC_SUBTASKS_PER_PLAN: usize = 50;

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
        validation_config: Arc<ValidationConfig>,
        repo_analysis_config: Arc<RepoAnalysisConfig>,
        session_workspace: SessionWorkspaceManager,
        skills_dir: Option<PathBuf>,
    ) -> Self {
        let skills = Arc::new(DashMap::new());
        let _ = skills_dir; // loaded asynchronously later
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
            validation_config,
            repo_analysis_config,
            repo_analyses: Arc::new(DashMap::new()),
            skills,
            session_workspace,
        }
    }

    pub fn router(&self) -> &Arc<ModelRouter> {
        &self.router
    }

    pub async fn ensure_session_workspace(&self, session_id: Uuid) -> anyhow::Result<PathBuf> {
        self.session_workspace.ensure_session_dir(session_id).await
    }

    async fn resolve_run_cli_working_dir(
        &self,
        session_id: Uuid,
        run_id: Uuid,
    ) -> anyhow::Result<PathBuf> {
        if let Some(analysis) = self.repo_analyses.get(&run_id) {
            let repo_path = analysis.repo_path.trim();
            if !repo_path.is_empty() {
                return Ok(PathBuf::from(repo_path));
            }
        }

        self.session_workspace.ensure_session_dir(session_id).await
    }

    pub async fn load_skills_from_dir(&self, dir: &std::path::Path) {
        let loaded = skill_loader::load_skills_from_dir(dir).await;
        for skill in loaded {
            self.skills.insert(skill.id.clone(), skill);
        }
        tracing::info!("Loaded {} skills", self.skills.len());
    }

    pub async fn reload_skills(&self, dir: &std::path::Path) {
        self.skills.clear();
        self.load_skills_from_dir(dir).await;
    }

    pub fn list_skills(&self) -> Vec<WorkflowTemplate> {
        self.skills.iter().map(|e| e.value().clone()).collect()
    }

    pub fn get_skill(&self, id: &str) -> Option<WorkflowTemplate> {
        self.skills.get(id).map(|e| e.value().clone())
    }





    async fn materialize_workflow_template(
        &self,
        workflow_id: &str,
        params: Option<&serde_json::Value>,
    ) -> anyhow::Result<WorkflowTemplate> {
        let template = match self.skills.get(workflow_id) {
            Some(t) => t.value().clone(),
            None => self
                .memory
                .get_workflow(workflow_id)
                .await?
                .ok_or_else(|| anyhow::anyhow!("workflow/skill not found: {workflow_id}"))?,
        };

        let param_map: HashMap<String, String> = params
            .and_then(|value| value.as_object())
            .map(|obj| {
                obj.iter()
                    .map(|(k, v)| (k.clone(), v.as_str().unwrap_or(&v.to_string()).to_string()))
                    .collect()
            })
            .unwrap_or_default();

        Ok(skill_loader::interpolate_params(&template, &param_map))
    }

    pub async fn list_coder_sessions(
        &self,
        run_id: Uuid,
    ) -> anyhow::Result<Vec<serde_json::Value>> {
        self.coder_manager.list_sessions_for_run(run_id).await
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
        self.session_workspace
            .delete_session_dir(session_id)
            .await?;
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
            repo_url: None,
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
            repo_url: None,
        };
        self.submit_run(req).await
    }

    pub async fn create_session(&self, session_id: Uuid) -> anyhow::Result<()> {
        self.session_workspace
            .ensure_session_dir(session_id)
            .await?;
        self.memory.create_session(session_id).await?;
        Ok(())
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
        if let Err(e) = self
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
            .await
        {
            tracing::warn!(
                run_id = %run_id,
                session_id = %session_id,
                action = ?action,
                error = %e,
                "failed to record action event"
            );
        }
    }

    async fn record_graph_initialized(
        &self,
        run_id: Uuid,
        session_id: Uuid,
        graph: &ExecutionGraph,
        stage: &str,
    ) {
        let graph_nodes = graph.nodes();
        self.record_action_event(
            run_id,
            session_id,
            RunActionType::GraphInitialized,
            Some("orchestrator"),
            Some("graph"),
            None,
            serde_json::json!({
                "stage": stage,
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
    }

    async fn record_node_progress(
        &self,
        run_id: Uuid,
        session_id: Uuid,
        node_id: &str,
        role: AgentRole,
        stage: &str,
        message: impl Into<String>,
        details: serde_json::Value,
    ) {
        let message = message.into();
        let payload = serde_json::json!({
            "node_id": node_id,
            "role": role,
            "stage": stage,
            "message": message,
            "details": details,
        });

        let _ = self
            .memory
            .append_event(SessionEvent {
                session_id,
                run_id: Some(run_id),
                event_type: SessionEventType::RunProgress,
                timestamp: Utc::now(),
                payload: payload.clone(),
            })
            .await;

        self.record_action_event(
            run_id,
            session_id,
            RunActionType::NodeProgress,
            Some("node"),
            Some(node_id),
            None,
            payload,
        )
        .await;
    }


    /// Builds a Planner-led recovery graph when one or more nodes fail outright.
    /// The Planner receives the failure context and produces a new SubtaskPlan
    /// targeting only the failed work.






    async fn build_system_state_snapshot(&self) -> String {
        let settings = self.current_settings().await;
        let coder_backend = self.coder_manager.default_backend_kind();
        let mcp_servers = self.mcp.server_descriptions();

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
            settings.terminal_command,
            settings.terminal_args.join(" "),
            settings.terminal_auto_spawn,
            mcp_section,
        )
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
                    clean_llm_output(&n.output)
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
            source: Default::default(),
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
            .materialize_workflow_template(workflow_id, params.as_ref())
            .await?;
        let graph = self.build_workflow_graph_from_template(&template)?;

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
            repo_url: None,
        };

        let run_id = Uuid::new_v4();
        let sid = req.session_id.unwrap_or_else(Uuid::new_v4);
        self.workflow_graphs.insert(run_id, graph);

        // We need to submit via the same path but with a pre-built graph
        // Re-implement submit_run inline to use the pre-stored graph
        self.create_session(sid).await?;
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
            | RunActionType::NodeProgress
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
            | RunActionType::CoderSessionCompleted
            | RunActionType::ValidationPassed
            | RunActionType::ValidationFailed
            | RunActionType::GitCommitCreated
            | RunActionType::GitPushCompleted
            | RunActionType::RepoCloneCompleted
            | RunActionType::RepoAnalysisCompleted
            | RunActionType::InteractiveStep
            | RunActionType::GitHubIssueCreated
            | RunActionType::GitHubIssueCommented
            | RunActionType::GitHubIssueClosed
            | RunActionType::GitHubPrCreated
            | RunActionType::GitHubPrReviewed
            | RunActionType::GitHubPrCommented
            | RunActionType::GitHubPrMerged
            | RunActionType::GitHubBranchCreated => {}
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


#[cfg(test)]
mod tests {
    use chrono::Utc;
    use crate::types::{CliModelBackendKind, DetectedCommands, TechStack};

    use super::*;
    use super::graph_builder::workflow_node_to_agent_node;
    use super::helpers::{
        infer_clone_target_dir, parse_tool_calls, summarize_selector_output,
        summarize_tool_input_schema,
    };
    use crate::runtime::graph::AgentNode;
    use crate::types::{CliModelConfig, WorkflowNodeTemplate};
    use std::time::Duration;

    async fn make_test_orchestrator(
        suffix: &str,
    ) -> (std::path::PathBuf, Arc<MemoryManager>, Orchestrator) {
        let tmp = std::env::temp_dir().join(format!("agent-{suffix}-{}", Uuid::new_v4()));
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

        let router = Arc::new(ModelRouter::new(
            "http://127.0.0.1:8000",
            None,
            None,
            None,
            None,
        ));
        let mut coder_mgr = coder_backend::CoderSessionManager::new(
            memory.clone(),
            std::path::PathBuf::new(),
            crate::types::CoderBackendKind::Llm,
        );
        coder_mgr.register_backend(Arc::new(coder_backend::LlmCoderBackend::new(
            router.clone(),
        )));
        let orchestrator = Orchestrator::new(
            AgentRuntime::new(4),
            AgentRegistry::builtin(),
            router,
            memory.clone(),
            Arc::new(ContextManager::new(8_000)),
            webhook,
            6,
            Arc::new(McpRegistry::new()),
            Arc::new(coder_mgr),
            Arc::new(ValidationConfig::default()),
            Arc::new(RepoAnalysisConfig::default()),
            crate::session_workspace::SessionWorkspaceManager::new(tmp.join("repo")),
            None,
        );

        (tmp, memory, orchestrator)
    }

    #[test]
    fn workflow_node_to_agent_node_preserves_git_commands() {
        let node = workflow_node_to_agent_node(&WorkflowNodeTemplate {
            id: "git_check".to_string(),
            role: AgentRole::Validator,
            instructions: "Run repository inspection".to_string(),
            dependencies: vec!["plan".to_string()],
            mcp_tools: vec![],
            git_commands: vec!["status --short".to_string(), "git diff --stat".to_string()],
            policy: serde_json::json!({
                "timeout_ms": 45000,
                "retry": 0,
                "max_parallelism": 1,
                "circuit_breaker": 1,
                "on_dependency_failure": "fail_fast",
                "fallback_node": null
            }),
        })
        .unwrap();

        assert_eq!(
            node.git_commands,
            vec!["status --short".to_string(), "git diff --stat".to_string()]
        );
        assert_eq!(node.policy.timeout_ms, 45_000);
    }

    #[test]
    fn workflow_node_to_agent_node_rejects_git_commands_on_non_validator() {
        let err = workflow_node_to_agent_node(&WorkflowNodeTemplate {
            id: "bad_git".to_string(),
            role: AgentRole::Planner,
            instructions: "Plan around git".to_string(),
            dependencies: vec![],
            mcp_tools: vec![],
            git_commands: vec!["status".to_string()],
            policy: serde_json::json!({}),
        })
        .unwrap_err();

        assert!(err.to_string().contains("git_commands"));
        assert!(err.to_string().contains("validator"));
    }

    #[test]
    fn infer_clone_target_dir_uses_repo_name() {
        assert_eq!(
            infer_clone_target_dir("https://github.com/openai/cli-agent.git"),
            "cli-agent".to_string()
        );
    }

    #[test]
    fn summarize_tool_input_schema_reports_properties_and_required() {
        let summary = summarize_tool_input_schema(&serde_json::json!({
            "type": "object",
            "properties": {
                "owner": {"type": "string"},
                "repo": {"type": "string"},
                "path": {"type": "string"}
            },
            "required": ["owner", "repo"]
        }));

        assert!(summary.contains("args: owner, path, repo"));
        assert!(summary.contains("required: owner, repo"));
    }

    #[tokio::test]
    async fn auto_skill_route_prefers_local_repo_overview_for_analysis() {
        let (tmp, _memory, orchestrator) = make_test_orchestrator("auto-skill").await;

        orchestrator.skills.insert(
            "local_repo_overview".to_string(),
            WorkflowTemplate {
                id: "local_repo_overview".to_string(),
                name: "Local Repo Overview".to_string(),
                description: "Clone and inspect a repository locally.".to_string(),
                created_at: Utc::now(),
                updated_at: Utc::now(),
                source_run_id: None,
                graph_template: crate::types::WorkflowGraphTemplate { nodes: vec![] },
                parameters: vec![],
                source: crate::types::SkillSource::File,
            },
        );
        orchestrator.skills.insert(
            "github_repo_overview".to_string(),
            WorkflowTemplate {
                id: "github_repo_overview".to_string(),
                name: "GitHub Repo Overview".to_string(),
                description: "Analyze a remote GitHub repository.".to_string(),
                created_at: Utc::now(),
                updated_at: Utc::now(),
                source_run_id: None,
                graph_template: crate::types::WorkflowGraphTemplate { nodes: vec![] },
                parameters: vec![],
                source: crate::types::SkillSource::File,
            },
        );

        let route = orchestrator.auto_skill_route(
            "Analyze https://github.com/example/project and summarize the architecture",
            None,
        );

        assert_eq!(
            route.as_ref().map(|item| item.workflow_id.as_str()),
            Some("local_repo_overview")
        );
        assert_eq!(
            route
                .as_ref()
                .and_then(|item| item.params.as_ref())
                .and_then(|value| value.get("repo_url"))
                .and_then(|value| value.as_str()),
            Some("https://github.com/example/project")
        );

        let _ = std::fs::remove_dir_all(tmp);
    }

    #[tokio::test]
    async fn auto_skill_route_uses_remote_overview_for_remote_only_requests() {
        let (tmp, _memory, orchestrator) = make_test_orchestrator("auto-skill-remote").await;

        orchestrator.skills.insert(
            "local_repo_overview".to_string(),
            WorkflowTemplate {
                id: "local_repo_overview".to_string(),
                name: "Local Repo Overview".to_string(),
                description: "Clone and inspect a repository locally.".to_string(),
                created_at: Utc::now(),
                updated_at: Utc::now(),
                source_run_id: None,
                graph_template: crate::types::WorkflowGraphTemplate { nodes: vec![] },
                parameters: vec![],
                source: crate::types::SkillSource::File,
            },
        );
        orchestrator.skills.insert(
            "github_repo_overview".to_string(),
            WorkflowTemplate {
                id: "github_repo_overview".to_string(),
                name: "GitHub Repo Overview".to_string(),
                description: "Analyze a remote GitHub repository.".to_string(),
                created_at: Utc::now(),
                updated_at: Utc::now(),
                source_run_id: None,
                graph_template: crate::types::WorkflowGraphTemplate { nodes: vec![] },
                parameters: vec![],
                source: crate::types::SkillSource::File,
            },
        );

        let route = orchestrator.auto_skill_route(
            "Analyze https://github.com/example/project without cloning and only use remote metadata",
            None,
        );

        assert_eq!(
            route.as_ref().map(|item| item.workflow_id.as_str()),
            Some("github_repo_overview")
        );

        let _ = std::fs::remove_dir_all(tmp);
    }

    #[tokio::test]
    async fn planner_subtasks_preserve_mcp_tool_allowlist() {
        let (tmp, _memory, orchestrator) = make_test_orchestrator("subtask-allowlist").await;

        let static_ids = std::sync::Arc::new(std::sync::Mutex::new(vec![]));
        let on_completed = orchestrator.build_on_completed_fn(
            Uuid::new_v4(),
            Uuid::new_v4(),
            "Inspect a remote repository".to_string(),
            static_ids,
        );
        let (result, _skip) = on_completed(
            AgentNode::new("plan", AgentRole::Planner, "plan"),
            NodeExecutionResult {
                node_id: "plan".to_string(),
                role: AgentRole::Planner,
                model: "mock:model".to_string(),
                output: serde_json::json!({
                    "subtasks": [
                        {
                            "id": "repo_meta",
                            "description": "Read repo metadata",
                            "agent_role": "tool_caller",
                            "dependencies": [],
                            "instructions": "Inspect GitHub metadata",
                            "mcp_tools": ["github/get_file_contents", "github/list_commits"]
                        }
                    ]
                })
                .to_string(),
                duration_ms: 1,
                succeeded: true,
                error: None,
            },
        )
        .await
        .unwrap();

        assert_eq!(result.len(), 1);
        assert_eq!(
            result[0].mcp_tools,
            vec![
                "github/get_file_contents".to_string(),
                "github/list_commits".to_string()
            ]
        );

        let _ = std::fs::remove_dir_all(tmp);
    }

    #[tokio::test]
    async fn planner_subtasks_apply_cli_timeout_normalization() {
        let tmp = std::env::temp_dir().join(format!("agent-cli-timeout-{}", Uuid::new_v4()));
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

        let router = Arc::new(ModelRouter::new(
            "http://127.0.0.1:8000",
            None,
            None,
            None,
            Some(CliModelConfig {
                backend: CliModelBackendKind::Codex,
                command: "codex".to_string(),
                args: vec![],
                timeout_ms: 300_000,
                cli_only: true,
            }),
        ));
        let mut coder_mgr = coder_backend::CoderSessionManager::new(
            memory.clone(),
            std::path::PathBuf::new(),
            crate::types::CoderBackendKind::Llm,
        );
        coder_mgr.register_backend(Arc::new(coder_backend::LlmCoderBackend::new(
            router.clone(),
        )));
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
            Arc::new(ValidationConfig::default()),
            Arc::new(RepoAnalysisConfig::default()),
            crate::session_workspace::SessionWorkspaceManager::new(tmp.join("repo")),
            None,
        );

        let static_ids = std::sync::Arc::new(std::sync::Mutex::new(vec![]));
        let on_completed = orchestrator.build_on_completed_fn(
            Uuid::new_v4(),
            Uuid::new_v4(),
            "Analyze the backend architecture".to_string(),
            static_ids,
        );
        let (result, _skip) = on_completed(
            AgentNode::new("plan", AgentRole::Planner, "plan"),
            NodeExecutionResult {
                node_id: "plan".to_string(),
                role: AgentRole::Planner,
                model: "mock:model".to_string(),
                output: serde_json::json!({
                    "subtasks": [
                        {
                            "id": "analyze_backend",
                            "description": "Analyze backend",
                            "agent_role": "analyzer",
                            "dependencies": [],
                            "instructions": "Inspect the backend implementation",
                            "mcp_tools": []
                        }
                    ]
                })
                .to_string(),
                duration_ms: 1,
                succeeded: true,
                error: None,
            },
        )
        .await
        .unwrap();

        assert_eq!(result.len(), 1);
        assert_eq!(result[0].policy.timeout_ms, 330_000);

        let _ = std::fs::remove_dir_all(tmp);
    }

    #[tokio::test]
    async fn graph_builder_respects_depth_and_dependencies() {
        let (tmp, _memory, orchestrator) = make_test_orchestrator("graph-builder").await;

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
            .build_graph("hello world", &no_servers, &no_tools, None, None, None, None)
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
                None,
                None,
            )
            .await
            .unwrap();
        assert!(analysis.node("plan").is_some());
        assert!(analysis.node("analyze").is_some());

        let _ = std::fs::remove_dir_all(tmp);
    }

    #[tokio::test]
    async fn completion_followup_graph_uses_planner_recovery_node() {
        let tmp = std::env::temp_dir().join(format!("agent-followup-test-{}", Uuid::new_v4()));
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

        let router = Arc::new(ModelRouter::new(
            "http://127.0.0.1:8000",
            None,
            None,
            None,
            None,
        ));
        let mut coder_mgr = coder_backend::CoderSessionManager::new(
            memory.clone(),
            std::env::temp_dir(),
            crate::types::CoderBackendKind::Llm,
        );
        coder_mgr.register_backend(Arc::new(coder_backend::LlmCoderBackend::new(
            router.clone(),
        )));
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
            Arc::new(ValidationConfig::default()),
            Arc::new(RepoAnalysisConfig::default()),
            crate::session_workspace::SessionWorkspaceManager::new(tmp.join("repo")),
            None,
        );

        let graph = orchestrator
            .build_completion_followup_graph(
                "Implement the remaining CLI orchestration pieces",
                "parallel subtask execution was not completed",
                &[NodeExecutionResult {
                    node_id: "code".to_string(),
                    role: AgentRole::Coder,
                    model: "mock:model".to_string(),
                    output: "Partial implementation".to_string(),
                    duration_ms: 10,
                    succeeded: true,
                    error: None,
                }],
                1,
            )
            .unwrap();

        let node = graph.node("replan_plan_1").unwrap();
        assert_eq!(node.role, AgentRole::Planner);
        assert!(node.instructions.contains("SubtaskPlan"));
        assert!(node.instructions.contains("parallel"));
    }

    #[tokio::test]
    async fn create_and_delete_session_manage_workspace_dirs() {
        let tmp = std::env::temp_dir().join(format!("agent-session-workspace-{}", Uuid::new_v4()));
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

        let router = Arc::new(ModelRouter::new(
            "http://127.0.0.1:8000",
            None,
            None,
            None,
            None,
        ));
        let mut coder_mgr = coder_backend::CoderSessionManager::new(
            memory.clone(),
            std::path::PathBuf::new(),
            crate::types::CoderBackendKind::Llm,
        );
        coder_mgr.register_backend(Arc::new(coder_backend::LlmCoderBackend::new(
            router.clone(),
        )));
        let repo_root = tmp.join("repo");
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
            Arc::new(ValidationConfig::default()),
            Arc::new(RepoAnalysisConfig::default()),
            crate::session_workspace::SessionWorkspaceManager::new(repo_root.clone()),
            None,
        );

        let session_id = Uuid::new_v4();
        let session_dir = repo_root.join(session_id.to_string());

        orchestrator.create_session(session_id).await.unwrap();
        assert!(session_dir.is_dir());

        orchestrator.delete_session(session_id).await.unwrap();
        assert!(!session_dir.exists());
    }

    #[tokio::test]
    async fn run_cli_working_dir_prefers_analyzed_repo_path() {
        let (tmp, _memory, orchestrator) = make_test_orchestrator("cli-working-dir").await;
        let session_id = Uuid::new_v4();
        let run_id = Uuid::new_v4();

        let session_dir = orchestrator.ensure_session_workspace(session_id).await.unwrap();
        let repo_path = session_dir.join("repos").join("sample");
        std::fs::create_dir_all(&repo_path).unwrap();

        orchestrator.repo_analyses.insert(
            run_id,
            RepoAnalysis {
                repo_path: repo_path.to_string_lossy().to_string(),
                tech_stack: TechStack {
                    primary_language: "Rust".to_string(),
                    languages: vec![("Rust".to_string(), 1.0)],
                    frameworks: vec![],
                    package_manager: Some("cargo".to_string()),
                },
                detected_commands: DetectedCommands {
                    lint_commands: vec![],
                    build_commands: vec![],
                    test_commands: vec![],
                },
                repo_map: String::new(),
                file_count: 0,
                key_files: vec![],
            },
        );

        let resolved = orchestrator
            .resolve_run_cli_working_dir(session_id, run_id)
            .await
            .unwrap();

        assert_eq!(resolved, repo_path);

        let _ = std::fs::remove_dir_all(tmp);
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

    #[test]
    fn parse_tool_calls_accepts_wrapped_tool_calls_array() {
        let raw = r#"{"tool_calls":[{"tool_name":"github/get_file_contents","arguments":{"owner":"haesookimDev","repo":"DevGarden","path":"README.md"}}]}"#;
        let calls = parse_tool_calls(raw);
        assert_eq!(calls.len(), 1);
        assert_eq!(
            calls[0].get("tool_name").and_then(|v| v.as_str()),
            Some("github/get_file_contents")
        );
    }

    #[test]
    fn parse_tool_calls_accepts_tool_call_tags() {
        let raw = r#"Analysis first.
<tool_call>{"tool_name":"github/list_commits","arguments":{"owner":"haesookimDev","repo":"DevGarden","perPage":5}}</tool_call>"#;
        let calls = parse_tool_calls(raw);
        assert_eq!(calls.len(), 1);
        assert_eq!(
            calls[0].get("tool_name").and_then(|v| v.as_str()),
            Some("github/list_commits")
        );
    }

    #[test]
    fn parse_tool_calls_accepts_prose_then_fenced_json() {
        let raw = r#"I will inspect the README first.

```json
[
  {
    "tool_name": "github/get_file_contents",
    "arguments": {
      "owner": "haesookimDev",
      "repo": "DevGarden",
      "path": "README.md"
    }
  }
]
```"#;
        let calls = parse_tool_calls(raw);
        assert_eq!(calls.len(), 1);
        assert_eq!(
            calls[0].get("tool_name").and_then(|v| v.as_str()),
            Some("github/get_file_contents")
        );
    }

    #[test]
    fn parse_tool_calls_extracts_inline_json_fragment_from_prose() {
        let raw = r#"Use this next call: [{"tool_name":"github/list_commits","arguments":{"owner":"haesookimDev","repo":"DevGarden","perPage":5}}] and then summarize."#;
        let calls = parse_tool_calls(raw);
        assert_eq!(calls.len(), 1);
        assert_eq!(
            calls[0].get("tool_name").and_then(|v| v.as_str()),
            Some("github/list_commits")
        );
    }

    #[test]
    fn summarize_selector_output_compacts_whitespace_and_truncates() {
        let preview =
            summarize_selector_output("line one\nline two\n\nline three with extra spacing");
        assert_eq!(preview, "line one line two line three with extra spacing");

        let long = "a".repeat(400);
        let preview = summarize_selector_output(&long);
        assert!(preview.ends_with("..."));
        assert!(preview.len() <= 323);
    }
}
