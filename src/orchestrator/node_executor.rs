//! Per-node execution: the giant `build_run_node_fn` closure factory and
//! the event-sink factory that drives runtime/memory streaming.
//!
//! `build_run_node_fn` is the heart of the orchestrator: it produces the
//! per-node async closure the runtime calls to run each `AgentNode`. It
//! contains branches for every AgentRole (Analyzer, Coder, Validator,
//! ToolCaller, etc.) and handles MCP tool calls, Coder-backend CLI
//! execution, git operations, and LLM inference. Known limitations of its
//! current monolithic structure are tracked under TODO 2-6/2-7/6-2.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::Utc;
use futures::FutureExt;
use tokio::sync::mpsc;
use tokio::time::sleep;
use uuid::Uuid;

use super::Orchestrator;
use super::context_builder::{
    build_memory_query, build_recent_history_chunks, build_recent_run_summary,
};
use super::helpers::{
    extract_repo_url_from_text, format_tool_catalog, parse_tool_calls, summarize_selector_output,
};
use super::task_classifier::is_local_workspace_task;
use super::{
    git_manager, interactive, repo_analyzer, skill_loader, tool_augment, validator,
};

use crate::agents::AgentInput;
use crate::context::{ContextChunk, ContextKind, ContextScope};
use crate::runtime::graph::AgentNode;
use crate::runtime::{EventSink, NodeExecutionResult, RunNodeFn, RuntimeEvent};
use crate::types::{
    AgentRole, RunActionType, RunRequest,
    SessionEvent, SessionEventType,
};

