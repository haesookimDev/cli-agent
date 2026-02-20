use std::str::FromStr;
use std::sync::Arc;

use chrono::Utc;
use cron::Schedule;
use tokio::time::sleep;
use tracing::{error, info};

use crate::memory::MemoryManager;
use crate::orchestrator::Orchestrator;

pub struct CronScheduler {
    memory: Arc<MemoryManager>,
    orchestrator: Orchestrator,
}

impl CronScheduler {
    pub fn new(memory: Arc<MemoryManager>, orchestrator: Orchestrator) -> Self {
        Self {
            memory,
            orchestrator,
        }
    }

    pub async fn run_loop(self) {
        info!("cron scheduler started (60s tick)");
        loop {
            sleep(std::time::Duration::from_secs(60)).await;

            let now = Utc::now();
            let due = match self.memory.list_due_schedules(now).await {
                Ok(list) => list,
                Err(e) => {
                    error!("scheduler: list_due_schedules failed: {e}");
                    continue;
                }
            };

            for schedule in due {
                info!(
                    "scheduler: executing workflow {} for schedule {}",
                    schedule.workflow_id, schedule.id
                );

                match self
                    .orchestrator
                    .execute_workflow(
                        &schedule.workflow_id,
                        schedule.parameters.clone(),
                        None,
                    )
                    .await
                {
                    Ok(submission) => {
                        info!(
                            "scheduler: workflow {} started run_id={}",
                            schedule.workflow_id, submission.run_id
                        );
                    }
                    Err(e) => {
                        error!(
                            "scheduler: failed to execute workflow {}: {e}",
                            schedule.workflow_id
                        );
                    }
                }

                // Compute next_run_at
                let next = compute_next_run(&schedule.cron_expr);
                if let Err(e) = self
                    .memory
                    .update_schedule_last_run(schedule.id, now, next)
                    .await
                {
                    error!("scheduler: update_schedule_last_run failed: {e}");
                }
            }
        }
    }
}

/// Parse cron expression and compute the next fire time after now.
pub fn compute_next_run(cron_expr: &str) -> Option<chrono::DateTime<Utc>> {
    let schedule = Schedule::from_str(cron_expr).ok()?;
    schedule.upcoming(Utc).next()
}
