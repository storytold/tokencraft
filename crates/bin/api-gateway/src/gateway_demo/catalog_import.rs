pub(super) mod review;
mod compute_patch;
mod reference_adoption;
mod reference_bundle;
mod reference_check;
mod reference_signature;

use std::{
    collections::{BTreeMap, HashSet},
    ffi::OsString,
    fs::File,
    io::{self, Read, Write},
    path::{Path, PathBuf},
};

use anyhow::{bail, ensure, Context, Result};
use serde::Deserialize;
use serde_json::{Map, Value};

use super::catalog::{
    digest, Adapter, ArtCraftReference, Catalog, CatalogFile, Offer, Pricing, Provider, MAX_CONFIG_BYTES, MAX_OFFERS,
};

const HELP: &str = "Usage:
  api-gateway catalog apply-compute-patch [--base FILE] --bundle FILE
  api-gateway catalog apply-provider-patch --base FILE --bundle FILE
  api-gateway catalog prepare --selection FILE [--base FILE] --reference FILE [--signature FILE --public-key FILE]
  api-gateway catalog prepare --selection FILE [--base FILE] --bundle FILE --public-key FILE
  api-gateway catalog review --base FILE --candidate FILE [--require-preset-only]
  api-gateway catalog check-bindings --catalog FILE
  api-gateway catalog check-drain --catalog FILE
  api-gateway catalog check-reference --reference FILE --plugin-catalog FILE [--plugin-catalog FILE ...]
  api-gateway catalog sign-reference --reference FILE --private-key FILE [--plugin-catalog FILE ...]
  api-gateway catalog verify-reference --reference FILE --signature FILE --public-key FILE
  api-gateway catalog verify-reference --bundle FILE --public-key FILE [--emit-catalog]
  api-gateway catalog pack-reference --reference FILE --signature FILE --public-key FILE
  api-gateway catalog stage-reference --reference FILE --signature FILE --public-key FILE --directory DIR --plugin-catalog FILE [--plugin-catalog FILE ...]
  api-gateway catalog fetch-reference --registry HTTPS_URL (--sha256 HEX | --current) --public-key FILE [--ca FILE]
  api-gateway catalog observe-openrouter --catalog FILE [--input FILE]
  api-gateway catalog observe-fal --catalog FILE [--input FILE]
  api-gateway catalog observe-kinovi --catalog FILE --probe FILE [--input FILE] [--usd-nanos-per-credit INTEGER]
  api-gateway catalog check-rate-feed --catalog FILE [--base-revision ID] [--feed FILE [--previous-feed FILE]]
  api-gateway catalog prepare-rate-feed --catalog FILE --approved FILE [--base-revision ID] [--previous-feed FILE]
  api-gateway catalog rebase-rate-feed --base FILE --candidate FILE --feed FILE

Prepare a version 1 routing catalog from selected ArtCraft offers and local rates.
Pass an existing operator catalog as --base to retain its offers.
Set update_from_sha256 on an existing imported offer to adopt a pinned revision.
The validated result goes to stdout.
Use observe-openrouter to report public list prices for configured OpenRouter chat offers.
";

const MAX_REFERENCE_BYTES: u64 = 4 * 1024 * 1024;
const MAX_REFERENCE_OFFERS: usize = 4096;

#[derive(Deserialize)]
struct ReferenceCatalog {
    version: u32,
    revision: String,
    publisher: String,
    currency: String,
    offers: Vec<ReferenceOffer>,
}

struct Reference {
    bytes: Vec<u8>,
    catalog: ReferenceCatalog,
    verified: Option<reference_signature::VerifiedReference>,
}

impl Reference {
    fn load(path: &Path) -> Result<Self> {
        Self::parse(read_limited(path, MAX_REFERENCE_BYTES)?)
    }

    fn parse(bytes: Vec<u8>) -> Result<Self> {
        let catalog = ReferenceCatalog::parse(&bytes)?;
        Ok(Self { bytes, catalog, verified: None })
    }
}

