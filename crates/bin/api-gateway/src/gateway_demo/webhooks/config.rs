use std::collections::{BTreeMap, HashMap};
use std::io::Read;
use std::time::Duration;

use anyhow::{anyhow, ensure, Context};
use base64::{engine::general_purpose::STANDARD, Engine};
use hmac::{Hmac, Mac};
use reqwest::{Certificate, Client, Url};
use serde::Deserialize;
use sha2::Sha256;

use super::super::catalog::valid_identifier;

type HmacSha256 = Hmac<Sha256>;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FileConfig {
    destinations: BTreeMap<String, String>,
}

pub struct WebhookConfig {
    destinations: BTreeMap<String, Url>,
    root_key: [u8; 32],
    pub client: Client,
}

impl WebhookConfig {
    pub fn load(hosted: bool) -> anyhow::Result<Option<Self>> {
        let config_path = std::env::var_os("TOKENCRAFT_DEMO_WEBHOOK_CONFIG");
        let trust = std::env::var_os("TOKENCRAFT_WEBHOOK_CA_FILE");
        ensure!(
            trust.is_none() || hosted && config_path.is_some(),
            "TOKENCRAFT_WEBHOOK_CA_FILE requires hosted webhook configuration"
        );
        let Some(path) = config_path else {
            return Ok(None);
        };
        let mut bytes = Vec::new();
        std::fs::File::open(path)
            .context("open TokenCraft webhook configuration")?
            .take(32_769)
            .read_to_end(&mut bytes)?;
        ensure!(
            bytes.len() <= 32_768,
            "webhook configuration exceeds 32 KiB"
        );
        let config: FileConfig = serde_json::from_slice(&bytes)
            .map_err(|_| anyhow!("invalid TokenCraft webhook configuration"))?;
        ensure!(
            (1..=64).contains(&config.destinations.len()),
            "configure 1 to 64 webhook destinations"
        );
        let destinations = config
            .destinations
            .into_iter()
            .map(|(id, raw_url)| {
                ensure!(valid_identifier(&id), "invalid webhook destination name");
                Ok((id, parse_destination(&raw_url, hosted)?))
            })
            .collect::<anyhow::Result<_>>()?;
        let mut root_key = [0; 32];
        let secret = std::env::var("TOKENCRAFT_DEMO_WEBHOOK_KEY")
            .context("set TOKENCRAFT_DEMO_WEBHOOK_KEY to a persistent 32-byte hex key")?;
        hex::decode_to_slice(secret, &mut root_key)
            .map_err(|_| anyhow!("TOKENCRAFT_DEMO_WEBHOOK_KEY must be a 32-byte hex key"))?;
        let mut client = Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .connect_timeout(Duration::from_secs(2))
            .timeout(Duration::from_secs(5));
        if let Some(path) = trust {
            let mut pem = Vec::new();
            std::fs::File::open(path).context("open webhook CA file")?
                .take(65_537).read_to_end(&mut pem)?;
            ensure!(pem.len() <= 65_536, "webhook CA file exceeds 64 KiB");
            let certificates = Certificate::from_pem_bundle(&pem).context("decode webhook CA file")?;
            ensure!(!certificates.is_empty(), "webhook CA file must contain a certificate");
            client = client.tls_built_in_root_certs(false);
            for certificate in certificates {
                client = client.add_root_certificate(certificate);
            }
        }
        Ok(Some(Self {
            destinations,
            root_key,
            client: client.build()?,
        }))
    }

    pub fn destination_ids(&self) -> impl Iterator<Item = &str> {
        self.destinations.keys().map(String::as_str)
    }

    pub(super) fn destinations_by_origin(&self) -> HashMap<String, Vec<String>> {
        let mut groups = HashMap::<String, Vec<String>>::new();
        for (id, url) in &self.destinations {
            groups
                .entry(url.origin().ascii_serialization())
                .or_default()
                .push(id.clone());
        }
        groups
    }

    pub fn destination(&self, id: &str) -> Option<&Url> {
        self.destinations.get(id)
    }

