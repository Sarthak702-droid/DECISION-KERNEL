//! Bounded request orchestration. Durable storage is supplied through `ReceiptStore`.
use kernel_compiler::{CompiledDecisionGraph, Operation};
use kernel_core::{
    Answer, DecisionReceipt, DecisionRequest, Disposition, ReasonCode, Scope, validate_answer,
};
use kernel_provider::{
    IntelligenceProvider, ProviderIdentity, SemanticQuestion, SemanticRequest, SemanticResponse,
};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    future::Future,
    pin::Pin,
    sync::Arc,
    time::{Instant, SystemTime, UNIX_EPOCH},
};
use thiserror::Error;
use tokio::sync::Semaphore;

#[derive(Debug, Error)]
pub enum RuntimeError {
    #[error("invalid request")]
    InvalidRequest,
    #[error("release does not match request")]
    Release,
    #[error("idempotency key reused with a different request")]
    Conflict,
    #[error("decision store unavailable")]
    Store,
    #[error("request deadline exceeded")]
    Deadline,
}

pub enum Claim {
    New(u64),
    Existing(Box<DecisionReceipt>),
    Busy,
    Conflict,
}
pub enum Reserve {
    Granted,
    Denied,
}
pub trait ReceiptStore: Send + Sync {
    fn save_recovery_recorded<'a>(
        &'a self,
        scope: &'a Scope,
        key: &'a str,
        generation: u64,
        receipt: &'a DecisionReceipt,
        _request: &'a DecisionRequest,
    ) -> Pin<Box<dyn Future<Output = Result<(), RuntimeError>> + Send + 'a>> {
        self.save_recovery(scope, key, generation, receipt)
    }
    fn save_recovery<'a>(
        &'a self,
        scope: &'a Scope,
        key: &'a str,
        generation: u64,
        receipt: &'a DecisionReceipt,
    ) -> Pin<Box<dyn Future<Output = Result<(), RuntimeError>> + Send + 'a>>;
    fn claim<'a>(
        &'a self,
        scope: &'a Scope,
        key: &'a str,
        digest: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<Claim, RuntimeError>> + Send + 'a>>;
    fn commit<'a>(
        &'a self,
        scope: &'a Scope,
        key: &'a str,
        digest: &'a str,
        generation: u64,
        receipt: &'a DecisionReceipt,
    ) -> Pin<Box<dyn Future<Output = Result<DecisionReceipt, RuntimeError>> + Send + 'a>>;
    fn reserve<'a>(
        &'a self,
        scope: &'a Scope,
        key: &'a str,
        generation: u64,
        amount: u64,
        provider_id: &'a str,
        model_id: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<Reserve, RuntimeError>> + Send + 'a>>;
    fn cached_prediction<'a>(
        &'a self,
        scope: &'a Scope,
        key: &'a str,
        now_ms: u64,
    ) -> Pin<Box<dyn Future<Output = Result<Option<SemanticResponse>, RuntimeError>> + Send + 'a>>;
    fn record_prediction<'a>(
        &'a self,
        scope: &'a Scope,
        key: &'a str,
        response: &'a SemanticResponse,
        expires_at_unix_ms: u64,
    ) -> Pin<Box<dyn Future<Output = Result<(), RuntimeError>> + Send + 'a>>;
    fn qualification_active<'a>(
        &'a self,
        scope: &'a Scope,
        qualification_ref: &'a str,
        release_hash: &'a str,
        identity: &'a ProviderIdentity,
        returned_model_id: &'a str,
        now_ms: u64,
    ) -> Pin<Box<dyn Future<Output = Result<bool, RuntimeError>> + Send + 'a>>;
}
pub mod connections;
pub mod performance;
pub mod postgres;

pub struct Engine {
    pub graph: Arc<CompiledDecisionGraph>,
    pub provider: Option<Arc<dyn IntelligenceProvider>>,
    pub store: Arc<dyn ReceiptStore>,
    pub provider_permits: Arc<Semaphore>,
    pub platform_deadline_ms: u64,
    pub platform_max_cost_nano_usd: u64,
}

fn semantic_response_valid(
    response: &SemanticResponse,
    identity: &ProviderIdentity,
    questions: &[SemanticQuestion],
) -> bool {
    if response.provider_id != identity.provider_id
        || !identity.accepts_returned_model(&response.model_id)
    {
        return false;
    }
    let expected: BTreeSet<_> = questions.iter().map(|q| q.id.as_str()).collect();
    let actual: BTreeSet<_> = response.answers.keys().map(String::as_str).collect();
    expected == actual
        && questions.iter().all(|q| {
            response
                .answers
                .get(&q.id)
                .is_some_and(|a| validate_answer(&q.output, a).is_ok())
        })
}

