use std::{
    ffi::OsString,
    io::{self, Write},
    path::{Path, PathBuf},
};

use anyhow::{bail, ensure, Context, Result};
use ring::signature::{Ed25519KeyPair, KeyPair, UnparsedPublicKey, ED25519};
use serde::{Deserialize, Serialize};

use super::{digest, read_file, reference_bundle, Reference, ReferenceCatalog};

const DOMAIN: &[u8] = b"TokenCraft ArtCraft reference catalog v1\n";
// RFC 8410: SubjectPublicKeyInfo with id-Ed25519, absent parameters, and a 32-byte key.
const PUBLIC_KEY_PREFIX: &[u8] = &[
    0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00,
];
const HELP: &str = "Usage:
  api-gateway catalog sign-reference --reference FILE --private-key FILE [--plugin-catalog FILE ...]
  api-gateway catalog verify-reference --reference FILE --signature FILE --public-key FILE
  api-gateway catalog verify-reference --bundle FILE --public-key FILE [--emit-catalog]

Sign and verify the exact bytes of an ArtCraft reference catalog using Ed25519.
Use a PKCS#8 DER private key and a separately trusted SubjectPublicKeyInfo DER public key.
Pass the plugin catalogs to check publication coverage before signing.
Use --emit-catalog with a bundle to write the exact verified public JSON to stdout.
";

pub(super) enum Action {
    Sign,
    Verify,
}

pub(super) struct Trust<'a> {
    pub(super) signature: &'a Path,
    pub(super) public_key: &'a Path,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SignatureFile {
    version: u32,
    reference_revision: String,
    reference_sha256: String,
    public_key_sha256: String,
    signature: String,
}

#[derive(Serialize)]
pub(super) struct VerifiedReference {
    publisher: &'static str,
    pub(super) reference_revision: String,
    pub(super) reference_sha256: String,
    pub(super) public_key_sha256: String,
}

pub(super) fn run(action: Action, mut args: impl Iterator<Item = OsString>) -> Result<()> {
    let (mut reference, mut bundle, mut signature, mut key) = (None, None, None, None);
    let mut plugins = Vec::new();
    let mut emit_catalog = false;
    while let Some(option) = args.next() {
        let slot = match (&action, option.to_str()) {
            (_, Some("--reference")) => &mut reference,
            (Action::Sign, Some("--private-key")) | (Action::Verify, Some("--public-key")) => {
                &mut key
            }
            (Action::Verify, Some("--signature")) => &mut signature,
            (Action::Verify, Some("--bundle")) => &mut bundle,
            (Action::Verify, Some("--emit-catalog")) => {
                ensure!(!emit_catalog, "supply --emit-catalog once");
                emit_catalog = true;
                continue;
            }
            (Action::Sign, Some("--plugin-catalog")) => {
                plugins.push(PathBuf::from(args.next().context("set --plugin-catalog FILE")?));
                continue;
            }
            (_, Some("--help" | "-h")) => {
                print!("{HELP}");
                return Ok(());
            }
            _ => bail!("unknown reference signature option. Use --help"),
        };
        let value = args
            .next()
            .context("reference signature option needs a file path")?;
        ensure!(
            slot.replace(PathBuf::from(value)).is_none(),
            "duplicate reference signature option"
        );
    }
    let mut output = io::stdout().lock();
    match action {
        Action::Sign => {
            let source = Reference::load(&reference.context("set --reference FILE")?)?;
            if plugins.is_empty() {
                super::reference_check::project(&source)?;
            } else {
                super::reference_check::check(&source, &plugins)?;
            }
            let private_key = read_file(&key.context("set --private-key FILE")?)?;
            let signature = sign(&source.bytes, &source.catalog, &private_key)?;
            serde_json::to_writer_pretty(output, &signature)?;
        }
        Action::Verify => {
            ensure!(!emit_catalog || bundle.is_some(), "use --emit-catalog with --bundle");
            let public_key = key.context("set --public-key FILE")?;
            let verified = if let Some(bundle) = bundle {
                ensure!(
                    reference.is_none() && signature.is_none(),
                    "use --bundle without --reference or --signature"
                );
                let source = reference_bundle::load(&bundle, &public_key)?;
                if emit_catalog {
                    super::reference_check::project(&source)?;
                    super::reference_check::validate_publication(&source)?;
                    output.write_all(&source.bytes)?;
                    return Ok(());
                }
                source.verified.context("reference bundle was not verified")?
            } else {
                let source = Reference::load(&reference.context("set --reference or --bundle FILE")?)?;
                let signature = signature.context("set --signature FILE")?;
                Trust {
                    signature: &signature,
                    public_key: &public_key,
                }
                .verify(&source.bytes, &source.catalog)?
            };
            serde_json::to_writer_pretty(output, &verified)?;
        }
    }
    println!();
    Ok(())
}

