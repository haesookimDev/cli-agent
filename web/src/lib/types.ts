export type TaskProfile = "planning" | "extraction" | "coding" | "general";
export type AgentRole = "planner" | "extractor" | "coder" | "summarizer" | "fallback";
export type RunStatus = "queued" | "cancelling" | "cancelled" | "paused" | "running" | "succeeded" | "failed";

export type RunActionType =
  | "run_queued" | "run_started" | "run_cancel_requested" | "run_pause_requested"
  | "run_resumed" | "graph_initialized" | "node_started" | "node_completed"
  | "node_failed" | "node_skipped" | "dynamic_node_added" | "graph_completed"
  | "model_selected" | "run_finished" | "webhook_dispatched";

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
