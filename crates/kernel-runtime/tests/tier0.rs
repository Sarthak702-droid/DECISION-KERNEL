//! Tier 0 regressions use actual, separate non-owner LOGIN credentials.
use kernel_compiler::{CompiledDecisionGraph, DecisionPack, compile};
use kernel_core::{Answer, DecisionRequest, Disposition, ReasonCode, RequestLimits, Scope};
use kernel_provider::{FixtureProvider, IntelligenceProvider, PricedProvider, SemanticResponse};
use kernel_runtime::{
    Claim, Engine, ReceiptStore, RuntimeError,
    postgres::{PostgresStore, database_ready, recover_stale, scoped_transaction},
};
use serde_json::json;
use sha2::{Digest, Sha256};
use sqlx::{
    PgPool,
    postgres::{PgConnectOptions, PgPoolOptions},
};
use std::{
    collections::BTreeMap,
    str::FromStr,
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tokio::sync::Semaphore;

fn graph() -> CompiledDecisionGraph {
    let pack: DecisionPack = serde_json::from_value(json!({
        "api_version":"kernel/v1", "kind":"DecisionPack", "id":"tier0", "version":"1",
        "required_evidence":[], "semantic_state_fields":["input"],
        "outputs":[{"kind":"binary","id":"ok","proposition":"ok"}],
        "nodes":[
            {"id":"deny","op":"fact_equals","field":"permission","value":false},
            {"id":"question","op":"semantic_question","output":"ok","prompt":"Is this okay?"}
        ],
        "policy":{"hard_deny_nodes":["deny"],"accept_nodes":[],"accept_deterministic":false,"accept_qualified_semantic":false},
        "limits":{"deadline_ms":1000,"max_billable_attempts":1,"max_cost_nano_usd":1000},
        "qualification":{"semantic_acceptance_qualified":false,"provider_id":"fixture","model_id":"fixture","adapter_version":"fixture-v1"}
    })).unwrap();
    compile(pack).unwrap()
}
fn response(charge: u64) -> SemanticResponse {
    SemanticResponse {
        answers: BTreeMap::from([(
            "question".into(),
            Answer::Binary {
                proposition: "ok".into(),
                value: Some(true),
                probability_true: None,
                probability_provenance: None,
            },
        )]),
        provider_id: "fixture".into(),
        model_id: "fixture".into(),
        reported_cost_nano_usd: Some(charge),
        usage: Some(json!({"input_tokens":10,"output_tokens":2})),
    }
}
fn request(key: &str, input: &str) -> DecisionRequest {
    DecisionRequest {
        schema_version: "1.1".into(),
        decision_type: "tier0".into(),
        release: "tier0@1".into(),
        idempotency_key: key.into(),
        state: BTreeMap::from([
            ("permission".into(), json!(true)),
            ("input".into(), json!(input)),
        ]),
        evidence: vec![],
        constraints: RequestLimits::default(),
    }
}
fn make_engine(graph: &CompiledDecisionGraph, store: Arc<PostgresStore>, charge: u64) -> Engine {
    Engine {
        graph: Arc::new(graph.clone()),
        provider: Some(Arc::new(PricedProvider {
            inner: Arc::new(FixtureProvider {
                response: response(charge),
            }),
            max_dispatch_nano_usd: 100,
            max_request_bytes: 65536,
        })),
        store,
        provider_permits: Arc::new(Semaphore::new(1)),
        platform_deadline_ms: 1000,
        platform_max_cost_nano_usd: 1000,
    }
}
async fn age(owner: &PgPool, scope: &Scope, key: &str) {
    sqlx::query("UPDATE decision_receipts SET created_at=now()-interval '2 minutes' WHERE tenant_id=$1 AND product_id=$2 AND idempotency_key=$3")
        .bind(&scope.tenant_id).bind(&scope.product_id).bind(key).execute(owner).await.unwrap();
}

#[tokio::test]
async fn isolated_credentials_cover_scope_deadlines_billing_and_recovery() {
    let Ok(url) = std::env::var("KERNEL_TEST_DATABASE_URL") else {
        assert!(std::env::var_os("CI").is_none(), "database required in CI");
        return;
    };
    let owner = PgPool::connect(&url).await.unwrap();
    let suffix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let api_role = format!("tier0_api_{suffix}");
    let worker_role = format!("tier0_worker_{suffix}");
    for (name, parent) in [
        (&api_role, "kernel_runtime"),
        (&worker_role, "kernel_worker"),
    ] {
        sqlx::raw_sql(&format!("CREATE ROLE {name} LOGIN PASSWORD 'isolated_test_only' NOSUPERUSER NOBYPASSRLS; GRANT {parent} TO {name}"))
            .execute(&owner).await.unwrap();
    }
    let options = PgConnectOptions::from_str(&url).unwrap();
    let api = PgPoolOptions::new()
        .max_connections(4)
        .connect_with(
            options
                .clone()
                .username(&api_role)
                .password("isolated_test_only"),
        )
        .await
        .unwrap();
    let worker = PgPoolOptions::new()
        .max_connections(4)
        .connect_with(
            options
                .username(&worker_role)
                .password("isolated_test_only"),
        )
        .await
        .unwrap();
    assert!(database_ready(&api, false).await);
    assert!(database_ready(&worker, true).await);
    assert!(!database_ready(&owner, false).await);
    assert!(!database_ready(&api, true).await);
    assert!(!database_ready(&worker, false).await);
    let scope = Scope {
        tenant_id: format!("tier0-{suffix}"),
        project_id: "project".into(),
        environment: "test".into(),
        product_id: "a".into(),
    };
    let product_b = Scope {
        product_id: "b".into(),
        ..scope.clone()
    };
    let other_tenant = Scope {
        tenant_id: format!("tier0-other-{suffix}"),
        ..scope.clone()
    };
    let other_env = Scope {
        environment: "other".into(),
        ..scope.clone()
    };
    let graph = graph();
    for s in [&scope, &product_b, &other_tenant, &other_env] {
        sqlx::query("INSERT INTO budget_buckets (tenant_id,project_id,environment,product_id,purpose,cap_nano_usd) VALUES ($1,$2,$3,$4,'tier0',1000)")
            .bind(&s.tenant_id).bind(&s.project_id).bind(&s.environment).bind(&s.product_id).execute(&owner).await.unwrap();
        sqlx::query("INSERT INTO behavior_releases (tenant_id,project_id,environment,decision_type,version,content_hash_sha256,compiled_graph) VALUES ($1,$2,$3,'tier0','1',$4,$5) ON CONFLICT DO NOTHING")
            .bind(&s.tenant_id).bind(&s.project_id).bind(&s.environment).bind(&graph.content_hash_sha256).bind(serde_json::to_value(&graph).unwrap()).execute(&owner).await.unwrap();
    }
    let store = Arc::new(PostgresStore {
        pool: api.clone(),
        purpose: "tier0".into(),
    });
    let engine = make_engine(&graph, store.clone(), 80);
    let (one, two) = tokio::join!(
        engine.decide(scope.clone(), request("same", "A")),
        engine.decide(scope.clone(), request("same", "A"))
    );
    let one = one.unwrap();
    let two = two.unwrap();
    assert_eq!(one.receipt_id, two.receipt_id);
    assert_eq!(one.attempts, 1);
    assert_eq!(one.provider_reported_cost_nano_usd, Some(80));
    assert_eq!(
        one.provider_usage,
        Some(json!({"input_tokens":10,"output_tokens":2}))
    );
    assert!(matches!(
        engine
            .decide(scope.clone(), request("same", "changed"))
            .await,
        Err(RuntimeError::Conflict)
    ));
    let cached = engine
        .decide(scope.clone(), request("cached", "A"))
        .await
        .unwrap();
    assert!(cached.prediction_cache_hit);
    assert_eq!(cached.cost_nano_usd, Some(0));
    for s in [&product_b, &other_tenant, &other_env] {
        assert!(store.get_receipt(s, "same").await.unwrap().is_none());
        let isolated = engine
            .decide(s.clone(), request("same", "A"))
            .await
            .unwrap();
        assert_eq!(&isolated.scope, s);
        assert_eq!(isolated.attempts, 1);
        assert!(!isolated.prediction_cache_hit);
        assert_ne!(isolated.receipt_id, one.receipt_id);
    }
    // Direct RLS access has a product boundary, even without application WHEREs.
    let mut tx = scoped_transaction(&api, &scope).await.unwrap();
    let foreign: i64 =
        sqlx::query_scalar("SELECT count(*) FROM decision_receipts WHERE product_id='b'")
            .fetch_one(&mut *tx)
            .await
            .unwrap();
    assert_eq!(foreign, 0);
    tx.rollback().await.unwrap();
    assert!(
        sqlx::query("SELECT * FROM decision_receipts")
            .fetch_all(&api)
            .await
            .unwrap()
            .is_empty()
    );
    // Evidence IDs/revisions may be identical in independent products.
    let value = json!({"source":"A"});
    let digest = format!("{:x}", Sha256::digest(serde_json::to_vec(&value).unwrap()));
    store
        .store_evidence(&scope, "ev", 1, 1, None, &digest, "internal", &value)
        .await
        .unwrap();
    let missing = store
        .load_evidence(&product_b, &[("ev".into(), 1)])
        .await
        .unwrap();
    assert_eq!(missing.len(), 1);
    assert_eq!(missing[0].expires_at_unix_ms, Some(0));
    let mut unresolved = request("missing", "A");
    unresolved.evidence = missing;
    let review = engine
        .decide(product_b.clone(), unresolved.clone())
        .await
        .unwrap();
    assert_eq!(review.reason_codes, vec![ReasonCode::MissingEvidence]);
    assert_eq!(review.evidence_revisions["ev"], 1);
    unresolved.evidence[0].id = "other-missing".into();
    assert!(matches!(
        engine.decide(product_b.clone(), unresolved).await,
        Err(RuntimeError::Conflict)
    ));
    let b_value = json!({"source":"B"});
    let b_digest = format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&b_value).unwrap())
    );
    store
        .store_evidence(
            &product_b, "ev", 1, 1, None, &b_digest, "internal", &b_value,
        )
        .await
        .unwrap();
    let owner_store = PostgresStore {
        pool: owner.clone(),
        purpose: "tier0".into(),
    };
    let a_ev = owner_store
        .load_evidence(&scope, &[("ev".into(), 1)])
        .await
        .unwrap();
    let b_ev = owner_store
        .load_evidence(&product_b, &[("ev".into(), 1)])
        .await
        .unwrap();
    assert_eq!(a_ev.len(), 1);
    assert_eq!(a_ev[0].value, value);
    assert_eq!(b_ev.len(), 1);
    assert_eq!(b_ev[0].value, b_value);
    for permission in [None, Some(json!(null)), Some(json!("unknown"))] {
        let mut req = request(&format!("permission-{permission:?}"), "A");
        req.state.remove("permission");
        if let Some(value) = permission {
            req.state.insert("permission".into(), value);
        }
        let r = engine.decide(scope.clone(), req).await.unwrap();
        assert_eq!(r.disposition, Disposition::Review);
        assert_eq!(r.reason_codes, vec![ReasonCode::MissingPermission]);
        assert_eq!(r.attempts, 0);
    }
    let mut deny = request("deny", "A");
    deny.state.insert("permission".into(), json!(false));
    assert_eq!(
        engine
            .decide(scope.clone(), deny)
            .await
            .unwrap()
            .disposition,
        Disposition::Deny
    );
    // Unknown pricing and insufficient cap both prevent dispatch.
    let unpriced = Engine {
        provider: Some(Arc::new(Unpriced)),
        ..make_engine(&graph, store.clone(), 80)
    };
    assert_eq!(
        unpriced
            .decide(scope.clone(), request("unpriced", "fresh"))
            .await
            .unwrap()
            .attempts,
        0
    );
    let mut cheap = request("cheap", "fresh");
    cheap.constraints.max_cost_nano_usd = Some(99);
    assert_eq!(
        engine.decide(scope.clone(), cheap).await.unwrap().attempts,
        0
    );
    // Reconciliation is exactly once, records overspend, and preserves invalid-output billing.
    store
        .reconcile(&scope, "same", 80, "invoice-one")
        .await
        .unwrap();
    store
        .reconcile(&scope, "same", 80, "invoice-one")
        .await
        .unwrap();
    assert!(matches!(
        store.reconcile(&scope, "same", 81, "different").await,
        Err(RuntimeError::Conflict)
    ));
    let over = make_engine(&graph, store.clone(), 150)
        .decide(scope.clone(), request("overspend", "new"))
        .await
        .unwrap();
    assert_eq!(over.reason_codes, vec![ReasonCode::InvalidProviderOutput]);
    assert_eq!(over.provider_reported_cost_nano_usd, Some(150));
    store
        .reconcile(&scope, "overspend", 150, "invoice-over")
        .await
        .unwrap();
    assert_eq!(
        store
            .get_receipt(&scope, "overspend")
            .await
            .unwrap()
            .unwrap()
            .cost_nano_usd,
        Some(150)
    );
    let overspend: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM outbox_events WHERE tenant_id=$1 AND event_type='provider.overspend'",
    )
    .bind(&scope.tenant_id)
    .fetch_one(&owner)
    .await
    .unwrap();
    assert_eq!(overspend, 1);
    // Claim DB wait is inside the request deadline.
    let mut lock = owner.begin().await.unwrap();
    sqlx::query("LOCK TABLE decision_receipts IN ACCESS EXCLUSIVE MODE")
        .execute(&mut *lock)
        .await
        .unwrap();
    let mut bounded = request("blocked-claim", "A");
    bounded.constraints.deadline_ms = Some(50);
    let start = Instant::now();
    assert!(matches!(
        engine.decide(scope.clone(), bounded).await,
        Err(RuntimeError::Deadline)
    ));
    assert!(start.elapsed() < Duration::from_millis(500));
    lock.rollback().await.unwrap();
    // A paid call finishes but commit is blocked; the durable dispatch intent is
    // recovered to one terminal review. No second paid call is permitted.
    let mut lock = owner.begin().await.unwrap();
    sqlx::query("SELECT * FROM behavior_releases WHERE tenant_id=$1 FOR UPDATE")
        .bind(&scope.tenant_id)
        .fetch_all(&mut *lock)
        .await
        .unwrap();
    let mut paid = request("paid-crash", "unique");
    paid.constraints.deadline_ms = Some(100);
    let start = Instant::now();
    assert!(matches!(
        engine.decide(scope.clone(), paid.clone()).await,
        Err(RuntimeError::Deadline)
    ));
    assert!(start.elapsed() < Duration::from_millis(500));
    lock.rollback().await.unwrap();
    age(&owner, &scope, "paid-crash").await;
    recover_stale(&worker).await.unwrap();
    recover_stale(&worker).await.unwrap();
    let recovered = engine.decide(scope.clone(), paid).await.unwrap();
    assert_eq!(recovered.reason_codes, vec![ReasonCode::CrashRecovery]);
    assert_eq!(recovered.disposition, Disposition::Review);
    assert_eq!(recovered.attempts, 1);
    assert_eq!(recovered.cost_nano_usd, None);
    store
        .reconcile(&scope, "paid-crash", 125, "invoice-crash")
        .await
        .unwrap();
    let n:i64=sqlx::query_scalar("SELECT count(*) FROM provider_attempts WHERE tenant_id=$1 AND product_id='a' AND idempotency_key='paid-crash'").bind(&scope.tenant_id).fetch_one(&owner).await.unwrap();
    assert_eq!(n, 1);
    // Undispatched crashes are retryable, with a fencing generation.
    assert!(matches!(
        store.claim(&scope, "unpaid-crash", "digest").await.unwrap(),
        Claim::New(1)
    ));
    age(&owner, &scope, "unpaid-crash").await;
    recover_stale(&worker).await.unwrap();
    assert!(matches!(
        store.claim(&scope, "unpaid-crash", "digest").await.unwrap(),
        Claim::New(2)
    ));
    assert!(
        store
            .reserve(&scope, "unpaid-crash", 1, 100, "fixture", "fixture")
            .await
            .is_err()
    );
    let usage = store.query_usage(&scope).await.unwrap();
    assert_eq!(usage[0].spent_nano_usd, 355);
    assert_eq!(usage[0].held_nano_usd, 0);
    api.close().await;
    worker.close().await;
    sqlx::raw_sql(&format!("DROP ROLE {api_role}; DROP ROLE {worker_role}"))
        .execute(&owner)
        .await
        .unwrap();
}

struct Unpriced;
impl IntelligenceProvider for Unpriced {
    fn identity(&self) -> kernel_provider::ProviderIdentity {
        FixtureProvider {
            response: response(80),
        }
        .identity()
    }
    fn capabilities(&self) -> kernel_provider::ProviderCapabilities {
        FixtureProvider {
            response: response(80),
        }
        .capabilities()
    }
    fn evaluate<'a>(
        &'a self,
        _: kernel_provider::SemanticRequest,
        _: Instant,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<
                    Output = Result<SemanticResponse, kernel_provider::ProviderError>,
                > + Send
                + 'a,
        >,
    > {
        Box::pin(async { panic!("unpriced provider must not be called") })
    }
}