impl Orchestrator {
    pub(super) fn build_run_node_fn(
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
                        return Ok(NodeExecutionResult::failure_with_output(
                            node.id,
                            node.role,
                            "repo_analyzer",
                            "No repository URL found in request or planner output".to_string(),
                            "missing repo_url",
                            started,
                        ));
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
                            return Ok(NodeExecutionResult::failure_with_output(
                                node.id,
                                node.role,
                                "repo_analyzer",
                                format!("Session workspace setup failed: {}", e),
                                format!("workspace setup failed: {}", e),
                                started,
                            ));
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
                            return Ok(NodeExecutionResult::failure_with_output(
                                node.id,
                                node.role,
                                "repo_analyzer",
                                format!("Clone failed: {}", e),
                                format!("clone failed: {}", e),
                                started,
                            ));
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
                            return Ok(NodeExecutionResult::success(
                                node.id,
                                node.role,
                                "repo_analyzer",
                                output,
                                started,
                            ));
                        }
                        Err(e) => {
                            return Ok(NodeExecutionResult::failure_with_output(
                                node.id,
                                node.role,
                                "repo_analyzer",
                                format!("Analysis failed: {}", e),
                                e.to_string(),
                                started,
                            ));
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
                            return Ok(NodeExecutionResult::failure_with_output(
                                node.id,
                                node.role,
                                "validator",
                                format!("Session workspace setup failed: {}", e),
                                format!("workspace setup failed: {}", e),
                                started,
                            ));
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
                            return Ok(NodeExecutionResult::failure_with_output(
                                node.id,
                                node.role,
                                "git",
                                "Not a git repository".to_string(),
                                "working directory is not a git repository",
                                started,
                            ));
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
                            return Ok(NodeExecutionResult::failure_with_output(
                                node.id,
                                node.role,
                                "git",
                                format!("git stage failed: {e}"),
                                e.to_string(),
                                started,
                            ));
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
                                return Ok(NodeExecutionResult::failure_with_output(
                                    node.id,
                                    node.role,
                                    "git",
                                    format!("git commit failed: {e}"),
                                    e.to_string(),
                                    started,
                                ));
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

                        return Ok(NodeExecutionResult::success(
                            node.id,
                            node.role,
                            "git",
                            serde_json::json!({
                                "commit_hash": commit_hash,
                                "commit_message": commit_msg,
                                "pushed": pushed,
                            })
                            .to_string(),
                            started,
                        ));
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
                                return Ok(NodeExecutionResult::success(
                                    node.id,
                                    node.role,
                                    "validator",
                                    "No commands detected for this phase".to_string(),
                                    started,
                                ));
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
                            return Ok(NodeExecutionResult::success(
                                node.id,
                                node.role,
                                "validator",
                                "No repo analysis available".to_string(),
                                started,
                            ));
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
                            return Ok(NodeExecutionResult::failure_with_output(
                                node.id,
                                node.role,
                                "workspace",
                                format!("Session workspace setup failed: {}", e),
                                format!("workspace setup failed: {}", e),
                                started,
                            ));
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
                        return Ok(NodeExecutionResult::failure_with_output(
                            node.id,
                            node.role,
                            "mcp:none",
                            "No MCP tools are available for this tool-caller node".to_string(),
                            "No allowed MCP tools available",
                            started,
                        ));
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
                    let local_first_hint = if is_local_workspace_task(req.task.as_str()) {
                        "\n\nSTRICT LOCAL-WORKSPACE RULE:\n\
                         - You MUST select filesystem/* tools for any operation on local files or directories.\n\
                         - NEVER select github/* tools for reading or modifying local project files.\n\
                         - github/* tools are ONLY for explicitly remote operations (PRs, issues, remote repo metadata).\n\
                         - If the planner mentions local file paths, you MUST use filesystem/* tools regardless of what other tools are available."
                    } else {
                        ""
                    };

                    // Phase B+C: Iterative tool calling loop
                    // Hard caps on both dimensions so a confused LLM can't run
                    // away with the ToolCaller node (TODO 2-6):
                    //   - MAX_TOOL_ITERATIONS: outer selection loop count
                    //   - MAX_TOOLS_PER_ITERATION: tool calls from one LLM turn
                    const MAX_TOOL_ITERATIONS: usize = 5;
                    const MAX_TOOLS_PER_ITERATION: usize = 10;
                    let mut all_results: Vec<String> = Vec::new();
                    let mut total_tool_calls: usize = 0;
                    let mut any_failure = false;
                    let mut exited_via_break = false;

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
                                return Ok(NodeExecutionResult::failure_with_output(
                                    node.id,
                                    node.role,
                                    "unavailable",
                                    all_results.join("\n---\n"),
                                    format!(
                                        "LLM tool selection failed at iteration {iteration}: {err}"
                                    ),
                                    started,
                                ));
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

                        let mut tool_calls = parse_tool_calls(json_str);
                        if tool_calls.len() > MAX_TOOLS_PER_ITERATION {
                            tracing::warn!(
                                run_id = %run_id,
                                node_id = %node.id,
                                iteration,
                                requested = tool_calls.len(),
                                cap = MAX_TOOLS_PER_ITERATION,
                                "Selector produced more tool calls than the per-iteration cap; truncating"
                            );
                            tool_calls.truncate(MAX_TOOLS_PER_ITERATION);
                        }

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
                                exited_via_break = true;
                                break;
                            }
                            // No prior results — this is a failure
                            return Ok(NodeExecutionResult::failure_with_output(
                                node.id,
                                node.role,
                                "mcp:none",
                                format!(
                                    "No executable tool calls were parsed.\nRaw selector output:\n{}",
                                    llm_output
                                ),
                                format!(
                                    "No executable tool calls were parsed from LLM output. Selector output preview: {}",
                                    summarize_selector_output(&llm_output)
                                ),
                                started,
                            ));
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

                            // Wrap call_tool in a hard timeout so a hung MCP server
                            // can't stall the entire ToolCaller node (TODO 2-7).
                            const MCP_CALL_TIMEOUT: Duration = Duration::from_secs(60);
                            let call_fut = mcp.call_tool(&tool_name, arguments);
                            let call_result = match tokio::time::timeout(
                                MCP_CALL_TIMEOUT,
                                call_fut,
                            )
                            .await
                            {
                                Ok(r) => r,
                                Err(_) => Err(anyhow::anyhow!(
                                    "MCP call_tool({}) timed out after {:?}",
                                    tool_name,
                                    MCP_CALL_TIMEOUT
                                )),
                            };

