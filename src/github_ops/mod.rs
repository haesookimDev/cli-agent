//! High-level GitHub operations layer.
//!
//! Wraps the MCP GitHub tool calls to provide a convenient API for agent
//! personas to create issues, pull requests, reviews, and branches.

pub mod branch_ops;
pub mod issue_ops;
pub mod pr_ops;

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::mcp::McpRegistry;
use crate::types::AgentPersona;

/// High-level GitHub operations backed by the MCP GitHub server.
#[derive(Clone)]
pub struct GitHubOps {
    mcp: Arc<McpRegistry>,
    pub repo_owner: String,
    pub repo_name: String,
    sign_comments: bool,
}

impl GitHubOps {
    pub fn new(
        mcp: Arc<McpRegistry>,
        repo_owner: String,
        repo_name: String,
        sign_comments: bool,
    ) -> Self {
        Self {
            mcp,
            repo_owner,
            repo_name,
            sign_comments,
        }
    }

    /// Full repo identifier in `owner/name` form.
    pub fn repo_full_name(&self) -> String {
        format!("{}/{}", self.repo_owner, self.repo_name)
    }

    /// Call an MCP tool by name with JSON arguments.
    pub(crate) async fn call_tool(
        &self,
        tool_name: &str,
        args: serde_json::Value,
    ) -> anyhow::Result<String> {
        let result = self.mcp.call_tool(tool_name, args).await?;
        if result.succeeded {
            Ok(result.content)
        } else {
            anyhow::bail!(
                "MCP tool '{}' failed: {}",
                tool_name,
                result.error.unwrap_or_default()
            )
        }
    }

    /// Append a persona signature to comment body if signing is enabled.
    pub(crate) fn sign_body(&self, persona: &AgentPersona, body: &str) -> String {
        if self.sign_comments {
            format!(
                "{}\n\n---\n*{}* — {} | `{}`",
                body, persona.display_name, persona.title, persona.github_username
            )
        } else {
            body.to_string()
        }
    }
}

// --- Shared types ---

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitHubIssue {
    pub number: i64,
    pub title: String,
    pub html_url: String,
    pub state: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitHubPR {
    pub number: i64,
    pub title: String,
    pub html_url: String,
    pub state: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ReviewEvent {
    Approve,
    RequestChanges,
    Comment,
}

impl std::fmt::Display for ReviewEvent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ReviewEvent::Approve => write!(f, "APPROVE"),
            ReviewEvent::RequestChanges => write!(f, "REQUEST_CHANGES"),
            ReviewEvent::Comment => write!(f, "COMMENT"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewComment {
    pub path: String,
    pub line: Option<u64>,
    pub body: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileChange {
    pub path: String,
    pub content: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MergeMethod {
    Merge,
    Squash,
    Rebase,
}

impl std::fmt::Display for MergeMethod {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MergeMethod::Merge => write!(f, "merge"),
            MergeMethod::Squash => write!(f, "squash"),
            MergeMethod::Rebase => write!(f, "rebase"),
        }
    }
}
