use std::collections::BTreeSet;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use axum::{body::Bytes, http::StatusCode, routing::post, Router};
use serde_json::{json, Value};
use tokio::{net::TcpListener, task::JoinHandle};

use super::{parse, tenant_https_endpoint, Binding, ChatClient};
use crate::gateway_demo::chat_input::{ChatInput, ChatOptions};
use crate::gateway_demo::providers::chat_transport::{ChatCall, ChatDispatch, SubmitOutcome};

const SUCCESS: &str =
    r#"{"id":"completion-1","choices":[{"finish_reason":"stop","message":{"content":"ready"}}]}"#;

#[test]
fn tenant_chat_transport_rejects_untrusted_endpoint_shapes() {
    assert!(tenant_https_endpoint("https://connector.example/v1/chat/completions").is_ok());
    for endpoint in [
        "http://connector.example/v1/chat/completions",
        "https://user@connector.example/v1/chat/completions",
        "https://connector.example/v1/chat/completions?target=example.test",
        "https://connector.example/v1/chat/completions#extra",
        "https://connector.example/anything-else",
    ] {
        assert!(tenant_https_endpoint(endpoint).is_err());
    }
}

fn target(id: &str, port: u16, priority: u8) -> Value {
    json!({
        "id": id, "location": {"cloud": "local", "region": "dev"}, "zone": id,
        "priority": priority, "endpoint": format!("http://127.0.0.1:{port}/v1/chat/completions"),
    })
}

fn config(primary: Value, secondary: Value) -> Value {
    json!({
        "version": 1, "cell": "dev-cell", "bindings": [{
            "name": "workstation", "allowed_locations": [{ "cloud": "local", "region": "dev" }],
            "targets": [primary, secondary],
        }],
    })
}

fn binding(config: &Value) -> anyhow::Result<Binding<ChatClient>> {
    let names = BTreeSet::from(["workstation"]);
    Ok(parse(&serde_json::to_vec(config)?, &names)?
        .remove("workstation")
        .unwrap())
}

#[test]
fn compute_topology_is_canonical_and_placement_is_checked() {
    let mut settings = config(target("a", 9001, 10), target("b", 9002, 20));
    let original = binding(&settings).unwrap().digest;
    settings["bindings"][0]["targets"]
        .as_array_mut()
        .unwrap()
        .reverse();
    assert_eq!(original, binding(&settings).unwrap().digest);
    settings["bindings"][0]["targets"][0]["priority"] = json!(5);
    assert_ne!(original, binding(&settings).unwrap().digest);
    settings["bindings"][0]["targets"][0]["location"]["region"] = json!("other");
    assert!(binding(&settings).is_err());
    settings["bindings"][0]["targets"][0]["location"]["region"] = json!("dev");
    settings["bindings"][0]["targets"][0]["endpoint"] =
        json!("https://example.invalid/v1/chat/completions");
    assert!(binding(&settings).is_err());
}

struct Fixture {
    port: u16,
    hits: Arc<AtomicUsize>,
    task: JoinHandle<()>,
}

impl Fixture {
    async fn start(status: StatusCode, body: &'static str) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let hits = Arc::new(AtomicUsize::new(0));
        let count = Arc::clone(&hits);
        let app = Router::new().route(
            "/v1/chat/completions",
            post(move |input: Bytes| {
                let count = Arc::clone(&count);
                async move {
                    let input = serde_json::from_slice(&input).unwrap();
                    provider_mapping::Registry::builtin()
                        .unwrap()
                        .validate_input("text", &input)
                        .unwrap();
                    count.fetch_add(1, Ordering::SeqCst);
                    (status, body)
                }
            }),
        );
        let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Self { port, hits, task }
    }

    fn requests(&self) -> usize {
        self.hits.load(Ordering::SeqCst)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn call(client: &ChatClient, request_id: &str) -> SubmitOutcome {
    let input = ChatInput {
        messages: vec![serde_json::from_value(json!({"role": "user", "content": "test"})).unwrap()],
        max_tokens: 32,
        options: ChatOptions {
            temperature: Some(0.5),
            top_p: Some(0.9),
        },
        stream: false,
    };
    client
        .submit_for_run(ChatCall {
            tenant_key: None,
            model: "compute-model",
            input: &input,
            stream: false,
            dispatch: Some(ChatDispatch {
                pool: None,
                request_id,
                trace_id: None,
                lease: chrono::Utc::now() + chrono::Duration::seconds(40),
            }),
        })
        .await
}

#[tokio::test]
async fn compute_fails_over_on_connection_refused() {
    let primary = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let secondary = Fixture::start(StatusCode::OK, SUCCESS).await;
    let client = binding(&config(
        target("a", primary.local_addr().unwrap().port(), 10),
        target("b", secondary.port, 20),
    ))
    .unwrap()
    .client;
    drop(primary);
    assert!(matches!(
        call(&client, "one").await,
        SubmitOutcome::Completed(_)
    ));
    assert_eq!(secondary.requests(), 1);
}

#[tokio::test]
async fn compute_does_not_retry_ambiguous_results_and_uses_capacity_contracts() {
    let secondary = Fixture::start(StatusCode::OK, SUCCESS).await;
    for (status, body) in [
        (StatusCode::SERVICE_UNAVAILABLE, "unavailable"),
        (StatusCode::OK, "invalid response"),
        (StatusCode::TOO_MANY_REQUESTS, "rate limited"),
    ] {
        let primary = Fixture::start(status, body).await;
        let client = binding(&config(
            target("a", primary.port, 10),
            target("b", secondary.port, 20),
        ))
        .unwrap()
        .client;
        let outcome = call(&client, "one").await;
        if status == StatusCode::TOO_MANY_REQUESTS {
            assert!(matches!(
                outcome,
                SubmitOutcome::Rejected("provider_rejected_request")
            ));
        } else {
            assert!(matches!(outcome, SubmitOutcome::Uncertain(_)));
        }
        assert_eq!(primary.requests(), 1);
        assert_eq!(secondary.requests(), 0);
    }

    for (status, initial_requests) in [
        (StatusCode::SERVICE_UNAVAILABLE, 0),
        (StatusCode::TOO_MANY_REQUESTS, 1),
    ] {
        let primary = Fixture::start(status, "capacity unavailable").await;
        let secondary = Fixture::start(StatusCode::OK, SUCCESS).await;
        let mut first = target("a", primary.port, 10);
        first["retry_on_rate_limit"] = json!(true);
        let client = binding(&config(first, target("b", secondary.port, 20)))
            .unwrap()
            .client;
        let _ = call(&client, "first").await;
        assert_eq!(secondary.requests(), initial_requests);
        assert!(matches!(
            call(&client, "new").await,
            SubmitOutcome::Completed(_)
        ));
        assert_eq!(primary.requests(), 1);
        assert_eq!(secondary.requests(), initial_requests + 1);
    }
}
