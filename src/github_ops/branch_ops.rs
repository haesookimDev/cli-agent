//! GitHub branch and file push operations.

use serde_json::json;

use super::{FileChange, GitHubOps};
use crate::types::AgentPersona;

impl GitHubOps {
    /// Create a new branch from an existing ref.
    pub async fn create_branch(&self, name: &str, from: &str) -> anyhow::Result<()> {
        self.call_tool(
            "github/create_branch",
            json!({
                "owner": self.repo_owner,
                "repo": self.repo_name,
                "branch": name,
                "from_branch": from,
            }),
        )
        .await?;

        Ok(())
    }

    /// Push file changes to a branch with a commit message.
    pub async fn push_files(
        &self,
        persona: &AgentPersona,
        branch: &str,
        files: Vec<FileChange>,
        message: &str,
    ) -> anyhow::Result<String> {
        let file_array: Vec<serde_json::Value> = files
            .iter()
            .map(|f| {
                json!({
                    "path": f.path,
                    "content": f.content,
                })
            })
            .collect();

        let commit_message = format!("{}\n\nCo-authored-by: {} <{}>",
            message, persona.display_name, persona.github_username);

        let result = self
            .call_tool(
                "github/push_files",
                json!({
                    "owner": self.repo_owner,
                    "repo": self.repo_name,
                    "branch": branch,
                    "files": file_array,
                    "message": commit_message,
                }),
            )
            .await?;

        // Try to extract the commit SHA from the response.
        let parsed: serde_json::Value =
            serde_json::from_str(&result).unwrap_or_else(|_| json!({}));
        let sha = parsed["sha"]
            .as_str()
            .or_else(|| parsed["commit"]["sha"].as_str())
            .unwrap_or("")
            .to_string();

        Ok(sha)
    }

    /// List branches in the repository.
    pub async fn list_branches(&self) -> anyhow::Result<Vec<String>> {
        let result = self
            .call_tool(
                "github/list_branches",
                json!({
                    "owner": self.repo_owner,
                    "repo": self.repo_name,
                }),
            )
            .await?;

        let parsed: Vec<serde_json::Value> =
            serde_json::from_str(&result).unwrap_or_default();

        Ok(parsed
            .into_iter()
            .filter_map(|v| v["name"].as_str().map(|s| s.to_string()))
            .collect())
    }
}
