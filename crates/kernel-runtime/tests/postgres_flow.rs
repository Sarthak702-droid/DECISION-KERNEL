use kernel_compiler::{DecisionPack, Node, Operation, PackLimits, Policy, Qualification, compile};
use kernel_core::{Answer, DecisionRequest, Disposition, OutputDefinition, RequestLimits, Scope};
use kernel_provider::{FixtureProvider, SemanticResponse};
use kernel_runtime::{Claim, Engine, ReceiptStore, Reserve, postgres::PostgresStore};
use sha2::Digest;
use sqlx::{PgPool, postgres::PgPoolOptions};
use std::{
    collections::BTreeMap,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};
use tokio::sync::Semaphore;

#[tokio::test]
async fn postgres_receipts_cache_and_budget_are_scoped() {
    let Ok(url) = std::env::var("KERNEL_TEST_DATABASE_URL") else {
        assert!(
            std::env::var_os("CI").is_none(),
            "KERNEL_TEST_DATABASE_URL is required in CI"
        );
        eprintln!("set KERNEL_TEST_DATABASE_URL to run PostgreSQL integration test");
        return;
    };
    let pool = PgPool::connect(&url).await.unwrap();
    sqlx::raw_sql(include_str!("../../../migrations/0001_init.sql"))
        .execute(&pool)
        .await
        .unwrap();
    sqlx::raw_sql(include_str!(
        "../../../migrations/0002_prediction_cache.sql"
    ))
    .execute(&pool)
    .await
    .unwrap();
    sqlx::raw_sql(include_str!("../../../migrations/0003_service_tokens.sql"))
        .execute(&pool)
        .await
        .unwrap();
    sqlx::raw_sql(include_str!("../../../migrations/0004_qualifications.sql"))
        .execute(&pool)
        .await
        .unwrap();
    sqlx::raw_sql(include_str!(
        "../../../migrations/0006_qualification_review.sql"
    ))
    .execute(&pool)
    .await
    .unwrap();
    sqlx::raw_sql(include_str!(
        "../../../migrations/0007_reviews_outcomes.sql"
    ))
    .execute(&pool)
    .await
    .unwrap();
    sqlx::raw_sql(include_str!(
        "../../../migrations/0009_returned_model_qualification.sql"
    ))
    .execute(&pool)
    .await
    .unwrap();
    sqlx::raw_sql(include_str!("../../../migrations/0010_tenant_rls.sql"))
        .execute(&pool)
        .await
        .unwrap();
    sqlx::raw_sql(include_str!("../../../migrations/0011_outbox.sql"))
        .execute(&pool)
        .await
        .unwrap();
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let scope = Scope {
        tenant_id: format!("runtime-test-{unique}"),
        project_id: "project".into(),
        environment: "test".into(),
        product_id: "product".into(),
    };
    sqlx::query("INSERT INTO budget_buckets (tenant_id,project_id,purpose,cap_nano_usd) VALUES ($1,$2,'test',100)")
        .bind(&scope.tenant_id).bind(&scope.project_id).execute(&pool).await.unwrap();
    let pack = DecisionPack {
        api_version: "kernel/v1".into(),
        kind: "DecisionPack".into(),
        id: "test".into(),
        version: "1".into(),
        required_evidence: vec![],
        permitted_provider_sensitivities: vec![],
        semantic_state_fields: vec!["case".into()],
        outputs: vec![OutputDefinition::Binary {
            id: "yes".into(),
            proposition: "yes".into(),
        }],
        nodes: vec![Node {
            id: "q".into(),
            depends_on: vec![],
            operation: Operation::SemanticQuestion {
                output: "yes".into(),
                prompt: "Is it yes?".into(),
            },
        }],
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
            provider_id: Some("fixture".into()),
            model_id: Some("fixture".into()),
            adapter_version: None,
            qualification_ref: None,
        },
        qualification_criteria: None,
    };
    let graph = compile(pack).unwrap();
    sqlx::query("INSERT INTO behavior_releases (tenant_id,project_id,environment,decision_type,version,content_hash_sha256,compiled_graph) VALUES ($1,$2,$3,'test','1',$4,$5)")
        .bind(&scope.tenant_id).bind(&scope.project_id).bind(&scope.environment).bind(&graph.content_hash_sha256).bind(serde_json::to_value(&graph).unwrap())
        .execute(&pool).await.unwrap();
    let store = Arc::new(PostgresStore {
        pool: pool.clone(),
        purpose: "test".into(),
    });
    let engine = Engine {
        graph: Arc::new(graph),
        provider: Some(Arc::new(FixtureProvider {
            response: SemanticResponse {
                answers: BTreeMap::from([(
                    "q".into(),
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
        })),
        store: store.clone() as Arc<dyn ReceiptStore>,
        provider_permits: Arc::new(Semaphore::new(2)),
        platform_deadline_ms: 1000,
        platform_max_cost_nano_usd: 100,
    };
    let request = |key: &str, case: &str| DecisionRequest {
        schema_version: "1.1".into(),
        decision_type: "test".into(),
        release: "test@1".into(),
        idempotency_key: key.into(),
        state: BTreeMap::from([("case".into(), serde_json::json!(case))]),
        evidence: vec![],
        constraints: RequestLimits::default(),
    };
    let first = engine
        .decide(scope.clone(), request("first", "A"))
        .await
        .unwrap();
    assert_eq!(first.attempts, 1);
    assert!(!first.prediction_cache_hit);
    assert_eq!(first.disposition, Disposition::Review);
    let first_events: Vec<String> = sqlx::query_scalar("SELECT event_type FROM outbox_events WHERE tenant_id=$1 AND project_id=$2 AND environment=$3 AND payload->>'idempotency_key'='first' ORDER BY event_type")
        .bind(&scope.tenant_id).bind(&scope.project_id).bind(&scope.environment)
        .fetch_all(&pool).await.unwrap();
    assert_eq!(first_events, vec!["decision.completed", "review.requested"]);
    let duplicate = engine
        .decide(scope.clone(), request("first", "A"))
        .await
        .unwrap();
    assert_eq!(duplicate.receipt_id, first.receipt_id);
    assert!(
        engine
            .decide(scope.clone(), request("first", "B"))
            .await
            .is_err()
    );
    let cached = engine
        .decide(scope.clone(), request("second", "A"))
        .await
        .unwrap();
    assert_eq!(cached.attempts, 0);
    assert!(cached.prediction_cache_hit);
    let blocked = engine
        .decide(scope.clone(), request("third", "B"))
        .await
        .unwrap();
    assert_eq!(blocked.attempts, 0);
    assert_eq!(blocked.disposition, Disposition::Review);
    sqlx::query("UPDATE behavior_releases SET revoked_at=now() WHERE tenant_id=$1 AND project_id=$2 AND environment=$3 AND decision_type='test' AND version='1'")
        .bind(&scope.tenant_id).bind(&scope.project_id).bind(&scope.environment).execute(&pool).await.unwrap();
    let revoked = engine
        .decide(scope.clone(), request("fourth", "A"))
        .await
        .unwrap();
    assert_eq!(revoked.disposition, Disposition::Deny);
    assert_eq!(
        revoked.reason_codes,
        vec![kernel_core::ReasonCode::RevokedRelease]
    );
    let held: i64=sqlx::query_scalar("SELECT held_nano_usd FROM budget_buckets WHERE tenant_id=$1 AND project_id=$2 AND purpose='test'")
        .bind(&scope.tenant_id).bind(&scope.project_id).fetch_one(&pool).await.unwrap();
    assert_eq!(held, 100);

    let other = Scope {
        tenant_id: format!("other-{unique}"),
        ..scope.clone()
    };
    sqlx::query("INSERT INTO budget_buckets (tenant_id,project_id,purpose,cap_nano_usd) VALUES ($1,$2,'test',100)")
        .bind(&other.tenant_id).bind(&other.project_id).execute(&pool).await.unwrap();
    for key in ["race-a", "race-b"] {
        assert!(matches!(
            store.claim(&other, key, key).await.unwrap(),
            Claim::New(1)
        ));
    }
    let (a, b) = tokio::join!(
        store.reserve(&other, "race-a", 1, 80, "fixture", "fixture"),
        store.reserve(&other, "race-b", 1, 80, "fixture", "fixture")
    );
    let grants = [a.unwrap(), b.unwrap()]
        .into_iter()
        .filter(|r| matches!(r, Reserve::Granted))
        .count();
    assert_eq!(grants, 1);
    let other_held: i64=sqlx::query_scalar("SELECT held_nano_usd FROM budget_buckets WHERE tenant_id=$1 AND project_id=$2 AND purpose='test'")
        .bind(&other.tenant_id).bind(&other.project_id).fetch_one(&pool).await.unwrap();
    assert_eq!(other_held, 80);
    let cache_key: String=sqlx::query_scalar("SELECT cache_key_sha256 FROM prediction_cache WHERE tenant_id=$1 AND project_id=$2 LIMIT 1")
        .bind(&scope.tenant_id).bind(&scope.project_id).fetch_one(&pool).await.unwrap();
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    assert!(
        store
            .cached_prediction(&other, &cache_key, now_ms)
            .await
            .unwrap()
            .is_none()
    );

    let recovery = Scope {
        tenant_id: format!("recovery-{unique}"),
        ..scope.clone()
    };
    sqlx::query("INSERT INTO budget_buckets (tenant_id,project_id,purpose,cap_nano_usd) VALUES ($1,$2,'test',100)")
        .bind(&recovery.tenant_id).bind(&recovery.project_id).execute(&pool).await.unwrap();
    assert!(matches!(
        store.claim(&recovery, "recover", "digest").await.unwrap(),
        Claim::New(1)
    ));
    sqlx::query("UPDATE decision_receipts SET status='retryable' WHERE tenant_id=$1 AND project_id=$2 AND environment=$3 AND idempotency_key='recover'")
        .bind(&recovery.tenant_id).bind(&recovery.project_id).bind(&recovery.environment).execute(&pool).await.unwrap();
    assert!(matches!(
        store.claim(&recovery, "recover", "digest").await.unwrap(),
        Claim::New(2)
    ));
    assert!(
        store
            .reserve(&recovery, "recover", 1, 80, "fixture", "fixture")
            .await
            .is_err()
    );
    assert!(matches!(
        store
            .reserve(&recovery, "recover", 2, 80, "fixture", "fixture")
            .await
            .unwrap(),
        Reserve::Granted
    ));

    let qscope = Scope {
        tenant_id: format!("qualified-{unique}"),
        ..scope.clone()
    };
    sqlx::query("INSERT INTO budget_buckets (tenant_id,project_id,purpose,cap_nano_usd) VALUES ($1,$2,'qualified',100)")
        .bind(&qscope.tenant_id).bind(&qscope.project_id).execute(&pool).await.unwrap();
    let qpack = DecisionPack {
        api_version: "kernel/v1".into(),
        kind: "DecisionPack".into(),
        id: "qualified".into(),
        version: "1".into(),
        required_evidence: vec![],
        permitted_provider_sensitivities: vec![],
        semantic_state_fields: vec![],
        outputs: vec![OutputDefinition::Binary {
            id: "yes".into(),
            proposition: "yes".into(),
        }],
        nodes: vec![Node {
            id: "q".into(),
            depends_on: vec![],
            operation: Operation::SemanticQuestion {
                output: "yes".into(),
                prompt: "Is it yes?".into(),
            },
        }],
        policy: Policy {
            hard_deny_nodes: vec![],
            accept_nodes: vec![],
            accept_deterministic: false,
            accept_qualified_semantic: true,
        },
        limits: PackLimits {
            deadline_ms: 1000,
            max_billable_attempts: 1,
            max_cost_nano_usd: 100,
        },
        qualification: Qualification {
            semantic_acceptance_qualified: true,
            provider_id: Some("fixture".into()),
            model_id: Some("fixture".into()),
            adapter_version: Some("fixture-v1".into()),
            qualification_ref: Some("reviewed-set-1".into()),
        },
        qualification_criteria: Some(kernel_compiler::QualificationCriteria {
            min_reviewed_test_cases: 1000,
            min_cases_per_required_slice: 1000,
            min_coverage: 0.9,
            min_accuracy: 0.9,
            required_slices: vec!["yes::test".into()],
        }),
    };
    let qgraph = compile(qpack).unwrap();
    sqlx::query("INSERT INTO behavior_releases (tenant_id,project_id,environment,decision_type,version,content_hash_sha256,compiled_graph) VALUES ($1,$2,$3,'qualified','1',$4,$5)")
        .bind(&qscope.tenant_id).bind(&qscope.project_id).bind(&qscope.environment).bind(&qgraph.content_hash_sha256).bind(serde_json::to_value(&qgraph).unwrap())
        .execute(&pool).await.unwrap();
    let qstore = Arc::new(PostgresStore {
        pool: pool.clone(),
        purpose: "qualified".into(),
    });
    let qengine = Engine {
        graph: Arc::new(qgraph.clone()),
        provider: Some(Arc::new(FixtureProvider {
            response: SemanticResponse {
                answers: BTreeMap::from([(
                    "q".into(),
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
        })),
        store: qstore.clone() as Arc<dyn ReceiptStore>,
        provider_permits: Arc::new(Semaphore::new(1)),
        platform_deadline_ms: 1000,
        platform_max_cost_nano_usd: 100,
    };
    let qrequest = |key: &str| DecisionRequest {
        schema_version: "1.1".into(),
        decision_type: "qualified".into(),
        release: "qualified@1".into(),
        idempotency_key: key.into(),
        state: BTreeMap::new(),
        evidence: vec![],
        constraints: RequestLimits::default(),
    };
    let before = qengine
        .decide(qscope.clone(), qrequest("before"))
        .await
        .unwrap();
    assert_eq!(before.disposition, Disposition::Review);
    assert_eq!(before.attempts, 1);
    sqlx::query("INSERT INTO provider_qualifications (tenant_id,project_id,environment,qualification_ref,release_hash_sha256,provider_id,model_id,adapter_version,evaluation_digest_sha256,reviewed_sample_count,valid_until_unix_ms,evaluation_report,approval_ref,approved_by) VALUES ($1,$2,$3,'reviewed-set-1',$4,'fixture','fixture','fixture-v1',$5,1000,$6,$7,'ticket-1','test-owner')")
        .bind(&qscope.tenant_id).bind(&qscope.project_id).bind(&qscope.environment).bind(&qgraph.content_hash_sha256).bind("a".repeat(64)).bind(i64::try_from(now_ms+60_000).unwrap()).bind(serde_json::json!({"fixture_test":true})).execute(&pool).await.unwrap();
    let identity = kernel_provider::ProviderIdentity {
        provider_id: "fixture".into(),
        model_id: "fixture".into(),
        adapter_version: "fixture-v1".into(),
        allowed_returned_models: vec!["served-model".into()],
    };
    assert!(
        qstore
            .qualification_active(
                &qscope,
                "reviewed-set-1",
                &qgraph.content_hash_sha256,
                &identity,
                "fixture",
                now_ms
            )
            .await
            .unwrap()
    );
    assert!(
        !qstore
            .qualification_active(
                &qscope,
                "reviewed-set-1",
                &qgraph.content_hash_sha256,
                &identity,
                "served-model",
                now_ms
            )
            .await
            .unwrap()
    );
    let after = qengine
        .decide(qscope.clone(), qrequest("after"))
        .await
        .unwrap();
    assert_eq!(after.disposition, Disposition::Accept);
    assert!(after.prediction_cache_hit);
    assert_eq!(after.attempts, 0);

    // Test PostgresStore::get_receipt
    let retrieved = qstore.get_receipt(&qscope, "after").await.unwrap();
    assert!(retrieved.is_some());
    assert_eq!(retrieved.unwrap().receipt_id, after.receipt_id);
    let other_product = kernel_core::Scope {
        product_id: "other-product".into(),
        ..qscope.clone()
    };
    assert!(
        qstore
            .get_receipt(&other_product, "after")
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        qstore
            .record_review(&other_product, "after", "reviewer", "accept", None)
            .await
            .is_err()
    );
    assert!(
        qstore
            .record_outcome(&other_product, "after", "success", true, Some("proof"))
            .await
            .is_err()
    );

    // Test PostgresStore::store_evidence
    let ev_val = serde_json::json!({"customer_tier": "gold"});
    let ev_digest = format!(
        "{:x}",
        sha2::Sha256::digest(serde_json::to_vec(&ev_val).unwrap())
    );
    let stored = qstore
        .store_evidence(
            &qscope, "ev_tier", 1, now_ms, None, &ev_digest, "internal", &ev_val,
        )
        .await
        .unwrap();
    assert_eq!(stored, kernel_runtime::postgres::EvidenceWrite::Created);
    assert_eq!(
        qstore
            .store_evidence(
                &qscope, "ev_tier", 1, now_ms, None, &ev_digest, "internal", &ev_val,
            )
            .await
            .unwrap(),
        kernel_runtime::postgres::EvidenceWrite::Existing
    );
    assert_eq!(
        qstore
            .store_evidence(
                &qscope,
                "ev_tier",
                1,
                now_ms,
                None,
                &ev_digest,
                "restricted",
                &ev_val,
            )
            .await
            .unwrap(),
        kernel_runtime::postgres::EvidenceWrite::Conflict
    );
    let loaded = qstore
        .load_evidence(&qscope, &[("ev_tier".into(), 1)])
        .await
        .unwrap();
    assert_eq!(loaded.len(), 1);
    assert_eq!(loaded[0].digest_sha256, ev_digest);

    // Test PostgresStore::record_review and immutability guard
    qstore
        .record_review(
            &qscope,
            "after",
            "reviewer_alice",
            "accept",
            Some("looks good"),
        )
        .await
        .unwrap();
    let bad_review_update =
        sqlx::query("UPDATE decision_reviews SET disposition='deny' WHERE idempotency_key='after'")
            .execute(&pool)
            .await;
    assert!(
        bad_review_update.is_err(),
        "decision_reviews must be append-only"
    );

    // Test PostgresStore::record_outcome and immutability guard
    qstore
        .record_outcome(
            &qscope,
            "after",
            "resolved_successfully",
            true,
            Some("proof_123"),
        )
        .await
        .unwrap();
    let bad_outcome_update =
        sqlx::query("UPDATE decision_outcomes SET outcome='failed' WHERE idempotency_key='after'")
            .execute(&pool)
            .await;
    assert!(
        bad_outcome_update.is_err(),
        "decision_outcomes must be append-only"
    );
    let event_types: Vec<String> = sqlx::query_scalar("SELECT event_type FROM outbox_events WHERE tenant_id=$1 AND project_id=$2 AND environment=$3 AND payload->>'idempotency_key'='after' ORDER BY event_type")
        .bind(&qscope.tenant_id).bind(&qscope.project_id).bind(&qscope.environment)
        .fetch_all(&pool).await.unwrap();
    assert_eq!(
        event_types,
        vec!["decision.completed", "outcome.observed", "review.recorded"]
    );

    // Test PostgresStore::query_usage
    let usage = qstore.query_usage(&qscope).await.unwrap();
    assert!(!usage.is_empty());
    let qual_usage = usage.iter().find(|u| u.purpose == "qualified").unwrap();
    assert_eq!(qual_usage.cap_nano_usd, 100);

    // Portability test (DK-R11): Two genuinely independent providers normalize to identical canonical contract
    let norm_q = vec![kernel_provider::SemanticQuestion {
        id: "urgency".into(),
        prompt: "urgent?".into(),
        output: OutputDefinition::Binary {
            id: "urgency".into(),
            proposition: "is_urgent".into(),
        },
    }];
    let jev_id = kernel_provider::ProviderIdentity {
        provider_id: "jev-ai".into(),
        model_id: "jev-1.13".into(),
        adapter_version: "system-one-v1".into(),
        allowed_returned_models: vec![],
    };
    let jev_raw = serde_json::to_vec(&serde_json::json!({
        "model": "jev-1.13",
        "predictions": {
            "urgency": {"type": "noul", "value": true}
        }
    }))
    .unwrap();
    let jev_res = kernel_provider::normalize_system_one(&jev_raw, &jev_id, &norm_q).unwrap();

    let _compat_id = kernel_provider::ProviderIdentity {
        provider_id: "openai-compatible".into(),
        model_id: "gpt-4o-mini".into(),
        adapter_version: "compatible-v1".into(),
        allowed_returned_models: vec![],
    };
    let compat_raw = serde_json::to_vec(&serde_json::json!({
        "model": "gpt-4o-mini",
        "choices": [{
            "finish_reason": "stop",
            "message": {
                "content": r#"{"answers":{"urgency":{"kind":"binary","proposition":"is_urgent","value":true,"probability_true":null}}}"#
            }
        }]
    })).unwrap();
    // Using strict json and validating answers match
    let compat_val: serde_json::Value = serde_json::from_slice(&compat_raw).unwrap();
    let compat_answers: std::collections::BTreeMap<String, Answer> = serde_json::from_str(
        compat_val["choices"][0]["message"]["content"]
            .as_str()
            .unwrap(),
    )
    .and_then(|v: serde_json::Value| serde_json::from_value(v["answers"].clone()))
    .unwrap();

    assert_eq!(
        jev_res.answers.get("urgency"),
        compat_answers.get("urgency")
    );

    // A non-owner role sees no rows without transaction-local scope and cannot
    // cross the tenant boundary even when the SQL explicitly requests it.
    let mut rls_tx = pool.begin().await.unwrap();
    sqlx::query("SET LOCAL ROLE kernel_runtime")
        .execute(&mut *rls_tx)
        .await
        .unwrap();
    let no_context: i64 = sqlx::query_scalar("SELECT count(*) FROM evidence")
        .fetch_one(&mut *rls_tx)
        .await
        .unwrap();
    assert_eq!(no_context, 0);
    sqlx::query("SELECT set_config('kernel.tenant_id',$1,true),set_config('kernel.project_id',$2,true),set_config('kernel.environment',$3,true)")
        .bind(&qscope.tenant_id).bind(&qscope.project_id).bind(&qscope.environment)
        .execute(&mut *rls_tx).await.unwrap();
    let own: i64 = sqlx::query_scalar("SELECT count(*) FROM evidence WHERE tenant_id=$1")
        .bind(&qscope.tenant_id)
        .fetch_one(&mut *rls_tx)
        .await
        .unwrap();
    assert!(own > 0);
    let other: i64 = sqlx::query_scalar("SELECT count(*) FROM evidence WHERE tenant_id=$1")
        .bind(&scope.tenant_id)
        .fetch_one(&mut *rls_tx)
        .await
        .unwrap();
    assert_eq!(other, 0);
    rls_tx.rollback().await.unwrap();
    let mut reused_tx = pool.begin().await.unwrap();
    sqlx::query("SET LOCAL ROLE kernel_runtime")
        .execute(&mut *reused_tx)
        .await
        .unwrap();
    let leaked: i64 = sqlx::query_scalar("SELECT count(*) FROM evidence")
        .fetch_one(&mut *reused_tx)
        .await
        .unwrap();
    assert_eq!(leaked, 0);
    reused_tx.rollback().await.unwrap();

    let runtime_pool = PgPoolOptions::new()
        .max_connections(2)
        .after_connect(|conn, _| {
            Box::pin(async move {
                sqlx::query("SET ROLE kernel_runtime").execute(conn).await?;
                Ok(())
            })
        })
        .connect(&url)
        .await
        .unwrap();
    let runtime_store = PostgresStore {
        pool: runtime_pool,
        purpose: "qualified".into(),
    };
    assert_eq!(
        runtime_store
            .load_evidence(&qscope, &[("ev_tier".into(), 1)])
            .await
            .unwrap()
            .len(),
        1
    );
    assert!(
        runtime_store
            .load_evidence(&scope, &[("ev_tier".into(), 1)])
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        runtime_store
            .get_receipt(&qscope, "after")
            .await
            .unwrap()
            .is_some()
    );
    assert!(
        runtime_store
            .get_receipt(&scope, "after")
            .await
            .unwrap()
            .is_none()
    );
}
