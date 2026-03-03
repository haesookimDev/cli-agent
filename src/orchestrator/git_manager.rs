use std::path::Path;

use tokio::process::Command;

use crate::router::ModelRouter;
use crate::types::TaskProfile;

pub struct GitManager;

impl GitManager {
    async fn run_git(working_dir: &Path, args: &[&str]) -> anyhow::Result<String> {
        let output = Command::new("git")
            .args(args)
            .current_dir(working_dir)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .output()
            .await?;

        if output.status.success() {
            Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            Err(anyhow::anyhow!("git {} failed: {}", args.join(" "), stderr))
        }
    }

    pub async fn is_git_repo(working_dir: &Path) -> bool {
        Self::run_git(working_dir, &["rev-parse", "--is-inside-work-tree"])
            .await
            .is_ok()
    }

    pub async fn current_branch(working_dir: &Path) -> anyhow::Result<String> {
        Self::run_git(working_dir, &["rev-parse", "--abbrev-ref", "HEAD"]).await
    }

    pub async fn has_dirty_files(working_dir: &Path) -> anyhow::Result<bool> {
        let status = Self::run_git(working_dir, &["status", "--porcelain"]).await?;
        Ok(!status.is_empty())
    }

    /// Stash dirty files. Returns true if something was stashed.
    pub async fn stash_save(working_dir: &Path, message: &str) -> anyhow::Result<bool> {
        let before = Self::run_git(working_dir, &["stash", "list"]).await?;
        Self::run_git(working_dir, &["stash", "push", "-m", message]).await?;
        let after = Self::run_git(working_dir, &["stash", "list"]).await?;
        Ok(before != after)
    }

    pub async fn stash_pop(working_dir: &Path) -> anyhow::Result<()> {
        Self::run_git(working_dir, &["stash", "pop"]).await?;
        Ok(())
    }

    pub async fn create_branch(working_dir: &Path, branch_name: &str) -> anyhow::Result<()> {
        Self::run_git(working_dir, &["checkout", "-b", branch_name]).await?;
        Ok(())
    }

    pub async fn stage_all(working_dir: &Path) -> anyhow::Result<()> {
        Self::run_git(working_dir, &["add", "-A"]).await?;
        Ok(())
    }

    pub async fn staged_diff(working_dir: &Path) -> anyhow::Result<String> {
        Self::run_git(working_dir, &["diff", "--cached", "--stat"]).await
    }

    /// Commit staged changes. Returns the commit hash.
    pub async fn commit(working_dir: &Path, message: &str) -> anyhow::Result<String> {
        Self::run_git(working_dir, &["commit", "-m", message]).await?;
        Self::run_git(working_dir, &["rev-parse", "--short", "HEAD"]).await
    }

    pub async fn push(working_dir: &Path, branch: &str) -> anyhow::Result<()> {
        Self::run_git(working_dir, &["push", "-u", "origin", branch]).await?;
        Ok(())
    }

    /// Generate a conventional commit message from the diff using a fast LLM.
    pub async fn generate_commit_message(
        diff: &str,
        task_description: &str,
        router: &ModelRouter,
    ) -> anyhow::Result<String> {
        let prompt = format!(
            "Generate a concise conventional commit message for these changes.\n\
             Follow the format: type: subject\n\n\
             Types: feat, fix, refactor, test, docs, style, chore\n\
             Subject: max 50 chars, imperative mood, no period.\n\n\
             TASK CONTEXT:\n{}\n\n\
             DIFF STATS:\n{}\n\n\
             Respond with ONLY the commit message, nothing else.",
            Self::truncate(task_description, 200),
            Self::truncate(diff, 1000),
        );

        let constraints =
            crate::router::RoutingConstraints::for_profile(TaskProfile::General);
        let (_decision, inference) = router
            .infer(TaskProfile::General, &prompt, &constraints)
            .await?;

        let msg = inference.output.trim().to_string();
        if msg.is_empty() {
            Ok("chore: Agent-generated changes".to_string())
        } else {
            Ok(msg)
        }
    }

    fn truncate(text: &str, max_chars: usize) -> &str {
        if text.len() <= max_chars {
            text
        } else {
            &text[..text.floor_char_boundary(max_chars)]
        }
    }
}
