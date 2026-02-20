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
    ToolCaller,
}

impl Display for AgentRole {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            AgentRole::Planner => "planner",
            AgentRole::Extractor => "extractor",
            AgentRole::Coder => "coder",
            AgentRole::Summarizer => "summarizer",
            AgentRole::Fallback => "fallback",
            AgentRole::ToolCaller => "tool_caller",
        };
        write!(f, "{s}")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Queued,
    Cancelling,
    Cancelled,
    Paused,
    Running,
    Succeeded,
    Failed,
}

impl RunStatus {
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            RunStatus::Cancelled | RunStatus::Succeeded | RunStatus::Failed
        )
    }
}

impl Display for RunStatus {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            RunStatus::Queued => "queued",
            RunStatus::Cancelling => "cancelling",
            RunStatus::Cancelled => "cancelled",
            RunStatus::Paused => "paused",
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
    RunCancelled,
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
    pub workflow_id: Option<String>,
    pub workflow_params: Option<serde_json::Value>,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunActionType {
    RunQueued,
    RunStarted,
    RunCancelRequested,
    RunPauseRequested,
    RunResumed,
    GraphInitialized,
    NodeStarted,
    NodeCompleted,
    NodeFailed,
    NodeSkipped,
    DynamicNodeAdded,
    GraphCompleted,
    ModelSelected,
    RunFinished,
    WebhookDispatched,
    McpToolCalled,
    SubtaskPlanned,
}

impl Display for RunActionType {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            RunActionType::RunQueued => "run_queued",
            RunActionType::RunStarted => "run_started",
            RunActionType::RunCancelRequested => "run_cancel_requested",
            RunActionType::RunPauseRequested => "run_pause_requested",
            RunActionType::RunResumed => "run_resumed",
            RunActionType::GraphInitialized => "graph_initialized",
            RunActionType::NodeStarted => "node_started",
            RunActionType::NodeCompleted => "node_completed",
            RunActionType::NodeFailed => "node_failed",
            RunActionType::NodeSkipped => "node_skipped",
            RunActionType::DynamicNodeAdded => "dynamic_node_added",
            RunActionType::GraphCompleted => "graph_completed",
            RunActionType::ModelSelected => "model_selected",
            RunActionType::RunFinished => "run_finished",
            RunActionType::WebhookDispatched => "webhook_dispatched",
            RunActionType::McpToolCalled => "mcp_tool_called",
            RunActionType::SubtaskPlanned => "subtask_planned",
        };
        write!(f, "{s}")
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunActionEvent {
    pub seq: i64,
    pub event_id: String,
    pub run_id: Uuid,
    pub session_id: Uuid,
    pub timestamp: DateTime<Utc>,
    pub action: RunActionType,
    pub actor_type: Option<String>,
    pub actor_id: Option<String>,
    pub cause_event_id: Option<String>,
    pub payload: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceEdge {
    pub from: String,
    pub to: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeTraceState {
    pub node_id: String,
    pub role: Option<AgentRole>,
    pub dependencies: Vec<String>,
    pub status: String,
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
    pub duration_ms: Option<u128>,
    pub retries: u32,
    pub model: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunTraceGraph {
    pub nodes: Vec<NodeTraceState>,
    pub edges: Vec<TraceEdge>,
    pub active_nodes: Vec<String>,
    pub completed_nodes: usize,
    pub failed_nodes: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunTrace {
    pub run_id: Uuid,
    pub session_id: Uuid,
    pub status: Option<RunStatus>,
    pub events: Vec<RunActionEvent>,
    pub graph: RunTraceGraph,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebhookDeliveryRecord {
    pub id: i64,
    pub endpoint_id: String,
    pub event: String,
    pub event_id: String,
    pub url: String,
    pub attempts: u32,
    pub delivered: bool,
    pub dead_letter: bool,
    pub status_code: Option<u16>,
    pub error: Option<String>,
    pub payload: serde_json::Value,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunBehaviorLane {
    pub node_id: String,
    pub role: Option<AgentRole>,
    pub status: String,
    pub dependencies: Vec<String>,
    pub start_offset_ms: Option<i64>,
    pub end_offset_ms: Option<i64>,
    pub duration_ms: Option<u128>,
    pub retries: u32,
    pub model: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunBehaviorActionCount {
    pub action: String,
    pub count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunBehaviorSummary {
    pub total_duration_ms: Option<u128>,
    pub lane_count: usize,
    pub completed_nodes: usize,
    pub failed_nodes: usize,
    pub critical_path_nodes: Vec<String>,
    pub critical_path_duration_ms: u128,
    pub bottleneck_node_id: Option<String>,
    pub bottleneck_duration_ms: Option<u128>,
    pub peak_parallelism: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunBehaviorView {
    pub run_id: Uuid,
    pub session_id: Uuid,
    pub status: Option<RunStatus>,
    pub window_start: Option<DateTime<Utc>>,
    pub window_end: Option<DateTime<Utc>>,
    pub active_nodes: Vec<String>,
    pub lanes: Vec<RunBehaviorLane>,
    pub action_mix: Vec<RunBehaviorActionCount>,
    pub summary: RunBehaviorSummary,
}

// --- MCP Types ---

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpToolDefinition {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpToolCallResult {
    pub tool_name: String,
    pub succeeded: bool,
    pub content: String,
    pub error: Option<String>,
    pub duration_ms: u128,
}

// --- Task Decomposition Types ---

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubtaskPlan {
    pub subtasks: Vec<SubtaskDefinition>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubtaskDefinition {
    pub id: String,
    pub description: String,
    pub agent_role: AgentRole,
    pub dependencies: Vec<String>,
    #[serde(default)]
    pub mcp_tools: Vec<String>,
    pub instructions: String,
}

// --- Workflow Types ---

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowTemplate {
    pub id: String,
    pub name: String,
    pub description: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub source_run_id: Option<Uuid>,
    pub graph_template: WorkflowGraphTemplate,
    pub parameters: Vec<WorkflowParameter>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowGraphTemplate {
    pub nodes: Vec<WorkflowNodeTemplate>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowNodeTemplate {
    pub id: String,
    pub role: AgentRole,
    pub instructions: String,
    pub dependencies: Vec<String>,
    #[serde(default)]
    pub mcp_tools: Vec<String>,
    pub policy: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowParameter {
    pub name: String,
    pub description: String,
    pub default_value: Option<String>,
}