pub struct PrefixResult {
    pub predicates: Vec<bool>,
    pub answers: BTreeMap<String, Answer>,
    pub unresolved: Vec<SemanticQuestion>,
    pub policy_unknown: bool,
}
pub fn evaluate_prefix(
    graph: &CompiledDecisionGraph,
    state: &BTreeMap<String, serde_json::Value>,
    valid_evidence: &BTreeSet<String>,
) -> Result<PrefixResult, RuntimeError> {
    let mut predicates = vec![false; graph.pack.nodes.len()];
    let mut unknown = vec![false; graph.pack.nodes.len()];
    let mut answers = BTreeMap::new();
    let mut unresolved = Vec::new();
    for &idx in &graph.execution_order {
        let node = &graph.pack.nodes[idx];
        let enabled = graph.dependencies[idx].iter().all(|&i| predicates[i]);
        match &node.operation {
            Operation::FactEquals { field, value } => {
                unknown[idx] = state.get(field).is_none_or(|actual| {
                    std::mem::discriminant(actual) != std::mem::discriminant(value)
                });
                predicates[idx] = state.get(field) == Some(value)
            }
            Operation::FactCompare {
                field,
                comparison,
                value,
            } => {
                let actual = state.get(field).and_then(serde_json::Value::as_f64);
                unknown[idx] = actual.is_none();
                predicates[idx] = actual.is_some_and(|a| comparison.evaluate(a, *value));
            }
            Operation::BooleanNot => {
                let dep = graph.dependencies[idx][0];
                unknown[idx] = unknown[dep];
                predicates[idx] = !unknown[dep] && !predicates[dep];
            }
            Operation::EvidencePresent { evidence_id } => {
                predicates[idx] = valid_evidence.contains(evidence_id)
            }
            Operation::BooleanAnd => {
                predicates[idx] = enabled;
                unknown[idx] = graph.dependencies[idx].iter().any(|&i| unknown[i])
                    && !graph.dependencies[idx]
                        .iter()
                        .any(|&i| !predicates[i] && !unknown[i]);
            }
            Operation::BooleanOr => {
                predicates[idx] = graph.dependencies[idx].iter().any(|&i| predicates[i]);
                unknown[idx] =
                    !predicates[idx] && graph.dependencies[idx].iter().any(|&i| unknown[i]);
            }
            Operation::StateAnswer { output, field } if enabled => {
                let def = graph
                    .pack
                    .outputs
                    .iter()
                    .find(|d| d.id() == output)
                    .ok_or(RuntimeError::Release)?;
                let value = state.get(field);
                let answer = match def {
                    kernel_core::OutputDefinition::Binary { proposition, .. } => Answer::Binary {
                        proposition: proposition.clone(),
                        value: value.and_then(serde_json::Value::as_bool),
                        probability_true: None,
                        probability_provenance: None,
                    },
                    kernel_core::OutputDefinition::Categorical { .. } => Answer::Categorical {
                        selected_id: value
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or_default()
                            .into(),
                        probabilities: None,
                        probability_provenance: None,
                    },
                    kernel_core::OutputDefinition::Ordinal { scale_id, .. } => Answer::Ordinal {
                        scale_id: scale_id.clone(),
                        selected_anchor: value
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or_default()
                            .into(),
                        distribution: None,
                        probability_provenance: None,
                    },
                };
                let answer = if validate_answer(def, &answer).is_ok() {
                    answer
                } else {
                    Answer::NotEvaluated {
                        reason: ReasonCode::NotEvaluated,
                    }
                };
                answers.insert(output.clone(), answer);
            }
            Operation::LiteralAnswer { output, answer } if enabled => {
                answers.insert(output.clone(), answer.clone());
            }
            Operation::SemanticQuestion { output, prompt } if enabled => {
                let def = graph
                    .pack
                    .outputs
                    .iter()
                    .find(|d| d.id() == output)
                    .ok_or(RuntimeError::Release)?;
                unresolved.push(SemanticQuestion {
                    id: node.id.clone(),
                    prompt: prompt.clone(),
                    output: def.clone(),
                });
            }
            _ => {}
        }
    }
    Ok(PrefixResult {
        predicates,
        answers,
        unresolved,
        policy_unknown: graph
            .hard_deny_indices
            .iter()
            .chain(&graph.accept_indices)
            .any(|&i| unknown[i])
            || graph.pack.nodes.iter().enumerate().any(|(i, n)| {
                (graph.pack.policy.review_nodes.contains(&n.id)
                    || graph.pack.policy.abstain_nodes.contains(&n.id))
                    && unknown[i]
            }),
    })
}

/// Validate raw input before any claim, provider dispatch or deferred admission.
/// The original request remains the idempotency identity; preparation is release-bound.
pub fn prepare_state(
    graph: &CompiledDecisionGraph,
    state: &BTreeMap<String, serde_json::Value>,
) -> Result<BTreeMap<String, serde_json::Value>, RuntimeError> {
    if state.len() > 128 {
        return Err(RuntimeError::InvalidRequest);
    }
    if let Some(schema) = &graph.pack.input_schema {
        let value = serde_json::to_value(state).map_err(|_| RuntimeError::InvalidRequest)?;
        if !schema.accepts(&value) {
            return Err(RuntimeError::InvalidRequest);
        }
    }
    let mut prepared = state.clone();
    if let Some(preparation) = &graph.pack.state_preparation {
        preparation
            .apply(&mut prepared)
            .map_err(|_| RuntimeError::InvalidRequest)?;
    }
    if prepared.len() > 128
        || serde_json::to_vec(&prepared)
            .map_err(|_| RuntimeError::InvalidRequest)?
            .len()
            > 64 * 1024
    {
        return Err(RuntimeError::InvalidRequest);
    }
    Ok(prepared)
}

fn explicit_policy(
    graph: &CompiledDecisionGraph,
    predicates: &[bool],
) -> Option<(Disposition, ReasonCode)> {
    let triggered = |ids: &[String]| {
        graph
            .pack
            .nodes
            .iter()
            .enumerate()
            .any(|(i, n)| ids.contains(&n.id) && predicates[i])
    };
    if triggered(&graph.pack.policy.abstain_nodes) {
        Some((Disposition::Abstain, ReasonCode::PolicyAbstain))
    } else if triggered(&graph.pack.policy.review_nodes) {
        Some((Disposition::Review, ReasonCode::PolicyReview))
    } else {
        None
    }
}

fn apply_fallback(
    graph: &CompiledDecisionGraph,
    disposition: &mut Disposition,
    reasons: &[ReasonCode],
) {
    if *disposition != Disposition::Review || reasons.contains(&ReasonCode::PolicyReview) {
        return;
    }
    if let Some(fallback) = &graph.pack.policy.fallback {
        *disposition = if reasons.iter().any(|r| {
            matches!(
                r,
                ReasonCode::ProviderUnavailable
                    | ReasonCode::ProviderTimeout
                    | ReasonCode::InvalidProviderOutput
                    | ReasonCode::DeadlineExceeded
                    | ReasonCode::BudgetUnavailable
            )
        }) {
            fallback.provider_failure.disposition()
        } else if reasons.iter().any(|r| {
            matches!(
                r,
                ReasonCode::MissingEvidence
                    | ReasonCode::InvalidEvidence
                    | ReasonCode::StaleEvidence
            )
        }) {
            fallback.missing_evidence.disposition()
        } else {
            fallback.unresolved.disposition()
        };
    }
}

fn validate_request(
    graph: &CompiledDecisionGraph,
    scope: &Scope,
    request: &DecisionRequest,
) -> Result<BTreeMap<String, serde_json::Value>, RuntimeError> {
    if !scope.is_valid()
        || request.schema_version != "1.1"
        || request.decision_type != graph.pack.id
        || request.release != format!("{}@{}", graph.pack.id, graph.pack.version)
        || request.idempotency_key.is_empty()
        || request.idempotency_key.len() > 128
        || request.evidence.len() > 32
        || request
            .evidence
            .iter()
            .any(|e| e.id.is_empty() || e.id.len() > 128)
        || request
            .evidence
            .iter()
            .map(|e| &e.id)
            .collect::<BTreeSet<_>>()
            .len()
            != request.evidence.len()
    {
        return Err(RuntimeError::InvalidRequest);
    }
    prepare_state(graph, &request.state)
}

