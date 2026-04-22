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

pub fn looks_like_follow_up_task(task: &str) -> bool {
    let lower = task.trim().to_lowercase();
    let token_count = lower.split_whitespace().count();
    if token_count <= 5 {
        return true;
    }

    let follow_up_markers = [
        "그거",
        "이거",
        "저거",
        "거기",
        "로컬에",
        "local",
        "that",
        "it",
        "there",
        "맞아",
        "응",
        "계속",
        "이어서",
    ];
    follow_up_markers.iter().any(|kw| lower.contains(kw))
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
pub fn classify_task_fallback(task: &str, has_mcp: bool) -> TaskType {
    let lower = task.to_lowercase();

    let config_kw = ["setting", "config", "model", "provider", "backend"];
    if config_kw.iter().any(|kw| lower.contains(kw)) {
        return TaskType::Configuration;
    }
    if has_mcp {
        let tool_kw = ["mcp", "file", "repo", "commit", "branch", "search"];
        if tool_kw.iter().any(|kw| lower.contains(kw)) {
            return TaskType::ToolOperation;
        }
    }
    let external_kw = ["clone", "github.com", "gitlab.com", "bitbucket.org"];
    if external_kw.iter().any(|kw| lower.contains(kw))
        || (lower.contains("https://") && lower.contains(".git"))
    {
        return TaskType::ExternalProject;
    }

    let code_kw = [
        "code",
        "implement",
        "function",
        "refactor",
        "bug",
        "fix",
        "debug",
    ];
    if code_kw.iter().any(|kw| lower.contains(kw)) {
        return TaskType::CodeGeneration;
    }
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
