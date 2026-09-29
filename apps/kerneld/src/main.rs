use axum::{
    Json, Router,
    body::{Body, Bytes},
    extract::{DefaultBodyLimit, State},
    http::{HeaderMap, HeaderValue, Request, StatusCode},
    middleware::{Next, from_fn},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use kernel_compiler::{CompiledDecisionGraph, compile};
use kernel_core::{DecisionRequest, QueuedDecision, RequestLimits, Scope};
use kernel_provider::{IntelligenceProvider, OpenAiCompatibleProvider};
use kernel_runtime::{
    Engine, ReceiptStore,
    postgres::{EvidenceWrite, PostgresStore, scoped_transaction},
};
use serde_json::Value;
use sha2::{Digest, Sha256};
use sqlx::{PgPool, Row};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    env,
    net::SocketAddr,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};
use tokio::sync::{RwLock, Semaphore};
static REQUEST_SEQUENCE: AtomicU64 = AtomicU64::new(0);

fn safe_request_id(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .filter(|v| {
            !v.is_empty()
                && v.len() <= 128
                && v.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
        })
        .map(str::to_owned)
}

async fn error_contract(request: Request<Body>, next: Next) -> Response {
    let path = request.uri().path().to_owned();
    let request_id = safe_request_id(request.headers(), "x-request-id").unwrap_or_else(|| {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        format!(
            "dk-{now:x}-{:x}",
            REQUEST_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        )
    });
    let correlation_id = safe_request_id(request.headers(), "x-correlation-id")
        .unwrap_or_else(|| request_id.clone());
    let mut response = next.run(request).await;
    let status = response.status();
    if status.is_client_error() || status.is_server_error() {
        let (code, message, retryable) = match (status, path.as_str()) {
            (StatusCode::BAD_REQUEST, "/v1/evidence") => {
                ("INVALID_EVIDENCE", "The evidence is invalid", false)
            }
            (StatusCode::CONFLICT, "/v1/evidence") => (
                "EVIDENCE_CONFLICT",
                "The evidence revision already differs",
                false,
            ),
            _ => match status {
                StatusCode::BAD_REQUEST => ("INVALID_REQUEST", "The request is invalid", false),
                StatusCode::UNAUTHORIZED => ("UNAUTHORIZED", "Authentication is required", false),
                StatusCode::FORBIDDEN => (
                    "TENANT_SCOPE_VIOLATION",
                    "The operation is not permitted",
                    false,
                ),
                StatusCode::NOT_FOUND => ("NOT_FOUND", "The resource was not found", false),
                StatusCode::CONFLICT => (
                    "IDEMPOTENCY_CONFLICT",
                    "The request conflicts with existing state",
                    false,
                ),
                StatusCode::PRECONDITION_FAILED => (
                    "RELEASE_NOT_ACTIVE",
                    "The requested release is not active",
                    false,
                ),
                StatusCode::GONE => ("RELEASE_REVOKED", "The release is revoked", false),
                StatusCode::TOO_MANY_REQUESTS => (
                    "ADMISSION_FULL",
                    "Request capacity is temporarily full",
                    true,
                ),
                StatusCode::SERVICE_UNAVAILABLE => (
                    "SERVICE_UNAVAILABLE",
                    "A required service is unavailable",
                    true,
                ),
                _ => (
                    "REQUEST_FAILED",
                    "The request could not be completed",
                    status.is_server_error(),
                ),
            },
        };
        response = (
            status,
            Json(serde_json::json!({
                "error": {"code": code, "message": message, "retryable": retryable,
                          "request_id": request_id, "correlation_id": correlation_id}
            })),
        )
            .into_response();
    }
    response.headers_mut().insert(
        "x-request-id",
        HeaderValue::from_str(&request_id).expect("validated request ID"),
    );
    response.headers_mut().insert(
        "x-correlation-id",
        HeaderValue::from_str(&correlation_id).expect("validated correlation ID"),
    );
    response
}

