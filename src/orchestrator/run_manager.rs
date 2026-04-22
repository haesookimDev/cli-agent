//! Run lifecycle: submit, execute, and wait.
//!
//! `submit_run` validates a `RunRequest`, auto-routes to a skill workflow
//! when applicable, registers a RunRecord, and spawns `execute_run` on a
//! background task. `run_and_wait` is a convenience that polls the submitted
//! run until it reaches a terminal status. `execute_run` is the main loop:
//! build a graph, run it, optionally verify completion, and either succeed
//! or escalate to a recovery/continuation graph up to the configured caps.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use chrono::Utc;
use tokio::time::sleep;
use tracing::error;
use uuid::Uuid;

use super::helpers::extract_repo_url_from_text;
use super::task_classifier::{classify_task_fallback, is_continuation_command};
use super::{MAX_COMPLETION_CONTINUATIONS, MAX_FAILURE_RETRIES, Orchestrator, RunControl};

use crate::runtime::NodeExecutionResult;
use crate::runtime::{ShouldCancelFn, ShouldPauseFn};
use crate::types::{
    AgentRole, RunActionType, RunRecord, RunRequest, RunStatus, RunSubmission, SessionEvent,
    SessionEventType,
};

impl Orchestrator {
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

    pub(super) async fn execute_run(&self, run_id: Uuid, req: RunRequest) -> anyhow::Result<()> {
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
            if is_continuation_command(req.task.as_str()) {
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
                        let prev_type = classify_task_fallback(
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
                    session_id,
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

        // Shared list of static (non-Planner) node IDs for the current graph stage.
        // Updated before each execute_graph call so the on_complete closure can skip
        // them when Planner generates a SubtaskPlan that supersedes the static graph.
        let static_node_ids: std::sync::Arc<std::sync::Mutex<Vec<String>>> =
            std::sync::Arc::new(std::sync::Mutex::new(
                graph
                    .nodes()
                    .iter()
                    .filter(|n| n.role != AgentRole::Planner)
                    .map(|n| n.id.clone())
                    .collect(),
            ));
        let on_complete =
            self.build_on_completed_fn(run_id, session_id, req.task.clone(), static_node_ids.clone());

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
        let mut failure_retries = 0u8;

        loop {
            // Update static_node_ids to reflect the current iteration's graph so that
            // on_complete correctly skips superseded nodes when Planner generates a plan.
            // Keep the critical section tight (build the new list, then assign under lock)
            // and recover from a poisoned mutex so a prior panic cannot take down the run.
            let next_ids: Vec<String> = current_graph
                .nodes()
                .iter()
                .filter(|n| n.role != AgentRole::Planner)
                .map(|n| n.id.clone())
                .collect();
            {
                let mut ids = static_node_ids
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                *ids = next_ids;
            }
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
                    // Capture failed nodes before consuming node_results via extend.
                    let stage_failed: Vec<NodeExecutionResult> = node_results
                        .iter()
                        .filter(|r| !r.succeeded)
                        .cloned()
                        .collect();
                    accumulated_results.extend(node_results);

                    if cancel_flag.load(Ordering::Relaxed) {
                        self.finish_run(run_id, RunStatus::Cancelled, accumulated_results, None)
                            .await?;
                        break;
                    }

                    if !stage_succeeded {
                        if failure_retries >= MAX_FAILURE_RETRIES {
                            self.finish_run(
                                run_id,
                                RunStatus::Failed,
                                accumulated_results,
                                Some(format!(
                                    "Stage failed after {} recovery attempt(s)",
                                    MAX_FAILURE_RETRIES
                                )),
                            )
                            .await?;
                            break;
                        }

                        failure_retries += 1;
                        let failed: Vec<&NodeExecutionResult> = stage_failed.iter().collect();
                        let succeeded: Vec<&NodeExecutionResult> =
                            accumulated_results.iter().filter(|r| r.succeeded).collect();

                        self.record_action_event(
                            run_id,
                            session_id,
                            RunActionType::ReplanTriggered,
                            Some("orchestrator"),
                            Some("failure_recovery"),
                            None,
                            serde_json::json!({
                                "attempt": failure_retries,
                                "failed_nodes": failed.iter().map(|r| r.node_id.as_str()).collect::<Vec<_>>(),
                                "mode": "failure_recovery",
                                "max_attempts": MAX_FAILURE_RETRIES,
                            }),
                        )
                        .await;

                        current_graph = match self.build_failure_recovery_graph(
                            req.task.as_str(),
                            &failed,
                            &succeeded,
                            failure_retries,
                        ) {
                            Ok(g) => g,
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
                        let stage_label = format!("recovery_{failure_retries}");
                        self.record_graph_initialized(
                            run_id,
                            session_id,
                            &current_graph,
                            stage_label.as_str(),
                        )
                        .await;
                        continue;
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
}
