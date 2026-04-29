use std::path::{Path, PathBuf};

use tracing::{info, warn};

use crate::types::{AgentPersona, AgentRole, TaskProfile};

/// YAML/JSON agent definition file schema.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
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
    /// Optional persona for team-based collaboration.
    #[serde(default)]
    pub persona: Option<AgentPersona>,
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

/// Convert a free-form persona name into a safe YAML filename slug.
/// e.g. "Senior Dev Minho" -> "senior_dev_minho.yaml"
pub fn agent_filename_for_name(name: &str) -> String {
    let mut out = String::with_capacity(name.len() + 5);
    let mut prev_underscore = false;
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            prev_underscore = false;
        } else if !prev_underscore && !out.is_empty() {
            out.push('_');
            prev_underscore = true;
        }
    }
    while out.ends_with('_') {
        out.pop();
    }
    if out.is_empty() {
        out.push_str("agent");
    }
    out.push_str(".yaml");
    out
}

/// Atomically write `def` as YAML to `<dir>/<slug>.yaml` (write to a sibling
/// `.tmp` file then `rename`). Returns the final path written.
pub async fn save_agent_definition(
    dir: &Path,
    def: &AgentDefinition,
) -> anyhow::Result<PathBuf> {
    tokio::fs::create_dir_all(dir).await?;
    let target = dir.join(agent_filename_for_name(&def.name));
    let tmp = target.with_extension("yaml.tmp");
    let yaml = serde_yaml::to_string(def)?;
    tokio::fs::write(&tmp, yaml).await?;
    tokio::fs::rename(&tmp, &target).await?;
    Ok(target)
}

/// Find and delete the YAML file in `dir` whose definition name matches
/// `name`. Returns true when a file was removed.
pub async fn delete_agent_definition_by_name(
    dir: &Path,
    name: &str,
) -> anyhow::Result<bool> {
    let mut entries = match tokio::fs::read_dir(dir).await {
        Ok(e) => e,
        Err(_) => return Ok(false),
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
            Err(_) => continue,
        };
        let parsed: Result<AgentDefinition, _> = if ext == "json" {
            serde_json::from_str(&content).map_err(|e| anyhow::anyhow!(e))
        } else {
            serde_yaml::from_str(&content).map_err(|e| anyhow::anyhow!(e))
        };
        if let Ok(def) = parsed {
            if def.name == name {
                tokio::fs::remove_file(&path).await?;
                return Ok(true);
            }
        }
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    #[test]
    fn slug_strips_special_chars() {
        assert_eq!(agent_filename_for_name("Senior Dev Minho"), "senior_dev_minho.yaml");
        assert_eq!(agent_filename_for_name("QA Lead 김지훈"), "qa_lead.yaml");
        assert_eq!(agent_filename_for_name("---"), "agent.yaml");
    }

    #[tokio::test]
    async fn save_then_load_roundtrip() {
        let dir = std::env::temp_dir().join(format!("agent-save-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let def = AgentDefinition {
            name: "Test Coder".to_string(),
            description: "test".to_string(),
            role: AgentRole::Coder,
            task_profile: TaskProfile::Coding,
            capabilities: vec!["rust".to_string()],
            system_prompt: "You write Rust.".to_string(),
            instructions: String::new(),
            persona: None,
        };
        let path = save_agent_definition(&dir, &def).await.unwrap();
        assert!(path.exists());
        let loaded = load_agents_from_dir(&dir).await;
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].name, "Test Coder");
        assert_eq!(loaded[0].system_prompt, "You write Rust.");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn delete_by_name_removes_matching_file() {
        let dir = std::env::temp_dir().join(format!("agent-delete-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let def = AgentDefinition {
            name: "Deletable".to_string(),
            description: "".to_string(),
            role: AgentRole::Validator,
            task_profile: TaskProfile::General,
            capabilities: vec![],
            system_prompt: "x".to_string(),
            instructions: String::new(),
            persona: None,
        };
        save_agent_definition(&dir, &def).await.unwrap();
        let removed = delete_agent_definition_by_name(&dir, "Deletable").await.unwrap();
        assert!(removed);
        let loaded = load_agents_from_dir(&dir).await;
        assert!(loaded.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
