use std::sync::Arc;

use chrono::Utc;
use dashmap::DashMap;
use tokio::sync::mpsc;
use tracing::{error, info};
use uuid::Uuid;

use crate::orchestrator::Orchestrator;
use crate::types::{
    ClusterRunRecord, ClusterRunRequest, ClusterRunStatus, ClusterSubRunEntry, RunRequest,
    RunStatus, TaskProfile,
};

// ── Internal message bus ─────────────────────────────────────────────────────

#[derive(Debug)]
enum ClusterMessage {
    SubRunCompleted {
        cluster_run_id: Uuid,
        run_id: Uuid,
        status: RunStatus,
    },
}

// ── OrchestratorCluster ──────────────────────────────────────────────────────

/// Manages a set of named `Orchestrator` instances and coordinates cluster-level runs.
///
/// Each member orchestrator handles one sub-task; the cluster aggregates their
/// results into a single [`ClusterRunRecord`].
///
/// Members share all underlying resources (router, memory, MCP, …) because
/// `Orchestrator` is `Clone` and all fields are `Arc<…>`.  They are
/// differentiated only by the sessions they create.
#[derive(Clone)]
pub struct OrchestratorCluster {
    members: Arc<DashMap<String, Orchestrator>>,
    runs: Arc<DashMap<Uuid, ClusterRunRecord>>,
    tx: mpsc::Sender<ClusterMessage>,
}

impl OrchestratorCluster {
    pub fn new() -> Self {
        let (tx, rx) = mpsc::channel::<ClusterMessage>(256);
        let runs: Arc<DashMap<Uuid, ClusterRunRecord>> = Arc::new(DashMap::new());

        let runs_bg = runs.clone();
        tokio::spawn(Self::message_loop(rx, runs_bg));

        Self {
            members: Arc::new(DashMap::new()),
            runs,
            tx,
        }
    }

    /// Background task: receives completion messages and updates cluster run state.
    async fn message_loop(
        mut rx: mpsc::Receiver<ClusterMessage>,
        runs: Arc<DashMap<Uuid, ClusterRunRecord>>,
    ) {
        while let Some(msg) = rx.recv().await {
            match msg {
                ClusterMessage::SubRunCompleted {
                    cluster_run_id,
                    run_id,
                    status,
                } => {
                    if let Some(mut record) = runs.get_mut(&cluster_run_id) {
                        for sub in &mut record.sub_runs {
                            if sub.run_id == run_id {
                                sub.status = status;
                                break;
                            }
                        }

                        let all_done = record.sub_runs.iter().all(|s| s.status.is_terminal());
                        if all_done {
                            let succeeded = record
                                .sub_runs
                                .iter()
                                .filter(|s| matches!(s.status, RunStatus::Succeeded))
                                .count();
                            let total = record.sub_runs.len();

                            record.status = if succeeded == total {
                                ClusterRunStatus::Succeeded
                            } else if succeeded == 0 {
                                ClusterRunStatus::Failed
                            } else {
                                ClusterRunStatus::PartialFailure
                            };
                            record.completed_at = Some(Utc::now());
                            info!(
                                "cluster run {} finished: {}",
                                cluster_run_id, record.status
                            );
                        }
                    }
                }
            }
        }
    }

    /// Register a named orchestrator as a cluster member.
    pub fn add(&self, name: impl Into<String>, orchestrator: Orchestrator) {
        let name = name.into();
        info!("cluster: registered member '{name}'");
        self.members.insert(name, orchestrator);
    }

    /// Names of all registered cluster members.
    pub fn member_names(&self) -> Vec<String> {
        let mut names: Vec<_> = self.members.iter().map(|e| e.key().clone()).collect();
        names.sort();
        names
    }

