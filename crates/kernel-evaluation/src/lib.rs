//! Offline, reviewable semantic qualification measurements. No model calls occur here.
use kernel_compiler::{CompiledDecisionGraph, QualificationCriteria};
use kernel_core::{Answer, OutputDefinition, validate_answer};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use thiserror::Error;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Partition {
    Development,
    Calibration,
    Test,
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LabelSource {
    Reviewer,
    VerifiedOutcome,
    ModelReference,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LabeledCase {
    pub id: String,
    pub group_id: String,
    pub partition: Partition,
    pub slice: String,
    pub output_id: String,
    pub expected: Answer,
    pub predicted: Answer,
    pub label_source: LabelSource,
    pub rights_cleared: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvaluationDataset {
    pub schema_version: String,
    pub task_id: String,
    pub release_hash_sha256: String,
    pub provider_id: String,
    pub model_id: String,
    pub adapter_version: String,
    pub cases: Vec<LabeledCase>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClassMetrics {
    pub true_positives: u64,
    pub false_positives: u64,
    pub false_negatives: u64,
    pub precision: Option<f64>,
    pub recall: Option<f64>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SliceMetrics {
    pub total: u64,
    pub evaluated: u64,
    pub correct: u64,
    pub coverage: f64,
    pub accuracy: Option<f64>,
    pub brier_score: Option<f64>,
    pub brier_count: u64,
    pub classes: BTreeMap<String, ClassMetrics>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvaluationReport {
    pub schema_version: String,
    pub task_id: String,
    pub release_hash_sha256: String,
    pub provider_id: String,
    pub model_id: String,
    pub adapter_version: String,
    pub dataset_digest_sha256: String,
    pub reviewed_test_cases: u64,
    pub metrics: BTreeMap<String, SliceMetrics>,
}
#[derive(Debug, Error)]
pub enum EvaluationError {
    #[error("dataset schema, identity, or size is invalid")]
    Identity,
    #[error("case {0} is invalid or unreviewed")]
    Case(String),
    #[error("group {0} crosses evaluation partitions")]
    Leakage(String),
    #[error("no reviewed test examples")]
    EmptyTest,
    #[error("serialization failed")]
    Serialize,
}
#[derive(Default)]
struct Acc {
    total: u64,
    evaluated: u64,
    correct: u64,
    brier_sum: f64,
    brier_count: u64,
    classes: BTreeMap<String, (u64, u64, u64)>,
}
fn label(answer: &Answer) -> Option<String> {
    match answer {
        Answer::Categorical { selected_id, .. } => Some(selected_id.clone()),
        Answer::Binary { value: Some(v), .. } => Some(v.to_string()),
        Answer::Ordinal {
            selected_anchor, ..
        } => Some(selected_anchor.clone()),
        _ => None,
    }
}
pub fn evaluate(
    graph: &CompiledDecisionGraph,
    dataset: EvaluationDataset,
) -> Result<EvaluationReport, EvaluationError> {
    if dataset.schema_version != "kernel-evaluation/v1"
        || dataset.task_id != graph.pack.id
        || dataset.release_hash_sha256 != graph.content_hash_sha256
        || dataset.provider_id.is_empty()
        || dataset.model_id.is_empty()
        || dataset.adapter_version.is_empty()
        || dataset.cases.is_empty()
        || dataset.cases.len() > 1_000_000
    {
        return Err(EvaluationError::Identity);
    }
    let mut ids = BTreeSet::new();
    let mut groups = BTreeMap::new();
    let mut accum: BTreeMap<String, Acc> = BTreeMap::new();
    let outputs: BTreeMap<_, _> = graph
        .pack
        .outputs
        .iter()
        .map(|o| (o.id().to_owned(), o))
        .collect();
    let mut reviewed_test_cases = 0u64;
    for case in &dataset.cases {
        if case.id.is_empty()
            || case.group_id.is_empty()
            || case.slice.is_empty()
            || case.slice.len() > 128
            || !ids.insert(case.id.clone())
            || !case.rights_cleared
        {
            return Err(EvaluationError::Case(case.id.clone()));
        }
        let Some(def) = outputs.get(&case.output_id) else {
            return Err(EvaluationError::Case(case.id.clone()));
        };
        if validate_answer(def, &case.expected).is_err()
            || validate_answer(def, &case.predicted).is_err()
            || !case.expected.is_evaluated()
        {
            return Err(EvaluationError::Case(case.id.clone()));
        }
        if let Some(previous) = groups.insert(case.group_id.clone(), case.partition) {
            if previous != case.partition {
                return Err(EvaluationError::Leakage(case.group_id.clone()));
            }
        }
        if case.partition != Partition::Test {
            continue;
        }
        if case.label_source == LabelSource::ModelReference {
            return Err(EvaluationError::Case(case.id.clone()));
        }
        reviewed_test_cases += 1;
        let key = format!("{}::{}", case.output_id, case.slice);
        let acc = accum.entry(key).or_default();
        acc.total += 1;
        let expected =
            label(&case.expected).ok_or_else(|| EvaluationError::Case(case.id.clone()))?;
        let Some(predicted) = label(&case.predicted) else {
            continue;
        };
        acc.evaluated += 1;
        if expected == predicted {
            acc.correct += 1;
            acc.classes.entry(expected.clone()).or_default().0 += 1;
        } else {
            acc.classes.entry(predicted).or_default().1 += 1;
            acc.classes.entry(expected.clone()).or_default().2 += 1;
        }
        if let (
            OutputDefinition::Binary { .. },
            Answer::Binary {
                value: Some(expected_bool),
                ..
            },
            Answer::Binary {
                probability_true: Some(p),
                ..
            },
        ) = (def, &case.expected, &case.predicted)
        {
            acc.brier_sum += (p - if *expected_bool { 1.0 } else { 0.0 }).powi(2);
            acc.brier_count += 1;
        }
    }
    if reviewed_test_cases == 0 {
        return Err(EvaluationError::EmptyTest);
    }
    let digest = format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&dataset).map_err(|_| EvaluationError::Serialize)?)
    );
    let metrics = accum
        .into_iter()
        .map(|(key, a)| {
            let classes = a
                .classes
                .into_iter()
                .map(|(label, (tp, fp, fn_count))| {
                    (
                        label,
                        ClassMetrics {
                            true_positives: tp,
                            false_positives: fp,
                            false_negatives: fn_count,
                            precision: (tp + fp > 0).then_some(tp as f64 / (tp + fp) as f64),
                            recall: (tp + fn_count > 0)
                                .then_some(tp as f64 / (tp + fn_count) as f64),
                        },
                    )
                })
                .collect();
            (
                key,
                SliceMetrics {
                    total: a.total,
                    evaluated: a.evaluated,
                    correct: a.correct,
                    coverage: a.evaluated as f64 / a.total as f64,
                    accuracy: (a.evaluated > 0).then_some(a.correct as f64 / a.evaluated as f64),
                    brier_score: (a.brier_count > 0).then_some(a.brier_sum / a.brier_count as f64),
                    brier_count: a.brier_count,
                    classes,
                },
            )
        })
        .collect();
    Ok(EvaluationReport {
        schema_version: "kernel-evaluation-report/v1".into(),
        task_id: dataset.task_id,
        release_hash_sha256: dataset.release_hash_sha256,
        provider_id: dataset.provider_id,
        model_id: dataset.model_id,
        adapter_version: dataset.adapter_version,
        dataset_digest_sha256: digest,
        reviewed_test_cases,
        metrics,
    })
}

pub fn meets_criteria(
    report: &EvaluationReport,
    criteria: &QualificationCriteria,
) -> Result<(), String> {
    if report.reviewed_test_cases < criteria.min_reviewed_test_cases {
        return Err("reviewed test sample count is below the pack minimum".into());
    }
    for key in &criteria.required_slices {
        let Some(slice) = report.metrics.get(key) else {
            return Err(format!("required slice {key} is absent"));
        };
        if slice.total < criteria.min_cases_per_required_slice
            || slice.coverage < criteria.min_coverage
            || slice.accuracy.is_none_or(|v| v < criteria.min_accuracy)
        {
            return Err(format!(
                "required slice {key} misses qualification criteria"
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn binary(v: Option<bool>) -> Answer {
        match v {
            Some(v) => Answer::Binary {
                proposition: "needs_urgent_review".into(),
                value: Some(v),
                probability_true: None,
                probability_provenance: None,
            },
            None => Answer::NotEvaluated {
                reason: kernel_core::ReasonCode::ProviderUnavailable,
            },
        }
    }
    fn graph() -> CompiledDecisionGraph {
        kernel_compiler::compile(
            serde_json::from_str(include_str!("../../../packs/examples/support-triage.json"))
                .unwrap(),
        )
        .unwrap()
    }
    #[test]
    fn labels_are_not_probabilities() {
        assert_eq!(
            label(&Answer::Binary {
                proposition: "p".into(),
                value: Some(true),
                probability_true: None,
                probability_provenance: None
            }),
            Some("true".into())
        );
    }
    #[test]
    fn reports_coverage_and_rejects_group_leakage() {
        let graph = graph();
        let cases = [
            (1, Some(true), Partition::Test),
            (2, Some(false), Partition::Test),
            (3, None, Partition::Test),
        ]
        .into_iter()
        .map(|(i, predicted, partition)| LabeledCase {
            id: format!("case-{i}"),
            group_id: format!("group-{i}"),
            partition,
            slice: "hi".into(),
            output_id: "urgent".into(),
            expected: binary(Some(true)),
            predicted: binary(predicted),
            label_source: LabelSource::Reviewer,
            rights_cleared: true,
        })
        .collect();
        let dataset = EvaluationDataset {
            schema_version: "kernel-evaluation/v1".into(),
            task_id: graph.pack.id.clone(),
            release_hash_sha256: graph.content_hash_sha256.clone(),
            provider_id: "fixture".into(),
            model_id: "fixture".into(),
            adapter_version: "fixture-v1".into(),
            cases,
        };
        let report = evaluate(&graph, dataset.clone()).unwrap();
        let slice = &report.metrics["urgent::hi"];
        assert_eq!(slice.total, 3);
        assert_eq!(slice.evaluated, 2);
        assert_eq!(slice.accuracy, Some(0.5));
        assert!(slice.brier_score.is_none());
        let mut leaked = dataset;
        leaked.cases[2].group_id = leaked.cases[0].group_id.clone();
        leaked.cases[2].partition = Partition::Development;
        assert!(matches!(
            evaluate(&graph, leaked),
            Err(EvaluationError::Leakage(_))
        ));
    }
}
