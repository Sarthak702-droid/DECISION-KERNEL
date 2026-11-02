//! Bounded declarative input contracts and pure, ordered state preparation.
use crate::CompileError;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

/// Deliberately small schema language; unsupported JSON Schema keywords are rejected.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum InputSchema {
    Object {
        properties: BTreeMap<String, InputSchema>,
        #[serde(default)]
        required: Vec<String>,
        #[serde(default)]
        additional_properties: bool,
    },
    String {
        #[serde(default)]
        values: Vec<String>,
        #[serde(default)]
        min_length: usize,
        #[serde(default = "default_length")]
        max_length: usize,
    },
    Number {
        minimum: Option<f64>,
        maximum: Option<f64>,
    },
    Integer {
        minimum: Option<i64>,
        maximum: Option<i64>,
    },
    Boolean,
    Array {
        items: Box<InputSchema>,
        #[serde(default = "default_items")]
        max_items: usize,
    },
}
fn default_length() -> usize {
    4096
}
fn default_items() -> usize {
    128
}
impl InputSchema {
    pub(crate) fn check(&self, depth: usize, count: &mut usize) -> Result<(), CompileError> {
        *count += 1;
        if depth > 8 || *count > 128 {
            return Err(CompileError::Limit("input schema"));
        }
        let valid = match self {
            Self::Object {
                properties,
                required,
                ..
            } => {
                for schema in properties.values() {
                    schema.check(depth + 1, count)?;
                }
                properties.keys().all(|k| !k.is_empty() && k.len() <= 128)
                    && required.iter().all(|k| properties.contains_key(k))
                    && required.iter().collect::<BTreeSet<_>>().len() == required.len()
            }
            Self::String {
                values,
                min_length,
                max_length,
            } => {
                min_length <= max_length
                    && *max_length <= 65536
                    && values.len() <= 32
                    && values.iter().collect::<BTreeSet<_>>().len() == values.len()
                    && values
                        .iter()
                        .all(|v| (*min_length..=*max_length).contains(&v.chars().count()))
            }
            Self::Number { minimum, maximum } => {
                minimum.iter().chain(maximum).all(|n| n.is_finite())
                    && !matches!((minimum, maximum), (Some(a), Some(b)) if a > b)
            }
            Self::Integer { minimum, maximum } => {
                !matches!((minimum, maximum), (Some(a), Some(b)) if a > b)
            }
            Self::Array { items, max_items } => {
                items.check(depth + 1, count)?;
                *max_items <= 128
            }
            Self::Boolean => true,
        };
        if valid {
            Ok(())
        } else {
            Err(CompileError::Type("invalid input schema".into()))
        }
    }
    pub fn accepts(&self, value: &Value) -> bool {
        match self {
            Self::Object {
                properties,
                required,
                additional_properties,
            } => value.as_object().is_some_and(|o| {
                required.iter().all(|k| o.contains_key(k))
                    && o.iter().all(|(k, v)| {
                        properties
                            .get(k)
                            .map_or(*additional_properties, |s| s.accepts(v))
                    })
            }),
            Self::String {
                values,
                min_length,
                max_length,
            } => value.as_str().is_some_and(|s| {
                (*min_length..=*max_length).contains(&s.chars().count())
                    && (values.is_empty() || values.iter().any(|v| v == s))
            }),
            Self::Number { minimum, maximum } => value.as_f64().is_some_and(|n| {
                n.is_finite() && minimum.is_none_or(|m| n >= m) && maximum.is_none_or(|m| n <= m)
            }),
            Self::Integer { minimum, maximum } => value
                .as_i64()
                .is_some_and(|n| minimum.is_none_or(|m| n >= m) && maximum.is_none_or(|m| n <= m)),
            Self::Boolean => value.is_boolean(),
            Self::Array { items, max_items } => value
                .as_array()
                .is_some_and(|a| a.len() <= *max_items && a.iter().all(|v| items.accepts(v))),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StatePreparation {
    pub version: String,
    pub steps: Vec<PreparationStep>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum PreparationStep {
    Copy {
        from: String,
        to: String,
    },
    Default {
        field: String,
        value: Value,
    },
    NormalizeString {
        field: String,
        #[serde(default)]
        lowercase: bool,
    },
}
impl StatePreparation {
    pub(crate) fn check(&self) -> Result<(), CompileError> {
        if self.version != "kernel/state-v1" {
            return Err(CompileError::Schema);
        }
        if self.steps.len() > 32 {
            return Err(CompileError::Limit("state preparation"));
        }
        for step in &self.steps {
            let fields = match step {
                PreparationStep::Copy { from, to } => vec![from, to],
                PreparationStep::Default { field, .. }
                | PreparationStep::NormalizeString { field, .. } => vec![field],
            };
            if fields.iter().any(|s| s.is_empty() || s.len() > 128) {
                return Err(CompileError::Type("invalid preparation field".into()));
            }
        }
        Ok(())
    }
    pub fn apply(&self, state: &mut BTreeMap<String, Value>) -> Result<(), &'static str> {
        for step in &self.steps {
            match step {
                PreparationStep::Copy { from, to } => {
                    let value = state.get(from).ok_or("missing copy source")?.clone();
                    state.insert(to.clone(), value);
                }
                PreparationStep::Default { field, value } => {
                    state.entry(field.clone()).or_insert_with(|| value.clone());
                }
                PreparationStep::NormalizeString { field, lowercase } => {
                    let value = state
                        .get(field)
                        .and_then(Value::as_str)
                        .ok_or("normalization requires string")?
                        .trim_matches(|c: char| c.is_ascii_whitespace());
                    let value = if *lowercase {
                        value.to_ascii_lowercase()
                    } else {
                        value.to_owned()
                    };
                    state.insert(field.clone(), Value::String(value));
                }
            }
        }
        Ok(())
    }
}
