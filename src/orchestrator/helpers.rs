//! Pure utility functions used across the orchestrator.
//!
//! Everything in this module is stateless and side-effect free. Keep it that
//! way — anything that touches `Orchestrator`, memory, router, or IO belongs
//! in a different submodule.

use std::collections::HashSet;

use crate::orchestrator::tool_augment;
use crate::types::{AgentRole, McpToolDefinition};

/// Strip markdown code fences and common JSON-response wrappers from raw
/// LLM output, leaving a plain-text body suitable for downstream parsing.
pub fn clean_llm_output(raw: &str) -> String {
    let mut text = raw.trim().to_string();

    // Strip markdown code fences
    if text.starts_with("```") {
        if let Some(end_of_first_line) = text.find('\n') {
            text = text[end_of_first_line + 1..].to_string();
        }
        if text.ends_with("```") {
            text = text[..text.len() - 3].to_string();
        }
        text = text.trim().to_string();
    }

    // If the output looks like a JSON object with common LLM response fields, extract content
    if text.starts_with('{') && text.ends_with('}') {
        if let Ok(obj) = serde_json::from_str::<serde_json::Value>(&text) {
            for key in [
                "content", "answer", "response", "summary", "text", "message",
            ] {
                if let Some(val) = obj.get(key).and_then(|v| v.as_str()) {
                    return val.trim().to_string();
                }
            }
        }
    }

    text
}

pub fn contains_any_keyword(haystack: &str, keywords: &[&str]) -> bool {
    keywords.iter().any(|keyword| haystack.contains(keyword))
}

/// Case-insensitive, word-boundary-aware `contains`. A byte is considered a
/// "word char" if it's ASCII alphanumeric or `_`; the needle matches only
/// when the surrounding characters (if any) are not word chars.
///
/// Used by `auto_skill_route` (TODO 2-4) so short skill IDs like "ci" don't
/// false-positive against words like "configure" or "mechanic".
pub fn contains_word(haystack: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return false;
    }
    let hbytes = haystack.as_bytes();
    let nbytes = needle.as_bytes();
    if nbytes.len() > hbytes.len() {
        return false;
    }
    let is_word_byte = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    for i in 0..=(hbytes.len() - nbytes.len()) {
        if hbytes[i..i + nbytes.len()].eq_ignore_ascii_case(nbytes) {
            let left_ok = i == 0 || !is_word_byte(hbytes[i - 1]);
            let right_ok =
                i + nbytes.len() == hbytes.len() || !is_word_byte(hbytes[i + nbytes.len()]);
            if left_ok && right_ok {
                return true;
            }
        }
    }
    false
}

pub fn infer_clone_target_dir(repo_url: &str) -> String {
    let candidate = repo_url
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or("cloned-repo")
        .trim_end_matches(".git")
        .trim()
        .to_string()
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
        .collect::<String>()
        .chars()
        .take(80)
        .collect::<String>()
        .trim_matches('.')
        .trim()
        .to_string();

    if candidate.is_empty() {
        "cloned-repo".to_string()
    } else {
        candidate
    }
}

