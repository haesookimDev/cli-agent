//! Auto-routing of user tasks to predefined skill workflows.
//!
//! Given a task string (and optionally a repo URL), this module decides
//! whether the orchestrator should skip dynamic graph building and instead
//! execute one of the preloaded skill workflows (e.g. git_clone_repo,
//! local_repo_overview). The decision uses keyword matching — see TODO 2-4
//! for known limitations of the current heuristic.

use super::Orchestrator;
use super::helpers::{contains_any_keyword, extract_repo_url_from_text, infer_clone_target_dir};

#[derive(Debug, Clone)]
pub(super) struct AutoSkillRoute {
    pub(super) workflow_id: String,
    pub(super) params: Option<serde_json::Value>,
    pub(super) reason: String,
}

impl Orchestrator {
    pub(super) fn auto_skill_route(
        &self,
        task: &str,
        repo_url: Option<&str>,
    ) -> Option<AutoSkillRoute> {
        if self.skills.is_empty() {
            return None;
        }

        let lower = task.to_lowercase();
        let detected_repo_url = repo_url
            .map(|value| value.to_string())
            .or_else(|| extract_repo_url_from_text(task));

        let explicit_skill_id = self
            .skills
            .iter()
            .map(|entry| entry.value().clone())
            .find(|skill| {
                lower.contains(skill.id.to_lowercase().as_str())
                    || lower.contains(skill.name.to_lowercase().as_str())
            })
            .map(|skill| skill.id);

        if let Some(skill_id) = explicit_skill_id {
            let params = match skill_id.as_str() {
                "git_clone_repo" => detected_repo_url.as_ref().map(|url| {
                    serde_json::json!({
                        "repo_url": url,
                        "target_dir": infer_clone_target_dir(url),
                    })
                }),
                "local_repo_overview" => detected_repo_url.as_ref().map(|url| {
                    serde_json::json!({
                        "repo_url": url,
                    })
                }),
                "github_repo_overview" => detected_repo_url
                    .as_ref()
                    .map(|url| serde_json::json!({ "repo_url": url })),
                _ => Some(serde_json::json!({})),
            };

            if let Some(params) = params {
                return Some(AutoSkillRoute {
                    workflow_id: skill_id,
                    params: if params == serde_json::json!({}) {
                        None
                    } else {
                        Some(params)
                    },
                    reason: "explicit_skill_match".to_string(),
                });
            }
        }

        if self.skills.contains_key("git_clone_repo")
            && detected_repo_url.is_some()
            && contains_any_keyword(
                lower.as_str(),
                &[
                    "clone",
                    "clon",
                    "fetch",
                    "checkout",
                    "복제",
                    "클론",
                    "가져와",
                ],
            )
        {
            let url = detected_repo_url?;
            let target_dir = infer_clone_target_dir(url.as_str());
            return Some(AutoSkillRoute {
                workflow_id: "git_clone_repo".to_string(),
                params: Some(serde_json::json!({
                    "repo_url": url,
                    "target_dir": target_dir,
                })),
                reason: "clone_skill_auto_route".to_string(),
            });
        }

        let wants_remote_only = contains_any_keyword(
            lower.as_str(),
            &[
                "remote only",
                "without cloning",
                "without clone",
                "no clone",
                "clone 없이",
                "원격으로만",
                "원격만",
                "readme only",
                "metadata",
                "issues",
                "issue",
                "pull request",
                "pr ",
                "commits",
                "commit history",
                "release notes",
            ],
        );

        if self.skills.contains_key("local_repo_overview")
            && detected_repo_url.is_some()
            && !wants_remote_only
            && !contains_any_keyword(
                lower.as_str(),
                &[
                    "clone", "checkout", "patch", "fix", "modify", "edit", "수정", "클론",
                ],
            )
            && contains_any_keyword(
                lower.as_str(),
                &[
                    "analyze",
                    "analysis",
                    "overview",
                    "summary",
                    "summarize",
                    "architecture",
                    "structure",
                    "stack",
                    "repo",
                    "repository",
                    "프로젝트",
                    "리포지토리",
                    "분석",
                    "요약",
                    "개요",
                    "구조",
                ],
            )
        {
            let url = detected_repo_url?;
            return Some(AutoSkillRoute {
                workflow_id: "local_repo_overview".to_string(),
                params: Some(serde_json::json!({ "repo_url": url })),
                reason: "local_repo_overview_auto_route".to_string(),
            });
        }

        if self.skills.contains_key("github_repo_overview")
            && detected_repo_url.is_some()
            && wants_remote_only
        {
            let url = detected_repo_url?;
            return Some(AutoSkillRoute {
                workflow_id: "github_repo_overview".to_string(),
                params: Some(serde_json::json!({ "repo_url": url })),
                reason: "github_repo_overview_auto_route".to_string(),
            });
        }

        None
    }
}
