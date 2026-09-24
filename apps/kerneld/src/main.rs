use axum::{
    Json, Router,
    body::Bytes,
    extract::{DefaultBodyLimit, State},
    http::{HeaderMap, StatusCode},
    routing::{get, post},
};
use kernel_compiler::{CompiledDecisionGraph, compile};
use kernel_core::{DecisionRequest, RequestLimits, Scope};
use kernel_provider::{IntelligenceProvider, OpenAiCompatibleProvider};
use kernel_runtime::{Engine, ReceiptStore, postgres::PostgresStore};
use serde_json::Value;
use sha2::{Digest, Sha256};
use sqlx::{PgPool, Row};
use std::{
    collections::{BTreeMap, BTreeSet},
    env,
    net::SocketAddr,
    sync::Arc,
};
use tokio::sync::Semaphore;

#[derive(Clone)]
struct AppState {
    engine: Arc<Engine>,
    store: Arc<PostgresStore>,
    static_auth: Option<([u8; 32], Scope)>,
    request_permits: Arc<Semaphore>,
}
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct EvidencePointer {
    id: String,
    revision: u64,
}
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct DecisionBody {
    schema_version: String,
    product_id: String,
    decision_type: String,
    release: String,
    idempotency_key: String,
    mode: String,
    execution_profile: String,
    state: BTreeMap<String, Value>,
    evidence_refs: Vec<EvidencePointer>,
    constraints: RequestLimits,
}
fn required(name: &str) -> String {
    env::var(name).unwrap_or_else(|_| {
        eprintln!("missing {name}");
        std::process::exit(2)
    })
}
async fn authenticated_scope(headers: &HeaderMap, state: &AppState) -> Result<Scope, StatusCode> {
    let Some(value) = headers
        .get("authorization")
        .and_then(|h| h.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "))
    else {
        return Err(StatusCode::UNAUTHORIZED);
    };
    let got: [u8; 32] = Sha256::digest(value.as_bytes()).into();
    if let Some((expected, scope)) = &state.static_auth {
        let mut diff = 0u8;
        for i in 0..32 {
            diff |= got[i] ^ expected[i];
        }
        if diff == 0 {
            return Ok(scope.clone());
        }
    }
    let hash = format!("{:x}", Sha256::digest(value.as_bytes()));
    let row=sqlx::query("SELECT tenant_id,project_id,environment,product_id FROM service_tokens WHERE token_hash_sha256=$1 AND active")
        .bind(hash).fetch_optional(&state.store.pool).await.map_err(|_|StatusCode::SERVICE_UNAVAILABLE)?;
    match row {
        Some(row) => Ok(Scope {
            tenant_id: row.get("tenant_id"),
            project_id: row.get("project_id"),
            environment: row.get("environment"),
            product_id: row.get("product_id"),
        }),
        None => Err(StatusCode::UNAUTHORIZED),
    }
}
async fn health() -> StatusCode {
    StatusCode::OK
}
async fn ready(State(state): State<AppState>) -> StatusCode {
    if sqlx::query("SELECT 1")
        .fetch_one(&state.store.pool)
        .await
        .is_ok()
    {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    }
}
async fn decide(
    State(state): State<AppState>,
    headers: HeaderMap,
    raw: Bytes,
) -> Result<Json<kernel_core::DecisionReceipt>, StatusCode> {
    let scope = authenticated_scope(&headers, &state).await?;
    let body: DecisionBody = serde_json::from_value(
        kernel_core::strict_json(&raw).map_err(|_| StatusCode::BAD_REQUEST)?,
    )
    .map_err(|_| StatusCode::BAD_REQUEST)?;
    if body.product_id != scope.product_id
        || body.mode != "evaluate"
        || body.execution_profile != "fast"
        || headers.get("idempotency-key").and_then(|v| v.to_str().ok())
            != Some(body.idempotency_key.as_str())
    {
        return Err(StatusCode::BAD_REQUEST);
    }
    if body.evidence_refs.len() > 32 {
        return Err(StatusCode::BAD_REQUEST);
    }
    if body
        .evidence_refs
        .iter()
        .any(|r| r.id.is_empty() || r.id.len() > 128)
        || body
            .evidence_refs
            .iter()
            .map(|r| &r.id)
            .collect::<BTreeSet<_>>()
            .len()
            != body.evidence_refs.len()
    {
        return Err(StatusCode::BAD_REQUEST);
    }
    let graph = &state.engine.graph;
    if body.decision_type != graph.pack.id
        || body.release != format!("{}@{}", graph.pack.id, graph.pack.version)
    {
        return Err(StatusCode::NOT_FOUND);
    }
    let row = sqlx::query("SELECT content_hash_sha256, revoked_at IS NOT NULL AS revoked FROM behavior_releases WHERE tenant_id=$1 AND project_id=$2 AND environment=$3 AND decision_type=$4 AND version=$5")
        .bind(&scope.tenant_id).bind(&scope.project_id).bind(&scope.environment).bind(&graph.pack.id).bind(&graph.pack.version)
        .fetch_optional(&state.store.pool).await.map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    let Some(row) = row else {
        return Err(StatusCode::NOT_FOUND);
    };
    if row.get::<String, _>("content_hash_sha256") != graph.content_hash_sha256
        || row.get::<bool, _>("revoked")
    {
        return Err(StatusCode::FORBIDDEN);
    }
    let refs: Vec<_> = body
        .evidence_refs
        .into_iter()
        .map(|r| (r.id, r.revision))
        .collect();
    let evidence = state
        .store
        .load_evidence(&scope, &refs)
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    let request = DecisionRequest {
        schema_version: body.schema_version,
        decision_type: body.decision_type,
        release: body.release,
        idempotency_key: body.idempotency_key,
        state: body.state,
        evidence,
        constraints: body.constraints,
    };
    let permit = state
        .request_permits
        .clone()
        .try_acquire_owned()
        .map_err(|_| StatusCode::TOO_MANY_REQUESTS)?;
    let engine = state.engine.clone();
    // The bounded task owns the attempt after admission. Client cancellation cannot drop it.
    let task = tokio::spawn(async move {
        let _permit = permit;
        engine.decide(scope, request).await
    });
    match task.await.map_err(|_| StatusCode::SERVICE_UNAVAILABLE)? {
        Ok(receipt) => Ok(Json(receipt)),
        Err(kernel_runtime::RuntimeError::Conflict) => Err(StatusCode::CONFLICT),
        Err(kernel_runtime::RuntimeError::InvalidRequest) => Err(StatusCode::BAD_REQUEST),
        Err(kernel_runtime::RuntimeError::Release) => Err(StatusCode::NOT_FOUND),
        Err(kernel_runtime::RuntimeError::Store) => Err(StatusCode::SERVICE_UNAVAILABLE),
    }
}

async fn get_decision(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Json<kernel_core::DecisionReceipt>, StatusCode> {
    let scope = authenticated_scope(&headers, &state).await?;
    let receipt = state
        .store
        .get_receipt(&scope, &id)
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    match receipt {
        Some(r) => Ok(Json(r)),
        None => Err(StatusCode::NOT_FOUND),
    }
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct IngestEvidenceBody {
    evidence_id: String,
    revision: u64,
    observed_at_unix_ms: u64,
    expires_at_unix_ms: Option<u64>,
    digest_sha256: String,
    sensitivity: String,
    value: Value,
}

async fn ingest_evidence(
    State(state): State<AppState>,
    headers: HeaderMap,
    raw: Bytes,
) -> Result<StatusCode, StatusCode> {
    let scope = authenticated_scope(&headers, &state).await?;
    let body: IngestEvidenceBody = serde_json::from_value(
        kernel_core::strict_json(&raw).map_err(|_| StatusCode::BAD_REQUEST)?,
    )
    .map_err(|_| StatusCode::BAD_REQUEST)?;

    if body.evidence_id.is_empty() || body.evidence_id.len() > 128 || body.digest_sha256.len() != 64 {
        return Err(StatusCode::BAD_REQUEST);
    }
    let actual_digest = format!(
        "{:x}",
        Sha256::digest(
            serde_json::to_vec(&body.value).map_err(|_| StatusCode::BAD_REQUEST)?
        )
    );
    if actual_digest != body.digest_sha256 {
        return Err(StatusCode::BAD_REQUEST);
    }
    state
        .store
        .store_evidence(
            &scope,
            &body.evidence_id,
            body.revision,
            body.observed_at_unix_ms,
            body.expires_at_unix_ms,
            &body.digest_sha256,
            &body.sensitivity,
            &body.value,
        )
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    Ok(StatusCode::CREATED)
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ReviewBody {
    reviewer_id: String,
    disposition: String,
    notes: Option<String>,
}

async fn post_review(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::extract::Path(id): axum::extract::Path<String>,
    raw: Bytes,
) -> Result<StatusCode, StatusCode> {
    let scope = authenticated_scope(&headers, &state).await?;
    let body: ReviewBody = serde_json::from_value(
        kernel_core::strict_json(&raw).map_err(|_| StatusCode::BAD_REQUEST)?,
    )
    .map_err(|_| StatusCode::BAD_REQUEST)?;

    if body.reviewer_id.is_empty()
        || body.reviewer_id.len() > 128
        || !matches!(body.disposition.as_str(), "accept" | "review" | "deny" | "abstain")
    {
        return Err(StatusCode::BAD_REQUEST);
    }
    state
        .store
        .record_review(
            &scope,
            &id,
            &body.reviewer_id,
            &body.disposition,
            body.notes.as_deref(),
        )
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    Ok(StatusCode::CREATED)
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct OutcomeBody {
    outcome: String,
    verified: Option<bool>,
    verification_ref: Option<String>,
}

async fn post_outcome(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::extract::Path(id): axum::extract::Path<String>,
    raw: Bytes,
) -> Result<StatusCode, StatusCode> {
    let scope = authenticated_scope(&headers, &state).await?;
    let body: OutcomeBody = serde_json::from_value(
        kernel_core::strict_json(&raw).map_err(|_| StatusCode::BAD_REQUEST)?,
    )
    .map_err(|_| StatusCode::BAD_REQUEST)?;

    if body.outcome.is_empty() || body.outcome.len() > 256 {
        return Err(StatusCode::BAD_REQUEST);
    }
    state
        .store
        .record_outcome(
            &scope,
            &id,
            &body.outcome,
            body.verified.unwrap_or(false),
            body.verification_ref.as_deref(),
        )
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    Ok(StatusCode::CREATED)
}

async fn get_usage(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Vec<kernel_runtime::postgres::UsageSummary>>, StatusCode> {
    let scope = authenticated_scope(&headers, &state).await?;
    let summaries = state
        .store
        .query_usage(&scope)
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    Ok(Json(summaries))
}

#[tokio::main]
async fn main() {
    if let Err(e) = run().await {
        eprintln!("kerneld startup failed: {e}");
        std::process::exit(1);
    }
}
async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let database_url = required("DATABASE_URL");
    let compiled_path = required("KERNEL_COMPILED_RELEASE");
    let static_auth = match env::var("KERNEL_SERVICE_TOKEN") {
        Ok(token) => {
            if token.len() < 32 {
                return Err("service token must contain at least 32 characters".into());
            }
            let scope = Scope {
                tenant_id: required("KERNEL_TENANT_ID"),
                project_id: required("KERNEL_PROJECT_ID"),
                environment: required("KERNEL_ENVIRONMENT"),
                product_id: required("KERNEL_PRODUCT_ID"),
            };
            Some((Sha256::digest(token.as_bytes()).into(), scope))
        }
        Err(_) => None,
    };
    let graph: CompiledDecisionGraph =
        serde_json::from_slice(&tokio::fs::read(&compiled_path).await?)?;
    let rebuilt = compile(graph.pack.clone())?;
    if rebuilt.content_hash_sha256 != graph.content_hash_sha256
        || rebuilt.execution_order != graph.execution_order
        || rebuilt.dependencies != graph.dependencies
        || rebuilt.hard_deny_indices != graph.hard_deny_indices
        || rebuilt.accept_indices != graph.accept_indices
        || rebuilt.semantic_batches != graph.semantic_batches
    {
        return Err("compiled release hash mismatch".into());
    }
    let pool = PgPool::connect(&database_url).await?;
    let store = Arc::new(PostgresStore {
        pool,
        purpose: graph.pack.id.clone(),
    });
    let provider: Option<Arc<dyn IntelligenceProvider>> = match env::var("KERNEL_PROVIDER_BASE_URL")
    {
        Ok(base) => {
            let adapter = env::var("KERNEL_PROVIDER_ADAPTER").unwrap_or_else(|_| "compatible".into());
            let allowed_host = required("KERNEL_PROVIDER_ALLOWED_HOST");
            let api_key = required("KERNEL_PROVIDER_API_KEY");
            let provider_id = required("KERNEL_PROVIDER_ID");
            let model_id = required("KERNEL_PROVIDER_MODEL");
            if adapter == "system-one" {
                Some(Arc::new(kernel_provider::SystemOneProvider::new(
                    &base,
                    &allowed_host,
                    api_key,
                    provider_id,
                    model_id,
                    512,
                )?))
            } else {
                Some(Arc::new(OpenAiCompatibleProvider::new(
                    &base,
                    &allowed_host,
                    api_key,
                    provider_id,
                    model_id,
                    512,
                )?))
            }
        }
        Err(_) => None,
    };
    let engine = Arc::new(Engine {
        graph: Arc::new(graph),
        provider,
        store: store.clone() as Arc<dyn ReceiptStore>,
        provider_permits: Arc::new(Semaphore::new(16)),
        platform_deadline_ms: 3000,
        platform_max_cost_nano_usd: 1_000_000,
    });
    let state = AppState {
        engine,
        store,
        static_auth,
        request_permits: Arc::new(Semaphore::new(256)),
    };
    let app = Router::new()
        .route("/healthz", get(health))
        .route("/readyz", get(ready))
        .route("/health/live", get(health))
        .route("/health/ready", get(ready))
        .route("/v1/decisions", post(decide))
        .route("/v1/decisions/:id", get(get_decision))
        .route("/v1/decisions/:id/reviews", post(post_review))
        .route("/v1/decisions/:id/outcomes", post(post_outcome))
        .route("/v1/evidence", post(ingest_evidence))
        .route("/v1/usage", get(get_usage))
        .layer(DefaultBodyLimit::max(64 * 1024))
        .with_state(state);
    let addr: SocketAddr = env::var("KERNEL_LISTEN_ADDR")
        .unwrap_or_else(|_| "127.0.0.1:8080".into())
        .parse()?;
    axum::serve(tokio::net::TcpListener::bind(addr).await?, app).await?;
    Ok(())
}