pub fn format_tool_catalog(tools: &[McpToolDefinition]) -> String {
    tools
        .iter()
        .map(|tool| {
            format!(
                "- {}: {} | {}",
                tool.name,
                tool.description,
                summarize_tool_input_schema(&tool.input_schema)
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn summarize_tool_input_schema(schema: &serde_json::Value) -> String {
    let properties = schema
        .get("properties")
        .and_then(|value| value.as_object())
        .map(|properties| {
            let mut keys = properties.keys().cloned().collect::<Vec<_>>();
            keys.sort();
            keys
        })
        .unwrap_or_default();

    let required = schema
        .get("required")
        .and_then(|value| value.as_array())
        .map(|values| {
            values
                .iter()
                .filter_map(|value| value.as_str().map(ToString::to_string))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    match (properties.is_empty(), required.is_empty()) {
        (true, true) => "args: none".to_string(),
        (false, true) => format!("args: {}", properties.join(", ")),
        (false, false) => format!(
            "args: {} | required: {}",
            properties.join(", "),
            required.join(", ")
        ),
        (true, false) => format!("required: {}", required.join(", ")),
    }
}

/// Extract the first complete JSON object `{...}` from `text`.
/// Handles three common LLM output patterns:
///   1. Raw JSON: `{"subtasks": [...]}`
///   2. Markdown fenced block: ` ```json\n{...}\n``` `
///   3. Prose followed by JSON: `Here is the plan: {...}`
pub fn extract_json_object(text: &str) -> Option<&str> {
    for fence in &["```json\n", "```\n", "```json ", "```"] {
        if let Some(start) = text.find(fence) {
            let after_fence = &text[start + fence.len()..];
            if let Some(end_fence) = after_fence.find("```") {
                let candidate = after_fence[..end_fence].trim();
                if candidate.starts_with('{') {
                    return Some(candidate);
                }
            }
        }
    }

    let start = text.find('{')?;
    let bytes = text.as_bytes();
    let mut depth: i32 = 0;
    let mut in_string = false;
    let mut escape_next = false;
    for (i, &b) in bytes[start..].iter().enumerate() {
        if escape_next {
            escape_next = false;
            continue;
        }
        match b {
            b'\\' if in_string => escape_next = true,
            b'"' => in_string = !in_string,
            b'{' if !in_string => depth += 1,
            b'}' if !in_string => {
                depth -= 1;
                if depth == 0 {
                    return Some(&text[start..start + i + 1]);
                }
            }
            _ => {}
        }
    }
    None
}

pub fn parse_agent_role(value: &str) -> Option<AgentRole> {
    match value {
        "planner" => Some(AgentRole::Planner),
        "extractor" => Some(AgentRole::Extractor),
        "coder" => Some(AgentRole::Coder),
        "summarizer" => Some(AgentRole::Summarizer),
        "fallback" => Some(AgentRole::Fallback),
        "tool_caller" => Some(AgentRole::ToolCaller),
        _ => None,
    }
}

pub fn extract_repo_url_from_text(text: &str) -> Option<String> {
    for word in text.split_whitespace() {
        let trimmed = word.trim_matches(|c: char| {
            !c.is_alphanumeric()
                && c != '/'
                && c != ':'
                && c != '.'
                && c != '-'
                && c != '_'
                && c != '@'
        });
        if (trimmed.starts_with("https://") || trimmed.starts_with("git@"))
            && (trimmed.contains("github.com")
                || trimmed.contains("gitlab.com")
                || trimmed.contains("bitbucket.org")
                || trimmed.ends_with(".git"))
        {
            return Some(trimmed.to_string());
        }
    }
    None
}

pub fn parse_tool_calls(raw: &str) -> Vec<serde_json::Value> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }

    let mut parsed = Vec::<serde_json::Value>::new();
    let mut seen_candidates = HashSet::<String>::new();
    for candidate in tool_call_parse_candidates(trimmed) {
        if !seen_candidates.insert(candidate.clone()) {
            continue;
        }
        collect_tool_calls_from_text(candidate.as_str(), &mut parsed);
    }

    if parsed.is_empty() {
        parsed.extend(tool_augment::extract_tool_calls(trimmed));
    }

    let mut seen = HashSet::<String>::new();
    let mut deduped = Vec::new();
    for call in parsed {
        let tool_name = call
            .get("tool_name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim();
        if tool_name.is_empty() {
            continue;
        }

        let key = serde_json::json!({
            "tool_name": tool_name,
            "arguments": call.get("arguments").cloned().unwrap_or(serde_json::json!({})),
        })
        .to_string();
        if seen.insert(key) {
            deduped.push(call);
        }
    }

    deduped
}

pub fn collect_tool_calls_from_text(raw: &str, out: &mut Vec<serde_json::Value>) {
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(raw) {
        collect_tool_calls(value, out);
        return;
    }

    let stream = serde_json::Deserializer::from_str(raw).into_iter::<serde_json::Value>();
    for value in stream.flatten() {
        collect_tool_calls(value, out);
    }
}

pub fn tool_call_parse_candidates(raw: &str) -> Vec<String> {
    let mut candidates = vec![raw.trim().to_string()];

    let cleaned = clean_llm_output(raw);
    if cleaned != raw.trim() {
        candidates.push(cleaned.clone());
    }

    candidates.extend(extract_markdown_code_blocks(raw));
    candidates.extend(extract_balanced_json_fragments(raw, 16));

    if cleaned != raw.trim() {
        candidates.extend(extract_markdown_code_blocks(cleaned.as_str()));
        candidates.extend(extract_balanced_json_fragments(cleaned.as_str(), 16));
    }

    candidates
}

pub fn extract_markdown_code_blocks(raw: &str) -> Vec<String> {
    let mut blocks = Vec::new();
    let mut cursor = raw;

    while let Some(open_idx) = cursor.find("```") {
        let after_open = &cursor[open_idx + 3..];
        let Some(first_newline) = after_open.find('\n') else {
            break;
        };
        let content_start = open_idx + 3 + first_newline + 1;
        let remainder = &cursor[content_start..];
        let Some(close_idx) = remainder.find("```") else {
            break;
        };
        let content = remainder[..close_idx].trim();
        if !content.is_empty() {
            blocks.push(content.to_string());
        }
        cursor = &remainder[close_idx + 3..];
    }

    blocks
}

pub fn extract_balanced_json_fragments(raw: &str, max_candidates: usize) -> Vec<String> {
    let mut fragments = Vec::new();

    for (start_idx, ch) in raw.char_indices() {
        if ch != '{' && ch != '[' {
            continue;
        }
        if let Some(end_idx) = find_balanced_json_end(raw, start_idx, ch) {
            let fragment = raw[start_idx..end_idx].trim();
            if !fragment.is_empty() {
                fragments.push(fragment.to_string());
                if fragments.len() >= max_candidates {
                    break;
                }
            }
        }
    }

    fragments
}

pub fn find_balanced_json_end(raw: &str, start_idx: usize, opening: char) -> Option<usize> {
    let mut expected = vec![match opening {
        '{' => '}',
        '[' => ']',
        _ => return None,
    }];
    let mut in_string = false;
    let mut escaped = false;

    for (offset, ch) in raw[start_idx..].char_indices() {
        if offset == 0 {
            continue;
        }

        if in_string {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }

        match ch {
            '"' => in_string = true,
            '{' => expected.push('}'),
            '[' => expected.push(']'),
            '}' | ']' => {
                if expected.pop() != Some(ch) {
                    return None;
                }
                if expected.is_empty() {
                    return Some(start_idx + offset + ch.len_utf8());
                }
            }
            _ => {}
        }
    }

    None
}

pub fn summarize_selector_output(raw: &str) -> String {
    let normalized = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut preview = String::new();
    let mut count = 0usize;

    for ch in normalized.chars() {
        if count >= 320 {
            preview.push_str("...");
            return preview;
        }
        preview.push(ch);
        count += 1;
    }

    preview
}

pub fn collect_tool_calls(value: serde_json::Value, out: &mut Vec<serde_json::Value>) {
    match value {
        serde_json::Value::Array(values) => {
            for item in values {
                collect_tool_calls(item, out);
            }
        }
        serde_json::Value::Object(map) => {
            if map.contains_key("tool_name") {
                out.push(serde_json::Value::Object(map));
            } else {
                for value in map.into_values() {
                    collect_tool_calls(value, out);
                }
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contains_word_requires_boundaries() {
        assert!(contains_word("run the ci pipeline", "ci"));
        assert!(contains_word("CI runs now", "ci"));
        assert!(contains_word("prefix ci", "ci"));
        assert!(contains_word("ci", "ci"));
    }

    #[test]
    fn contains_word_rejects_substring_inside_word() {
        // The original TODO 2-4 failure mode.
        assert!(!contains_word("circle the wagons", "ci"));
        assert!(!contains_word("mechanic", "ci"));
        assert!(!contains_word("configure this", "config"));
    }

    #[test]
    fn contains_word_treats_underscore_as_word_char() {
        // git_clone_repo should NOT match in "git_clone_repository".
        assert!(!contains_word(
            "git_clone_repository tool",
            "git_clone_repo"
        ));
        // But plain "git_clone_repo" as its own token matches.
        assert!(contains_word("use git_clone_repo now", "git_clone_repo"));
    }

    #[test]
    fn contains_word_handles_empty_needle() {
        assert!(!contains_word("anything", ""));
    }
}
