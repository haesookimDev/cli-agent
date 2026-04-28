//! Heuristic requirement analysis layer (TODO 7-1, shadow mode).
//!
//! Sits between user input and `classify_task` to capture the *shape* of a
//! request, not just its dominant TaskType. Today this is purely heuristic —
//! no LLM call — so it costs nothing to run on every submission. Run results
//! are emitted as a `requirement_analyzed` payload alongside SubtaskPlanned
//! so dashboards can compare planner output to the upstream analysis.
//!
//! The orchestrator keeps using the legacy `classify_task` path for graph
//! building. Once we trust the analyzer we can promote it to drive
//! WorkflowComposer (TODO 7-2).
//!
//! Designed to be pure + cheap so the call site can sprinkle it without
//! worrying about latency or cost.

use serde::{Deserialize, Serialize};

use crate::types::TaskType;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Complexity {
    Simple,
    Moderate,
    Complex,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Priority {
    Low,
    Normal,
    High,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RequirementAnalysis {
    pub primary_intent: TaskType,
    pub sub_intents: Vec<TaskType>,
    pub required_capabilities: Vec<String>,
    pub priority: Priority,
    pub estimated_complexity: Complexity,
    pub context_requirements: Vec<String>,
}

/// Heuristic analyzer. Inputs:
/// - `task` — the raw user prompt
/// - `primary` — the TaskType already chosen by `classify_task`, used so we
///   stay in sync with the existing classifier rather than disagreeing.
pub fn analyze(task: &str, primary: TaskType) -> RequirementAnalysis {
    let lower = task.to_lowercase();

    let mut sub_intents = Vec::new();
    let interleaves_implementation_and_explanation =
        lower.contains("implement") || lower.contains("write code");
    if interleaves_implementation_and_explanation
        && (lower.contains("explain") || lower.contains("describe"))
    {
        sub_intents.push(TaskType::SimpleQuery);
    }
    if (lower.contains("test") || lower.contains("verify"))
        && primary == TaskType::CodeGeneration
    {
        sub_intents.push(TaskType::Analysis);
    }

    let mut required_capabilities = Vec::new();
    if lower.contains("github.com") || lower.contains("pull request") || lower.contains(" pr ") {
        required_capabilities.push("github".to_string());
    }
    if lower.contains("git ") || lower.contains("commit") || lower.contains("branch") {
        required_capabilities.push("git".to_string());
    }
    if lower.contains("read file") || lower.contains("write file") || lower.contains("filesystem")
    {
        required_capabilities.push("filesystem".to_string());
    }
    if lower.contains("http") || lower.contains("api ") || lower.contains("fetch ") {
        required_capabilities.push("http".to_string());
    }

    // Word-count heuristic for complexity.
    let words = task.split_whitespace().count();
    let estimated_complexity = if words < 8 {
        Complexity::Simple
    } else if words >= 40 {
        Complexity::Complex
    } else {
        Complexity::Moderate
    };
    // Anything Complex / ExternalProject is at least Moderate even on a
    // short prompt because the classifier already promoted the TaskType.
    let estimated_complexity = match (primary, estimated_complexity) {
        (TaskType::Complex, Complexity::Simple)
        | (TaskType::ExternalProject, Complexity::Simple) => Complexity::Moderate,
        (_, c) => c,
    };

    let priority = if lower.contains("urgent") || lower.contains("asap") || lower.contains("now") {
        Priority::High
    } else {
        Priority::Normal
    };

    let mut context_requirements = Vec::new();
    if lower.contains("this repo") || lower.contains("this codebase") || lower.contains("our code")
    {
        context_requirements.push("local_repo".to_string());
    }
    if lower.contains("yesterday") || lower.contains("previous") || lower.contains("earlier") {
        context_requirements.push("session_history".to_string());
    }

    RequirementAnalysis {
        primary_intent: primary,
        sub_intents,
        required_capabilities,
        priority,
        estimated_complexity,
        context_requirements,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_simple_query_is_simple() {
        let a = analyze("what is rust?", TaskType::SimpleQuery);
        assert_eq!(a.primary_intent, TaskType::SimpleQuery);
        assert_eq!(a.estimated_complexity, Complexity::Simple);
        assert!(a.required_capabilities.is_empty());
    }

    #[test]
    fn long_planning_task_is_complex() {
        let task = "Plan and implement a multi-stage refactor that introduces a new \
                    workflow composer, updates every call site, retrofits all the \
                    integration and unit tests, captures token usage end-to-end across \
                    providers, refreshes the trace UI to surface per-node consumption, \
                    and adds documentation for the new public API surface.";
        assert!(task.split_whitespace().count() >= 40, "test prompt too short");
        let a = analyze(task, TaskType::Complex);
        assert_eq!(a.estimated_complexity, Complexity::Complex);
    }

    #[test]
    fn complex_task_promotes_simple_to_moderate() {
        let a = analyze("Refactor everything", TaskType::Complex);
        assert_eq!(a.estimated_complexity, Complexity::Moderate);
    }

    #[test]
    fn detects_github_capability() {
        let a = analyze(
            "Open a pull request on github.com/anthropics/something",
            TaskType::CodeGeneration,
        );
        assert!(a.required_capabilities.iter().any(|c| c == "github"));
    }

    #[test]
    fn detects_git_capability_when_committing() {
        let a = analyze(
            "Run git commit and push the branch",
            TaskType::CodeGeneration,
        );
        assert!(a.required_capabilities.iter().any(|c| c == "git"));
    }

    #[test]
    fn urgent_marker_lifts_priority() {
        let a = analyze("Fix the prod bug ASAP", TaskType::CodeGeneration);
        assert_eq!(a.priority, Priority::High);
    }

    #[test]
    fn local_repo_context_detected() {
        let a = analyze(
            "Walk through this repo and summarize the orchestrator",
            TaskType::Analysis,
        );
        assert!(
            a.context_requirements.iter().any(|c| c == "local_repo"),
            "expected local_repo context for `this repo`"
        );
    }

    #[test]
    fn implementation_plus_explanation_adds_sub_intent() {
        let a = analyze(
            "Implement the new feature and explain how it works",
            TaskType::CodeGeneration,
        );
        assert!(a.sub_intents.iter().any(|t| matches!(t, TaskType::SimpleQuery)));
    }
}
