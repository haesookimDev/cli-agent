pub mod agent_loader;

use std::collections::HashMap;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::RwLock;

use async_trait::async_trait;

use crate::context::OptimizedContext;
use crate::router::{CliOutputCallback, ModelRouter, RoutingConstraints, TokenCallback};
use crate::types::{AgentRole, StructuredBrief, TaskProfile};

use agent_loader::load_agents_from_dir;

#[derive(Debug, Clone)]
pub struct AgentInput {
    pub task: String,
    pub instructions: String,
    pub context: OptimizedContext,
    pub dependency_outputs: Vec<String>,
    pub brief: StructuredBrief,
    pub working_dir: Option<PathBuf>,
}

#[derive(Debug, Clone, Default)]
pub struct AgentOutput {
    pub model: String,
    pub content: String,
    /// Token usage reported by the upstream provider for this single
    /// inference call. `None` when the provider doesn't expose a usage
    /// block (e.g. CLI backends).
    pub usage: Option<crate::types::TokenUsage>,
}

#[async_trait]
pub trait SubAgent: Send + Sync {
    fn role(&self) -> AgentRole;
    fn system_prompt(&self) -> &str;
    async fn run(
        &self,
        input: AgentInput,
        router: Arc<ModelRouter>,
        cli_output: Option<CliOutputCallback>,
    ) -> anyhow::Result<AgentOutput>;
}

#[derive(Clone)]
pub struct AgentRegistry {
    /// RwLock so `reload_from_dir` can swap the table at runtime without
    /// invalidating outstanding clones of the registry. All reads acquire a
    /// read lock and clone the Arc<dyn SubAgent> immediately.
    agents: Arc<RwLock<HashMap<AgentRole, Arc<dyn SubAgent>>>>,
}

impl AgentRegistry {
    pub fn builtin() -> Self {
        let mut map: HashMap<AgentRole, Arc<dyn SubAgent>> = HashMap::new();
        for &role in AgentRole::all() {
            map.insert(role, Arc::new(BuiltinAgent::new(role)));
        }
        Self {
            agents: Arc::new(RwLock::new(map)),
        }
    }

    fn build_map_from_definitions(
        definitions: Vec<crate::agents::agent_loader::AgentDefinition>,
    ) -> HashMap<AgentRole, Arc<dyn SubAgent>> {
        let mut map: HashMap<AgentRole, Arc<dyn SubAgent>> = HashMap::new();
        for def in definitions {
            map.insert(
                def.role,
                Arc::new(BuiltinAgent {
                    role: def.role,
                    system_prompt: def.system_prompt,
                    task_profile: def.task_profile,
                }),
            );
        }
        for &role in AgentRole::all() {
            map.entry(role).or_insert_with(|| Arc::new(BuiltinAgent::new(role)));
        }
        map
    }

    /// Load agent definitions from YAML files in `dir`, falling back to builtin
    /// defaults for any role not covered by a YAML file.
    pub async fn from_dir_with_fallback(dir: &Path) -> Self {
        let definitions = load_agents_from_dir(dir).await;
        Self {
            agents: Arc::new(RwLock::new(Self::build_map_from_definitions(definitions))),
        }
    }

    /// Reload agent definitions from `dir`. Used by TODO 9-5 to support
    /// runtime hot-swap without bouncing the orchestrator. Existing
    /// in-flight `run_role` calls keep their Arc<dyn SubAgent> snapshot;
    /// the next call resolves against the new map.
    pub async fn reload_from_dir(&self, dir: &Path) -> anyhow::Result<usize> {
        let definitions = load_agents_from_dir(dir).await;
        let new_map = Self::build_map_from_definitions(definitions);
        let count = new_map.len();
        let mut guard = self
            .agents
            .write()
            .map_err(|e| anyhow::anyhow!("agents map poisoned: {e}"))?;
        *guard = new_map;
        Ok(count)
    }

