use anyhow::{anyhow, Result};

use super::super::sealed::{decode_key, optional_key_setting, read_keys, SealedKeyRing};

const WRITE_KEY: &str = "TOKENCRAFT_DEMO_ARCHIVE_KEY";
pub(super) const READ_KEYS: &str = "TOKENCRAFT_DEMO_ARCHIVE_READ_KEYS";
const DOMAIN: &[u8] = b"tokencraft/archive-body/aes256/v1";

pub(super) fn load() -> Result<SealedKeyRing> {
    let write = std::env::var(WRITE_KEY)
        .map_err(|_| anyhow!("set {WRITE_KEY} to a persistent 32-byte hex key"))?;
    let read = optional_key_setting(READ_KEYS)?;
    from_hex(&write, read.as_deref())
}

fn from_hex(write: &str, read: Option<&str>) -> Result<SealedKeyRing> {
    let write = decode_key(WRITE_KEY, write)?;
    let read = read_keys(READ_KEYS, &write, read)?;
    Ok(SealedKeyRing::new(&write, &read, DOMAIN))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gateway_demo::sealed::MAX_READ_KEYS;

    #[test]
    fn rotation_changes_writes_while_both_staged_configs_can_read() {
        let old = "ab".repeat(32);
        let new = "23".repeat(32);
        let old_only = from_hex(&old, None).unwrap();
        let staged = from_hex(&old, Some(&new)).unwrap();
        let rotated = from_hex(&new, Some(&old)).unwrap();
        let new_only = from_hex(&new, None).unwrap();
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
        assert!(from_hex(&active, Some("")).is_err());
        assert!(from_hex(&active, Some("xyz")).is_err());
        assert!(from_hex(&active, Some(&active.to_uppercase())).is_err());
        assert!(from_hex(&active, Some(&format!("{extra},{extra}"))).is_err());
        let keys: Vec<_> = (1..=MAX_READ_KEYS + 1).map(|n| format!("{n:064x}")).collect();
        let permitted = keys[..MAX_READ_KEYS].join(",");
        assert!(from_hex(&active, Some(&permitted)).is_ok());
        assert!(from_hex(&active, Some(&keys.join(","))).is_err());
    }
}
