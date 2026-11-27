use std::io::{self, Write};

use db_queries::gateway_exports::ExportEvent;
use serde::Serialize;

use super::SCHEMA_VERSION;
use crate::error::{ApiError, ApiResult};

pub(in crate::gateway_demo) const MAX_BYTES: usize = 8 * 1024 * 1024;

#[derive(Default)]
pub(in crate::gateway_demo) struct Prefix {
    pub body: Vec<u8>,
    pub last_id: Option<String>,
    pub count: i32,
}

impl Prefix {
    /// Returns true when the next whole event did not fit.
    pub(super) fn append(&mut self, events: Vec<ExportEvent>) -> ApiResult<bool> {
        let mut output = Body(&mut self.body);
        for event in events {
            if event.data.is_none() {
                return Err(ApiError::Internal("export event is missing its payload".into()));
            }
            let offset = output.0.len();
            let document = VersionedEvent {
                schema_version: SCHEMA_VERSION,
                event: &event,
            };
            let written = serde_json::to_writer(&mut output, &document)
                .and_then(|_| output.write_all(b"\n").map_err(serde_json::Error::io));
            match written {
                Ok(()) => {
                    self.last_id = Some(event.id);
                    self.count += 1;
                }
                Err(error) if error.io_error_kind() == Some(io::ErrorKind::FileTooLarge) => {
                    output.0.truncate(offset);
                    return Ok(true);
                }
                Err(_) => return Err(encode_error()),
            }
        }
        Ok(false)
    }
}

pub(in crate::gateway_demo) fn encode(events: Vec<ExportEvent>) -> ApiResult<(Prefix, bool)> {
    let mut prefix = Prefix::default();
    let truncated = prefix.append(events)?;
    Ok((prefix, truncated))
}

pub(super) fn encode_error() -> ApiError {
    ApiError::Internal("could not encode export event".into())
}

#[derive(Serialize)]
struct VersionedEvent<'a> {
    schema_version: u8,
    #[serde(flatten)]
    event: &'a ExportEvent,
}

struct Body<'a>(&'a mut Vec<u8>);

impl Write for Body<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > MAX_BYTES - self.0.len() {
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
        let padding = MAX_BYTES / 2;
        let (first, _) = encode(vec![event("one", padding)]).unwrap();
        let mut prefix = Prefix::default();
        assert!(!prefix.append(vec![event("one", padding)]).unwrap());
        assert!(prefix.append(vec![event("two", padding)]).unwrap());
        assert_eq!(prefix.body, first.body);
        assert_eq!(prefix.last_id.as_deref(), Some("one"));
        assert_eq!(prefix.count, 1);
    }

    #[test]
    fn an_individual_event_must_fit_including_its_newline() {
        let (empty, _) = encode(vec![event("one", 0)]).unwrap();
        let padding = MAX_BYTES - empty.body.len();
        let (fits, truncated) = encode(vec![event("one", padding)]).unwrap();
        assert!(!truncated);
        assert_eq!(fits.body.len(), MAX_BYTES);
        assert_eq!(fits.body.last(), Some(&b'\n'));
        let (oversize, truncated) = encode(vec![event("one", padding + 1)]).unwrap();
        assert!(truncated);
        assert!(oversize.body.is_empty());
        assert_eq!(oversize.count, 0);
        assert!(oversize.last_id.is_none());
    }
}