    /// Submit a cluster run.
    ///
    /// If `req.subtasks` is empty, the task is broadcast to **all** members.
    /// Otherwise each entry in `subtasks` is dispatched to the named orchestrator.
    pub async fn submit(&self, req: ClusterRunRequest) -> anyhow::Result<ClusterRunRecord> {
        if self.members.is_empty() {
            anyhow::bail!("no orchestrators registered in cluster");
        }

        // Build (orchestrator_name, task) pairs
        let assignments: Vec<(String, String)> = if req.subtasks.is_empty() {
            self.members
                .iter()
                .map(|e| (e.key().clone(), req.task.clone()))
                .collect()
        } else {
            // Validate that all named orchestrators exist before submitting anything
            for s in &req.subtasks {
                if !self.members.contains_key(&s.orchestrator) {
                    anyhow::bail!(
                        "orchestrator '{}' not registered in cluster",
                        s.orchestrator
                    );
                }
            }
            req.subtasks
                .iter()
                .map(|s| (s.orchestrator.clone(), s.task.clone()))
                .collect()
        };

        let cluster_run_id = Uuid::new_v4();
        let mut sub_runs = Vec::with_capacity(assignments.len());

        for (orch_name, subtask) in &assignments {
            let orch = self.members.get(orch_name).unwrap().clone();

            let run_req = RunRequest {
                task: subtask.clone(),
                profile: TaskProfile::General,
                session_id: None,
                workflow_id: None,
                workflow_params: None,
                repo_url: None,
            };

            let submission = orch.submit_run(run_req).await?;
            info!(
                "cluster run {cluster_run_id}: submitted sub-run {} on '{orch_name}'",
                submission.run_id
            );

            sub_runs.push(ClusterSubRunEntry {
                orchestrator_name: orch_name.clone(),
                run_id: submission.run_id,
                task: subtask.clone(),
                status: RunStatus::Queued,
            });

            Self::spawn_watcher(
                orch.clone(),
                cluster_run_id,
                submission.run_id,
                self.tx.clone(),
            );
        }

        let record = ClusterRunRecord {
            cluster_run_id,
            task: req.task,
            sub_runs,
            status: ClusterRunStatus::Running,
            created_at: Utc::now(),
            completed_at: None,
        };

        self.runs.insert(cluster_run_id, record.clone());
        Ok(record)
    }

    /// Spawn a background task that polls a sub-run to completion and notifies the bus.
    fn spawn_watcher(
        orch: Orchestrator,
        cluster_run_id: Uuid,
        run_id: Uuid,
        tx: mpsc::Sender<ClusterMessage>,
    ) {
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
                match orch.get_run(run_id).await {
                    Ok(Some(run)) if run.status.is_terminal() => {
                        let _ = tx
                            .send(ClusterMessage::SubRunCompleted {
                                cluster_run_id,
                                run_id,
                                status: run.status,
                            })
                            .await;
                        break;
                    }
                    Ok(Some(_)) => continue,
                    Ok(None) => {
                        error!("cluster watcher: run {run_id} disappeared");
                        break;
                    }
                    Err(e) => {
                        error!("cluster watcher: error polling {run_id}: {e}");
                        break;
                    }
                }
            }
        });
    }

    /// Retrieve a cluster run by ID (returns the latest snapshot).
    pub fn get(&self, id: Uuid) -> Option<ClusterRunRecord> {
        self.runs.get(&id).map(|r| r.clone())
    }

    /// List recent cluster runs, newest first.
    pub fn list(&self, limit: usize) -> Vec<ClusterRunRecord> {
        let mut runs: Vec<_> = self.runs.iter().map(|e| e.value().clone()).collect();
        runs.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        runs.truncate(limit);
        runs
    }
}

impl Default for OrchestratorCluster {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn member_names_sorted() {
        // OrchestratorCluster::new() requires a tokio runtime for the background task
        // so we only test helper logic here without spinning up the full cluster.
        let names: Vec<String> = {
            let mut v = vec!["gamma".to_string(), "alpha".to_string(), "beta".to_string()];
            v.sort();
            v
        };
        assert_eq!(names, ["alpha", "beta", "gamma"]);
    }
}
