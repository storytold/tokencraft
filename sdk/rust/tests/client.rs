use std::time::Duration;

use serde_json::json;
use tokencraft_sdk::{Error, Quote, QuoteInput, RequestStatus, TokenCraftClient, WaitOptions, Workload};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::TcpListener,
    task::JoinHandle,
    time::timeout,
};

const REQUEST: &str = "req_01m881w0c8np13xfvetqsbk8m0";
const TRACEPARENT: &str = "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01";

fn input() -> QuoteInput {
    QuoteInput {
        model: "studio_image_demo".into(),
        workload: Workload::Image {
            count: 1,
            aspect_ratio: "7:5".into(),
            max_relative_error_bps: None,
        },
        provider: None,
        max_total_usd_nanos: None,
    }
}

fn quote_fixture() -> Quote {
    serde_json::from_value(json!({
        "id": "quo_fixture", "catalog_revision": "fixture", "input": input(),
        "currency": "USD", "pinned_offer_id": "image_fixture",
        "selected_offer_id": "image_fixture", "candidates": []
    })).unwrap()
}

async fn reply(status: &str, headers: &str, body: &str) -> (String, JoinHandle<(String, String)>) {
    reply_wire(wire_response(status, headers, body)).await
}

async fn reply_wire(response: String) -> (String, JoinHandle<(String, String)>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let capture = tokio::spawn(async move { capture_and_reply(&listener, &response).await });
    (origin, capture)
}

fn wire_response(status: &str, headers: &str, body: &str) -> String {
    format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n{headers}\r\n{body}",
        body.len()
    )
}

async fn capture_and_reply(listener: &TcpListener, response: &str) -> (String, String) {
    let (socket, _) = listener.accept().await.unwrap();
    let mut socket = BufReader::new(socket);
    let mut head = String::new();
    loop {
        let mut line = String::new();
        assert!(socket.read_line(&mut line).await.unwrap() > 0);
        if line == "\r\n" {
            break;
        }
        head.push_str(&line);
    }
    let length = head
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().unwrap())
        })
        .unwrap_or(0);
    let mut body = vec![0; length];
    socket.read_exact(&mut body).await.unwrap();
    socket
        .get_mut()
        .write_all(response.as_bytes())
        .await
        .unwrap();
    (head.to_lowercase(), String::from_utf8(body).unwrap())
}

