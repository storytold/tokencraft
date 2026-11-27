use std::io::{self, Write};

use db_queries::gateway_exports::ExportEvent;

use super::{VersionedEvent, encode_error};
use crate::error::{ApiError, ApiResult};

const MAX_PAGE_BYTES: usize = 8 * 1024 * 1024;

pub(super) struct Page {
    pub body: Vec<u8>,
    pub last_id: Option<String>,
    pub has_more: bool,
}

pub(super) fn encode(events: Vec<ExportEvent>, mut has_more: bool) -> ApiResult<Page> {
    let mut body = Body::default();
    let mut last_id = None;
    for event in events {
        let offset = body.0.len();
        match VersionedEvent::new(&event)?.write(&mut body) {
            Ok(()) => last_id = Some(event.id),
            Err(error) if error.io_error_kind() == Some(io::ErrorKind::FileTooLarge) => {
                if offset == 0 {
                    return Err(ApiError::Internal("export event exceeds the page byte limit".into()));
                }
                body.0.truncate(offset);
                has_more = true;
                break;
            }
            Err(_) => return Err(encode_error()),
        }
    }
    Ok(Page {
        body: body.0,
        last_id,
        has_more,
    })
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
        let page = encode(vec![event("one", padding), event("two", padding)], false).unwrap();
        assert_eq!(page.body, first);
        assert_eq!(page.last_id.as_deref(), Some("one"));
        assert!(page.has_more);
    }

    #[test]
    fn an_individual_event_must_fit_including_its_newline() {
        let envelope = super::super::encode_events(vec![event("one", 0)]).unwrap().len();
        let fits = encode(vec![event("one", MAX_PAGE_BYTES - envelope)], false).unwrap();
        assert_eq!(fits.body.len(), MAX_PAGE_BYTES);
        assert_eq!(fits.body.last(), Some(&b'\n'));
        assert!(!fits.has_more);
        assert!(matches!(
            encode(vec![event("one", MAX_PAGE_BYTES - envelope + 1)], false),
            Err(ApiError::Internal(_))
        ));
    }
}
