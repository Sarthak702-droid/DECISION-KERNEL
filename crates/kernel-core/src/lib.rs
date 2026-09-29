//! Provider-free contracts and deterministic policy for Decision Kernel.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use thiserror::Error;

/// Parse JSON while rejecting duplicate keys at every nesting level.
pub fn strict_json(bytes: &[u8]) -> Result<Value, serde_json::Error> {
    use serde::Deserialize;
    struct Strict(Value);
    impl<'de> Deserialize<'de> for Strict {
        fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
            use serde::de::{Error, MapAccess, SeqAccess, Visitor};
            struct V;
            impl<'de> Visitor<'de> for V {
                type Value = Strict;
                fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                    f.write_str("JSON without duplicate object keys")
                }
                fn visit_bool<E: Error>(self, v: bool) -> Result<Strict, E> {
                    Ok(Strict(Value::Bool(v)))
                }
                fn visit_i64<E: Error>(self, v: i64) -> Result<Strict, E> {
                    Ok(Strict(Value::from(v)))
                }
                fn visit_u64<E: Error>(self, v: u64) -> Result<Strict, E> {
                    Ok(Strict(Value::from(v)))
                }
                fn visit_f64<E: Error>(self, v: f64) -> Result<Strict, E> {
                    serde_json::Number::from_f64(v)
                        .map(|n| Strict(Value::Number(n)))
                        .ok_or_else(|| E::custom("non-finite number"))
                }
                fn visit_str<E: Error>(self, v: &str) -> Result<Strict, E> {
                    Ok(Strict(Value::String(v.into())))
                }
                fn visit_string<E: Error>(self, v: String) -> Result<Strict, E> {
                    Ok(Strict(Value::String(v)))
                }
                fn visit_none<E: Error>(self) -> Result<Strict, E> {
                    Ok(Strict(Value::Null))
                }
                fn visit_unit<E: Error>(self) -> Result<Strict, E> {
                    Ok(Strict(Value::Null))
                }
                fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Strict, A::Error> {
                    let mut out = Vec::new();
                    while let Some(v) = seq.next_element::<Strict>()? {
                        out.push(v.0);
                    }
                    Ok(Strict(Value::Array(out)))
                }
                fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Strict, A::Error> {
                    let mut out = serde_json::Map::new();
                    while let Some((k, v)) = map.next_entry::<String, Strict>()? {
                        if out.insert(k, v.0).is_some() {
                            return Err(A::Error::custom("duplicate JSON key"));
                        }
                    }
                    Ok(Strict(Value::Object(out)))
                }
            }
            deserializer.deserialize_any(V)
        }
    }
    let mut de = serde_json::Deserializer::from_slice(bytes);
    let value = Strict::deserialize(&mut de)?.0;
    de.end()?;
    Ok(value)
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Disposition {
    Accept,
    Review,
    Deny,
    Abstain,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReasonCode {
    HardPolicyDeny,
    RevokedRelease,
    MissingEvidence,
    StaleEvidence,
    InvalidEvidence,
    DeterministicResolution,
    QualifiedSemanticResolution,
    UnqualifiedSemantic,
    ProviderUnavailable,
    ProviderTimeout,
    InvalidProviderOutput,
    DeadlineExceeded,
    BudgetUnavailable,
    NotEvaluated,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "source", rename_all = "snake_case", deny_unknown_fields)]
