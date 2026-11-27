use std::sync::Arc;

use axum::body::Bytes;
use db_queries::{gateway_exports::ExportEvent, gateway_requests::RequestScope};
use tokio::sync::OwnedSemaphorePermit;

use super::{
    super::{archive_blobs::ArchiveStorage, archive_events, catalog::digest, response_bytes},
    ndjson::{MAX_BYTES, Prefix, encode_error},
};
use crate::error::{ApiError, ApiResult};

pub(super) struct Page {
    pub body: Bytes,
    pub checksum: String,
    pub last_id: Option<String>,
    pub has_more: bool,
}

pub(super) async fn encode(
    pool: &db::PgPool,
    storage: Option<&Arc<ArchiveStorage>>,
    scope: RequestScope<'_>,
    events: Vec<ExportEvent>,
    mut has_more: bool,
    mut permit: OwnedSemaphorePermit,
) -> ApiResult<Page> {
    let mut events = events.into_iter().peekable();
    let mut builder = Prefix::default();
    while let Some(first) = events.next() {
        if builder.body.len() == MAX_BYTES {
            has_more = true;
            break;
        }
        let archive = first.archive_id.clone();
        let mut partition = vec![first];
        partition.extend(std::iter::from_fn(|| {
            events.next_if(|event| event.archive_id == archive)
        }));
        let hydration = archive_events::hydrate_partition(pool, storage, scope, &mut partition).await;
        if matches!(&hydration, Err(ApiError::ArchiveRetrievalRequired(_)))
            && builder.last_id.is_some()
        {
            has_more = true;
            break;
        }
        hydration?;

        // Hold the export slot through CPU work even if the caller disconnects.
        let (next, held_permit, truncated) = tokio::task::spawn_blocking(move || {
            let truncated = builder.append(partition)?;
            Ok::<_, ApiError>((builder, permit, truncated))
        })
        .await
        .map_err(|_| encode_error())??;
        builder = next;
        permit = held_permit;
        if truncated {
            if builder.count == 0 {
                return Err(ApiError::Internal(
                    "export event exceeds the page byte limit".into(),
                ));
            }
            has_more = true;
            break;
        }
    }
    tokio::task::spawn_blocking(move || {
        let body = builder.body;
        let checksum = digest(&body);
        Page {
            body: response_bytes::from_vec(body, permit),
            checksum,
            last_id: builder.last_id,
            has_more,
        }
    })
    .await
    .map_err(|_| encode_error())
}
