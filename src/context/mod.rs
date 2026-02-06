use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::types::StructuredBrief;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextScope {
    GlobalShared,
    SessionShared,
    AgentPrivate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextKind {
    System,
    Instructions,
    History,
    Retrieval,
    ToolResults,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextChunk {
    pub id: String,
    pub scope: ContextScope,
    pub kind: ContextKind,
    pub content: String,
    pub priority: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenBudget {
    pub max_context: usize,
    pub system_ratio: f64,
    pub instructions_ratio: f64,
    pub history_ratio: f64,
    pub retrieval_ratio: f64,
    pub tool_ratio: f64,
    pub output_reserve_ratio: f64,
}

impl TokenBudget {
    pub fn default_for(max_context: usize) -> Self {
        Self {
            max_context,
            system_ratio: 0.08,
            instructions_ratio: 0.15,
            history_ratio: 0.32,
            retrieval_ratio: 0.25,
            tool_ratio: 0.10,
            output_reserve_ratio: 0.10,
        }
    }

    pub fn budget_by_kind(&self, kind: ContextKind) -> usize {
        let ratio = match kind {
            ContextKind::System => self.system_ratio,
            ContextKind::Instructions => self.instructions_ratio,
            ContextKind::History => self.history_ratio,
            ContextKind::Retrieval => self.retrieval_ratio,
            ContextKind::ToolResults => self.tool_ratio,
        };
        (self.max_context as f64 * ratio) as usize
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OptimizedContext {
    pub system: Vec<String>,
    pub instructions: Vec<String>,
    pub history: Vec<String>,
    pub retrieval: Vec<String>,
    pub tool_results: Vec<String>,
    pub token_estimate: usize,
}

impl OptimizedContext {
    pub fn flatten(&self) -> String {
        [
            section("SYSTEM", &self.system),
            section("INSTRUCTIONS", &self.instructions),
            section("HISTORY", &self.history),
            section("RETRIEVAL", &self.retrieval),
            section("TOOLS", &self.tool_results),
        ]
        .join("\n\n")
    }
}

#[derive(Debug, Clone)]
pub struct ContextManager {
    budget: TokenBudget,
}

impl ContextManager {
    pub fn new(max_context: usize) -> Self {
        Self {
            budget: TokenBudget::default_for(max_context),
        }
    }

    pub fn optimize(&self, chunks: Vec<ContextChunk>) -> OptimizedContext {
        let deduped = dedup_chunks(chunks);
        let summarized = summarize_large_chunks(deduped);

        let mut grouped: HashMap<ContextKind, Vec<ContextChunk>> = HashMap::new();
        for chunk in summarized {
            grouped.entry(chunk.kind).or_default().push(chunk);
        }

        for values in grouped.values_mut() {
            values.sort_by(|a, b| {
                b.priority
                    .partial_cmp(&a.priority)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
        }

        let mut system = apply_budget(
            grouped.remove(&ContextKind::System).unwrap_or_default(),
            self.budget.budget_by_kind(ContextKind::System),
        );
        let mut instructions = apply_budget(
            grouped
                .remove(&ContextKind::Instructions)
                .unwrap_or_default(),
            self.budget.budget_by_kind(ContextKind::Instructions),
        );
        let mut history = apply_budget(
            grouped.remove(&ContextKind::History).unwrap_or_default(),
            self.budget.budget_by_kind(ContextKind::History),
        );
        let mut retrieval = apply_budget(
            grouped.remove(&ContextKind::Retrieval).unwrap_or_default(),
            self.budget.budget_by_kind(ContextKind::Retrieval),
        );
        let mut tool_results = apply_budget(
            grouped
                .remove(&ContextKind::ToolResults)
                .unwrap_or_default(),
            self.budget.budget_by_kind(ContextKind::ToolResults),
        );

        // overflow handling: deduplicate -> summarize -> low-value truncation -> retrieval top-k
        history = truncate_low_value(history, self.budget.budget_by_kind(ContextKind::History));
        retrieval = retrieval.into_iter().take(8).collect();

        let token_estimate = total_tokens(&system)
            + total_tokens(&instructions)
            + total_tokens(&history)
            + total_tokens(&retrieval)
            + total_tokens(&tool_results);

        OptimizedContext {
            system: std::mem::take(&mut system),
            instructions: std::mem::take(&mut instructions),
            history: std::mem::take(&mut history),
            retrieval: std::mem::take(&mut retrieval),
            tool_results: std::mem::take(&mut tool_results),
            token_estimate,
        }
    }

    pub fn build_structured_brief(
        &self,
        goal: impl Into<String>,
        constraints: Vec<String>,
        decisions: Vec<String>,
        references: Vec<String>,
    ) -> StructuredBrief {
        StructuredBrief {
            goal: goal.into(),
            constraints,
            decisions,
            references,
        }
    }
}

fn dedup_chunks(chunks: Vec<ContextChunk>) -> Vec<ContextChunk> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();

    for chunk in chunks {
        let normalized = chunk.content.trim().to_lowercase();
        if normalized.is_empty() {
            continue;
        }
        if seen.insert(normalized) {
            out.push(chunk);
        }
    }

    out
}

fn summarize_large_chunks(chunks: Vec<ContextChunk>) -> Vec<ContextChunk> {
    chunks
        .into_iter()
        .map(|mut chunk| {
            if estimate_tokens(chunk.content.as_str()) > 600 {
                let summarized = summarize_text(chunk.content.as_str(), 480);
                chunk.content = summarized;
                chunk.priority = (chunk.priority + 0.05).min(1.0);
            }
            chunk
        })
        .collect()
}

fn apply_budget(chunks: Vec<ContextChunk>, budget_tokens: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut used = 0usize;

    for chunk in chunks {
        let t = estimate_tokens(chunk.content.as_str());
        if used + t > budget_tokens {
            continue;
        }
        out.push(chunk.content);
        used += t;
    }

    out
}

fn truncate_low_value(mut items: Vec<String>, budget_tokens: usize) -> Vec<String> {
    let mut used = total_tokens(&items);
    while used > budget_tokens && !items.is_empty() {
        items.pop();
        used = total_tokens(&items);
    }
    items
}

fn total_tokens(items: &[String]) -> usize {
    items.iter().map(|s| estimate_tokens(s.as_str())).sum()
}

fn estimate_tokens(text: &str) -> usize {
    ((text.split_whitespace().count() as f64) * 1.3).ceil() as usize
}

fn summarize_text(text: &str, max_chars: usize) -> String {
    if text.len() <= max_chars {
        return text.to_string();
    }

    let head = text.chars().take(max_chars / 2).collect::<String>();
    let tail = text
        .chars()
        .rev()
        .take(max_chars / 2)
        .collect::<String>()
        .chars()
        .rev()
        .collect::<String>();

    format!("{head}\n... [summary-trimmed] ...\n{tail}")
}

fn section(name: &str, values: &[String]) -> String {
    if values.is_empty() {
        return format!("[{name}]\n<empty>");
    }
    format!("[{name}]\n{}", values.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn optimize_respects_budget_and_dedup() {
        let manager = ContextManager::new(4000);

        let chunks = vec![
            ContextChunk {
                id: "1".to_string(),
                scope: ContextScope::GlobalShared,
                kind: ContextKind::System,
                content: "system rules".to_string(),
                priority: 1.0,
            },
            ContextChunk {
                id: "2".to_string(),
                scope: ContextScope::SessionShared,
                kind: ContextKind::History,
                content: "same line".to_string(),
                priority: 0.7,
            },
            ContextChunk {
                id: "3".to_string(),
                scope: ContextScope::SessionShared,
                kind: ContextKind::History,
                content: "same line".to_string(),
                priority: 0.6,
            },
        ];

        let optimized = manager.optimize(chunks);
        assert_eq!(optimized.system.len(), 1);
        assert_eq!(optimized.history.len(), 1);
        assert!(optimized.token_estimate > 0);
    }
}
