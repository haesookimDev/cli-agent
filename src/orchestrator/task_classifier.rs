//! Task classification heuristics and LLM-backed TaskType inference.
//!
//! Functions here are stateless. Known issues in the current heuristics are
//! tracked as TODO 2-2 (LLM call caching), 2-3 (keyword oversensitivity),
//! and 2-5 (short-task follow-up false positives).

use crate::router::ModelRouter;
use crate::types::TaskType;

pub fn has_explicit_remote_repo_reference(task: &str) -> bool {
    let lower = task.to_lowercase();
    let remote_markers = [
        "github",
        "gitlab",
        "bitbucket",
        "http://",
        "https://",
        "owner/repo",
        "repository url",
        "remote repo",
    ];
    remote_markers.iter().any(|kw| lower.contains(kw))
}

pub fn is_local_workspace_task(task: &str) -> bool {
    let lower = task.to_lowercase();
    let local_markers = [
        "로컬",
        "local",
        "workspace",
        "현재 폴더",
        "현재 디렉토리",
        "this project",
        "this repo",
        "프로젝트 폴더",
        "경로",
        "파일",
        "readme.md",
        "current directory",
        "current folder",
        "my project",
        "my repo",
        "working directory",
        "작성",
        "수정",
        "편집",
        "커밋",
    ];

    let has_path_like = lower.contains('/')
        && !lower.contains("http://")
        && !lower.contains("https://")
        && !lower.contains("github.com");

    (local_markers.iter().any(|kw| lower.contains(kw)) || has_path_like)
        && !has_explicit_remote_repo_reference(lower.as_str())
}

/// Decide whether `task` looks like a short follow-up reply that only makes
/// sense in the context of the previous user turn (so the memory-retrieval
/// query should be concatenated with the prior message).
///
/// Uses verb-vs-referent pivot rather than raw length so independent short
/// imperatives like "Fix this bug" don't get flagged just because they're
/// short (TODO 2-5). All keyword checks are word-boundary-aware, so
/// "commit" no longer matches the "it" referent marker.
pub fn looks_like_follow_up_task(task: &str) -> bool {
    use crate::orchestrator::helpers::contains_word;

    let lower = task.trim().to_lowercase();
    if lower.is_empty() {
        return false;
    }
    let token_count = lower.split_whitespace().count();

    // Words that start a concrete new ask — presence of any of these means
    // the message is a fresh task, not a contextual follow-up.
    const FRESH_TASK_VERBS: &[&str] = &[
        "fix", "add", "remove", "create", "delete", "implement", "write",
        "refactor", "debug", "test", "run", "build", "deploy", "analyze",
        "review", "clone", "check", "update", "explain", "show", "list",
        "수정", "추가", "삭제", "생성", "구현", "작성", "검토", "분석",
    ];
    let has_fresh_verb = FRESH_TASK_VERBS
        .iter()
        .any(|v| contains_word(lower.as_str(), v));
    if has_fresh_verb {
        return false;
    }

    // Pronouns / demonstratives that only resolve against previous context.
    const REFERENT_MARKERS: &[&str] = &[
        "그거", "이거", "저거", "거기", "로컬에",
        "that", "it", "there", "them",
        "이어서", "계속",
    ];
    // Bare confirmation / go-ahead signals.
    const CONFIRMATION_MARKERS: &[&str] = &[
        "맞아", "응", "네",
        "yes", "yep", "sure", "ok", "okay",
    ];

    let has_referent = REFERENT_MARKERS
        .iter()
        .any(|m| contains_word(lower.as_str(), m));
    let has_confirmation = CONFIRMATION_MARKERS
        .iter()
        .any(|m| contains_word(lower.as_str(), m));

    // Very short utterance without any verb — treat as follow-up (likely an
    // acknowledgment or demonstrative).
    if token_count <= 2 {
        return true;
    }
    has_referent || has_confirmation
}

/// Detects explicit continuation/execution commands that should inherit
/// the previous run's TaskType rather than being classified independently.
pub fn is_continuation_command(task: &str) -> bool {
    let lower = task.trim().to_lowercase();
    if lower.split_whitespace().count() > 6 {
        return false;
    }
    let continuation_markers = [
        "실행해",
        "진행해",
        "계속해",
        "이어서",
        "다음 작업",
        "go ahead",
        "do it",
        "execute",
        "proceed",
        "continue",
        "run it",
        "carry on",
    ];
    continuation_markers.iter().any(|kw| lower.contains(kw))
}