#[derive(Clone)]
struct AppState {
    release_cache: Arc<RwLock<HashMap<String, Arc<CompiledDecisionGraph>>>>,
    provider: Option<Arc<dyn IntelligenceProvider>>,
    provider_permits: Arc<Semaphore>,
    store: Arc<PostgresStore>,
    static_auth: Option<([u8; 32], Scope)>,
    request_permits: Arc<Semaphore>,
}
async fn resolve_release(
    state: &AppState,
    scope: &Scope,
    decision_type: &str,
    requested_release: &str,
) -> Result<Arc<CompiledDecisionGraph>, StatusCode> {
    let mut tx = scoped_transaction(&state.store.pool, scope)
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    let row = sqlx::query("SELECT p.version,p.content_hash_sha256,r.compiled_graph,r.revoked_at IS NOT NULL AS revoked FROM release_pointers p JOIN behavior_releases r ON r.tenant_id=p.tenant_id AND r.project_id=p.project_id AND r.environment=p.environment AND r.decision_type=p.decision_type AND r.version=p.version AND r.content_hash_sha256=p.content_hash_sha256 WHERE p.tenant_id=$1 AND p.project_id=$2 AND p.environment=$3 AND p.decision_type=$4")
        .bind(&scope.tenant_id).bind(&scope.project_id).bind(&scope.environment).bind(decision_type)
        .fetch_optional(&mut *tx).await.map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?
        .ok_or(StatusCode::NOT_FOUND)?;
    if row.get::<bool, _>("revoked") {
        return Err(StatusCode::GONE);
    }
    let version: String = row.get("version");
    if requested_release != "active" && requested_release != format!("{decision_type}@{version}") {
        return Err(StatusCode::PRECONDITION_FAILED);
    }
    let digest: String = row.get("content_hash_sha256");
    if let Some(graph) = state.release_cache.read().await.get(&digest).cloned() {
        return Ok(graph);
    }
    let raw: Value = row.get("compiled_graph");
    let graph: CompiledDecisionGraph =
        serde_json::from_value(raw.clone()).map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    let rebuilt = compile(graph.pack.clone()).map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    if digest != graph.content_hash_sha256
        || graph.pack.id != decision_type
        || graph.pack.version != version
        || serde_json::to_value(&rebuilt).map_err(|_| StatusCode::SERVICE_UNAVAILABLE)? != raw
    {
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    }
    let graph = Arc::new(graph);
    let mut cache = state.release_cache.write().await;
    Ok(cache.entry(digest).or_insert_with(|| graph.clone()).clone())
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
    let row = sqlx::query(
        "SELECT tenant_id,project_id,environment,product_id FROM kernel_authenticate_token($1)",
    )
    .bind(hash)
    .fetch_optional(&state.store.pool)
    .await
    .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
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
    let graph = resolve_release(&state, &scope, &body.decision_type, &body.release).await?;
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
        release: format!("{}@{}", graph.pack.id, graph.pack.version),
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
    let store = Arc::new(PostgresStore {
        pool: state.store.pool.clone(),
        purpose: graph.pack.id.clone(),
    });
    let engine = Engine {
        graph,
        provider: state.provider.clone(),
        store: store as Arc<dyn ReceiptStore>,
        provider_permits: state.provider_permits.clone(),
        platform_deadline_ms: 3000,
        platform_max_cost_nano_usd: 1_000_000,
    };
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

async fn create_job(
    State(state): State<AppState>,
    headers: HeaderMap,
    raw: Bytes,
) -> Result<(StatusCode, Json<Value>), StatusCode> {
    let scope = authenticated_scope(&headers, &state).await?;
    let body: DecisionBody = serde_json::from_value(
        kernel_core::strict_json(&raw).map_err(|_| StatusCode::BAD_REQUEST)?,
    )
    .map_err(|_| StatusCode::BAD_REQUEST)?;
    if body.product_id != scope.product_id
        || body.mode != "evaluate"
        || body.execution_profile != "deferred"
        || body.evidence_refs.len() > 32
        || headers
            .get("idempotency-key")
            .and_then(|value| value.to_str().ok())
            != Some(body.idempotency_key.as_str())
        || body.idempotency_key.is_empty()
        || body.idempotency_key.len() > 128
        || body
            .evidence_refs
            .iter()
            .any(|reference| reference.id.is_empty() || reference.id.len() > 128)
        || body
            .evidence_refs
            .iter()
            .map(|reference| &reference.id)
            .collect::<BTreeSet<_>>()
            .len()
            != body.evidence_refs.len()
    {
        return Err(StatusCode::BAD_REQUEST);
    }
    let graph = resolve_release(&state, &scope, &body.decision_type, &body.release).await?;
    let queued = QueuedDecision {
        scope: scope.clone(),
        schema_version: body.schema_version,
        decision_type: body.decision_type,
        release: format!("{}@{}", graph.pack.id, graph.pack.version),
        release_digest_sha256: graph.content_hash_sha256.clone(),
        idempotency_key: body.idempotency_key,
        state: body.state,
        evidence_refs: body
            .evidence_refs
            .into_iter()
            .map(|reference| (reference.id, reference.revision))
            .collect(),
        constraints: body.constraints,
    };
    let payload = serde_json::to_value(&queued).map_err(|_| StatusCode::BAD_REQUEST)?;
    let digest = format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&payload).map_err(|_| StatusCode::BAD_REQUEST)?)
    );
    let job_id = format!(
        "job_{:x}",
        Sha256::digest(format!(
            "{}:{}:{}:{}:{}",
            scope.tenant_id,
            scope.project_id,
            scope.environment,
            scope.product_id,
            queued.idempotency_key
        ))
    );
    let mut tx = scoped_transaction(&state.store.pool, &scope)
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    let inserted = sqlx::query("INSERT INTO decision_jobs (job_id,tenant_id,project_id,environment,product_id,decision_type,idempotency_key,request_digest_sha256,request_payload) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9) ON CONFLICT DO NOTHING")
        .bind(&job_id).bind(&scope.tenant_id).bind(&scope.project_id).bind(&scope.environment)
        .bind(&scope.product_id).bind(&queued.decision_type).bind(&queued.idempotency_key)
        .bind(&digest).bind(payload).execute(&mut *tx).await.map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    let mut status = "queued".to_owned();
    if inserted.rows_affected() == 0 {
        let prior = sqlx::query("SELECT request_digest_sha256,status FROM decision_jobs WHERE tenant_id=$1 AND project_id=$2 AND environment=$3 AND product_id=$4 AND idempotency_key=$5")
            .bind(&scope.tenant_id).bind(&scope.project_id).bind(&scope.environment)
            .bind(&scope.product_id).bind(&queued.idempotency_key)
            .fetch_optional(&mut *tx).await.map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
        let prior = prior.ok_or(StatusCode::CONFLICT)?;
        if prior.get::<String, _>("request_digest_sha256") != digest {
            return Err(StatusCode::CONFLICT);
        }
        status = prior.get("status");
    }
    tx.commit()
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    Ok((
        StatusCode::ACCEPTED,
        Json(serde_json::json!({"job_id":job_id,"status":status})),
    ))
}