#[tokio::test]
async fn public_requests_omit_keys_and_respect_base_path() {
    let (origin, captured) = reply("200 OK", "", r#"{"revision":"fixture","offers":[]}"#).await;
    let client = TokenCraftClient::new(&format!("{origin}/api"), Some("fixture")).unwrap();
    assert_eq!(client.catalog().await.unwrap().revision, "fixture");
    let (head, body) = captured.await.unwrap();
    assert!(head.starts_with("get /api/v1/catalog/offers"));
    assert!(!head.contains("authorization:"));
    assert!(body.is_empty());
    assert!(TokenCraftClient::new("http://example.com", Some("fixture")).is_err());
    assert!(TokenCraftClient::new("https://usr@example.com", Some("fixture")).is_err());
    assert!(TokenCraftClient::new(&origin, Some("bad\nkey")).is_err());
}

#[tokio::test]
async fn model_discovery_routes_public_and_project_calls() {
    let body = r#"{"revision":"fixture","models":[{"id":"studio_video","image":false,"text":false,"aspect_ratios":[],"video":[{"durations_seconds":[5],"aspect_ratios":["16:9"],"resolution":"720p"}]}]}"#;
    let (origin, captured) = reply("200 OK", "", body).await;
    let client = TokenCraftClient::new(&format!("{origin}/api"), Some("fixture")).unwrap();
    let models = client.models().await.unwrap();
    assert_eq!(models.models[0].video[0].durations_seconds, [5]);
    assert!(models.models[0].audio.is_empty());
    let (head, _) = captured.await.unwrap();
    assert!(head.starts_with("get /api/v1/catalog/models/scoped "));
    assert!(head.contains("authorization: bearer fixture\r\n"));

    let (origin, captured) = reply("200 OK", "", body).await;
    let client = TokenCraftClient::new(&origin, Some("fixture")).unwrap();
    client.models_public().await.unwrap();
    let (head, _) = captured.await.unwrap();
    assert!(head.starts_with("get /v1/catalog/models "));
    assert!(!head.contains("authorization:"));
}

#[tokio::test]
async fn scoped_quote_uses_bearer_and_preserves_gateway_error() {
    let headers = format!("TokenCraft-Request-Id: {REQUEST}\r\n");
    let (origin, captured) = reply(
        "409 Conflict",
        &headers,
        r#"{"error":{"code":"stale","message":"quote changed"}}"#,
    )
    .await;
    let client = TokenCraftClient::new(&origin, Some("fixture")).unwrap();
    let error = client.quote(&input()).await.unwrap_err();
    assert!(matches!(error, Error::Gateway { ref code, .. } if code == "stale"));
    assert_eq!(error.status().unwrap(), 409);
    assert_eq!(error.request_id(), Some(REQUEST));
    let (head, body) = captured.await.unwrap();
    assert!(head.starts_with("post /v1/quotes/scoped"));
    assert!(head.contains("authorization: bearer fixture\r\n"));
    let encoded: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(encoded["workload"]["aspect_ratio"], "7:5");
}

#[tokio::test]
async fn reads_provider_path_from_scoped_trace() {
    use tokencraft_sdk::{DownstreamProvider, Provider, ProviderAttemptState};

    let body = json!({
        "provider_path": {
            "admitted_offer_id": "openrouter_fixture",
            "attempts": [{
                "offer_id": "openrouter_fixture",
                "provider": "openrouter",
                "state": "succeeded",
                "step_id": "provider",
                "correlation_id": "gen_fixture"
            }],
            "downstream": {"status": "reported", "provider": "Fixture Provider"}
        }
    });
    let (origin, captured) = reply("200 OK", "", &body.to_string()).await;
    let client = TokenCraftClient::new(&origin, Some("fixture")).unwrap();
    let path = client.provider_path(REQUEST).await.unwrap().unwrap();
    let [attempt] = path.attempts.as_slice() else {
        panic!("expected one recorded attempt");
    };
    assert!(matches!(attempt.provider, Some(Provider::OpenRouter)));
    assert!(matches!(attempt.state, ProviderAttemptState::Succeeded));
    assert_eq!(attempt.correlation_id.as_deref(), Some("gen_fixture"));
    assert!(matches!(
        path.downstream,
        Some(DownstreamProvider::Reported { provider }) if provider == "Fixture Provider"
    ));
    let (head, _) = captured.await.unwrap();
    assert!(head.starts_with(&format!("get /v1/requests/{REQUEST}/trace ")));
    assert!(head.contains("authorization: bearer fixture\r\n"));
}

#[tokio::test]
async fn reads_trace_evidence_and_downloads_scoped_otlp() {
    let body = json!({
        "revision": "trace-v1",
        "observed_at": "2026-11-25T18:00:00Z",
        "analysis_available": false,
        "correlation": {"trace_id": "11111111111111111111111111111111", "admission_span_id": "2222222222222222"},
        "provider_path": {
            "admitted_offer_id": "example_custom_image",
            "attempts": [{
                "offer_id": "example_custom_image", "provider": "example-custom", "state": "succeeded", "step_id": "provider",
                "compute_attempts": [{
                    "step_id": "compute-attempt-1",
                    "selection": {"home_cell": "home", "stack": "chat", "target": "standby", "cloud": "aws",
                                  "region": "us-west-2", "zone": "west-b", "priority": 20}
                }]
            }]
        },
        "evidence": {
            "schema_version": 1, "request_id": REQUEST, "status": "succeeded", "subscription_configured": false,
            "delivery_status": null, "delivery_attempts_omitted": 0, "coverage": [],
            "summary": {
                "headline": "Request succeeded.", "next_check": {"label": "Inspect dispatch", "step_id": "compute-attempt-1"},
                "signals": [{"label": "Execution", "value": "Succeeded", "tone": "ok", "step_id": "compute-attempt-1"}]
            },
            "routing": {"admitted_offer_id": "example_custom_image", "attempts": [
                {"offer_id": "example_custom_image", "provider": "example-custom", "state": "succeeded", "step_id": "provider"}
            ]},
            "steps": [{
                "id": "compute-attempt-1", "parent_id": "provider", "name": "Compute attempt 1",
                "component": "compute dispatch", "kind": "interval", "status": "Response started", "tone": "ok",
                "started_at": "2026-11-25T18:00:00Z", "ended_at": "2026-11-25T18:00:01Z",
                "logs": [{"at": "2026-11-25T18:00:00Z", "code": "compute.dispatch_recorded", "message": "Dispatch recorded.", "severity": "active"}],
                "attributes": [{"label": "HTTP status", "value": "200"}]
            }]
        }
    });
    let (origin, captured) = reply("200 OK", "", &body.to_string()).await;
    let client = TokenCraftClient::new(&origin, Some("fixture")).unwrap();
    let trace = client.trace(REQUEST).await.unwrap();
    let path = trace.provider_path.unwrap();
    assert_eq!(path.attempts[0].attempt.provider.as_ref().unwrap().as_str(), "example-custom");
    let target = &path.attempts[0].compute_attempts[0];
    assert_eq!(trace.revision, "trace-v1");
    assert_eq!(trace.evidence.request_id, REQUEST);
    assert_eq!(target.selection.region, "us-west-2");
    assert_eq!(trace.evidence.steps[0].id, target.step_id);
    assert_eq!(trace.evidence.steps[0].logs[0].code, "compute.dispatch_recorded");
    assert_eq!(trace.evidence.summary.unwrap().next_check.step_id, target.step_id);
    assert_eq!(trace.evidence.routing.unwrap().attempts[0].offer_id, "example_custom_image");
    let (head, _) = captured.await.unwrap();
    assert!(head.starts_with(&format!("get /v1/requests/{REQUEST}/trace ")));
    assert!(head.contains("authorization: bearer fixture\r\n"));

    let otlp = json!({"resourceSpans": [{"scopeSpans": [{"spans": [{"startTimeUnixNano": "1795629600000000000"}]}]}]});
    let (origin, captured) = reply("200 OK", "", &otlp.to_string()).await;
    let client = TokenCraftClient::new(&origin, Some("fixture")).unwrap();
    let export = client.trace_otlp(REQUEST).await.unwrap();
    assert_eq!(serde_json::to_value(export).unwrap(), otlp);
    let (head, _) = captured.await.unwrap();
    assert!(head.starts_with(&format!("get /v1/requests/{REQUEST}/trace/otlp ")));
    assert!(head.contains("authorization: bearer fixture\r\n"));
}

#[tokio::test]
async fn preferred_order_reaches_quotes_and_preserves_request_overrides_on_submit() {
    use tokencraft_sdk::{Provider, SubmitOptions};

    let order = json!(["openrouter", "fal"]);
    let document = |source: &str| {
        json!({
            "id": "quo_fixture",
            "catalog_revision": "fixture",
            "input": input(),
            "currency": "USD",
            "selected_offer_id": "openrouter_image",
            "candidates": [],
            "routing": {
                "organization_revision": 0,
                "project_revision": 0,
                "provider_preference": { "source": source, "value": order },
            },
        })
    };
    let (origin, captured) = reply("200 OK", "", &document("request").to_string()).await;
    let client = TokenCraftClient::new(&origin, Some("fixture")).unwrap();
    let explicit = client
        .quote_with_preferred_providers(&input(), &[Provider::OpenRouter, Provider::Fal])
        .await
        .unwrap();
    let (head, body) = captured.await.unwrap();
    assert!(head.starts_with("post /v1/quotes/scoped"));
    let body: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(body["preferred_providers"], order);

    let inherited = serde_json::from_value(document("organization_default")).unwrap();
    for (quote, expected) in [(explicit, Some(&order)), (inherited, None)] {
        let (origin, captured) = reply(
            "409 Conflict",
            "",
            r#"{"error":{"code":"stale","message":"quote changed"}}"#,
        )
        .await;
        let client = TokenCraftClient::new(&origin, Some("fixture")).unwrap();
        assert!(client
            .submit(&quote, SubmitOptions::new("fixture-order"))
            .await
            .is_err());
        let (_, body) = captured.await.unwrap();
        let body: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(body["input"].get("preferred_providers"), expected);
    }
}

#[tokio::test]
async fn exact_offer_pin_reaches_quote_and_submit() {
    use tokencraft_sdk::SubmitOptions;

    let (origin, captured) = reply(
        "200 OK",
        "",
        &json!({
            "id": "quo_pinned",
            "catalog_revision": "fixture",
            "input": input(),
            "currency": "USD",
            "pinned_offer_id": "fal_image",
            "selected_offer_id": "fal_image",
            "candidates": [{
                "offer_id": "fal_image",
                "provider": "fal",
                "reason": "eligible",
                "estimated_usd_nanos": 31_000_000,
                "applied_aspect_ratio": "4:3",
                "artcraft_reference": {
                    "offer_id": "artcraft_fal_image",
                    "revision": "fixture_01",
                    "sha256": "a".repeat(64),
                    "public_key_sha256": "b".repeat(64),
                },
                "rate_evidence": {
                    "source_url": "https://vendor.example/prices",
                    "observed_at": "2026-11-25T00:00:00Z",
                    "valid_until": "2026-11-26T00:00:00Z",
                },
            }],
        })
        .to_string(),
    )
    .await;
    let client = TokenCraftClient::new(&origin, Some("fixture")).unwrap();
    let quote = client
        .quote_offer(&input(), "fal_image", Some("studio_a"))
        .await
        .unwrap();
    let candidate = &quote.candidates[0];
    let preset = candidate.artcraft_reference.as_ref().unwrap();
    assert_eq!(preset.offer_id, "artcraft_fal_image");
    assert_eq!(preset.public_key_sha256.as_deref(), Some("b".repeat(64).as_str()));
    let rate = candidate.rate_evidence.as_ref().unwrap();
    assert_eq!(rate.valid_until, "2026-11-26T00:00:00Z");
    assert_eq!(candidate.estimated_usd_nanos, Some(31_000_000));
    let (head, _) = captured.await.unwrap();
    assert!(head.starts_with("post /v1/quotes/scoped?"));
    assert!(head.contains("pinned_offer_id=fal_image"));
    assert!(head.contains("client_ref=studio_a"));

    let (origin, captured) = reply("409 Conflict", "", "{}").await;
    let client = TokenCraftClient::new(&origin, Some("fixture")).unwrap();
    assert!(client
        .submit(&quote, SubmitOptions::new("fixture-pin"))
        .await
        .is_err());
    let (_, body) = captured.await.unwrap();
    let body: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(body["pinned_offer_id"], "fal_image");
    assert_eq!(body["quote_id"], "quo_pinned");
}

#[tokio::test]
async fn usage_sends_the_app_filter_with_grouping_and_client_scope() {
    use tokencraft_sdk::{UsageGroup, UsageOptions};

    let (origin, captured) = reply("404 Not Found", "", "{}").await;
    let client = TokenCraftClient::new(&origin, Some("fixture")).unwrap();
    let error = client
        .usage_for_app(
            "craft:film",
            UsageOptions {
                client_ref: Some("studio_a"),
                group_by: Some(UsageGroup::App),
                ..Default::default()
            },
        )
        .await
        .unwrap_err();
    assert_eq!(error.status(), Some(reqwest::StatusCode::NOT_FOUND));
    let (head, _) = captured.await.unwrap();
    let target = head.lines().next().unwrap().split_whitespace().nth(1).unwrap();
    let url = reqwest::Url::parse(&format!("{origin}{target}")).unwrap();
    assert_eq!(url.path(), "/v1/usage");
    let query: std::collections::BTreeMap<_, _> = url.query_pairs().into_owned().collect();
    assert_eq!(query.get("client_ref").map(String::as_str), Some("studio_a"));
    assert_eq!(query.get("group_by").map(String::as_str), Some("app"));
    assert_eq!(query.get("app_ref").map(String::as_str), Some("craft:film"));
}

#[tokio::test]
async fn history_sends_trace_and_group_filters_with_client_scope() {
    use tokencraft_sdk::RequestFilters;

    let trace_id = "4bf92f3577b34da6a3ce929d0e0e4736";
    let (origin, captured) = reply("200 OK", "", r#"{"requests":[],"next_after":null}"#).await;
    let client = TokenCraftClient::new(&origin, Some("fixture")).unwrap();
    let page = client.list_filtered(RequestFilters {
        client_ref: Some("studio_a"),
        trace_id: Some(trace_id),
        app_unattributed: Some(true),
        model: Some("studio_image_demo"),
        exact_model: Some(true),
        ..Default::default()
    }).await.unwrap();
    assert!(page.requests.is_empty());

    let (head, _) = captured.await.unwrap();
    let target = head.lines().next().unwrap().split_whitespace().nth(1).unwrap();
    let url = reqwest::Url::parse(&format!("{origin}{target}")).unwrap();
    assert_eq!(url.path(), "/v1/requests");
    let query: std::collections::BTreeMap<_, _> = url.query_pairs().into_owned().collect();
    assert_eq!(query.get("client_ref").map(String::as_str), Some("studio_a"));
    assert_eq!(query.get("trace_id").map(String::as_str), Some(trace_id));
    assert_eq!(query.get("app_unattributed").map(String::as_str), Some("true"));
    assert_eq!(query.get("exact_model").map(String::as_str), Some("true"));
}

#[tokio::test]
async fn redirects_are_returned_without_forwarding_bearer() {
    let target = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let location = format!(
        "Location: http://{}/private\r\n",
        target.local_addr().unwrap()
    );
    let (origin, captured) = reply("302 Found", &location, "{}").await;
    let client =
        TokenCraftClient::with_timeout(&origin, Some("fixture"), Duration::from_secs(1)).unwrap();
    let error = client.quote(&input()).await.unwrap_err();
    assert_eq!(error.status().unwrap(), 302);
    assert!(captured
        .await
        .unwrap()
        .0
        .contains("authorization: bearer fixture\r\n"));
    assert!(timeout(Duration::from_millis(50), target.accept())
        .await
        .is_err());
}

#[tokio::test]
async fn submission_reuses_the_same_wire_intent_after_hinted_failures() {
    use tokencraft_sdk::SubmitOptions;

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let captured = tokio::spawn(async move {
        let unavailable = wire_response(
            "503 Service Unavailable", "Retry-After: Sun Nov  6 08:49:37 1994\r\n", "{}",
        );
        let limited = wire_response("429 Too Many Requests", "Retry-After: 0\r\n", "{}");
        let admitted = wire_response("202 Accepted", "", &json!({
            "id": REQUEST, "project_id": "fixture", "status_url": format!("/v1/requests/{REQUEST}"),
        }).to_string());
        let first = capture_and_reply(&listener, &unavailable).await;
        let second = capture_and_reply(&listener, &limited).await;
        let third = capture_and_reply(&listener, &admitted).await;
        assert_eq!(first, second);
        assert_eq!(first, third);
        first
    });
    let client = TokenCraftClient::with_timeout(&origin, Some("fixture"), Duration::from_secs(2))
        .unwrap().with_submit_max_attempts(3).unwrap().with_app_ref("studio")
        .with_submit_traceparent(TRACEPARENT).unwrap();
    let mut options = SubmitOptions::new("fixture-retry");
    options.client_ref = Some("customer-1");
    options.image_prompt = Some("A violet paper kite");
    assert_eq!(client.submit(&quote_fixture(), options).await.unwrap().id, REQUEST);
    let (head, body) = captured.await.unwrap();
    assert!(head.contains("idempotency-key: fixture-retry\r\n"));
    assert!(head.contains("authorization: bearer fixture\r\n"));
    assert!(head.contains(&format!("traceparent: {TRACEPARENT}\r\n")));
    let body: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(body["pinned_offer_id"], "image_fixture");
    assert_eq!(body["app_ref"], "studio");
    assert_eq!(body["client_ref"], "customer-1");
    assert_eq!(body["image_prompt"], "A violet paper kite");
}

#[tokio::test]
async fn caller_context_is_validated_and_applies_only_to_the_scoped_client_submissions() {
    use tokencraft_sdk::SubmitOptions;

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let catalog = wire_response("200 OK", "", r#"{"revision":"fixture","offers":[]}"#);
        let receipt = wire_response("202 Accepted", "", &json!({
            "id": REQUEST, "project_id": "fixture", "status_url": format!("/v1/requests/{REQUEST}"),
        }).to_string());
        let (read, _) = capture_and_reply(&listener, &catalog).await;
        let (linked, _) = capture_and_reply(&listener, &receipt).await;
        let (unlinked, _) = capture_and_reply(&listener, &receipt).await;
        (read, linked, unlinked)
    });
    let client = TokenCraftClient::new(&origin, Some("fixture")).unwrap();
    let future = TRACEPARENT.replacen("00-", "01-", 1) + "-extension";
    for invalid in [
        TRACEPARENT.replacen("00-", "ff-", 1),
        TRACEPARENT.replace("00f067aa0ba902b7", "0000000000000000"),
        TRACEPARENT.replace("4bf92f3577b34da6a3ce929d0e0e4736", "0".repeat(32).as_str()),
        format!("{TRACEPARENT}, {TRACEPARENT}"),
        format!("{TRACEPARENT}-extra"),
        format!("{future}{}", "a".repeat(256)),
        format!("{TRACEPARENT}\r\nX-Other: discarded"),
    ] {
        assert!(matches!(client.clone().with_submit_traceparent(&invalid), Err(Error::InvalidInput(_))));
    }
    let traced = client.clone().with_submit_traceparent(&future).unwrap();
    traced.catalog().await.unwrap();
    traced.submit(&quote_fixture(), SubmitOptions::new("fixture-linked")).await.unwrap();
    client.submit(&quote_fixture(), SubmitOptions::new("fixture-unlinked")).await.unwrap();
    let (read, linked, unlinked) = server.await.unwrap();
    assert!(!read.contains("traceparent:"));
    assert!(linked.contains(&format!("traceparent: {future}\r\n")));
    assert!(!unlinked.contains("traceparent:"));
    assert!(!linked.contains("tracestate:") && !linked.contains("baggage:"));
}

#[tokio::test]
async fn submission_retries_require_opt_in_an_unambiguous_hint_and_remaining_attempts() {
    use tokencraft_sdk::SubmitOptions;

    let problem = r#"{"error":{"code":"unavailable","message":"try again"}}"#;
    for (attempts, status, hint, expected) in [
        (1, "503 Service Unavailable", "Retry-After: 0\r\n", 503),
        (3, "503 Service Unavailable", "", 503),
        (3, "503 Service Unavailable", "Retry-After: 0\r\nRetry-After: 1\r\n", 503),
        (3, "503 Service Unavailable", "Retry-After: Sun, 06 Nov 1994 08:49:37 GMT, 0\r\n", 503),
        (3, "409 Conflict", "Retry-After: 0\r\n", 409),
    ] {
        let (origin, captured) = reply(status, hint, problem).await;
        let client = TokenCraftClient::new(&origin, Some("fixture")).unwrap();
        let client = if attempts == 1 { client } else {
            client.with_submit_max_attempts(attempts).unwrap()
        };
        let error = client.submit(&quote_fixture(), SubmitOptions::new("fixture-err"))
            .await.unwrap_err();
        assert!(matches!(error, Error::Gateway { status, code, .. }
            if status.as_u16() == expected && code == "unavailable"));
        captured.await.unwrap();
    }

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let first = wire_response(
            "503 Service Unavailable",
            &format!("Retry-After: 0\r\nTokenCraft-Request-Id: {REQUEST}\r\n"),
            problem,
        );
        let response = wire_response("503 Service Unavailable", "Retry-After: 0\r\n", problem);
        capture_and_reply(&listener, &first).await;
        capture_and_reply(&listener, &response).await;
        assert!(timeout(Duration::from_millis(30), listener.accept()).await.is_err());
    });
    let client = TokenCraftClient::with_timeout(&origin, Some("fixture"), Duration::from_secs(2))
        .unwrap().with_submit_max_attempts(2).unwrap();
    let error = client.submit(&quote_fixture(), SubmitOptions::new("fixture-err"))
        .await.unwrap_err();
    assert_eq!(error.request_id(), Some(REQUEST));
    assert!(matches!(error, Error::Gateway { code, .. } if code == "unavailable"));
    server.await.unwrap();
}