impl ReferenceCatalog {
    fn parse(bytes: &[u8]) -> Result<Self> {
        ensure!(
            bytes.len() as u64 <= MAX_REFERENCE_BYTES,
            "ArtCraft reference catalog exceeds {} KiB",
            MAX_REFERENCE_BYTES / 1024
        );
        let source: Self = serde_json::from_slice(bytes).context("read ArtCraft reference catalog")?;
        ensure!(
            source.version == 1 && source.publisher == "ArtCraft" && source.currency == "USD",
            "expected an ArtCraft version 1 reference catalog in USD"
        );
        ensure!(
            !source.revision.is_empty() && !source.offers.is_empty(),
            "reference catalog requires a revision and offers"
        );
        ensure!(
            source.offers.len() <= MAX_REFERENCE_OFFERS,
            "ArtCraft reference catalog exceeds {MAX_REFERENCE_OFFERS} offers"
        );
        let mut ids = HashSet::new();
        ensure!(
            source.offers.iter().all(|offer| ids.insert(&offer.id)),
            "duplicate offer ID in the ArtCraft reference catalog"
        );
        Ok(source)
    }
}

#[derive(Deserialize)]
struct ReferenceOffer {
    id: String,
    model: String,
    plugin: Plugin,
    upstream_model: String,
    kind: Kind,
    capabilities: Map<String, Value>,
}

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Plugin {
    Fal,
    Kinovi,
    Openrouter,
}

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Kind {
    Image,
    Video,
    Audio,
    Mesh,
    Text,
}