fn valid_evidence_ids(
    graph: &CompiledDecisionGraph,
    request: &DecisionRequest,
    now_ms: u64,
) -> BTreeSet<String> {
    request
        .evidence
        .iter()
        .filter(|e| {
            e.observed_at_unix_ms <= now_ms
                && e.expires_at_unix_ms.is_none_or(|t| t > now_ms)
                && graph.pack.evidence_requirements.get(&e.id).is_none_or(|r| {
                    r.value_schema.accepts(&e.value)
                        && r.max_age_ms
                            .is_none_or(|age| now_ms.saturating_sub(e.observed_at_unix_ms) <= age)
                })
                && serde_json::to_vec(&e.value)
                    .is_ok_and(|bytes| format!("{:x}", Sha256::digest(bytes)) == e.digest_sha256)
        })
        .map(|e| e.id.clone())
        .collect()
}

pub fn evaluate_pure(
    graph: &CompiledDecisionGraph,
    scope: Scope,
    request: DecisionRequest,
) -> Result<DecisionReceipt, RuntimeError> {
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    evaluate_recorded(graph, scope, request, now_ms, None)
}

/// Offline policy simulation at the original observation time. Stored semantic
/// answers never grant qualification; acceptance still requires an online gate.
pub fn evaluate_recorded(
    graph: &CompiledDecisionGraph,
    scope: Scope,
    request: DecisionRequest,
    now_ms: u64,
    predictions: Option<&BTreeMap<String, Answer>>,
) -> Result<DecisionReceipt, RuntimeError> {
    let start = Instant::now();
    let prepared_state = validate_request(graph, &scope, &request)?;
    let (decision_type, version) = request
        .release
        .rsplit_once('@')
        .ok_or(RuntimeError::Release)?;
    if decision_type != graph.pack.id || version != graph.pack.version {
        return Err(RuntimeError::Release);
    }
    let valid_evidence = valid_evidence_ids(graph, &request, now_ms);
    let PrefixResult {
        predicates,
        mut answers,
        unresolved,
        policy_unknown,
    } = evaluate_prefix(graph, &prepared_state, &valid_evidence)?;
    let hard_deny = graph.hard_deny_indices.iter().any(|&i| predicates[i]);
    let missing_evidence = graph
        .pack
        .required_evidence
        .iter()
        .any(|id| !valid_evidence.contains(id))
        || request
            .evidence
            .iter()
            .any(|e| !valid_evidence.contains(&e.id));
    let mut disposition = Disposition::Review;
    let mut reasons = Vec::new();
    if hard_deny {
        disposition = Disposition::Deny;
        reasons.push(ReasonCode::HardPolicyDeny);
    } else if let Some((d, reason)) = explicit_policy(graph, &predicates) {
        disposition = d;
        reasons.push(reason);
    } else if policy_unknown {
        reasons.push(ReasonCode::MissingPermission);
    } else if missing_evidence {
        reasons.push(ReasonCode::MissingEvidence);
    } else if unresolved.is_empty() && answers.len() == graph.pack.outputs.len() {
        let allowed = graph.accept_indices.iter().all(|&i| predicates[i]);
        if allowed
            && graph.pack.policy.accept_deterministic
            && answers.values().all(Answer::is_evaluated)
        {
            disposition = Disposition::Accept;
            reasons.push(ReasonCode::DeterministicResolution);
        } else {
            reasons.push(ReasonCode::NotEvaluated);
        }
    } else if let Some(predictions) = predictions {
        for question in &unresolved {
            if let Some(answer) = predictions.get(question.output.id()) {
                validate_answer(&question.output, answer)
                    .map_err(|_| RuntimeError::InvalidRequest)?;
                answers.insert(question.output.id().to_owned(), answer.clone());
            }
        }
        reasons.push(ReasonCode::UnqualifiedSemantic);
    } else {
        reasons.push(if unresolved.is_empty() {
            ReasonCode::NotEvaluated
        } else {
            ReasonCode::ProviderUnavailable
        });
    }

    apply_fallback(graph, &mut disposition, &reasons);
    for output in &graph.pack.outputs {
        answers
            .entry(output.id().to_owned())
            .or_insert(Answer::NotEvaluated {
                reason: reasons.first().cloned().unwrap_or(ReasonCode::NotEvaluated),
            });
    }
    let compute_ms = start.elapsed().as_millis() as u64;
    let evidence_revisions = request
        .evidence
        .iter()
        .map(|e| (e.id.clone(), e.revision))
        .collect();

    Ok(DecisionReceipt {
        receipt_id: format!("rec_pure_{}", request.idempotency_key),
        scope,
        request_digest_sha256: format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(&request).map_err(|_| RuntimeError::InvalidRequest)?)
        ),
        release: request.release,
        release_digest_sha256: graph.content_hash_sha256.clone(),
        answers,
        disposition,
        reason_codes: reasons,
        evidence_revisions,
        provider_id: None,
        model_id: None,
        returned_model_id: None,
        attempts: 0,
        prediction_cache_hit: false,
        cost_nano_usd: Some(0),
        provider_reported_cost_nano_usd: None,
        provider_usage: None,
        created_at_unix_ms: now_ms,
        kernel_compute_ms: compute_ms,
        db_claim_ms: 0,
        provider_latency_ms: None,
        db_commit_ms: None,
        total_latency_ms: Some(compute_ms),
    })
}

