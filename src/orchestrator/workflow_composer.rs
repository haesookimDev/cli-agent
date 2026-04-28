//! Compose multiple skill / workflow templates into a single composite
//! workflow (TODO 7-2, first slice).
//!
//! This is intentionally the *small* version of the Workflow Composer:
//! `chain_skills(skill_ids, params)` produces a brand-new WorkflowTemplate
//! whose nodes are the union of the inputs, with each later skill's roots
//! depending on the previous skill's terminal nodes so they execute in
//! sequence. Node IDs are namespaced (`{skill_id}__{node_id}`) so two
//! skills with the same internal id don't collide.
//!
//! The future `compose(analysis)` and `generate_from_description(...)`
//! entry points (TODO 7-2 follow-ups) can build on top of the helpers
//! here. For now `chain_skills` is enough to let users assemble
//! existing skills without writing YAML by hand.

use std::collections::{HashMap, HashSet};

use chrono::Utc;
use uuid::Uuid;

use crate::orchestrator::skill_loader::interpolate_params;
use crate::types::{
    SkillSource, WorkflowGraphTemplate, WorkflowNodeTemplate, WorkflowParameter,
    WorkflowTemplate,
};

/// Result of `chain_skills`.
#[derive(Debug)]
pub struct ChainResult {
    pub template: WorkflowTemplate,
    /// Union of parameters declared by the input skills, deduped by name.
    pub merged_parameters: Vec<WorkflowParameter>,
}

