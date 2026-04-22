//! DAG construction logic.
//!
//! Collects the scattered graph-building helpers (`build_graph` proper plus
//! recovery/followup graph builders, node-policy normalization, and the
//! workflow-template-to-graph converter) into one module. All methods remain
//! on `Orchestrator` and access its private fields via the submodule
//! visibility rule.

use super::Orchestrator;
use super::context_builder::trim_for_context;
use super::task_classifier::{classify_task, is_local_workspace_task};

use crate::runtime::graph::{AgentNode, DependencyFailurePolicy, ExecutionGraph, ExecutionPolicy};
use crate::runtime::NodeExecutionResult;
use crate::types::{AgentRole, TaskType, WorkflowNodeTemplate, WorkflowTemplate};

pub(super) fn workflow_node_to_agent_node(wn: &WorkflowNodeTemplate) -> anyhow::Result<AgentNode> {
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

impl Orchestrator {
    pub(super) fn normalize_node_policy_for_runtime(&self, node: &mut AgentNode) {
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

    pub(super) fn add_graph_node(
        &self,
        graph: &mut ExecutionGraph,
        mut node: AgentNode,
    ) -> anyhow::Result<()> {
        self.normalize_node_policy_for_runtime(&mut node);
        graph.add_node(node)
    }

    pub(super) fn normalize_dynamic_nodes_for_runtime(&self, nodes: &mut [AgentNode]) {
        for node in nodes {
            self.normalize_node_policy_for_runtime(node);
        }
    }

    pub(super) fn build_workflow_graph_from_template(
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

    pub(super) fn summarize_results_for_followup(results: &[NodeExecutionResult]) -> String {
        results
            .iter()
            .filter(|r| r.succeeded && !r.output.trim().is_empty())
            .map(|r| {
                format!(
                    "[{}:{}]\n{}",
                    r.node_id,
                    r.role,
                    trim_for_context(r.output.as_str(), 700)
                )
            })
            .collect::<Vec<_>>()
            .join("\n---\n")
    }

    /// Builds a Planner-led recovery graph when one or more nodes fail outright.
    /// The Planner receives the failure context and produces a new SubtaskPlan
    /// targeting only the failed work.
    pub(super) fn build_failure_recovery_graph(
        &self,
        original_task: &str,
        failed_results: &[&NodeExecutionResult],
        successful_results: &[&NodeExecutionResult],
        attempt: u8,
    ) -> anyhow::Result<ExecutionGraph> {
        let mut graph = ExecutionGraph::new(self.max_graph_depth);

        let failed_summary = failed_results
            .iter()
            .map(|r| {
                format!(
                    "[{}] error: {}",
                    r.node_id,
                    r.error.as_deref().unwrap_or("unknown error")
                )
            })
            .collect::<Vec<_>>()
            .join("\n");

        let success_summary = Self::summarize_results_for_followup(
            &successful_results.iter().copied().cloned().collect::<Vec<_>>(),
        );

        let planner_id = format!("recovery_plan_{attempt}");
        let mut planner = AgentNode::new(
            planner_id,
            AgentRole::Planner,
            format!(
                "One or more agents failed during execution. Your job is to recover.\n\n\
                 ORIGINAL TASK:\n{}\n\n\
                 FAILED AGENTS (with errors):\n{}\n\n\
                 SUCCESSFUL OUTPUTS SO FAR:\n{}\n\n\
                 Return a JSON SubtaskPlan that addresses the failed work:\n\
                 {{\"subtasks\":[{{\"id\":\"string\",\"description\":\"string\",\"agent_role\":\"planner|extractor|coder|summarizer|fallback|tool_caller|analyzer|reviewer|scheduler|config_manager|validator\",\"dependencies\":[\"node_id\"],\"instructions\":\"string\",\"mcp_tools\":[\"server/tool\"]}}]}}\n\n\
                 Rules:\n\
                 - Focus only on what failed; do not repeat succeeded work.\n\
                 - If an agent failed due to a transient error, retry it with clearer instructions.\n\
                 - If an agent failed due to a design flaw, replace it with a different approach.\n\
                 - Do not wrap the JSON in markdown fences.",
                trim_for_context(original_task, 1000),
                trim_for_context(&failed_summary, 800),
                trim_for_context(&success_summary, 3000),
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

    pub(super) fn build_completion_followup_graph(
        &self,
        original_task: &str,
        incomplete_reason: &str,
        results: &[NodeExecutionResult],
        attempt: u8,
    ) -> anyhow::Result<ExecutionGraph> {
        let mut graph = ExecutionGraph::new(self.max_graph_depth);
        let prior_summary = Self::summarize_results_for_followup(results);
        let local_first_hint = if is_local_workspace_task(original_task) {
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
                trim_for_context(original_task, 1000),
                trim_for_context(incomplete_reason, 600),
                trim_for_context(prior_summary.as_str(), 4000),
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

    pub(super) fn default_policy() -> ExecutionPolicy {
        ExecutionPolicy {
            max_parallelism: 1,
            retry: 1,
            timeout_ms: 120_000,
            circuit_breaker: 3,
            on_dependency_failure: DependencyFailurePolicy::ContinueOnError,
            fallback_node: None,
        }
    }

    pub(super) async fn build_graph(
        &self,
        task: &str,
        mcp_server_names: &[String],
        mcp_tool_names: &[String],
        previous_task_type: Option<TaskType>,
        previous_plan: Option<String>,
        repo_url: Option<&str>,
        working_dir: Option<&std::path::Path>,
    ) -> anyhow::Result<ExecutionGraph> {
        let task_type = classify_task(
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
            let local_first_hint = if is_local_workspace_task(task) {
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
                trim_for_context(prev_plan, 800)
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
}
