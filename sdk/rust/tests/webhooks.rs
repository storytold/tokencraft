use base64::{engine::general_purpose::STANDARD, Engine};
use hmac::{Hmac, Mac};
use reqwest::header::HeaderMap;
use sha2::Sha256;
use tokencraft_sdk::{
    verify_webhook, verify_workflow_webhook, VerificationError, WebhookKind, WebhookOptions,
    WorkflowWebhookKind, WorkflowWebhookStatus,
};

const ID: &str = "whd_01m881w0c8np13xfvetqsbk8m0";
const REQUEST: &str = "req_01m881w0c8np13xfvetqsbk8m0";
const NOW: u64 = 1_795_565_000;

fn sign(key: &[u8; 32], body: &[u8]) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(key).unwrap();
    mac.update(format!("{ID}.{NOW}.").as_bytes());
    mac.update(body);
    format!("v1,{}", STANDARD.encode(mac.finalize().into_bytes()))
}

#[test]
fn verifies_raw_bytes_with_both_keys_and_enforces_envelope_and_time() {
    let event = serde_json::json!({
        "id": ID,
        "schema_version": 1,
        "type": "request.succeeded",
        "occurred_at": "2026-11-24T12:00:00Z",
        "sequence": 3,
        "data": { "request_id": REQUEST, "provider": "kinovi" },
    });
    let body = serde_json::to_vec_pretty(&event).unwrap();
    let old = [22; 32];
    let new = [41; 32];
    let old_secret = format!("whsec_{}", STANDARD.encode(old));
    let new_secret = format!("whsec_{}", STANDARD.encode(new));
    let options = WebhookOptions {
        now_seconds: Some(NOW),
        ..Default::default()
    };
    let mut headers = HeaderMap::new();
    headers.insert("webhook-id", ID.parse().unwrap());
    headers.insert("webhook-timestamp", NOW.to_string().parse().unwrap());
    headers.insert(
        "webhook-signature",
        format!("{} {}", sign(&new, &body), sign(&old, &body))
            .parse()
            .unwrap(),
    );
    for secret in [&old_secret, &new_secret] {
        let verified = verify_webhook(&headers, &body, &[secret], options).unwrap();
        assert_eq!(verified.kind, WebhookKind::Succeeded);
        assert_eq!(verified.data.request_id, REQUEST);
        assert_eq!(verified.data.attributes["provider"], "kinovi");
    }

    let normalized = serde_json::to_vec(&event).unwrap();
    assert_eq!(
        verify_webhook(&headers, &normalized, &[&old_secret], options).unwrap_err(),
        VerificationError::Signature
    );
    let expired = WebhookOptions {
        now_seconds: Some(NOW + 301),
        ..options
    };
    assert_eq!(
        verify_webhook(&headers, &body, &[&new_secret], expired).unwrap_err(),
        VerificationError::Timestamp
    );
    headers.append("webhook-id", ID.parse().unwrap());
    assert_eq!(
        verify_webhook(&headers, &body, &[&new_secret], options).unwrap_err(),
        VerificationError::Headers
    );
}

#[test]
fn verifies_workflow_notifications_without_changing_request_events() {
    let run = REQUEST.replace("req_", "wfr_");
    let workflow = REQUEST.replace("req_", "wf_");
    let project = REQUEST.replace("req_", "proj_");
    let url = format!("/v1/projects/{project}/workflow-endpoints/{workflow}/runs/{run}");
    let mut event = serde_json::json!({
        "id": ID, "schema_version": 1, "type": "workflow.run.canceled",
        "occurred_at": "2026-11-24T12:00:00Z", "sequence": 4,
        "data": {"run_id": run, "workflow_id": workflow, "project_id": project,
                 "workflow_version": 1, "status": "canceled", "fetch_url": url},
    });
    let key = [22; 32];
    let secret = format!("whsec_{}", STANDARD.encode(key));
    let options = WebhookOptions { now_seconds: Some(NOW), ..Default::default() };
    let mut headers = HeaderMap::new();
    headers.insert("webhook-id", ID.parse().unwrap());
    headers.insert("webhook-timestamp", NOW.to_string().parse().unwrap());
    let body = serde_json::to_vec_pretty(&event).unwrap();
    headers.insert("webhook-signature", sign(&key, &body).parse().unwrap());
    let verified = verify_workflow_webhook(&headers, &body, &[&secret], options).unwrap();
    assert_eq!(verified.kind, WorkflowWebhookKind::Canceled);
    assert_eq!(verified.data.status, WorkflowWebhookStatus::Canceled);
    assert_eq!(verified.data.fetch_url, url);
    assert_eq!(verify_webhook(&headers, &body, &[&secret], options).unwrap_err(), VerificationError::Payload);

    event["data"]["status"] = "succeeded".into();
    let mismatched = serde_json::to_vec(&event).unwrap();
    headers.insert("webhook-signature", sign(&key, &mismatched).parse().unwrap());
    assert_eq!(
        verify_workflow_webhook(&headers, &mismatched, &[&secret], options).unwrap_err(),
        VerificationError::Payload
    );
}
