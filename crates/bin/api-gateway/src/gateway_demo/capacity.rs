use std::sync::Arc;

use axum::{
    http::{header::AUTHORIZATION, HeaderMap, StatusCode},
    Extension, Json,
};
use hmac::{Hmac, Mac};
use serde::Serialize;
use sha2::Sha256;

use super::{
    process::ProcessRole,
    providers::{ProviderRuntime, MAX_PROVIDER_TASKS, MAX_SSE_READERS},
    readiness::{Readiness, SetupChecks},
};

type ProbeMac = Hmac<Sha256>;

pub(super) struct Access {
    expected: [u8; 32],
    instance_id: String,
}

impl Access {
    pub(super) fn load() -> anyhow::Result<Option<Arc<Self>>> {
        let Ok(value) = std::env::var("TOKENCRAFT_CAPACITY_TOKEN") else {
            anyhow::ensure!(
                std::env::var_os("TOKENCRAFT_CAPACITY_TOKEN").is_none(),
                "invalid capacity token encoding"
            );
            return Ok(None);
        };
        anyhow::ensure!(
            value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit()),
            "TOKENCRAFT_CAPACITY_TOKEN must be a generated 32-byte hex value"
        );
        let mut expected = [0; 32];
        expected.copy_from_slice(&Self::mac(&value).finalize().into_bytes());
        Ok(Some(Arc::new(Self {
            expected,
            instance_id: hex::encode(rand::random::<[u8; 16]>()),
        })))
    }

    fn mac(token: &str) -> ProbeMac {
        let mut mac = ProbeMac::new_from_slice(b"TokenCraft capacity probe v1")
            .expect("HMAC accepts fixed keys");
        mac.update(token.as_bytes());
        mac
    }

    fn authorizes(&self, headers: &HeaderMap) -> bool {
        headers.get_all(AUTHORIZATION).iter().count() == 1
            && headers
                .get(AUTHORIZATION)
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.strip_prefix("Bearer "))
                .is_some_and(|value| {
                    value.len() == 64 && Self::mac(value).verify_slice(&self.expected).is_ok()
                })
    }
}

#[derive(Serialize)]
pub(super) struct Signals {
    role: &'static str,
    instance_id: String,
    ready: bool,
    checks: SetupChecks,
    sse_readers: Option<usize>,
    sse_capacity: Option<usize>,
    provider_tasks: Option<usize>,
    provider_capacity: Option<usize>,
    local_bindings: Vec<String>,
    connector_bindings: Vec<String>,
    poll_bindings: Vec<String>,
    lookup_bindings: Vec<String>,
}

pub(super) async fn inspect(
    Extension(access): Extension<Arc<Access>>,
    Extension(readiness): Extension<Arc<Readiness>>,
    Extension(providers): Extension<Option<Arc<ProviderRuntime>>>,
    headers: HeaderMap,
) -> Result<Json<Signals>, StatusCode> {
    if !access.authorizes(&headers) {
        return Err(StatusCode::UNAUTHORIZED);
    }

    let role = readiness.role();
    let streams = providers.as_ref().filter(|_| role.serves_api());
    let worker = providers.as_ref().filter(|_| role.runs_workers());
    Ok(Json(Signals {
        role: match role {
            ProcessRole::Combined => "combined",
            ProcessRole::Api => "api",
            ProcessRole::Worker => "worker",
        },
        instance_id: access.instance_id.clone(),
        ready: readiness.traffic_ready(),
        checks: readiness.snapshot(true),
        sse_readers: streams
            .map(|runtime| MAX_SSE_READERS.saturating_sub(runtime.streams.available_permits())),
        sse_capacity: streams.map(|_| MAX_SSE_READERS),
        provider_tasks: worker.map(|_| readiness.provider_tasks()),
        provider_capacity: worker.map(|_| MAX_PROVIDER_TASKS),
        local_bindings: worker
            .map(|runtime| runtime.local_binding_digests().map(str::to_owned).collect())
            .unwrap_or_default(),
        connector_bindings: worker
            .map(|runtime| runtime.connector_binding_digests().map(str::to_owned).collect())
            .unwrap_or_default(),
        poll_bindings: worker
            .map(|runtime| runtime.poll_binding_digests().map(str::to_owned).collect())
            .unwrap_or_default(),
        lookup_bindings: worker
            .map(|runtime| runtime.lookup_binding_digests().map(str::to_owned).collect())
            .unwrap_or_default(),
    }))
}
