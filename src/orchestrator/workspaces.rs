//! High-level workspace operations bridging the SQLite metadata layer in
//! `memory::store` and the filesystem helper in `workspace::WorkspaceManager`.
//!
//! Handlers in `interface::handlers::workspaces` go through these methods
//! instead of touching either layer directly so creation always pairs a
//! row with the on-disk root, and deletes always run as a soft-then-purge
//! that drops the DB row even if the filesystem cleanup partially fails.

use std::path::PathBuf;

use chrono::Utc;
use uuid::Uuid;

use crate::types::{Workspace, WorkspaceFile, WorkspaceFileCreatedBy, WorkspaceKind};
use crate::workspace::{relative_path_normalized, validate_relative_subpath, workspace_slug};

use super::Orchestrator;

#[derive(Debug, Clone)]
pub struct CreateWorkspaceArgs {
    pub name: String,
    pub kind: WorkspaceKind,
    pub root_path: Option<String>,
    pub description: Option<String>,
}

impl Orchestrator {
    pub async fn create_workspace(
        &self,
        args: CreateWorkspaceArgs,
    ) -> anyhow::Result<Workspace> {
        let name = args.name.trim().to_string();
        anyhow::ensure!(!name.is_empty(), "workspace name must not be empty");

        // Pick a unique slug; collide with existing slugs by suffixing -2,-3,..
        let base_slug = workspace_slug(&name);
        let mut slug = base_slug.clone();
        let mut counter = 2;
        while self.memory.store().get_workspace_by_slug(&slug).await?.is_some() {
            slug = format!("{base_slug}_{counter}");
            counter += 1;
            if counter > 100 {
                anyhow::bail!("could not allocate unique workspace slug for {name}");
            }
        }

        let root_path = match args.root_path.as_ref().and_then(|p| {
            let trimmed = p.trim();
            if trimmed.is_empty() { None } else { Some(trimmed.to_string()) }
        }) {
            Some(p) => p,
            None => self
                .workspaces_root
                .join(&slug)
                .to_string_lossy()
                .to_string(),
        };

        let now = Utc::now();
        let ws = Workspace {
            id: Uuid::new_v4().to_string(),
            slug,
            name,
            kind: args.kind,
            root_path,
            description: args.description.filter(|s| !s.trim().is_empty()),
            created_at: now,
            updated_at: now,
        };

        self.memory.store().insert_workspace(&ws).await?;
        self.workspace_manager.ensure_workspace_dir(&ws).await?;
        Ok(ws)
    }

    pub async fn list_workspaces(
        &self,
        kind: Option<&str>,
    ) -> anyhow::Result<Vec<Workspace>> {
        self.memory.store().list_workspaces(kind).await
    }

    pub async fn get_workspace(&self, id: &str) -> anyhow::Result<Option<Workspace>> {
        self.memory.store().get_workspace(id).await
    }

    pub async fn update_workspace(
        &self,
        id: &str,
        name: Option<&str>,
        description: Option<Option<&str>>,
    ) -> anyhow::Result<()> {
        self.memory
            .store()
            .update_workspace(id, name, description)
            .await
    }

    /// Soft-delete: drops the DB row (cascades workspace_files, NULLs out
    /// session.workspace_id) and best-effort removes the filesystem root.
    pub async fn delete_workspace(&self, id: &str) -> anyhow::Result<()> {
        let ws = self.memory.store().get_workspace(id).await?;
        self.memory.store().delete_workspace(id).await?;
        if let Some(ws) = ws {
            let _ = self.workspace_manager.delete_workspace_dir(&ws).await;
        }
        Ok(())
    }

    pub async fn list_workspace_sessions(
        &self,
        workspace_id: &str,
        limit: usize,
    ) -> anyhow::Result<Vec<Uuid>> {
        self.memory
            .store()
            .list_workspace_sessions(workspace_id, limit)
            .await
    }

    pub async fn assign_session_workspace(
        &self,
        session_id: Uuid,
        workspace_id: Option<&str>,
    ) -> anyhow::Result<()> {
        self.memory
            .store()
            .set_session_workspace(session_id, workspace_id)
            .await
    }

    pub async fn list_workspace_files(
        &self,
        workspace_id: &str,
        session_id: Option<Uuid>,
        prefix: Option<&str>,
    ) -> anyhow::Result<Vec<WorkspaceFile>> {
        self.memory
            .store()
            .list_workspace_files(workspace_id, session_id, prefix)
            .await
    }