    pub fn matches_destination(&self, id: &str, saved_url: &str) -> Option<&Url> {
        self.destination(id).filter(|url| url.as_str() == saved_url)
    }

    pub fn signing_secret(&self, subscription: &str, version: i32) -> String {
        format!(
            "whsec_{}",
            STANDARD.encode(self.signing_key(subscription, version))
        )
    }

    pub(super) fn rotation_receipt_digest(
        &self,
        actor: &str,
        subscription: &str,
        expected_version: i32,
        key: &str,
    ) -> String {
        let mut mac = HmacSha256::new_from_slice(&self.root_key).expect("HMAC accepts 32-byte keys");
        mac.update(b"tokencraft-webhook-rotation-v1/");
        let intent = serde_json::to_vec(&(actor, subscription, expected_version, key))
            .expect("rotation intent serializes");
        mac.update(&intent);
        hex::encode(mac.finalize().into_bytes())
    }

    fn signing_key(&self, subscription: &str, version: i32) -> [u8; 32] {
        let mut mac =
            HmacSha256::new_from_slice(&self.root_key).expect("HMAC accepts 32-byte keys");
        mac.update(format!("tokencraft-webhook-signing-v1/{subscription}/{version}").as_bytes());
        mac.finalize().into_bytes().into()
    }

    pub fn signature(
        &self,
        delivery: &db_queries::gateway_webhooks::Claim,
        timestamp: &str,
    ) -> String {
        let sign = |version| {
            let key = self.signing_key(&delivery.subscription_id, version);
            let mut mac = HmacSha256::new_from_slice(&key).expect("HMAC accepts 32-byte keys");
            mac.update(format!("{}.{timestamp}.", delivery.id).as_bytes());
            mac.update(delivery.payload.as_bytes());
            format!("v1,{}", STANDARD.encode(mac.finalize().into_bytes()))
        };
        let current = sign(delivery.signing_version);
        if delivery.sign_with_previous {
            format!("{current} {}", sign(delivery.signing_version - 1))
        } else {
            current
        }
    }
}

fn parse_destination(raw: &str, hosted: bool) -> anyhow::Result<Url> {
    let url = Url::parse(raw).map_err(|_| anyhow!("invalid webhook destination URL"))?;
    ensure!(
        url.host_str().is_some()
            && url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none(),
        "webhook destinations require a host and must be free of user info, query, and fragment"
    );
    if hosted {
        ensure!(url.scheme() == "https", "hosted webhook destinations require HTTPS");
    } else {
        ensure!(
            (raw.starts_with("http://127.0.0.1:") || raw.starts_with("http://[::1]:"))
                && url.scheme() == "http"
                && matches!(url.host_str(), Some("127.0.0.1" | "[::1]"))
                && url.port().is_some_and(|port| port > 0),
            "development webhook destinations require a literal loopback address and explicit port"
        );
    }
    Ok(url)
}

#[cfg(test)]
mod tests {
    use super::parse_destination;

    #[test]
    fn accepts_only_operator_configured_loopback_urls_without_secrets() {
        for url in ["http://127.0.0.1:4011/events", "http://[::1]:4011/events"] {
            assert!(parse_destination(url, false).is_ok());
        }
        for url in [
            "http://localhost:4011/events",
            "https://127.0.0.1:4011/events",
            "http://127.0.0.1/events",
            "http://127.0.0.1:4011/events?secret=value",
            "http://127.0.0.1:4011/events#fragment",
            "http://example.com:4011/events",
        ] {
            assert!(parse_destination(url, false).is_err());
        }
    }

    #[test]
    fn hosted_destinations_require_https_without_embedded_credentials() {
        assert!(parse_destination("https://consumer.example/events", true).is_ok());
        for url in ["http://consumer.example/events", "http://127.0.0.1:4011/events",
            "https://user:secret@consumer.example/events", "https://consumer.example/events#extra",
            "https://consumer.example/events?secret=value"] {
            assert!(parse_destination(url, true).is_err(), "{url}");
        }
    }
}