#[tokio::test]
async fn submission_retry_sleep_preserves_the_deadline_and_request_id() {
    use std::time::SystemTime;
    use tokencraft_sdk::SubmitOptions;

    let later = httpdate::fmt_http_date(SystemTime::now() + Duration::from_secs(30));
    let headers = format!("Retry-After: {later}\r\nTokenCraft-Request-Id: {REQUEST}\r\n");
    let (origin, captured) = reply("503 Service Unavailable", &headers, "{}").await;
    let client = TokenCraftClient::with_timeout(&origin, Some("fixture"), Duration::from_millis(100))
        .unwrap().with_submit_max_attempts(3).unwrap();
    let error = timeout(Duration::from_secs(2),
        client.submit(&quote_fixture(), SubmitOptions::new("fixture-later")))
        .await.unwrap().unwrap_err();
    assert!(matches!(error, Error::Timeout { request_id: Some(id) } if id == REQUEST));
    captured.await.unwrap();
}

#[tokio::test]
async fn wait_reuses_validator_through_rollout_and_respects_poll_hint() {
    use std::time::Instant;

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let trace = |status| {
        json!({
            "id": REQUEST, "project_id": "fixture", "status": status, "events": [],
            "quote": {"id": "quo_fixture", "catalog_revision": "fixture", "input": input(),
                      "currency": "USD", "selected_offer_id": null, "candidates": []}
        })
        .to_string()
    };
    let captured = tokio::spawn(async move {
        let queued = wire_response("200 OK", "ETag: W/\"queued\"\r\nRetry-After: 1\r\n", &trace("queued"));
        let (first, _) = capture_and_reply(&listener, &queued).await;
        let started = Instant::now();
        let rollout = wire_response(
            "503 Service Unavailable", "Retry-After: Sunday, 06-Nov-94 08:49:37 GMT\r\nETag: \"invalidated\"\r\n", "{}",
        );
        let (second, _) = capture_and_reply(&listener, &rollout).await;
        let elapsed = started.elapsed();
        let no_change = wire_response("304 Not Modified", "Retry-After: Sun Nov  6 08:49:37 1994\r\n", "");
        let (third, _) = capture_and_reply(&listener, &no_change).await;
        let succeeded = wire_response("200 OK", "", &trace("succeeded"));
        let (fourth, _) = capture_and_reply(&listener, &succeeded).await;
        (first, [second, third, fourth], elapsed)
    });
    let client = TokenCraftClient::new(&origin, Some("fixture")).unwrap();
    let result = client.wait(REQUEST, WaitOptions {
        timeout: Duration::from_secs(4),
        poll_interval: Duration::from_millis(5),
    }).await.unwrap();
    assert_eq!(result.status, RequestStatus::Succeeded);
    let (first, later, elapsed) = captured.await.unwrap();
    assert!(!first.contains("if-none-match:"));
    for head in later {
        assert!(head.contains("if-none-match: w/\"queued\""));
    }
    assert!(elapsed >= Duration::from_millis(900));
}

