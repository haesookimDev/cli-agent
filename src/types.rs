use std::fmt::{Display, Formatter};

use chrono::{DateTime, Utc};
use clap::ValueEnum;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum TaskProfile {
    Planning,
    Extraction,
    Coding,
    General,
}

impl Default for TaskProfile {
    fn default() -> Self {
        Self::General
    }
}

impl Display for TaskProfile {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            TaskProfile::Planning => "planning",
            TaskProfile::Extraction => "extraction",
            TaskProfile::Coding => "coding",
            TaskProfile::General => "general",
        };
        write!(f, "{s}")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentRole {
    Planner,
    Extractor,
    Coder,
    Summarizer,
    Fallback,
}

impl Display for AgentRole {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            AgentRole::Planner => "planner",
            AgentRole::Extractor => "extractor",
            AgentRole::Coder => "coder",
            AgentRole::Summarizer => "summarizer",
            AgentRole::Fallback => "fallback",
        };
        write!(f, "{s}")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Queued,
    Running,
    Succeeded,
    Failed,
}

impl RunStatus {
    pub fn is_terminal(&self) -> bool {
        matches!(self, RunStatus::Succeeded | RunStatus::Failed)
    }
}

impl Display for RunStatus {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            RunStatus::Queued => "queued",
            RunStatus::Running => "running",
            RunStatus::Succeeded => "succeeded",
            RunStatus::Failed => "failed",
        };
        write!(f, "{s}")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionEventType {
    UserMessage,
    AgentStarted,
    AgentOutput,
    ToolCall,
    ModelDecision,
    MemoryWrite,
    SessionSummary,
    RunProgress,
    RunFailed,
    RunCompleted,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionEvent {
    pub session_id: Uuid,
    pub run_id: Option<Uuid>,
    pub event_type: SessionEventType,
    pub timestamp: DateTime<Utc>,
    pub payload: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunRequest {
    pub task: String,
    #[serde(default)]
    pub profile: TaskProfile,
    pub session_id: Option<Uuid>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunSubmission {
    pub run_id: Uuid,
    pub session_id: Uuid,
    pub status: RunStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentExecutionRecord {
    pub node_id: String,
    pub role: AgentRole,
    pub model: String,
    pub output: String,
    pub duration_ms: u128,
    pub succeeded: bool,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunRecord {
    pub run_id: Uuid,
    pub session_id: Uuid,
    pub task: String,
    pub profile: TaskProfile,
    pub status: RunStatus,
    pub created_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
    pub outputs: Vec<AgentExecutionRecord>,
    pub error: Option<String>,
    pub timeline: Vec<String>,
}

impl RunRecord {
    pub fn new_queued(run_id: Uuid, session_id: Uuid, task: String, profile: TaskProfile) -> Self {
        Self {
            run_id,
            session_id,
            task,
            profile,
            status: RunStatus::Queued,
            created_at: Utc::now(),
            started_at: None,
            finished_at: None,
            outputs: Vec::new(),
            error: None,
            timeline: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StructuredBrief {
    pub goal: String,
    pub constraints: Vec<String>,
    pub decisions: Vec<String>,
    pub references: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReplayEvent {
    pub line: usize,
    pub event: SessionEvent,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryHit {
    pub id: String,
    pub content: String,
    pub importance: f64,
    pub created_at: DateTime<Utc>,
    pub score: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebhookEndpoint {
    pub id: String,
    pub url: String,
    pub events: Vec<String>,
    pub secret: String,
    pub enabled: bool,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionSummary {
    pub session_id: Uuid,
    pub created_at: DateTime<Utc>,
    pub run_count: usize,
    pub last_run_at: Option<DateTime<Utc>>,
    pub last_task: Option<String>,
}
