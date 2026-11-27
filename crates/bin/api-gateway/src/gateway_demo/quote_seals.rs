use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use chrono::{DateTime, Utc};
use db_queries::gateway_requests::RequestScope;
use ring::hmac;

use super::quote::{Quote, Reason};
use crate::error::{ApiError, ApiResult};

const DOMAIN: &[u8] = b"tokencraft/scoped-quote/v2";
// Keep the wire format recognizable for accepted requests admitted with an earlier seal.
const PREFIX: &str = "quo_s1_";
const TTL_SECONDS: i64 = 300;
const SEAL_BYTES: usize = 72;
pub(super) const MAX_QUOTE_ID_BYTES: usize = PREFIX.len() + 96;

pub(super) struct QuoteSeals(hmac::Key);

impl QuoteSeals {
    pub(super) fn new(root: &str) -> Self {
        let root = hmac::Key::new(hmac::HMAC_SHA256, root.as_bytes());
        let key = hmac::sign(&root, DOMAIN);
        Self(hmac::Key::new(hmac::HMAC_SHA256, key.as_ref()))
    }

    pub(super) fn seal(&self, quote: &mut Quote, scope: RequestScope<'_>) -> ApiResult<()> {
        let now = Utc::now().timestamp();
        let expires = quote.candidates.iter()
            .filter(|candidate| candidate.reason == Reason::Eligible)
            .filter_map(|candidate| candidate.rate_evidence.as_ref().map(|rate| rate.valid_until.timestamp()))
            .fold(now + TTL_SECONDS, i64::min);
        if expires <= now {
            return Err(ApiError::QuoteStale);
        }
        let digest = quote.id.strip_prefix("quo_")
            .ok_or_else(|| ApiError::Internal("invalid quote fingerprint".into()))?;
        let mut bytes = [0; SEAL_BYTES];
        bytes[..8].copy_from_slice(&expires.to_be_bytes());
        hex::decode_to_slice(digest, &mut bytes[8..40])
            .map_err(|_| ApiError::Internal("invalid quote fingerprint".into()))?;
        let message = binding(&bytes[..40], scope);
        let tag = hmac::sign(&self.0, &message);
        bytes[40..].copy_from_slice(tag.as_ref());
        quote.id = format!("{PREFIX}{}", URL_SAFE_NO_PAD.encode(bytes));
        quote.expires_at = DateTime::from_timestamp(expires, 0);
        Ok(())
    }

    pub(super) fn verify(
        &self,
        id: &str,
        quote: &Quote,
        scope: RequestScope<'_>,
        now: DateTime<Utc>,
    ) -> ApiResult<DateTime<Utc>> {
        let bytes = decode(id).ok_or(ApiError::QuoteStale)?;
        let (proof, tag) = bytes.split_at(40);
        hmac::verify(&self.0, &binding(proof, scope), tag).map_err(|_| ApiError::QuoteStale)?;
        let expires = i64::from_be_bytes(bytes[..8].try_into().expect("eight-byte expiration"));
        let expires = DateTime::from_timestamp(expires, 0).ok_or(ApiError::QuoteStale)?;
        let mut digest = [0; 32];
        let matches = quote.id.strip_prefix("quo_")
            .is_some_and(|id| hex::decode_to_slice(id, &mut digest).is_ok() && digest == bytes[8..40]);
        if !matches || now >= expires {
            return Err(ApiError::QuoteStale);
        }
        Ok(expires)
    }
}

pub(super) fn is_sealed(id: &str) -> bool {
    id.starts_with(PREFIX)
}

pub(super) fn valid_id(id: &str) -> bool {
    decode(id).is_some() || id.strip_prefix("quo_")
        .is_some_and(|digest| digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit()))
}

fn decode(id: &str) -> Option<[u8; SEAL_BYTES]> {
    let encoded = id.strip_prefix(PREFIX)?;
    if id.len() != MAX_QUOTE_ID_BYTES {
        return None;
    }
    URL_SAFE_NO_PAD.decode(encoded).ok()?.try_into().ok()
}

fn binding(proof: &[u8], scope: RequestScope<'_>) -> Vec<u8> {
    let mut message = proof.to_vec();
    for field in [
        scope.account.as_str(),
        scope.project.as_str(),
        scope.client_ref.unwrap_or_default(),
    ] {
        message.extend_from_slice(&(field.len() as u64).to_be_bytes());
        message.extend_from_slice(field.as_bytes());
    }
    message
}

#[cfg(test)]
mod tests {
    use ids::{AccountId, GatewayProjectId};
    use serde_json::json;

    use super::*;

    fn quote() -> Quote {
        serde_json::from_value(json!({
            "id": format!("quo_{}", "22".repeat(32)),
            "catalog_revision": "test",
            "currency": "USD",
            "input":{"model": "test", "workload":{"kind":"image","count":1,"aspect_ratio":"1:1"}},
            "selected_offer_id": null,
            "candidates": [],
        })).unwrap()
    }

    #[test]
    fn validates_scope_fingerprint_signature_and_deadline() {
        let seals = QuoteSeals::new("a-synthetic-fixture-secret");
        let account = AccountId::generate();
        let project = GatewayProjectId::generate();
        let different_project = GatewayProjectId::generate();
        let scope = RequestScope { account: &account, project: &project, client_ref: None };
        let mut sealed = quote();
        seals.seal(&mut sealed, scope).unwrap();
        let expires = sealed.expires_at.unwrap();
        let before = expires - chrono::Duration::seconds(1);
        let original = quote();
        assert_eq!(seals.verify(&sealed.id, &original, scope, before).unwrap(), expires);
        assert!(matches!(seals.verify(&sealed.id, &original, scope, expires), Err(ApiError::QuoteStale)));
        assert!(seals.verify(&sealed.id, &original, RequestScope {
            project: &different_project, ..scope
        }, before).is_err());
        let mut different = quote();
        different.id = format!("quo_{}", "33".repeat(32));
        assert!(seals.verify(&sealed.id, &different, scope, before).is_err());
        let mut bad = decode(&sealed.id).unwrap();
        bad[0] ^= 1;
        assert!(seals.verify(&format!("{PREFIX}{}", URL_SAFE_NO_PAD.encode(bad)), &original, scope, before).is_err());
    }

    #[test]
    fn binds_each_client_and_the_unattributed_scope() {
        let seals = QuoteSeals::new("a-synthetic-fixture-secret");
        let account = AccountId::generate();
        let project = GatewayProjectId::generate();
        let project_scope = RequestScope { account: &account, project: &project, client_ref: None };
        let client = RequestScope { client_ref: Some("client-a"), ..project_scope };
        let sibling = RequestScope { client_ref: Some("client-b"), ..project_scope };
        let mut client_quote = quote();
        seals.seal(&mut client_quote, client).unwrap();
        let before = client_quote.expires_at.unwrap() - chrono::Duration::seconds(1);
        assert!(seals.verify(&client_quote.id, &quote(), client, before).is_ok());
        for other in [sibling, project_scope] {
            assert!(matches!(seals.verify(&client_quote.id, &quote(), other, before), Err(ApiError::QuoteStale)));
        }
        let mut project_quote = quote();
        seals.seal(&mut project_quote, project_scope).unwrap();
        assert!(seals.verify(&project_quote.id, &quote(), project_scope, before).is_ok());
        assert!(matches!(seals.verify(&project_quote.id, &quote(), client, before), Err(ApiError::QuoteStale)));
    }
}