#[tokio::test]
async fn wait_preserves_request_id_when_a_read_stalls() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let client = TokenCraftClient::new(
        &format!("http://{}", listener.local_addr().unwrap()),
        Some("fixture"),
    )
    .unwrap();
    let receiver = tokio::spawn(async move {
        let (_socket, _) = listener.accept().await.unwrap();
        std::future::pending::<()>().await;
    });
    let result = client
        .wait(
            REQUEST,
            WaitOptions {
                timeout: Duration::from_millis(100),
                poll_interval: Duration::from_millis(5),
            },
        )
        .await;
    receiver.abort();
    assert!(matches!(result, Err(Error::Timeout { request_id: Some(id) }) if id == REQUEST));
}

#[tokio::test]
async fn unconfirmed_provider_outcome_returns_to_the_caller() {
    let body = json!({
        "id": REQUEST, "project_id": "project", "status": "awaiting_reconciliation",
        "quote": { "id": "quote", "catalog_revision": "revision", "input": input(), "currency": "USD",
            "candidates": [] },
        "events": [ { "kind": "provider_outcome_unconfirmed", "sequence": 3, "at": "2026-11-24T12:00:00Z" } ],
    });
    let (origin, captured) = reply("200 OK", "", &body.to_string()).await;
    let client = TokenCraftClient::new(&origin, Some("fixture")).unwrap();
    let request = client.wait(REQUEST, WaitOptions::default()).await.unwrap();
    assert_eq!(request.status, RequestStatus::AwaitingReconciliation);
    assert!(!request.status.is_terminal());
    assert!(captured
        .await
        .unwrap()
        .0
        .starts_with(&format!("get /v1/requests/{REQUEST}")));
}

