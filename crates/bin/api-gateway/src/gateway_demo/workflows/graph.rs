use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

pub(super) const MAX_NODES: usize = 20;
pub(super) const MAX_INPUT_BYTES: usize = 4000;
pub(super) const MAX_COST: i64 = 100_000_000_000;
pub(super) type InputValues = BTreeMap<String, Value>;

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(super) enum Kind {
    Text,
    Image,
    Video,
    Tool,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(super) enum ValueType {
    Text,
    Integer,
    Image,
    Video,
    Audio,
    Media,
    Metadata,
    Mesh,
    Document,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(untagged)]
pub(super) enum Value {
    Text(String),
    Integer(i64),
}

impl Value {
    fn matches(&self, kind: ValueType) -> bool {
        match (self, kind) {
            (Self::Text(text), ValueType::Text) => {
                !text.trim().is_empty() && text.len() <= MAX_INPUT_BYTES
            }
            (Self::Integer(number), ValueType::Integer) => (1..=32000).contains(number),
            _ => false,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "source", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum Binding {
    Fixed { value: Value },
    Request { field: String },
    Step { step: String },
    Template { text: String },
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(super) struct InputField {
    pub title: String,
    pub kind: ValueType,
    pub default: Option<Value>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Node {
    pub id: String,
    pub title: String,
    pub kind: Kind,
    pub model: String,
    pub max_usd_nanos: i64,
    pub settings: BTreeMap<String, Binding>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<ToolStep>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ToolStep {
    pub binding_revision: String,
    pub revision: u32,
    pub inputs: BTreeMap<String, ValueType>,
    pub output: ValueType,
    pub parameters: BTreeMap<String, serde_json::Value>,
}

impl ValueType {
    pub(super) fn file(self) -> bool {
        matches!(self, Self::Image | Self::Video | Self::Audio | Self::Mesh | Self::Document)
    }

    fn accepts(self, source: Self) -> bool {
        self == source || self == Self::Media && source.file()
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Graph {
    pub schema: u8,
    pub title: String,
    pub inputs: BTreeMap<String, InputField>,
    pub nodes: Vec<Node>,
    pub outputs: BTreeMap<String, String>,
    pub max_usd_nanos: i64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(super) enum Output {
    Text { text: String },
    Image { request_id: ids::GatewayRequestId },
    Video { request_id: ids::GatewayRequestId },
    Fixture { kind: ValueType },
    Tool { node: String, kind: ValueType },
}

impl Output {
    pub fn value(&self) -> Option<Value> {
        match self {
            Self::Text { text } => Some(Value::Text(text.clone())),
            Self::Image { .. } | Self::Video { .. } | Self::Fixture { .. } | Self::Tool { .. } => None,
        }
    }
}

#[derive(Clone, Copy, Serialize)]
pub(super) struct Setting {
    pub name: &'static str,
    pub label: &'static str,
    pub kind: ValueType,
    pub required: bool,
}

const PROMPT: Setting = Setting {
    name: "prompt",
    label: "Prompt",
    kind: ValueType::Text,
    required: true,
};
const ASPECT: Setting = Setting {
    name: "aspect_ratio",
    label: "Aspect ratio",
    kind: ValueType::Text,
    required: true,
};
const SYSTEM: Setting = Setting {
    name: "system",
    label: "System instruction",
    kind: ValueType::Text,
    required: false,
};
const MAX_OUTPUT: Setting = Setting {
    name: "max_output_tokens",
    label: "Maximum output tokens",
    kind: ValueType::Integer,
    required: true,
};
const DURATION: Setting = Setting {
    name: "duration_seconds",
    label: "Duration in seconds",
    kind: ValueType::Integer,
    required: true,
};
const RESOLUTION: Setting = Setting {
    name: "resolution",
    label: "Resolution",
    kind: ValueType::Text,
    required: true,
};
const FRAME: Setting = Setting {
    name: "first_frame",
    label: "First frame",
    kind: ValueType::Image,
    required: false,
};

impl Kind {
    pub fn settings(self) -> &'static [Setting] {
        match self {
            Self::Text => &[PROMPT, SYSTEM, MAX_OUTPUT],
            Self::Image => &[PROMPT, ASPECT],
            Self::Video => &[PROMPT, ASPECT, DURATION, RESOLUTION, FRAME],
            Self::Tool => &[],
        }
    }

    pub fn output(self) -> ValueType {
        match self {
            Self::Text => ValueType::Text,
            Self::Image => ValueType::Image,
            Self::Video => ValueType::Video,
            Self::Tool => ValueType::Media,
        }
    }
}

pub(super) struct Plan<'a> {
    pub nodes: Vec<&'a Node>,
}

impl Graph {
    pub fn compile(&self) -> Result<Plan<'_>, String> {
        if self.schema != 1 {
            return Err("This workflow definition uses an unsupported schema version.".into());
        }
        if self.title.trim().is_empty()
            || self.title.len() > 100
            || !(1..=MAX_NODES).contains(&self.nodes.len())
            || self.inputs.len() > 24
            || !(1..=20).contains(&self.outputs.len())
            || !(0..=MAX_COST).contains(&self.max_usd_nanos)
        {
            return Err(
                "Check the name, input and step limits, outputs, and run spending cap.".into(),
            );
        }
        for (name, field) in &self.inputs {
            if !identifier(name)
                || field.title.trim().is_empty()
                || field.title.len() > 100
                || !matches!(field.kind, ValueType::Text | ValueType::Integer)
                || field
                    .default
                    .as_ref()
                    .is_some_and(|value| !value.matches(field.kind))
            {
                return Err(format!("Invalid request input {name}."));
            }
        }

        let mut nodes = BTreeMap::new();
        let mut cost = 0;
        for node in &self.nodes {
            if !identifier(&node.id)
                || node.title.trim().is_empty()
                || node.title.len() > 100
                || !node.valid_model()
                || !(0..=MAX_COST).contains(&node.max_usd_nanos)
                || nodes.insert(node.id.as_str(), node).is_some()
            {
                return Err("Every step needs a unique ID, model, title, and spending cap.".into());
            }
            cost += node.max_usd_nanos;
        }
        if cost > self.max_usd_nanos {
            return Err("The step spending caps add up to more than the run cap.".into());
        }
        for (name, source) in &self.outputs {
            if !identifier(name) || !nodes.contains_key(source.as_str()) {
                return Err(format!("Output {name} needs a valid source step."));
            }
        }

        let mut dependencies = BTreeMap::new();
        for node in &self.nodes {
            let mut required = BTreeSet::new();
            if let Some(tool) = &node.tool {
                for (slot, kind) in &tool.inputs {
                    let Some(binding @ Binding::Step { .. }) = node.settings.get(slot) else {
                        return Err(format!("{} needs a media step for {slot}.", node.title));
                    };
                    self.validate_binding(binding, *kind, &nodes, &mut required)
                        .map_err(|error| format!("{} · {slot}. {error}", node.title))?;
                }
                if node.settings.len() != tool.inputs.len() {
                    return Err(format!("{} has an unknown input slot.", node.title));
                }
                dependencies.insert(node.id.as_str(), required);
                continue;
            }
            for setting in node.kind.settings() {
                match node.settings.get(setting.name) {
                    Some(binding) => {
                        self.validate_binding(binding, setting.kind, &nodes, &mut required)
                            .map_err(|error| {
                                format!("{} · {}. {error}", node.title, setting.label)
                            })?;
                    }
                    None if setting.required => {
                        return Err(format!("{} requires {}.", node.title, setting.label))
                    }
                    None => {}
                }
            }
            if node.settings.keys().any(|name| {
                !node
                    .kind
                    .settings()
                    .iter()
                    .any(|setting| setting.name == name)
            }) {
                return Err(format!("{} has an unsupported setting.", node.title));
            }
            dependencies.insert(node.id.as_str(), required);
        }

        let mut plan = Vec::with_capacity(nodes.len());
        let mut done = BTreeSet::new();
        while plan.len() < nodes.len() {
            let next = self
                .nodes
                .iter()
                .find(|node| {
                    !done.contains(node.id.as_str())
                        && dependencies[node.id.as_str()].is_subset(&done)
                })
                .ok_or_else(|| "Remove the circular step connection.".to_owned())?;
            done.insert(next.id.as_str());
            plan.push(next);
        }
        let mut used: BTreeSet<_> = self.outputs.values().map(String::as_str).collect();
        for node in plan.iter().rev() {
            if used.contains(node.id.as_str()) {
                used.extend(dependencies[node.id.as_str()].iter().copied());
            }
        }
        if let Some(node) = plan.iter().find(|node| !used.contains(node.id.as_str())) {
            return Err(format!(
                "Connect {} to a workflow output or remove the step.",
                node.title
            ));
        }
        Ok(Plan { nodes: plan })
    }

    fn validate_binding<'a>(
        &'a self,
        binding: &'a Binding,
        kind: ValueType,
        nodes: &BTreeMap<&str, &Node>,
        required: &mut BTreeSet<&'a str>,
    ) -> Result<(), &'static str> {
        let request = |field: &str| {
            self.inputs
                .get(field)
                .filter(|definition| definition.kind == kind)
                .map(|_| ())
                .ok_or("Choose a request input of the correct type.")
        };
        let step = |step: &'a str, required: &mut BTreeSet<&'a str>| {
            if nodes
                .get(step)
                .is_some_and(|node| kind.accepts(node.output()))
            {
                required.insert(step);
                Ok(())
            } else {
                Err("Connect a step with the correct output type.")
            }
        };
        match binding {
            Binding::Request { field } => request(field),
            Binding::Step { step: source } => step(source, required),
            Binding::Fixed { value } if value.matches(kind) => Ok(()),
            Binding::Template { text } if kind == ValueType::Text => {
                template(text, |source| match source {
                    Reference::Input(field) => request(field).map(|_| String::new()),
                    Reference::Step(source) => step(source, required).map(|_| String::new()),
                })
                .map(|_| ())
            }
            _ => Err("Set a value of the correct type."),
        }
    }

    pub fn resolve_inputs(&self, mut supplied: InputValues) -> Result<InputValues, String> {
        if let Some(name) = supplied
            .keys()
            .find(|name| !self.inputs.contains_key(*name))
        {
            return Err(format!("Unknown request input {name}."));
        }
        for (name, field) in &self.inputs {
            let value = supplied
                .get(name)
                .or(field.default.as_ref())
                .filter(|value| value.matches(field.kind))
                .ok_or_else(|| format!("Supply a valid value for {name}."))?;
            let value = value.clone();
            supplied.entry(name.clone()).or_insert(value);
        }
        Ok(supplied)
    }

    pub fn contract_matches(&self, previous: &Self) -> bool {
        self.inputs.len() == previous.inputs.len()
            && self.inputs.iter().all(|(name, field)| {
                previous
                    .inputs
                    .get(name)
                    .is_some_and(|old| old.kind == field.kind && old.default == field.default)
            })
            && self.outputs.len() == previous.outputs.len()
            && self.outputs.iter().all(|(name, step)| {
                previous
                    .outputs
                    .get(name)
                    .is_some_and(|old| self.output_kind(step) == previous.output_kind(old))
            })
    }

    pub fn output_kind(&self, step: &str) -> Option<ValueType> {
        self.nodes
            .iter()
            .find(|node| node.id == step)
            .map(Node::output)
    }
}

pub(super) fn identifier(value: &str) -> bool {
    (1..=48).contains(&value.len())
        && value.as_bytes()[0].is_ascii_lowercase()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
}

enum Reference<'a> {
    Input(&'a str),
    Step(&'a str),
}

fn template<'a>(
    mut text: &'a str,
    mut resolve: impl FnMut(Reference<'a>) -> Result<String, &'static str>,
) -> Result<String, &'static str> {
    if text.trim().is_empty() || text.len() > MAX_INPUT_BYTES {
        return Err("Enter a template within the text limit.");
    }
    let mut output = String::new();
    while let Some((literal, rest)) = text.split_once("{{") {
        if literal.contains("}}") {
            return Err("Check the template braces.");
        }
        let (reference, remainder) = rest
            .split_once("}}")
            .ok_or("Close the template reference.")?;
        let (source, name) = reference
            .trim()
            .split_once('.')
            .ok_or("Use input.name or step.name in templates.")?;
        if !identifier(name) {
            return Err("Use a valid input or step name in templates.");
        }
        let value = resolve(match source {
            "input" => Reference::Input(name),
            "step" => Reference::Step(name),
            _ => return Err("Use input.name or step.name in templates."),
        })?;
        output.push_str(literal);
        output.push_str(&value);
        if output.len() > MAX_INPUT_BYTES {
            return Err("The resolved template is too long.");
        }
        text = remainder;
    }
    if text.contains("}}") || output.len() + text.len() > MAX_INPUT_BYTES {
        return Err("Check the template braces and text limit.");
    }
    output.push_str(text);
    Ok(output)
}

impl Node {
    fn valid_model(&self) -> bool {
        let identifier = super::super::catalog::valid_identifier;
        match (&self.tool, self.kind) {
            (Some(tool), Kind::Tool) => {
                self.max_usd_nanos == 0 && self.model.len() <= 96
                    && self.model.split('.').all(identifier)
                    && tool.revision > 0
                    && tool.binding_revision.len() == 64
                    && tool.binding_revision.bytes().all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
                    && (1..=8).contains(&tool.inputs.len())
                    && tool.inputs.iter().all(|(name, kind)| super::graph::identifier(name)
                        && (kind.file() || *kind == ValueType::Media))
                    && (tool.output.file() || tool.output == ValueType::Metadata)
            }
            (None, Kind::Tool) | (Some(_), _) => false,
            (None, _) => identifier(&self.model) && self.model != "auto",
        }
    }

    pub(super) fn output(&self) -> ValueType {
        self.tool.as_ref().map_or_else(|| self.kind.output(), |tool| tool.output)
    }

    pub(super) fn dependencies(&self) -> Vec<String> {
        let mut sources = BTreeSet::new();
        for binding in self.settings.values() {
            match binding {
                Binding::Step { step } => {
                    sources.insert(step.as_str());
                }
                Binding::Template { text } => {
                    let _ = template(text, |reference| {
                        if let Reference::Step(step) = reference {
                            sources.insert(step);
                        }
                        Ok(String::new())
                    });
                }
                _ => {}
            }
        }
        sources.into_iter().map(str::to_owned).collect()
    }

    pub fn resolve(
        &self,
        field: &str,
        inputs: &InputValues,
        outputs: &BTreeMap<String, Output>,
    ) -> Result<Option<Value>, &'static str> {
        let Some(binding) = self.settings.get(field) else {
            return Ok(None);
        };
        let step = |source: &str| outputs.get(source).and_then(Output::value);
        let value = match binding {
            Binding::Fixed { value } => Some(value.clone()),
            Binding::Request { field } => inputs.get(field).cloned(),
            Binding::Step { step: source } => step(source),
            Binding::Template { text } => Some(Value::Text(template(text, |reference| {
                match reference {
                    Reference::Input(name) => inputs.get(name).cloned(),
                    Reference::Step(name) => step(name),
                }
                .and_then(|value| match value {
                    Value::Text(text) => Some(text),
                    Value::Integer(_) => None,
                })
                .ok_or("A template value is unavailable.")
            })?)),
        };
        value.map(Some).ok_or("A bound value is unavailable.")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn templates_resolve_only_named_values() {
        let result = template("Hello {{input.name}} / {{step.copy}}", |reference| {
            Ok(match reference {
                Reference::Input(value) | Reference::Step(value) => value.to_owned(),
            })
        });
        assert_eq!(result.unwrap(), "Hello name / copy");
        assert!(template("{{environment.secret}}", |_| panic!()).is_err());
        assert!(template("{{input.test", |_| panic!()).is_err());
    }
}