pub async fn classify_task(
    task: &str,
    mcp_server_names: &[String],
    mcp_tool_names: &[String],
    previous_task_type: Option<TaskType>,
    router: &ModelRouter,
    repo_url: Option<&str>,
    working_dir: Option<&std::path::Path>,
) -> TaskType {
    if repo_url.is_some() {
        return TaskType::ExternalProject;
    }

    if is_continuation_command(task) {
        if let Some(prev_type) = previous_task_type {
            return prev_type;
        }
    }

    let has_mcp = !mcp_server_names.is_empty();
    let lower = task.to_lowercase();

    if has_mcp {
        let matches_server = mcp_server_names
            .iter()
            .any(|name| lower.contains(&name.to_lowercase()));
        let matches_tool = mcp_tool_names.iter().any(|name| {
            let bare = name.rsplit('/').next().unwrap_or(name);
            lower.contains(&bare.to_lowercase())
        });
        if matches_server || matches_tool {
            return TaskType::ToolOperation;
        }
    }

    let tool_context = if has_mcp {
        format!(
            "\nAvailable MCP tools: [{}]",
            mcp_tool_names
                .iter()
                .take(15)
                .cloned()
                .collect::<Vec<_>>()
                .join(", ")
        )
    } else {
        String::new()
    };

    let classification_prompt = format!(
        "Classify the following user task into exactly ONE type.\n\n\
         Task types:\n\
         - simple_query: Questions or lookups that need no external action (e.g. greetings, factual Q&A, explanations)\n\
         - analysis: Tasks requiring data extraction, pattern analysis, or evaluation\n\
         - code_generation: Tasks involving writing, fixing, refactoring, or debugging code\n\
         - configuration: Tasks that CHANGE system settings (enable/disable features, switch models, update preferences)\n\
         - config_query: Tasks that ASK ABOUT current system state or settings without changing them (e.g. \"what backend is active?\", \"show current model\")\n\
         - tool_operation: Tasks requiring external tool calls (file I/O, API calls, MCP tools){tool_ctx}\n\
         - external_project: Tasks involving cloning, analyzing, or modifying an external repository given a URL or path\n\
         - interactive: Open-ended tasks requiring multi-step reasoning, exploration, or iterative tool usage\n\
         - complex: Multi-step tasks spanning multiple categories\n\n\
         User task: \"{task}\"\n\n\
         Respond with ONLY the type name, nothing else.",
        tool_ctx = tool_context,
        task = task,
    );

    let constraints = crate::router::RoutingConstraints {
        quality_weight: 0.3,
        latency_budget_ms: 3_000,
        cost_budget: 0.01,
        min_context: 1_000,
        tool_call_weight: 0.0,
    };

    match router
        .infer_in_dir(
            crate::types::TaskProfile::General,
            &classification_prompt,
            &constraints,
            working_dir,
        )
        .await
    {
        Ok((_decision, result)) => {
            let output = result.output.trim().to_lowercase();
            match output.as_str() {
                "simple_query" => TaskType::SimpleQuery,
                "analysis" => TaskType::Analysis,
                "code_generation" => TaskType::CodeGeneration,
                "configuration" => TaskType::Configuration,
                "config_query" => TaskType::ConfigQuery,
                "tool_operation" => TaskType::ToolOperation,
                "external_project" => TaskType::ExternalProject,
                "interactive" => TaskType::Interactive,
                "complex" => TaskType::Complex,
                _ => classify_task_fallback(task, has_mcp),
            }
        }
        Err(_) => classify_task_fallback(task, has_mcp),
    }
}

