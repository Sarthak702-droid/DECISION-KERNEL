//! Publication-time validation and bounded DAG compilation.
use kernel_core::{Answer, OutputDefinition, validate_answer};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use thiserror::Error;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DecisionPack {
    pub api_version: String,
    pub kind: String,
    pub id: String,
    pub version: String,
    pub required_evidence: Vec<String>,
    #[serde(default)]
    pub permitted_provider_sensitivities: Vec<String>,
    #[serde(default)]
    pub semantic_state_fields: Vec<String>,
    pub outputs: Vec<OutputDefinition>,
    pub nodes: Vec<Node>,
    pub policy: Policy,
    pub limits: PackLimits,
    pub qualification: Qualification,
    #[serde(default)]
    pub qualification_criteria: Option<QualificationCriteria>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Node {
    pub id: String,
    #[serde(default)]
    pub depends_on: Vec<String>,
    #[serde(flatten)]
    pub operation: Operation,
}
impl<'de> Deserialize<'de> for Node {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        use serde::de::Error;
        #[derive(Deserialize)]
        struct Wire {
            id: String,
            #[serde(default)]
            depends_on: Vec<String>,
            #[serde(flatten)]
            operation: Operation,
        }
        let value = Value::deserialize(deserializer)?;
        let object = value
            .as_object()
            .ok_or_else(|| D::Error::custom("node must be an object"))?;
        let op = object
            .get("op")
            .and_then(Value::as_str)
            .ok_or_else(|| D::Error::custom("missing node op"))?;
        let extra: &[&str] = match op {
            "fact_equals" => &["field", "value"],
            "evidence_present" => &["evidence_id"],
            "boolean_and" | "boolean_or" => &[],
            "semantic_question" => &["output", "prompt"],
            "literal_answer" => &["output", "answer"],
            _ => return Err(D::Error::custom("unknown node operator")),
        };
        for key in object.keys() {
            if key != "id" && key != "depends_on" && key != "op" && !extra.contains(&key.as_str()) {
                return Err(D::Error::custom(format!("unknown node field: {key}")));
            }
        }
        let wire: Wire = serde_json::from_value(value).map_err(D::Error::custom)?;
        Ok(Node {
            id: wire.id,
            depends_on: wire.depends_on,
            operation: wire.operation,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum Operation {
    FactEquals { field: String, value: Value },
    EvidencePresent { evidence_id: String },
    BooleanAnd,
    BooleanOr,
    SemanticQuestion { output: String, prompt: String },
    LiteralAnswer { output: String, answer: Answer },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    #[serde(default)]
    pub hard_deny_nodes: Vec<String>,
    #[serde(default)]
    pub accept_nodes: Vec<String>,
    #[serde(default)]
    pub accept_deterministic: bool,
    #[serde(default)]
    pub accept_qualified_semantic: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Qualification {
    pub semantic_acceptance_qualified: bool,
    pub provider_id: Option<String>,
    pub model_id: Option<String>,
    #[serde(default)]
    pub adapter_version: Option<String>,
    #[serde(default)]
    pub qualification_ref: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QualificationCriteria {
    pub min_reviewed_test_cases: u64,
    pub min_cases_per_required_slice: u64,
    pub min_coverage: f64,
    pub min_accuracy: f64,
    pub required_slices: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackLimits {
    pub deadline_ms: u64,
    pub max_billable_attempts: u8,
    pub max_cost_nano_usd: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompiledDecisionGraph {
    pub pack: DecisionPack,
    pub execution_order: Vec<usize>,
    pub dependencies: Vec<Vec<usize>>,
    pub hard_deny_indices: Vec<usize>,
    pub accept_indices: Vec<usize>,
    pub semantic_batches: Vec<Vec<usize>>,
    pub content_hash_sha256: String,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum CompileError {
    #[error("unsupported pack schema or kind")]
    Schema,
    #[error("pack exceeds a bounded limit: {0}")]
    Limit(&'static str),
    #[error("duplicate or empty ID: {0}")]
    Duplicate(String),
    #[error("unknown reference: {0}")]
    Unknown(String),
    #[error("invalid node type or dependency: {0}")]
    Type(String),
    #[error("cycle in decision graph")]
    Cycle,
    #[error("serialization failed: {0}")]
    Serialize(String),
}

pub fn compile(pack: DecisionPack) -> Result<CompiledDecisionGraph, CompileError> {
    if pack.api_version != "kernel/v1"
        || pack.kind != "DecisionPack"
        || pack.id.is_empty()
        || pack.version.is_empty()
    {
        return Err(CompileError::Schema);
    }
    if pack.nodes.len() > 64 {
        return Err(CompileError::Limit("nodes"));
    }
    if pack.qualification.semantic_acceptance_qualified
        && (pack.qualification.provider_id.is_none()
            || pack.qualification.model_id.is_none()
            || pack.qualification.adapter_version.is_none()
            || pack.qualification.qualification_ref.is_none())
    {
        return Err(CompileError::Type(
            "semantic acceptance requires a complete qualification binding".into(),
        ));
    }
    if pack.qualification.semantic_acceptance_qualified && pack.qualification_criteria.is_none() {
        return Err(CompileError::Type(
            "semantic acceptance requires qualification criteria".into(),
        ));
    }
    if let Some(criteria) = &pack.qualification_criteria
        && (criteria.min_reviewed_test_cases == 0
            || criteria.min_cases_per_required_slice == 0
            || criteria.required_slices.is_empty()
            || criteria.required_slices.iter().any(String::is_empty)
            || criteria
                .required_slices
                .iter()
                .collect::<BTreeSet<_>>()
                .len()
                != criteria.required_slices.len()
            || !criteria.min_coverage.is_finite()
            || !(0.0..=1.0).contains(&criteria.min_coverage)
            || !criteria.min_accuracy.is_finite()
            || !(0.0..=1.0).contains(&criteria.min_accuracy))
    {
        return Err(CompileError::Type("invalid qualification criteria".into()));
    }
    if pack.semantic_state_fields.len() > 32
        || pack.semantic_state_fields.iter().any(String::is_empty)
        || pack
            .semantic_state_fields
            .iter()
            .collect::<BTreeSet<_>>()
            .len()
            != pack.semantic_state_fields.len()
    {
        return Err(CompileError::Limit("semantic state fields"));
    }
    if pack.permitted_provider_sensitivities.len() > 8
        || pack
            .permitted_provider_sensitivities
            .iter()
            .any(String::is_empty)
        || pack
            .permitted_provider_sensitivities
            .iter()
            .collect::<BTreeSet<_>>()
            .len()
            != pack.permitted_provider_sensitivities.len()
    {
        return Err(CompileError::Limit("sensitivity classes"));
    }
    if pack.outputs.len() > 8 {
        return Err(CompileError::Limit("outputs"));
    }
    if pack.limits.deadline_ms == 0 || pack.limits.max_billable_attempts > 2 {
        return Err(CompileError::Limit("execution limits"));
    }
    let mut outputs = BTreeMap::new();
    for (i, o) in pack.outputs.iter().enumerate() {
        if o.id().is_empty() || outputs.insert(o.id().to_owned(), i).is_some() {
            return Err(CompileError::Duplicate(o.id().into()));
        }
        match o {
            OutputDefinition::Categorical { vocabulary, .. }
                if vocabulary.is_empty()
                    || vocabulary.len() > 32
                    || vocabulary.iter().any(String::is_empty)
                    || vocabulary.iter().collect::<BTreeSet<_>>().len() != vocabulary.len() =>
            {
                return Err(CompileError::Type(o.id().into()));
            }
            OutputDefinition::Ordinal { anchors, .. }
                if anchors.len() < 2
                    || anchors.len() > 32
                    || anchors.iter().collect::<BTreeSet<_>>().len() != anchors.len() =>
            {
                return Err(CompileError::Type(o.id().into()));
            }
            _ => {}
        }
    }
    let mut ids = BTreeMap::new();
    for (i, n) in pack.nodes.iter().enumerate() {
        if n.id.is_empty() || ids.insert(n.id.clone(), i).is_some() {
            return Err(CompileError::Duplicate(n.id.clone()));
        }
    }
    let mut indegree = vec![0usize; pack.nodes.len()];
    let mut next = vec![Vec::new(); pack.nodes.len()];
    let mut dependencies = vec![Vec::new(); pack.nodes.len()];
    let mut edges = 0usize;
    let mut producers = BTreeSet::new();
    let mut semantics = Vec::new();
    for (i, n) in pack.nodes.iter().enumerate() {
        let mut local = BTreeSet::new();
        for dep in &n.depends_on {
            let &from = ids
                .get(dep)
                .ok_or_else(|| CompileError::Unknown(dep.clone()))?;
            if !local.insert(from) {
                return Err(CompileError::Duplicate(dep.clone()));
            }
            if !matches!(
                pack.nodes[from].operation,
                Operation::FactEquals { .. }
                    | Operation::EvidencePresent { .. }
                    | Operation::BooleanAnd
                    | Operation::BooleanOr
            ) {
                return Err(CompileError::Type(n.id.clone()));
            }
            indegree[i] += 1;
            dependencies[i].push(from);
            next[from].push(i);
            edges += 1;
        }
        match &n.operation {
            Operation::BooleanAnd | Operation::BooleanOr if n.depends_on.is_empty() => {
                return Err(CompileError::Type(n.id.clone()));
            }
            Operation::FactEquals { field, .. } if field.is_empty() || !n.depends_on.is_empty() => {
                return Err(CompileError::Type(n.id.clone()));
            }
            Operation::EvidencePresent { evidence_id }
                if !pack.required_evidence.contains(evidence_id) || !n.depends_on.is_empty() =>
            {
                return Err(CompileError::Unknown(evidence_id.clone()));
            }
            Operation::SemanticQuestion { output, prompt } => {
                if prompt.len() > 1024 || prompt.is_empty() {
                    return Err(CompileError::Limit("prompt"));
                }
                if !outputs.contains_key(output) {
                    return Err(CompileError::Unknown(output.clone()));
                }
                if !producers.insert(output.clone()) {
                    return Err(CompileError::Duplicate(output.clone()));
                }
                semantics.push(i);
            }
            Operation::LiteralAnswer { output, answer } => {
                let &idx = outputs
                    .get(output)
                    .ok_or_else(|| CompileError::Unknown(output.clone()))?;
                validate_answer(&pack.outputs[idx], answer)
                    .map_err(|_| CompileError::Type(output.clone()))?;
                if !producers.insert(output.clone()) {
                    return Err(CompileError::Duplicate(output.clone()));
                }
            }
            _ => {}
        }
    }
    if edges > 128 {
        return Err(CompileError::Limit("edges"));
    }
    if semantics.len() > 8 {
        return Err(CompileError::Limit("semantic questions"));
    }
    if producers.len() != outputs.len() {
        return Err(CompileError::Type("every output needs one producer".into()));
    }
    for id in pack
        .policy
        .hard_deny_nodes
        .iter()
        .chain(&pack.policy.accept_nodes)
    {
        let &idx = ids
            .get(id)
            .ok_or_else(|| CompileError::Unknown(id.clone()))?;
        if !matches!(
            pack.nodes[idx].operation,
            Operation::FactEquals { .. }
                | Operation::EvidencePresent { .. }
                | Operation::BooleanAnd
                | Operation::BooleanOr
        ) {
            return Err(CompileError::Type(id.clone()));
        }
    }
    let hard_deny_indices: Vec<usize> = pack
        .policy
        .hard_deny_nodes
        .iter()
        .map(|id| ids[id])
        .collect();
    let accept_indices: Vec<usize> = pack.policy.accept_nodes.iter().map(|id| ids[id]).collect();
    let mut ready: VecDeque<_> = indegree
        .iter()
        .enumerate()
        .filter_map(|(i, &d)| (d == 0).then_some(i))
        .collect();
    let mut order = Vec::with_capacity(pack.nodes.len());
    while let Some(i) = ready.pop_front() {
        order.push(i);
        for &j in &next[i] {
            indegree[j] -= 1;
            if indegree[j] == 0 {
                ready.push_back(j);
            }
        }
    }
    if order.len() != pack.nodes.len() {
        return Err(CompileError::Cycle);
    }
    let mut live = vec![false; pack.nodes.len()];
    let mut pending: Vec<usize> = pack
        .nodes
        .iter()
        .enumerate()
        .filter_map(|(i, node)| {
            matches!(
                node.operation,
                Operation::SemanticQuestion { .. } | Operation::LiteralAnswer { .. }
            )
            .then_some(i)
        })
        .chain(hard_deny_indices.iter().copied())
        .chain(accept_indices.iter().copied())
        .collect();
    while let Some(index) = pending.pop() {
        if !live[index] {
            live[index] = true;
            pending.extend(dependencies[index].iter().copied());
        }
    }
    if let Some(index) = live.iter().position(|used| !used) {
        return Err(CompileError::Type(format!(
            "unreachable node: {}",
            pack.nodes[index].id
        )));
    }
    // Semantic prerequisites are deterministic, so all active questions share one bounded stage.
    let semantic_batches = if semantics.is_empty() {
        vec![]
    } else {
        vec![semantics]
    };
    let bytes = serde_json::to_vec(&pack).map_err(|e| CompileError::Serialize(e.to_string()))?;
    if bytes.len() > 256 * 1024 {
        return Err(CompileError::Limit("compiled pack bytes"));
    }
    let content_hash_sha256 = format!("{:x}", Sha256::digest(bytes));
    Ok(CompiledDecisionGraph {
        pack,
        execution_order: order,
        dependencies,
        hard_deny_indices,
        accept_indices,
        semantic_batches,
        content_hash_sha256,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn pack() -> DecisionPack {
        DecisionPack {
            api_version: "kernel/v1".into(),
            kind: "DecisionPack".into(),
            id: "x".into(),
            version: "1".into(),
            required_evidence: vec![],
            permitted_provider_sensitivities: vec![],
            semantic_state_fields: vec![],
            outputs: vec![OutputDefinition::Binary {
                id: "b".into(),
                proposition: "p".into(),
            }],
            nodes: vec![
                Node {
                    id: "a".into(),
                    depends_on: vec![],
                    operation: Operation::FactEquals {
                        field: "x".into(),
                        value: Value::Bool(true),
                    },
                },
                Node {
                    id: "b".into(),
                    depends_on: vec!["a".into()],
                    operation: Operation::SemanticQuestion {
                        output: "b".into(),
                        prompt: "Is p true?".into(),
                    },
                },
            ],
            policy: Policy {
                hard_deny_nodes: vec![],
                accept_nodes: vec![],
                accept_deterministic: false,
                accept_qualified_semantic: false,
            },
            limits: PackLimits {
                deadline_ms: 1000,
                max_billable_attempts: 1,
                max_cost_nano_usd: 100,
            },
            qualification: Qualification {
                semantic_acceptance_qualified: false,
                provider_id: None,
                model_id: None,
                adapter_version: None,
                qualification_ref: None,
            },
            qualification_criteria: None,
        }
    }
    #[test]
    fn compiles_and_orders() {
        assert_eq!(compile(pack()).unwrap().execution_order, vec![0, 1]);
    }
    #[test]
    fn cycle_fails() {
        let mut p = pack();
        p.nodes[0].depends_on = vec!["b".into()];
        assert!(compile(p).is_err());
    }
    #[test]
    fn missing_producer_fails() {
        let mut p = pack();
        p.nodes.pop();
        assert!(compile(p).is_err());
    }
    #[test]
    fn unknown_node_field_is_rejected() {
        let node =
            r#"{"id":"x","op":"fact_equals","field":"flag","value":true,"shell":"echo bad"}"#;
        assert!(serde_json::from_str::<Node>(node).is_err());
    }
    #[test]
    fn unreachable_node_is_rejected() {
        let mut p = pack();
        p.nodes.push(Node {
            id: "unused".into(),
            depends_on: vec![],
            operation: Operation::FactEquals {
                field: "unused".into(),
                value: Value::Bool(true),
            },
        });
        assert!(
            matches!(compile(p), Err(CompileError::Type(message)) if message.contains("unreachable node"))
        );
    }
}
