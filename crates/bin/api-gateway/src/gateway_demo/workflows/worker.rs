use std::sync::Arc;
use std::time::Duration;

use db_queries::workflows as db;
use tokio::{task::JoinSet, time::MissedTickBehavior};

use super::runner::{self, Runtime};
use crate::error::ApiError;

const MAX_CONCURRENT_ADVANCES: usize = 2;

pub(super) fn spawn(runtime: Runtime) {
    let runtime = Arc::new(runtime);
    tokio::spawn(expire_due_runs(Arc::clone(&runtime)));
    tokio::spawn(clean_up(Arc::clone(&runtime)));
    tokio::spawn(run(runtime));
}

async fn run(runtime: Arc<Runtime>) {
    let tool_pins = runtime.security.tools.worker_pins();
    let mut tasks = JoinSet::new();
    let mut schedule = tokio::time::interval(Duration::from_millis(300));
    schedule.set_missed_tick_behavior(MissedTickBehavior::Skip);

    loop {
        runtime.readiness.work_tick(&mut schedule).await;
        while let Some(result) = tasks.try_join_next() {
            let category = match result {
                Ok(Ok(())) => continue,
                Ok(Err(ApiError::Internal(_))) => "storage",
                Ok(Err(_)) => "workflow",
                Err(_) => "task",
            };
            tracing::warn!(category, "workflow worker deferred a claimed run");
        }

        for _ in tasks.len()..MAX_CONCURRENT_ADVANCES {
            if !runtime.readiness.database_schema_ready() {
                break;
            }
            match db::claim_run(&runtime.pool, &tool_pins).await {
                Ok(Some(row)) => {
                    let runtime = Arc::clone(&runtime);
                    tasks.spawn(async move { runner::advance(&runtime, row).await });
                }
                Ok(None) => break,
                Err(_) => {
                    tracing::warn!(category = "storage", "workflow worker could not claim a run");
                    break;
                }
            }
        }
    }
}

async fn expire_due_runs(runtime: Arc<Runtime>) {
    let mut schedule = tokio::time::interval(Duration::from_millis(500));
    schedule.set_missed_tick_behavior(MissedTickBehavior::Skip);
    loop {
        runtime.readiness.work_tick(&mut schedule).await;
        if db::expire_runs(&runtime.pool).await.is_err() {
            tracing::warn!("workflow deadline processing deferred");
        }
    }
}

async fn clean_up(runtime: Arc<Runtime>) {
    let mut schedule = tokio::time::interval(Duration::from_secs(60));
    schedule.set_missed_tick_behavior(MissedTickBehavior::Skip);
    loop {
        runtime.readiness.work_tick(&mut schedule).await;
        if db::forget_expired_content(&runtime.pool).await.is_err() {
            tracing::warn!("workflow retention cleanup deferred");
        }
        if db::forget_expired_uploads(&runtime.pool).await.is_err() {
            tracing::warn!("workflow upload cleanup deferred");
        }
    }
}
