use std::collections::{BTreeMap, BTreeSet};

use db_queries::gateway_requests::RequestScope;
use serde::{Deserialize, Serialize};

use super::super::catalog::{public_model_id, Adapter, Catalog, Offer, Pricing};
use super::super::providers::ProviderRuntime;
use super::super::routing_policy::Policies;
use super::graph::{Binding, Graph, InputField, Kind, Node, Setting, Value, ValueType};

#[derive(Serialize)]
pub(super) struct Primitive {
    pub kind: Kind,
    pub label: &'static str,
    pub output: ValueType,
    pub settings: &'static [Setting],
}

pub(super) fn primitives() -> Vec<Primitive> {
    [
        (Kind::Text, "Generate text"),
        (Kind::Image, "Generate image"),
        (Kind::Video, "Generate video"),
    ]
    .into_iter()
    .map(|(kind, label)| Primitive {
        kind,
        label,
        output: kind.output(),
        settings: kind.settings(),
    })
    .collect()
}

#[derive(Serialize)]
pub(super) struct Model {
    pub id: String,
    pub kind: Kind,
    pub providers: BTreeSet<String>,
    pub aspect_ratios: BTreeSet<String>,
    pub durations: BTreeSet<u16>,
    pub resolutions: BTreeSet<String>,
    pub accepts_first_frame: bool,
}

pub(super) fn available_offers<'a>(
    catalog: &'a Catalog,
    scope: Option<(&'a Policies, RequestScope<'a>)>,
) -> impl Iterator<Item = &'a Offer> {
    catalog.offers.iter().filter(move |offer| match scope {
        Some((policies, scope)) => policies.allows_offer(catalog, offer, scope),
        None => catalog.visible_to(offer, None),
    })
}

pub(super) fn offer_kind(offer: &Offer) -> Option<Kind> {
    match (&offer.pricing, offer.adapter) {
        (Pricing::Text { .. }, adapter) if adapter.supports_chat() || adapter == Adapter::Mock => Some(Kind::Text),
        (Pricing::Image { .. }, Adapter::ConnectorQueue | Adapter::Mock) => Some(Kind::Image),
        (Pricing::Video { .. }, Adapter::ConnectorQueue | Adapter::Mock) => Some(Kind::Video),
        _ => None,
    }
}

