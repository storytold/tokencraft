use std::{sync::Arc, time::Duration};

use db_queries::{
    gateway_archive_retirement as db_retirement, gateway_archives, gateway_exports,
    gateway_requests::RequestScope,
};
use ids::{AccountId, GatewayArchiveId, GatewayProjectId};

use super::{
    archive_blobs::{ArchiveStorage, Expected},
    exports::ndjson,
    readiness::Readiness,
};

pub(super) fn spawn(pool: db::PgPool, storage: Arc<ArchiveStorage>, age_days: i32, readiness: Arc<Readiness>) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(5));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            readiness.work_tick(&mut tick).await;
            if let Err(error) = work(&pool, &storage, age_days).await {
                tracing::warn!(
                    sqlstate = ?error.as_database_error().and_then(|error| error.code()),
                    "archive retirement database operation failed"
                );
            }
        }
    });
}

async fn work(pool: &db::PgPool, storage: &Arc<ArchiveStorage>, age_days: i32) -> sqlx::Result<()> {
    let Some(task) = db_retirement::claim(pool, age_days).await? else {
        return Ok(());
    };
    let (Ok(account), Ok(project), Ok(id)) = (
        task.account_id.parse::<AccountId>(),
        task.project_id.parse::<GatewayProjectId>(),
        task.id.parse::<GatewayArchiveId>(),
    ) else {
        return db_retirement::defer(pool, &task, "archive_scope_invalid").await;
    };
    let scope = RequestScope {
        account: &account,
        project: &project,
        client_ref: None,
    };
    let archive = gateway_archives::list(pool, scope, Some(&id), None, 1)
        .await?
        .pop();
    let Some(expected) = archive.and_then(|archive| Expected::from_manifest(&account, &archive))
    else {
        return db_retirement::defer(pool, &task, "archive_manifest_missing").await;
    };
    let count = i64::from(expected.event_count);
    let archive_bytes = match storage.read(expected).await {
        Ok(bytes) => bytes,
        Err(code) => return db_retirement::defer(pool, &task, code).await,
    };

    // Lock only after storage I/O. Repeatable read ensures a concurrent edit of
    // a source row aborts the retirement instead of discarding the newer value.
    let mut tx = pool
        .begin_with("BEGIN ISOLATION LEVEL REPEATABLE READ")
        .await?;
    if !db_retirement::lock(&mut tx, &task).await? {
        return tx.rollback().await;
    }
    let source = gateway_exports::events_between(
        &mut *tx,
        scope,
        task.after_seq,
        task.through_seq,
        count + 1,
    )
    .await?;
    let matches_archive = source.len() as i64 == count
        && ndjson::encode(source).is_ok_and(|(prefix, truncated)| !truncated && prefix.body == archive_bytes);
    if !matches_archive || !db_retirement::retire(&mut tx, &task).await? {
        tx.rollback().await?;
        return db_retirement::defer(pool, &task, "archive_source_changed").await;
    }
    tx.commit().await
}
