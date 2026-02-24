pub mod discord;
pub mod slack;

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use axum::Router;
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use tracing::error;
use uuid::Uuid;

use crate::orchestrator::Orchestrator;
use crate::types::{RunRecord, RunSubmission, SessionSummary, TaskProfile};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Platform {
    Slack,
    Discord,
    Web,
}

impl std::fmt::Display for Platform {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Platform::Slack => write!(f, "slack"),
            Platform::Discord => write!(f, "discord"),
            Platform::Web => write!(f, "web"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GatewayAction {
    SubmitRun {
        task: String,
        profile: TaskProfile,
    },
    CancelRun {
        run_id: Uuid,
    },
    PauseRun {
        run_id: Uuid,
    },
    ResumeRun {
        run_id: Uuid,
    },
    RetryRun {
        run_id: Uuid,
    },
    CloneRun {
        run_id: Uuid,
        target_session: Option<Uuid>,
    },
    GetRun {
        run_id: Uuid,
    },
    ListRuns {
        session_id: Option<Uuid>,
        limit: usize,
    },
    ListSessions {
        limit: usize,
    },
    Help,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageOrigin {
    pub platform: Platform,
    pub channel_id: String,
    pub thread_id: Option<String>,
    pub user_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GatewayCommand {
    pub action: GatewayAction,
    pub origin: MessageOrigin,
    pub session_id: Option<Uuid>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GatewayResponsePayload {
    RunSubmitted(RunSubmission),
    RunDetails(Box<RunRecord>),
    RunAction {
        run_id: Uuid,
        action: String,
        success: bool,
    },
    RunList(Vec<RunRecord>),
    SessionList(Vec<SessionSummary>),
    HelpText(String),
    Error(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GatewayResponse {
    pub origin: MessageOrigin,
    pub payload: GatewayResponsePayload,
}

#[async_trait]
pub trait GatewayAdapter: Send + Sync + 'static {
    fn platform(&self) -> Platform;
    fn routes(&self, manager: Arc<GatewayManager>) -> Router;
    async fn start_background(&self, manager: Arc<GatewayManager>) -> anyhow::Result<()>;
    async fn deliver_response(&self, response: GatewayResponse) -> anyhow::Result<()>;
}

pub struct GatewayManager {
    pub orchestrator: Orchestrator,
    adapters: Vec<Arc<dyn GatewayAdapter>>,
    session_map: DashMap<(Platform, String), Uuid>,
}

impl GatewayManager {
    pub fn new(orchestrator: Orchestrator) -> Self {
        Self {
            orchestrator,
            adapters: Vec::new(),
            session_map: DashMap::new(),
        }
    }

    pub fn register_adapter(&mut self, adapter: Arc<dyn GatewayAdapter>) {
        self.adapters.push(adapter);
    }

    pub async fn resolve_session(
        &self,
        platform: Platform,
        channel_id: &str,
        explicit_session: Option<Uuid>,
    ) -> anyhow::Result<Uuid> {
        if let Some(sid) = explicit_session {
            return Ok(sid);
        }
        let key = (platform, channel_id.to_string());
        if let Some(entry) = self.session_map.get(&key) {
            return Ok(*entry.value());
        }
        let session_id = Uuid::new_v4();
        self.orchestrator.create_session(session_id).await?;
        self.session_map.insert(key, session_id);
        Ok(session_id)
    }

    pub async fn handle_command(&self, cmd: GatewayCommand) -> GatewayResponse {
        let origin = cmd.origin.clone();
        let payload = match self.execute(cmd).await {
            Ok(p) => p,
            Err(err) => GatewayResponsePayload::Error(err.to_string()),
        };
        GatewayResponse { origin, payload }
    }

    async fn execute(&self, cmd: GatewayCommand) -> anyhow::Result<GatewayResponsePayload> {
        match cmd.action {
            GatewayAction::SubmitRun { task, profile } => {
                let session_id = self
                    .resolve_session(cmd.origin.platform, &cmd.origin.channel_id, cmd.session_id)
                    .await?;
                let req = crate::types::RunRequest {
                    task,
                    profile,
                    session_id: Some(session_id),
                    workflow_id: None,
                    workflow_params: None,
                };
                let sub = self.orchestrator.submit_run(req).await?;
                Ok(GatewayResponsePayload::RunSubmitted(sub))
            }
            GatewayAction::CancelRun { run_id } => {
                let success = self.orchestrator.cancel_run(run_id).await?;
                Ok(GatewayResponsePayload::RunAction {
                    run_id,
                    action: "cancel".to_string(),
                    success,
                })
            }
            GatewayAction::PauseRun { run_id } => {
                let success = self.orchestrator.pause_run(run_id).await?;
                Ok(GatewayResponsePayload::RunAction {
                    run_id,
                    action: "pause".to_string(),
                    success,
                })
            }
            GatewayAction::ResumeRun { run_id } => {
                let success = self.orchestrator.resume_run(run_id).await?;
                Ok(GatewayResponsePayload::RunAction {
                    run_id,
                    action: "resume".to_string(),
                    success,
                })
            }
            GatewayAction::RetryRun { run_id } => {
                let sub = self.orchestrator.retry_run(run_id).await?;
                Ok(GatewayResponsePayload::RunSubmitted(sub))
            }
            GatewayAction::CloneRun {
                run_id,
                target_session,
            } => {
                let sub = self.orchestrator.clone_run(run_id, target_session).await?;
                Ok(GatewayResponsePayload::RunSubmitted(sub))
            }
            GatewayAction::GetRun { run_id } => {
                let run = self
                    .orchestrator
                    .get_run(run_id)
                    .await?
                    .ok_or_else(|| anyhow::anyhow!("run not found"))?;
                Ok(GatewayResponsePayload::RunDetails(Box::new(run)))
            }
            GatewayAction::ListRuns { session_id, limit } => {
                let runs = if let Some(sid) = session_id {
                    self.orchestrator.list_session_runs(sid, limit).await?
                } else {
                    self.orchestrator.list_recent_runs(limit).await?
                };
                Ok(GatewayResponsePayload::RunList(runs))
            }
            GatewayAction::ListSessions { limit } => {
                let sessions = self.orchestrator.list_sessions(limit).await?;
                Ok(GatewayResponsePayload::SessionList(sessions))
            }
            GatewayAction::Help => Ok(GatewayResponsePayload::HelpText(Self::help_text())),
        }
    }

    fn help_text() -> String {
        [
            "Agent Orchestrator Commands:",
            "  run <task> [--profile planning|extraction|coding|general]",
            "  cancel <run_id>",
            "  pause <run_id>",
            "  resume <run_id>",
            "  retry <run_id>",
            "  clone <run_id> [--session <session_id>]",
            "  status <run_id>",
            "  runs [--session <session_id>] [--limit N]",
            "  sessions [--limit N]",
            "  help",
        ]
        .join("\n")
    }

    pub fn build_router(self: &Arc<Self>) -> Router {
        let mut router = Router::new();
        for adapter in &self.adapters {
            let adapter_routes = adapter.routes(Arc::clone(self));
            router = router.merge(adapter_routes);
        }
        router
    }

    pub async fn start_all_backgrounds(self: &Arc<Self>) -> anyhow::Result<()> {
        for adapter in &self.adapters {
            adapter.start_background(Arc::clone(self)).await?;
        }
        Ok(())
    }

    pub fn adapter_for(&self, platform: Platform) -> Option<&Arc<dyn GatewayAdapter>> {
        self.adapters.iter().find(|a| a.platform() == platform)
    }
}

/// Wait for a run to reach terminal status, then deliver the result via the adapter.
pub async fn poll_and_deliver(
    manager: Arc<GatewayManager>,
    adapter: Arc<dyn GatewayAdapter>,
    run_id: Uuid,
    origin: MessageOrigin,
) {
    loop {
        tokio::time::sleep(Duration::from_millis(500)).await;
        match manager.orchestrator.get_run(run_id).await {
            Ok(Some(run)) if run.status.is_terminal() => {
                let response = GatewayResponse {
                    origin,
                    payload: GatewayResponsePayload::RunDetails(Box::new(run)),
                };
                if let Err(err) = adapter.deliver_response(response).await {
                    error!("failed to deliver run completion for {run_id}: {err}");
                }
                break;
            }
            Ok(Some(_)) => continue,
            Ok(None) => {
                error!("run {run_id} not found during poll");
                break;
            }
            Err(err) => {
                error!("error polling run {run_id}: {err}");
                break;
            }
        }
    }
}

pub fn parse_profile(s: &str) -> TaskProfile {
    match s.to_lowercase().as_str() {
        "planning" => TaskProfile::Planning,
        "extraction" => TaskProfile::Extraction,
        "coding" => TaskProfile::Coding,
        _ => TaskProfile::General,
    }
}

pub fn parse_gateway_text(text: &str, origin: MessageOrigin) -> GatewayCommand {
    let trimmed = text.trim();
    let parts: Vec<&str> = trimmed.splitn(2, char::is_whitespace).collect();
    let subcommand = parts.first().map(|s| s.to_lowercase()).unwrap_or_default();
    let rest = parts.get(1).copied().unwrap_or("");

    let action = match subcommand.as_str() {
        "run" => {
            let (task, profile) = parse_run_args(rest);
            GatewayAction::SubmitRun { task, profile }
        }
        "cancel" => GatewayAction::CancelRun {
            run_id: parse_uuid_arg(rest),
        },
        "pause" => GatewayAction::PauseRun {
            run_id: parse_uuid_arg(rest),
        },
        "resume" => GatewayAction::ResumeRun {
            run_id: parse_uuid_arg(rest),
        },
        "retry" => GatewayAction::RetryRun {
            run_id: parse_uuid_arg(rest),
        },
        "clone" => {
            let (run_id, target_session) = parse_clone_args(rest);
            GatewayAction::CloneRun {
                run_id,
                target_session,
            }
        }
        "status" => GatewayAction::GetRun {
            run_id: parse_uuid_arg(rest),
        },
        "runs" => {
            let (session_id, limit) = parse_list_args(rest);
            GatewayAction::ListRuns { session_id, limit }
        }
        "sessions" => {
            let limit = parse_limit_arg(rest);
            GatewayAction::ListSessions { limit }
        }
        "help" => GatewayAction::Help,
        _ => GatewayAction::SubmitRun {
            task: trimmed.to_string(),
            profile: TaskProfile::General,
        },
    };

    GatewayCommand {
        action,
        origin,
        session_id: None,
    }
}

fn parse_run_args(text: &str) -> (String, TaskProfile) {
    let mut task_parts = Vec::new();
    let mut profile = TaskProfile::General;
    let mut tokens = text.split_whitespace().peekable();

    while let Some(tok) = tokens.next() {
        if tok == "--profile" {
            if let Some(p) = tokens.next() {
                profile = parse_profile(p);
            }
        } else {
            task_parts.push(tok);
        }
    }

    (task_parts.join(" "), profile)
}

fn parse_uuid_arg(text: &str) -> Uuid {
    text.trim()
        .split_whitespace()
        .next()
        .and_then(|s| Uuid::parse_str(s).ok())
        .unwrap_or_default()
}

fn parse_clone_args(text: &str) -> (Uuid, Option<Uuid>) {
    let mut run_id = Uuid::default();
    let mut target_session = None;
    let mut tokens = text.split_whitespace().peekable();

    if let Some(tok) = tokens.next() {
        run_id = Uuid::parse_str(tok).unwrap_or_default();
    }

    while let Some(tok) = tokens.next() {
        if tok == "--session" {
            if let Some(sid) = tokens.next() {
                target_session = Uuid::parse_str(sid).ok();
            }
        }
    }

    (run_id, target_session)
}

fn parse_list_args(text: &str) -> (Option<Uuid>, usize) {
    let mut session_id = None;
    let mut limit = 20_usize;
    let mut tokens = text.split_whitespace().peekable();

    while let Some(tok) = tokens.next() {
        match tok {
            "--session" => {
                if let Some(sid) = tokens.next() {
                    session_id = Uuid::parse_str(sid).ok();
                }
            }
            "--limit" => {
                if let Some(l) = tokens.next() {
                    limit = l.parse().unwrap_or(20).clamp(1, 100);
                }
            }
            _ => {}
        }
    }

    (session_id, limit)
}

fn parse_limit_arg(text: &str) -> usize {
    let mut limit = 20_usize;
    let mut tokens = text.split_whitespace().peekable();

    while let Some(tok) = tokens.next() {
        if tok == "--limit" {
            if let Some(l) = tokens.next() {
                limit = l.parse().unwrap_or(20).clamp(1, 100);
            }
        }
    }

    limit
}