/// Compose `skills` (already in the order they should execute) into a single
/// WorkflowTemplate. Returns Err if `skills` is empty or if a node id
/// collision survives namespacing (which would indicate caller-supplied
/// duplicates within a single skill).
pub fn chain_skills(
    skills: &[WorkflowTemplate],
    params: &HashMap<String, String>,
    name: impl Into<String>,
    description: impl Into<String>,
) -> anyhow::Result<ChainResult> {
    if skills.is_empty() {
        return Err(anyhow::anyhow!("cannot chain an empty list of skills"));
    }

    let mut nodes: Vec<WorkflowNodeTemplate> = Vec::new();
    let mut last_terminals: Vec<String> = Vec::new();
    let mut seen_ids: HashSet<String> = HashSet::new();
    let mut merged_parameters: Vec<WorkflowParameter> = Vec::new();
    let mut seen_param_names: HashSet<String> = HashSet::new();

    for skill in skills {
        let interpolated = interpolate_params(skill, params);

        // Build the namespaced node list and an id-rewrite map for this skill.
        let prefix = format!("{}__", skill.id);
        let mut local_to_namespaced: HashMap<String, String> = HashMap::new();
        for n in &interpolated.graph_template.nodes {
            let new_id = format!("{prefix}{}", n.id);
            local_to_namespaced.insert(n.id.clone(), new_id);
        }

        let mut current_nodes: Vec<WorkflowNodeTemplate> = Vec::new();
        for n in &interpolated.graph_template.nodes {
            let new_id = local_to_namespaced.get(&n.id).cloned().unwrap();
            if !seen_ids.insert(new_id.clone()) {
                return Err(anyhow::anyhow!(
                    "duplicate node id `{new_id}` after namespacing"
                ));
            }
            // Rewrite intra-skill dependencies; cross-skill anchoring
            // is added below.
            let mut deps: Vec<String> = n
                .dependencies
                .iter()
                .map(|d| {
                    local_to_namespaced
                        .get(d)
                        .cloned()
                        .unwrap_or_else(|| d.clone())
                })
                .collect();
            // For roots of this skill (no internal deps), depend on the
            // previous skill's terminal nodes so the chain serializes.
            if n.dependencies.is_empty() && !last_terminals.is_empty() {
                deps = last_terminals.clone();
            }
            current_nodes.push(WorkflowNodeTemplate {
                id: new_id,
                role: n.role,
                instructions: n.instructions.clone(),
                dependencies: deps,
                mcp_tools: n.mcp_tools.clone(),
                git_commands: n.git_commands.clone(),
                policy: n.policy.clone(),
            });
        }

        // The terminals of *this* skill are nodes nobody else depends on.
        let dependents: HashSet<&str> = current_nodes
            .iter()
            .flat_map(|n| n.dependencies.iter().map(|s| s.as_str()))
            .collect();
        last_terminals = current_nodes
            .iter()
            .filter(|n| !dependents.contains(n.id.as_str()))
            .map(|n| n.id.clone())
            .collect();

        nodes.extend(current_nodes);

        for p in &interpolated.parameters {
            if seen_param_names.insert(p.name.clone()) {
                merged_parameters.push(p.clone());
            }
        }
    }

    let now = Utc::now();
    let template = WorkflowTemplate {
        id: Uuid::new_v4().to_string(),
        name: name.into(),
        description: description.into(),
        created_at: now,
        updated_at: now,
        source_run_id: None,
        graph_template: WorkflowGraphTemplate { nodes },
        parameters: merged_parameters.clone(),
        source: SkillSource::Api,
    };
    Ok(ChainResult {
        template,
        merged_parameters,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::AgentRole;

    fn skill(
        id: &str,
        nodes: Vec<(&str, AgentRole, Vec<&str>)>,
        params: Vec<&str>,
    ) -> WorkflowTemplate {
        let now = Utc::now();
        WorkflowTemplate {
            id: id.to_string(),
            name: id.to_string(),
            description: String::new(),
            created_at: now,
            updated_at: now,
            source_run_id: None,
            graph_template: WorkflowGraphTemplate {
                nodes: nodes
                    .into_iter()
                    .map(|(nid, role, deps)| WorkflowNodeTemplate {
                        id: nid.to_string(),
                        role,
                        instructions: format!("instructions for {nid}"),
                        dependencies: deps.into_iter().map(String::from).collect(),
                        mcp_tools: vec![],
                        git_commands: vec![],
                        policy: serde_json::json!({}),
                    })
                    .collect(),
            },
            parameters: params
                .into_iter()
                .map(|p| WorkflowParameter {
                    name: p.to_string(),
                    description: String::new(),
                    default_value: None,
                })
                .collect(),
            source: SkillSource::File,
        }
    }

    #[test]
    fn chain_skills_namespaces_nodes_and_links_chain() {
        let a = skill(
            "scan",
            vec![
                ("plan", AgentRole::Planner, vec![]),
                ("analyze", AgentRole::Analyzer, vec!["plan"]),
            ],
            vec![],
        );
        let b = skill(
            "report",
            vec![("write", AgentRole::Summarizer, vec![])],
            vec![],
        );
        let result = chain_skills(
            &[a, b],
            &HashMap::new(),
            "scan_then_report",
            "demo chain",
        )
        .expect("chain ok");
        let nodes = &result.template.graph_template.nodes;
        let ids: Vec<&str> = nodes.iter().map(|n| n.id.as_str()).collect();
        assert!(ids.contains(&"scan__plan"));
        assert!(ids.contains(&"scan__analyze"));
        assert!(ids.contains(&"report__write"));

        // scan__plan → no deps (root of first skill)
        let plan = nodes.iter().find(|n| n.id == "scan__plan").unwrap();
        assert!(plan.dependencies.is_empty());

        // scan__analyze still depends on scan__plan via local rewrite
        let analyze = nodes.iter().find(|n| n.id == "scan__analyze").unwrap();
        assert_eq!(analyze.dependencies, vec!["scan__plan".to_string()]);

        // report__write should chain on top of scan's terminal (scan__analyze)
        let write = nodes.iter().find(|n| n.id == "report__write").unwrap();
        assert_eq!(write.dependencies, vec!["scan__analyze".to_string()]);
    }

    #[test]
    fn chain_skills_dedupes_parameters_by_name() {
        let a = skill("a", vec![("p", AgentRole::Planner, vec![])], vec!["repo_url"]);
        let b = skill(
            "b",
            vec![("p", AgentRole::Summarizer, vec![])],
            vec!["repo_url"],
        );
        let result =
            chain_skills(&[a, b], &HashMap::new(), "merged", "").expect("chain ok");
        assert_eq!(result.merged_parameters.len(), 1);
    }

    #[test]
    fn chain_skills_empty_input_errors() {
        let err = chain_skills(&[], &HashMap::new(), "x", "")
            .err()
            .expect("must error");
        assert!(err.to_string().contains("empty"));
    }

    #[test]
    fn chain_skills_terminal_set_handles_diamonds() {
        // a: plan → (analyze, summarize)
        // expect terminals = {analyze, summarize}
        let a = skill(
            "scan",
            vec![
                ("plan", AgentRole::Planner, vec![]),
                ("analyze", AgentRole::Analyzer, vec!["plan"]),
                ("summarize", AgentRole::Summarizer, vec!["plan"]),
            ],
            vec![],
        );
        let b = skill(
            "report",
            vec![("write", AgentRole::Summarizer, vec![])],
            vec![],
        );
        let result =
            chain_skills(&[a, b], &HashMap::new(), "diamond", "").expect("chain ok");
        let write = result
            .template
            .graph_template
            .nodes
            .iter()
            .find(|n| n.id == "report__write")
            .unwrap();
        let mut deps = write.dependencies.clone();
        deps.sort();
        assert_eq!(deps, vec!["scan__analyze", "scan__summarize"]);
    }
}
