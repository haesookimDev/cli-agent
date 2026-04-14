//! GitHub pull request operations.

use serde_json::json;

use super::{GitHubOps, GitHubPR, MergeMethod, ReviewComment, ReviewEvent};
use crate::types::AgentPersona;

impl GitHubOps {
    /// Create a new pull request.
    pub async fn create_pr(
        &self,
        persona: &AgentPersona,
        title: &str,
        body: &str,
        head: &str,
        base: &str,
    ) -> anyhow::Result<GitHubPR> {
        let signed_body = self.sign_body(persona, body);

        let result = self
            .call_tool(
                "github/create_pull_request",
                json!({
                    "owner": self.repo_owner,
                    "repo": self.repo_name,
                    "title": title,
                    "body": signed_body,
                    "head": head,
                    "base": base,
                }),
            )
            .await?;

        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap_or_else(|_| {
            json!({ "number": 0, "title": title, "html_url": "", "state": "open" })
        });

        Ok(GitHubPR {
            number: parsed["number"].as_i64().unwrap_or(0),
            title: parsed["title"].as_str().unwrap_or(title).to_string(),
            html_url: parsed["html_url"].as_str().unwrap_or("").to_string(),
            state: parsed["state"].as_str().unwrap_or("open").to_string(),
        })
    }

    /// Submit a review on a pull request.
    pub async fn review_pr(
        &self,
        persona: &AgentPersona,
        pr_number: i64,
        event: ReviewEvent,
        body: &str,
        comments: Vec<ReviewComment>,
    ) -> anyhow::Result<()> {
        let signed_body = self.sign_body(persona, body);

        let mut args = json!({
            "owner": self.repo_owner,
            "repo": self.repo_name,
            "pull_number": pr_number,
            "event": event.to_string(),
            "body": signed_body,
        });

        if !comments.is_empty() {
            let comment_array: Vec<serde_json::Value> = comments
                .iter()
                .map(|c| {
                    let mut obj = json!({
                        "path": c.path,
                        "body": c.body,
                    });
                    if let Some(line) = c.line {
                        obj["line"] = json!(line);
                    }
                    obj
                })
                .collect();
            args["comments"] = json!(comment_array);
        }

        self.call_tool("github/create_pull_request_review", args)
            .await?;

        Ok(())
    }

    /// Add a comment on a pull request (not a review, just a plain comment).
    pub async fn comment_on_pr(
        &self,
        persona: &AgentPersona,
        pr_number: i64,
        body: &str,
    ) -> anyhow::Result<()> {
        let signed_body = self.sign_body(persona, body);

        // PR comments use the issues comment endpoint on GitHub.
        self.call_tool(
            "github/add_issue_comment",
            json!({
                "owner": self.repo_owner,
                "repo": self.repo_name,
                "issue_number": pr_number,
                "body": signed_body,
            }),
        )
        .await?;

        Ok(())
    }

    /// Merge a pull request.
    pub async fn merge_pr(
        &self,
        pr_number: i64,
        method: MergeMethod,
    ) -> anyhow::Result<()> {
        self.call_tool(
            "github/merge_pull_request",
            json!({
                "owner": self.repo_owner,
                "repo": self.repo_name,
                "pull_number": pr_number,
                "merge_method": method.to_string(),
            }),
        )
        .await?;

        Ok(())
    }

    /// List pull requests with optional state filter.
    pub async fn list_prs(
        &self,
        state: &str,
    ) -> anyhow::Result<Vec<GitHubPR>> {
        let result = self
            .call_tool(
                "github/list_pull_requests",
                json!({
                    "owner": self.repo_owner,
                    "repo": self.repo_name,
                    "state": state,
                }),
            )
            .await?;

        let parsed: Vec<serde_json::Value> =
            serde_json::from_str(&result).unwrap_or_default();

        Ok(parsed
            .into_iter()
            .map(|v| GitHubPR {
                number: v["number"].as_i64().unwrap_or(0),
                title: v["title"].as_str().unwrap_or("").to_string(),
                html_url: v["html_url"].as_str().unwrap_or("").to_string(),
                state: v["state"].as_str().unwrap_or("open").to_string(),
            })
            .collect())
    }
}
