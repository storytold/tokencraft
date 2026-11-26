use axum::{
    body::Body,
    http::{HeaderMap, Request},
    middleware::Next,
    response::Response,
};
use sha2::{Digest, Sha256};

pub(crate) const TRACE_ID_HEADER: &str = "tokencraft-trace-id";

tokio::task_local! {
    static INGRESS: TraceContext;
}

#[derive(Clone)]
pub(crate) struct TraceContext {
    pub trace_id: String,
    pub span_id: String,
    pub caller: Option<CallerLink>,
}

#[derive(Clone)]
pub(crate) struct CallerLink {
    pub trace_id: String,
    pub span_id: String,
}

impl TraceContext {
    fn new(caller: Option<CallerLink>) -> Self {
        Self {
            trace_id: format!("{:032x}", rand::random::<u128>().max(1)),
            span_id: new_span_id(),
            caller,
        }
    }

    pub(crate) fn admission() -> Self {
        INGRESS.try_with(Clone::clone).unwrap_or_else(|_| Self::new(None))
    }
}

pub(crate) async fn scope(mut request: Request<Body>, next: Next) -> Response {
    let caller = caller_link(request.headers());
    let headers = request.headers_mut();
    headers.remove("traceparent");
    headers.remove("tracestate");
    headers.remove("baggage");
    INGRESS.scope(TraceContext::new(caller), next.run(request)).await
}

fn caller_link(headers: &HeaderMap) -> Option<CallerLink> {
    let mut values = headers.get_all("traceparent").iter();
    let value = values.next()?.to_str().ok()?;
    if values.next().is_some() || value.len() > 256 {
        return None;
    }
    let base = value.get(..55)?;
    let mut parts = base.split('-');
    let version = parts.next()?.as_bytes();
    let trace_id = parts.next()?;
    let span_id = parts.next()?;
    let flags = parts.next()?.as_bytes();
    if parts.next().is_some() || !valid_hex(version, 2) || version == b"ff"
        || !valid_trace_id(trace_id) || !nonzero_hex(span_id.as_bytes(), 16)
        || !valid_hex(flags, 2)
    {
        return None;
    }
    let suffix = &value.as_bytes()[55..];
    if (version == b"00" && !suffix.is_empty())
        || (!suffix.is_empty() && suffix.first() != Some(&b'-'))
        || value.contains(',')
    {
        return None;
    }
    Some(CallerLink {
        trace_id: trace_id.to_owned(),
        span_id: span_id.to_owned(),
    })
}

pub(crate) fn valid_trace_id(id: &str) -> bool {
    nonzero_hex(id.as_bytes(), 32)
}

fn valid_hex(data: &[u8], size: usize) -> bool {
    data.len() == size && data.iter().all(|byte| matches!(*byte, b'0'..=b'9' | b'a'..=b'f'))
}

fn nonzero_hex(data: &[u8], size: usize) -> bool {
    valid_hex(data, size) && data.iter().any(|byte| *byte != b'0')
}

fn new_span_id() -> String {
    format!("{:016x}", rand::random::<u64>().max(1))
}

pub(crate) fn stable_id(input: &str, len: usize) -> String {
    let mut value = format!("{:x}", Sha256::digest(input));
    value.truncate(len);
    if value.bytes().all(|byte| byte == b'0') {
        value.replace_range(..1, "1");
    }
    value
}

pub(crate) fn evidence_root_span_id(trace_id: &str) -> String {
    stable_id(&format!("tokencraft.otlp.v1.root:{trace_id}"), 16)
}

pub(crate) fn attach_provider_trace(
    request: reqwest::RequestBuilder,
    trace_id: Option<&str>,
) -> reqwest::RequestBuilder {
    let Some(trace_id) = trace_id.filter(|value| valid_trace_id(value)) else {
        return request;
    };
    // Connector spans can attach to the same root the scoped OTLP download exports.
    let parent = evidence_root_span_id(trace_id);
    request.header("traceparent", format!("00-{trace_id}-{parent}-01"))
}

pub(crate) fn work_span(
    operation: &'static str,
    request_id: &str,
    trace_id: Option<&str>,
    admission_span_id: Option<&str>,
) -> tracing::Span {
    tracing::debug_span!(
        "request_work",
        operation,
        request_id,
        trace_id = trace_id.unwrap_or(""),
        parent_span_id = admission_span_id.unwrap_or(""),
        span_id = %new_span_id(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    const TRACE_ID: &str = "4bf92f3577b34da6a3ce929d0e0e4736";
    const SPAN_ID: &str = "00f067aa0ba902b7";
    const VALID: &str = "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01";

    #[test]
    fn extracts_links_without_vendor_state() {
        let mut headers = HeaderMap::new();
        headers.insert("traceparent", HeaderValue::from_static(VALID));
        headers.insert("tracestate", HeaderValue::from_static("vendor=private"));
        let link = caller_link(&headers).unwrap();
        assert_eq!(link.trace_id, TRACE_ID);
        assert_eq!(link.span_id, SPAN_ID);
        let generated = TraceContext::new(Some(link));
        assert_ne!(generated.trace_id, TRACE_ID);
        assert!(valid_trace_id(&generated.trace_id));
        let future = format!("01-{}-{}-01-extension", TRACE_ID, SPAN_ID);
        headers.insert("traceparent", future.parse().unwrap());
        assert_eq!(caller_link(&headers).unwrap().trace_id, TRACE_ID);
    }

    #[test]
    fn ignores_invalid_or_duplicate_headers() {
        for value in [
            VALID.replacen("00-", "ff-", 1),
            VALID.replace(TRACE_ID, &TRACE_ID.to_ascii_uppercase()),
            VALID.replace(SPAN_ID, "0000000000000000"),
            VALID.replace(TRACE_ID, "00000000000000000000000000000000"),
            format!("{VALID}-extra"),
            format!("{VALID},{VALID}"),
            VALID[..54].to_owned(),
        ] {
            let mut headers = HeaderMap::new();
            headers.insert("traceparent", value.parse().unwrap());
            assert!(caller_link(&headers).is_none());
        }
        let mut headers = HeaderMap::new();
        headers.append("traceparent", HeaderValue::from_static(VALID));
        headers.append("traceparent", HeaderValue::from_static(VALID));
        assert!(caller_link(&headers).is_none());
    }

    #[test]
    fn provider_requests_only_carry_valid_execution_trace_ids() {
        let client = reqwest::Client::new();
        let request = || client.get("https://connector.example/v1/jobs");
        for trace_id in [None, Some(VALID), Some("00000000000000000000000000000000")] {
            let outbound = attach_provider_trace(request(), trace_id).build().unwrap();
            assert!(!outbound.headers().contains_key("traceparent"));
        }
        let outbound = attach_provider_trace(request(), Some(TRACE_ID)).build().unwrap();
        let expected = format!("00-{TRACE_ID}-{}-01", evidence_root_span_id(TRACE_ID));
        assert_eq!(outbound.headers()["traceparent"], expected);
    }
}
