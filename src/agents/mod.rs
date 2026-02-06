use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;

use crate::context::OptimizedContext;
use crate::router::{ModelRouter, RoutingConstraints};
use crate::types::{AgentRole, StructuredBrief, TaskProfile};

#[derive(Debug, Clone)]
pub struct AgentInput {
    pub task: String,
    pub instructions: String,
    pub context: OptimizedContext,
    pub dependency_outputs: Vec<String>,
    pub brief: StructuredBrief,
}

#[derive(Debug, Clone)]
pub struct AgentOutput {
    pub model: String,
    pub content: String,
}

#[async_trait]
pub trait SubAgent: Send + Sync {
    fn role(&self) -> AgentRole;
    async fn run(&self, input: AgentInput, router: Arc<ModelRouter>)
    -> anyhow::Result<AgentOutput>;
}

#[derive(Clone)]
pub struct AgentRegistry {
    agents: Arc<HashMap<AgentRole, Arc<dyn SubAgent>>>,
}

impl AgentRegistry {
    pub fn builtin() -> Self {
        let mut map: HashMap<AgentRole, Arc<dyn SubAgent>> = HashMap::new();
        map.insert(
            AgentRole::Planner,
            Arc::new(BuiltinAgent::new(AgentRole::Planner)),
        );
        map.insert(
            AgentRole::Extractor,
            Arc::new(BuiltinAgent::new(AgentRole::Extractor)),
        );
        map.insert(
            AgentRole::Coder,
            Arc::new(BuiltinAgent::new(AgentRole::Coder)),
        );
        map.insert(
            AgentRole::Summarizer,
            Arc::new(BuiltinAgent::new(AgentRole::Summarizer)),
        );
        map.insert(
            AgentRole::Fallback,
            Arc::new(BuiltinAgent::new(AgentRole::Fallback)),
        );

        Self {
            agents: Arc::new(map),
        }
    }

    pub async fn run_role(
        &self,
        role: AgentRole,
        input: AgentInput,
        router: Arc<ModelRouter>,
    ) -> anyhow::Result<AgentOutput> {
        let agent = self
            .agents
            .get(&role)
            .ok_or_else(|| anyhow::anyhow!("agent role {} not found", role))?
            .clone();
        agent.run(input, router).await
    }
}

#[derive(Debug)]
struct BuiltinAgent {
    role: AgentRole,
}

impl BuiltinAgent {
    fn new(role: AgentRole) -> Self {
        Self { role }
    }

    fn profile(&self) -> TaskProfile {
        match self.role {
            AgentRole::Planner => TaskProfile::Planning,
            AgentRole::Extractor => TaskProfile::Extraction,
            AgentRole::Coder => TaskProfile::Coding,
            AgentRole::Summarizer | AgentRole::Fallback => TaskProfile::General,
        }
    }

    fn role_prompt(&self) -> &'static str {
        match self.role {
            AgentRole::Planner => {
                "You are the planning agent. Build execution strategy, constraints, and dependency-safe steps."
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
        }
    }
}

#[async_trait]
impl SubAgent for BuiltinAgent {
    fn role(&self) -> AgentRole {
        self.role
    }

    async fn run(
        &self,
        input: AgentInput,
        router: Arc<ModelRouter>,
    ) -> anyhow::Result<AgentOutput> {
        let prompt = format!(
            "{}\n\nTASK:\n{}\n\nINSTRUCTIONS:\n{}\n\nSTRUCTURED BRIEF:\n{}\n\nDEPENDENCY OUTPUTS:\n{}\n\nCONTEXT:\n{}",
            self.role_prompt(),
            input.task,
            input.instructions,
            serde_json::to_string_pretty(&input.brief)?,
            input.dependency_outputs.join("\n---\n"),
            input.context.flatten(),
        );

        let constraints = RoutingConstraints::for_profile(self.profile());
        let (_decision, inference) = router
            .infer(self.profile(), prompt.as_str(), &constraints)
            .await?;

        Ok(AgentOutput {
            model: format!("{}:{}", inference.provider, inference.model_id),
            content: inference.output,
        })
    }
}