#[cfg(test)]
#[allow(clippy::items_after_test_module)]
mod tests {
    include!("tier2_tests.rs");
    use super::*;
    use kernel_compiler::{
        DecisionPack, Node, Operation, PackLimits, Policy, Qualification, compile,
    };
    use kernel_core::{OutputDefinition, RequestLimits};
    use kernel_provider::{
        ProviderCapabilities, ProviderError, ProviderIdentity, SemanticResponse,
    };
    use std::sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    };

    #[derive(Default)]
    struct MemoryStore {
        receipts: Mutex<BTreeMap<String, (String, DecisionReceipt)>>,
        predictions: Mutex<BTreeMap<String, (SemanticResponse, u64)>>,
    }
    impl ReceiptStore for MemoryStore {
        fn save_recovery<'a>(
            &'a self,
            _: &'a Scope,
            _: &'a str,
            _: u64,
            _: &'a DecisionReceipt,
        ) -> Pin<Box<dyn Future<Output = Result<(), RuntimeError>> + Send + 'a>> {
            // This test store is intentionally ephemeral.
            Box::pin(async { Ok(()) })
        }
        fn claim<'a>(
            &'a self,
            _scope: &'a Scope,
            key: &'a str,
            digest: &'a str,
        ) -> Pin<Box<dyn Future<Output = Result<Claim, RuntimeError>> + Send + 'a>> {
            Box::pin(async move {
                Ok(match self.receipts.lock().unwrap().get(key) {
                    Some((d, r)) if d == digest => Claim::Existing(Box::new(r.clone())),
                    Some(_) => Claim::Conflict,
                    None => Claim::New(1),
                })
            })
        }
        fn reserve<'a>(
            &'a self,
            _: &'a Scope,
            _: &'a str,
            _: u64,
            _: u64,
            _: &'a str,
            _: &'a str,
        ) -> Pin<Box<dyn Future<Output = Result<Reserve, RuntimeError>> + Send + 'a>> {
            Box::pin(async { Ok(Reserve::Granted) })
        }
        fn commit<'a>(
            &'a self,
            _: &'a Scope,
            key: &'a str,
            digest: &'a str,
            _: u64,
            receipt: &'a DecisionReceipt,
        ) -> Pin<Box<dyn Future<Output = Result<DecisionReceipt, RuntimeError>> + Send + 'a>>
        {
            Box::pin(async move {
                self.receipts
                    .lock()
                    .unwrap()
                    .insert(key.into(), (digest.into(), receipt.clone()));
                Ok(receipt.clone())
            })
        }
        fn cached_prediction<'a>(
            &'a self,
            _: &'a Scope,
            key: &'a str,
            now_ms: u64,
        ) -> Pin<Box<dyn Future<Output = Result<Option<SemanticResponse>, RuntimeError>> + Send + 'a>>
        {
            Box::pin(async move {
                Ok(self
                    .predictions
                    .lock()
                    .unwrap()
                    .get(key)
                    .filter(|(_, expiry)| *expiry > now_ms)
                    .map(|(r, _)| r.clone()))
            })
        }
        fn record_prediction<'a>(
            &'a self,
            _: &'a Scope,
            key: &'a str,
            response: &'a SemanticResponse,
            expiry: u64,
        ) -> Pin<Box<dyn Future<Output = Result<(), RuntimeError>> + Send + 'a>> {
            Box::pin(async move {
                self.predictions
                    .lock()
                    .unwrap()
                    .insert(key.into(), (response.clone(), expiry));
                Ok(())
            })
        }
        fn qualification_active<'a>(
            &'a self,
            _: &'a Scope,
            _: &'a str,
            _: &'a str,
            _: &'a ProviderIdentity,
            _: &'a str,
            _: u64,
        ) -> Pin<Box<dyn Future<Output = Result<bool, RuntimeError>> + Send + 'a>> {
            Box::pin(async { Ok(false) })
        }
    }
    struct CountingProvider(Arc<AtomicUsize>);
    impl IntelligenceProvider for CountingProvider {
        fn identity(&self) -> ProviderIdentity {
            ProviderIdentity {
                provider_id: "fixture".into(),
                model_id: "fixture".into(),
                adapter_version: "1".into(),
                allowed_returned_models: vec![],
            }
        }
        fn capabilities(&self) -> ProviderCapabilities {
            ProviderCapabilities {
                supports_batch: true,
                max_questions: 8,
                max_output_tokens: 100,
            }
        }
        fn evaluate<'a>(
            &'a self,
            _: SemanticRequest,
            _: Instant,
        ) -> Pin<Box<dyn Future<Output = Result<SemanticResponse, ProviderError>> + Send + 'a>>
        {
            self.0.fetch_add(1, Ordering::SeqCst);
            Box::pin(async { Err(ProviderError::Unavailable) })
        }
    }
    struct RoutedProvider {
        identity: ProviderIdentity,
        response: SemanticResponse,
    }
    impl IntelligenceProvider for RoutedProvider {
        fn worst_case_cost_nano_usd(&self, _: &SemanticRequest) -> Option<u64> {
            Some(100)
        }
        fn identity(&self) -> ProviderIdentity {
            self.identity.clone()
        }
        fn capabilities(&self) -> ProviderCapabilities {
            ProviderCapabilities {
                supports_batch: true,
                max_questions: 8,
                max_output_tokens: 100,
            }
        }
        fn evaluate<'a>(
            &'a self,
            _: SemanticRequest,
            _: Instant,
        ) -> Pin<Box<dyn Future<Output = Result<SemanticResponse, ProviderError>> + Send + 'a>>
        {
            let response = self.response.clone();
            Box::pin(async move { Ok(response) })
        }
    }
    fn setup(hard_deny: bool) -> (Engine, Arc<AtomicUsize>, Scope, DecisionRequest) {
        let pack = DecisionPack {
            api_version: "kernel/v1".into(),
            kind: "DecisionPack".into(),
            id: "test".into(),
            version: "1".into(),
            input_schema: None,
            state_preparation: None,
            required_evidence: vec![],
            evidence_requirements: BTreeMap::new(),
            permitted_provider_sensitivities: vec![],
            semantic_state_fields: vec![],
            outputs: vec![OutputDefinition::Binary {
                id: "yes".into(),
                proposition: "yes".into(),
            }],
            nodes: vec![
                Node {
                    id: "deny".into(),
                    depends_on: vec![],
                    operation: Operation::FactEquals {
                        field: "deny".into(),
                        value: serde_json::json!(true),
                    },
                },
                Node {
                    id: "answer".into(),
                    depends_on: vec![],
                    operation: Operation::LiteralAnswer {
                        output: "yes".into(),
                        answer: Answer::Binary {
                            proposition: "yes".into(),
                            value: Some(true),
                            probability_true: None,
                            probability_provenance: None,
                        },
                    },
                },
            ],
            policy: Policy {
                review_nodes: vec![],
                abstain_nodes: vec![],
                fallback: None,
                hard_deny_nodes: vec!["deny".into()],
                accept_nodes: vec![],
                accept_deterministic: true,
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
        };
        let counter = Arc::new(AtomicUsize::new(0));
        let engine = Engine {
            graph: Arc::new(compile(pack).unwrap()),
            provider: Some(Arc::new(CountingProvider(counter.clone()))),
            store: Arc::new(MemoryStore::default()),
            provider_permits: Arc::new(Semaphore::new(1)),
            platform_deadline_ms: 1000,
            platform_max_cost_nano_usd: 100,
        };
        let scope = Scope {
            tenant_id: "t".into(),
            project_id: "p".into(),
            environment: "test".into(),
            product_id: "x".into(),
        };
        let request = DecisionRequest {
            schema_version: "1.1".into(),
            decision_type: "test".into(),
            release: "test@1".into(),
            idempotency_key: "key".into(),
            state: BTreeMap::from([("deny".into(), serde_json::json!(hard_deny))]),
            evidence: vec![],
            constraints: RequestLimits::default(),
        };
        (engine, counter, scope, request)
    }
    #[tokio::test]
    async fn hard_deny_dominates_and_is_idempotent() {
        let (engine, counter, scope, request) = setup(true);
        let first = engine.decide(scope.clone(), request.clone()).await.unwrap();
        let second = engine.decide(scope, request).await.unwrap();
        assert_eq!(first.disposition, Disposition::Deny);
        assert_eq!(first.receipt_id, second.receipt_id);
        assert_eq!(counter.load(Ordering::SeqCst), 0);
    }
    #[tokio::test]
    async fn deterministic_answer_uses_zero_provider_calls() {
        let (engine, counter, scope, request) = setup(false);
        let result = engine.decide(scope, request).await.unwrap();
        assert_eq!(result.disposition, Disposition::Accept);
        assert_eq!(result.attempts, 0);
        assert_eq!(counter.load(Ordering::SeqCst), 0);
    }
    #[tokio::test]
    async fn label_only_semantic_result_stays_review_only_and_uncalibrated() {
        let (mut engine, _, scope, request) = setup(false);
        let mut pack = engine.graph.pack.clone();
        pack.nodes[1] = Node {
            id: "semantic".into(),
            depends_on: vec![],
            operation: Operation::SemanticQuestion {
                output: "yes".into(),
                prompt: "Is it yes?".into(),
            },
        };
        pack.policy.accept_deterministic = false;
        pack.qualification.provider_id = Some("fixture".into());
        pack.qualification.model_id = Some("fixture".into());
        engine.graph = Arc::new(compile(pack).unwrap());
        engine.provider = Some(Arc::new(kernel_provider::FixtureProvider {
            response: SemanticResponse {
                answers: BTreeMap::from([(
                    "semantic".into(),
                    Answer::Binary {
                        proposition: "yes".into(),
                        value: Some(true),
                        probability_true: None,
                        probability_provenance: None,
                    },
                )]),
                provider_id: "fixture".into(),
                model_id: "fixture".into(),
                reported_cost_nano_usd: None,
                usage: None,
            },
        }));
        let result = engine.decide(scope.clone(), request.clone()).await.unwrap();
        assert_eq!(result.disposition, Disposition::Review);
        assert_eq!(result.attempts, 1);
        assert!(matches!(
            result.answers.get("yes"),
            Some(Answer::Binary {
                probability_true: None,
                ..
            })
        ));
        let replay = evaluate_recorded(
            &engine.graph,
            scope.clone(),
            request.clone(),
            result.created_at_unix_ms,
            Some(&result.answers),
        )
        .unwrap();
        assert_eq!(
            serde_json::to_value(&replay.answers).unwrap(),
            serde_json::to_value(&result.answers).unwrap()
        );
        assert_eq!(replay.disposition, Disposition::Review);
        assert_eq!(replay.attempts, 0);
        let mut denied_request = request.clone();
        denied_request
            .state
            .insert("deny".into(), serde_json::json!(true));
        let denied = evaluate_recorded(
            &engine.graph,
            scope.clone(),
            denied_request,
            result.created_at_unix_ms,
            Some(&result.answers),
        )
        .unwrap();
        assert_eq!(denied.disposition, Disposition::Deny);
        let malformed = BTreeMap::from([(
            "yes".into(),
            Answer::Binary {
                proposition: "wrong".into(),
                value: Some(true),
                probability_true: None,
                probability_provenance: None,
            },
        )]);
        assert!(
            evaluate_recorded(
                &engine.graph,
                scope.clone(),
                request.clone(),
                result.created_at_unix_ms,
                Some(&malformed)
            )
            .is_err()
        );
        let mut second = request;
        second.idempotency_key = "second-key".into();
        second.constraints.max_billable_attempts = Some(0);
        let reused = engine.decide(scope, second).await.unwrap();
        assert_eq!(reused.attempts, 0);
        assert!(reused.prediction_cache_hit);
        assert_eq!(reused.disposition, Disposition::Review);
    }
    #[tokio::test]
    async fn routed_semantic_model_records_requested_and_served_ids() {
        let (mut engine, _, scope, request) = setup(false);
        let mut pack = engine.graph.pack.clone();
        pack.nodes[1] = Node {
            id: "semantic".into(),
            depends_on: vec![],
            operation: Operation::SemanticQuestion {
                output: "yes".into(),
                prompt: "Is it yes?".into(),
            },
        };
        pack.policy.accept_deterministic = false;
        pack.qualification.provider_id = Some("fixture".into());
        pack.qualification.model_id = Some("requested-model".into());
        engine.graph = Arc::new(compile(pack).unwrap());
        engine.provider = Some(Arc::new(RoutedProvider {
            identity: ProviderIdentity {
                provider_id: "fixture".into(),
                model_id: "requested-model".into(),
                adapter_version: "1".into(),
                allowed_returned_models: vec!["served-model".into()],
            },
            response: SemanticResponse {
                answers: BTreeMap::from([(
                    "semantic".into(),
                    Answer::Binary {
                        proposition: "yes".into(),
                        value: Some(true),
                        probability_true: None,
                        probability_provenance: None,
                    },
                )]),
                provider_id: "fixture".into(),
                model_id: "served-model".into(),
                reported_cost_nano_usd: None,
                usage: None,
            },
        }));
        let result = engine.decide(scope.clone(), request.clone()).await.unwrap();
        assert_eq!(result.disposition, Disposition::Review);
        assert_eq!(result.attempts, 1);
        assert_eq!(result.provider_id.as_deref(), Some("fixture"));
        assert_eq!(result.model_id.as_deref(), Some("requested-model"));
        assert_eq!(result.returned_model_id.as_deref(), Some("served-model"));

        let mut second = request;
        second.idempotency_key = "second-routed-key".into();
        second.constraints.max_billable_attempts = Some(0);
        let cached = engine.decide(scope, second).await.unwrap();
        assert!(cached.prediction_cache_hit);
        assert_eq!(cached.attempts, 0);
        assert_eq!(cached.returned_model_id.as_deref(), Some("served-model"));
    }
}

