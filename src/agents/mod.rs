pub mod agent_loader;

use std::collections::HashMap;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;

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

#[derive(Debug, Clone)]
pub struct AgentOutput {
    pub model: String,
    pub content: String,
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
    agents: Arc<HashMap<AgentRole, Arc<dyn SubAgent>>>,
}

impl AgentRegistry {
    pub fn builtin() -> Self {
        let mut map: HashMap<AgentRole, Arc<dyn SubAgent>> = HashMap::new();
        for &role in AgentRole::all() {
            map.insert(role, Arc::new(BuiltinAgent::new(role)));
        }
        Self {
            agents: Arc::new(map),
        }
    }

    /// Load agent definitions from YAML files in `dir`, falling back to builtin
    /// defaults for any role not covered by a YAML file.
    pub async fn from_dir_with_fallback(dir: &Path) -> Self {
        let definitions = load_agents_from_dir(dir).await;

        let mut map: HashMap<AgentRole, Arc<dyn SubAgent>> = HashMap::new();

        // Insert YAML-defined agents first
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

        // Fill in any missing roles with hardcoded defaults
        for &role in AgentRole::all() {
            map.entry(role).or_insert_with(|| Arc::new(BuiltinAgent::new(role)));
        }

        Self {
            agents: Arc::new(map),
        }
    }

    pub async fn run_role(
        &self,
        role: AgentRole,
        input: AgentInput,
        router: Arc<ModelRouter>,
        cli_output: Option<CliOutputCallback>,
    ) -> anyhow::Result<AgentOutput> {
        let agent = self
            .agents
            .get(&role)
            .ok_or_else(|| anyhow::anyhow!("agent role {} not found", role))?
            .clone();
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
        let agent = self
            .agents
            .get(&role)
            .ok_or_else(|| anyhow::anyhow!("agent role {} not found", role))?
            .clone();

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
