use std::collections::HashMap;
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
    Analyzer,
    Reviewer,
    Scheduler,
    ConfigManager,
    Validator,
}

impl AgentRole {
    /// Returns all agent role variants.
    pub fn all() -> &'static [AgentRole] {
        &[
            AgentRole::Planner,
            AgentRole::Extractor,
            AgentRole::Coder,
            AgentRole::Summarizer,
            AgentRole::Fallback,
            AgentRole::ToolCaller,
            AgentRole::Analyzer,
            AgentRole::Reviewer,
            AgentRole::Scheduler,
            AgentRole::ConfigManager,
            AgentRole::Validator,
        ]
    }
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
            AgentRole::Analyzer => "analyzer",
            AgentRole::Reviewer => "reviewer",
            AgentRole::Scheduler => "scheduler",
            AgentRole::ConfigManager => "config_manager",
            AgentRole::Validator => "validator",
        };
        write!(f, "{s}")
    }
}

// --- Agent Persona ---

/// Personality traits that influence how an agent communicates and works.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersonalityTraits {
    /// 0.0–1.0: how thorough / detail-oriented the agent is.
    #[serde(default = "default_mid")]
    pub thoroughness: f32,
    /// 0.0–1.0: creative vs. conservative approach.
    #[serde(default = "default_mid")]
    pub creativity: f32,
    /// 0.0–1.0: strictness when reviewing code / PRs.
    #[serde(default = "default_mid")]
    pub strictness: f32,
    /// 0.0–1.0: verbosity in explanations and comments.
    #[serde(default = "default_mid")]
    pub verbosity: f32,
}

fn default_mid() -> f32 {
    0.5
}

impl Default for PersonalityTraits {
    fn default() -> Self {
        Self {
            thoroughness: 0.5,
            creativity: 0.5,
            strictness: 0.5,
            verbosity: 0.5,
        }
    }
}

/// A persona gives an agent a unique identity for GitHub collaboration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentPersona {
    /// Human-readable display name, e.g. "김지훈" or "Alex Chen".
    pub display_name: String,
    /// Job title, e.g. "Senior Backend Engineer".
    pub title: String,
    /// Username shown in GitHub comments/signatures.
    pub github_username: String,
    /// Optional avatar image URL.
    #[serde(default)]
    pub avatar_url: Option<String>,
    /// Short biography describing expertise and style.
    #[serde(default)]
    pub bio: String,
    /// Personality configuration.
    #[serde(default)]
    pub personality: PersonalityTraits,
    /// Areas of expertise, e.g. ["rust", "performance", "systems"].
    #[serde(default)]
    pub expertise: Vec<String>,
    /// Communication style tag, e.g. "concise-technical", "friendly-mentor".
    #[serde(default = "default_communication_style")]
    pub communication_style: String,
}

fn default_communication_style() -> String {
    "balanced".to_string()
}

// --- GitHub Activity ---

/// Represents a recorded GitHub activity performed by an agent persona.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitHubActivity {
    pub id: String,
    pub run_id: Uuid,
    pub session_id: Uuid,
    pub persona_name: String,
    pub activity_type: GitHubActivityType,
    #[serde(default)]
    pub github_url: Option<String>,
    #[serde(default)]
    pub target_number: Option<i64>,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub body_preview: String,
    #[serde(default)]
    pub metadata: serde_json::Value,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GitHubActivityType {
    IssueCreated,
    IssueCommented,
    IssueClosed,
    PrCreated,
    PrReviewed,
    PrCommented,
    PrMerged,
    BranchCreated,
}

impl Display for GitHubActivityType {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            GitHubActivityType::IssueCreated => "issue_created",
            GitHubActivityType::IssueCommented => "issue_commented",
            GitHubActivityType::IssueClosed => "issue_closed",
            GitHubActivityType::PrCreated => "pr_created",
            GitHubActivityType::PrReviewed => "pr_reviewed",
            GitHubActivityType::PrCommented => "pr_commented",
            GitHubActivityType::PrMerged => "pr_merged",
            GitHubActivityType::BranchCreated => "branch_created",
        };
        write!(f, "{s}")
    }
}