impl Engine {
    pub async fn decide(
        &self,
        scope: Scope,
        request: DecisionRequest,
    ) -> Result<DecisionReceipt, RuntimeError> {
        let deadline = Instant::now()
            + std::time::Duration::from_millis(
                self.platform_deadline_ms
                    .min(self.graph.pack.limits.deadline_ms)
                    .min(request.constraints.deadline_ms.unwrap_or(u64::MAX)),
            );
        self.decide_until(scope, request, deadline).await
    }

    pub async fn decide_until(
        &self,
        scope: Scope,
        request: DecisionRequest,
        deadline: Instant,
    ) -> Result<DecisionReceipt, RuntimeError> {
        self.decide_measured(scope, request, deadline).await.0
    }

    /// Timings describe this invocation, including replay and failed attempts.
    /// Durable receipts retain their original execution identity.
    pub async fn decide_measured(
        &self,
        scope: Scope,
        request: DecisionRequest,
        deadline: Instant,
    ) -> (Result<DecisionReceipt, RuntimeError>, performance::Timings) {
        let metrics = Arc::new(performance::Metrics::default());
        let engine = Engine {
            graph: self.graph.clone(),
            provider: self.provider.clone(),
            store: Arc::new(performance::TimedStore {
                inner: self.store.clone(),
                metrics: metrics.clone(),
            }),
            provider_permits: self.provider_permits.clone(),
            platform_deadline_ms: self.platform_deadline_ms,
            platform_max_cost_nano_usd: self.platform_max_cost_nano_usd,
        };
        let start = Instant::now();
        let result = performance::METRICS
            .scope(metrics.clone(), async {
                tokio::time::timeout_at(
                    tokio::time::Instant::from_std(deadline),
                    engine.decide_inner(scope, request, deadline),
                )
                .await
                .map_err(|_| RuntimeError::Deadline)?
            })
            .await;
        let timings = metrics.snapshot(start.elapsed());
        (result, timings)
    }

