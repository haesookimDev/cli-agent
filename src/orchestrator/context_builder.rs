//! Build memory retrieval queries and context chunks from recent history.
//!
//! These helpers package user task context (the current task plus a few prior
//! messages and the last run's summary) into formats the memory subsystem and
//! LLM prompt assembly expect.

use uuid::Uuid;

use crate::context::{ContextChunk, ContextKind, ContextScope};
use crate::orchestrator::task_classifier::looks_like_follow_up_task;
use crate::types::{AgentRole, RunRecord};

/// Build the query string used to retrieve relevant memory items for the
/// current task. For short follow-up messages, prepend the preceding user
/// message so retrieval gets enough keywords to match on.
pub fn build_memory_query(
    current_task: &str,
    recent_messages: &[(i64, String, String, String)],
) -> String {
    let current = current_task.trim();
    if current.is_empty() {
        return String::new();
    }

    if !looks_like_follow_up_task(current) {
        return current.to_string();
    }

    let previous_user_message =
        recent_messages
            .iter()
            .rev()
            .find_map(|(_, role, content, _)| {
                if role == "user" && content.trim() != current {
                    Some(content.trim().to_string())
                } else {
                    None
                }
            });

    match previous_user_message {
        Some(prev) if !prev.is_empty() => format!("{current} {prev}"),
        _ => current.to_string(),
    }
}

/// Surface up to 4 recent user messages as ContextChunks for the planner.
/// The most recent message is the highest priority; priority decays by 0.03
/// per step back.
pub fn build_recent_history_chunks(
    recent_messages: &[(i64, String, String, String)],
    current_task: &str,
) -> Vec<ContextChunk> {
    let current = current_task.trim();
    let mut selected = Vec::new();

    for (_, role, content, _) in recent_messages.iter().rev() {
        if role != "user" {
            continue;
        }
        let trimmed = content.trim();
        if trimmed.is_empty() || trimmed == current {
            continue;
        }
        selected.push(trimmed.to_string());
        if selected.len() >= 4 {
            break;
        }
    }

    selected.reverse();
    selected
        .into_iter()
        .enumerate()
        .map(|(idx, content)| ContextChunk {
            id: format!("history-user-{idx}"),
            scope: ContextScope::SessionShared,
            kind: ContextKind::History,
            content: format!("Previous user message: {content}"),
            priority: 0.75 - (idx as f64 * 0.03),
        })
        .collect()
}

/// Summarize the most recent previous run's task + primary output, preferring
/// a Summarizer node's output when available.
pub fn build_recent_run_summary(current_run_id: Uuid, runs: &[RunRecord]) -> Option<String> {
    let previous = runs
        .iter()
        .find(|run| run.run_id != current_run_id && !run.outputs.is_empty())?;

    let primary_output = previous
        .outputs
        .iter()
        .find(|o| o.role == AgentRole::Summarizer && !o.output.trim().is_empty())
        .or_else(|| {
            previous
                .outputs
                .iter()
                .find(|o| !o.output.trim().is_empty())
        })?;

    Some(format!(
        "Previous run task: {}\nPrevious run outcome: {}",
        trim_for_context(previous.task.as_str(), 220),
        trim_for_context(primary_output.output.as_str(), 420)
    ))
}

/// Truncate text to `max_chars` characters total, keeping head and tail halves
/// around a `... [trimmed] ...` marker so the reader still sees both ends.
pub fn trim_for_context(text: &str, max_chars: usize) -> String {
    let normalized = text.trim();
    if normalized.chars().count() <= max_chars {
        return normalized.to_string();
    }

    let head_len = max_chars / 2;
    let tail_len = max_chars.saturating_sub(head_len);
    let head = normalized.chars().take(head_len).collect::<String>();
    let tail = normalized
        .chars()
        .rev()
        .take(tail_len)
        .collect::<String>()
        .chars()
        .rev()
        .collect::<String>();
    format!("{head}\n... [trimmed] ...\n{tail}")
}
