use tracing::{debug, warn};

use crate::mcp::McpRegistry;
use crate::types::McpToolCallResult;

/// Maximum tool augmentation rounds per node execution.
pub const MAX_TOOL_ROUNDS: usize = 3;

/// Extract `<tool_call>{"tool_name":"...","arguments":{...}}</tool_call>` blocks from LLM output.
pub fn extract_tool_calls(output: &str) -> Vec<serde_json::Value> {
    let mut calls = Vec::new();
    let mut remaining = output;
    while let Some(start) = remaining.find("<tool_call>") {
        let after_tag = &remaining[start + 11..];
        if let Some(end) = after_tag.find("</tool_call>") {
            let json_str = after_tag[..end].trim();
            if let Ok(val) = serde_json::from_str::<serde_json::Value>(json_str) {
                if val.get("tool_name").is_some() {
                    calls.push(val);
                }
            }
            remaining = &after_tag[end + 12..];
        } else {
            break;
        }
    }
    calls
}

/// Execute extracted tool calls against the MCP registry.
pub async fn execute_tool_calls(
    calls: &[serde_json::Value],
    mcp: &McpRegistry,
    allowed_tools: &[String],
) -> Vec<McpToolCallResult> {
    let mut results = Vec::new();
    for call in calls {
        let tool_name = call
            .get("tool_name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        // Check tool allowlist
        if !allowed_tools.is_empty()
            && !allowed_tools.iter().any(|a| {
                tool_name == *a || tool_name.contains(a.as_str()) || a.contains(tool_name.as_str())
            })
        {
            results.push(McpToolCallResult {
                tool_name: tool_name.clone(),
                succeeded: false,
                content: String::new(),
                error: Some(format!("Tool '{tool_name}' not in allowed list")),
                duration_ms: 0,
            });
            continue;
        }

        let arguments = call
            .get("arguments")
            .cloned()
            .unwrap_or(serde_json::json!({}));

        debug!("Tool augmentation: calling {tool_name}");
        match mcp.call_tool(&tool_name, arguments).await {
            Ok(result) => results.push(result),
            Err(e) => {
                warn!("Tool augmentation call failed for {tool_name}: {e}");
                results.push(McpToolCallResult {
                    tool_name,
                    succeeded: false,
                    content: String::new(),
                    error: Some(e.to_string()),
                    duration_ms: 0,
                });
            }
        }
    }
    results
}

/// Format tool results for injection into an agent follow-up prompt.
pub fn format_tool_results(results: &[McpToolCallResult]) -> String {
    results
        .iter()
        .map(|r| {
            format!(
                "<tool_result name=\"{}\" success=\"{}\">{}</tool_result>",
                r.tool_name,
                r.succeeded,
                if r.succeeded {
                    &r.content
                } else {
                    r.error.as_deref().unwrap_or("unknown error")
                },
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Remove `<tool_call>...</tool_call>` tags from the final output.
pub fn strip_tool_call_tags(output: &str) -> String {
    let mut result = output.to_string();
    while let Some(start) = result.find("<tool_call>") {
        if let Some(end_tag) = result[start..].find("</tool_call>") {
            let end = start + end_tag + 12;
            result = format!("{}{}", &result[..start], &result[end..]);
        } else {
            break;
        }
    }
    result.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_tool_calls_parses_tags() {
        let output = "Here is my analysis.\n<tool_call>{\"tool_name\": \"filesystem/read_file\", \"arguments\": {\"path\": \"src/main.rs\"}}</tool_call>\nMore text.";
        let calls = extract_tool_calls(output);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0]["tool_name"], "filesystem/read_file");
    }

    #[test]
    fn extract_tool_calls_handles_no_tags() {
        let output = "Just a normal response with no tool calls.";
        let calls = extract_tool_calls(output);
        assert!(calls.is_empty());
    }

    #[test]
    fn extract_tool_calls_multiple() {
        let output = "<tool_call>{\"tool_name\": \"a\", \"arguments\": {}}</tool_call> text <tool_call>{\"tool_name\": \"b\", \"arguments\": {}}</tool_call>";
        let calls = extract_tool_calls(output);
        assert_eq!(calls.len(), 2);
    }

    #[test]
    fn strip_tool_call_tags_removes_tags() {
        let output = "Before <tool_call>{\"tool_name\":\"t\"}</tool_call> After";
        let cleaned = strip_tool_call_tags(output);
        assert_eq!(cleaned, "Before  After");
    }

    #[test]
    fn strip_tool_call_tags_no_tags() {
        let output = "Just normal text";
        let cleaned = strip_tool_call_tags(output);
        assert_eq!(cleaned, "Just normal text");
    }

    #[test]
    fn format_tool_results_formats_correctly() {
        let results = vec![McpToolCallResult {
            tool_name: "test/tool".to_string(),
            succeeded: true,
            content: "result data".to_string(),
            error: None,
            duration_ms: 100,
        }];
        let formatted = format_tool_results(&results);
        assert!(formatted.contains("test/tool"));
        assert!(formatted.contains("result data"));
        assert!(formatted.contains("success=\"true\""));
    }

    #[test]
    fn extract_tool_calls_skips_malformed_json() {
        // Two blocks: first malformed, second valid. The malformed one must not
        // poison the parser — the valid one is still returned.
        let output = "<tool_call>{not json}</tool_call> later \
                      <tool_call>{\"tool_name\":\"good\",\"arguments\":{}}</tool_call>";
        let calls = extract_tool_calls(output);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0]["tool_name"], "good");
    }

    #[test]
    fn extract_tool_calls_skips_blocks_missing_tool_name() {
        // JSON parses but lacks the required tool_name key.
        let output = "<tool_call>{\"arguments\":{\"x\":1}}</tool_call>";
        assert!(extract_tool_calls(output).is_empty());
    }

    #[test]
    fn extract_tool_calls_handles_unterminated_tag() {
        // No closing tag — extractor must stop, not loop or panic.
        let output = "<tool_call>{\"tool_name\":\"x\",\"arguments\":{}}";
        assert!(extract_tool_calls(output).is_empty());
    }

    #[tokio::test]
    async fn execute_tool_calls_against_empty_registry_returns_failure() {
        let registry = McpRegistry::new();
        let calls = vec![serde_json::json!({
            "tool_name": "filesystem/read_file",
            "arguments": {"path": "/tmp/none"}
        })];
        let results = execute_tool_calls(&calls, &registry, &[]).await;
        assert_eq!(results.len(), 1);
        assert!(!results[0].succeeded);
        assert!(
            results[0]
                .error
                .as_deref()
                .unwrap_or("")
                .contains("No MCP server"),
            "missing-tool errors should surface as error text, not a panic"
        );
    }

    #[tokio::test]
    async fn execute_tool_calls_enforces_allowlist() {
        let registry = McpRegistry::new();
        let calls = vec![serde_json::json!({
            "tool_name": "github/create_issue",
            "arguments": {}
        })];
        // Allowlist that doesn't cover the requested tool.
        let allow = vec!["filesystem/read_file".to_string()];
        let results = execute_tool_calls(&calls, &registry, &allow).await;
        assert_eq!(results.len(), 1);
        assert!(!results[0].succeeded);
        assert!(
            results[0]
                .error
                .as_deref()
                .unwrap_or("")
                .contains("not in allowed list"),
            "allowlist rejection must surface in the error"
        );
    }

    #[tokio::test]
    async fn mcp_registry_call_tool_unknown_returns_error_not_panic() {
        let registry = McpRegistry::new();
        let result = registry
            .call_tool("nonexistent/tool", serde_json::json!({}))
            .await;
        assert!(result.is_err(), "unknown tool must return Err");
    }
}