    async fn decide_inner(
        &self,
        scope: Scope,
        request: DecisionRequest,
        deadline: Instant,
    ) -> Result<DecisionReceipt, RuntimeError> {
        let start = Instant::now();
        let prepared_state = validate_request(&self.graph, &scope, &request)?;
        let bytes =
            serde_json::to_vec(&(&scope, &request)).map_err(|_| RuntimeError::InvalidRequest)?;
        if bytes.len() > 64 * 1024 {
            return Err(RuntimeError::InvalidRequest);
        }
        let digest = format!("{:x}", Sha256::digest(&bytes));
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| RuntimeError::InvalidRequest)?
            .as_millis() as u64;
        let refs: BTreeMap<_, _> = request
            .evidence
            .iter()
            .map(|e| (e.id.clone(), e.revision))
            .collect();
        if refs.len() != request.evidence.len() {
            return Err(RuntimeError::InvalidRequest);
        }
        let claim_start = Instant::now();
        let generation = loop {
            match self
                .store
                .claim(&scope, &request.idempotency_key, &digest)
                .await?
            {
                Claim::Existing(r) if r.scope == scope => return Ok(*r),
                Claim::Existing(_) => return Err(RuntimeError::Store),
                Claim::Busy if Instant::now() < deadline => {
                    performance::timed(
                        performance::Kind::Queue,
                        tokio::time::sleep(std::time::Duration::from_millis(10)),
                    )
                    .await
                }
                Claim::Busy => return Err(RuntimeError::Deadline),
                Claim::Conflict => return Err(RuntimeError::Conflict),
                Claim::New(generation) => break generation,
            }
        };
        let db_claim_ms = claim_start.elapsed().as_millis() as u64;
        let recovery = DecisionReceipt {
            receipt_id: format!(
                "{:x}",
                Sha256::digest(format!("{}:{}", request.idempotency_key, digest))
            ),
            scope: scope.clone(),
            request_digest_sha256: digest.clone(),
            release: request.release.clone(),
            release_digest_sha256: self.graph.content_hash_sha256.clone(),
            answers: self
                .graph
                .pack
                .outputs
                .iter()
                .map(|o| {
                    (
                        o.id().to_owned(),
                        Answer::NotEvaluated {
                            reason: ReasonCode::CrashRecovery,
                        },
                    )
                })
                .collect(),
            disposition: Disposition::Review,
            reason_codes: vec![ReasonCode::CrashRecovery],
            evidence_revisions: refs.clone(),
            provider_id: None,
            model_id: None,
            returned_model_id: None,
            attempts: 1,
            prediction_cache_hit: false,
            cost_nano_usd: None,
            provider_reported_cost_nano_usd: None,
            provider_usage: None,
            created_at_unix_ms: now_ms,
            kernel_compute_ms: 0,
            db_claim_ms,
            provider_latency_ms: None,
            db_commit_ms: None,
            total_latency_ms: None,
        };
        self.store
            .save_recovery_recorded(
                &scope,
                &request.idempotency_key,
                generation,
                &recovery,
                &request,
            )
            .await?;

