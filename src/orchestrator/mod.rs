pub mod coder_backend;
pub mod git_manager;
pub mod interactive;
pub mod prompt_composer;
pub mod repo_analyzer;
pub mod skill_loader;
pub mod tool_augment;
pub mod validator;

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
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
use crate::router::ProviderKind;
use crate::runtime::graph::{AgentNode, DependencyFailurePolicy, ExecutionGraph, ExecutionPolicy};
use crate::runtime::{
    AgentRuntime, EventSink, NodeExecutionResult, OnNodeCompletedFn, RunNodeFn, RuntimeEvent,
    ShouldCancelFn, ShouldPauseFn,
};
use crate::session_workspace::SessionWorkspaceManager;
use crate::types::{
    AgentExecutionRecord, AgentRole, AppSettings, ChatMessage, ChatRole, CliModelConfig,
    KnowledgeItem, McpToolDefinition, NodeTraceState, RepoAnalysis, RepoAnalysisConfig,
    RunActionEvent, RunActionType, RunBehaviorActionCount, RunBehaviorLane, RunBehaviorSummary,
    RunBehaviorView, RunRecord, RunRequest, RunStatus, RunSubmission, RunTrace, RunTraceGraph,
    SessionEvent, SessionEventType, SessionMemoryItem, SettingsPatch, SubtaskPlan, TaskProfile,
    TaskType, TraceEdge, ValidationConfig, WebhookDeliveryRecord, WebhookEndpoint,
    WorkflowNodeTemplate, WorkflowTemplate,
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

#[derive(Debug, Clone)]
struct AutoSkillRoute {
    workflow_id: String,
    params: Option<serde_json::Value>,
    reason: String,
}

const MAX_COMPLETION_CONTINUATIONS: u8 = 2;
const DYNAMIC_SUBTASK_MAX_PARALLELISM: usize = 4;

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

    fn normalize_node_policy_for_runtime(&self, node: &mut AgentNode) {
        if node.role == AgentRole::Validator {
            return;
        }

        if let Some(cli_model) = self.router.cli_model_config() {
            let extra_ms = match node.role {
                AgentRole::ToolCaller | AgentRole::Coder => 120_000,
                AgentRole::Planner
                | AgentRole::Extractor
                | AgentRole::Analyzer
                | AgentRole::Reviewer
                | AgentRole::Summarizer
                | AgentRole::Fallback
                | AgentRole::Scheduler
                | AgentRole::ConfigManager => 30_000,
                AgentRole::Validator => 0,
            };
            let minimum = cli_model.timeout_ms.saturating_add(extra_ms);
            node.policy.timeout_ms = node.policy.timeout_ms.max(minimum);
        }
    }

    fn add_graph_node(
        &self,
        graph: &mut ExecutionGraph,
        mut node: AgentNode,
    ) -> anyhow::Result<()> {
        self.normalize_node_policy_for_runtime(&mut node);
        graph.add_node(node)
    }

    fn normalize_dynamic_nodes_for_runtime(&self, nodes: &mut [AgentNode]) {
        for node in nodes {
            self.normalize_node_policy_for_runtime(node);
        }
    }

    fn build_workflow_graph_from_template(
        &self,
        template: &WorkflowTemplate,
    ) -> anyhow::Result<ExecutionGraph> {
        let mut graph = ExecutionGraph::new(self.max_graph_depth);
        for wn in &template.graph_template.nodes {
            let node = workflow_node_to_agent_node(wn)?;
            self.add_graph_node(&mut graph, node)?;
        }
        Ok(graph)
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

    fn auto_skill_route(&self, task: &str, repo_url: Option<&str>) -> Option<AutoSkillRoute> {
        if self.skills.is_empty() {
            return None;
        }

        let lower = task.to_lowercase();
        let detected_repo_url = repo_url
            .map(|value| value.to_string())
            .or_else(|| extract_repo_url_from_text(task));

        let explicit_skill_id = self
            .skills
            .iter()
            .map(|entry| entry.value().clone())
            .find(|skill| {
                lower.contains(skill.id.to_lowercase().as_str())
                    || lower.contains(skill.name.to_lowercase().as_str())
            })
            .map(|skill| skill.id);

        if let Some(skill_id) = explicit_skill_id {
            let params = match skill_id.as_str() {
                "git_clone_repo" => detected_repo_url.as_ref().map(|url| {
                    serde_json::json!({
                        "repo_url": url,
                        "target_dir": infer_clone_target_dir(url),
                    })
                }),
                "local_repo_overview" => detected_repo_url.as_ref().map(|url| {
                    serde_json::json!({
                        "repo_url": url,
                    })
                }),
                "github_repo_overview" => detected_repo_url
                    .as_ref()
                    .map(|url| serde_json::json!({ "repo_url": url })),
                _ => Some(serde_json::json!({})),
            };

            if let Some(params) = params {
                return Some(AutoSkillRoute {
                    workflow_id: skill_id,
                    params: if params == serde_json::json!({}) {
                        None
                    } else {
                        Some(params)
                    },
                    reason: "explicit_skill_match".to_string(),
                });
            }
        }

        if self.skills.contains_key("git_clone_repo")
            && detected_repo_url.is_some()
            && contains_any_keyword(
                lower.as_str(),
                &[
                    "clone",
                    "clon",
                    "fetch",
                    "checkout",
                    "복제",
                    "클론",
                    "가져와",
                ],
            )
        {
            let url = detected_repo_url?;
            let target_dir = infer_clone_target_dir(url.as_str());
            return Some(AutoSkillRoute {
                workflow_id: "git_clone_repo".to_string(),
                params: Some(serde_json::json!({
                    "repo_url": url,
                    "target_dir": target_dir,
                })),
                reason: "clone_skill_auto_route".to_string(),
            });
        }

        let wants_remote_only = contains_any_keyword(
            lower.as_str(),
            &[
                "remote only",
                "without cloning",
                "without clone",
                "no clone",
                "clone 없이",
                "원격으로만",
                "원격만",
                "readme only",
                "metadata",
                "issues",
                "issue",
                "pull request",
                "pr ",
                "commits",
                "commit history",
                "release notes",
            ],
        );

        if self.skills.contains_key("local_repo_overview")
            && detected_repo_url.is_some()
            && !wants_remote_only
            && !contains_any_keyword(
                lower.as_str(),
                &[
                    "clone", "checkout", "patch", "fix", "modify", "edit", "수정", "클론",
                ],
            )
            && contains_any_keyword(
                lower.as_str(),
                &[
                    "analyze",
                    "analysis",
                    "overview",
                    "summary",
                    "summarize",
                    "architecture",
                    "structure",
                    "stack",
                    "repo",
                    "repository",
                    "프로젝트",
                    "리포지토리",
                    "분석",
                    "요약",
                    "개요",
                    "구조",
                ],
            )
        {
            let url = detected_repo_url?;
            return Some(AutoSkillRoute {
                workflow_id: "local_repo_overview".to_string(),
                params: Some(serde_json::json!({ "repo_url": url })),
                reason: "local_repo_overview_auto_route".to_string(),
            });
        }

        if self.skills.contains_key("github_repo_overview")
            && detected_repo_url.is_some()
            && wants_remote_only
        {
            let url = detected_repo_url?;
            return Some(AutoSkillRoute {
                workflow_id: "github_repo_overview".to_string(),
                params: Some(serde_json::json!({ "repo_url": url })),
                reason: "github_repo_overview_auto_route".to_string(),
            });
        }

        None
    }

    pub async fn list_coder_sessions(
        &self,
        run_id: Uuid,
    ) -> anyhow::Result<Vec<serde_json::Value>> {
        self.coder_manager.list_sessions_for_run(run_id).await
    }

    pub fn get_settings(&self) -> AppSettings {
        let cli_model = self.router.cli_model_config();
        let (
            cli_model_enabled,
            cli_model_backend,
            cli_model_command,
            cli_model_args,
            cli_model_timeout_ms,
            cli_model_only,
        ) = match cli_model {
            Some(config) => (
                true,
                Some(config.backend),
                config.command,
                config.args,
                config.timeout_ms,
                config.cli_only,
            ),
            None => (false, None, String::new(), Vec::new(), 300_000, false),
        };

        AppSettings {
            default_profile: TaskProfile::General,
            preferred_model: self.router.preferred_model(),
            disabled_models: self.router.disabled_models(),
            disabled_providers: self
                .router
                .disabled_providers()
                .iter()
                .map(|p| p.to_string())
                .collect(),
            cli_model_enabled,
            cli_model_backend,
            cli_model_command,
            cli_model_args,
            cli_model_timeout_ms,
            cli_model_only,
            terminal_command: crate::types::default_terminal_command(),
            terminal_args: crate::types::default_terminal_args(),
            terminal_auto_spawn: false,
        }
    }

    pub async fn current_settings(&self) -> AppSettings {
        let mut settings = self.get_settings();
        if let Ok(Some(persisted)) = self.memory.store().load_settings().await {
            settings.default_profile = persisted.default_profile;
            settings.terminal_command = persisted.terminal_command;
            settings.terminal_args = persisted.terminal_args;
            settings.terminal_auto_spawn = persisted.terminal_auto_spawn;

            if !settings.cli_model_enabled && persisted.cli_model_enabled {
                settings.cli_model_enabled = true;
                settings.cli_model_backend = persisted.cli_model_backend;
                settings.cli_model_command = persisted.cli_model_command;
                settings.cli_model_args = persisted.cli_model_args;
                settings.cli_model_timeout_ms = persisted.cli_model_timeout_ms;
                settings.cli_model_only = persisted.cli_model_only;
            }
        }
        settings
    }

    pub async fn persist_current_settings(&self) {
        let settings = self.current_settings().await;
        if let Err(e) = self.memory.store().save_settings(&settings).await {
            error!("failed to persist settings: {e}");
        }
    }

    pub async fn update_settings(&self, patch: SettingsPatch) {
        let cli_patch_applied = patch.cli_model_enabled.is_some()
            || patch.cli_model_backend.is_some()
            || patch.cli_model_command.is_some()
            || patch.cli_model_args.is_some()
            || patch.cli_model_timeout_ms.is_some()
            || patch.cli_model_only.is_some();

        let mut settings = self.current_settings().await;

        if let Some(profile) = patch.default_profile {
            settings.default_profile = profile;
        }
        if let Some(preferred) = patch.preferred_model {
            settings.preferred_model = preferred;
        }
        if let Some(disabled_models) = patch.disabled_models {
            settings.disabled_models = disabled_models;
        }
        if let Some(disabled_providers) = patch.disabled_providers {
            settings.disabled_providers = disabled_providers;
        }
        if let Some(enabled) = patch.cli_model_enabled {
            settings.cli_model_enabled = enabled;
            if !enabled {
                settings.cli_model_backend = None;
            }
        }
        if let Some(backend) = patch.cli_model_backend {
            settings.cli_model_backend = backend;
        }
        if let Some(command) = patch.cli_model_command {
            settings.cli_model_command = command;
        }
        if let Some(args) = patch.cli_model_args {
            settings.cli_model_args = args;
        }
        if let Some(timeout_ms) = patch.cli_model_timeout_ms {
            settings.cli_model_timeout_ms = timeout_ms;
        }
        if let Some(cli_only) = patch.cli_model_only {
            settings.cli_model_only = cli_only;
        }
        if let Some(cmd) = patch.terminal_command {
            settings.terminal_command = cmd;
        }
        if let Some(args) = patch.terminal_args {
            settings.terminal_args = args;
        }
        if let Some(auto) = patch.terminal_auto_spawn {
            settings.terminal_auto_spawn = auto;
        }

        if cli_patch_applied {
            match Self::normalize_cli_model_config(&settings) {
                Some(cli_model) => {
                    settings.preferred_model =
                        Some(cli_model.backend.default_model_id().to_string());
                }
                None => {
                    if settings
                        .preferred_model
                        .as_deref()
                        .is_some_and(Self::is_cli_model_id)
                    {
                        settings.preferred_model = None;
                    }
                }
            }
        }

        self.apply_settings_to_runtime(&settings);

        if let Err(e) = self.memory.store().save_settings(&settings).await {
            error!("failed to persist settings: {e}");
        }
    }

    pub async fn load_persisted_settings(&self) {
        match self.memory.store().load_settings().await {
            Ok(Some(settings)) => {
                self.apply_settings_to_runtime(&settings);
                info!("restored persisted settings");
            }
            Ok(None) => {}
            Err(e) => {
                error!("failed to load persisted settings: {e}");
            }
        }
    }

    fn apply_settings_to_runtime(&self, settings: &AppSettings) {
        self.router
            .set_preferred_model(settings.preferred_model.clone());

        for spec in self.router.catalog() {
            self.router.set_model_disabled(&spec.model_id, false);
        }
        for provider in Self::all_providers() {
            self.router.set_provider_disabled(provider, false);
        }

        let cli_model = Self::normalize_cli_model_config(settings);
        self.router.set_cli_model_config(cli_model.clone());
        if let Some(cli_model) = cli_model.as_ref() {
            self.router.apply_cli_model_bootstrap(cli_model);
            self.router.set_preferred_model(
                settings
                    .preferred_model
                    .clone()
                    .or_else(|| Some(cli_model.backend.default_model_id().to_string())),
            );
        }

        for model_id in &settings.disabled_models {
            self.router.set_model_disabled(model_id, true);
        }

        if !(settings.cli_model_enabled && settings.cli_model_only) {
            for name in &settings.disabled_providers {
                if let Ok(pk) =
                    serde_json::from_value::<ProviderKind>(serde_json::Value::String(name.clone()))
                {
                    self.router.set_provider_disabled(pk, true);
                }
            }
        }
    }

    fn normalize_cli_model_config(settings: &AppSettings) -> Option<CliModelConfig> {
        if !settings.cli_model_enabled {
            return None;
        }

        let backend = settings.cli_model_backend?;
        let command = if settings.cli_model_command.trim().is_empty() {
            backend.default_command().to_string()
        } else {
            settings.cli_model_command.clone()
        };

        Some(CliModelConfig {
            backend,
            command,
            args: settings.cli_model_args.clone(),
            timeout_ms: settings.cli_model_timeout_ms.max(1_000),
            cli_only: settings.cli_model_only,
        })
    }

    fn all_providers() -> [ProviderKind; 7] {
        [
            ProviderKind::OpenAi,
            ProviderKind::Anthropic,
            ProviderKind::Gemini,
            ProviderKind::Vllm,
            ProviderKind::ClaudeCode,
            ProviderKind::Codex,
            ProviderKind::Mock,
        ]
    }

    fn is_cli_model_id(model_id: &str) -> bool {
        matches!(model_id, "claude-code-cli" | "codex-cli")
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
        if req.repo_url.is_none() {
            req.repo_url = extract_repo_url_from_text(req.task.as_str());
        }

        let auto_route = if req.workflow_id.is_none() {
            self.auto_skill_route(req.task.as_str(), req.repo_url.as_deref())
        } else {
            None
        };

        if let Some(route) = auto_route.as_ref() {
            req.workflow_id = Some(route.workflow_id.clone());
            req.workflow_params = route.params.clone();
        }

        if let Some(workflow_id) = req.workflow_id.clone() {
            let template = self
                .materialize_workflow_template(&workflow_id, req.workflow_params.as_ref())
                .await?;
            let graph = self.build_workflow_graph_from_template(&template)?;
            self.workflow_graphs.insert(run_id, graph);
        }

        self.create_session(session_id).await?;

        let mut record = RunRecord::new_queued(run_id, session_id, req.task.clone(), req.profile);
        record
            .timeline
            .push(format!("{} run queued", Utc::now().to_rfc3339()));
        if let Some(route) = auto_route.as_ref() {
            record.timeline.push(format!(
                "{} auto-routed to skill '{}' ({})",
                Utc::now().to_rfc3339(),
                route.workflow_id,
                route.reason,
            ));
        }

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
                "task": req.task.clone(),
                "workflow_id": req.workflow_id.clone(),
                "workflow_params": req.workflow_params.clone(),
                "routing": auto_route.as_ref().map(|route| serde_json::json!({
                    "type": "skill",
                    "workflow_id": route.workflow_id.clone(),
                    "reason": route.reason.clone(),
                })),
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

    fn summarize_results_for_followup(results: &[NodeExecutionResult]) -> String {
        results
            .iter()
            .filter(|r| r.succeeded && !r.output.trim().is_empty())
            .map(|r| {
                format!(
                    "[{}:{}]\n{}",
                    r.node_id,
                    r.role,
                    Self::trim_for_context(r.output.as_str(), 700)
                )
            })
            .collect::<Vec<_>>()
            .join("\n---\n")
    }

    fn build_completion_followup_graph(
        &self,
        original_task: &str,
        incomplete_reason: &str,
        results: &[NodeExecutionResult],
        attempt: u8,
    ) -> anyhow::Result<ExecutionGraph> {
        let mut graph = ExecutionGraph::new(self.max_graph_depth);
        let prior_summary = Self::summarize_results_for_followup(results);
        let local_first_hint = if Self::is_local_workspace_task(original_task) {
            "\n- The remaining work must stay local-first and CLI-compatible.\n- Prefer filesystem and local workspace actions; do not require UI-only steps."
        } else {
            ""
        };
        let planner_id = format!("replan_plan_{attempt}");
        let mut planner = AgentNode::new(
            planner_id,
            AgentRole::Planner,
            format!(
                "Reviewer marked the run incomplete.\n\n\
                 ORIGINAL TASK:\n{}\n\n\
                 REMAINING GAP:\n{}\n\n\
                 PREVIOUS SUCCESSFUL OUTPUTS:\n{}\n\n\
                 Return a JSON SubtaskPlan object matching exactly this schema:\n\
                 {{\"subtasks\":[{{\"id\":\"string\",\"description\":\"string\",\"agent_role\":\"planner|extractor|coder|summarizer|fallback|tool_caller|analyzer|reviewer|scheduler|config_manager|validator\",\"dependencies\":[\"node_id\"],\"instructions\":\"string\",\"mcp_tools\":[\"server/tool\"]}}]}}\n\n\
                 Rules:\n\
                 - Cover only the remaining requirements.\n\
                 - Split independent remaining work into separate subtasks so they can run in parallel.\n\
                 - Use dependencies only when sequencing is truly required.\n\
                 - Reuse the smallest set of sub-agents needed to fully complete the request.\n\
                 - If only one focused step remains, still return a single-item subtasks array.\n\
                 - Do not wrap the JSON in markdown fences.{}",
                Self::trim_for_context(original_task, 1000),
                Self::trim_for_context(incomplete_reason, 600),
                Self::trim_for_context(prior_summary.as_str(), 4000),
                local_first_hint,
            ),
        );
        planner.policy = ExecutionPolicy {
            timeout_ms: 120_000,
            retry: 1,
            max_parallelism: 1,
            circuit_breaker: 2,
            on_dependency_failure: DependencyFailurePolicy::FailFast,
            fallback_node: None,
        };
        self.add_graph_node(&mut graph, planner)?;

        Ok(graph)
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
        let (previous_task_type, previous_plan) =
            if Self::is_continuation_command(req.task.as_str()) {
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
            None => {
                let graph_working_dir = self.session_workspace.ensure_session_dir(session_id).await?;
                self.build_graph(
                    req.task.as_str(),
                    &mcp_server_names,
                    &mcp_tool_names,
                    previous_task_type,
                    previous_plan,
                    req.repo_url.as_deref(),
                    Some(graph_working_dir.as_path()),
                )
                .await?
            }
        };
        self.record_graph_initialized(run_id, session_id, &graph, "initial")
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

        let should_cancel: ShouldCancelFn = Arc::new({
            let cancel_flag = cancel_flag.clone();
            move || cancel_flag.load(Ordering::Relaxed)
        });
        let should_pause: ShouldPauseFn = Arc::new({
            let pause_flag = pause_flag.clone();
            move || pause_flag.load(Ordering::Relaxed)
        });

        let mut current_graph = graph;
        let mut accumulated_results = Vec::<NodeExecutionResult>::new();
        let mut continuation_attempts = 0u8;

        loop {
            let outputs = self
                .runtime
                .execute_graph(
                    current_graph,
                    run_node.clone(),
                    on_complete.clone(),
                    Some(event_sink.clone()),
                    Some(should_cancel.clone()),
                    Some(should_pause.clone()),
                )
                .await;

            match outputs {
                Ok(node_results) => {
                    let stage_succeeded = node_results.iter().all(|r| r.succeeded);
                    accumulated_results.extend(node_results);

                    if cancel_flag.load(Ordering::Relaxed) {
                        self.finish_run(run_id, RunStatus::Cancelled, accumulated_results, None)
                            .await?;
                        break;
                    }

                    if !stage_succeeded {
                        self.finish_run(run_id, RunStatus::Failed, accumulated_results, None)
                            .await?;
                        break;
                    }

                    let (verified, reason) = self
                        .verify_completion(run_id, session_id, &req.task, &accumulated_results)
                        .await;
                    if verified {
                        self.finish_run(run_id, RunStatus::Succeeded, accumulated_results, None)
                            .await?;
                        break;
                    }

                    if continuation_attempts >= MAX_COMPLETION_CONTINUATIONS {
                        self.finish_run(
                            run_id,
                            RunStatus::Failed,
                            accumulated_results,
                            Some(format!(
                                "Verification incomplete after {} continuation attempts: {}",
                                MAX_COMPLETION_CONTINUATIONS, reason
                            )),
                        )
                        .await?;
                        break;
                    }

                    continuation_attempts += 1;
                    let continuation_reason = reason.clone();
                    self.record_action_event(
                        run_id,
                        session_id,
                        RunActionType::ReplanTriggered,
                        Some("reviewer"),
                        Some("continuation"),
                        None,
                        serde_json::json!({
                            "attempt": continuation_attempts,
                            "reason": continuation_reason,
                            "mode": "completion_continuation",
                            "max_attempts": MAX_COMPLETION_CONTINUATIONS,
                        }),
                    )
                    .await;

                    current_graph = match self.build_completion_followup_graph(
                        req.task.as_str(),
                        reason.as_str(),
                        &accumulated_results,
                        continuation_attempts,
                    ) {
                        Ok(graph) => graph,
                        Err(err) => {
                            self.finish_run(
                                run_id,
                                RunStatus::Failed,
                                accumulated_results,
                                Some(err.to_string()),
                            )
                            .await?;
                            break;
                        }
                    };
                    let stage_label = format!("continuation_{continuation_attempts}");
                    self.record_graph_initialized(
                        run_id,
                        session_id,
                        &current_graph,
                        stage_label.as_str(),
                    )
                    .await;
                }
                Err(err) => {
                    self.finish_run(
                        run_id,
                        RunStatus::Failed,
                        accumulated_results,
                        Some(err.to_string()),
                    )
                    .await?;
                    break;
                }
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
            working_dir: match self.resolve_run_cli_working_dir(session_id, run_id).await {
                Ok(dir) => Some(dir),
                Err(err) => {
                    tracing::warn!("failed to resolve reviewer CLI working dir: {err}");
                    None
                }
            },
        };

        let review_result = self
            .agents
            .run_role(AgentRole::Reviewer, review_input, self.router.clone(), None)
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
                    (
                        false,
                        format!(
                            "Ambiguous reviewer response: {}",
                            Self::trim_for_context(content, 240)
                        ),
                    )
                }
            }
            Err(e) => {
                // Completion must be explicit; reviewer failure is not success.
                (false, format!("Reviewer unavailable: {e}"))
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
                for key in [
                    "content", "answer", "response", "summary", "text", "message",
                ] {
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
        repo_url: Option<&str>,
        working_dir: Option<&std::path::Path>,
    ) -> TaskType {
        // If a repo_url is provided, this is an external project task
        if repo_url.is_some() {
            return TaskType::ExternalProject;
        }

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
                mcp_tool_names
                    .iter()
                    .take(15)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ")
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
             - external_project: Tasks involving cloning, analyzing, or modifying an external repository given a URL or path\n\
             - interactive: Open-ended tasks requiring multi-step reasoning, exploration, or iterative tool usage\n\
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
            .infer_in_dir(
                crate::types::TaskProfile::General,
                &classification_prompt,
                &constraints,
                working_dir,
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
                    "external_project" => TaskType::ExternalProject,
                    "interactive" => TaskType::Interactive,
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
        let external_kw = ["clone", "github.com", "gitlab.com", "bitbucket.org"];
        if external_kw.iter().any(|kw| lower.contains(kw))
            || (lower.contains("https://") && lower.contains(".git"))
        {
            return TaskType::ExternalProject;
        }

        let code_kw = [
            "code",
            "implement",
            "function",
            "refactor",
            "bug",
            "fix",
            "debug",
        ];
        if code_kw.iter().any(|kw| lower.contains(kw)) {
            return TaskType::CodeGeneration;
        }
        let analysis_kw = ["analyze", "analysis", "pattern", "compare", "evaluate"];
        if analysis_kw.iter().any(|kw| lower.contains(kw)) {
            return TaskType::Analysis;
        }
        let interactive_kw = [
            "explore",
            "investigate",
            "figure out",
            "step by step",
            "iteratively",
        ];
        if interactive_kw.iter().any(|kw| lower.contains(kw)) {
            return TaskType::Interactive;
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

    async fn build_graph(
        &self,
        task: &str,
        mcp_server_names: &[String],
        mcp_tool_names: &[String],
        previous_task_type: Option<TaskType>,
        previous_plan: Option<String>,
        repo_url: Option<&str>,
        working_dir: Option<&std::path::Path>,
    ) -> anyhow::Result<ExecutionGraph> {
        let task_type = Self::classify_task(
            task,
            mcp_server_names,
            mcp_tool_names,
            previous_task_type,
            &self.router,
            repo_url,
            working_dir,
        )
        .await;
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
                 If the task requires external data or operations, plan to use these tools via the tool_call node. \
                 For non-trivial work, output a JSON SubtaskPlan so sub-agents can execute independent work in parallel and continue until the request is fully completed.{}{}",
                tool_summary, guide, local_first_hint
            )
        } else {
            "Plan work and constraints. For non-trivial work, output a JSON SubtaskPlan so sub-agents can execute independent work in parallel and continue until the request is fully completed.".to_string()
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
        self.add_graph_node(&mut graph, plan)?;

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
                self.add_graph_node(&mut graph, summarize)?;
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
                self.add_graph_node(&mut graph, extract)?;

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
                self.add_graph_node(&mut graph, analyze)?;

                let mut summarize = AgentNode::new(
                    "summarize",
                    AgentRole::Summarizer,
                    "Consolidate analysis results into a concise report.",
                );
                summarize.dependencies = vec!["analyze".to_string()];
                summarize.policy = Self::default_policy();
                self.add_graph_node(&mut graph, summarize)?;
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
                self.add_graph_node(&mut graph, summarize)?;
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
                self.add_graph_node(&mut graph, config)?;

                let mut summarize = AgentNode::new(
                    "summarize",
                    AgentRole::Summarizer,
                    "Summarize the configuration changes made.",
                );
                summarize.dependencies = vec!["config_manage".to_string()];
                summarize.policy = Self::default_policy();
                self.add_graph_node(&mut graph, summarize)?;
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
                self.add_graph_node(&mut graph, tool)?;

                let mut summarize = AgentNode::new(
                    "summarize",
                    AgentRole::Summarizer,
                    "Summarize tool execution results.",
                );
                summarize.dependencies = vec!["tool_call".to_string()];
                summarize.policy = Self::default_policy();
                self.add_graph_node(&mut graph, summarize)?;
            }
            TaskType::ExternalProject => {
                let mut repo_analyze = AgentNode::new(
                    "repo_analyze",
                    AgentRole::Analyzer,
                    "Clone and analyze the external repository.",
                );
                repo_analyze.dependencies = vec!["plan".to_string()];
                repo_analyze.depth = 1;
                repo_analyze.policy = ExecutionPolicy {
                    timeout_ms: 300_000,
                    retry: 1,
                    ..Self::default_policy()
                };
                self.add_graph_node(&mut graph, repo_analyze)?;

                let mut extract = AgentNode::new(
                    "extract",
                    AgentRole::Extractor,
                    "Extract structured facts from the repo analysis and user task.",
                );
                extract.dependencies = vec!["repo_analyze".to_string()];
                extract.depth = 2;
                extract.policy = ExecutionPolicy {
                    max_parallelism: 2,
                    ..Self::default_policy()
                };
                self.add_graph_node(&mut graph, extract)?;

                let mut context_probe = AgentNode::new(
                    "context_probe",
                    AgentRole::Extractor,
                    "Probe repo map for high-value code areas to focus modification.",
                );
                context_probe.dependencies = vec!["repo_analyze".to_string()];
                context_probe.depth = 2;
                context_probe.policy = ExecutionPolicy {
                    max_parallelism: 2,
                    timeout_ms: 60_000,
                    ..Self::default_policy()
                };
                self.add_graph_node(&mut graph, context_probe)?;

                let mut code = AgentNode::new(
                    "code",
                    AgentRole::Coder,
                    "Modify the external repository code based on plan and repo context.",
                );
                code.dependencies = vec!["extract".to_string(), "context_probe".to_string()];
                code.depth = 3;
                code.policy = ExecutionPolicy {
                    max_parallelism: 2,
                    retry: 2,
                    timeout_ms: 300_000,
                    circuit_breaker: 2,
                    on_dependency_failure: DependencyFailurePolicy::FailFast,
                    fallback_node: Some("fallback_code".to_string()),
                };
                self.add_graph_node(&mut graph, code)?;

                let mut fallback_code = AgentNode::new(
                    "fallback_code",
                    AgentRole::Fallback,
                    "Recover from coding failures in external repo.",
                );
                fallback_code.dependencies = vec!["code".to_string()];
                fallback_code.depth = 4;
                fallback_code.policy = Self::default_policy();
                self.add_graph_node(&mut graph, fallback_code)?;

                // Validation node: uses detected commands from repo analysis
                let mut validate_lint =
                    AgentNode::new("validate_lint", AgentRole::Validator, "external_lint");
                validate_lint.dependencies = vec!["code".to_string()];
                validate_lint.depth = 4;
                validate_lint.policy = ExecutionPolicy {
                    timeout_ms: 120_000,
                    retry: 0,
                    on_dependency_failure: DependencyFailurePolicy::FallbackNode,
                    fallback_node: Some("fallback_code".to_string()),
                    ..Self::default_policy()
                };
                self.add_graph_node(&mut graph, validate_lint)?;

                // summarize omitted — dynamically injected by on_completed
            }
            TaskType::Interactive => {
                let mut react_node = AgentNode::new(
                    "interactive",
                    AgentRole::ToolCaller,
                    "Execute interactive ReAct loop to complete the task.",
                );
                react_node.dependencies = vec!["plan".to_string()];
                react_node.policy = ExecutionPolicy {
                    timeout_ms: 600_000,
                    retry: 0,
                    ..ExecutionPolicy::default()
                };
                self.add_graph_node(&mut graph, react_node)?;

                let mut summarize = AgentNode::new(
                    "summarize",
                    AgentRole::Summarizer,
                    "Summarize the interactive session results.",
                );
                summarize.dependencies = vec!["interactive".to_string()];
                summarize.policy = ExecutionPolicy::default();
                self.add_graph_node(&mut graph, summarize)?;
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
                self.add_graph_node(&mut graph, extract)?;

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
                self.add_graph_node(&mut graph, context_probe)?;

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
                self.add_graph_node(&mut graph, code)?;

                let mut fallback_code = AgentNode::new(
                    "fallback_code",
                    AgentRole::Fallback,
                    "Recover from coding node failures using robust conservative strategy.",
                );
                fallback_code.dependencies = vec!["code".to_string()];
                fallback_code.policy = Self::default_policy();
                self.add_graph_node(&mut graph, fallback_code)?;

                // When validation is configured, insert validate_lint after code.
                // Summarize is injected dynamically via on_completed.
                if self.validation_config.has_lint() {
                    let mut validate_lint =
                        AgentNode::new("validate_lint", AgentRole::Validator, "lint");
                    validate_lint.dependencies = vec!["code".to_string()];
                    validate_lint.policy = ExecutionPolicy {
                        timeout_ms: self.validation_config.lint_timeout_ms,
                        retry: 0,
                        on_dependency_failure: DependencyFailurePolicy::FallbackNode,
                        fallback_node: Some("fallback_code".to_string()),
                        ..Self::default_policy()
                    };
                    self.add_graph_node(&mut graph, validate_lint)?;
                    // No static summarize — injected dynamically after validation chain
                } else {
                    // No validation: keep original static summarize
                    let mut summarize = AgentNode::new(
                        "summarize",
                        AgentRole::Summarizer,
                        "Summarize outcomes and produce checkpoint summary for context compaction.",
                    );
                    summarize.dependencies = vec!["code".to_string(), "fallback_code".to_string()];
                    summarize.policy = Self::default_policy();
                    self.add_graph_node(&mut graph, summarize)?;
                }

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
                    self.add_graph_node(&mut graph, webhook)?;
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
        let validation_config = self.validation_config.clone();
        let repo_analysis_config = self.repo_analysis_config.clone();
        let repo_analyses = self.repo_analyses.clone();
        let session_workspace = self.session_workspace.clone();
        let orchestrator = self.clone();

        Arc::new(move |node: AgentNode, deps: Vec<NodeExecutionResult>| {
            let agents = agents.clone();
            let router = router.clone();
            let memory = memory.clone();
            let context = context.clone();
            let req = req.clone();
            let mcp = mcp.clone();
            let event_sink = event_sink.clone();
            let coder_manager = coder_manager.clone();
            let validation_config = validation_config.clone();
            let repo_analysis_config = repo_analysis_config.clone();
            let repo_analyses = repo_analyses.clone();
            let session_workspace = session_workspace.clone();
            let orchestrator = orchestrator.clone();

            async move {
                let started = Instant::now();

                // --- Repo analysis handler ---
                if node.id == "repo_analyze" && node.role == AgentRole::Analyzer {
                    let repo_config = repo_analysis_config.clone();

                    let repo_url = req
                        .repo_url
                        .clone()
                        .or_else(|| extract_repo_url_from_text(&req.task))
                        .or_else(|| extract_repo_url_from_text(&node.instructions))
                        .or_else(|| {
                            deps.iter()
                                .find(|d| d.node_id == "plan" && d.succeeded)
                                .and_then(|d| extract_repo_url_from_text(&d.output))
                        });

                    let Some(url) = repo_url else {
                        return Ok(NodeExecutionResult {
                            node_id: node.id,
                            role: node.role,
                            model: "repo_analyzer".to_string(),
                            output: "No repository URL found in request or planner output".to_string(),
                            duration_ms: started.elapsed().as_millis(),
                            succeeded: false,
                            error: Some("missing repo_url".to_string()),
                        });
                    };

                    orchestrator
                        .record_node_progress(
                            run_id,
                            session_id,
                            node.id.as_str(),
                            node.role,
                            "repo_url_resolved",
                            format!("Resolved repository target: {url}"),
                            serde_json::json!({
                                "repo_url": url.clone(),
                            }),
                        )
                        .await;

                    let clone_dir = match session_workspace
                        .ensure_scoped_dir(session_id, Some(&repo_config.clone_base_dir))
                        .await
                    {
                        Ok(dir) => dir,
                        Err(e) => {
                            return Ok(NodeExecutionResult {
                                node_id: node.id,
                                role: node.role,
                                model: "repo_analyzer".to_string(),
                                output: format!("Session workspace setup failed: {}", e),
                                duration_ms: started.elapsed().as_millis(),
                                succeeded: false,
                                error: Some(format!("workspace setup failed: {}", e)),
                            });
                        }
                    };

                    orchestrator
                        .record_node_progress(
                            run_id,
                            session_id,
                            node.id.as_str(),
                            node.role,
                            "repo_clone_started",
                            "Cloning or refreshing repository in session workspace",
                            serde_json::json!({
                                "repo_url": url.clone(),
                                "clone_dir": clone_dir.clone(),
                                "shallow": repo_config.shallow_clone,
                            }),
                        )
                        .await;

                    let repo_path = match git_manager::GitManager::clone_repo(
                        &url,
                        &clone_dir,
                        repo_config.shallow_clone,
                    )
                    .await
                    {
                        Ok(path) => path,
                        Err(e) => {
                            return Ok(NodeExecutionResult {
                                node_id: node.id,
                                role: node.role,
                                model: "repo_analyzer".to_string(),
                                output: format!("Clone failed: {}", e),
                                duration_ms: started.elapsed().as_millis(),
                                succeeded: false,
                                error: Some(format!("clone failed: {}", e)),
                            });
                        }
                    };

                    orchestrator
                        .record_node_progress(
                            run_id,
                            session_id,
                            node.id.as_str(),
                            node.role,
                            "repo_clone_completed",
                            "Repository is ready in the session workspace",
                            serde_json::json!({
                                "repo_path": repo_path.clone(),
                                "shallow": repo_config.shallow_clone,
                            }),
                        )
                        .await;

                    event_sink(RuntimeEvent::RepoCloned {
                        node_id: node.id.clone(),
                        repo_path: repo_path.to_string_lossy().to_string(),
                        shallow: repo_config.shallow_clone,
                    });

                    let analyzer = repo_analyzer::RepoAnalyzer::new((*repo_config).clone());
                    orchestrator
                        .record_node_progress(
                            run_id,
                            session_id,
                            node.id.as_str(),
                            node.role,
                            "repo_analysis_started",
                            "Scanning repository structure and key files",
                            serde_json::json!({
                                "repo_path": repo_path.clone(),
                            }),
                        )
                        .await;
                    match analyzer.analyze(&repo_path, &router).await {
                        Ok(analysis) => {
                            orchestrator
                                .record_node_progress(
                                    run_id,
                                    session_id,
                                    node.id.as_str(),
                                    node.role,
                                    "repo_analysis_completed",
                                    format!(
                                        "Repository scan completed: {} files, primary language {}",
                                        analysis.file_count,
                                        analysis.tech_stack.primary_language
                                    ),
                                    serde_json::json!({
                                        "repo_path": analysis.repo_path.clone(),
                                        "file_count": analysis.file_count,
                                        "primary_language": analysis.tech_stack.primary_language.clone(),
                                        "key_files": analysis.key_files.clone(),
                                    }),
                                )
                                .await;
                            event_sink(RuntimeEvent::RepoAnalyzed {
                                node_id: node.id.clone(),
                                primary_language: analysis.tech_stack.primary_language.clone(),
                                file_count: analysis.file_count,
                            });
                            let output = serde_json::to_string(&analysis).unwrap_or_default();
                            repo_analyses.insert(run_id, analysis);
                            return Ok(NodeExecutionResult {
                                node_id: node.id,
                                role: node.role,
                                model: "repo_analyzer".to_string(),
                                output,
                                duration_ms: started.elapsed().as_millis(),
                                succeeded: true,
                                error: None,
                            });
                        }
                        Err(e) => {
                            return Ok(NodeExecutionResult {
                                node_id: node.id,
                                role: node.role,
                                model: "repo_analyzer".to_string(),
                                output: format!("Analysis failed: {}", e),
                                duration_ms: started.elapsed().as_millis(),
                                succeeded: false,
                                error: Some(e.to_string()),
                            });
                        }
                    }
                }

                // Validator role: run lint/build/test/git commands
                if node.role == AgentRole::Validator {
                    let working_dir = match session_workspace
                        .ensure_scoped_dir(
                            session_id,
                            validation_config.working_dir.as_deref(),
                        )
                        .await
                    {
                        Ok(dir) => dir,
                        Err(e) => {
                            return Ok(NodeExecutionResult {
                                node_id: node.id,
                                role: node.role,
                                model: "validator".to_string(),
                                output: format!("Session workspace setup failed: {}", e),
                                duration_ms: started.elapsed().as_millis(),
                                succeeded: false,
                                error: Some(format!("workspace setup failed: {}", e)),
                            });
                        }
                    };
                    let instructions = node.instructions.as_str();

                    if !node.git_commands.is_empty() {
                        use crate::orchestrator::git_manager::GitManager;

                        let results = GitManager::run_cli_commands(
                            &node.git_commands,
                            &working_dir,
                            node.policy.timeout_ms,
                        )
                        .await;

                        let all_passed = results.iter().all(|r| r.passed);
                        let output = serde_json::to_string(&results).unwrap_or_default();

                        event_sink(RuntimeEvent::ValidationCompleted {
                            node_id: node.id.clone(),
                            phase: "git_cli".to_string(),
                            passed: all_passed,
                        });

                        let error = if all_passed {
                            None
                        } else {
                            results
                                .iter()
                                .find(|r| !r.passed)
                                .map(|r| {
                                    format!(
                                        "{}: {}",
                                        r.command,
                                        r.stderr.chars().take(500).collect::<String>()
                                    )
                                })
                        };

                        return Ok(NodeExecutionResult {
                            node_id: node.id,
                            role: node.role,
                            model: "git".to_string(),
                            output,
                            duration_ms: started.elapsed().as_millis(),
                            succeeded: all_passed,
                            error,
                        });
                    }

                    if instructions.contains("git") {
                        // Git automation phase
                        use crate::orchestrator::git_manager::GitManager;

                        if !GitManager::is_git_repo(&working_dir).await {
                            return Ok(NodeExecutionResult {
                                node_id: node.id,
                                role: node.role,
                                model: "git".to_string(),
                                output: "Not a git repository".to_string(),
                                duration_ms: started.elapsed().as_millis(),
                                succeeded: false,
                                error: Some("working directory is not a git repository".to_string()),
                            });
                        }

                        let stashed = if validation_config.git_protect_dirty {
                            GitManager::stash_save(&working_dir, "agent-auto-stash")
                                .await
                                .unwrap_or(false)
                        } else {
                            false
                        };

                        if let Some(ref prefix) = validation_config.git_branch_prefix {
                            let branch_name = format!("{}{}", prefix, run_id);
                            if let Err(e) = GitManager::create_branch(&working_dir, &branch_name).await {
                                tracing::warn!("branch creation failed (may already exist): {e}");
                            }
                        }

                        if let Err(e) = GitManager::stage_all(&working_dir).await {
                            return Ok(NodeExecutionResult {
                                node_id: node.id,
                                role: node.role,
                                model: "git".to_string(),
                                output: format!("git stage failed: {e}"),
                                duration_ms: started.elapsed().as_millis(),
                                succeeded: false,
                                error: Some(e.to_string()),
                            });
                        }

                        let diff = GitManager::staged_diff(&working_dir)
                            .await
                            .unwrap_or_default();

                        let commit_msg = GitManager::generate_commit_message(
                            &diff,
                            &req.task,
                            &working_dir,
                            &router,
                        )
                        .await
                        .unwrap_or_else(|_| "chore: Agent-generated changes".to_string());

                        let commit_hash = match GitManager::commit(&working_dir, &commit_msg).await {
                            Ok(hash) => hash,
                            Err(e) => {
                                if stashed {
                                    let _ = GitManager::stash_pop(&working_dir).await;
                                }
                                return Ok(NodeExecutionResult {
                                    node_id: node.id,
                                    role: node.role,
                                    model: "git".to_string(),
                                    output: format!("git commit failed: {e}"),
                                    duration_ms: started.elapsed().as_millis(),
                                    succeeded: false,
                                    error: Some(e.to_string()),
                                });
                            }
                        };

                        let mut pushed = false;
                        if validation_config.git_auto_push {
                            let branch = GitManager::current_branch(&working_dir)
                                .await
                                .unwrap_or_else(|_| "HEAD".to_string());
                            match GitManager::push(&working_dir, &branch).await {
                                Ok(()) => pushed = true,
                                Err(e) => tracing::warn!("git push failed: {e}"),
                            }
                        }

                        if stashed {
                            let _ = GitManager::stash_pop(&working_dir).await;
                        }

                        event_sink(RuntimeEvent::GitCommitCreated {
                            node_id: node.id.clone(),
                            commit_hash: commit_hash.clone(),
                            commit_message: commit_msg.clone(),
                            pushed,
                        });

                        return Ok(NodeExecutionResult {
                            node_id: node.id,
                            role: node.role,
                            model: "git".to_string(),
                            output: serde_json::json!({
                                "commit_hash": commit_hash,
                                "commit_message": commit_msg,
                                "pushed": pushed,
                            })
                            .to_string(),
                            duration_ms: started.elapsed().as_millis(),
                            succeeded: true,
                            error: None,
                        });
                    }

                    // External project validation: use detected commands from repo analysis
                    if instructions.contains("external") {
                        let analysis = repo_analyses.get(&run_id);
                        if let Some(ref analysis) = analysis {
                            let working_dir = std::path::PathBuf::from(&analysis.repo_path);
                            let (commands, timeout) = if instructions.contains("test") {
                                (analysis.detected_commands.test_commands.clone(), validation_config.test_timeout_ms)
                            } else {
                                (analysis.detected_commands.lint_and_build_commands(), validation_config.lint_timeout_ms)
                            };

                            if commands.is_empty() {
                                return Ok(NodeExecutionResult {
                                    node_id: node.id,
                                    role: node.role,
                                    model: "validator".to_string(),
                                    output: "No commands detected for this phase".to_string(),
                                    duration_ms: started.elapsed().as_millis(),
                                    succeeded: true,
                                    error: None,
                                });
                            }

                            let results = validator::CommandRunner::run_commands(&commands, &working_dir, timeout).await;
                            let all_passed = results.iter().all(|r| r.passed);
                            let output = serde_json::to_string(&results).unwrap_or_default();

                            let phase = if instructions.contains("test") { "external_test" } else { "external_lint" };
                            event_sink(RuntimeEvent::ValidationCompleted {
                                node_id: node.id.clone(),
                                phase: phase.to_string(),
                                passed: all_passed,
                            });

                            return Ok(NodeExecutionResult {
                                node_id: node.id,
                                role: node.role,
                                model: "validator".to_string(),
                                output,
                                duration_ms: started.elapsed().as_millis(),
                                succeeded: all_passed,
                                error: if all_passed { None } else { Some("external validation failed".to_string()) },
                            });
                        } else {
                            return Ok(NodeExecutionResult {
                                node_id: node.id,
                                role: node.role,
                                model: "validator".to_string(),
                                output: "No repo analysis available".to_string(),
                                duration_ms: started.elapsed().as_millis(),
                                succeeded: true,
                                error: None,
                            });
                        }
                    }

                    // Lint or test phase
                    let (commands, timeout) = if instructions.contains("test") {
                        (
                            validation_config.test_commands.clone(),
                            validation_config.test_timeout_ms,
                        )
                    } else {
                        (
                            validation_config.lint_and_build_commands(),
                            validation_config.lint_timeout_ms,
                        )
                    };

                    let phase = if instructions.contains("test") {
                        "test"
                    } else {
                        "lint"
                    };

                    let results = validator::CommandRunner::run_commands(
                        &commands,
                        &working_dir,
                        timeout,
                    )
                    .await;

                    let all_passed = results.iter().all(|r| r.passed);
                    let output = serde_json::to_string(&results).unwrap_or_default();

                    event_sink(RuntimeEvent::ValidationCompleted {
                        node_id: node.id.clone(),
                        phase: phase.to_string(),
                        passed: all_passed,
                    });

                    let error = if all_passed {
                        None
                    } else {
                        results
                            .iter()
                            .find(|r| !r.passed)
                            .map(|r| {
                                format!(
                                    "{}: {}",
                                    r.command,
                                    r.stderr.chars().take(500).collect::<String>()
                                )
                            })
                    };

                    return Ok(NodeExecutionResult {
                        node_id: node.id,
                        role: node.role,
                        model: "shell".to_string(),
                        output,
                        duration_ms: started.elapsed().as_millis(),
                        succeeded: all_passed,
                        error,
                    });
                }

                let cli_working_dir =
                    match orchestrator.resolve_run_cli_working_dir(session_id, run_id).await {
                        Ok(dir) => dir,
                        Err(e) => {
                            return Ok(NodeExecutionResult {
                                node_id: node.id,
                                role: node.role,
                                model: "workspace".to_string(),
                                output: format!("Session workspace setup failed: {}", e),
                                duration_ms: started.elapsed().as_millis(),
                                succeeded: false,
                                error: Some(format!("workspace setup failed: {}", e)),
                            });
                        }
                    };

                // Interactive ReAct loop handler
                if node.id == "interactive" {
                    let react_result = interactive::execute_react_loop(
                        &node,
                        &deps,
                        &req.task,
                        run_id,
                        session_id,
                        Some(cli_working_dir.clone()),
                        15, // max iterations
                        &agents,
                        &router,
                        &memory,
                        &context,
                        &mcp,
                        &event_sink,
                        &node.mcp_tools,
                    )
                    .await;
                    return Ok(react_result);
                }

                // ToolCaller role: LLM-driven tool selection + MCP execution
                if node.role == AgentRole::ToolCaller {
                    // Phase A: Collect dependency outputs and available tools
                    let dep_outputs: Vec<String> = deps
                        .iter()
                        .map(|d| format!("{}: {}", d.node_id, d.output))
                        .collect();

                    let all_tools = mcp.list_all_tools().await;
                    let available_tools = if node.mcp_tools.is_empty() {
                        all_tools
                    } else {
                        skill_loader::filter_tools_for_node(&all_tools, &node.mcp_tools)
                    };
                    if available_tools.is_empty() {
                        return Ok(NodeExecutionResult {
                            node_id: node.id,
                            role: node.role,
                            model: "mcp:none".to_string(),
                            output: "No MCP tools are available for this tool-caller node".to_string(),
                            duration_ms: started.elapsed().as_millis(),
                            succeeded: false,
                            error: Some("No allowed MCP tools available".to_string()),
                        });
                    }

                    orchestrator
                        .record_node_progress(
                            run_id,
                            session_id,
                            node.id.as_str(),
                            node.role,
                            "tool_catalog_ready",
                            format!(
                                "Prepared {} allowed MCP tools for execution",
                                available_tools.len()
                            ),
                            serde_json::json!({
                                "tool_count": available_tools.len(),
                                "tools": available_tools
                                    .iter()
                                    .map(|tool| tool.name.clone())
                                    .collect::<Vec<_>>(),
                            }),
                        )
                        .await;

                    let allowed_tool_names: HashSet<String> = available_tools
                        .iter()
                        .flat_map(|tool| {
                            let bare = tool
                                .name
                                .rsplit('/')
                                .next()
                                .unwrap_or(tool.name.as_str())
                                .to_string();
                            [tool.name.clone(), bare]
                        })
                        .collect();
                    let tool_list_str = format_tool_catalog(&available_tools);

                    let server_descs = mcp.server_descriptions();
                    let server_guide = if !server_descs.is_empty() {
                        let allowed_servers: HashSet<&str> = available_tools
                            .iter()
                            .filter_map(|tool| tool.name.split('/').next())
                            .collect();
                        let lines: Vec<String> = server_descs
                            .iter()
                            .filter(|(name, _)| {
                                allowed_servers.is_empty()
                                    || allowed_servers.contains(name.as_str())
                            })
                            .map(|(name, desc)| format!("- {}: {}", name, desc))
                            .collect();
                        if lines.is_empty() {
                            String::new()
                        } else {
                            format!("\n\nSERVER SELECTION GUIDE:\n{}", lines.join("\n"))
                        }
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
                        orchestrator
                            .record_node_progress(
                                run_id,
                                session_id,
                                node.id.as_str(),
                                node.role,
                                "tool_selection_started",
                                format!("Selecting tools for iteration {}", iteration + 1),
                                serde_json::json!({
                                    "iteration": iteration,
                                    "prior_result_count": all_results.len(),
                                }),
                            )
                            .await;

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
                             NODE INSTRUCTIONS:\n{}\n\n\
                             DEPENDENCY OUTPUTS:\n{}\n\n\
                             AVAILABLE MCP TOOLS:\n{}{}{}{}\n\n\
                             INSTRUCTIONS:\n\
                             - Select the NEXT tool call(s) needed to complete the task.\n\
                             - You MUST obey the node instructions and stay within the allowed MCP tools.\n\
                             - You may use results from previous iterations to inform arguments.\n\
                             - If the task requires sequential steps (e.g., read a value, then use it), \
                               select only the NEXT step's tool(s) in this iteration.\n\
                             - When ALL steps are complete and no more tools are needed, respond with exactly: DONE\n\
                             - Your response MUST start with '[' for tool calls or 'DONE' for completion.\n\
                             - Do NOT return prose, analysis, markdown, or a wrapper object like {{\"tool_calls\": [...]}}.\n\
                             - If you are uncertain, return the single best next tool call instead of explanation-only text.\n\
                             - Do NOT explain why you chose a tool.\n\
                             - Valid example: [{{\"tool_name\":\"github/get_file_contents\",\"arguments\":{{\"owner\":\"haesookimDev\",\"repo\":\"DevGarden\",\"path\":\"README.md\"}}}}]\n\n\
                             Respond ONLY with a JSON array of tool calls, OR the word DONE.\n\
                             Each element: {{\"tool_name\": \"server/tool_name\", \"arguments\": {{...}}}}",
                            req.task,
                            node.instructions,
                            if dep_outputs.is_empty() {
                                "(none)".to_string()
                            } else {
                                dep_outputs.join("\n---\n")
                            },
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
                            working_dir: Some(cli_working_dir.clone()),
                        };

                        let selection_cli_output = router.cli_model_config().map(|cli_model| {
                            let cli_session_id =
                                format!("cli-{}-{}-{}", run_id, node.id, iteration + 1);
                            event_sink(RuntimeEvent::CoderSessionStarted {
                                node_id: node.id.clone(),
                                session_id: cli_session_id.clone(),
                                backend: cli_model.backend.to_string(),
                            });
                            let output_node_id = node.id.clone();
                            let output_session_id = cli_session_id.clone();
                            let output_sink = event_sink.clone();
                            Arc::new(move |stream: &str, content: &str| {
                                if content.trim().is_empty() {
                                    return;
                                }
                                output_sink(RuntimeEvent::CoderOutputChunk {
                                    node_id: output_node_id.clone(),
                                    session_id: output_session_id.clone(),
                                    stream: stream.to_string(),
                                    content: content.to_string(),
                                });
                            }) as crate::router::CliOutputCallback
                        });

                        let selection_result = agents
                            .run_role(
                                AgentRole::ToolCaller,
                                selection_input,
                                router.clone(),
                                selection_cli_output,
                            )
                            .await;

                        let (selection_model, llm_output) = match selection_result {
                            Ok(output) => {
                                let model = output.model;
                                orchestrator
                                    .record_action_event(
                                        run_id,
                                        session_id,
                                        RunActionType::ModelSelected,
                                        Some("node"),
                                        Some(node.id.as_str()),
                                        None,
                                        serde_json::json!({
                                            "node_id": node.id.clone(),
                                            "role": node.role,
                                            "model": model.clone(),
                                            "iteration": iteration,
                                        }),
                                    )
                                    .await;
                                (model, output.content)
                            }
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

                        orchestrator
                            .record_node_progress(
                                run_id,
                                session_id,
                                node.id.as_str(),
                                node.role,
                                "tool_selection_completed",
                                format!(
                                    "Selector produced output for iteration {}",
                                    iteration + 1
                                ),
                                serde_json::json!({
                                    "iteration": iteration,
                                    "model": selection_model.clone(),
                                    "selector_output_preview": summarize_selector_output(&llm_output),
                                }),
                            )
                            .await;

                        // Check for DONE signal
                        let trimmed_output = llm_output.trim();
                        if trimmed_output.eq_ignore_ascii_case("done")
                            || trimmed_output == "\"DONE\""
                            || trimmed_output == "\"done\""
                        {
                            orchestrator
                                .record_node_progress(
                                    run_id,
                                    session_id,
                                    node.id.as_str(),
                                    node.role,
                                    "tool_selection_done",
                                    "Tool selector indicated that no further tool calls are needed",
                                    serde_json::json!({
                                        "iteration": iteration,
                                    }),
                                )
                                .await;
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
                                orchestrator
                                    .record_node_progress(
                                        run_id,
                                        session_id,
                                        node.id.as_str(),
                                        node.role,
                                        "tool_selection_implicit_done",
                                        "Selector returned no executable calls after prior results; treating as completion",
                                        serde_json::json!({
                                            "iteration": iteration,
                                            "selector_output_preview": summarize_selector_output(&llm_output),
                                        }),
                                    )
                                    .await;
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
                                error: Some(format!(
                                    "No executable tool calls were parsed from LLM output. Selector output preview: {}",
                                    summarize_selector_output(&llm_output)
                                )),
                            });
                        }

                        // Execute tool calls for this iteration
                        for call in &tool_calls {
                            let tool_name = call
                                .get("tool_name")
                                .and_then(|v| v.as_str())
                                .unwrap_or("unknown")
                                .to_string();
                            if !allowed_tool_names.contains(&tool_name) {
                                any_failure = true;
                                all_results.push(format!(
                                    "[iter={}] [ERR] {}: tool is not allowed for this node",
                                    iteration, tool_name
                                ));
                                continue;
                            }
                            let arguments = call
                                .get("arguments")
                                .cloned()
                                .unwrap_or(serde_json::json!({}));
                            let arguments_snapshot = arguments.clone();

                            orchestrator
                                .record_node_progress(
                                    run_id,
                                    session_id,
                                    node.id.as_str(),
                                    node.role,
                                    "mcp_tool_started",
                                    format!(
                                        "Calling {} (iteration {})",
                                        tool_name,
                                        iteration + 1
                                    ),
                                    serde_json::json!({
                                        "iteration": iteration,
                                        "tool_name": tool_name.clone(),
                                        "arguments": arguments_snapshot.clone(),
                                    }),
                                )
                                .await;

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
                                                "node_id": node.id.clone(),
                                                "tool_name": tool_name.clone(),
                                                "arguments": arguments_snapshot.clone(),
                                                "succeeded": result.succeeded,
                                                "content": result.content.clone(),
                                                "error": result.error.clone(),
                                                "duration_ms": result.duration_ms,
                                                "iteration": iteration,
                                            }),
                                        )
                                        .await;
                                    if !result.succeeded {
                                        any_failure = true;
                                    }
                                    orchestrator
                                        .record_node_progress(
                                            run_id,
                                            session_id,
                                            node.id.as_str(),
                                            node.role,
                                            "mcp_tool_completed",
                                            format!(
                                                "{} finished with {}",
                                                tool_name,
                                                if result.succeeded { "success" } else { "failure" }
                                            ),
                                            serde_json::json!({
                                                "iteration": iteration,
                                                "tool_name": tool_name.clone(),
                                                "succeeded": result.succeeded,
                                                "duration_ms": result.duration_ms,
                                            }),
                                        )
                                        .await;
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
                                    orchestrator
                                        .record_node_progress(
                                            run_id,
                                            session_id,
                                            node.id.as_str(),
                                            node.role,
                                            "mcp_tool_completed",
                                            format!("{} failed to execute", tool_name),
                                            serde_json::json!({
                                                "iteration": iteration,
                                                "tool_name": tool_name.clone(),
                                                "succeeded": false,
                                                "error": err.to_string(),
                                            }),
                                        )
                                        .await;
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

                        // Check if this is an external project run
                        let external_analysis = repo_analyses.get(&run_id);
                        let (effective_working_dir, enriched_task) =
                            if let Some(ref analysis) = external_analysis {
                                let enriched = format!(
                                    "{}\n\nREPOSITORY MAP:\n{}\n\nTECH STACK: {} ({})\nKey files: {}",
                                    req.task,
                                    analysis.repo_map,
                                    analysis.tech_stack.primary_language,
                                    analysis.tech_stack.frameworks.join(", "),
                                    analysis.key_files.join(", "),
                                );
                                (std::path::PathBuf::from(&analysis.repo_path), enriched)
                            } else {
                                let requested = coder_manager
                                    .default_working_dir()
                                    .to_str()
                                    .filter(|value| !value.is_empty());
                                let working_dir = match session_workspace
                                    .ensure_scoped_dir(session_id, requested)
                                    .await
                                {
                                    Ok(dir) => dir,
                                    Err(e) => {
                                        return Ok(NodeExecutionResult {
                                            node_id: node.id,
                                            role: node.role,
                                            model: backend_kind.to_string(),
                                            output: format!(
                                                "Session workspace setup failed: {}",
                                                e
                                            ),
                                            duration_ms: started.elapsed().as_millis(),
                                            succeeded: false,
                                            error: Some(format!(
                                                "workspace setup failed: {}",
                                                e
                                            )),
                                        });
                                    }
                                };
                                (working_dir, req.task.clone())
                            };

                        let result = coder_manager
                            .run_session_at(
                                run_id,
                                &node.id,
                                backend_kind,
                                &enriched_task,
                                &context_str,
                                &effective_working_dir,
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

                // Add MCP tool availability to context
                let mcp_tools_for_node = if node.mcp_tools.is_empty() {
                    mcp.list_all_tools().await
                } else {
                    skill_loader::filter_tools_for_node(&mcp.list_all_tools().await, &node.mcp_tools)
                };
                if !mcp_tools_for_node.is_empty() {
                    let tool_summary = mcp_tools_for_node
                        .iter()
                        .take(20)
                        .map(|t| format!("- {}: {}", t.name, t.description))
                        .collect::<Vec<_>>()
                        .join("\n");
                    chunks.push(ContextChunk {
                        id: "tools-available".to_string(),
                        scope: ContextScope::AgentPrivate,
                        kind: ContextKind::ToolResults,
                        content: format!(
                            "You have access to MCP tools. To call a tool, include in your response:\n\
                             <tool_call>{{\"tool_name\": \"server/tool\", \"arguments\": {{...}}}}</tool_call>\n\n\
                             Available tools:\n{}",
                            tool_summary
                        ),
                        priority: 0.85,
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

                let node_instructions = node.instructions.clone();
                let input = AgentInput {
                    task: req.task.clone(),
                    instructions: node.instructions,
                    context: optimized.clone(),
                    dependency_outputs: dep_outputs.clone(),
                    brief: brief.clone(),
                    working_dir: Some(cli_working_dir.clone()),
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

                let node_timeout_ms = node.policy.timeout_ms;
                let cli_model = router.cli_model_config();
                let cli_output = cli_model.as_ref().map(|cli_model| {
                    let cli_session_id = format!("cli-{}-{}", run_id, node.id);
                    event_sink(RuntimeEvent::CoderSessionStarted {
                        node_id: node.id.clone(),
                        session_id: cli_session_id.clone(),
                        backend: cli_model.backend.to_string(),
                    });
                    let output_node_id = node.id.clone();
                    let output_session_id = cli_session_id.clone();
                    let output_sink = event_sink.clone();
                    Arc::new(move |stream: &str, content: &str| {
                        if content.trim().is_empty() {
                            return;
                        }
                        output_sink(RuntimeEvent::CoderOutputChunk {
                            node_id: output_node_id.clone(),
                            session_id: output_session_id.clone(),
                            stream: stream.to_string(),
                            content: content.to_string(),
                        });
                    }) as crate::router::CliOutputCallback
                });
                let heartbeat = if let Some(cli_model) = cli_model {
                    orchestrator
                        .record_node_progress(
                            run_id,
                            session_id,
                            node.id.as_str(),
                            node.role,
                            "cli_execution_started",
                            format!(
                                "Executing {} from {}",
                                cli_model.backend,
                                cli_working_dir.display()
                            ),
                            serde_json::json!({
                                "backend": cli_model.backend.to_string(),
                                "working_dir": cli_working_dir.display().to_string(),
                                "timeout_ms": node_timeout_ms,
                            }),
                        )
                        .await;

                    let (stop_tx, mut stop_rx) = tokio::sync::watch::channel(false);
                    let heartbeat_orchestrator = orchestrator.clone();
                    let heartbeat_node_id = node.id.clone();
                    let heartbeat_role = node.role;
                    let heartbeat_working_dir = cli_working_dir.clone();
                    let heartbeat_backend = cli_model.backend.to_string();
                    let heartbeat_handle = tokio::spawn(async move {
                        let heartbeat_started = Instant::now();
                        loop {
                            tokio::select! {
                                _ = sleep(Duration::from_secs(15)) => {
                                    heartbeat_orchestrator
                                        .record_node_progress(
                                            run_id,
                                            session_id,
                                            heartbeat_node_id.as_str(),
                                            heartbeat_role,
                                            "cli_execution_heartbeat",
                                            format!(
                                                "{} still running for {}s",
                                                heartbeat_backend,
                                                heartbeat_started.elapsed().as_secs()
                                            ),
                                            serde_json::json!({
                                                "backend": heartbeat_backend.clone(),
                                                "elapsed_ms": heartbeat_started.elapsed().as_millis(),
                                                "working_dir": heartbeat_working_dir.display().to_string(),
                                                "timeout_ms": node_timeout_ms,
                                            }),
                                        )
                                        .await;
                                }
                                changed = stop_rx.changed() => {
                                    if changed.is_err() || *stop_rx.borrow() {
                                        break;
                                    }
                                }
                            }
                        }
                    });
                    Some((stop_tx, heartbeat_handle))
                } else {
                    None
                };

                let run = agents
                    .run_role_stream(
                        node.role,
                        input,
                        router.clone(),
                        on_token.clone(),
                        cli_output.clone(),
                    )
                    .await;
                if let Some((stop_tx, heartbeat_handle)) = heartbeat {
                    let _ = stop_tx.send(true);
                    let _ = heartbeat_handle.await;
                }
                match run {
                    Ok(output) => {
                        let node_id = node.id.clone();
                        let role = node.role;

                        // Tool augmentation: if LLM output contains <tool_call> tags, execute and re-query
                        let mut current_output = output.content.clone();
                        let mut current_model = output.model.clone();
                        for _tool_round in 0..tool_augment::MAX_TOOL_ROUNDS {
                            let tool_calls = tool_augment::extract_tool_calls(&current_output);
                            if tool_calls.is_empty() {
                                break;
                            }
                            let tool_results =
                                tool_augment::execute_tool_calls(&tool_calls, &mcp, &node.mcp_tools).await;
                            for result in &tool_results {
                                let _ = memory.append_run_action_event(
                                    run_id, session_id,
                                    RunActionType::McpToolCalled,
                                    Some("node"), Some(&node_id), None,
                                    serde_json::json!({
                                        "tool_name": &result.tool_name,
                                        "succeeded": result.succeeded,
                                        "augment_round": _tool_round,
                                    }),
                                ).await;
                            }
                            let results_str = tool_augment::format_tool_results(&tool_results);
                            let followup_instructions = format!(
                                "{}\n\nTOOL RESULTS:\n{}\n\nContinue your response incorporating these tool results.",
                                node_instructions, results_str,
                            );
                            let followup_input = AgentInput {
                                task: req.task.clone(),
                                instructions: followup_instructions,
                                context: optimized.clone(),
                                dependency_outputs: dep_outputs.clone(),
                                brief: brief.clone(),
                                working_dir: Some(cli_working_dir.clone()),
                            };
                            match agents.run_role_stream(
                                node.role,
                                followup_input,
                                router.clone(),
                                on_token.clone(),
                                cli_output.clone(),
                            ).await {
                                Ok(followup) => {
                                    current_output = followup.content;
                                    current_model = followup.model;
                                }
                                Err(_) => break,
                            }
                        }
                        let current_output = tool_augment::strip_tool_call_tags(&current_output);

                        let model = current_model.clone();
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
                                current_output.clone(),
                                0.6,
                                Duration::from_secs(20 * 60),
                            )
                            .await;

                        let _ = memory
                            .remember_long(
                                session_id,
                                "agent_output",
                                current_output.as_str(),
                                0.65,
                                Some(node_id.as_str()),
                            )
                            .await;

                        Ok(NodeExecutionResult {
                            node_id,
                            role,
                            model: current_model,
                            output: current_output,
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

    fn inject_git_and_summarize(
        config: &ValidationConfig,
        completed_node: &AgentNode,
        dynamic_nodes: &mut Vec<AgentNode>,
        run_id: Uuid,
    ) {
        let mut last_dep = completed_node.id.clone();
        let mut depth = completed_node.depth + 1;

        if config.git_auto_commit {
            let mut git_node = AgentNode::new("git_commit", AgentRole::Validator, "git");
            git_node.dependencies = vec![last_dep];
            git_node.depth = depth;
            git_node.policy = ExecutionPolicy {
                timeout_ms: 60_000,
                retry: 0,
                ..ExecutionPolicy::default()
            };
            last_dep = "git_commit".to_string();
            depth += 1;
            dynamic_nodes.push(git_node);
        }

        let mut summarize = AgentNode::new(
            format!("summarize_final_{}", run_id),
            AgentRole::Summarizer,
            "Summarize the code generation, validation, and commit results.",
        );
        summarize.dependencies = vec![last_dep];
        summarize.depth = depth;
        summarize.policy = ExecutionPolicy::default();
        dynamic_nodes.push(summarize);
    }

    fn parse_fix_iteration(node_id: &str) -> u8 {
        // "validate_lint" → 0, "validate_lint_1" → 1, "validate_test_2" → 2
        node_id
            .rsplit('_')
            .next()
            .and_then(|s| s.parse::<u8>().ok())
            .unwrap_or(0)
    }

    fn build_on_completed_fn(
        &self,
        run_id: Uuid,
        session_id: Uuid,
        task: String,
    ) -> OnNodeCompletedFn {
        let memory = self.memory.clone();
        let validation_config = self.validation_config.clone();
        let repo_analyses = self.repo_analyses.clone();
        let orchestrator = self.clone();
        Arc::new(move |node: AgentNode, result: NodeExecutionResult| {
            let task = task.clone();
            let memory = memory.clone();
            let validation_config = validation_config.clone();
            let repo_analyses = repo_analyses.clone();
            let orchestrator = orchestrator.clone();
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
                                sub_node.mcp_tools = subtask.mcp_tools.clone();
                                sub_node.policy = ExecutionPolicy {
                                    max_parallelism: DYNAMIC_SUBTASK_MAX_PARALLELISM,
                                    retry: 1,
                                    timeout_ms: 120_000,
                                    circuit_breaker: 3,
                                    on_dependency_failure: DependencyFailurePolicy::ContinueOnError,
                                    fallback_node: None,
                                };
                                dynamic_nodes.push(sub_node);
                            }
                            orchestrator.normalize_dynamic_nodes_for_runtime(&mut dynamic_nodes);
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
                        dynamic.dependencies = vec![node.id.clone()];
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

                // --- Validator dynamic node injection ---

                if node.role == AgentRole::Validator && !result.succeeded {
                    // Validator failed: inject fix + re-validate (up to max iterations)
                    let iteration = Self::parse_fix_iteration(&node.id);
                    let max_iters = validation_config.max_fix_iterations;

                    let phase = if node.instructions.contains("test") {
                        "test"
                    } else {
                        "lint"
                    };

                    if iteration < max_iters {
                        let next_iter = iteration + 1;

                        // Inject Coder fix node with error context
                        let fix_id = format!("fix_{}_{}", phase, next_iter);
                        let error_output: String =
                            result.output.chars().take(2000).collect();
                        let mut fix_node = AgentNode::new(
                            fix_id.clone(),
                            AgentRole::Coder,
                            format!(
                                "The previous {} validation failed. Fix ONLY the reported errors below.\n\n\
                                 ERRORS:\n{}\n\n\
                                 Do not change unrelated code.",
                                phase, error_output
                            ),
                        );
                        fix_node.dependencies = vec![node.id.clone()];
                        fix_node.depth = node.depth + 1;
                        fix_node.retry_context = Some(result.output.clone());
                        fix_node.policy = ExecutionPolicy {
                            timeout_ms: 180_000,
                            retry: 1,
                            ..ExecutionPolicy::default()
                        };
                        dynamic_nodes.push(fix_node);

                        // Inject re-validate node
                        let revalidate_id =
                            format!("validate_{}_{}", phase, next_iter);
                        let mut revalidate = AgentNode::new(
                            revalidate_id,
                            AgentRole::Validator,
                            phase.to_string(),
                        );
                        revalidate.dependencies = vec![fix_id];
                        revalidate.depth = node.depth + 2;
                        revalidate.policy = ExecutionPolicy {
                            timeout_ms: if phase == "test" {
                                validation_config.test_timeout_ms
                            } else {
                                validation_config.lint_timeout_ms
                            },
                            retry: 0,
                            ..ExecutionPolicy::default()
                        };
                        dynamic_nodes.push(revalidate);

                        let _ = memory
                            .append_run_action_event(
                                run_id,
                                session_id,
                                RunActionType::ReplanTriggered,
                                Some("validator"),
                                Some(&node.id),
                                None,
                                serde_json::json!({
                                    "phase": phase,
                                    "iteration": next_iter,
                                    "reason": result.error,
                                }),
                            )
                            .await;
                    } else {
                        // Max iterations reached: inject fallback summarize
                        let mut summarize_failed = AgentNode::new(
                            "summarize_failed",
                            AgentRole::Summarizer,
                            format!(
                                "Validation failed after {} fix attempts. Summarize the errors and partial results.\n\n\
                                 LAST ERROR:\n{}",
                                max_iters,
                                result.error.as_deref().unwrap_or("unknown")
                            ),
                        );
                        summarize_failed.dependencies = vec![node.id.clone()];
                        summarize_failed.depth = node.depth + 1;
                        summarize_failed.policy = ExecutionPolicy::default();
                        dynamic_nodes.push(summarize_failed);
                    }
                }

                if node.role == AgentRole::Validator && result.succeeded {
                    let is_lint = node.instructions.contains("lint")
                        || (!node.instructions.contains("test")
                            && !node.instructions.contains("git"));
                    let is_test = node.instructions.contains("test");

                    if is_lint {
                        // External project lint success
                        if node.instructions.contains("external") {
                            let analysis = repo_analyses.get(&run_id);
                            let has_test = analysis.as_ref().map(|a| a.detected_commands.has_test()).unwrap_or(false);
                            if has_test {
                                // Inject external_test
                                let mut test_node = AgentNode::new(
                                    "validate_test",
                                    AgentRole::Validator,
                                    "external_test",
                                );
                                test_node.dependencies = vec![node.id.clone()];
                                test_node.depth = node.depth + 1;
                                test_node.policy = ExecutionPolicy {
                                    timeout_ms: validation_config.test_timeout_ms,
                                    retry: 0,
                                    ..Self::default_policy()
                                };
                                dynamic_nodes.push(test_node);
                            } else {
                                Self::inject_git_and_summarize(
                                    &validation_config,
                                    &node,
                                    &mut dynamic_nodes,
                                    run_id,
                                );
                            }
                            orchestrator.normalize_dynamic_nodes_for_runtime(&mut dynamic_nodes);
                            return Ok(dynamic_nodes);
                        }

                        // Lint passed: inject test phase if configured, else summarize
                        if validation_config.has_test() {
                            let test_id = if node.id == "validate_lint" {
                                "validate_test".to_string()
                            } else {
                                let iter = Self::parse_fix_iteration(&node.id);
                                format!("validate_test_{}", iter)
                            };
                            let mut test_node = AgentNode::new(
                                test_id,
                                AgentRole::Validator,
                                "test",
                            );
                            test_node.dependencies = vec![node.id.clone()];
                            test_node.depth = node.depth + 1;
                            test_node.policy = ExecutionPolicy {
                                timeout_ms: validation_config.test_timeout_ms,
                                retry: 0,
                                ..ExecutionPolicy::default()
                            };
                            dynamic_nodes.push(test_node);
                        } else {
                            // No test: go directly to git or summarize
                            Self::inject_git_and_summarize(
                                &validation_config,
                                &node,
                                &mut dynamic_nodes,
                                run_id,
                            );
                        }
                    } else if is_test {
                        // External project test success
                        if node.instructions.contains("external") {
                            Self::inject_git_and_summarize(
                                &validation_config,
                                &node,
                                &mut dynamic_nodes,
                                run_id,
                            );
                            orchestrator.normalize_dynamic_nodes_for_runtime(&mut dynamic_nodes);
                            return Ok(dynamic_nodes);
                        }

                        // Test passed: inject git commit (if configured) then summarize
                        Self::inject_git_and_summarize(
                            &validation_config,
                            &node,
                            &mut dynamic_nodes,
                            run_id,
                        );
                    }
                }

                orchestrator.normalize_dynamic_nodes_for_runtime(&mut dynamic_nodes);
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
                    RuntimeEvent::CoderSessionStarted {
                        node_id,
                        session_id,
                        backend,
                    } => serde_json::json!({
                        "node_id": node_id,
                        "session_id": session_id,
                        "backend": backend,
                        "phase": "coder_session_started",
                    }),
                    RuntimeEvent::CoderOutputChunk {
                        node_id,
                        session_id,
                        stream,
                        content,
                    } => serde_json::json!({
                        "node_id": node_id,
                        "session_id": session_id,
                        "stream": stream,
                        "content": content,
                        "phase": "coder_output_chunk",
                    }),
                    RuntimeEvent::CoderFileChanged {
                        node_id,
                        session_id,
                        file,
                    } => serde_json::json!({
                        "node_id": node_id,
                        "session_id": session_id,
                        "file": file,
                        "phase": "coder_file_changed",
                    }),
                    RuntimeEvent::CoderSessionCompleted {
                        node_id,
                        session_id,
                        files_changed,
                        exit_code,
                    } => serde_json::json!({
                        "node_id": node_id,
                        "session_id": session_id,
                        "files_changed": files_changed,
                        "exit_code": exit_code,
                        "phase": "coder_session_completed",
                    }),
                    RuntimeEvent::InteractiveStep {
                        node_id,
                        iteration,
                        thought,
                        action_type,
                        observation_preview,
                    } => serde_json::json!({
                        "node_id": node_id,
                        "iteration": iteration,
                        "thought": thought,
                        "action_type": action_type,
                        "observation_preview": observation_preview,
                    }),
                    RuntimeEvent::GraphCompleted => {
                        serde_json::json!({ "phase": "graph_completed" })
                    }
                    RuntimeEvent::ValidationCompleted {
                        node_id,
                        phase,
                        passed,
                    } => serde_json::json!({
                        "node_id": node_id,
                        "phase": phase,
                        "passed": passed,
                    }),
                    RuntimeEvent::GitCommitCreated {
                        node_id,
                        commit_hash,
                        commit_message,
                        pushed,
                    } => serde_json::json!({
                        "node_id": node_id,
                        "commit_hash": commit_hash,
                        "commit_message": commit_message,
                        "pushed": pushed,
                    }),
                    RuntimeEvent::RepoCloned {
                        node_id,
                        repo_path,
                        shallow,
                    } => serde_json::json!({
                        "node_id": node_id,
                        "repo_path": repo_path,
                        "shallow": shallow,
                    }),
                    RuntimeEvent::RepoAnalyzed {
                        node_id,
                        primary_language,
                        file_count,
                    } => serde_json::json!({
                        "node_id": node_id,
                        "primary_language": primary_language,
                        "file_count": file_count,
                    }),
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
                    | RuntimeEvent::GraphCompleted
                    | RuntimeEvent::ValidationCompleted { .. }
                    | RuntimeEvent::GitCommitCreated { .. }
                    | RuntimeEvent::InteractiveStep { .. } => SessionEventType::RunProgress,
                    RuntimeEvent::RepoCloned { .. } | RuntimeEvent::RepoAnalyzed { .. } => {
                        SessionEventType::AgentOutput
                    }
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
                    RuntimeEvent::ValidationCompleted {
                        node_id,
                        phase: _,
                        passed,
                    } => {
                        let action = if *passed {
                            RunActionType::ValidationPassed
                        } else {
                            RunActionType::ValidationFailed
                        };
                        (action, Some(node_id.as_str()))
                    }
                    RuntimeEvent::GitCommitCreated { node_id, .. } => {
                        (RunActionType::GitCommitCreated, Some(node_id.as_str()))
                    }
                    RuntimeEvent::RepoCloned { node_id, .. } => {
                        (RunActionType::RepoCloneCompleted, Some(node_id.as_str()))
                    }
                    RuntimeEvent::RepoAnalyzed { node_id, .. } => {
                        (RunActionType::RepoAnalysisCompleted, Some(node_id.as_str()))
                    }
                    RuntimeEvent::InteractiveStep { node_id, .. } => {
                        (RunActionType::InteractiveStep, Some(node_id.as_str()))
                    }
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

fn workflow_node_to_agent_node(wn: &WorkflowNodeTemplate) -> anyhow::Result<AgentNode> {
    if !wn.git_commands.is_empty() && wn.role != AgentRole::Validator {
        anyhow::bail!(
            "workflow node '{}' uses git_commands but role '{}' is not validator",
            wn.id,
            wn.role
        );
    }

    let mut node = AgentNode::new(wn.id.clone(), wn.role, wn.instructions.clone());
    node.dependencies = wn.dependencies.clone();
    node.mcp_tools = wn.mcp_tools.clone();
    node.git_commands = wn.git_commands.clone();
    if !wn.policy.is_null() {
        if let Ok(policy) = serde_json::from_value::<ExecutionPolicy>(wn.policy.clone()) {
            node.policy = policy;
        }
    }
    Ok(node)
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
            | RunActionType::InteractiveStep => {}
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

fn contains_any_keyword(haystack: &str, keywords: &[&str]) -> bool {
    keywords.iter().any(|keyword| haystack.contains(keyword))
}

fn infer_clone_target_dir(repo_url: &str) -> String {
    let candidate = repo_url
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or("cloned-repo")
        .trim_end_matches(".git")
        .trim()
        .to_string()
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
        .collect::<String>()
        .chars()
        .take(80)
        .collect::<String>()
        .trim_matches('.')
        .trim()
        .to_string();

    if candidate.is_empty() {
        "cloned-repo".to_string()
    } else {
        candidate
    }
}

fn format_tool_catalog(tools: &[McpToolDefinition]) -> String {
    tools
        .iter()
        .map(|tool| {
            format!(
                "- {}: {} | {}",
                tool.name,
                tool.description,
                summarize_tool_input_schema(&tool.input_schema)
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn summarize_tool_input_schema(schema: &serde_json::Value) -> String {
    let properties = schema
        .get("properties")
        .and_then(|value| value.as_object())
        .map(|properties| {
            let mut keys = properties.keys().cloned().collect::<Vec<_>>();
            keys.sort();
            keys
        })
        .unwrap_or_default();

    let required = schema
        .get("required")
        .and_then(|value| value.as_array())
        .map(|values| {
            values
                .iter()
                .filter_map(|value| value.as_str().map(ToString::to_string))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    match (properties.is_empty(), required.is_empty()) {
        (true, true) => "args: none".to_string(),
        (false, true) => format!("args: {}", properties.join(", ")),
        (false, false) => format!(
            "args: {} | required: {}",
            properties.join(", "),
            required.join(", ")
        ),
        (true, false) => format!("required: {}", required.join(", ")),
    }
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

fn extract_repo_url_from_text(text: &str) -> Option<String> {
    for word in text.split_whitespace() {
        let trimmed = word.trim_matches(|c: char| {
            !c.is_alphanumeric()
                && c != '/'
                && c != ':'
                && c != '.'
                && c != '-'
                && c != '_'
                && c != '@'
        });
        if (trimmed.starts_with("https://") || trimmed.starts_with("git@"))
            && (trimmed.contains("github.com")
                || trimmed.contains("gitlab.com")
                || trimmed.contains("bitbucket.org")
                || trimmed.ends_with(".git"))
        {
            return Some(trimmed.to_string());
        }
    }
    None
}

fn parse_tool_calls(raw: &str) -> Vec<serde_json::Value> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }

    let mut parsed = Vec::<serde_json::Value>::new();
    let mut seen_candidates = HashSet::<String>::new();
    for candidate in tool_call_parse_candidates(trimmed) {
        if !seen_candidates.insert(candidate.clone()) {
            continue;
        }
        collect_tool_calls_from_text(candidate.as_str(), &mut parsed);
    }

    if parsed.is_empty() {
        parsed.extend(tool_augment::extract_tool_calls(trimmed));
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

fn collect_tool_calls_from_text(raw: &str, out: &mut Vec<serde_json::Value>) {
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(raw) {
        collect_tool_calls(value, out);
        return;
    }

    let stream = serde_json::Deserializer::from_str(raw).into_iter::<serde_json::Value>();
    for value in stream.flatten() {
        collect_tool_calls(value, out);
    }
}

fn tool_call_parse_candidates(raw: &str) -> Vec<String> {
    let mut candidates = vec![raw.trim().to_string()];

    let cleaned = Orchestrator::clean_llm_output(raw);
    if cleaned != raw.trim() {
        candidates.push(cleaned.clone());
    }

    candidates.extend(extract_markdown_code_blocks(raw));
    candidates.extend(extract_balanced_json_fragments(raw, 16));

    if cleaned != raw.trim() {
        candidates.extend(extract_markdown_code_blocks(cleaned.as_str()));
        candidates.extend(extract_balanced_json_fragments(cleaned.as_str(), 16));
    }

    candidates
}

fn extract_markdown_code_blocks(raw: &str) -> Vec<String> {
    let mut blocks = Vec::new();
    let mut cursor = raw;

    while let Some(open_idx) = cursor.find("```") {
        let after_open = &cursor[open_idx + 3..];
        let Some(first_newline) = after_open.find('\n') else {
            break;
        };
        let content_start = open_idx + 3 + first_newline + 1;
        let remainder = &cursor[content_start..];
        let Some(close_idx) = remainder.find("```") else {
            break;
        };
        let content = remainder[..close_idx].trim();
        if !content.is_empty() {
            blocks.push(content.to_string());
        }
        cursor = &remainder[close_idx + 3..];
    }

    blocks
}

fn extract_balanced_json_fragments(raw: &str, max_candidates: usize) -> Vec<String> {
    let mut fragments = Vec::new();

    for (start_idx, ch) in raw.char_indices() {
        if ch != '{' && ch != '[' {
            continue;
        }
        if let Some(end_idx) = find_balanced_json_end(raw, start_idx, ch) {
            let fragment = raw[start_idx..end_idx].trim();
            if !fragment.is_empty() {
                fragments.push(fragment.to_string());
                if fragments.len() >= max_candidates {
                    break;
                }
            }
        }
    }

    fragments
}

fn find_balanced_json_end(raw: &str, start_idx: usize, opening: char) -> Option<usize> {
    let mut expected = vec![match opening {
        '{' => '}',
        '[' => ']',
        _ => return None,
    }];
    let mut in_string = false;
    let mut escaped = false;

    for (offset, ch) in raw[start_idx..].char_indices() {
        if offset == 0 {
            continue;
        }

        if in_string {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }

        match ch {
            '"' => in_string = true,
            '{' => expected.push('}'),
            '[' => expected.push(']'),
            '}' | ']' => {
                if expected.pop() != Some(ch) {
                    return None;
                }
                if expected.is_empty() {
                    return Some(start_idx + offset + ch.len_utf8());
                }
            }
            _ => {}
        }
    }

    None
}

fn summarize_selector_output(raw: &str) -> String {
    let normalized = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut preview = String::new();
    let mut count = 0usize;

    for ch in normalized.chars() {
        if count >= 320 {
            preview.push_str("...");
            return preview;
        }
        preview.push(ch);
        count += 1;
    }

    preview
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
            } else {
                for value in map.into_values() {
                    collect_tool_calls(value, out);
                }
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use crate::types::{CliModelBackendKind, DetectedCommands, TechStack};

    use super::*;

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

        let on_completed = orchestrator.build_on_completed_fn(
            Uuid::new_v4(),
            Uuid::new_v4(),
            "Inspect a remote repository".to_string(),
        );
        let result = on_completed(
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

        let on_completed = orchestrator.build_on_completed_fn(
            Uuid::new_v4(),
            Uuid::new_v4(),
            "Analyze the backend architecture".to_string(),
        );
        let result = on_completed(
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
