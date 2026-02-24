export type TaskProfile = "planning" | "extraction" | "coding" | "general";
export type AgentRole = "planner" | "extractor" | "coder" | "summarizer" | "fallback" | "tool_caller" | "analyzer" | "reviewer" | "scheduler" | "config_manager";
export type RunStatus = "queued" | "cancelling" | "cancelled" | "paused" | "running" | "succeeded" | "failed";

export type RunActionType =
  | "run_queued" | "run_started" | "run_cancel_requested" | "run_pause_requested"
  | "run_resumed" | "graph_initialized" | "node_started" | "node_completed"
  | "node_failed" | "node_skipped" | "dynamic_node_added" | "graph_completed"
  | "model_selected" | "run_finished" | "webhook_dispatched"
  | "mcp_tool_called" | "node_token_chunk" | "subtask_planned"
  | "verification_started" | "verification_complete" | "replan_triggered"
  | "terminal_suggested";

export interface RunRequest {
  task: string;
  profile?: TaskProfile;
  session_id?: string;
}

export interface RunSubmission {
  run_id: string;
  session_id: string;
  status: RunStatus;
}

export interface AgentExecutionRecord {
  node_id: string;
  role: AgentRole;
  model: string;
  output: string;
  duration_ms: number;
  succeeded: boolean;
  error: string | null;
}

export interface RunRecord {
  run_id: string;
  session_id: string;
  task: string;
  profile: TaskProfile;
  status: RunStatus;
  created_at: string;
  started_at: string | null;
  finished_at: string | null;
  outputs: AgentExecutionRecord[];
  error: string | null;
  timeline: string[];
}

export interface SessionSummary {
  session_id: string;
  created_at: string;
  run_count: number;
  last_run_at: string | null;
  last_task: string | null;
}

export interface RunActionEvent {
  seq: number;
  event_id: string;
  run_id: string;
  session_id: string;
  timestamp: string;
  action: RunActionType;
  actor_type: string | null;
  actor_id: string | null;
  cause_event_id: string | null;
  payload: Record<string, unknown>;
}

export interface RunBehaviorLane {
  node_id: string;
  role: AgentRole | null;
  status: string;
  dependencies: string[];
  start_offset_ms: number | null;
  end_offset_ms: number | null;
  duration_ms: number | null;
  retries: number;
  model: string | null;
}

export interface RunBehaviorActionCount {
  action: string;
  count: number;
}

export interface RunBehaviorSummary {
  total_duration_ms: number | null;
  lane_count: number;
  completed_nodes: number;
  failed_nodes: number;
  critical_path_nodes: string[];
  critical_path_duration_ms: number;
  bottleneck_node_id: string | null;
  bottleneck_duration_ms: number | null;
  peak_parallelism: number;
}

export interface RunBehaviorView {
  run_id: string;
  session_id: string;
  status: RunStatus | null;
  window_start: string | null;
  window_end: string | null;
  active_nodes: string[];
  lanes: RunBehaviorLane[];
  action_mix: RunBehaviorActionCount[];
  summary: RunBehaviorSummary;
}

export interface NodeTraceState {
  node_id: string;
  role: AgentRole | null;
  dependencies: string[];
  status: string;
  started_at: string | null;
  finished_at: string | null;
  duration_ms: number | null;
  retries: number;
  model: string | null;
}

export interface TraceEdge {
  from: string;
  to: string;
}

export interface RunTraceGraph {
  nodes: NodeTraceState[];
  edges: TraceEdge[];
  active_nodes: string[];
  completed_nodes: number;
  failed_nodes: number;
}

export interface RunTrace {
  run_id: string;
  session_id: string;
  status: RunStatus | null;
  events: RunActionEvent[];
  graph: RunTraceGraph;
}

// --- Chat Types ---

export type ChatRole = "user" | "agent" | "system";

export interface ChatMessage {
  id: string;
  session_id: string;
  run_id: string | null;
  role: ChatRole;
  content: string;
  agent_role: AgentRole | null;
  model: string | null;
  timestamp: string;
}

export interface SessionMemoryItem {
  id: string;
  session_id: string;
  scope: string;
  content: string;
  importance: number;
  source_refs: string[];
  created_at: string;
  updated_at: string;
}

export interface GlobalMemoryItem {
  id: string;
  topic: string;
  content: string;
  importance: number;
  access_count: number;
  created_at: string;
  updated_at: string;
}

// --- Settings Types ---

export interface AppSettings {
  default_profile: TaskProfile;
  preferred_model: string | null;
  disabled_models: string[];
  disabled_providers: string[];
  terminal_command: string;
  terminal_args: string[];
  terminal_auto_spawn: boolean;
}

export interface ModelWithStatus {
  spec: {
    provider: string;
    model_id: string;
    quality: number;
    latency: number;
    cost: number;
    context_window: number;
    tool_call_accuracy: number;
    local_only: boolean;
  };
  enabled: boolean;
  is_preferred: boolean;
}

// --- Cron Schedule Types ---

export interface CronSchedule {
  id: string;
  workflow_id: string;
  cron_expr: string;
  enabled: boolean;
  parameters: Record<string, unknown> | null;
  last_run_at: string | null;
  next_run_at: string | null;
  created_at: string;
}

// --- MCP Types ---

export interface McpToolDefinition {
  name: string;
  description: string;
  input_schema: Record<string, unknown>;
  server_name?: string;
}

// --- Workflow Types ---

export interface WorkflowParameter {
  name: string;
  description: string;
  default_value: string | null;
}

export interface WorkflowNodeTemplate {
  id: string;
  role: AgentRole;
  instructions: string;
  dependencies: string[];
  mcp_tools: string[];
  policy: Record<string, unknown>;
}

export interface WorkflowGraphTemplate {
  nodes: WorkflowNodeTemplate[];
}

export interface WorkflowTemplate {
  id: string;
  name: string;
  description: string;
  created_at: string;
  updated_at: string;
  source_run_id: string | null;
  graph_template: WorkflowGraphTemplate;
  parameters: WorkflowParameter[];
}