pub(super) fn models(
    catalog: &Catalog,
    runtime: Option<&ProviderRuntime>,
    scope: Option<(&Policies, RequestScope<'_>)>,
) -> Vec<Model> {
    let mut entries = BTreeMap::new();
    for offer in available_offers(catalog, scope) {
        let Some(kind) = offer_kind(offer) else { continue };
        let id = public_model_id(&offer.model);
        let (entry, all_mock) = entries.entry((id, kind as u8)).or_insert_with(|| {
            (Model {
                id: id.to_owned(),
                kind,
                providers: BTreeSet::new(),
                aspect_ratios: BTreeSet::new(),
                durations: BTreeSet::new(),
                resolutions: BTreeSet::new(),
                accepts_first_frame: false,
            }, true)
        });
        *all_mock &= offer.adapter == Adapter::Mock;
        entry.providers.insert(offer.provider.as_str().to_owned());
        match &offer.pricing {
            Pricing::Image { aspect_ratios, .. } => {
                entry.aspect_ratios.extend(aspect_ratios.iter().cloned())
            }
            Pricing::Video {
                aspect_ratios,
                durations_seconds,
                resolution,
                ..
            } => {
                entry.aspect_ratios.extend(aspect_ratios.iter().cloned());
                entry.durations.extend(durations_seconds);
                entry.resolutions.insert(resolution.clone());
                entry.accepts_first_frame |= runtime.is_some_and(|value| value.accepts_workflow_frame(offer));
            }
            _ => {}
        }
    }
    entries.into_values()
        .map(|(mut model, all_mock)| {
            model.accepts_first_frame |= model.kind == Kind::Video && all_mock;
            model
        })
        .collect()
}

pub(super) fn validate_models(graph: &Graph, models: &[Model]) -> Result<(), String> {
    for node in &graph.nodes {
        if node.kind == Kind::Tool {
            continue;
        }
        let model = models
            .iter()
            .find(|model| model.id == node.model && model.kind == node.kind)
            .ok_or_else(|| {
                format!(
                    "Choose an available {} model for {}.",
                    label(node.kind),
                    node.title
                )
            })?;
        if node.settings.contains_key("first_frame") && !model.accepts_first_frame {
            return Err(format!(
                "{} needs a model configured for an image first frame.",
                node.title
            ));
        }
    }
    Ok(())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PlanInput {
    pub intent: String,
    pub primitives: Option<Vec<RequestedStep>>,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RequestedStep {
    pub kind: Kind,
    pub title: String,
    #[serde(default)]
    pub animated_image: bool,
}

pub(super) fn plan(input: PlanInput, models: &[Model]) -> Graph {
    let requested = input
        .primitives
        .filter(|steps| {
            (1..=8).contains(&steps.len())
                && steps
                    .iter()
                    .all(|step| !step.title.trim().is_empty() && step.title.len() <= 80)
        })
        .unwrap_or_else(|| local_steps(&input.intent));
    let mut nodes: Vec<Node> = Vec::new();
    let mut exports = BTreeMap::new();
    for (index, step) in requested.into_iter().enumerate() {
        let id = format!("{}_{index}", label(step.kind));
        let first_frame = step
            .animated_image
            .then(|| nodes.iter().rev().find(|node| node.kind == Kind::Image))
            .flatten();
        let selected = models
            .iter()
            .find(|model| {
                model.kind == step.kind && (first_frame.is_none() || model.accepts_first_frame)
            })
            .or_else(|| models.iter().find(|model| model.kind == step.kind));
        let mut settings = BTreeMap::from([(
            "prompt".into(),
            nodes
                .iter()
                .rev()
                .find(|node| node.kind == Kind::Text)
                .map(|node| Binding::Step {
                    step: node.id.clone(),
                })
                .unwrap_or_else(|| Binding::Request {
                    field: "brief".into(),
                }),
        )]);
        let fixed = |value| Binding::Fixed { value };
        if step.kind == Kind::Text {
            settings.insert(
                "system".into(),
                fixed(Value::Text(input.intent.trim().to_owned())),
            );
            settings.insert("max_output_tokens".into(), fixed(Value::Integer(256)));
        } else {
            let aspect = selected
                .and_then(|model| model.aspect_ratios.first())
                .map(String::as_str)
                .unwrap_or("16:9");
            settings.insert("aspect_ratio".into(), fixed(Value::Text(aspect.into())));
        }
        if step.kind == Kind::Video {
            let duration = selected
                .and_then(|model| model.durations.first())
                .copied()
                .unwrap_or(5);
            let resolution = selected
                .and_then(|model| model.resolutions.first())
                .map(String::as_str)
                .unwrap_or("540p");
            settings.insert(
                "duration_seconds".into(),
                fixed(Value::Integer(i64::from(duration))),
            );
            settings.insert("resolution".into(), fixed(Value::Text(resolution.into())));
            if let Some(source) = first_frame {
                settings.insert(
                    "first_frame".into(),
                    Binding::Step {
                        step: source.id.clone(),
                    },
                );
            }
        }
        exports.insert(id.clone(), id.clone());
        nodes.push(Node {
            id,
            title: step.title,
            kind: step.kind,
            model: selected.map(|model| model.id.clone()).unwrap_or_default(),
            max_usd_nanos: match step.kind {
                Kind::Text => 50_000_000,
                Kind::Image => 250_000_000,
                Kind::Video => 1_000_000_000,
                Kind::Tool => 0,
            },
            settings,
            tool: None,
        });
    }
    let title = if nodes.len() == 1 {
        nodes[0].title.clone()
    } else {
        "Untitled workflow".into()
    };
    Graph {
        schema: 1,
        title,
        inputs: BTreeMap::from([(
            "brief".into(),
            InputField {
                title: "Brief".into(),
                kind: ValueType::Text,
                default: None,
            },
        )]),
        max_usd_nanos: nodes.iter().map(|node| node.max_usd_nanos).sum(),
        nodes,
        outputs: exports,
    }
}

fn label(kind: Kind) -> &'static str {
    match kind {
        Kind::Text => "text",
        Kind::Image => "image",
        Kind::Video => "video",
        Kind::Tool => "tool",
    }
}

fn local_steps(intent: &str) -> Vec<RequestedStep> {
    let intent = intent.to_ascii_lowercase();
    let words: BTreeSet<_> = intent.split(|c: char| !c.is_ascii_alphanumeric()).collect();
    let includes = |candidates: &[&str]| candidates.iter().any(|word| words.contains(word));
    let animation = includes(&["animate", "motion", "animation"]);
    let image = includes(&[
        "image",
        "images",
        "hero",
        "poster",
        "photo",
        "illustration",
        "still",
        "artwork",
    ]);
    let video = animation || includes(&["video", "clip", "trailer"]);
    let text = includes(&[
        "caption",
        "copy",
        "summarize",
        "rewrite",
        "translate",
        "script",
        "outline",
        "describe",
        "write",
    ]);
    let mut steps = Vec::new();
    if text || (!image && !video) {
        steps.push(RequestedStep {
            kind: Kind::Text,
            title: "Write copy".into(),
            animated_image: false,
        });
    }
    if image {
        steps.push(RequestedStep {
            kind: Kind::Image,
            title: "Generate hero".into(),
            animated_image: false,
        });
    }
    if video {
        steps.push(RequestedStep {
            kind: Kind::Video,
            title: if image {
                "Animate hero"
            } else {
                "Generate video"
            }
            .into(),
            animated_image: image,
        });
    }
    steps
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn intent_builds_the_expected_primitives() {
        let steps = local_steps("Write a caption, create a hero image, then animate the hero.");
        assert_eq!(
            steps.iter().map(|step| step.kind).collect::<Vec<_>>(),
            [Kind::Text, Kind::Image, Kind::Video]
        );
        assert!(steps[2].animated_image);
    }
}
