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
}
pub fn evaluate_prefix(
    graph: &CompiledDecisionGraph,
    state: &BTreeMap<String, serde_json::Value>,
    valid_evidence: &BTreeSet<String>,
) -> Result<PrefixResult, RuntimeError> {
    let mut predicates = vec![false; graph.pack.nodes.len()];
    let mut answers = BTreeMap::new();
    let mut unresolved = Vec::new();
    for &idx in &graph.execution_order {
        let node = &graph.pack.nodes[idx];
        let enabled = graph.dependencies[idx].iter().all(|&i| predicates[i]);
        match &node.operation {
            Operation::FactEquals { field, value } => {
                predicates[idx] = state.get(field) == Some(value)
            }
            Operation::EvidencePresent { evidence_id } => {
                predicates[idx] = valid_evidence.contains(evidence_id)
            }
            Operation::BooleanAnd => predicates[idx] = enabled,
            Operation::BooleanOr => {
                predicates[idx] = graph.dependencies[idx].iter().any(|&i| predicates[i])
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
    })
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
    let start = Instant::now();
    let (decision_type, version) = request
        .release
        .rsplit_once('@')
        .ok_or(RuntimeError::Release)?;
    if decision_type != graph.pack.id || version != graph.pack.version {
        return Err(RuntimeError::Release);
    }
    let mut valid_evidence = BTreeSet::new();
    for e in &request.evidence {
        if e.observed_at_unix_ms > now_ms
            || e.expires_at_unix_ms.is_some_and(|t| t <= now_ms)
            || e.digest_sha256.len() != 64
        {
            continue;
        }
        let actual = format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(&e.value).map_err(|_| RuntimeError::InvalidRequest)?)
        );
        if actual == e.digest_sha256 {
            valid_evidence.insert(e.id.clone());
        }
    }
    let PrefixResult {
        predicates,
        answers,
        unresolved,
    } = evaluate_prefix(graph, &request.state, &valid_evidence)?;
    let hard_deny = graph.hard_deny_indices.iter().any(|&i| predicates[i]);
    let missing_evidence = graph
        .pack
        .required_evidence
        .iter()
        .any(|id| !valid_evidence.contains(id));
    let mut disposition = Disposition::Review;
    let mut reasons = Vec::new();
    if hard_deny {
        disposition = Disposition::Deny;
        reasons.push(ReasonCode::HardPolicyDeny);
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
    } else {
        reasons.push(ReasonCode::NotEvaluated);
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
    fn setup(hard_deny: bool) -> (Engine, Arc<AtomicUsize>, Scope, DecisionRequest) {
        let pack = DecisionPack {
            api_version: "kernel/v1".into(),
            kind: "DecisionPack".into(),
            id: "test".into(),
            version: "1".into(),
            required_evidence: vec![],
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
        let mut second = request;
        second.idempotency_key = "second-key".into();
        second.constraints.max_billable_attempts = Some(0);
        let reused = engine.decide(scope, second).await.unwrap();
        assert_eq!(reused.attempts, 0);
        assert!(reused.prediction_cache_hit);
        assert_eq!(reused.disposition, Disposition::Review);
    }
}

impl Engine {
    pub async fn decide(
        &self,
        scope: Scope,
        request: DecisionRequest,
    ) -> Result<DecisionReceipt, RuntimeError> {
        let start = Instant::now();
        if request.schema_version != "1.1"
            || request.decision_type != self.graph.pack.id
            || request.release != format!("{}@{}", self.graph.pack.id, self.graph.pack.version)
            || request.idempotency_key.is_empty()
            || request.idempotency_key.len() > 128
            || request.state.len() > 128
            || request.evidence.len() > 32
        {
            return Err(RuntimeError::InvalidRequest);
        }
        let bytes = serde_json::to_vec(&request).map_err(|_| RuntimeError::InvalidRequest)?;
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
        let deadline_ms = self
            .platform_deadline_ms
            .min(self.graph.pack.limits.deadline_ms)
            .min(request.constraints.deadline_ms.unwrap_or(u64::MAX));
        let deadline = start + std::time::Duration::from_millis(deadline_ms);
        let claim_start = Instant::now();
        let generation = loop {
            match self
                .store
                .claim(&scope, &request.idempotency_key, &digest)
                .await?
            {
                Claim::Existing(r) => return Ok(*r),
                Claim::Busy if Instant::now() < deadline => {
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await
                }
                Claim::Busy => return Err(RuntimeError::Store),
                Claim::Conflict => return Err(RuntimeError::Conflict),
                Claim::New(generation) => break generation,
            }
        };
        let db_claim_ms = claim_start.elapsed().as_millis() as u64;
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
        let mut valid_evidence = BTreeSet::new();
        for e in &request.evidence {
            if e.observed_at_unix_ms > now_ms
                || e.expires_at_unix_ms.is_some_and(|t| t <= now_ms)
                || e.digest_sha256.len() != 64
            {
                continue;
            }
            let actual = format!(
                "{:x}",
                Sha256::digest(
                    serde_json::to_vec(&e.value).map_err(|_| RuntimeError::InvalidRequest)?
                )
            );
            if actual == e.digest_sha256 {
                valid_evidence.insert(e.id.clone());
            }
        }
        let PrefixResult {
            predicates,
            mut answers,
            unresolved,
        } = evaluate_prefix(&self.graph, &request.state, &valid_evidence)?;
        let hard_deny = self.graph.hard_deny_indices.iter().any(|&i| predicates[i]);
        let missing_evidence = self
            .graph
            .pack
            .required_evidence
            .iter()
            .any(|id| !valid_evidence.contains(id));
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
        let mut prediction_cache_hit = false;
        let mut cache_candidate: Option<(String, SemanticResponse, u64)> = None;
        let mut provider_latency_ms = None;
        if hard_deny {
            disposition = Disposition::Deny;
            reasons.push(ReasonCode::HardPolicyDeny);
        } else if Instant::now() >= deadline {
            reasons.push(ReasonCode::DeadlineExceeded);
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
                    state: request
                        .state
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
                let cache_material = serde_json::json!({
                    "tenant":&scope.tenant_id,"project":&scope.project_id,"environment":&scope.environment,
                    "purpose":&self.graph.pack.id,"decision_type":&request.decision_type,
                    "release":&self.graph.content_hash_sha256,"state_builder":"v1",
                    "evidence":&evidence_identity,"semantic_state":&semantic.state,"questions":&semantic.questions,
                    "provider":&identity.provider_id,"model":&identity.model_id,"adapter":&identity.adapter_version,
                    "allowed_returned_models":&identity.allowed_returned_models
                });
                let cache_key = format!(
                    "{:x}",
                    Sha256::digest(
                        serde_json::to_vec(&cache_material)
                            .map_err(|_| RuntimeError::InvalidRequest)?
                    )
                );
                let cached = self
                    .store
                    .cached_prediction(&scope, &cache_key, now_ms)
                    .await
                    .ok()
                    .flatten()
                    .filter(|r| semantic_response_valid(r, &identity, &unresolved));
                let result = if let Some(response) = cached {
                    prediction_cache_hit = true;
                    Some(Ok(response))
                } else if cost_cap == 0 || max_attempts == 0 {
                    reasons.push(ReasonCode::BudgetUnavailable);
                    None
                } else if let Ok(Ok(_permit)) = tokio::time::timeout_at(
                    tokio::time::Instant::from_std(deadline),
                    self.provider_permits.acquire(),
                )
                .await
                {
                    match self
                        .store
                        .reserve(
                            &scope,
                            &request.idempotency_key,
                            generation,
                            cost_cap,
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
                            attempts = 1;
                            let provider_start = Instant::now();
                            let call = provider.evaluate(semantic, deadline);
                            let result = match deadline.checked_duration_since(Instant::now()) {
                                Some(remain) => tokio::time::timeout(remain, call)
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
                    match result {
                        Ok(response)
                            if semantic_response_valid(&response, &identity, &unresolved)
                                && (attempts == 0
                                    || response
                                        .reported_cost_nano_usd
                                        .is_none_or(|c| c <= cost_cap)) =>
                        {
                            if attempts > 0 {
                                provider_reported_cost_nano_usd = response.reported_cost_nano_usd;
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
        for output in &self.graph.pack.outputs {
            answers
                .entry(output.id().to_owned())
                .or_insert(Answer::NotEvaluated {
                    reason: reasons.first().cloned().unwrap_or(ReasonCode::NotEvaluated),
                });
        }
        let receipt = DecisionReceipt {
            receipt_id: format!(
                "{:x}",
                Sha256::digest(format!(
                    "{}:{}:{}",
                    scope.tenant_id, request.idempotency_key, digest
                ))
            ),
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
            cost_nano_usd: None,
            provider_reported_cost_nano_usd,
            created_at_unix_ms: now_ms,
            kernel_compute_ms: (start.elapsed().as_millis() as u64)
                .saturating_sub(db_claim_ms)
                .saturating_sub(provider_latency_ms.unwrap_or(0)),
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
        Ok(committed)
    }
}