/// Fast keyword-based fallback when LLM classification is unavailable.
///
/// Uses a small precedence ladder instead of naive "first-match wins":
///   1. Explicit external-project markers (URLs, clone).
///   2. Coding verbs (fix/debug/bug/refactor/…) — these beat config nouns
///      so "debugging a settings display bug" no longer routes to
///      Configuration (TODO 2-3).
///   3. Configuration: needs a config verb ("change", "enable", …) alongside
///      a config noun ("setting", "model", …), or a config noun with no
///      coding verb present.
///   4. Tool operation (only when MCP is registered).
///   5. Analysis / interactive / simple / complex — by length + keywords.
pub fn classify_task_fallback(task: &str, has_mcp: bool) -> TaskType {
    use crate::orchestrator::helpers::contains_word;

    let lower = task.to_lowercase();

    // (1) External project — strongest signal (URL or explicit verb).
    let external_kw = ["clone", "github.com", "gitlab.com", "bitbucket.org"];
    if external_kw.iter().any(|kw| lower.contains(kw))
        || (lower.contains("https://") && lower.contains(".git"))
    {
        return TaskType::ExternalProject;
    }

    // (2) Coding verbs — beat config nouns so "fix settings bug" stays
    // classified as CodeGeneration.
    const CODE_VERBS: &[&str] = &[
        "code",
        "implement",
        "function",
        "refactor",
        "bug",
        "fix",
        "debug",
        "patch",
        "rewrite",
    ];
    let has_code_verb = CODE_VERBS
        .iter()
        .any(|v| contains_word(lower.as_str(), v));
    if has_code_verb {
        return TaskType::CodeGeneration;
    }

    // (3) Configuration: need either an explicit config verb, or a bare
    // config noun when no coding verb was found above.
    const CONFIG_VERBS: &[&str] = &[
        "change",
        "update",
        "set",
        "enable",
        "disable",
        "switch",
        "toggle",
        "configure",
    ];
    const CONFIG_NOUNS: &[&str] = &["setting", "settings", "config", "provider", "backend"];
    let has_config_verb = CONFIG_VERBS
        .iter()
        .any(|v| contains_word(lower.as_str(), v));
    let has_config_noun = CONFIG_NOUNS
        .iter()
        .any(|n| contains_word(lower.as_str(), n));
    if has_config_noun && (has_config_verb || has_bare_model_ref(lower.as_str())) {
        return TaskType::Configuration;
    }

    // (4) Tool operation — only when an MCP server is registered.
    if has_mcp {
        let tool_kw = ["mcp", "file", "repo", "commit", "branch", "search"];
        if tool_kw.iter().any(|kw| contains_word(lower.as_str(), kw)) {
            return TaskType::ToolOperation;
        }
    }

    // (5) Analysis vs interactive vs simple/complex.
    let analysis_kw = ["analyze", "analysis", "pattern", "compare", "evaluate"];
    if analysis_kw.iter().any(|kw| lower.contains(kw)) {
        return TaskType::Analysis;
    }
    let interactive_kw = [
        "explore",
        "investigate",
        "figure out",
        "step by step",
        "iteratively",
    ];
    if interactive_kw.iter().any(|kw| lower.contains(kw)) {
        return TaskType::Interactive;
    }
    if lower.split_whitespace().count() <= 8 {
        return TaskType::SimpleQuery;
    }
    TaskType::Complex
}

/// "Show me the current model" → Configuration (a config read). Only true
/// if `model` appears as a bare noun, not inside words like `modeling`.
fn has_bare_model_ref(lower: &str) -> bool {
    use crate::orchestrator::helpers::contains_word;
    contains_word(lower, "model")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_short_imperatives_are_not_followups() {
        // The TODO 2-5 regression: 3-token fresh task misclassified.
        assert!(!looks_like_follow_up_task("Fix this bug"));
        assert!(!looks_like_follow_up_task("Add a button"));
        assert!(!looks_like_follow_up_task("Clone the repo"));
        assert!(!looks_like_follow_up_task("수정 해주세요"));
    }

    #[test]
    fn confirmations_and_referents_are_followups() {
        assert!(looks_like_follow_up_task("네"));
        assert!(looks_like_follow_up_task("yes"));
        assert!(looks_like_follow_up_task("ok go ahead"));
        assert!(looks_like_follow_up_task("그거 실행"));
        assert!(looks_like_follow_up_task("do that one"));
    }

    #[test]
    fn substring_it_in_commit_is_not_a_referent() {
        // Without word boundaries, "it" inside "commit" would have matched.
        assert!(!looks_like_follow_up_task("commit the changes"));
    }

    #[test]
    fn empty_input_is_not_a_followup() {
        assert!(!looks_like_follow_up_task(""));
        assert!(!looks_like_follow_up_task("   "));
    }

    // --- classify_task_fallback (TODO 2-3) ---

    #[test]
    fn fix_plus_settings_routes_to_code_generation() {
        // Regression: the TODO's concrete example.
        assert_eq!(
            classify_task_fallback("debugging a settings display bug", false),
            TaskType::CodeGeneration
        );
        assert_eq!(
            classify_task_fallback("fix the settings page crash", false),
            TaskType::CodeGeneration
        );
    }

    #[test]
    fn change_plus_settings_stays_configuration() {
        assert_eq!(
            classify_task_fallback("change the default model setting", false),
            TaskType::Configuration
        );
        assert_eq!(
            classify_task_fallback("enable the anthropic provider", false),
            TaskType::Configuration
        );
    }

    #[test]
    fn external_project_url_wins_over_other_keywords() {
        assert_eq!(
            classify_task_fallback(
                "fix something in https://github.com/a/b.git",
                false
            ),
            TaskType::ExternalProject
        );
    }

    #[test]
    fn tool_operation_requires_mcp() {
        // Without MCP, 'repo' falls through to simple/complex — not
        // misclassified as ToolOperation.
        let no_mcp = classify_task_fallback("search the repo", false);
        assert!(matches!(
            no_mcp,
            TaskType::SimpleQuery | TaskType::Complex
        ));
        // With MCP, same task is a ToolOperation.
        assert_eq!(
            classify_task_fallback("search the repo", true),
            TaskType::ToolOperation
        );
    }
}
