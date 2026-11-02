// Included in the runtime test module to share its in-memory receipt store.
fn tier2_pack(name: &str) -> DecisionPack {
    let bytes: &[u8] = match name {
        "expense" => include_bytes!("../../../packs/examples/expense-approval.json"),
        _ => include_bytes!("../../../packs/examples/content-moderation.json"),
    };
    serde_json::from_slice(bytes).unwrap()
}
fn tier2_request(pack: &DecisionPack, state: serde_json::Value) -> DecisionRequest {
    DecisionRequest {
        schema_version: "1.1".into(),
        decision_type: pack.id.clone(),
        release: format!("{}@{}", pack.id, pack.version),
        idempotency_key: "tier2-key".into(),
        state: serde_json::from_value(state).unwrap(),
        evidence: vec![],
        constraints: RequestLimits::default(),
    }
}
fn tier2_evidence(value: serde_json::Value, age: u64) -> kernel_core::EvidenceRef {
    kernel_core::EvidenceRef {
        id: "receipt".into(),
        revision: 1,
        observed_at_unix_ms: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64
            - age,
        expires_at_unix_ms: None,
        digest_sha256: format!("{:x}", Sha256::digest(serde_json::to_vec(&value).unwrap())),
        sensitivity: "standard".into(),
        value,
    }
}
fn tier2_engine(pack: DecisionPack) -> (Engine, Scope) {
    let (mut engine, _, scope, _) = setup(false);
    engine.graph = Arc::new(compile(pack).unwrap());
    engine.provider = None;
    engine.platform_max_cost_nano_usd = 1000;
    (engine, scope)
}
#[tokio::test]
async fn tier2_multiple_packs_and_all_dispositions_use_the_same_engine() {
    use serde_json::json;
    for (amount, vendor, uncertain, country, expected) in [
        (100, true, false, " IN ", Disposition::Accept),
        (1000, true, false, "in", Disposition::Review),
        (1000, true, true, "in", Disposition::Abstain),
        (1000, false, true, "in", Disposition::Deny),
        (100, true, false, "unsupported", Disposition::Abstain),
    ] {
        let pack = tier2_pack("expense");
        let mut request = tier2_request(
            &pack,
            json!({"amount":amount,"vendor_allowed":vendor,"uncertain":uncertain,"country":country}),
        );
        request
            .evidence
            .push(tier2_evidence(json!({"reference":"r-1"}), 100));
        let (engine, scope) = tier2_engine(pack);
        let pure = evaluate_pure(&engine.graph, scope.clone(), request.clone()).unwrap();
        let receipt = engine.decide(scope, request).await.unwrap();
        assert_eq!(receipt.disposition, expected);
        assert_eq!(pure.disposition, receipt.disposition);
        assert_eq!(receipt.answers.len(), 3);
        assert_eq!(receipt.answers, pure.answers);
        assert_eq!(receipt.attempts, 0);
        assert!(
            matches!(&receipt.answers["priority"], Answer::Ordinal { selected_anchor, .. } if selected_anchor == "normal")
        );
        assert!(
            matches!(&receipt.answers["route"], Answer::Categorical { selected_id, .. } if selected_id == "reimbursement")
        );
        assert!(
            matches!(&receipt.answers["eligible"], Answer::Binary { value: Some(v), .. } if *v == vendor)
        );
    }
    let pack = tier2_pack("content");
    let request = tier2_request(&pack, json!({"text":"hello","blocked":false}));
    let (engine, scope) = tier2_engine(pack);
    let pure = evaluate_pure(&engine.graph, scope.clone(), request.clone()).unwrap();
    let receipt = engine.decide(scope, request).await.unwrap();
    assert_eq!(receipt.disposition, Disposition::Abstain);
    assert_eq!(receipt.reason_codes, vec![ReasonCode::ProviderUnavailable]);
    assert_eq!(pure.disposition, receipt.disposition);
    assert!(receipt.answers.values().all(|a| !a.is_evaluated()));
}
#[tokio::test]
async fn tier2_invalid_input_never_commits_and_preparation_is_release_bound() {
    use serde_json::json;
    let pack = tier2_pack("expense");
    let (engine, scope) = tier2_engine(pack.clone());
    for value in [
        json!({"amount":"100","vendor_allowed":true,"country":"in"}),
        json!({"amount":-1,"vendor_allowed":true,"country":"in"}),
        json!({"amount":100,"country":"in"}),
        json!({"amount":100,"vendor_allowed":true,"country":"in","extra":true}),
        json!({"amount":100,"vendor_allowed":true,"country":"in","priority":"critical"}),
    ] {
        let request = tier2_request(&pack, value);
        assert!(matches!(
            engine.decide(scope.clone(), request.clone()).await,
            Err(RuntimeError::InvalidRequest)
        ));
        assert!(matches!(
            evaluate_pure(&engine.graph, scope.clone(), request),
            Err(RuntimeError::InvalidRequest)
        ));
    }
    let raw = BTreeMap::from([
        ("amount".into(), json!(100)),
        ("vendor_allowed".into(), json!(true)),
        ("country".into(), json!(" IN ")),
    ]);
    let prepared = prepare_state(&engine.graph, &raw).unwrap();
    assert_eq!(prepared["region"], json!("in"));
    assert_eq!(prepared, prepare_state(&engine.graph, &raw).unwrap());
    assert!(!raw.contains_key("region"));
    let mut changed = pack.clone();
    changed.state_preparation.as_mut().unwrap().steps.pop();
    assert_ne!(
        compile(changed).unwrap().content_hash_sha256,
        engine.graph.content_hash_sha256
    );
    let request = tier2_request(&pack, serde_json::to_value(raw).unwrap());
    assert!(engine.decide(scope, request).await.is_ok());
}
#[tokio::test]
async fn tier2_evidence_type_age_integrity_and_missing_require_review() {
    use serde_json::json;
    for evidence in [
        None,
        Some(tier2_evidence(json!({"reference":123}), 100)),
        Some(tier2_evidence(json!({"reference":"r"}), 86400001)),
        Some({
            let mut e = tier2_evidence(json!({"reference":"r"}), 100);
            e.digest_sha256 = "0".repeat(64);
            e
        }),
    ] {
        let pack = tier2_pack("expense");
        let mut request = tier2_request(
            &pack,
            json!({"amount":100,"vendor_allowed":true,"country":"in"}),
        );
        request.evidence.extend(evidence);
        let (engine, scope) = tier2_engine(pack);
        let receipt = engine.decide(scope, request).await.unwrap();
        assert_eq!(receipt.disposition, Disposition::Review);
        assert_eq!(receipt.reason_codes, vec![ReasonCode::MissingEvidence]);
        assert_eq!(receipt.attempts, 0);
    }
}
struct Tier2Provider {
    mode: &'static str,
    calls: Arc<AtomicUsize>,
}
impl IntelligenceProvider for Tier2Provider {
    fn identity(&self) -> ProviderIdentity {
        ProviderIdentity {
            provider_id: "fixture".into(),
            model_id: "fixture".into(),
            adapter_version: "fixture-v1".into(),
            allowed_returned_models: vec![],
        }
    }
    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            supports_batch: true,
            max_questions: 8,
            max_output_tokens: 4096,
        }
    }
    fn worst_case_cost_nano_usd(&self, _: &SemanticRequest) -> Option<u64> {
        Some(100)
    }
    fn evaluate<'a>(
        &'a self,
        request: SemanticRequest,
        _: Instant,
    ) -> Pin<Box<dyn Future<Output = Result<SemanticResponse, ProviderError>> + Send + 'a>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move {
            assert_eq!(request.state["text"], serde_json::json!("hello"));
            assert_eq!(request.state.len(), 1); // raw policy flags never egress
            match self.mode {
                "timeout" => return Err(ProviderError::Timeout),
                "unavailable" => return Err(ProviderError::Unavailable),
                "invalid" => return Err(ProviderError::Invalid),
                _ => {}
            }
            let mut answers = BTreeMap::from([
                (
                    "safety".into(),
                    Answer::Binary {
                        proposition: "content_is_safe".into(),
                        value: Some(true),
                        probability_true: None,
                        probability_provenance: None,
                    },
                ),
                (
                    "category".into(),
                    Answer::Categorical {
                        selected_id: if self.mode == "bad_label" {
                            "invented"
                        } else {
                            "ordinary"
                        }
                        .into(),
                        probabilities: None,
                        probability_provenance: None,
                    },
                ),
                (
                    "severity".into(),
                    Answer::Ordinal {
                        scale_id: "moderation_severity/v1".into(),
                        selected_anchor: "low".into(),
                        distribution: None,
                        probability_provenance: None,
                    },
                ),
            ]);
            match self.mode {
                "missing" => {
                    answers.remove("severity");
                }
                "extra" => {
                    answers.insert("extra".into(), answers["safety"].clone());
                }
                "bad_kind" => {
                    answers.insert("category".into(), answers["safety"].clone());
                }
                _ => {}
            }
            Ok(SemanticResponse {
                answers,
                provider_id: "fixture".into(),
                model_id: if self.mode == "bad_model" {
                    "other"
                } else {
                    "fixture"
                }
                .into(),
                reported_cost_nano_usd: Some(100),
                usage: None,
            })
        })
    }
}
#[tokio::test]
async fn tier2_typed_semantics_reject_malformed_results_without_caching_and_fallback_explicitly() {
    use serde_json::json;
    for mode in [
        "valid",
        "timeout",
        "unavailable",
        "invalid",
        "bad_label",
        "missing",
        "extra",
        "bad_kind",
        "bad_model",
    ] {
        let pack = tier2_pack("content");
        let mut request = tier2_request(&pack, json!({"text":"  hello  ","blocked":false}));
        let (mut engine, scope) = tier2_engine(pack);
        let calls = Arc::new(AtomicUsize::new(0));
        engine.provider = Some(Arc::new(Tier2Provider {
            mode,
            calls: calls.clone(),
        }));
        let first = engine.decide(scope.clone(), request.clone()).await.unwrap();
        assert_eq!(first.attempts, 1);
        if mode == "valid" {
            assert_eq!(first.disposition, Disposition::Review); // unqualified answers cannot accept
            assert!(first.answers.values().all(Answer::is_evaluated));
        } else {
            assert_eq!(first.disposition, Disposition::Abstain, "{mode}");
            assert!(first.answers.values().all(|a| !a.is_evaluated()), "{mode}");
        }
        request.idempotency_key = "next".into();
        let second = engine.decide(scope, request).await.unwrap();
        assert_eq!(second.prediction_cache_hit, mode == "valid");
        assert_eq!(
            calls.load(Ordering::SeqCst),
            if mode == "valid" { 1 } else { 2 }
        );
    }
}
#[test]
fn tier2_pack_validation_rejects_invalid_schema_preparation_policy_and_output_contracts() {
    let original = serde_json::to_value(tier2_pack("expense")).unwrap();
    for (path, value) in [
        (
            "/state_preparation/version",
            serde_json::json!("kernel/state-v99"),
        ),
        ("/input_schema/required", serde_json::json!(["nonexistent"])),
        (
            "/input_schema/properties/country/max_length",
            serde_json::json!(1),
        ),
        ("/policy/review_nodes", serde_json::json!(["eligibility"])),
        ("/policy/abstain_nodes", serde_json::json!(["missing"])),
        ("/nodes/1/depends_on", serde_json::json!([])),
        ("/outputs/2/scale_id", serde_json::json!("")),
        (
            "/required_evidence",
            serde_json::json!(["receipt", "receipt"]),
        ),
    ] {
        let mut value_pack = original.clone();
        *value_pack.pointer_mut(path).unwrap() = value;
        let pack: DecisionPack = serde_json::from_value(value_pack).unwrap();
        assert!(compile(pack).is_err(), "{path}");
    }
    let mut unknown = original.clone();
    unknown["input_schema"]["unsupported_keyword"] = serde_json::json!(true);
    assert!(serde_json::from_value::<DecisionPack>(unknown).is_err());
    let mut unknown = original;
    unknown["policy"]["fallback"]["provider_failure"] = serde_json::json!("accept");
    assert!(serde_json::from_value::<DecisionPack>(unknown).is_err());
}
#[test]
fn tier2_old_pack_serialization_preserves_its_content_digest() {
    let original: serde_json::Value = serde_json::from_slice(include_bytes!(
        "../../../packs/examples/support-triage.json"
    ))
    .unwrap();
    let graph = compile(serde_json::from_value(original).unwrap()).unwrap();
    let mut wire = serde_json::to_value(&graph.pack).unwrap();
    for key in ["input_schema", "state_preparation", "evidence_requirements"] {
        assert!(wire.get(key).is_none());
    }
    for key in ["review_nodes", "abstain_nodes", "fallback"] {
        assert!(wire["policy"].get(key).is_none());
    }
    // These are precisely the fields serialized before Tier 2.
    let reserialized: DecisionPack = serde_json::from_value(wire.take()).unwrap();
    assert_eq!(
        compile(reserialized).unwrap().content_hash_sha256,
        graph.content_hash_sha256
    );
}
#[tokio::test]
async fn tier2_explicit_policy_prevents_dispatch_and_overrides_failure_fallback() {
    use serde_json::json;
    for expected in [Disposition::Deny, Disposition::Abstain, Disposition::Review] {
        let mut pack = tier2_pack("content");
        pack.policy.hard_deny_nodes.clear();
        match expected {
            Disposition::Deny => pack.policy.hard_deny_nodes.push("blocked".into()),
            Disposition::Abstain => pack.policy.abstain_nodes.push("blocked".into()),
            Disposition::Review => pack.policy.review_nodes.push("blocked".into()),
            _ => unreachable!(),
        }
        pack.policy.fallback.as_mut().unwrap().unresolved =
            kernel_compiler::FallbackDisposition::Abstain;
        let request = tier2_request(&pack, json!({"text":"hello","blocked":true}));
        let (mut engine, scope) = tier2_engine(pack);
        let calls = Arc::new(AtomicUsize::new(0));
        engine.provider = Some(Arc::new(Tier2Provider {
            mode: "valid",
            calls: calls.clone(),
        }));
        let receipt = engine.decide(scope, request).await.unwrap();
        assert_eq!(receipt.disposition, expected);
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert!(receipt.answers.values().all(|a| !a.is_evaluated()));
    }
}
#[test]
fn tier2_unknown_negation_and_invalid_state_outputs_cannot_accept() {
    use serde_json::json;
    let mut pack = tier2_pack("expense");
    pack.input_schema = None;
    pack.state_preparation = None;
    let graph = compile(pack.clone()).unwrap();
    let prefix = evaluate_prefix(&graph, &BTreeMap::new(), &BTreeSet::new()).unwrap();
    assert!(!prefix.predicates[1]); // NOT of unknown vendor permission cannot assert deny or allow
    assert!(prefix.policy_unknown);
    let (_, scope) = tier2_engine(pack.clone());
    let mut request = tier2_request(
        &pack,
        json!({"vendor_allowed":true,"amount":100,"region":"in","uncertain":false,"route":"invented","priority":"invented"}),
    );
    request
        .evidence
        .push(tier2_evidence(json!({"reference":"r"}), 100));
    let result = evaluate_pure(&graph, scope, request).unwrap();
    assert_eq!(result.disposition, Disposition::Abstain);
    assert!(!result.answers["route"].is_evaluated());
    assert!(!result.answers["priority"].is_evaluated());
}
#[test]
fn tier2_nested_input_contracts_reject_wrong_types_unknown_fields_and_unbounded_schema() {
    use serde_json::json;
    let schema: kernel_compiler::InputSchema = serde_json::from_value(json!({
        "type":"object", "properties":{"items":{"type":"array","max_items":2,"items":{
            "type":"object", "properties":{"quantity":{"type":"integer","minimum":1,"maximum":4}},"required":["quantity"]
        }}},"required":["items"]
    })).unwrap();
    assert!(schema.accepts(&json!({"items":[{"quantity":1},{"quantity":4}]})));
    for input in [
        json!({"items":[{"quantity":1.5}]}),
        json!({"items":[{"quantity":0}]}),
        json!({"items":[{"quantity":2,"extra":true}]}),
        json!({"items":[{"quantity":1},{"quantity":2},{"quantity":3}]}),
        json!({}),
    ] {
        assert!(!schema.accepts(&input));
    }
    let mut nested = json!({"type":"boolean"});
    for _ in 0..10 {
        nested = json!({"type":"array","items":nested});
    }
    let mut pack = tier2_pack("content");
    pack.input_schema = Some(
        serde_json::from_value(json!({"type":"object","properties":{"deep":nested}})).unwrap(),
    );
    assert!(compile(pack).is_err());
}