impl Trust<'_> {
    pub(super) fn verify(
        &self,
        bytes: &[u8],
        source: &ReferenceCatalog,
    ) -> Result<VerifiedReference> {
        let signature = read_signature(self.signature)?;
        verify(bytes, source, &signature, &read_file(self.public_key)?)
    }
}

pub(super) fn read_signature(path: &Path) -> Result<SignatureFile> {
    serde_json::from_slice(&read_file(path)?).context("decode ArtCraft reference signature")
}

fn sign(bytes: &[u8], source: &ReferenceCatalog, private_der: &[u8]) -> Result<SignatureFile> {
    let key = Ed25519KeyPair::from_pkcs8_maybe_unchecked(private_der)
        .map_err(|_| anyhow::anyhow!("invalid Ed25519 PKCS#8 DER private key"))?;
    let public_der = [PUBLIC_KEY_PREFIX, key.public_key().as_ref()].concat();
    Ok(SignatureFile {
        version: 1,
        reference_revision: source.revision.clone(),
        reference_sha256: digest(bytes),
        public_key_sha256: digest(public_der),
        signature: hex::encode(key.sign(&[DOMAIN, bytes].concat()).as_ref()),
    })
}

pub(super) fn public_key_bytes(public_der: &[u8]) -> Result<&[u8]> {
    public_der
        .strip_prefix(PUBLIC_KEY_PREFIX)
        .filter(|key| key.len() == 32)
        .context("invalid Ed25519 SubjectPublicKeyInfo DER public key")
}

pub(super) fn verify(
    bytes: &[u8],
    source: &ReferenceCatalog,
    signature: &SignatureFile,
    public_der: &[u8],
) -> Result<VerifiedReference> {
    let public_key = public_key_bytes(public_der)?;
    ensure!(
        signature.version == 1,
        "unsupported reference signature version"
    );
    ensure!(
        signature.public_key_sha256 == digest(public_der),
        "reference signature uses a different public key"
    );
    ensure!(
        signature.reference_revision == source.revision,
        "reference signature revision does not match"
    );
    ensure!(
        signature.reference_sha256 == digest(bytes),
        "reference signature checksum does not match"
    );
    let mut decoded = [0_u8; 64];
    hex::decode_to_slice(&signature.signature, &mut decoded)
        .context("invalid Ed25519 reference signature encoding")?;
    UnparsedPublicKey::new(&ED25519, public_key)
        .verify(&[DOMAIN, bytes].concat(), &decoded)
        .map_err(|_| anyhow::anyhow!("ArtCraft reference signature is invalid"))?;
    Ok(VerifiedReference {
        publisher: "ArtCraft",
        reference_revision: signature.reference_revision.clone(),
        reference_sha256: signature.reference_sha256.clone(),
        public_key_sha256: signature.public_key_sha256.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signature_rejects_changed_bytes_even_with_a_recomputed_checksum() {
        let original = super::super::tests::SNAPSHOT;
        let source = ReferenceCatalog::parse(original).unwrap();
        let rng = ring::rand::SystemRandom::new();
        let private_key = Ed25519KeyPair::generate_pkcs8(&rng).unwrap();
        let pair = Ed25519KeyPair::from_pkcs8(private_key.as_ref()).unwrap();
        let public_der = [PUBLIC_KEY_PREFIX, pair.public_key().as_ref()].concat();
        let signature = sign(original, &source, private_key.as_ref()).unwrap();
        let verified = verify(original, &source, &signature, &public_der).unwrap();
        assert_eq!(verified.reference_sha256, digest(original));
        let mut forgery = sign(original, &source, private_key.as_ref()).unwrap();
        let changed = [original, b"\n"].concat();
        forgery.reference_sha256 = digest(&changed);
        let result = verify(&changed, &source, &forgery, &public_der);
        assert_eq!(
            result.err().unwrap().to_string(),
            "ArtCraft reference signature is invalid"
        );
    }
}