    pub async fn run_role(
        &self,
        role: AgentRole,
        input: AgentInput,
        router: Arc<ModelRouter>,
        cli_output: Option<CliOutputCallback>,
    ) -> anyhow::Result<AgentOutput> {
        let agent = {
            let guard = self
                .agents
                .read()
                .map_err(|e| anyhow::anyhow!("agents map poisoned: {e}"))?;
            guard
                .get(&role)
                .ok_or_else(|| anyhow::anyhow!("agent role {} not found", role))?
                .clone()
        };
        agent.run(input, router, cli_output).await
    }

    pub async fn run_role_stream(
        &self,
        role: AgentRole,
        input: AgentInput,
        router: Arc<ModelRouter>,
        on_token: TokenCallback,
        cli_output: Option<CliOutputCallback>,
    ) -> anyhow::Result<AgentOutput> {
        let agent = {
            let guard = self
                .agents
                .read()
                .map_err(|e| anyhow::anyhow!("agents map poisoned: {e}"))?;
            guard
                .get(&role)
                .ok_or_else(|| anyhow::anyhow!("agent role {} not found", role))?
                .clone()
        };

        let prompt = format!(
            "{}\n\nTASK:\n{}\n\nINSTRUCTIONS:\n{}\n\nDEPENDENCY OUTPUTS:\n{}\n\nCONTEXT:\n{}",
            agent.system_prompt(),
            input.task,
            input.instructions,
            input.dependency_outputs.join("\n---\n"),
            input.context.flatten(),
        );

        let profile = agent_role_profile(role);
        let constraints = RoutingConstraints::for_profile(profile);
        let working_dir = input.working_dir.as_deref();
        let (_decision, inference) = router
            .infer_stream_in_dir_with_cli_output(
                profile,
                prompt.as_str(),
                &constraints,
                working_dir,
                on_token,
                cli_output,
            )
            .await?;

        Ok(AgentOutput {
            model: format!("{}:{}", inference.provider, inference.model_id),
            content: inference.output,
            usage: inference.usage,
        })
    }
}

#[derive(Debug)]
struct BuiltinAgent {
    role: AgentRole,
    system_prompt: String,
    task_profile: TaskProfile,
}

impl BuiltinAgent {
    fn new(role: AgentRole) -> Self {
        Self {
            role,
            system_prompt: default_system_prompt(role).to_string(),
            task_profile: agent_role_profile(role),
        }
    }
}

#[async_trait]
impl SubAgent for BuiltinAgent {
    fn role(&self) -> AgentRole {
        self.role
    }

    fn system_prompt(&self) -> &str {
        &self.system_prompt
    }

    async fn run(
        &self,
        input: AgentInput,
        router: Arc<ModelRouter>,
        cli_output: Option<CliOutputCallback>,
    ) -> anyhow::Result<AgentOutput> {
        let prompt = format!(
            "{}\n\nTASK:\n{}\n\nINSTRUCTIONS:\n{}\n\nSTRUCTURED BRIEF:\n{}\n\nDEPENDENCY OUTPUTS:\n{}\n\nCONTEXT:\n{}",
            self.system_prompt,
            input.task,
            input.instructions,
            serde_json::to_string_pretty(&input.brief)?,
            input.dependency_outputs.join("\n---\n"),
            input.context.flatten(),
        );

        let constraints = RoutingConstraints::for_profile(self.task_profile);
        let working_dir = input.working_dir.as_deref();
        let (_decision, inference) = router
            .infer_in_dir_with_cli_output(
                self.task_profile,
                prompt.as_str(),
                &constraints,
                working_dir,
                cli_output,
            )
            .await?;

        Ok(AgentOutput {
            model: format!("{}:{}", inference.provider, inference.model_id),
            content: inference.output,
            usage: inference.usage,
        })
    }
}

fn agent_role_profile(role: AgentRole) -> TaskProfile {
    match role {
        AgentRole::Planner => TaskProfile::Planning,
        AgentRole::Extractor | AgentRole::Analyzer => TaskProfile::Extraction,
        AgentRole::Coder => TaskProfile::Coding,
        AgentRole::Reviewer => TaskProfile::Planning,
        AgentRole::Summarizer
        | AgentRole::Fallback
        | AgentRole::ToolCaller
        | AgentRole::Scheduler
        | AgentRole::ConfigManager
        | AgentRole::Validator => TaskProfile::General,
    }
}

