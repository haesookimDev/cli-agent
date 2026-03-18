use std::path::{Component, Path, PathBuf};

use uuid::Uuid;

#[derive(Debug, Clone)]
pub struct SessionWorkspaceManager {
    root: PathBuf,
}

impl SessionWorkspaceManager {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn session_dir(&self, session_id: Uuid) -> PathBuf {
        self.root.join(session_id.to_string())
    }

    pub async fn ensure_session_dir(&self, session_id: Uuid) -> anyhow::Result<PathBuf> {
        let dir = self.session_dir(session_id);
        tokio::fs::create_dir_all(&dir).await?;
        Ok(dir)
    }

    pub async fn ensure_scoped_dir(
        &self,
        session_id: Uuid,
        requested: Option<&str>,
    ) -> anyhow::Result<PathBuf> {
        let session_dir = self.ensure_session_dir(session_id).await?;
        let scoped = match requested.map(str::trim).filter(|value| !value.is_empty()) {
            Some(raw) => session_dir.join(Self::validate_relative_subpath(raw)?),
            None => session_dir,
        };
        tokio::fs::create_dir_all(&scoped).await?;
        Ok(scoped)
    }

    pub async fn delete_session_dir(&self, session_id: Uuid) -> anyhow::Result<()> {
        let dir = self.session_dir(session_id);
        match tokio::fs::remove_dir_all(&dir).await {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(err.into()),
        }
    }

    fn validate_relative_subpath(raw: &str) -> anyhow::Result<PathBuf> {
        let candidate = PathBuf::from(raw);
        anyhow::ensure!(
            !candidate.is_absolute(),
            "session-scoped paths must be relative: {}",
            raw
        );
        anyhow::ensure!(
            candidate.components().all(|component| {
                matches!(component, Component::Normal(_) | Component::CurDir)
            }),
            "session-scoped paths must not contain parent traversal: {}",
            raw
        );
        Ok(candidate)
    }
}

#[cfg(test)]
mod tests {
    use super::SessionWorkspaceManager;
    use uuid::Uuid;

    #[tokio::test]
    async fn ensure_scoped_dir_creates_session_root() {
        let root = std::env::temp_dir().join(format!("workspace-test-{}", Uuid::new_v4()));
        let manager = SessionWorkspaceManager::new(root.clone());
        let session_id = Uuid::new_v4();

        let dir = manager.ensure_scoped_dir(session_id, None).await.unwrap();

        assert_eq!(dir, root.join(session_id.to_string()));
        assert!(dir.is_dir());

        let _ = tokio::fs::remove_dir_all(root).await;
    }

    #[tokio::test]
    async fn ensure_scoped_dir_rejects_parent_traversal() {
        let root = std::env::temp_dir().join(format!("workspace-test-{}", Uuid::new_v4()));
        let manager = SessionWorkspaceManager::new(root.clone());
        let session_id = Uuid::new_v4();

        let err = manager
            .ensure_scoped_dir(session_id, Some("../other-session"))
            .await
            .unwrap_err();

        assert!(err.to_string().contains("parent traversal"));

        let _ = tokio::fs::remove_dir_all(root).await;
    }
}
