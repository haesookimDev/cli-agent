use std::sync::Arc;
use std::time::Instant;

use tracing::{debug, warn};
use uuid::Uuid;

use crate::agents::{AgentInput, AgentRegistry};
use crate::context::{ContextChunk, ContextKind, ContextManager, ContextScope};
use crate::mcp::McpRegistry;
use crate::memory::MemoryManager;
use crate::orchestrator::skill_loader;
use crate::router::ModelRouter;
use crate::runtime::graph::AgentNode;
use crate::runtime::{EventSink, NodeExecutionResult, RuntimeEvent};
use crate::types::{AgentRole, RunActionType};

/// Default max iterations for the interactive ReAct loop.
pub const DEFAULT_MAX_ITERATIONS: usize = 15;

/// Maximum history entries to keep in the ReAct prompt to control token usage.
const MAX_HISTORY_ENTRIES: usize = 10;

/// Execute the ReAct loop for an interactive node.
pub async fn execute_react_loop(
    node: &AgentNode,
    deps: &[NodeExecutionResult],
    task: &str,
    run_id: Uuid,
    session_id: Uuid,
    working_dir: Option<std::path::PathBuf>,
    max_iterations: usize,
    agents: &AgentRegistry,
    router: &Arc<ModelRouter>,
    memory: &Arc<MemoryManager>,
    context: &Arc<ContextManager>,
    mcp: &Arc<McpRegistry>,
    event_sink: &EventSink,
    allowed_mcp_tools: &[String],
) -> NodeExecutionResult {
    let started = Instant::now();
    let mut history: Vec<String> = Vec::new();
    let mut final_output = String::new();
    let mut last_model = String::new();

    let available_tools = if allowed_mcp_tools.is_empty() {
        mcp.list_all_tools().await
    } else {
        skill_loader::filter_tools_for_node(&mcp.list_all_tools().await, allowed_mcp_tools)
    };

    let tool_list_str = if available_tools.is_empty() {
        "No MCP tools available. You can only use 'done' action.".to_string()
    } else {
        available_tools
            .iter()
            .map(|t| format!("- {}: {}", t.name, t.description))
            .collect::<Vec<_>>()
            .join("\n")
    };

    let dep_context: String = deps
        .iter()
        .filter(|d| d.succeeded)
        .map(|d| d.output.clone())
        .collect::<Vec<_>>()
        .join("\n---\n");

    for iteration in 0..max_iterations {
        let history_str = if history.len() > MAX_HISTORY_ENTRIES {
            let start = history.len() - MAX_HISTORY_ENTRIES;
            history[start..].join("\n\n")
        } else {
            history.join("\n\n")
        };

        let react_prompt = format!(
            "You are an interactive agent using the ReAct pattern (Thought → Action → Observation).\n\n\
             TASK: {task}\n\n\
             PLANNING CONTEXT:\n{dep_context}\n\n\
             AVAILABLE TOOLS:\n{tool_list_str}\n\n\
             CONVERSATION HISTORY:\n{history_str}\n\n\
             Respond in this EXACT JSON format:\n\
             {{\"thought\": \"your reasoning about what to do next\", \
               \"action\": {{\"type\": \"mcp_tool_call\", \"tool_name\": \"server/tool\", \"arguments\": {{...}}}} }}\n\
             OR\n\
             {{\"thought\": \"your reasoning\", \
               \"action\": {{\"type\": \"done\", \"summary\": \"final answer and results\"}} }}\n\n\
             Choose 'done' when the task is fully complete. Respond with ONLY the JSON.",
        );

        let chunks = vec![ContextChunk {
            id: format!("react-iteration-{iteration}"),
            scope: ContextScope::AgentPrivate,
            kind: ContextKind::Instructions,
            content: react_prompt.clone(),
            priority: 1.0,
        }];
        let optimized = context.optimize(chunks);
        let brief = context.build_structured_brief(task.to_string(), vec![], vec![], vec![]);

        let input = AgentInput {
            task: task.to_string(),
            instructions: react_prompt,
            context: optimized,
            dependency_outputs: vec![],
            brief,
            working_dir: working_dir.clone(),
        };

        let output = match agents
            .run_role(AgentRole::ToolCaller, input, router.clone())
            .await
        {
            Ok(o) => o,
            Err(e) => {
                warn!("Interactive iteration {iteration} LLM call failed: {e}");
                history.push(format!(
                    "Iteration {iteration}:\nError: LLM call failed: {e}"
                ));
                continue;
            }
        };

        last_model = output.model.clone();

        // Parse the JSON response
        let raw = output.content.trim();
        let json_str = extract_json_block(raw);

        let parsed: serde_json::Value = match serde_json::from_str(json_str) {
            Ok(v) => v,
            Err(e) => {
                warn!("Interactive iteration {iteration} JSON parse failed: {e}");
                history.push(format!(
                    "Iteration {iteration}:\nThought: (parse error)\nAction: none\nObservation: Failed to parse response as JSON: {e}"
                ));
                continue;
            }
        };

        let thought = parsed
            .get("thought")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        let action = parsed
            .get("action")
            .cloned()
            .unwrap_or(serde_json::json!({}));
        let action_type = action
            .get("type")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string();

        match action_type.as_str() {
            "mcp_tool_call" => {
                let tool_name = action
                    .get("tool_name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let arguments = action
                    .get("arguments")
                    .cloned()
                    .unwrap_or(serde_json::json!({}));

                debug!("Interactive iteration {iteration}: calling tool {tool_name}");

                let observation = match mcp.call_tool(&tool_name, arguments).await {
                    Ok(result) => {
                        if result.succeeded {
                            truncate_str(&result.content, 4000)
                        } else {
                            format!(
                                "Tool error: {}",
                                result.error.as_deref().unwrap_or("unknown")
                            )
                        }
                    }
                    Err(e) => format!("Tool call failed: {e}"),
                };

                let observation_preview = truncate_str(&observation, 500);

                event_sink(RuntimeEvent::InteractiveStep {
                    node_id: node.id.clone(),
                    iteration,
                    thought: truncate_str(&thought, 200),
                    action_type: format!("mcp_tool_call:{tool_name}"),
                    observation_preview: observation_preview.clone(),
                });

                let _ = memory
                    .append_run_action_event(
                        run_id,
                        session_id,
                        RunActionType::InteractiveStep,
                        Some("node"),
                        Some(&node.id),
                        None,
                        serde_json::json!({
                            "iteration": iteration,
                            "thought": &thought,
                            "action": "mcp_tool_call",
                            "tool_name": &tool_name,
                            "observation_preview": &observation_preview,
                        }),
                    )
                    .await;

                history.push(format!(
                    "Iteration {iteration}:\nThought: {thought}\nAction: call {tool_name}\nObservation: {observation}"
                ));
            }
            "done" => {
                let summary = action
                    .get("summary")
                    .and_then(|v| v.as_str())
                    .unwrap_or(&thought)
                    .to_string();

                event_sink(RuntimeEvent::InteractiveStep {
                    node_id: node.id.clone(),
                    iteration,
                    thought: truncate_str(&thought, 200),
                    action_type: "done".to_string(),
                    observation_preview: truncate_str(&summary, 200),
                });

                let _ = memory
                    .append_run_action_event(
                        run_id,
                        session_id,
                        RunActionType::InteractiveStep,
                        Some("node"),
                        Some(&node.id),
                        None,
                        serde_json::json!({
                            "iteration": iteration,
                            "thought": &thought,
                            "action": "done",
                            "summary_preview": truncate_str(&summary, 500),
                        }),
                    )
                    .await;

                final_output = summary;
                break;
            }
            other => {
                warn!("Interactive iteration {iteration}: unknown action type '{other}'");
                history.push(format!(
                    "Iteration {iteration}:\nThought: {thought}\nAction: unknown({other})\nObservation: Unknown action type, use 'mcp_tool_call' or 'done'."
                ));
            }
        }
    }

    let converged = !final_output.is_empty();
    let output = if converged {
        final_output
    } else {
        format!(
            "Max iterations ({max_iterations}) reached without convergence.\n\nHistory:\n{}",
            history.join("\n\n")
        )
    };

    NodeExecutionResult {
        node_id: node.id.clone(),
        role: node.role,
        model: last_model,
        output,
        duration_ms: started.elapsed().as_millis(),
        succeeded: converged,
        error: if converged {
            None
        } else {
            Some(format!("Did not converge in {max_iterations} iterations"))
        },
    }
}

/// Extract the first JSON object from text (handles markdown code blocks).
fn extract_json_block(text: &str) -> &str {
    let trimmed = text.trim();
    // Strip markdown ```json ... ``` wrapper
    if let Some(start) = trimmed.find("```json") {
        let after = &trimmed[start + 7..];
        if let Some(end) = after.find("```") {
            return after[..end].trim();
        }
    }
    if let Some(start) = trimmed.find("```") {
        let after = &trimmed[start + 3..];
        if let Some(end) = after.find("```") {
            let block = after[..end].trim();
            if block.starts_with('{') {
                return block;
            }
        }
    }
    trimmed
}

fn truncate_str(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        let boundary = s.floor_char_boundary(max);
        format!("{}...", &s[..boundary])
    }
}