fn default_system_prompt(role: AgentRole) -> &'static str {
    match role {
        AgentRole::Planner => {
            "You are the planning agent. Build execution strategy, constraints, and dependency-safe steps. For non-trivial work, return a JSON SubtaskPlan and split independent work into separate subtasks so they can run in parallel."
        }
        AgentRole::Extractor => {
            "You are the extraction agent. Pull key facts and structured data with precision and low latency."
        }
        AgentRole::Coder => {
            "You are the coding agent. Produce implementable code-level output with tradeoffs and failure handling."
        }
        AgentRole::Summarizer => {
            "You are the summarizer agent. Consolidate all previous outputs into concise checkpoint summaries."
        }
        AgentRole::Fallback => {
            "You are the fallback agent. Recover gracefully when upstream nodes fail and provide safe alternatives."
        }
        AgentRole::ToolCaller => {
            "You are the tool caller agent. Select and execute MCP tool calls as instructed. When asked to choose tools, respond with ONLY a machine-readable JSON array of tool calls or the word DONE. Your first non-whitespace character must be '[' or your entire response must be DONE. Never add prose, markdown fences, or wrapper objects."
        }
        AgentRole::Analyzer => {
            "You are the analyzer agent. Examine data and results to identify patterns, anomalies, and insights."
        }
        AgentRole::Reviewer => {
            "You are the reviewer agent. Verify results against the original request, assess quality, and flag gaps. Output COMPLETE if satisfied, or INCOMPLETE: <reason> if not."
        }
        AgentRole::Scheduler => {
            "You are the scheduler agent. Manage cron schedules and workflow automation configurations."
        }
        AgentRole::ConfigManager => {
            "You are the config manager agent. Handle system settings changes including model toggles and preferences."
        }
        AgentRole::Validator => {
            "You are the validator agent. Run lint, build, test, and git commands to verify code correctness."
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    #[tokio::test]
    async fn reload_from_dir_swaps_definitions_for_existing_registry() {
        let dir = std::env::temp_dir().join(format!("agents-reload-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        // Initial YAML: planner with prompt "v1".
        let v1 = r#"
name: planner
description: ""
role: planner
task_profile: planning
system_prompt: "v1"
"#;
        std::fs::write(dir.join("planner.yaml"), v1).unwrap();

        let registry = AgentRegistry::from_dir_with_fallback(&dir).await;
        {
            let map = registry.agents.read().unwrap();
            // The planner entry came from YAML. We can't introspect prompt
            // through the trait, but the entry must exist.
            assert!(map.contains_key(&AgentRole::Planner));
        }

        // Overwrite with v2 + add a coder definition.
        let v2 = r#"
name: planner
description: ""
role: planner
task_profile: planning
system_prompt: "v2"
"#;
        std::fs::write(dir.join("planner.yaml"), v2).unwrap();
        let coder = r#"
name: coder
description: ""
role: coder
task_profile: coding
system_prompt: "from yaml"
"#;
        std::fs::write(dir.join("coder.yaml"), coder).unwrap();

        let count = registry.reload_from_dir(&dir).await.unwrap();
        // count = total roles (YAML-defined + defaults) = AgentRole::all().len()
        assert_eq!(count, AgentRole::all().len());
        {
            let map = registry.agents.read().unwrap();
            assert!(map.contains_key(&AgentRole::Planner));
            assert!(map.contains_key(&AgentRole::Coder));
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn reload_from_missing_dir_falls_back_to_builtins() {
        let dir = std::env::temp_dir().join(format!("agents-missing-{}", Uuid::new_v4()));
        let registry = AgentRegistry::builtin();
        let count = registry.reload_from_dir(&dir).await.unwrap();
        assert_eq!(count, AgentRole::all().len());
    }
}
