use std::path::Path;

use tracing::{info, warn};

use crate::types::{AgentRole, TaskProfile};

/// YAML/JSON agent definition file schema.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct AgentDefinition {
    pub name: String,
    pub description: String,
    pub role: AgentRole,
    pub task_profile: TaskProfile,
    #[serde(default)]
    pub capabilities: Vec<String>,
    pub system_prompt: String,
    #[serde(default)]
    pub instructions: String,
}

/// Load all `*.yaml`, `*.yml`, and `*.json` agent definition files from a directory.
pub async fn load_agents_from_dir(dir: &Path) -> Vec<AgentDefinition> {
    let mut agents = Vec::new();

    let mut entries = match tokio::fs::read_dir(dir).await {
        Ok(e) => e,
        Err(e) => {
            warn!("Failed to read agents directory {}: {e}", dir.display());
            return agents;
        }
    };

    while let Ok(Some(entry)) = entries.next_entry().await {
        let path = entry.path();
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_lowercase();

        if ext != "yaml" && ext != "yml" && ext != "json" {
            continue;
        }

        let content = match tokio::fs::read_to_string(&path).await {
            Ok(c) => c,
            Err(e) => {
                warn!("Failed to read agent file {}: {e}", path.display());
                continue;
            }
        };

        let def: AgentDefinition = if ext == "json" {
            match serde_json::from_str(&content) {
                Ok(d) => d,
                Err(e) => {
                    warn!("Failed to parse agent JSON {}: {e}", path.display());
                    continue;
                }
            }
        } else {
            match serde_yaml::from_str(&content) {
                Ok(d) => d,
                Err(e) => {
                    warn!("Failed to parse agent YAML {}: {e}", path.display());
                    continue;
                }
            }
        };

        info!(
            "Loaded agent '{}' ({}) from {}",
            def.name,
            def.role,
            path.display()
        );
        agents.push(def);
    }

    agents
}
