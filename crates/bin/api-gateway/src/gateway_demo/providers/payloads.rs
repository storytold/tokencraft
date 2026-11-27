use anyhow::{ensure, Result};
use ring::hmac;

use super::super::sealed::{decode_key, optional_key_setting, read_keys, SealedKeyRing};

const WRITE_KEY: &str = "TOKENCRAFT_CONTENT_ENCRYPTION_KEY";
const READ_KEYS: &str = "TOKENCRAFT_CONTENT_ENCRYPTION_READ_KEYS";

pub(super) struct ContentEncryption {
    write: [u8; 32],
    read: Vec<[u8; 32]>,
}

impl ContentEncryption {
    pub(super) fn load(identity: &[u8; 32], session_secret: &str) -> Result<Self> {
        let write = optional_key_setting(WRITE_KEY)?;
        let read = optional_key_setting(READ_KEYS)?;
        Self::from_settings(identity, write.as_deref(), read.as_deref(), session_secret)
    }

    fn from_settings(
        identity: &[u8; 32],
        write: Option<&str>,
        read: Option<&str>,
        session_secret: &str,
    ) -> Result<Self> {
        let explicit_write = write.is_some();
        let write = write.map(|value| decode_key(WRITE_KEY, value)).transpose()?.unwrap_or(*identity);
        let read = read_keys(READ_KEYS, &write, read)?;
        let mut session_key = [0; 32];
        if hex::decode_to_slice(session_secret, &mut session_key).is_ok() {
            ensure!((!explicit_write || write != session_key) && !read.contains(&session_key),
                "request content and session keys must be independent");
        }
        Ok(Self { write, read })
    }

    pub(super) fn roots(&self) -> impl Iterator<Item = &[u8; 32]> {
        std::iter::once(&self.write).chain(&self.read)
    }
}

pub struct Payloads {
    encryption_key: SealedKeyRing,
    intent_key: hmac::Key,
    pub(in crate::gateway_demo) media: SealedKeyRing,
}

impl Payloads {
    pub(super) fn new(identity: &[u8; 32], content: &ContentEncryption) -> Self {
        let root_key = hmac::Key::new(hmac::HMAC_SHA256, identity);
        let intent = hmac::sign(&root_key, b"tokencraft/dispatch-intent/hmac/v1");
        Self {
            encryption_key: SealedKeyRing::new(
                &content.write, &content.read, b"tokencraft/dispatch-payload/aes256/v1",
            ),
            intent_key: hmac::Key::new(hmac::HMAC_SHA256, intent.as_ref()),
            media: SealedKeyRing::new(
                &content.write, &content.read, b"tokencraft/artifact-body/aes256/v1",
            ),
        }
    }

    pub fn fingerprint(&self, bytes: &[u8]) -> String {
        hex::encode(hmac::sign(&self.intent_key, bytes).as_ref())
    }

    pub fn seal(&self, request_id: &str, field: &str, bytes: Vec<u8>) -> Option<Vec<u8>> {
        self.encryption_key
            .seal(&format!("{request_id}/{field}"), bytes)
    }

    pub fn open(&self, request_id: &str, field: &str, bytes: &[u8]) -> Option<Vec<u8>> {
        self.encryption_key
            .open(&format!("{request_id}/{field}"), bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn configured(identity: &[u8; 32], write: Option<&str>, read: Option<&str>) -> Payloads {
        Payloads::new(identity, &ContentEncryption::from_settings(identity, write, read, "").unwrap())
    }

    #[test]
    fn encrypted_payload_is_bound_to_its_request_and_purpose() {
        let payloads = configured(&[42; 32], None, None);
        let encrypted = payloads
            .seal("req_a", "input", b"example".to_vec())
            .unwrap();
        assert_eq!(
            payloads.open("req_a", "input", &encrypted).unwrap(),
            b"example"
        );
        assert!(payloads.open("req_b", "input", &encrypted).is_none());
        assert!(payloads.open("req_a", "result", &encrypted).is_none());
        assert!(configured(&[43; 32], None, None)
            .open("req_a", "input", &encrypted)
            .is_none());
    }

    #[test]
    fn rotation_reads_each_generation_and_keeps_the_intent_fingerprint() {
        let identity = [42; 32];
        let legacy = hex::encode(identity);
        let next = "b7".repeat(32);
        let original = configured(&identity, None, None);
        let staged = configured(&identity, None, Some(&next));
        let rotated = configured(&identity, Some(&next), Some(&legacy));
        let current = configured(&identity, Some(&next), None);
        let before = original.seal("req_a", "input", b"before".to_vec()).unwrap();
        let after = rotated.seal("req_a", "input", b"after".to_vec()).unwrap();
        let old_media = original.media.seal("req_a/0", vec![1]).unwrap();
        let new_media = rotated.media.seal("req_a/0", vec![2]).unwrap();

        for overlap in [&staged, &rotated] {
            assert_eq!(overlap.open("req_a", "input", &before).unwrap(), b"before");
            assert_eq!(overlap.open("req_a", "input", &after).unwrap(), b"after");
            assert_eq!(overlap.media.open("req_a/0", &old_media).unwrap(), [1]);
            assert_eq!(overlap.media.open("req_a/0", &new_media).unwrap(), [2]);
            assert!(overlap.open("req_b", "input", &after).is_none());
            assert!(overlap.media.open("req_a/1", &old_media).is_none());
            assert_eq!(overlap.fingerprint(b"intent"), original.fingerprint(b"intent"));
        }
        assert!(original.open("req_a", "input", &after).is_none());
        assert!(current.media.open("req_a/0", &old_media).is_none());
        assert_eq!(current.open("req_a", "input", &after).unwrap(), b"after");
    }

    #[test]
    fn rejects_malformed_content_keys_and_reuse_of_the_session_key() {
        let identity = [42; 32];
        let session = "c8".repeat(32);
        assert!(ContentEncryption::from_settings(&identity, Some("invalid"), None, &session).is_err());
        assert!(ContentEncryption::from_settings(&identity, Some(&session), None, &session).is_err());
        assert!(ContentEncryption::from_settings(&identity, None, Some(&session), &session).is_err());
    }
}
