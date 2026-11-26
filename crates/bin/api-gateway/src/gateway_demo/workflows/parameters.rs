use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(tag = "type", rename_all = "lowercase", deny_unknown_fields)]
pub(super) enum Constraint {
    Integer {
        minimum: i64,
        maximum: i64,
        #[serde(rename = "multipleOf", skip_serializing_if = "Option::is_none")]
        multiple_of: Option<i64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        default: Option<i64>,
    },
    Number {
        minimum: f64,
        maximum: f64,
        #[serde(skip_serializing_if = "Option::is_none")]
        default: Option<f64>,
    },
    String {
        #[serde(rename = "enum")]
        choices: BTreeSet<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        default: Option<String>,
    },
}

impl Constraint {
    pub(super) fn matches(&self, value: &Value) -> bool {
        match self {
            Self::Integer { minimum, maximum, multiple_of, .. } => value.as_i64().is_some_and(|number| {
                (*minimum..=*maximum).contains(&number)
                    && multiple_of.is_none_or(|stride| stride > 0 && number % stride == 0)
            }),
            Self::Number { minimum, maximum, .. } => value.as_f64().is_some_and(|number| {
                (*minimum..=*maximum).contains(&number)
            }),
            Self::String { choices, .. } => value.as_str().is_some_and(|value| choices.contains(value)),
        }
    }

    pub(super) fn valid(&self) -> bool {
        match self {
            Self::Integer { minimum, maximum, multiple_of, default } => {
                first_integer(*minimum, multiple_of.unwrap_or(1)).is_some_and(|first| first <= *maximum)
                    && default.is_none_or(|value| self.matches(&json!(value)))
            }
            Self::Number { minimum, maximum, default } => {
                minimum.is_finite() && maximum.is_finite() && minimum <= maximum
                    && default.is_none_or(|value| self.matches(&json!(value)))
            }
            Self::String { choices, default } => {
                (1..=64).contains(&choices.len())
                    && choices.iter().all(|choice| (1..=80).contains(&choice.len()))
                    && default.as_ref().is_none_or(|value| choices.contains(value))
            }
        }
    }

    pub(super) fn contract(&self) -> (Value, Value) {
        let example = match self {
            Self::Integer { minimum, multiple_of, default, .. } => {
                json!(default.or_else(|| first_integer(*minimum, multiple_of.unwrap_or(1))))
            }
            Self::Number { minimum, default, .. } => json!(default.unwrap_or(*minimum)),
            Self::String { choices, default } => json!(default.as_ref().or_else(|| choices.first())),
        };
        let mut schema = json!(self);
        if let Some(fields) = schema.as_object_mut() {
            fields.remove("default");
        }
        (schema, example)
    }
}

fn first_integer(minimum: i64, stride: i64) -> Option<i64> {
    if stride <= 0 {
        return None;
    }
    minimum.checked_add((stride - minimum.rem_euclid(stride)) % stride)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contract_examples_match_reviewed_rules() {
        for (source, expected) in [
            (json!({"type":"integer","minimum":-5,"maximum":9,"multipleOf":4}), json!(-4)),
            (json!({"type":"number","minimum":0.25,"maximum":2.0}), json!(0.25)),
            (json!({"type":"string","enum":["yes","no"],"default":"yes"}), json!("yes")),
        ] {
            let constraint: Constraint = serde_json::from_value(source).unwrap();
            assert!(constraint.valid());
            let (schema, example) = constraint.contract();
            assert_eq!(example, expected);
            assert!(constraint.matches(&example));
            assert!(schema.get("default").is_none());
        }
    }
}