impl Kind {
    fn pricing_fields(self) -> (&'static str, &'static [&'static str]) {
        match self {
            Self::Image => ("image", &["usd_nanos_per_image"]),
            Self::Video => ("video", &["usd_nanos_per_second"]),
            Self::Audio => ("audio", &["usd_nanos_per_second"]),
            Self::Mesh => ("mesh", &["usd_nanos_per_mesh"]),
            Self::Text => (
                "text",
                &[
                    "usd_nanos_per_1000_input_tokens",
                    "usd_nanos_per_1000_output_tokens",
                ],
            ),
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SelectionFile {
    version: u32,
    reference_revision: String,
    reference_sha256: Option<String>,
    reference_public_key_sha256: Option<String>,
    offers: Vec<Selection>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Selection {
    reference: String,
    binding: Option<String>,
    id: Option<String>,
    model: Option<String>,
    rates: Option<Map<String, Value>>,
    update_from_sha256: Option<String>,
}

pub async fn run(mut args: impl Iterator<Item = OsString>) -> Result<()> {
    match args.next().as_deref().and_then(|arg| arg.to_str()) {
        None | Some("help" | "--help" | "-h") => {
            print!("{HELP}");
            return Ok(());
        }
        Some("prepare") => {}
        Some("apply-compute-patch") => return compute_patch::run(args),
        Some("apply-provider-patch") => return run_provider_patch(args),
        Some("review") => return review::run(args),
        Some("check-bindings") => return super::providers::binding_check::run(args),
        Some("check-drain") => return super::providers::rollout::run(args).await,
        Some("check-reference") => return reference_check::run(args),
        Some("sign-reference") => return reference_signature::run(reference_signature::Action::Sign, args),
        Some("verify-reference") => return reference_signature::run(reference_signature::Action::Verify, args),
        Some("pack-reference") => return reference_bundle::run(reference_bundle::Action::Pack, args).await,
        Some("stage-reference") => return reference_bundle::run(reference_bundle::Action::Stage, args).await,
        Some("fetch-reference") => return reference_bundle::run(reference_bundle::Action::Fetch, args).await,
        Some("observe-openrouter") => return super::catalog_observations::run(args).await,
        Some("observe-fal") => return super::catalog_observations::fal::run(args).await,
        Some("observe-kinovi") => return super::catalog_observations::kinovi::run(args).await,
        Some("check-rate-feed") => return super::catalog_refresh::check::run(args),
        Some("prepare-rate-feed") => return super::catalog_refresh::prepare::run(args),
        Some("rebase-rate-feed") => return super::catalog_refresh::rebase::run(args),
        _ => bail!("unknown catalog command. Use api-gateway catalog --help"),
    }

    let (mut reference, mut selection, mut base) = (None, None, None);
    let (mut bundle, mut signature, mut public_key) = (None, None, None);
    while let Some(arg) = args.next() {
        let slot = match arg.to_str() {
            Some("--reference") => &mut reference,
            Some("--selection") => &mut selection,
            Some("--base") => &mut base,
            Some("--bundle") => &mut bundle,
            Some("--signature") => &mut signature,
            Some("--public-key") => &mut public_key,
            Some("--help" | "-h") => {
                print!("{HELP}");
                return Ok(());
            }
            _ => bail!("unknown catalog option. Use api-gateway catalog --help"),
        };
        let value = args.next().context("catalog option requires a file path")?;
        ensure!(
            slot.replace(PathBuf::from(value)).is_none(),
            "duplicate catalog option"
        );
    }

    let reference = if let Some(bundle) = bundle {
        ensure!(reference.is_none() && signature.is_none(), "use --bundle without --reference or --signature");
        reference_bundle::load(&bundle, &public_key.context("set --public-key FILE for the bundle")?)?
    } else {
        let mut reference = Reference::load(&reference.context("set --reference or --bundle FILE")?)?;
        match (signature.as_deref(), public_key.as_deref()) {
            (Some(signature), Some(public_key)) => {
                let trust = reference_signature::Trust { signature, public_key };
                reference.verified = Some(trust.verify(&reference.bytes, &reference.catalog)?);
            }
            (None, None) => {}
            _ => bail!("use --signature and --public-key together"),
        }
        reference
    };
    let selection = read_file(&selection.context("set --selection FILE")?)?;
    let base = base.as_deref().map(Catalog::load).transpose()?;
    let output = prepare(&reference, &selection, base)?;
    io::stdout().lock().write_all(&output)?;
    Ok(())
}

fn run_provider_patch(mut args: impl Iterator<Item = OsString>) -> Result<()> {
    let (mut base, mut bundle) = (None, None);
    while let Some(arg) = args.next() {
        let slot = match arg.to_str() {
            Some("--base") => &mut base,
            Some("--bundle") => &mut bundle,
            Some("--help" | "-h") => {
                print!("{HELP}");
                return Ok(());
            }
            _ => bail!("unknown provider patch option"),
        };
        let value = args.next().context("provider patch option requires a file path")?;
        ensure!(slot.replace(PathBuf::from(value)).is_none(), "duplicate provider patch option");
    }
    let base = Catalog::load(&base.context("set --base FILE")?)?;
    // A review bundle can exceed the catalog limit because it contains review evidence.
    let mut bytes = Vec::new();
    let max_bundle_bytes = 256 * 1024;
    File::open(bundle.context("set --bundle FILE")?)?
        .take(max_bundle_bytes + 1).read_to_end(&mut bytes)?;
    ensure!(bytes.len() as u64 <= max_bundle_bytes, "provider bundle exceeds 256 KiB");
    let output = apply_provider_patch(base, serde_json::from_slice(&bytes)?)?;
    serde_json::to_writer_pretty(io::stdout().lock(), &output)?;
    Ok(())
}

#[derive(Deserialize)]
struct ProviderPatch {
    catalog_patch: bool,
    base_catalog_revision: String,
    proposed_revision: String,
    changed_offer_ids: Vec<String>,
    catalog: CatalogFile,
}

fn apply_provider_patch(base: Catalog, patch: ProviderPatch) -> Result<CatalogFile> {
    ensure!(patch.catalog_patch, "expected a provider catalog patch");
    ensure!(patch.base_catalog_revision == base.file_revision, "reviewed base catalog changed");
    let mut catalog = base.into_file();
    let changed: HashSet<_> = patch.changed_offer_ids.iter().map(String::as_str).collect();
    ensure!(
        patch.catalog.version == catalog.version && !changed.is_empty()
            && changed.len() == patch.catalog.offers.len()
            && changed.len() == patch.changed_offer_ids.len()
            && changed == patch.catalog.offers.iter().map(|offer| offer.id.as_str()).collect(),
        "provider patch offers do not match the reviewed change"
    );
    catalog.offers.retain(|offer| !changed.contains(offer.id.as_str()));
    catalog.offers.extend(patch.catalog.offers);
    for (id, access) in patch.catalog.connection_access {
        ensure!(catalog.offers.iter().any(|offer| {
            changed.contains(offer.id.as_str()) && offer.connection_id() == id
        }), "provider patch scope must belong to a changed connection");
        catalog.connection_access.insert(id, access);
    }
    let next = Catalog::parse(&serde_json::to_vec(&catalog)?)?;
    ensure!(next.file_revision == patch.proposed_revision, "provider patch no longer matches the reviewed catalog");
    Ok(next.into_file())
}

pub(super) fn read_file(path: &Path) -> Result<Vec<u8>> {
    read_limited(path, MAX_CONFIG_BYTES)
}

fn read_limited(path: &Path, max: u64) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    File::open(path)
        .with_context(|| format!("open {}", path.display()))?
        .take(max + 1)
        .read_to_end(&mut bytes)?;
    ensure!(bytes.len() as u64 <= max, "catalog input exceeds {} KiB", max / 1024);
    Ok(bytes)
}

fn prepare(
    input: &Reference,
    selection_bytes: &[u8],
    base: Option<Catalog>,
) -> Result<Vec<u8>> {
    let source = &input.catalog;
    let selection: SelectionFile =
        serde_json::from_slice(selection_bytes).context("read catalog selection")?;
    if let Some(expected) = &selection.reference_public_key_sha256 {
        let verified = input.verified.as_ref().context("the selection requires a verified reference signature")?;
        ensure!(
            verified.public_key_sha256.eq_ignore_ascii_case(expected),
            "reference signing key does not match the selection"
        );
    }
    ensure!(
        selection.version == 1 && !selection.offers.is_empty(),
        "selection requires version 1 and at least one offer"
    );
    ensure!(
        selection.reference_revision == source.revision,
        "selection references a different ArtCraft revision"
    );
    let reference_sha256 = digest(&input.bytes);
    if let Some(expected) = &selection.reference_sha256 {
        ensure!(
            reference_sha256.eq_ignore_ascii_case(expected),
            "ArtCraft reference SHA-256 does not match the selection"
        );
    }
    let mut catalog = base.map(Catalog::into_file).unwrap_or_else(|| CatalogFile {
        version: 1, offers: Vec::new(),
        connection_access: Default::default(), rate_feed_offers: None,
    });
    let additions = selection.offers.iter().filter(|choice| choice.update_from_sha256.is_none()).count();
    ensure!(
        selection.offers.len() <= MAX_OFFERS && catalog.offers.len() + additions <= MAX_OFFERS,
        "prepared catalog exceeds the gateway's offer limit"
    );

    let reference: BTreeMap<_, _> = source.offers.iter().map(|entry| (entry.id.as_str(), entry)).collect();
    let mut selected = HashSet::new();
    for choice in selection.offers {
        let entry = reference
            .get(choice.reference.as_str())
            .with_context(|| format!("unknown reference offer {}", choice.reference))?;
        let preset = ArtCraftReference {
            offer_id: entry.id.clone(),
            revision: source.revision.clone(),
            sha256: reference_sha256.clone(),
            public_key_sha256: input.verified.as_ref().map(|value| value.public_key_sha256.clone()),
        };
        let id = choice.id.as_deref().unwrap_or(&entry.id);
        ensure!(
            selected.insert(id.to_owned()),
            "offer ID {id} already exists in the selection. Choose a distinct local ID",
        );
        let existing = catalog.offers.iter_mut().find(|offer| offer.id == id);
        if let Some(previous_sha256) = &choice.update_from_sha256 {
            ensure!(selection.reference_sha256.is_some(), "pin reference_sha256 when adopting a revision");
            ensure!(choice.binding.is_none() && choice.model.is_none() && choice.rates.is_none(),
                "omit binding, model and rates when updating {id} to keep the operator's settings");
            let offer = existing.with_context(|| format!("update requires local offer {id} in --base"))?;
            reference_adoption::apply(
                entry, offer, preset, previous_sha256, selection.reference_public_key_sha256.is_some(),
            ).with_context(|| format!("update local offer {id}"))?;
        } else {
            ensure!(existing.is_none(), "offer ID {id} already exists. Choose a distinct local ID");
            let prepared = prepare_offer(entry, choice, preset)
                .with_context(|| format!("prepare reference offer {}", entry.id))?;
            catalog.offers.push(prepared);
        }
    }

    let normalized = Catalog::parse(&serde_json::to_vec(&catalog)?)
        .context("validate prepared gateway catalog")?
        .into_file();
    let mut output = serde_json::to_vec_pretty(&normalized)?;
    if output.len() as u64 >= MAX_CONFIG_BYTES {
        output = serde_json::to_vec(&normalized)?;
    }
    ensure!((output.len() as u64) < MAX_CONFIG_BYTES, "prepared gateway catalog exceeds 64 KiB");
    output.push(b'\n');
    Ok(output)
}

fn prepare_offer(entry: &ReferenceOffer, choice: Selection, preset: ArtCraftReference) -> Result<Offer> {
    let (provider, adapter) = entry.route()?;
    let pricing = entry.pricing(choice.rates.context("supply local rates for the new offer")?)?;
    Ok(Offer {
        id: choice.id.unwrap_or_else(|| entry.id.clone()),
        provider,
        model: choice.model.unwrap_or_else(|| entry.model.clone()),
        adapter,
        mock_latency_ms: None,
        upstream_model: Some(entry.upstream_model.clone()),
        local_binding: Some(choice.binding.context("supply a local binding for the new offer")?),
        data_handling: None,
        artcraft_reference: Some(preset),
        pricing,
    })
}

impl ReferenceOffer {
    fn route(&self) -> Result<(Provider, Adapter)> {
        let (provider, adapter) = match (self.plugin, self.kind) {
            (Plugin::Fal, Kind::Image | Kind::Video | Kind::Audio | Kind::Mesh) => ("fal", Adapter::ConnectorQueue),
            (Plugin::Kinovi, Kind::Video) => ("kinovi", Adapter::ConnectorQueue),
            (Plugin::Openrouter, Kind::Text) => ("openrouter", Adapter::OpenAiCompatibleChat),
            _ => bail!("unsupported workload for the selected plugin"),
        };
        Ok((provider.parse().map_err(anyhow::Error::msg)?, adapter))
    }

    fn pricing(&self, rates: Map<String, Value>) -> Result<Pricing> {
        let (kind, rate_fields) = self.kind.pricing_fields();
        ensure!(
            rates.len() == rate_fields.len() && rate_fields.iter().all(|field| rates.contains_key(*field)),
            "supply local rates for {}",
            rate_fields.join(", ")
        );
        let mut pricing = self.capabilities.clone();
        ensure!(
            pricing.insert("kind".into(), Value::String(kind.into())).is_none(),
            "reference capabilities contain a pricing kind override"
        );
        for (field, rate) in rates {
            ensure!(pricing.insert(field, rate).is_none(), "reference capabilities contain a price override");
        }
        serde_json::from_value(Value::Object(pricing)).context("validate pricing fields")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(super) const SNAPSHOT: &[u8] = br#"{
        "version": 1, "revision": "ref-1", "publisher": "ArtCraft", "currency": "USD",
        "offers": [{
            "id": "fixture_image", "plugin": "fal", "model": "fixture_image",
            "upstream_model": "fal-ai/flux/dev", "kind": "image",
            "capabilities": {"aspect_ratios": ["4:3"]},
            "rates": [{"unit": "megapixel", "usd_nanos": 25000000}]
        }]
    }"#;
    const SELECTION: &[u8] = br#"{
        "version": 1, "reference_revision": "ref-1",
        "offers": [{
            "reference": "fixture_image", "binding": "local-fal",
            "rates": {"usd_nanos_per_image": 31000000}
        }]
    }"#;

    #[test]
    fn preserves_capabilities_and_uses_the_local_rate_and_binding() {
        let input = Reference::parse(SNAPSHOT.to_vec()).unwrap();
        let bytes = prepare(&input, SELECTION, None).unwrap();
        let catalog = Catalog::parse(&bytes).unwrap();
        let [offer] = catalog.offers.as_slice() else {
            panic!("expected one selected offer");
        };
        assert_eq!(offer.local_binding.as_deref(), Some("local-fal"));
        assert_eq!(offer.artcraft_reference, Some(ArtCraftReference {
            offer_id: "fixture_image".into(),
            revision: "ref-1".into(),
            sha256: digest(SNAPSHOT),
            public_key_sha256: None,
        }));
        assert_eq!(
            offer.pricing,
            Pricing::Image {
                usd_nanos_per_image: 31_000_000,
                aspect_ratios: vec!["4:3".into()],
            },
        );
    }

    #[test]
    fn selects_one_offer_from_a_reference_larger_than_the_local_catalog_limit() {
        let mut snapshot: Value = serde_json::from_slice(SNAPSHOT).unwrap();
        let template = snapshot["offers"][0].clone();
        let offers: Vec<_> = (0..384).map(|index| {
            let mut offer = template.clone();
            offer["id"] = format!("fixture_{index}").into();
            offer
        }).collect();
        snapshot["offers"] = offers.into();
        let reference_bytes = serde_json::to_vec(&snapshot).unwrap();
        assert!(reference_bytes.len() as u64 > MAX_CONFIG_BYTES);
        let reference = Reference::parse(reference_bytes).unwrap();
        let mut selection: Value = serde_json::from_slice(SELECTION).unwrap();
        selection["offers"][0]["reference"] = "fixture_383".into();
        let local_bytes = prepare(&reference, &serde_json::to_vec(&selection).unwrap(), None).unwrap();
        let local = Catalog::parse(&local_bytes).unwrap();
        assert_eq!(local.offers.len(), 1);
        assert_eq!(local.offers[0].id, "fixture_383");
    }

    #[test]
    fn rejects_reference_inputs_over_the_reference_limits() {
        let mut oversized = SNAPSHOT.to_vec();
        oversized.resize(MAX_REFERENCE_BYTES as usize + 1, b' ');
        assert!(Reference::parse(oversized).err().unwrap().to_string().contains("exceeds 4096 KiB"));

        let snapshot: Value = serde_json::from_slice(SNAPSHOT).unwrap();
        let template = snapshot["offers"][0].clone();
        let offers: Vec<_> = (0..=MAX_REFERENCE_OFFERS).map(|index| {
            let mut offer = template.clone();
            offer["id"] = format!("fixture_{index}").into();
            offer
        }).collect();
        let mut too_many = snapshot;
        too_many["offers"] = offers.into();
        let bytes = serde_json::to_vec(&too_many).unwrap();
        assert!(Reference::parse(bytes).err().unwrap().to_string().contains("exceeds 4096 offers"));
    }

    #[test]
    fn rejects_malformed_preset_metadata_in_the_local_catalog() {
        let input = Reference::parse(SNAPSHOT.to_vec()).unwrap();
        let bytes = prepare(&input, SELECTION, None).unwrap();
        let mut catalog: Value = serde_json::from_slice(&bytes).unwrap();
        catalog["offers"][0]["artcraft_reference"]["sha256"] = Value::String("invalid".into());
        let result = Catalog::parse(&serde_json::to_vec(&catalog).unwrap());
        assert_eq!(result.err().unwrap().to_string(), "invalid ArtCraft reference SHA-256");
    }

    #[test]
    fn refuses_to_replace_an_existing_operator_offer() {
        let input = Reference::parse(SNAPSHOT.to_vec()).unwrap();
        let bytes = prepare(&input, SELECTION, None).unwrap();
        let catalog = Catalog::parse(&bytes).unwrap();
        let error = prepare(&input, SELECTION, Some(catalog)).unwrap_err();
        assert!(error.to_string().contains("fixture_image already exists"));
    }

    #[test]
    fn adopts_a_compatible_revision_without_changing_operator_settings() {
        let mut initial: Value = serde_json::from_slice(SNAPSHOT).unwrap();
        initial["offers"][0]["capabilities"]["aspect_ratios"] = serde_json::json!(["4:3", "1:1"]);
        let original = Reference::parse(serde_json::to_vec(&initial).unwrap()).unwrap();
        let mut initial_selection: Value = serde_json::from_slice(SELECTION).unwrap();
        initial_selection["offers"][0]["id"] = "local_image".into();
        initial_selection["offers"][0]["model"] = "studio_image".into();
        let base = prepare(&original, &serde_json::to_vec(&initial_selection).unwrap(), None).unwrap();
        let mut base_file: Value = serde_json::from_slice(&base).unwrap();
        base_file["rate_feed_offers"] = serde_json::json!(["local_image"]);
        let base = serde_json::to_vec(&base_file).unwrap();

        let mut published: Value = serde_json::from_slice(SNAPSHOT).unwrap();
        published["revision"] = "ref-2".into();
        published["offers"][0]["capabilities"]["aspect_ratios"] = serde_json::json!(["8:6", "1:1"]);
        let newer = Reference::parse(serde_json::to_vec(&published).unwrap()).unwrap();
        let selection = serde_json::to_vec(&serde_json::json!({
            "version": 1, "reference_revision": "ref-2", "reference_sha256": digest(&newer.bytes),
            "offers": [{ "id": "local_image", "reference": "fixture_image", "update_from_sha256": digest(&original.bytes) }]
        })).unwrap();

        let upgraded = prepare(&newer, &selection, Some(Catalog::parse(&base).unwrap())).unwrap();
        let mut result: Value = serde_json::from_slice(&upgraded).unwrap();
        assert_eq!(result["offers"][0]["artcraft_reference"]["sha256"], digest(&newer.bytes));
        assert_eq!(result["offers"][0]["artcraft_reference"]["revision"], "ref-2");
        result["offers"][0]["artcraft_reference"] = base_file["offers"][0]["artcraft_reference"].clone();
        assert_eq!(result, base_file);
        let replay = prepare(&newer, &selection, Some(Catalog::parse(&upgraded).unwrap())).unwrap();
        assert_eq!(replay, upgraded);

        published["offers"][0]["capabilities"]["aspect_ratios"] = serde_json::json!(["1:1"]);
        let incompatible = Reference::parse(serde_json::to_vec(&published).unwrap()).unwrap();
        let mut incompatible_selection: Value = serde_json::from_slice(&selection).unwrap();
        incompatible_selection["reference_sha256"] = digest(&incompatible.bytes).into();
        let error = prepare(
            &incompatible,
            &serde_json::to_vec(&incompatible_selection).unwrap(),
            Some(Catalog::parse(&base).unwrap()),
        ).unwrap_err();
        assert!(format!("{error:#}").contains("no longer covers the local offer's configured capabilities"));
    }
}