pub enum ProbabilityProvenance {
    ProviderNative,
    ModelReported,
    Derived { method: String, version: String },
    Calibrated { calibrator_ref: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Answer {
    Categorical {
        selected_id: String,
        probabilities: Option<BTreeMap<String, f64>>,
        #[serde(default)]
        probability_provenance: Option<ProbabilityProvenance>,
    },
    Binary {
        proposition: String,
        value: Option<bool>,
        probability_true: Option<f64>,
        #[serde(default)]
        probability_provenance: Option<ProbabilityProvenance>,
    },
    Ordinal {
        scale_id: String,
        selected_anchor: String,
        distribution: Option<Vec<f64>>,
        #[serde(default)]
        probability_provenance: Option<ProbabilityProvenance>,
    },
    NotEvaluated {
        reason: ReasonCode,
    },
}
impl Answer {
    pub fn is_evaluated(&self) -> bool {
        match self {
            Self::Binary { value, .. } => value.is_some(),
            Self::NotEvaluated { .. } => false,
            _ => true,
        }
    }
    pub fn has_probability(&self) -> bool {
        match self {
            Self::Categorical { probabilities, .. } => probabilities.is_some(),
            Self::Binary {
                probability_true, ..
            } => probability_true.is_some(),
            Self::Ordinal { distribution, .. } => distribution.is_some(),
            Self::NotEvaluated { .. } => false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum OutputDefinition {
    Categorical {
        id: String,
        vocabulary: Vec<String>,
    },
    Binary {
        id: String,
        proposition: String,
    },
    Ordinal {
        id: String,
        scale_id: String,
        anchors: Vec<String>,
    },
}
impl OutputDefinition {
    pub fn id(&self) -> &str {
        match self {
            Self::Categorical { id, .. } | Self::Binary { id, .. } | Self::Ordinal { id, .. } => id,
        }
    }
}

#[derive(Debug, Error)]
pub enum ContractError {
    #[error("answer does not match output {0}")]
    Kind(String),
    #[error("invalid label or proposition for {0}")]
    Label(String),
    #[error("invalid probability for {0}")]
    Probability(String),
}

pub fn validate_answer(def: &OutputDefinition, answer: &Answer) -> Result<(), ContractError> {
    let id = def.id().to_owned();
    match (def, answer) {
        (_, Answer::NotEvaluated { .. }) => Ok(()),
        (
            OutputDefinition::Categorical { vocabulary, .. },
            Answer::Categorical {
                selected_id,
                probabilities,
                probability_provenance,
            },
        ) => {
            if !vocabulary.contains(selected_id) {
                return Err(ContractError::Label(id));
            }
            if probabilities.is_some() != probability_provenance.is_some() {
                return Err(ContractError::Probability(id));
            }
            if let Some(p) = probabilities {
                let keys: BTreeSet<_> = p.keys().collect();
                if keys != vocabulary.iter().collect() {
                    return Err(ContractError::Label(id));
                }
                let sum: f64 = p.values().sum();
                if p.values()
                    .any(|v| !v.is_finite() || !(0.0..=1.0).contains(v))
                    || (sum - 1.0).abs() > 0.001
                {
                    return Err(ContractError::Probability(id));
                }
            }
            Ok(())
        }
        (
            OutputDefinition::Binary { proposition, .. },
            Answer::Binary {
                proposition: got,
                probability_true,
                probability_provenance,
                ..
            },
        ) => {
            if proposition != got {
                return Err(ContractError::Label(id));
            }
            if probability_true.is_some() != probability_provenance.is_some() {
                return Err(ContractError::Probability(id));
            }
            if probability_true.is_some_and(|p| !p.is_finite() || !(0.0..=1.0).contains(&p)) {
                return Err(ContractError::Probability(id));
            }
            Ok(())
        }
        (
            OutputDefinition::Ordinal {
                scale_id, anchors, ..
            },
            Answer::Ordinal {
                scale_id: got,
                selected_anchor,
                distribution,
                probability_provenance,
            },
        ) => {
            if scale_id != got || !anchors.contains(selected_anchor) {
                return Err(ContractError::Label(id));
            }
            if distribution.is_some() != probability_provenance.is_some() {
                return Err(ContractError::Probability(id));
            }
            if let Some(p) = distribution {
                let sum: f64 = p.iter().sum();
                if p.len() != anchors.len()
                    || p.iter().any(|v| !v.is_finite() || !(0.0..=1.0).contains(v))
                    || (sum - 1.0).abs() > 0.001
                {
                    return Err(ContractError::Probability(id));
                }
            }
            Ok(())
        }
        _ => Err(ContractError::Kind(id)),
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvidenceRef {
    pub id: String,
    pub revision: u64,
    pub observed_at_unix_ms: u64,
    pub expires_at_unix_ms: Option<u64>,
    pub digest_sha256: String,
    pub sensitivity: String,
    pub value: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DecisionRequest {
    pub schema_version: String,
    pub decision_type: String,
    pub release: String,
    pub idempotency_key: String,
    pub state: BTreeMap<String, Value>,
    pub evidence: Vec<EvidenceRef>,
    pub constraints: RequestLimits,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QueuedDecision {
    pub scope: Scope,
    pub schema_version: String,
    pub decision_type: String,
    pub release: String,
    pub release_digest_sha256: String,
    pub idempotency_key: String,
    pub state: BTreeMap<String, Value>,
    pub evidence_refs: Vec<(String, u64)>,
    pub constraints: RequestLimits,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequestLimits {
    pub deadline_ms: Option<u64>,
    pub max_billable_attempts: Option<u8>,
    pub max_cost_nano_usd: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Scope {
    pub tenant_id: String,
    pub project_id: String,
    pub environment: String,
    pub product_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DecisionReceipt {
    pub receipt_id: String,
    pub scope: Scope,
    pub request_digest_sha256: String,
    pub release: String,
    pub release_digest_sha256: String,
    pub answers: BTreeMap<String, Answer>,
    pub disposition: Disposition,
    pub reason_codes: Vec<ReasonCode>,
    pub evidence_revisions: BTreeMap<String, u64>,
    pub provider_id: Option<String>,
    pub model_id: Option<String>,
    #[serde(default)]
    pub returned_model_id: Option<String>,
    pub attempts: u8,
    pub prediction_cache_hit: bool,
    pub cost_nano_usd: Option<u64>,
    pub provider_reported_cost_nano_usd: Option<u64>,
    pub created_at_unix_ms: u64,
    pub kernel_compute_ms: u64,
    pub db_claim_ms: u64,
    pub provider_latency_ms: Option<u64>,
    pub db_commit_ms: Option<u64>,
    pub total_latency_ms: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn discrete_true_has_no_fabricated_probability() {
        let d = OutputDefinition::Binary {
            id: "urgent".into(),
            proposition: "needs_review".into(),
        };
        let a = Answer::Binary {
            proposition: "needs_review".into(),
            value: Some(true),
            probability_true: None,
            probability_provenance: None,
        };
        assert!(validate_answer(&d, &a).is_ok());
    }
    #[test]
    fn invalid_distribution_is_rejected() {
        let d = OutputDefinition::Categorical {
            id: "queue".into(),
            vocabulary: vec!["a".into(), "b".into()],
        };
        let a = Answer::Categorical {
            selected_id: "a".into(),
            probabilities: Some(BTreeMap::from([("a".into(), 0.9), ("b".into(), 0.9)])),
            probability_provenance: Some(ProbabilityProvenance::ProviderNative),
        };
        assert!(validate_answer(&d, &a).is_err());
    }
    #[test]
    fn duplicate_nested_json_is_rejected() {
        assert!(strict_json(br#"{"a":{"x":1,"x":2}}"#).is_err());
    }
}
