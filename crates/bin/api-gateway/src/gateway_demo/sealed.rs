use anyhow::{anyhow, ensure, Result};
use ring::{
    aead::{Aad, LessSafeKey, Nonce, UnboundKey, AES_256_GCM, NONCE_LEN},
    hmac,
    rand::{SecureRandom, SystemRandom},
};

pub(super) const MAX_READ_KEYS: usize = 8;

pub(super) fn optional_key_setting(setting: &str) -> Result<Option<String>> {
    match std::env::var(setting) {
        Ok(value) => Ok(Some(value)),
        Err(std::env::VarError::NotPresent) => Ok(None),
        Err(_) => anyhow::bail!("{setting} must be valid UTF-8"),
    }
}

pub(super) fn decode_key(setting: &str, value: &str) -> Result<[u8; 32]> {
    let mut root = [0; 32];
    hex::decode_to_slice(value, &mut root)
        .map_err(|_| anyhow!("{setting} requires 32-byte hex keys"))?;
    Ok(root)
}

pub(super) fn read_keys(setting: &str, write: &[u8; 32], value: Option<&str>) -> Result<Vec<[u8; 32]>> {
    let mut keys = Vec::new();
    if let Some(value) = value {
        for text in value.split(',') {
            ensure!(keys.len() < MAX_READ_KEYS, "{setting} permits at most {MAX_READ_KEYS} keys");
            let key = decode_key(setting, text)?;
            ensure!(&key != write && !keys.contains(&key), "{setting} contains a duplicate key");
            keys.push(key);
        }
    }
    Ok(keys)
}

pub(super) struct SealedKeyRing {
    write: SealedBytes,
    read: Vec<SealedBytes>,
}

impl SealedKeyRing {
    pub fn new(write: &[u8; 32], read: &[[u8; 32]], domain: &[u8]) -> Self {
        Self {
            write: SealedBytes::new(write, domain),
            read: read.iter().map(|key| SealedBytes::new(key, domain)).collect(),
        }
    }

    pub fn seal(&self, binding: &str, bytes: Vec<u8>) -> Option<Vec<u8>> {
        self.write.seal(binding, bytes)
    }

    pub fn open(&self, binding: &str, bytes: &[u8]) -> Option<Vec<u8>> {
        self.write.open(binding, bytes)
            .or_else(|| self.read.iter().find_map(|key| key.open(binding, bytes)))
    }
}

pub(super) struct SealedBytes(LessSafeKey);

impl SealedBytes {
    pub fn new(root: &[u8; 32], domain: &[u8]) -> Self {
        let root_key = hmac::Key::new(hmac::HMAC_SHA256, root);
        let encryption = hmac::sign(&root_key, domain);
        Self(LessSafeKey::new(
            UnboundKey::new(&AES_256_GCM, encryption.as_ref())
                .expect("SHA-256 gives a 32-byte key"),
        ))
    }

    pub fn seal(&self, binding: &str, mut bytes: Vec<u8>) -> Option<Vec<u8>> {
        let mut nonce = [0u8; NONCE_LEN];
        SystemRandom::new().fill(&mut nonce).ok()?;
        self.0
            .seal_in_place_append_tag(
                Nonce::assume_unique_for_key(nonce),
                Aad::from(binding.as_bytes()),
                &mut bytes,
            )
            .ok()?;
        let mut encrypted = nonce.to_vec();
        encrypted.extend_from_slice(&bytes);
        Some(encrypted)
    }

    pub fn open(&self, binding: &str, bytes: &[u8]) -> Option<Vec<u8>> {
        let nonce = Nonce::try_assume_unique_for_key(bytes.get(..NONCE_LEN)?).ok()?;
        let mut body = bytes.get(NONCE_LEN..)?.to_vec();
        let length = self
            .0
            .open_in_place(nonce, Aad::from(binding.as_bytes()), &mut body)
            .ok()?
            .len();
        body.truncate(length);
        Some(body)
    }
}