#[tokio::test]
async fn retained_history_reads_metadata_and_contents_on_separate_routes() {
    use tokencraft_sdk::{ContentPhase, RestoreState, RetainedResult};

    let metadata = json!({
        "phase": "archived", "media": true, "archive_at": null, "expires_at": "2027-12-25T00:00:00Z",
        "restore": {"state": "pending", "ready_objects": 1, "total_objects": 3,
                    "requested_at": "2026-12-25T00:00:00Z", "retry_at": "2026-12-26T00:00:00Z"}
    });
    let (origin, capture) = reply("200 OK", "", &metadata.to_string()).await;
    let client = TokenCraftClient::new(&format!("{origin}/edge/"), Some("fixture")).unwrap();
    let history = client.content_metadata(REQUEST).await.unwrap();
    assert_eq!(history.phase, ContentPhase::Archived);
    assert_eq!(history.restore.unwrap().state, RestoreState::Pending);
    let (head, _) = capture.await.unwrap();
    assert!(head.starts_with(&format!("get /edge/v1/requests/{REQUEST}/content?metadata_only=true ")));
    assert!(head.contains("authorization: bearer fixture\r\n"));
    assert!(head.contains("cache-control: no-store\r\n"));

    let chat = json!({"phase": "available", "input": {"messages": [{"content": "Synthetic input"}]},
                      "result": {"text": "Synthetic reply"}, "archive_at": null, "expires_at": null});
    let (origin, capture) = reply("200 OK", "", &chat.to_string()).await;
    let client = TokenCraftClient::new(&origin, Some("fixture")).unwrap();
    let saved = client.warm_content(REQUEST).await.unwrap();
    assert!(!saved.metadata.media);
    assert!(matches!(saved.result, Some(RetainedResult::Chat(value)) if value.text == "Synthetic reply"));
    assert_eq!(saved.input.unwrap()["messages"][0]["content"], "Synthetic input");
    assert!(capture.await.unwrap().0.starts_with(&format!("get /v1/requests/{REQUEST}/content ")));

    let (origin, capture) = reply("409 Conflict", "", r#"{"error":{"code":"archive_retrieval_required"}}"#).await;
    let client = TokenCraftClient::new(&origin, Some("fixture")).unwrap();
    let error = client.retrieve_content(REQUEST).await.err().unwrap();
    assert!(matches!(error, Error::Gateway { ref code, .. } if code == "archive_retrieval_required"));
    assert_eq!(error.request_id(), Some(REQUEST));
    let (head, body) = capture.await.unwrap();
    assert!(head.starts_with(&format!("post /v1/requests/{REQUEST}/content/retrieve ")));
    assert!(body.is_empty());
}

#[tokio::test]
async fn artifact_download_checks_slot_size_digest_and_scoped_gateway_errors() {
    use sha2::{Digest, Sha256};
    use tokencraft_sdk::{ArtifactLocation, ArtifactRef};

    let reference = ArtifactRef { slot: 1, byte_length: 3, sha256: format!("{:x}", Sha256::digest(b"abc")) };
    let body = |headers: &str, framed: &str| {
        format!("HTTP/1.1 200 OK\r\nContent-Type: image/png\r\nX-TokenCraft-Content-Sha256: {}\r\nConnection: close\r\n{headers}\r\n{framed}", reference.sha256)
    };
    for (location, path) in [(ArtifactLocation::Warm, "artifacts"), (ArtifactLocation::Archived, "content/artifacts")] {
        let (origin, capture) = reply_wire(body("Content-Length: 3\r\n", "abc")).await;
        let client = TokenCraftClient::new(&origin, Some("fixture")).unwrap();
        let verified = client.download_artifact(REQUEST, &reference, location).await.unwrap();
        assert_eq!(verified.content_type, "image/png");
        assert_eq!(verified.bytes, b"abc");
        let (head, _) = capture.await.unwrap();
        assert!(head.starts_with(&format!("get /v1/requests/{REQUEST}/{path}/1 ")));
        assert!(head.contains("accept: application/octet-stream\r\n"));
    }
    for response in [body("Content-Length: 3\r\n", "abd"),
                     body("Transfer-Encoding: chunked\r\n", "4\r\nabcd\r\n0\r\n\r\n"),
                     body("Content-Length: 2\r\n", "ab")] {
        let (origin, capture) = reply_wire(response).await;
        let client = TokenCraftClient::new(&origin, Some("fixture")).unwrap();
        let error = client.download_artifact(REQUEST, &reference, ArtifactLocation::Archived).await.err().unwrap();
        assert!(matches!(error, Error::Gateway { ref code, .. } if code == "invalid_artifact"));
        assert_eq!(error.request_id(), Some(REQUEST));
        capture.await.unwrap();
    }
    let (origin, capture) = reply("404 Not Found", "", r#"{"error":{"code":"not_found"}}"#).await;
    let client = TokenCraftClient::new(&origin, Some("fixture")).unwrap();
    let invalid = ArtifactRef { slot: 8, ..reference.clone() };
    assert!(matches!(client.download_artifact(REQUEST, &invalid, ArtifactLocation::Archived).await, Err(Error::InvalidInput(_))));
    let error = client.download_artifact(REQUEST, &reference, ArtifactLocation::Archived).await.err().unwrap();
    assert_eq!(error.status().unwrap(), 404);
    capture.await.unwrap();
}
