use std::path::Path;
use std::time::Instant;

use tokio::process::Command;

use crate::types::ValidationResult;

pub struct CommandRunner;

impl CommandRunner {
    /// Run a single shell command and return a structured result.
    pub async fn run_command(
        command: &str,
        working_dir: &Path,
        timeout_ms: u64,
    ) -> anyhow::Result<ValidationResult> {
        let started = Instant::now();
        let parts = shell_words::split(command)?;
        let (program, args) = parts
            .split_first()
            .ok_or_else(|| anyhow::anyhow!("empty command"))?;

        let mut cmd = Command::new(program);
        cmd.args(args)
            .current_dir(working_dir)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());

        let output = tokio::time::timeout(
            std::time::Duration::from_millis(timeout_ms),
            cmd.output(),
        )
        .await
        .map_err(|_| anyhow::anyhow!("command timed out after {}ms", timeout_ms))??;

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        let exit_code = output.status.code().unwrap_or(-1);

        Ok(ValidationResult {
            command: command.to_string(),
            exit_code,
            stdout,
            stderr,
            duration_ms: started.elapsed().as_millis(),
            passed: exit_code == 0,
        })
    }

    /// Run a sequence of commands, stopping at first failure.
    pub async fn run_commands(
        commands: &[String],
        working_dir: &Path,
        timeout_ms: u64,
    ) -> Vec<ValidationResult> {
        let mut results = Vec::new();
        for cmd in commands {
            match Self::run_command(cmd, working_dir, timeout_ms).await {
                Ok(result) => {
                    let passed = result.passed;
                    results.push(result);
                    if !passed {
                        break;
                    }
                }
                Err(e) => {
                    results.push(ValidationResult {
                        command: cmd.clone(),
                        exit_code: -1,
                        stdout: String::new(),
                        stderr: e.to_string(),
                        duration_ms: 0,
                        passed: false,
                    });
                    break;
                }
            }
        }
        results
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[tokio::test]
    async fn run_command_success() {
        let dir = PathBuf::from(".");
        let result = CommandRunner::run_command("echo hello", &dir, 5000)
            .await
            .unwrap();
        assert!(result.passed);
        assert_eq!(result.exit_code, 0);
        assert!(result.stdout.contains("hello"));
    }

    #[tokio::test]
    async fn run_command_failure() {
        let dir = PathBuf::from(".");
        let result = CommandRunner::run_command("false", &dir, 5000)
            .await
            .unwrap();
        assert!(!result.passed);
        assert_ne!(result.exit_code, 0);
    }

    #[tokio::test]
    async fn run_commands_stops_on_failure() {
        let dir = PathBuf::from(".");
        let commands = vec![
            "echo first".to_string(),
            "false".to_string(),
            "echo third".to_string(),
        ];
        let results = CommandRunner::run_commands(&commands, &dir, 5000).await;
        assert_eq!(results.len(), 2);
        assert!(results[0].passed);
        assert!(!results[1].passed);
    }
}