async fn get_job(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Json<Value>, StatusCode> {
    let scope = authenticated_scope(&headers, &state).await?;
    let mut tx = scoped_transaction(&state.store.pool, &scope)
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    let row = sqlx::query("SELECT status,idempotency_key,attempt_count,last_error FROM decision_jobs WHERE job_id=$1 AND tenant_id=$2 AND project_id=$3 AND environment=$4 AND product_id=$5")
        .bind(&id).bind(&scope.tenant_id).bind(&scope.project_id).bind(&scope.environment).bind(&scope.product_id)
        .fetch_optional(&mut *tx).await.map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?
        .ok_or(StatusCode::NOT_FOUND)?;
    let status: String = row.get("status");
    let key: String = row.get("idempotency_key");
    drop(tx);
    let receipt = if status == "completed" {
        state
            .store
            .get_receipt(&scope, &key)
            .await
            .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?
    } else {
        None
    };
    Ok(Json(serde_json::json!({
        "job_id":id,"status":status,"attempt_count":row.get::<i32,_>("attempt_count"),
        "last_error":row.get::<Option<String>,_>("last_error"),"receipt":receipt
    })))
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

    if body.evidence_id.is_empty()
        || body.evidence_id.len() > 128
        || body.revision == 0
        || body.observed_at_unix_ms == 0
        || body
            .expires_at_unix_ms
            .is_some_and(|expiry| expiry <= body.observed_at_unix_ms)
        || !matches!(
            body.sensitivity.as_str(),
            "public" | "internal" | "confidential" | "restricted"
        )
        || body.digest_sha256.len() != 64
    {
        return Err(StatusCode::BAD_REQUEST);
    }
    let actual_digest = format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&body.value).map_err(|_| StatusCode::BAD_REQUEST)?)
    );
    if actual_digest != body.digest_sha256 {
        return Err(StatusCode::BAD_REQUEST);
    }
    let result = state
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
    match result {
        EvidenceWrite::Created => Ok(StatusCode::CREATED),
        EvidenceWrite::Existing => Ok(StatusCode::OK),
        EvidenceWrite::Conflict => Err(StatusCode::CONFLICT),
    }
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
        || !matches!(
            body.disposition.as_str(),
            "accept" | "review" | "deny" | "abstain"
        )
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
        .map_err(|error| match error {
            kernel_runtime::RuntimeError::InvalidRequest => StatusCode::NOT_FOUND,
            _ => StatusCode::SERVICE_UNAVAILABLE,
        })?;
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
        .map_err(|error| match error {
            kernel_runtime::RuntimeError::InvalidRequest => StatusCode::NOT_FOUND,
            _ => StatusCode::SERVICE_UNAVAILABLE,
        })?;
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
    let pool = PgPool::connect(&database_url).await?;
    let store = Arc::new(PostgresStore {
        pool,
        purpose: String::new(),
    });
    let provider: Option<Arc<dyn IntelligenceProvider>> = match env::var("KERNEL_PROVIDER_BASE_URL")
    {
        Ok(base) => {
            let adapter =
                env::var("KERNEL_PROVIDER_ADAPTER").unwrap_or_else(|_| "compatible".into());
            let allowed_host = required("KERNEL_PROVIDER_ALLOWED_HOST");
            let api_key = required("KERNEL_PROVIDER_API_KEY");
            let provider_id = required("KERNEL_PROVIDER_ID");
            let model_id = required("KERNEL_PROVIDER_MODEL");
            let allowed_returned_models = env::var("KERNEL_PROVIDER_ALLOWED_RETURNED_MODELS")
                .ok()
                .filter(|value| !value.is_empty())
                .map(|value| value.split(',').map(str::to_owned).collect::<Vec<_>>())
                .unwrap_or_default();
            if adapter == "system-one" {
                if !allowed_returned_models.is_empty() {
                    return Err("returned model allowlist requires compatible adapter".into());
                }
                Some(Arc::new(kernel_provider::SystemOneProvider::new(
                    &base,
                    &allowed_host,
                    api_key,
                    provider_id,
                    model_id,
                    512,
                )?))
            } else {
                Some(Arc::new(
                    OpenAiCompatibleProvider::new(
                        &base,
                        &allowed_host,
                        api_key,
                        provider_id,
                        model_id,
                        512,
                    )?
                    .with_allowed_returned_models(allowed_returned_models)?,
                ))
            }
        }
        Err(_) => None,
    };
    let state = AppState {
        release_cache: Arc::new(RwLock::new(HashMap::new())),
        provider,
        provider_permits: Arc::new(Semaphore::new(16)),
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
        .route("/v1/decision-jobs", post(create_job))
        .route("/v1/decision-jobs/:id", get(get_job))
        .route("/v1/decisions/:id", get(get_decision))
        .route("/v1/decisions/:id/reviews", post(post_review))
        .route("/v1/decisions/:id/outcomes", post(post_outcome))
        .route("/v1/evidence", post(ingest_evidence))
        .route("/v1/usage", get(get_usage))
        .layer(DefaultBodyLimit::max(64 * 1024))
        .layer(from_fn(error_contract))
        .with_state(state);
    let addr: SocketAddr = env::var("KERNEL_LISTEN_ADDR")
        .unwrap_or_else(|_| "127.0.0.1:8080".into())
        .parse()?;
    axum::serve(tokio::net::TcpListener::bind(addr).await?, app).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use kernel_compiler::DecisionPack;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[tokio::test]
    async fn active_release_is_pinned_and_promotion_uses_revision() {
        let Ok(url) = env::var("KERNEL_TEST_DATABASE_URL") else {
            assert!(
                env::var_os("CI").is_none(),
                "KERNEL_TEST_DATABASE_URL is required in CI"
            );
            eprintln!("set KERNEL_TEST_DATABASE_URL to run release registry integration test");
            return;
        };
        let pool = PgPool::connect(&url).await.unwrap();
        sqlx::raw_sql(include_str!("../../../migrations/0001_init.sql"))
            .execute(&pool)
            .await
            .unwrap();
        sqlx::raw_sql(include_str!(
            "../../../migrations/0008_release_pointers.sql"
        ))
        .execute(&pool)
        .await
        .unwrap();
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let scope = Scope {
            tenant_id: format!("registry-test-{unique}"),
            project_id: "project".into(),
            environment: "test".into(),
            product_id: "product".into(),
        };
        let pack: DecisionPack = serde_json::from_slice(include_bytes!(
            "../../../packs/examples/support-triage.json"
        ))
        .unwrap();
        let first = compile(pack.clone()).unwrap();
        let mut second_pack = pack;
        second_pack.version = "1.1.1".into();
        let second = compile(second_pack).unwrap();
        for graph in [&first, &second] {
            sqlx::query("INSERT INTO behavior_releases (tenant_id,project_id,environment,decision_type,version,content_hash_sha256,compiled_graph) VALUES ($1,$2,$3,$4,$5,$6,$7)")
                .bind(&scope.tenant_id).bind(&scope.project_id).bind(&scope.environment)
                .bind(&graph.pack.id).bind(&graph.pack.version).bind(&graph.content_hash_sha256)
                .bind(serde_json::to_value(graph).unwrap()).execute(&pool).await.unwrap();
        }
        let store = Arc::new(PostgresStore {
            pool: pool.clone(),
            purpose: String::new(),
        });
        let mut state = AppState {
            release_cache: Arc::new(RwLock::new(HashMap::new())),
            provider: None,
            provider_permits: Arc::new(Semaphore::new(1)),
            store,
            static_auth: None,
            request_permits: Arc::new(Semaphore::new(1)),
        };
        assert!(
            resolve_release(&state, &scope, &first.pack.id, "active")
                .await
                .is_err()
        );
        sqlx::query("INSERT INTO release_pointers (tenant_id,project_id,environment,decision_type,version,content_hash_sha256,revision) VALUES ($1,$2,$3,$4,$5,$6,1)")
            .bind(&scope.tenant_id).bind(&scope.project_id).bind(&scope.environment)
            .bind(&first.pack.id).bind(&first.pack.version).bind(&first.content_hash_sha256)
            .execute(&pool).await.unwrap();
        let pinned = resolve_release(&state, &scope, &first.pack.id, "active")
            .await
            .unwrap();
        assert_eq!(pinned.content_hash_sha256, first.content_hash_sha256);
        let token = "release-job-test-token-32-characters";
        state.static_auth = Some((Sha256::digest(token.as_bytes()).into(), scope.clone()));
        let mut headers = HeaderMap::new();
        headers.insert("authorization", format!("Bearer {token}").parse().unwrap());
        headers.insert("idempotency-key", "deferred-one".parse().unwrap());
        let body = serde_json::json!({
            "schema_version":"1.1","product_id":scope.product_id,
            "decision_type":first.pack.id,"release":"active","idempotency_key":"deferred-one",
            "mode":"evaluate","execution_profile":"deferred",
            "state":{"egress_denied":true},"evidence_refs":[],"constraints":{}
        });
        let raw = Bytes::from(serde_json::to_vec(&body).unwrap());
        let (status, Json(created)) =
            create_job(State(state.clone()), headers.clone(), raw.clone())
                .await
                .unwrap();
        assert_eq!(status, StatusCode::ACCEPTED);
        let id = created["job_id"].as_str().unwrap().to_owned();
        let (_, Json(duplicate)) = create_job(State(state.clone()), headers.clone(), raw)
            .await
            .unwrap();
        assert_eq!(duplicate["job_id"], id);
        let Json(found) = get_job(
            State(state.clone()),
            headers.clone(),
            axum::extract::Path(id),
        )
        .await
        .unwrap();
        assert_eq!(found["status"], "queued");
        let mut changed = body;
        changed["state"]["egress_denied"] = serde_json::json!(false);
        assert!(matches!(
            create_job(
                State(state.clone()),
                headers,
                Bytes::from(serde_json::to_vec(&changed).unwrap())
            )
            .await,
            Err(StatusCode::CONFLICT)
        ));
        assert!(matches!(
            resolve_release(
                &state,
                &scope,
                &first.pack.id,
                &format!("{}@{}", second.pack.id, second.pack.version)
            )
            .await,
            Err(StatusCode::PRECONDITION_FAILED)
        ));
        let changed = sqlx::query("UPDATE release_pointers SET version=$5,content_hash_sha256=$6,revision=revision+1 WHERE tenant_id=$1 AND project_id=$2 AND environment=$3 AND decision_type=$4 AND revision=$7")
            .bind(&scope.tenant_id).bind(&scope.project_id).bind(&scope.environment)
            .bind(&first.pack.id).bind(&second.pack.version).bind(&second.content_hash_sha256).bind(1_i64)
            .execute(&pool).await.unwrap();
        assert_eq!(changed.rows_affected(), 1);
        let stale = sqlx::query("UPDATE release_pointers SET version=$5,content_hash_sha256=$6,revision=revision+1 WHERE tenant_id=$1 AND project_id=$2 AND environment=$3 AND decision_type=$4 AND revision=$7")
            .bind(&scope.tenant_id).bind(&scope.project_id).bind(&scope.environment)
            .bind(&first.pack.id).bind(&first.pack.version).bind(&first.content_hash_sha256).bind(1_i64)
            .execute(&pool).await.unwrap();
        assert_eq!(stale.rows_affected(), 0);
        let active = resolve_release(&state, &scope, &first.pack.id, "active")
            .await
            .unwrap();
        assert_eq!(active.content_hash_sha256, second.content_hash_sha256);
        assert_eq!(pinned.content_hash_sha256, first.content_hash_sha256);
        sqlx::query("UPDATE behavior_releases SET revoked_at=now() WHERE tenant_id=$1 AND project_id=$2 AND environment=$3 AND decision_type=$4 AND version=$5")
            .bind(&scope.tenant_id).bind(&scope.project_id).bind(&scope.environment)
            .bind(&second.pack.id).bind(&second.pack.version).execute(&pool).await.unwrap();
        assert!(matches!(
            resolve_release(&state, &scope, &first.pack.id, "active").await,
            Err(StatusCode::GONE)
        ));
    }
}
