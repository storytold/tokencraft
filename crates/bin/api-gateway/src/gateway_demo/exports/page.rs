use std::{
    io::{self, Write},
    sync::Arc,
};

use axum::body::Bytes;
use db_queries::{gateway_exports::ExportEvent, gateway_requests::RequestScope};
use tokio::sync::OwnedSemaphorePermit;

use super::{
    super::{archive_blobs::ArchiveStorage, archive_events, catalog::digest, response_bytes},
    VersionedEvent, encode_error,
};
use crate::error::{ApiError, ApiResult};

const MAX_PAGE_BYTES: usize = 8 * 1024 * 1024;

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
    let mut builder = Builder::default();
    while let Some(first) = events.next() {
        if builder.body.0.len() == MAX_PAGE_BYTES {
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
            has_more = true;
            break;
        }
    }
    tokio::task::spawn_blocking(move || {
        let body = builder.body.0;
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

#[derive(Default)]
struct Builder {
    body: Body,
    last_id: Option<String>,
}

impl Builder {
    fn append(&mut self, events: Vec<ExportEvent>) -> ApiResult<bool> {
        for event in events {
            let offset = self.body.0.len();
            match VersionedEvent::new(&event)?.write(&mut self.body) {
                Ok(()) => self.last_id = Some(event.id),
                Err(error) if error.io_error_kind() == Some(io::ErrorKind::FileTooLarge) => {
                    if offset == 0 {
                        return Err(ApiError::Internal(
                            "export event exceeds the page byte limit".into(),
                        ));
                    }
                    self.body.0.truncate(offset);
                    return Ok(true);
                }
                Err(_) => return Err(encode_error()),
            }
        }
        Ok(false)
    }
}

#[derive(Default)]
struct Body(Vec<u8>);

impl Write for Body {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > MAX_PAGE_BYTES - self.0.len() {
            return Err(io::ErrorKind::FileTooLarge.into());
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(id: &str, padding: usize) -> ExportEvent {
        ExportEvent {
            id: id.into(),
            project_id: "fixture-project".into(),
            request_id: "fixture-request".into(),
            client_ref: None,
            event_type: "request.succeeded".into(),
            occurred_at: Default::default(),
            data: Some(serde_json::json!({ "padding": "a".repeat(padding) })),
            archive_id: None,
        }
    }

    #[test]
    fn stops_before_the_first_event_that_cannot_fit() {
        let padding = MAX_PAGE_BYTES / 2;
        let first = super::super::encode_events(vec![event("one", padding)]).unwrap();
        let mut page = Builder::default();
        assert!(!page.append(vec![event("one", padding)]).unwrap());
        assert!(page.append(vec![event("two", padding)]).unwrap());
        assert_eq!(page.body.0, first);
        assert_eq!(page.last_id.as_deref(), Some("one"));
    }

    #[test]
    fn an_individual_event_must_fit_including_its_newline() {
        let envelope = super::super::encode_events(vec![event("one", 0)]).unwrap().len();
        let mut fits = Builder::default();
        assert!(!fits.append(vec![event("one", MAX_PAGE_BYTES - envelope)]).unwrap());
        assert_eq!(fits.body.0.len(), MAX_PAGE_BYTES);
        assert_eq!(fits.body.0.last(), Some(&b'\n'));
        assert!(matches!(
            Builder::default().append(vec![event("one", MAX_PAGE_BYTES - envelope + 1)]),
            Err(ApiError::Internal(_))
        ));
    }
}
