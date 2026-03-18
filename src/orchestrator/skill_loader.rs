use std::collections::HashMap;
use std::path::Path;

use chrono::Utc;
use tracing::{info, warn};

use crate::types::{
    AgentRole, McpToolDefinition, SkillSource, WorkflowGraphTemplate, WorkflowNodeTemplate,
    WorkflowParameter, WorkflowTemplate,
};

/// YAML/JSON skill file schema.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct SkillDefinition {
    pub name: String,
    pub description: String,
    #[serde(default)]
    pub parameters: Vec<WorkflowParameter>,
    pub nodes: Vec<SkillNodeDefinition>,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct SkillNodeDefinition {
    pub id: String,
    pub role: AgentRole,
    pub instructions: String,
    #[serde(default)]
    pub dependencies: Vec<String>,
    #[serde(default)]
    pub mcp_tools: Vec<String>,
    #[serde(default)]
    pub git_commands: Vec<String>,
    #[serde(default = "default_policy")]
    pub policy: serde_json::Value,
}

fn default_policy() -> serde_json::Value {
    serde_json::json!({})
}

/// Load all `*.yaml` and `*.json` skill files from a directory.
pub async fn load_skills_from_dir(dir: &Path) -> Vec<WorkflowTemplate> {
    let mut skills = Vec::new();

    let mut entries = match tokio::fs::read_dir(dir).await {
        Ok(e) => e,
        Err(e) => {
            warn!("Failed to read skills directory {}: {e}", dir.display());
            return skills;
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
                warn!("Failed to read skill file {}: {e}", path.display());
                continue;
            }
        };

        let def: SkillDefinition = if ext == "json" {
            match serde_json::from_str(&content) {
                Ok(d) => d,
                Err(e) => {
                    warn!("Failed to parse skill JSON {}: {e}", path.display());
                    continue;
                }
            }
        } else {
            match serde_yaml::from_str(&content) {
                Ok(d) => d,
                Err(e) => {
                    warn!("Failed to parse skill YAML {}: {e}", path.display());
                    continue;
                }
            }
        };

        let skill_id = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown")
            .to_string();

        let now = Utc::now();
        let nodes: Vec<WorkflowNodeTemplate> = def
            .nodes
            .into_iter()
            .map(|n| WorkflowNodeTemplate {
                id: n.id,
                role: n.role,
                instructions: n.instructions,
                dependencies: n.dependencies,
                mcp_tools: n.mcp_tools,
                git_commands: n.git_commands,
                policy: n.policy,
            })
            .collect();

        let template = WorkflowTemplate {
            id: skill_id.clone(),
            name: def.name,
            description: def.description,
            created_at: now,
            updated_at: now,
            source_run_id: None,
            graph_template: WorkflowGraphTemplate { nodes },
            parameters: def.parameters,
            source: SkillSource::File,
        };

        info!("Loaded skill '{}' from {}", skill_id, path.display());
        skills.push(template);
    }

    skills
}

/// Replace `{{param_name}}` patterns in all node instructions with parameter values.
pub fn interpolate_params(
    template: &WorkflowTemplate,
    params: &HashMap<String, String>,
) -> WorkflowTemplate {
    let mut result = template.clone();
    for node in &mut result.graph_template.nodes {
        node.instructions =
            interpolate_template_text(&node.instructions, &template.parameters, params);
        node.git_commands = node
            .git_commands
            .iter()
            .map(|command| interpolate_template_text(command, &template.parameters, params))
            .collect();
    }
    result
}

fn interpolate_template_text(
    text: &str,
    parameters: &[WorkflowParameter],
    params: &HashMap<String, String>,
) -> String {
    let mut result = text.to_string();
    for param_def in parameters {
        let placeholder = format!("{{{{{}}}}}", param_def.name);
        if let Some(value) = params.get(&param_def.name) {
            result = result.replace(&placeholder, value);
        } else if let Some(default) = &param_def.default_value {
            result = result.replace(&placeholder, default);
        }
    }
    result
}

