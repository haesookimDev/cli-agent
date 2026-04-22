//! Post-node completion callbacks and reviewer-based completion verification.
//!
//! `verify_completion` runs the Reviewer agent against accumulated node outputs
//! to decide whether a run is done. `build_on_completed_fn` returns the
//! per-node callback the runtime invokes after each node finishes — that
//! callback is what grows the DAG dynamically (Planner SubtaskPlans, fix
//! loops after validator failures, in-graph replan after reviewer
//! INCOMPLETE, etc.).

use std::sync::Arc;

use futures::FutureExt;
use uuid::Uuid;

use super::Orchestrator;
use super::helpers::{contains_word, extract_json_object};
use super::{DYNAMIC_SUBTASK_MAX_PARALLELISM, MAX_DYNAMIC_SUBTASKS_PER_PLAN};

use crate::runtime::graph::{AgentNode, DependencyFailurePolicy, ExecutionPolicy};
use crate::runtime::{NodeExecutionResult, OnNodeCompletedFn};
use crate::types::{AgentRole, RunActionType, SubtaskPlan, ValidationConfig};

impl Orchestrator {
    pub(super) async fn verify_completion(
        &self,
        run_id: Uuid,
        session_id: Uuid,
        original_task: &str,
        results: &[NodeExecutionResult],
    ) -> (bool, String) {
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

        // First attempt: ask for a structured JSON verdict with prefix fallback.
        let (is_complete, reason) = match self
            .run_reviewer(run_id, session_id, original_task, &outputs_summary, false)
            .await
        {
            Ok(verdict) => match verdict {
                VerificationVerdict::Complete => (true, "Verified complete".to_string()),
                VerificationVerdict::Incomplete(reason) => (false, reason),
                VerificationVerdict::Ambiguous(preview) => {
                    // Retry once with a stricter JSON-only prompt before giving up.
                    tracing::warn!(
                        run_id = %run_id,
                        "Reviewer returned ambiguous output; retrying with strict JSON prompt: {}",
                        preview
                    );
                    match self
                        .run_reviewer(
                            run_id,
                            session_id,
                            original_task,
                            &outputs_summary,
                            true,
                        )
                        .await
                    {
                        Ok(VerificationVerdict::Complete) => {
                            (true, "Verified complete (on retry)".to_string())
                        }
                        Ok(VerificationVerdict::Incomplete(reason)) => (false, reason),
                        Ok(VerificationVerdict::Ambiguous(retry_preview)) => (
                            false,
                            format!(
                                "Ambiguous reviewer response after retry: {}",
                                retry_preview
                            ),
                        ),
                        Err(e) => (false, format!("Reviewer unavailable on retry: {e}")),
                    }
                }
            },
            Err(e) => (false, format!("Reviewer unavailable: {e}")),
        };

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

    /// Run the Reviewer agent once. `strict` requests JSON-only output with
    /// no surrounding prose — used on the retry after the first response was
    /// ambiguous.
    async fn run_reviewer(
        &self,
        run_id: Uuid,
        session_id: Uuid,
        original_task: &str,
        outputs_summary: &str,
        strict: bool,
    ) -> anyhow::Result<VerificationVerdict> {
        let prompt = if strict {
            format!(
                "Original request: {}\n\nExecution results:\n{}\n\n\
                 Respond with ONLY a single JSON object and no other text, like:\n\
                 {{\"status\": \"complete\", \"reason\": \"...\"}} or \
                 {{\"status\": \"incomplete\", \"reason\": \"...\"}}. \
                 No markdown fences, no explanation.",
                original_task, outputs_summary
            )
        } else {
            format!(
                "Original request: {}\n\nExecution results:\n{}\n\n\
                 Did the execution fully satisfy the original request? \
                 Reply as JSON: {{\"status\": \"complete\"|\"incomplete\", \"reason\": \"...\"}}. \
                 If your runtime can't produce JSON, fall back to the literal prefix \
                 \"COMPLETE\" or \"INCOMPLETE: <reason>\".",
                original_task, outputs_summary
            )
        };

        let review_input = crate::agents::AgentInput {
            task: prompt,
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

        let output = self
            .agents
            .run_role(AgentRole::Reviewer, review_input, self.router.clone(), None)
            .await?;

        Ok(parse_reviewer_verdict(output.content.as_str()))
    }

    pub(super) fn inject_git_and_summarize(
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

    pub(super) fn parse_fix_iteration(node_id: &str) -> u8 {
        // "validate_lint" → 0, "validate_lint_1" → 1, "validate_test_2" → 2
        node_id
            .rsplit('_')
            .next()
            .and_then(|s| s.parse::<u8>().ok())
            .unwrap_or(0)
    }

    pub(super) fn build_on_completed_fn(
        &self,
        run_id: Uuid,
        session_id: Uuid,
        task: String,
        // Node IDs of static graph nodes (non-Planner) for the current graph stage.
        // When Planner generates a SubtaskPlan, these are returned as "skip" targets
        // so the runtime skips them in favour of the dynamic subtask nodes.
        static_node_ids: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    ) -> OnNodeCompletedFn {
        let memory = self.memory.clone();
        let validation_config = self.validation_config.clone();
        let repo_analyses = self.repo_analyses.clone();
        let orchestrator = self.clone();
        Arc::new(move |node: AgentNode, result: NodeExecutionResult| {
            let task = task.clone();
            let static_node_ids = static_node_ids.clone();
            let memory = memory.clone();
            let validation_config = validation_config.clone();
            let repo_analyses = repo_analyses.clone();
            let orchestrator = orchestrator.clone();
            async move {
                let mut dynamic_nodes = Vec::new();

                if result.succeeded && !result.output.is_empty() {
                    let importance = match node.role {
                        AgentRole::Analyzer | AgentRole::Reviewer => 0.8,
                        AgentRole::Coder | AgentRole::Extractor => 0.7,
                        AgentRole::Summarizer => 0.6,
                        _ => 0.5,
                    };
                    let content: String = result.output.chars().take(4000).collect();
                    let scope = format!("agent_output:{}", node.role);
                    let source = format!("{}:{}", run_id, node.id);
                    let _ = memory
                        .remember_long(session_id, &scope, &content, importance, Some(&source))
                        .await;
                }

                if node.role == AgentRole::Planner && result.succeeded && node.depth < 5 {
                    let plan_result = extract_json_object(&result.output)
                        .and_then(|json_str| serde_json::from_str::<SubtaskPlan>(json_str).ok())
                        .or_else(|| serde_json::from_str::<SubtaskPlan>(&result.output).ok());
                    if let Some(plan) = plan_result {
                        if !plan.subtasks.is_empty() {
                            let subtasks: Vec<_> = plan
                                .subtasks
                                .into_iter()
                                .take(MAX_DYNAMIC_SUBTASKS_PER_PLAN)
                                .collect();

                            let _ = memory
                                .append_run_action_event(
                                    run_id,
                                    session_id,
                                    RunActionType::SubtaskPlanned,
                                    Some("node"),
                                    Some(node.id.as_str()),
                                    None,
                                    serde_json::json!({
                                        "subtask_count": subtasks.len(),
                                        "subtasks": subtasks.iter().map(|s| &s.id).collect::<Vec<_>>(),
                                    }),
                                )
                                .await;

                            for subtask in subtasks {
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
                            let skip_ids = {
                                let guard = static_node_ids
                                    .lock()
                                    .unwrap_or_else(|e| e.into_inner());
                                guard.clone()
                            };
                            return Ok((dynamic_nodes, skip_ids));
                        }
                    }

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

                if node.role == AgentRole::Reviewer
                    && result.succeeded
                    && result.output.contains("REQUEST_CHANGES")
                {
                    let iteration = Self::parse_fix_iteration(&node.id);
                    let max_review_iterations: u8 = 3;
                    if iteration < max_review_iterations {
                        let next_iter = iteration + 1;
                        let review_feedback: String =
                            result.output.chars().take(3000).collect();

                        let fix_id = format!("fix_review_{}", next_iter);
                        let mut fix_node = AgentNode::new(
                            fix_id.clone(),
                            AgentRole::Coder,
                            format!(
                                "The reviewer requested changes. Address the feedback below and push fixes.\n\n{}",
                                review_feedback
                            ),
                        );
                        fix_node.dependencies = vec![node.id.clone()];
                        fix_node.depth = node.depth + 1;
                        fix_node.mcp_tools = vec![
                            "github/push_files".into(),
                            "github/get_file_contents".into(),
                        ];
                        fix_node.policy = ExecutionPolicy {
                            max_parallelism: 1,
                            retry: 1,
                            timeout_ms: 180_000,
                            circuit_breaker: 2,
                            on_dependency_failure: DependencyFailurePolicy::ContinueOnError,
                            fallback_node: None,
                        };

                        let re_review_id = format!("re_review_{}", next_iter);
                        let mut re_review = AgentNode::new(
                            re_review_id,
                            AgentRole::Reviewer,
                            "Review the updated code after fixes. Output APPROVE or REQUEST_CHANGES.",
                        );
                        re_review.dependencies = vec![fix_id];
                        re_review.depth = node.depth + 2;
                        re_review.mcp_tools = vec![
                            "github/pull_request_read".into(),
                            "github/pull_request_review_write".into(),
                        ];
                        re_review.policy = ExecutionPolicy {
                            max_parallelism: 1,
                            retry: 1,
                            timeout_ms: 120_000,
                            circuit_breaker: 2,
                            on_dependency_failure: DependencyFailurePolicy::ContinueOnError,
                            fallback_node: None,
                        };

                        dynamic_nodes.push(fix_node);
                        dynamic_nodes.push(re_review);
                    }
                }

                if node.role == AgentRole::Validator && !result.succeeded {
                    let iteration = Self::parse_fix_iteration(&node.id);
                    let max_iters = validation_config.max_fix_iterations;

                    let phase = if node.instructions.contains("test") {
                        "test"
                    } else {
                        "lint"
                    };

                    if iteration < max_iters {
                        let next_iter = iteration + 1;

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
                        if node.instructions.contains("external") {
                            let analysis = repo_analyses.get(&run_id);
                            let has_test = analysis.as_ref().map(|a| a.detected_commands.has_test()).unwrap_or(false);
                            if has_test {
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
                            return Ok((dynamic_nodes, vec![]));
                        }

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
                            Self::inject_git_and_summarize(
                                &validation_config,
                                &node,
                                &mut dynamic_nodes,
                                run_id,
                            );
                        }
                    } else if is_test {
                        if node.instructions.contains("external") {
                            Self::inject_git_and_summarize(
                                &validation_config,
                                &node,
                                &mut dynamic_nodes,
                                run_id,
                            );
                            orchestrator.normalize_dynamic_nodes_for_runtime(&mut dynamic_nodes);
                            return Ok((dynamic_nodes, vec![]));
                        }

                        Self::inject_git_and_summarize(
                            &validation_config,
                            &node,
                            &mut dynamic_nodes,
                            run_id,
                        );
                    }
                }

                if node.role == AgentRole::Reviewer && result.succeeded && node.depth < 4 {
                    let content = result.output.trim();
                    if content.starts_with("INCOMPLETE") {
                        let reason = content
                            .strip_prefix("INCOMPLETE:")
                            .unwrap_or(content)
                            .trim();
                        let replan_id = format!("replan_after_{}", node.id);
                        let mut replan_node = AgentNode::new(
                            replan_id,
                            AgentRole::Planner,
                            format!(
                                "Reviewer flagged the previous work as incomplete.\n\n\
                                 REASON:\n{}\n\n\
                                 ORIGINAL TASK:\n{}\n\n\
                                 Create a JSON SubtaskPlan to address the remaining gap. \
                                 Do not wrap the JSON in markdown fences.",
                                reason, task,
                            ),
                        );
                        replan_node.dependencies = vec![node.id.clone()];
                        replan_node.depth = node.depth + 1;
                        replan_node.policy = ExecutionPolicy {
                            timeout_ms: 120_000,
                            retry: 1,
                            max_parallelism: 1,
                            circuit_breaker: 2,
                            on_dependency_failure: DependencyFailurePolicy::FailFast,
                            fallback_node: None,
                        };
                        dynamic_nodes.push(replan_node);
                        let _ = memory
                            .append_run_action_event(
                                run_id,
                                session_id,
                                RunActionType::ReplanTriggered,
                                Some("reviewer"),
                                Some(node.id.as_str()),
                                None,
                                serde_json::json!({
                                    "reason": reason,
                                    "mode": "in_graph_reviewer_replan",
                                }),
                            )
                            .await;
                        tracing::warn!(
                            run_id = %run_id,
                            node_id = %node.id,
                            reason,
                            "Reviewer flagged task as incomplete; injecting replan planner"
                        );
                    }
                }

                orchestrator.normalize_dynamic_nodes_for_runtime(&mut dynamic_nodes);
                Ok((dynamic_nodes, vec![]))
            }
            .boxed()
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum VerificationVerdict {
    Complete,
    Incomplete(String),
    /// Parser couldn't extract a clear verdict. Carries a preview of the raw
    /// response for logging / retry prompts.
    Ambiguous(String),
}

/// Parse Reviewer output into a structured verdict. Tries in order:
///   1. JSON object with `status: "complete"|"incomplete"` and optional `reason`.
///   2. Literal prefix `COMPLETE` / `INCOMPLETE: …`.
///   3. Keyword scan using word-boundary matching.
///
/// Returns `Ambiguous` if none of these match, letting the caller retry or
/// surface the raw preview in the UI.
fn parse_reviewer_verdict(raw: &str) -> VerificationVerdict {
    let content = raw.trim();
    let preview: String = content.chars().take(240).collect();
    if content.is_empty() {
        return VerificationVerdict::Ambiguous(String::from("<empty>"));
    }

    // (1) Try structured JSON.
    let json_candidate = extract_json_object(content).unwrap_or(content);
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(json_candidate) {
        if let Some(status_raw) = value.get("status").and_then(|s| s.as_str()) {
            let reason = value
                .get("reason")
                .and_then(|s| s.as_str())
                .unwrap_or("")
                .trim()
                .to_string();
            let status = status_raw.trim().to_lowercase();
            match status.as_str() {
                "complete" | "completed" | "success" | "satisfied" | "done" => {
                    return VerificationVerdict::Complete;
                }
                "incomplete" | "not_complete" | "fail" | "failed" | "not_done" => {
                    return VerificationVerdict::Incomplete(if reason.is_empty() {
                        "No reason provided".to_string()
                    } else {
                        reason
                    });
                }
                _ => {}
            }
        }
    }

    // (2) Literal prefix match (historical protocol).
    if content.starts_with("COMPLETE") {
        return VerificationVerdict::Complete;
    }
    if content.starts_with("INCOMPLETE") {
        let reason = content
            .strip_prefix("INCOMPLETE:")
            .unwrap_or(content.strip_prefix("INCOMPLETE").unwrap_or(content))
            .trim()
            .to_string();
        return VerificationVerdict::Incomplete(if reason.is_empty() {
            "No reason provided".to_string()
        } else {
            reason
        });
    }

    // (3) Keyword scan as a last resort. Negation phrases ("not complete")
    // take precedence over the bare "complete" word because they're the more
    // specific statement about the outcome.
    let lower = content.to_lowercase();
    let has_incomplete_phrase = [
        "incomplete",
        "not satisfied",
        "not complete",
        "not done",
        "missing",
    ]
    .iter()
    .any(|m| lower.contains(m));
    if has_incomplete_phrase {
        return VerificationVerdict::Incomplete(preview);
    }
    let has_complete = ["complete", "completed", "satisfied", "success"]
        .iter()
        .any(|m| contains_word(lower.as_str(), m));
    if has_complete {
        return VerificationVerdict::Complete;
    }
    VerificationVerdict::Ambiguous(preview)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_structured_json_complete() {
        let v = parse_reviewer_verdict(r#"{"status": "complete", "reason": "all good"}"#);
        assert_eq!(v, VerificationVerdict::Complete);
    }

    #[test]
    fn parses_structured_json_incomplete_with_reason() {
        let v = parse_reviewer_verdict(
            r#"{"status": "incomplete", "reason": "tests still failing"}"#,
        );
        assert_eq!(
            v,
            VerificationVerdict::Incomplete("tests still failing".to_string())
        );
    }

    #[test]
    fn parses_json_inside_markdown_fence() {
        let v = parse_reviewer_verdict(
            "```json\n{\"status\": \"complete\", \"reason\": \"ok\"}\n```",
        );
        assert_eq!(v, VerificationVerdict::Complete);
    }

    #[test]
    fn falls_back_to_complete_prefix() {
        let v = parse_reviewer_verdict("COMPLETE\nEverything looks good.");
        assert_eq!(v, VerificationVerdict::Complete);
    }

    #[test]
    fn falls_back_to_incomplete_prefix_with_reason() {
        let v = parse_reviewer_verdict("INCOMPLETE: missing the error handler");
        assert_eq!(
            v,
            VerificationVerdict::Incomplete("missing the error handler".to_string())
        );
    }

    #[test]
    fn keyword_scan_picks_incomplete_over_ambiguous() {
        let v =
            parse_reviewer_verdict("The run is not complete because the build failed partway.");
        match v {
            VerificationVerdict::Incomplete(_) => {}
            other => panic!("expected Incomplete, got {:?}", other),
        }
    }

    #[test]
    fn truly_ambiguous_output_is_ambiguous() {
        let v = parse_reviewer_verdict("I think it depends on what you mean by satisfying.");
        match v {
            VerificationVerdict::Ambiguous(_) => {}
            other => panic!("expected Ambiguous, got {:?}", other),
        }
    }

    #[test]
    fn empty_output_is_ambiguous() {
        let v = parse_reviewer_verdict("   ");
        match v {
            VerificationVerdict::Ambiguous(_) => {}
            other => panic!("expected Ambiguous, got {:?}", other),
        }
    }
}