        let max_attempts = self
            .graph
            .pack
            .limits
            .max_billable_attempts
            .min(request.constraints.max_billable_attempts.unwrap_or(u8::MAX))
            .min(1);
        let cost_cap = self
            .platform_max_cost_nano_usd
            .min(self.graph.pack.limits.max_cost_nano_usd)
            .min(request.constraints.max_cost_nano_usd.unwrap_or(u64::MAX));
        let valid_evidence = valid_evidence_ids(&self.graph, &request, now_ms);
        let PrefixResult {
            predicates,
            mut answers,
            unresolved,
            policy_unknown,
        } = evaluate_prefix(&self.graph, &prepared_state, &valid_evidence)?;
        let hard_deny = self.graph.hard_deny_indices.iter().any(|&i| predicates[i]);
        let missing_evidence = self
            .graph
            .pack
            .required_evidence
            .iter()
            .any(|id| !valid_evidence.contains(id))
            || request
                .evidence
                .iter()
                .any(|e| !valid_evidence.contains(&e.id));
        let sensitive_egress_denied = request.evidence.iter().any(|e| {
            self.graph.pack.required_evidence.contains(&e.id)
                && !self
                    .graph
                    .pack
                    .permitted_provider_sensitivities
                    .contains(&e.sensitivity)
        });
        let mut disposition = Disposition::Review;
        let mut reasons = Vec::new();
        let mut provider_id = None;
        let mut model_id = None;
        let mut returned_model_id = None;
        let mut attempts = 0;
        let mut provider_reported_cost_nano_usd = None;
        let mut provider_usage = None;
        let mut prediction_cache_hit = false;
        let mut cache_candidate: Option<(String, SemanticResponse, u64)> = None;
        let mut provider_latency_ms = None;
        let mut prediction_guard = None;
        if hard_deny {
            disposition = Disposition::Deny;
            reasons.push(ReasonCode::HardPolicyDeny);
        } else if let Some((d, reason)) = explicit_policy(&self.graph, &predicates) {
            disposition = d;
            reasons.push(reason);
        } else if Instant::now() >= deadline {
            reasons.push(ReasonCode::DeadlineExceeded);
        } else if policy_unknown {
            reasons.push(ReasonCode::MissingPermission);
        } else if missing_evidence {
            reasons.push(ReasonCode::MissingEvidence);
        } else if unresolved.is_empty() && answers.len() == self.graph.pack.outputs.len() {
            let allowed = self.graph.accept_indices.iter().all(|&i| predicates[i]);
            if allowed
                && self.graph.pack.policy.accept_deterministic
                && answers.values().all(Answer::is_evaluated)
            {
                disposition = Disposition::Accept;
                reasons.push(ReasonCode::DeterministicResolution);
            } else {
                reasons.push(ReasonCode::NotEvaluated);
            }
        } else if unresolved.is_empty() {
            reasons.push(ReasonCode::NotEvaluated);
        } else if sensitive_egress_denied {
            reasons.push(ReasonCode::InvalidEvidence);
        } else if let Some(provider) = &self.provider {
            let identity = provider.identity();
            let caps = provider.capabilities();
            let qualified = self.graph.pack.qualification.provider_id.as_deref()
                == Some(&identity.provider_id)
                && self.graph.pack.qualification.model_id.as_deref() == Some(&identity.model_id)
                && self
                    .graph
                    .pack
                    .qualification
                    .adapter_version
                    .as_deref()
                    .is_none_or(|v| v == identity.adapter_version);
            if !qualified {
                reasons.push(ReasonCode::UnqualifiedSemantic);
            } else if unresolved.len() > caps.max_questions
                || (!caps.supports_batch && unresolved.len() > 1)
            {
                reasons.push(ReasonCode::ProviderUnavailable);
            } else {
                let semantic = SemanticRequest {
                    state: prepared_state
                        .iter()
                        .filter(|(key, _)| self.graph.pack.semantic_state_fields.contains(key))
                        .map(|(k, v)| (k.clone(), v.clone()))
                        .collect(),
                    evidence: request
                        .evidence
                        .iter()
                        .filter(|e| {
                            valid_evidence.contains(&e.id)
                                && self.graph.pack.required_evidence.contains(&e.id)
                        })
                        .map(|e| (e.id.clone(), e.value.clone()))
                        .collect(),
                    questions: unresolved.clone(),
                };
                let evidence_identity: BTreeMap<_, _> = request
                    .evidence
                    .iter()
                    .map(|e| (e.id.clone(), (e.revision, e.digest_sha256.clone())))
                    .collect();
                // Serialize the identity directly; avoid building a second JSON tree.
                let cache_material = (
                    "prediction-cache-v2",
                    &scope,
                    &self.graph.pack.id,
                    &request.decision_type,
                    &self.graph.content_hash_sha256,
                    &self.graph.pack.state_preparation,
                    &evidence_identity,
                    &semantic.state,
                    &semantic.questions,
                    &identity.provider_id,
                    &identity.model_id,
                    &identity.adapter_version,
                    &identity.allowed_returned_models,
                );
                let cache_key = format!(
                    "{:x}",
                    Sha256::digest(
                        serde_json::to_vec(&cache_material)
                            .map_err(|_| RuntimeError::InvalidRequest)?
                    )
                );
                // Only coordination is local: every follower re-reads the durable
                // exact cache and rechecks qualification, expiry and output validity.
                prediction_guard = Some(
                    performance::timed(
                        performance::Kind::Queue,
                        performance::prediction_lock(&cache_key),
                    )
                    .await,
                );
                let cached = self
                    .store
                    .cached_prediction(
                        &scope,
                        &cache_key,
                        SystemTime::now()
                            .duration_since(UNIX_EPOCH)
                            .map_err(|_| RuntimeError::InvalidRequest)?
                            .as_millis() as u64,
                    )
                    .await
                    .ok()
                    .flatten()
                    .filter(|r| semantic_response_valid(r, &identity, &unresolved));
                let estimate = provider.worst_case_cost_nano_usd(&semantic);
                let result = if let Some(response) = cached {
                    // Coordination protects cache population/paid dispatch. A validated
                    // hit has no shared write; its receipt and qualification checks
                    // must not serialize all other callers of the same prediction.
                    drop(prediction_guard.take());
                    prediction_cache_hit = true;
                    Some(Ok(response))
                } else if cost_cap == 0
                    || max_attempts == 0
                    || estimate.is_none_or(|c| c > cost_cap)
                {
                    reasons.push(ReasonCode::BudgetUnavailable);
                    None
                } else if let Ok(Ok(_permit)) = performance::timed(
                    performance::Kind::Queue,
                    tokio::time::timeout_at(
                        tokio::time::Instant::from_std(deadline),
                        self.provider_permits.acquire(),
                    ),
                )
                .await
                {
                    match self
                        .store
                        .reserve(
                            &scope,
                            &request.idempotency_key,
                            generation,
                            estimate.expect("priced dispatch"),
                            &identity.provider_id,
                            &identity.model_id,
                        )
                        .await?
                    {
                        Reserve::Denied => {
                            reasons.push(ReasonCode::BudgetUnavailable);
                            None
                        }
                        Reserve::Granted => {
                            provider_id = Some(identity.provider_id.clone());
                            model_id = Some(identity.model_id.clone());
                            attempts = 1;
                            let provider_start = Instant::now();
                            let call = provider.evaluate(semantic, deadline);
                            let result = match deadline.checked_duration_since(Instant::now()) {
                                Some(remain) => performance::timed(
                                    performance::Kind::Provider,
                                    tokio::time::timeout(remain, call),
                                )
                                .await
                                .unwrap_or(Err(kernel_provider::ProviderError::Timeout)),
                                None => Err(kernel_provider::ProviderError::Timeout),
                            };
                            provider_latency_ms = Some(provider_start.elapsed().as_millis() as u64);
                            Some(result)
                        }
                    }
                } else {
                    reasons.push(ReasonCode::ProviderTimeout);
                    None
                };
                if let Some(result) = result {
                    if attempts > 0
                        && let Ok(response) = &result
                    {
                        provider_reported_cost_nano_usd = response.reported_cost_nano_usd;
                        provider_usage = response.usage.clone();
                    }
                    if let Err(kernel_provider::ProviderError::BilledInvalid { cost, usage }) =
                        &result
                    {
                        provider_reported_cost_nano_usd = *cost;
                        provider_usage = usage.clone();
                    }
                    match result {
                        Ok(response)
                            if semantic_response_valid(&response, &identity, &unresolved)
                                && (attempts == 0
                                    || response
                                        .reported_cost_nano_usd
                                        .is_none_or(|c| c <= estimate.unwrap_or(0))) =>
                        {
                            if attempts > 0 {
                                provider_reported_cost_nano_usd = response.reported_cost_nano_usd;
                                provider_usage = response.usage.clone();
                            }
                            for q in &unresolved {
                                if let Some(a) = response.answers.get(&q.id) {
                                    answers.insert(q.output.id().to_owned(), a.clone());
                                }
                            }
                            provider_id = Some(identity.provider_id.clone());
                            model_id = Some(identity.model_id.clone());
                            returned_model_id = Some(response.model_id.clone());
                            if attempts > 0 && response.answers.values().all(Answer::is_evaluated) {
                                let evidence_expiry = request
                                    .evidence
                                    .iter()
                                    .filter_map(|e| e.expires_at_unix_ms)
                                    .min()
                                    .unwrap_or(u64::MAX);
                                let expiry = now_ms.saturating_add(300_000).min(evidence_expiry);
                                cache_candidate = Some((cache_key, response, expiry));
                            }
                            let allowed = self.graph.accept_indices.iter().all(|&i| predicates[i]);
                            let approval = if self.graph.pack.policy.accept_qualified_semantic
                                && self.graph.pack.qualification.semantic_acceptance_qualified
                            {
                                match self.graph.pack.qualification.qualification_ref.as_deref() {
                                    Some(reference) => self
                                        .store
                                        .qualification_active(
                                            &scope,
                                            reference,
                                            &self.graph.content_hash_sha256,
                                            &identity,
                                            returned_model_id
                                                .as_deref()
                                                .expect("validated response model"),
                                            now_ms,
                                        )
                                        .await
                                        .unwrap_or(false),
                                    None => false,
                                }
                            } else {
                                false
                            };
                            if allowed
                                && approval
                                && answers.len() == self.graph.pack.outputs.len()
                                && answers.values().all(Answer::is_evaluated)
                            {
                                disposition = Disposition::Accept;
                                reasons.push(ReasonCode::QualifiedSemanticResolution);
                            } else {
                                reasons.push(ReasonCode::UnqualifiedSemantic);
                            }
                        }
                        Ok(_) => reasons.push(ReasonCode::InvalidProviderOutput),
                        Err(
                            kernel_provider::ProviderError::BilledInvalid { .. }
                            | kernel_provider::ProviderError::Invalid,
                        ) => reasons.push(ReasonCode::InvalidProviderOutput),
                        Err(kernel_provider::ProviderError::Timeout) => {
                            reasons.push(ReasonCode::ProviderTimeout)
                        }
                        Err(_) => reasons.push(ReasonCode::ProviderUnavailable),
                    }
                }
            }
        } else {
            reasons.push(ReasonCode::ProviderUnavailable);
        }
        apply_fallback(&self.graph, &mut disposition, &reasons);
        for output in &self.graph.pack.outputs {
            answers
                .entry(output.id().to_owned())
                .or_insert(Answer::NotEvaluated {
                    reason: reasons.first().cloned().unwrap_or(ReasonCode::NotEvaluated),
                });
        }
        let receipt = DecisionReceipt {
            receipt_id: recovery.receipt_id,
            scope: scope.clone(),
            request_digest_sha256: digest.clone(),
            release: request.release,
            release_digest_sha256: self.graph.content_hash_sha256.clone(),
            answers,
            disposition,
            reason_codes: reasons,
            evidence_revisions: refs,
            provider_id,
            model_id,
            returned_model_id,
            attempts,
            prediction_cache_hit,
            cost_nano_usd: if attempts == 0 { Some(0) } else { None },
            provider_reported_cost_nano_usd,
            provider_usage,
            created_at_unix_ms: now_ms,
            kernel_compute_ms: performance::current(start.elapsed()).engine_us / 1000,
            db_claim_ms,
            provider_latency_ms,
            db_commit_ms: None,
            total_latency_ms: None,
        };
        let committed = self
            .store
            .commit(
                &scope,
                &request.idempotency_key,
                &digest,
                generation,
                &receipt,
            )
            .await?;
        if let Some((key, response, expiry)) = cache_candidate {
            let _ = self
                .store
                .record_prediction(&scope, &key, &response, expiry)
                .await;
        }
        drop(prediction_guard);
        Ok(committed)
    }
}

/// Container probe without a shell or external HTTP client. Exits before database startup.
pub fn readiness_probe(default_address: &str, address_variable: &str) -> Option<bool> {
    if std::env::args().nth(1).as_deref() != Some("--check-ready") {
        return None;
    }
    use std::io::{Read, Write};
    let result = (|| -> std::io::Result<bool> {
        let address = std::env::var(address_variable).unwrap_or_else(|_| default_address.into());
        let address = address.replace("0.0.0.0:", "127.0.0.1:");
        let mut stream = std::net::TcpStream::connect_timeout(
            &address.parse().map_err(std::io::Error::other)?,
            std::time::Duration::from_secs(2),
        )?;
        stream.set_read_timeout(Some(std::time::Duration::from_secs(4)))?;
        stream.write_all(b"GET /readyz HTTP/1.0\r\nHost: localhost\r\n\r\n")?;
        let mut buffer = [0u8; 64];
        let count = stream.read(&mut buffer)?;
        Ok(buffer[..count].starts_with(b"HTTP/1.0 200")
            || buffer[..count].starts_with(b"HTTP/1.1 200"))
    })();
    Some(result.unwrap_or(false))
}