    pub async fn read_workspace_file(
        &self,
        workspace_id: &str,
        relative_path: &str,
    ) -> anyhow::Result<Option<(WorkspaceFile, Vec<u8>)>> {
        let Some(ws) = self.memory.store().get_workspace(workspace_id).await? else {
            return Ok(None);
        };
        let Some(meta) = self
            .memory
            .store()
            .get_workspace_file(workspace_id, relative_path)
            .await?
        else {
            return Ok(None);
        };
        let bytes = self.workspace_manager.read_file(&ws, relative_path).await?;
        Ok(Some((meta, bytes)))
    }

    pub async fn upload_workspace_file(
        &self,
        workspace_id: &str,
        relative_path: &str,
        session_id: Option<Uuid>,
        bytes: Vec<u8>,
        mime: Option<String>,
        created_by: WorkspaceFileCreatedBy,
        created_by_persona: Option<String>,
    ) -> anyhow::Result<WorkspaceFile> {
        let Some(ws) = self.memory.store().get_workspace(workspace_id).await? else {
            anyhow::bail!("workspace not found");
        };

        // Path validation — reject traversal / absolute paths up front.
        let normalized = validate_relative_subpath(relative_path)?;
        let stored_path = relative_path_normalized(&normalized)
            .ok_or_else(|| anyhow::anyhow!("invalid relative path"))?;

        self.workspace_manager
            .write_file(&ws, &stored_path, &bytes)
            .await?;

        let now = Utc::now();
        let mut hasher = sha2_like_digest(&bytes);
        let existing = self
            .memory
            .store()
            .get_workspace_file(workspace_id, &stored_path)
            .await?;
        let id = existing
            .as_ref()
            .map(|f| f.id.clone())
            .unwrap_or_else(|| Uuid::new_v4().to_string());
        let created_at = existing
            .as_ref()
            .map(|f| f.created_at)
            .unwrap_or(now);
        let file = WorkspaceFile {
            id,
            workspace_id: workspace_id.to_string(),
            session_id,
            relative_path: stored_path,
            size_bytes: bytes.len() as u64,
            mime,
            sha256: Some(std::mem::take(&mut hasher)),
            created_by,
            created_by_persona,
            created_at,
            updated_at: now,
        };
        self.memory.store().upsert_workspace_file(&file).await?;
        Ok(file)
    }

    pub async fn delete_workspace_file(
        &self,
        workspace_id: &str,
        relative_path: &str,
    ) -> anyhow::Result<bool> {
        let Some(ws) = self.memory.store().get_workspace(workspace_id).await? else {
            return Ok(false);
        };
        let removed = self
            .memory
            .store()
            .delete_workspace_file(workspace_id, relative_path)
            .await?;
        if removed {
            let _ = self.workspace_manager.delete_file(&ws, relative_path).await;
        }
        Ok(removed)
    }

    /// Get-or-create the default general workspace. Used by run_manager when
    /// a request arrives without a workspace_id but the runtime still needs
    /// somewhere to put working files (so we never write into the cli-agent
    /// repo by accident).
    pub async fn ensure_default_workspace(&self) -> anyhow::Result<Workspace> {
        if let Some(ws) = self
            .memory
            .store()
            .get_workspace_by_slug("default")
            .await?
        {
            self.workspace_manager.ensure_workspace_dir(&ws).await?;
            return Ok(ws);
        }
        let default_root: PathBuf = self.workspaces_root.join("default");
        let ws = Workspace {
            id: Uuid::new_v4().to_string(),
            slug: "default".to_string(),
            name: "Default".to_string(),
            kind: WorkspaceKind::General,
            root_path: default_root.to_string_lossy().to_string(),
            description: Some("Auto-created default workspace".to_string()),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        self.memory.store().insert_workspace(&ws).await?;
        self.workspace_manager.ensure_workspace_dir(&ws).await?;
        Ok(ws)
    }
}

/// Tiny sha256 stand-in. Avoids pulling a fresh crate just for upload
/// dedup in v1; we only need a stable identifier per content blob, not a
/// security-grade hash.
fn sha2_like_digest(bytes: &[u8]) -> String {
    use std::hash::Hasher;
    let mut h = std::collections::hash_map::DefaultHasher::new();
    h.write(bytes);
    format!("{:016x}", h.finish())
}
