//! GitHub issue operations.

use serde_json::json;

use super::{GitHubIssue, GitHubOps};
use crate::types::AgentPersona;

impl GitHubOps {
    /// Create a new issue in the repository.
    pub async fn create_issue(
        &self,
        persona: &AgentPersona,
        title: &str,
        body: &str,
        labels: &[String],
        assignees: &[String],
    ) -> anyhow::Result<GitHubIssue> {
        let signed_body = self.sign_body(persona, body);

        let mut args = json!({
            "owner": self.repo_owner,
            "repo": self.repo_name,
            "title": title,
            "body": signed_body,
        });

        if !labels.is_empty() {
            args["labels"] = json!(labels);
        }
        if !assignees.is_empty() {
            args["assignees"] = json!(assignees);
        }

        let result = self.call_tool("github/create_issue", args).await?;
        let parsed: serde_json::Value = serde_json::from_str(&result)
            .unwrap_or_else(|_| json!({ "number": 0, "title": title, "html_url": "", "state": "open" }));

        Ok(GitHubIssue {
            number: parsed["number"].as_i64().unwrap_or(0),
            title: parsed["title"].as_str().unwrap_or(title).to_string(),
            html_url: parsed["html_url"].as_str().unwrap_or("").to_string(),
            state: parsed["state"].as_str().unwrap_or("open").to_string(),
        })
    }

    /// Add a comment to an existing issue.
    pub async fn comment_on_issue(
        &self,
        persona: &AgentPersona,
        issue_number: i64,
        body: &str,
    ) -> anyhow::Result<()> {
        let signed_body = self.sign_body(persona, body);

        self.call_tool(
            "github/add_issue_comment",
            json!({
                "owner": self.repo_owner,
                "repo": self.repo_name,
                "issue_number": issue_number,
                "body": signed_body,
            }),
        )
        .await?;

        Ok(())
    }

    /// Close an issue with an optional closing comment.
    pub async fn close_issue(
        &self,
        persona: &AgentPersona,
        issue_number: i64,
        comment: &str,
    ) -> anyhow::Result<()> {
        if !comment.is_empty() {
            self.comment_on_issue(persona, issue_number, comment)
                .await?;
        }

        self.call_tool(
            "github/update_issue",
            json!({
                "owner": self.repo_owner,
                "repo": self.repo_name,
                "issue_number": issue_number,
                "state": "closed",
            }),
        )
        .await?;

        Ok(())
    }

    /// List issues with optional state and label filters.
    pub async fn list_issues(
        &self,
        state: &str,
        labels: &[String],
    ) -> anyhow::Result<Vec<GitHubIssue>> {
        let mut args = json!({
            "owner": self.repo_owner,
            "repo": self.repo_name,
            "state": state,
        });

        if !labels.is_empty() {
            args["labels"] = json!(labels.join(","));
        }

        let result = self.call_tool("github/list_issues", args).await?;
        let parsed: Vec<serde_json::Value> =
            serde_json::from_str(&result).unwrap_or_default();

        Ok(parsed
            .into_iter()
            .map(|v| GitHubIssue {
                number: v["number"].as_i64().unwrap_or(0),
                title: v["title"].as_str().unwrap_or("").to_string(),
                html_url: v["html_url"].as_str().unwrap_or("").to_string(),
                state: v["state"].as_str().unwrap_or("open").to_string(),
            })
            .collect())
    }
}
