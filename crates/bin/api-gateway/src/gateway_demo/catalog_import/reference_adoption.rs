use anyhow::{ensure, Context, Result};
use serde_json::{json, Value};

use super::{ArtCraftReference, Catalog, Offer, Pricing, ReferenceOffer};

pub(super) fn apply(
    entry: &ReferenceOffer,
    offer: &mut Offer,
    preset: ArtCraftReference,
    previous_sha256: &str,
    verified_key_pinned: bool,
) -> Result<()> {
    let installed = offer.artcraft_reference.as_ref().context("the local offer has no imported ArtCraft preset")?;
    ensure!(installed.offer_id == entry.id, "the local offer uses a different ArtCraft preset");
    ensure!(
        previous_sha256.len() == 64 && previous_sha256.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "update_from_sha256 must be a SHA-256 digest"
    );
    // Accept an exact replay after the candidate has already been installed.
    ensure!(
        installed == &preset || installed.sha256.eq_ignore_ascii_case(previous_sha256),
        "the installed preset SHA-256 changed"
    );
    if let Some(previous_key) = &installed.public_key_sha256 {
        let key = preset.public_key_sha256.as_ref().context("the installed preset requires a verified signature")?;
        ensure!(previous_key == key || verified_key_pinned, "pin reference_public_key_sha256 to rotate the verified signer");
    }
    let (provider, adapter) = entry.route()?;
    ensure!(
        offer.provider == provider && offer.adapter == adapter
            && offer.upstream_model.as_deref() == Some(entry.upstream_model.as_str()),
        "the reference changed the provider, adapter or upstream model"
    );

    // Use the normal catalog parser to validate and normalize reference capabilities.
    // Temporary rates only make the reference shape parseable. The operator's rates stay on the offer.
    let rate_fields = entry.kind.pricing_fields().1;
    let mut proposed = offer.clone();
    proposed.pricing = entry.pricing(rate_fields.iter().map(|&field| (field.into(), Value::from(0))).collect())?;
    let candidate = Catalog::parse(&serde_json::to_vec(&json!({ "version": 1, "offers": [proposed] }))?)
        .context("validate reference capabilities")?;
    ensure!(
        covers(&candidate.offers[0].pricing, &offer.pricing),
        "the reference no longer covers the local offer's configured capabilities"
    );
    offer.artcraft_reference = Some(preset);
    Ok(())
}

pub(super) fn covers(reference: &Pricing, local: &Pricing) -> bool {
    match (reference, local) {
        (Pricing::Image { aspect_ratios: supported, .. }, Pricing::Image { aspect_ratios: ratios, .. }) => {
            ratios.iter().all(|ratio| supported.contains(ratio))
        }
        (
            Pricing::Video { durations_seconds: supported_durations, aspect_ratios: supported_ratios, resolution, .. },
            Pricing::Video { durations_seconds, aspect_ratios, resolution: local_resolution, .. },
        ) => resolution == local_resolution
            && durations_seconds.iter().all(|duration| supported_durations.contains(duration))
            && aspect_ratios.iter().all(|ratio| supported_ratios.contains(ratio)),
        (
            Pricing::Audio { durations_seconds: supported_durations, format, .. },
            Pricing::Audio { durations_seconds, format: local_format, .. },
        ) => format == local_format && durations_seconds.iter().all(|duration| supported_durations.contains(duration)),
        (Pricing::Mesh { format, .. }, Pricing::Mesh { format: local_format, .. }) => format == local_format,
        (Pricing::Text { .. }, Pricing::Text { .. }) => true,
        _ => false,
    }
}