/// Filter MCP tools for a node based on its mcp_tools whitelist.
/// If `allowed` is empty, all tools are returned.
pub fn filter_tools_for_node(
    all_tools: &[McpToolDefinition],
    allowed: &[String],
) -> Vec<McpToolDefinition> {
    if allowed.is_empty() {
        return all_tools.to_vec();
    }
    all_tools
        .iter()
        .filter(|t| {
            allowed.iter().any(|a| {
                t.name == *a
                    || t.name.ends_with(&format!("/{a}"))
                    || a.ends_with(&format!("/{}", t.name.rsplit('/').next().unwrap_or("")))
            })
        })
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interpolate_params_replaces_placeholders() {
        let template = WorkflowTemplate {
            id: "test".to_string(),
            name: "Test".to_string(),
            description: "".to_string(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            source_run_id: None,
            graph_template: WorkflowGraphTemplate {
                nodes: vec![WorkflowNodeTemplate {
                    id: "n1".to_string(),
                    role: AgentRole::Planner,
                    instructions: "Review {{file_path}} with {{mode}}".to_string(),
                    dependencies: vec![],
                    mcp_tools: vec![],
                    git_commands: vec![],
                    policy: serde_json::json!({}),
                }],
            },
            parameters: vec![
                WorkflowParameter {
                    name: "file_path".to_string(),
                    description: "".to_string(),
                    default_value: Some(".".to_string()),
                },
                WorkflowParameter {
                    name: "mode".to_string(),
                    description: "".to_string(),
                    default_value: Some("strict".to_string()),
                },
            ],
            source: SkillSource::File,
        };

        let mut params = HashMap::new();
        params.insert("file_path".to_string(), "src/main.rs".to_string());

        let result = interpolate_params(&template, &params);
        assert_eq!(
            result.graph_template.nodes[0].instructions,
            "Review src/main.rs with strict"
        );
        assert!(result.graph_template.nodes[0].git_commands.is_empty());
    }

    #[test]
    fn interpolate_params_replaces_git_command_placeholders() {
        let template = WorkflowTemplate {
            id: "test".to_string(),
            name: "Test".to_string(),
            description: "".to_string(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            source_run_id: None,
            graph_template: WorkflowGraphTemplate {
                nodes: vec![WorkflowNodeTemplate {
                    id: "git".to_string(),
                    role: AgentRole::Validator,
                    instructions: "Clone and inspect {{repo_url}}".to_string(),
                    dependencies: vec![],
                    mcp_tools: vec![],
                    git_commands: vec![
                        "clone \"{{repo_url}}\" \"{{target_dir}}\"".to_string(),
                        "-C \"{{target_dir}}\" status --short".to_string(),
                    ],
                    policy: serde_json::json!({}),
                }],
            },
            parameters: vec![
                WorkflowParameter {
                    name: "repo_url".to_string(),
                    description: "".to_string(),
                    default_value: None,
                },
                WorkflowParameter {
                    name: "target_dir".to_string(),
                    description: "".to_string(),
                    default_value: Some("tmp-repo".to_string()),
                },
            ],
            source: SkillSource::File,
        };

        let mut params = HashMap::new();
        params.insert(
            "repo_url".to_string(),
            "https://example.com/repo.git".to_string(),
        );

        let result = interpolate_params(&template, &params);
        assert_eq!(
            result.graph_template.nodes[0].git_commands,
            vec![
                "clone \"https://example.com/repo.git\" \"tmp-repo\"".to_string(),
                "-C \"tmp-repo\" status --short".to_string(),
            ]
        );
    }

    #[test]
    fn filter_tools_empty_allows_all() {
        let tools = vec![McpToolDefinition {
            name: "server/read_file".to_string(),
            description: "Read a file".to_string(),
            input_schema: serde_json::json!({}),
            server_name: Some("server".to_string()),
        }];
        let filtered = filter_tools_for_node(&tools, &[]);
        assert_eq!(filtered.len(), 1);
    }

    #[test]
    fn filter_tools_whitelist() {
        let tools = vec![
            McpToolDefinition {
                name: "fs/read_file".to_string(),
                description: "".to_string(),
                input_schema: serde_json::json!({}),
                server_name: Some("fs".to_string()),
            },
            McpToolDefinition {
                name: "fs/write_file".to_string(),
                description: "".to_string(),
                input_schema: serde_json::json!({}),
                server_name: Some("fs".to_string()),
            },
        ];
        let filtered = filter_tools_for_node(&tools, &["fs/read_file".to_string()]);
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].name, "fs/read_file");
    }
}
