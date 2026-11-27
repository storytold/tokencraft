use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

use db_queries::{gateway_archives, gateway_exports::ExportEvent, gateway_requests::RequestScope};
use ids::GatewayArchiveId;

use super::archive_blobs::{ArchiveStorage, Expected, BAD_OBJECT, COLD_OBJECT};
use crate::error::{ApiError, ApiResult};

const MAX_ARCHIVES_PER_PAGE: usize = 4;

/// Keep a page contiguous while bounding storage reads, even for many small archives.
pub(super) fn limit_page(events: &mut Vec<ExportEvent>, limit: usize) -> bool {
    let mut archives = HashSet::new();
    let mut length = events.len().min(limit);
    for (index, event) in events[..length].iter().enumerate() {
        if let Some(id) = event.archive_id.as_deref() {
            archives.insert(id);
            if archives.len() > MAX_ARCHIVES_PER_PAGE {
                length = index;
                break;
            }
        }
    }
    let has_more = events.len() > length;
    events.truncate(length);
    has_more
}

pub(super) async fn hydrate(
    pool: &db::PgPool,
    storage: Option<&Arc<ArchiveStorage>>,
    scope: RequestScope<'_>,
    events: &mut [ExportEvent],
) -> ApiResult<()> {
    // Automatic archives retire contiguous publication ranges in the feed.
    for partition in events.chunk_by_mut(|left, right| left.archive_id == right.archive_id) {
        let Some(raw) = partition[0].archive_id.as_deref() else {
            continue;
        };
        let id = raw.parse().map_err(|_| invalid_event())?;
        let storage = storage.ok_or_else(storage_unavailable)?;
        let ids = partition.iter().map(|event| event.id.clone()).collect();
        let mut archived = read(pool, storage, scope, &id, ids).await?;
        for event in partition {
            let source = archived.remove(&event.id).ok_or_else(invalid_event)?;
            if source.project_id != event.project_id
                || source.request_id != event.request_id
                || source.client_ref != event.client_ref
                || source.event_type != event.event_type
                || source.occurred_at != event.occurred_at
                || !source
                    .data
                    .as_ref()
                    .is_some_and(serde_json::Value::is_object)
            {
                return Err(invalid_event());
            }
            event.data = source.data;
        }
    }
    Ok(())
}

async fn read(
    pool: &db::PgPool,
    storage: &Arc<ArchiveStorage>,
    scope: RequestScope<'_>,
    id: &GatewayArchiveId,
    ids: HashSet<String>,
) -> ApiResult<HashMap<String, ExportEvent>> {
    // The page already contains only authorized event IDs. Read their project-wide
    // archive internally and return only payloads matching those event markers.
    let project = RequestScope {
        client_ref: None,
        ..scope
    };
    let archive = gateway_archives::list(pool, project, Some(id), None, 1)
        .await?
        .pop()
        .filter(|archive| archive.automatic && archive.retired_at.is_some())
        .ok_or_else(invalid_event)?;
    let expected = Expected::from_manifest(scope.account, &archive).ok_or_else(invalid_event)?;
    storage.read_events(expected, ids).await.map_err(|code| match code {
        COLD_OBJECT => {
            let message = if scope.client_ref.is_none() {
                format!("Restore archive {id} under Exports, then resume the export from the same cursor.")
            } else {
                "Ask a project administrator to restore the archived history, then resume from the same cursor.".into()
            };
            ApiError::ArchiveRetrievalRequired(message)
        }
        BAD_OBJECT => invalid_event(),
        _ => storage_unavailable(),
    })
}

fn invalid_event() -> ApiError {
    ApiError::Internal("archived export event failed integrity verification".into())
}

fn storage_unavailable() -> ApiError {
    ApiError::ServiceUnavailable("archived export storage is unavailable".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_stops_before_the_fifth_archive_without_dropping_the_hot_prefix() {
        let mut events: Vec<_> = [
            Some("a"),
            Some("a"),
            Some("b"),
            Some("c"),
            Some("d"),
            None,
            Some("e"),
        ]
        .into_iter()
        .enumerate()
        .map(|(index, archive_id)| ExportEvent {
            id: index.to_string(),
            project_id: "project".into(),
            request_id: "request".into(),
            client_ref: None,
            event_type: "request.accepted".into(),
            occurred_at: Default::default(),
            data: archive_id.is_none().then(|| serde_json::json!({})),
            archive_id: archive_id.map(str::to_owned),
        })
        .collect();

        assert!(limit_page(&mut events, 500));
        assert_eq!(events.len(), 6);
        assert_eq!(events.last().unwrap().id, "5");
        assert!(!limit_page(&mut events, 6));
        assert!(limit_page(&mut events, 2));
        assert_eq!(events.last().unwrap().id, "1");
    }
}