// --- Task Classification ---

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskType {
    SimpleQuery,
    Analysis,
    CodeGeneration,
    Configuration,
    ConfigQuery,
    ToolOperation,
    ExternalProject,
    Interactive,
    Complex,
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
    pub repo_url: Option<String>,
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
pub struct SessionMemoryItem {
    pub id: String,
    pub session_id: Uuid,
    pub scope: String,
    pub content: String,
    pub importance: f64,
    pub source_refs: Vec<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeItem {
    pub id: String,
    pub topic: String,
    pub content: String,
    pub importance: f64,
    pub access_count: u64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
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
    NodeProgress,
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
    NodeTokenChunk,
    SubtaskPlanned,
    VerificationStarted,
    VerificationComplete,
    ReplanTriggered,
    TerminalSuggested,
    CoderSessionStarted,
    CoderSessionCompleted,
    ValidationPassed,
    ValidationFailed,
    GitCommitCreated,
    GitPushCompleted,
    RepoCloneCompleted,
    RepoAnalysisCompleted,
    InteractiveStep,
    GitHubIssueCreated,
    GitHubIssueCommented,
    GitHubIssueClosed,
    GitHubPrCreated,
    GitHubPrReviewed,
    GitHubPrCommented,
    GitHubPrMerged,
    GitHubBranchCreated,
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
            RunActionType::NodeProgress => "node_progress",
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
            RunActionType::NodeTokenChunk => "node_token_chunk",
            RunActionType::SubtaskPlanned => "subtask_planned",
            RunActionType::VerificationStarted => "verification_started",
            RunActionType::VerificationComplete => "verification_complete",
            RunActionType::ReplanTriggered => "replan_triggered",
            RunActionType::TerminalSuggested => "terminal_suggested",
            RunActionType::CoderSessionStarted => "coder_session_started",
            RunActionType::CoderSessionCompleted => "coder_session_completed",
            RunActionType::ValidationPassed => "validation_passed",
            RunActionType::ValidationFailed => "validation_failed",
            RunActionType::GitCommitCreated => "git_commit_created",
            RunActionType::GitPushCompleted => "git_push_completed",
            RunActionType::RepoCloneCompleted => "repo_clone_completed",
            RunActionType::RepoAnalysisCompleted => "repo_analysis_completed",
            RunActionType::InteractiveStep => "interactive_step",
            RunActionType::GitHubIssueCreated => "github_issue_created",
            RunActionType::GitHubIssueCommented => "github_issue_commented",
            RunActionType::GitHubIssueClosed => "github_issue_closed",
            RunActionType::GitHubPrCreated => "github_pr_created",
            RunActionType::GitHubPrReviewed => "github_pr_reviewed",
            RunActionType::GitHubPrCommented => "github_pr_commented",
            RunActionType::GitHubPrMerged => "github_pr_merged",
            RunActionType::GitHubBranchCreated => "github_branch_created",
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

// --- Chat Types ---

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChatRole {
    User,
    Agent,
    System,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub id: String,
    pub session_id: Uuid,
    pub run_id: Option<Uuid>,
    pub role: ChatRole,
    pub content: String,
    pub agent_role: Option<AgentRole>,
    pub model: Option<String>,
    pub timestamp: DateTime<Utc>,
}

// --- Settings Types ---

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppSettings {
    pub default_profile: TaskProfile,
    pub preferred_model: Option<String>,
    pub disabled_models: Vec<String>,
    pub disabled_providers: Vec<String>,
    #[serde(default)]
    pub cli_model_enabled: bool,
    #[serde(default)]
    pub cli_model_backend: Option<CliModelBackendKind>,
    #[serde(default = "default_cli_model_command")]
    pub cli_model_command: String,
    #[serde(default)]
    pub cli_model_args: Vec<String>,
    #[serde(default = "default_cli_model_timeout_ms")]
    pub cli_model_timeout_ms: u64,
    #[serde(default)]
    pub cli_model_only: bool,
    #[serde(default = "default_terminal_command")]
    pub terminal_command: String,
    #[serde(default = "default_terminal_args")]
    pub terminal_args: Vec<String>,
    #[serde(default)]
    pub terminal_auto_spawn: bool,
    #[serde(default = "default_vllm_base_url")]
    pub vllm_base_url: String,
    #[serde(default)]
    pub vllm_custom_model: Option<String>,
}

pub fn default_terminal_command() -> String {
    std::env::var("SHELL")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "bash".to_string())
}

pub fn default_terminal_args() -> Vec<String> {
    Vec::new()
}

fn default_cli_model_command() -> String {
    String::new()
}

fn default_cli_model_timeout_ms() -> u64 {
    300_000
}

pub fn default_vllm_base_url() -> String {
    std::env::var("VLLM_BASE_URL")
        .unwrap_or_else(|_| "http://127.0.0.1:8000".to_string())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SettingsPatch {
    pub default_profile: Option<TaskProfile>,
    pub preferred_model: Option<Option<String>>,
    pub disabled_models: Option<Vec<String>>,
    pub disabled_providers: Option<Vec<String>>,
    pub cli_model_enabled: Option<bool>,
    pub cli_model_backend: Option<Option<CliModelBackendKind>>,
    pub cli_model_command: Option<String>,
    pub cli_model_args: Option<Vec<String>>,
    pub cli_model_timeout_ms: Option<u64>,
    pub cli_model_only: Option<bool>,
    pub terminal_command: Option<String>,
    pub terminal_args: Option<Vec<String>>,
    pub terminal_auto_spawn: Option<bool>,
    pub vllm_base_url: Option<String>,
    pub vllm_custom_model: Option<Option<String>>,
}

// --- Cron Schedule Types ---

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CronSchedule {
    pub id: Uuid,
    pub workflow_id: String,
    pub cron_expr: String,
    pub enabled: bool,
    pub parameters: Option<serde_json::Value>,
    pub last_run_at: Option<DateTime<Utc>>,
    pub next_run_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateScheduleRequest {
    pub workflow_id: String,
    pub cron_expr: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    pub parameters: Option<serde_json::Value>,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateScheduleRequest {
    pub cron_expr: Option<String>,
    pub enabled: Option<bool>,
    pub parameters: Option<Option<serde_json::Value>>,
}

// --- MCP Types ---

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpToolDefinition {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server_name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpToolCallResult {
    pub tool_name: String,
    pub succeeded: bool,
    pub content: String,
    pub error: Option<String>,
    pub duration_ms: u128,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerConfig {
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub command: String,
    pub args: String,
    #[serde(default)]
    pub env: HashMap<String, String>,
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkillSource {
    File,
    Run,
    Api,
}

impl Default for SkillSource {
    fn default() -> Self {
        Self::Api
    }
}

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
    #[serde(default)]
    pub source: SkillSource,
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
    #[serde(default)]
    pub git_commands: Vec<String>,
    pub policy: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowParameter {
    pub name: String,
    pub description: String,
    pub default_value: Option<String>,
}

// --- Coder Backend Types ---

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CoderBackendKind {
    ClaudeCode,
    Codex,
    Llm,
}

impl Default for CoderBackendKind {
    fn default() -> Self {
        Self::Llm
    }
}

impl Display for CoderBackendKind {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            CoderBackendKind::ClaudeCode => "claude_code",
            CoderBackendKind::Codex => "codex",
            CoderBackendKind::Llm => "llm",
        };
        write!(f, "{s}")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CliModelBackendKind {
    ClaudeCode,
    Codex,
}

impl CliModelBackendKind {
    pub fn default_command(self) -> &'static str {
        match self {
            CliModelBackendKind::ClaudeCode => "claude",
            CliModelBackendKind::Codex => "codex",
        }
    }

    pub fn default_model_id(self) -> &'static str {
        match self {
            CliModelBackendKind::ClaudeCode => "claude-code-cli",
            CliModelBackendKind::Codex => "codex-cli",
        }
    }
}

impl Display for CliModelBackendKind {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            CliModelBackendKind::ClaudeCode => "claude_code",
            CliModelBackendKind::Codex => "codex",
        };
        write!(f, "{s}")
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CliModelConfig {
    pub backend: CliModelBackendKind,
    pub command: String,
    pub args: Vec<String>,
    pub timeout_ms: u64,
    pub cli_only: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoderOutputChunk {
    pub session_id: String,
    pub stream: String,
    pub content: String,
    pub timestamp: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoderFileChanged {
    pub path: String,
    pub change_type: String,
    pub diff_preview: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoderSessionResult {
    pub output: String,
    pub exit_code: i32,
    pub files_changed: Vec<CoderFileChanged>,
    pub duration_ms: u128,
}

// --- Prompt Composer Types ---

#[derive(Debug, Clone, Default)]
pub struct PromptLayers {
    pub system_policy: String,
    pub task_intent: String,
    pub session_anchor: String,
    pub memory_retrieval: String,
    pub failure_delta: Option<String>,
    pub output_schema: String,
}

// --- Validation Types ---

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidationConfig {
    pub lint_commands: Vec<String>,
    pub build_commands: Vec<String>,
    pub test_commands: Vec<String>,
    pub working_dir: Option<String>,
    pub max_fix_iterations: u8,
    pub lint_timeout_ms: u64,
    pub test_timeout_ms: u64,
    pub git_auto_commit: bool,
    pub git_auto_push: bool,
    pub git_branch_prefix: Option<String>,
    pub git_protect_dirty: bool,
}

impl Default for ValidationConfig {
    fn default() -> Self {
        Self {
            lint_commands: Vec::new(),
            build_commands: Vec::new(),
            test_commands: Vec::new(),
            working_dir: None,
            max_fix_iterations: 3,
            lint_timeout_ms: 60_000,
            test_timeout_ms: 120_000,
            git_auto_commit: false,
            git_auto_push: false,
            git_branch_prefix: None,
            git_protect_dirty: true,
        }
    }
}

impl ValidationConfig {
    pub fn has_lint(&self) -> bool {
        !self.lint_commands.is_empty() || !self.build_commands.is_empty()
    }

    pub fn has_test(&self) -> bool {
        !self.test_commands.is_empty()
    }

    pub fn lint_and_build_commands(&self) -> Vec<String> {
        self.lint_commands
            .iter()
            .chain(self.build_commands.iter())
            .cloned()
            .collect()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidationResult {
    pub command: String,
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
    pub duration_ms: u128,
    pub passed: bool,
}

// --- Repo Analysis Types ---

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TechStack {
    pub primary_language: String,
    pub languages: Vec<(String, f32)>,
    pub frameworks: Vec<String>,
    pub package_manager: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DetectedCommands {
    pub lint_commands: Vec<String>,
    pub build_commands: Vec<String>,
    pub test_commands: Vec<String>,
}

impl DetectedCommands {
    pub fn has_lint(&self) -> bool {
        !self.lint_commands.is_empty() || !self.build_commands.is_empty()
    }

    pub fn has_test(&self) -> bool {
        !self.test_commands.is_empty()
    }

    pub fn lint_and_build_commands(&self) -> Vec<String> {
        self.lint_commands
            .iter()
            .chain(self.build_commands.iter())
            .cloned()
            .collect()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepoAnalysis {
    pub repo_path: String,
    pub tech_stack: TechStack,
    pub detected_commands: DetectedCommands,
    pub repo_map: String,
    pub file_count: usize,
    pub key_files: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepoAnalysisConfig {
    pub clone_base_dir: String,
    pub shallow_clone: bool,
    pub max_files_to_scan: usize,
    pub max_file_size_bytes: u64,
    pub repo_map_max_tokens: usize,
}

impl Default for RepoAnalysisConfig {
    fn default() -> Self {
        Self {
            clone_base_dir: "repos".to_string(),
            shallow_clone: true,
            max_files_to_scan: 5000,
            max_file_size_bytes: 100_000,
            repo_map_max_tokens: 4000,
        }
    }
}

// --- Interactive Agent Types ---

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InteractiveStep {
    pub iteration: usize,
    pub thought: String,
    pub action: InteractiveAction,
    pub observation: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum InteractiveAction {
    McpToolCall {
        tool_name: String,
        arguments: serde_json::Value,
    },
    LlmQuery {
        prompt: String,
    },
    Done {
        summary: String,
    },
}

// ── Multi-Orchestrator Cluster ──────────────────────────────────────────────

/// A sub-task assigned to a specific named orchestrator in a cluster run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClusterSubtask {
    /// Name of the target orchestrator (must be registered in the cluster).
    pub orchestrator: String,
    /// The task description to run on that orchestrator.
    pub task: String,
}

/// Request body for `POST /v1/cluster/runs`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClusterRunRequest {
    /// High-level task description (used as label; also sent to all members when
    /// `subtasks` is empty).
    pub task: String,
    /// Explicit per-orchestrator assignments. If empty, the task is broadcast to
    /// every registered cluster member.
    #[serde(default)]
    pub subtasks: Vec<ClusterSubtask>,
}

/// Status of a cluster-level run (aggregated across all sub-runs).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClusterRunStatus {
    Running,
    Succeeded,
    /// At least one sub-run failed but others succeeded.
    PartialFailure,
    Failed,
}

impl Display for ClusterRunStatus {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            ClusterRunStatus::Running => "running",
            ClusterRunStatus::Succeeded => "succeeded",
            ClusterRunStatus::PartialFailure => "partial_failure",
            ClusterRunStatus::Failed => "failed",
        };
        write!(f, "{s}")
    }
}

/// One sub-run entry within a cluster run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClusterSubRunEntry {
    pub orchestrator_name: String,
    pub run_id: Uuid,
    pub task: String,
    pub status: RunStatus,
}

/// Top-level record for a cluster run spanning multiple orchestrators.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClusterRunRecord {
    pub cluster_run_id: Uuid,
    pub task: String,
    pub sub_runs: Vec<ClusterSubRunEntry>,
    pub status: ClusterRunStatus,
    pub created_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
}
