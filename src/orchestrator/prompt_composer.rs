use crate::types::PromptLayers;

/// 6-layer prompt assembly.
///
/// Layers:
/// 1. SystemPolicy  — global system rules, role prompt
/// 2. TaskIntent     — current user task + node instructions
/// 3. SessionAnchor  — session continuity (recent messages + recent run summary)
/// 4. MemoryRetrieval— session memory hits + global knowledge hits
/// 5. FailureDelta   — retry/recovery context (omitted on first attempt)
/// 6. OutputSchema   — expected output format
pub struct PromptComposer;

impl PromptComposer {
    pub fn compose(layers: &PromptLayers) -> String {
        let mut prompt = String::with_capacity(4096);

        prompt.push_str("[SYSTEM_POLICY]\n");
        prompt.push_str(&layers.system_policy);
        prompt.push_str("\n\n");

        prompt.push_str("[TASK_INTENT]\n");
        prompt.push_str(&layers.task_intent);
        prompt.push_str("\n\n");

        if !layers.session_anchor.is_empty() {
            prompt.push_str("[SESSION_ANCHOR]\n");
            prompt.push_str(&layers.session_anchor);
            prompt.push_str("\n\n");
        }

        if !layers.memory_retrieval.is_empty() {
            prompt.push_str("[MEMORY]\n");
            prompt.push_str(&layers.memory_retrieval);
            prompt.push_str("\n\n");
        }

        if let Some(ref delta) = layers.failure_delta {
            prompt.push_str("[FAILURE_DELTA]\n");
            prompt.push_str(delta);
            prompt.push_str("\n\n");
        }

        if !layers.output_schema.is_empty() {
            prompt.push_str("[OUTPUT_SCHEMA]\n");
            prompt.push_str(&layers.output_schema);
            prompt.push_str("\n\n");
        }

        prompt
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compose_includes_all_layers() {
        let layers = PromptLayers {
            system_policy: "You are a planner.".to_string(),
            task_intent: "Build a REST API".to_string(),
            session_anchor: "Previous: user asked about auth".to_string(),
            memory_retrieval: "Key fact: uses JWT".to_string(),
            failure_delta: Some("Last attempt failed: missing schema".to_string()),
            output_schema: "Respond in JSON".to_string(),
        };
        let result = PromptComposer::compose(&layers);
        assert!(result.contains("[SYSTEM_POLICY]"));
        assert!(result.contains("[TASK_INTENT]"));
        assert!(result.contains("[SESSION_ANCHOR]"));
        assert!(result.contains("[MEMORY]"));
        assert!(result.contains("[FAILURE_DELTA]"));
        assert!(result.contains("[OUTPUT_SCHEMA]"));
        assert!(result.contains("You are a planner."));
        assert!(result.contains("Build a REST API"));
    }

    #[test]
    fn compose_omits_empty_optional_layers() {
        let layers = PromptLayers {
            system_policy: "role".to_string(),
            task_intent: "task".to_string(),
            session_anchor: String::new(),
            memory_retrieval: String::new(),
            failure_delta: None,
            output_schema: String::new(),
        };
        let result = PromptComposer::compose(&layers);
        assert!(result.contains("[SYSTEM_POLICY]"));
        assert!(result.contains("[TASK_INTENT]"));
        assert!(!result.contains("[SESSION_ANCHOR]"));
        assert!(!result.contains("[MEMORY]"));
        assert!(!result.contains("[FAILURE_DELTA]"));
        assert!(!result.contains("[OUTPUT_SCHEMA]"));
    }
}
