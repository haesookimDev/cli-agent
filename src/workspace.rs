//! Disk layout for a Workspace and the safe-write helpers used by the
//! `/v1/workspaces/:wid/files` handlers.
//!
//! ```text
//! <workspace.root_path>/
//! ├── sessions/<session_uuid>/   # per-session agent working dir
//! └── files/                     # shared workspace files (notes/, etc.)
//! ```
//!
//! `validate_relative_subpath` mirrors the helper used by
//! `SessionWorkspaceManager` so user/persona-supplied paths can never escape
//! the workspace root.

use std::path::{Component, Path, PathBuf};

use anyhow::Context;
use tokio::fs;
use uuid::Uuid;

use crate::types::Workspace;

#[derive(Debug, Clone)]
pub struct WorkspaceManager;

impl Default for WorkspaceManager {
    fn default() -> Self {
        Self
    }
}

impl WorkspaceManager {
    pub fn new() -> Self {
        Self
    }

    pub fn workspace_dir(&self, ws: &Workspace) -> PathBuf {
        PathBuf::from(&ws.root_path)
    }

    pub fn sessions_root(&self, ws: &Workspace) -> PathBuf {
        self.workspace_dir(ws).join("sessions")
    }

    pub fn files_dir(&self, ws: &Workspace) -> PathBuf {
        self.workspace_dir(ws).join("files")
    }

    pub fn session_dir(&self, ws: &Workspace, session_id: Uuid) -> PathBuf {
        self.sessions_root(ws).join(session_id.to_string())
    }

    pub async fn ensure_workspace_dir(&self, ws: &Workspace) -> anyhow::Result<PathBuf> {
        let root = self.workspace_dir(ws);
        fs::create_dir_all(&root).await.with_context(|| {
            format!("create workspace root {}", root.display())
        })?;
        fs::create_dir_all(self.sessions_root(ws)).await.ok();
        fs::create_dir_all(self.files_dir(ws)).await.ok();
        Ok(root)
    }

    pub async fn ensure_session_dir(
        &self,
        ws: &Workspace,
        session_id: Uuid,
    ) -> anyhow::Result<PathBuf> {
        self.ensure_workspace_dir(ws).await?;
        let dir = self.session_dir(ws, session_id);
        fs::create_dir_all(&dir).await?;
        Ok(dir)
    }

    pub async fn delete_workspace_dir(&self, ws: &Workspace) -> anyhow::Result<()> {
        let root = self.workspace_dir(ws);
        match fs::remove_dir_all(&root).await {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(err.into()),
        }
    }

    /// Resolve `relative_path` underneath `<workspace>/files/`. Refuses
    /// absolute paths and parent-traversal so a malicious upload cannot
    /// escape the workspace root.
    pub fn resolve_file_path(
        &self,
        ws: &Workspace,
        relative_path: &str,
    ) -> anyhow::Result<PathBuf> {
        let normalized = validate_relative_subpath(relative_path)?;
        Ok(self.files_dir(ws).join(normalized))
    }

    pub async fn write_file(
        &self,
        ws: &Workspace,
        relative_path: &str,
        bytes: &[u8],
    ) -> anyhow::Result<PathBuf> {
        self.ensure_workspace_dir(ws).await?;
        let target = self.resolve_file_path(ws, relative_path)?;
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).await?;
        }
        fs::write(&target, bytes).await?;
        Ok(target)
    }

    pub async fn read_file(
        &self,
        ws: &Workspace,
        relative_path: &str,
    ) -> anyhow::Result<Vec<u8>> {
        let target = self.resolve_file_path(ws, relative_path)?;
        let bytes = fs::read(&target).await?;
        Ok(bytes)
    }

    pub async fn delete_file(
        &self,
        ws: &Workspace,
        relative_path: &str,
    ) -> anyhow::Result<()> {
        let target = self.resolve_file_path(ws, relative_path)?;
        match fs::remove_file(&target).await {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(err.into()),
        }
    }
}

/// Default workspace root for new installs: `$HOME/.cli-agent/workspaces/`.
/// Falls back to `./.cli-agent/workspaces/` when `$HOME` is missing.
pub fn default_workspaces_root() -> PathBuf {
    if let Ok(home) = std::env::var("HOME") {
        if !home.is_empty() {
            return PathBuf::from(home).join(".cli-agent").join("workspaces");
        }
    }
    PathBuf::from(".cli-agent").join("workspaces")
}

/// Slug-ify a workspace name for use as a directory component. Mirrors
/// the agent name slug helper but exposed here so workspace creation
/// doesn't depend on the agent loader.
pub fn workspace_slug(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
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
        out.push_str("workspace");
    }
    out
}

pub fn validate_relative_subpath(raw: &str) -> anyhow::Result<PathBuf> {
    let trimmed = raw.trim();
    anyhow::ensure!(!trimmed.is_empty(), "path must not be empty");
    let candidate = PathBuf::from(trimmed);
    anyhow::ensure!(
        !candidate.is_absolute(),
        "workspace paths must be relative: {raw}"
    );
    anyhow::ensure!(
        candidate
            .components()
            .all(|component| matches!(component, Component::Normal(_) | Component::CurDir)),
        "workspace paths must not contain parent traversal: {raw}"
    );
    Ok(candidate)
}

pub fn relative_path_normalized(path: &Path) -> Option<String> {
    let mut parts = Vec::new();
    for component in path.components() {
        match component {
            Component::Normal(p) => parts.push(p.to_string_lossy().to_string()),
            Component::CurDir => {}
            _ => return None,
        }
    }
    Some(parts.join("/"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::WorkspaceKind;
    use chrono::Utc;

    fn make_ws(root: PathBuf) -> Workspace {
        Workspace {
            id: Uuid::new_v4().to_string(),
            slug: "test_ws".to_string(),
            name: "Test".to_string(),
            kind: WorkspaceKind::General,
            root_path: root.to_string_lossy().to_string(),
            description: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    #[test]
    fn slug_handles_unicode_and_specials() {
        assert_eq!(workspace_slug("My Project!"), "my_project");
        assert_eq!(workspace_slug("---"), "workspace");
    }

    #[test]
    fn validate_rejects_traversal() {
        assert!(validate_relative_subpath("../etc/passwd").is_err());
        assert!(validate_relative_subpath("/abs/path").is_err());
        assert!(validate_relative_subpath("notes/foo.md").is_ok());
    }

    #[tokio::test]
    async fn write_read_roundtrip() {
        let dir = std::env::temp_dir().join(format!("ws-rw-{}", Uuid::new_v4()));
        let ws = make_ws(dir.clone());
        let mgr = WorkspaceManager::new();
        mgr.write_file(&ws, "notes/a.md", b"# hi").await.unwrap();
        let bytes = mgr.read_file(&ws, "notes/a.md").await.unwrap();
        assert_eq!(bytes, b"# hi");
        mgr.delete_workspace_dir(&ws).await.unwrap();
    }

    #[tokio::test]
    async fn resolve_blocks_traversal() {
        let dir = std::env::temp_dir().join(format!("ws-trav-{}", Uuid::new_v4()));
        let ws = make_ws(dir.clone());
        let mgr = WorkspaceManager::new();
        let err = mgr.write_file(&ws, "../escape", b"x").await.unwrap_err();
        assert!(err.to_string().contains("parent traversal"));
        let _ = fs::remove_dir_all(&dir).await;
    }
}