                            match call_result {
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

                    if !exited_via_break {
                        tracing::warn!(
                            run_id = %run_id,
                            node_id = %node.id,
                            max_iterations = MAX_TOOL_ITERATIONS,
                            total_tool_calls,
                            "ToolCaller reached the iteration cap without explicit completion"
                        );
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
                        // Use node.instructions as the specific task for this coder node.
                        // When Planner creates multiple parallel Coder subtasks, each node
                        // has distinct instructions; req.task is the top-level user request.
                        let node_task = if node.instructions.is_empty() {
                            req.task.clone()
                        } else {
                            node.instructions.clone()
                        };
                        let (effective_working_dir, enriched_task) =
                            if let Some(ref analysis) = external_analysis {
                                let enriched = format!(
                                    "{}\n\nREPOSITORY MAP:\n{}\n\nTECH STACK: {} ({})\nKey files: {}",
                                    node_task,
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
                                        return Ok(NodeExecutionResult::failure_with_output(
                                            node.id,
                                            node.role,
                                            backend_kind.to_string(),
                                            format!("Session workspace setup failed: {}", e),
                                            format!("workspace setup failed: {}", e),
                                            started,
                                        ));
                                    }
                                };
                                (working_dir, node_task)
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
                            Err(err) => Ok(NodeExecutionResult::failure(
                                node.id,
                                node.role,
                                format!("coder:{}", backend_kind),
                                err.to_string(),
                                started,
                            )),
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
                    build_recent_history_chunks(&recent_messages, req.task.as_str());
                let memory_query = build_memory_query(req.task.as_str(), &recent_messages);
                let memory_hits = memory
                    .retrieve(session_id, memory_query.as_str(), 8)
                    .await
                    .unwrap_or_default();
                let global_hits = memory
                    .search_knowledge(req.task.as_str(), 4)
                    .await
                    .unwrap_or_default();
                let recent_runs = memory.list_session_runs(session_id, 4).await.unwrap_or_default();
                let recent_run_summary = build_recent_run_summary(run_id, &recent_runs);
                let local_workspace_task = is_local_workspace_task(req.task.as_str());

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

                        Ok(NodeExecutionResult::success(
                            node_id,
                            role,
                            current_model,
                            current_output,
                            started,
                        ))
                    }
                    Err(err) => Ok(NodeExecutionResult::failure(
                        node.id,
                        node.role,
                        "unavailable",
                        err.to_string(),
                        started,
                    )),
                }
            }
            .boxed()
        })
    }


    pub(super) fn build_event_sink(&self, run_id: Uuid, session_id: Uuid) -> EventSink {
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
                    RuntimeEvent::GitHubActivity {
                        node_id,
                        persona_name,
                        activity_type,
                        target_number,
                        url,
                        title,
                    } => serde_json::json!({
                        "node_id": node_id,
                        "persona_name": persona_name,
                        "activity_type": activity_type,
                        "target_number": target_number,
                        "url": url,
                        "title": title,
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
                    | RuntimeEvent::InteractiveStep { .. }
                    | RuntimeEvent::GitHubActivity { .. } => SessionEventType::RunProgress,
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
                    RuntimeEvent::GitHubActivity { node_id, activity_type, .. } => {
                        let action = match activity_type.as_str() {
                            "issue_created" => RunActionType::GitHubIssueCreated,
                            "issue_commented" => RunActionType::GitHubIssueCommented,
                            "issue_closed" => RunActionType::GitHubIssueClosed,
                            "pr_created" => RunActionType::GitHubPrCreated,
                            "pr_reviewed" => RunActionType::GitHubPrReviewed,
                            "pr_commented" => RunActionType::GitHubPrCommented,
                            "pr_merged" => RunActionType::GitHubPrMerged,
                            "branch_created" => RunActionType::GitHubBranchCreated,
                            _ => RunActionType::GitHubIssueCreated,
                        };
                        (action, Some(node_id.as_str()))
                    }
                };

                if let Err(err) = memory
                    .append_event(SessionEvent {
                        session_id,
                        run_id: Some(run_id),
                        event_type,
                        timestamp: Utc::now(),
                        payload: payload.clone(),
                    })
                    .await
                {
                    tracing::warn!(%run_id, %session_id, "failed to append session event: {}", err);
                }

                if let Err(err) = memory
                    .append_run_action_event(
                        run_id,
                        session_id,
                        action,
                        Some("runtime"),
                        actor_id,
                        None,
                        payload.clone(),
                    )
                    .await
                {
                    tracing::warn!(%run_id, %session_id, "failed to append run action event: {}", err);
                }

                if let Err(err) = webhook.dispatch("run.progress", payload).await {
                    tracing::warn!(%run_id, "webhook dispatch failed: {}", err);
                }
            });
        })
    }
}
