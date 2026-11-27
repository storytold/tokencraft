use anyhow::{Context, Result, ensure};

use super::super::sealed::SealedBytes;

const WRITE_KEY: &str = "TOKENCRAFT_DEMO_ARCHIVE_KEY";
pub(super) const READ_KEYS: &str = "TOKENCRAFT_DEMO_ARCHIVE_READ_KEYS";
const MAX_READ_KEYS: usize = 8;
const DOMAIN: &[u8] = b"tokencraft/archive-body/aes256/v1";

pub(super) struct ArchiveKeys {
    write: SealedBytes,
    read: Vec<SealedBytes>,
}

impl ArchiveKeys {
    pub(super) fn load() -> Result<Self> {
        let write = std::env::var(WRITE_KEY)
            .context("set TOKENCRAFT_DEMO_ARCHIVE_KEY to a persistent 32-byte hex key")?;
        let read = match std::env::var(READ_KEYS) {
            Ok(keys) => Some(keys),
            Err(std::env::VarError::NotPresent) => None,
            Err(_) => anyhow::bail!("{READ_KEYS} must contain hex keys separated by commas"),
        };
        Self::from_hex(&write, read.as_deref())
    }

    fn from_hex(write: &str, read: Option<&str>) -> Result<Self> {
        let active = decode(WRITE_KEY, write)?;
        let mut known = vec![active];
        let mut readable = Vec::new();
        if let Some(keys) = read {
            for (index, key) in keys.split(',').enumerate() {
                ensure!(
                    index < MAX_READ_KEYS,
                    "{READ_KEYS} permits at most {MAX_READ_KEYS} keys"
                );
                let root = decode(READ_KEYS, key)?;
                ensure!(!known.contains(&root), "{READ_KEYS} contains a duplicate archive key");
                known.push(root);
                readable.push(SealedBytes::new(&root, DOMAIN));
            }
        }
        Ok(Self {
            write: SealedBytes::new(&active, DOMAIN),
            read: readable,
        })
    }

    pub(super) fn seal(&self, binding: &str, bytes: Vec<u8>) -> Option<Vec<u8>> {
        self.write.seal(binding, bytes)
    }

    pub(super) fn open(&self, binding: &str, bytes: &[u8]) -> Option<Vec<u8>> {
        self.write
            .open(binding, bytes)
            .or_else(|| self.read.iter().find_map(|key| key.open(binding, bytes)))
    }
}

fn decode(setting: &str, value: &str) -> Result<[u8; 32]> {
    let mut root = [0; 32];
    hex::decode_to_slice(value, &mut root)
        .map_err(|_| anyhow::anyhow!("{setting} requires 32-byte hex keys"))?;
    Ok(root)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rotation_changes_writes_while_both_staged_configs_can_read() {
        let old = "ab".repeat(32);
        let new = "23".repeat(32);
        let old_only = ArchiveKeys::from_hex(&old, None).unwrap();
        let staged = ArchiveKeys::from_hex(&old, Some(&new)).unwrap();
        let rotated = ArchiveKeys::from_hex(&new, Some(&old)).unwrap();
        let new_only = ArchiveKeys::from_hex(&new, None).unwrap();
        let binding = "archive/1";
        let before = old_only.seal(binding, b"old archive".to_vec()).unwrap();
        let after = rotated.seal(binding, b"new archive".to_vec()).unwrap();

        for reader in [staged, rotated] {
            assert_eq!(reader.open(binding, &before).unwrap(), b"old archive");
            assert_eq!(reader.open(binding, &after).unwrap(), b"new archive");
            assert!(reader.open("another/archive", &before).is_none());
        }
        assert!(old_only.open(binding, &after).is_none());
        assert!(new_only.open(binding, &before).is_none());
        assert_eq!(new_only.open(binding, &after).unwrap(), b"new archive");
    }

    #[test]
    fn rejects_invalid_duplicate_and_excess_keys() {
        let active = "ab".repeat(32);
        let extra = "cd".repeat(32);
        assert!(ArchiveKeys::from_hex(&active, Some("")).is_err());
        assert!(ArchiveKeys::from_hex(&active, Some("xyz")).is_err());
        assert!(ArchiveKeys::from_hex(&active, Some(&active.to_uppercase())).is_err());
        assert!(ArchiveKeys::from_hex(&active, Some(&format!("{extra},{extra}"))).is_err());
        let keys: Vec<_> = (1..=MAX_READ_KEYS + 1).map(|n| format!("{n:064x}")).collect();
        let permitted = keys[..MAX_READ_KEYS].join(",");
        assert!(ArchiveKeys::from_hex(&active, Some(&permitted)).is_ok());
        assert!(ArchiveKeys::from_hex(&active, Some(&keys.join(","))).is_err());
    }
}
