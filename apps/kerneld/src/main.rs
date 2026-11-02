mod connections;
mod operations;
mod product;
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
use kernel_provider::IntelligenceProvider;
use kernel_runtime::{
    Engine, ReceiptStore,
    postgres::{EvidenceWrite, PostgresStore, scoped_transaction},
};
use serde_json::Value;
use sha2::{Digest, Sha256};
use sqlx::Row;
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
    let started = std::time::Instant::now();
    let (parts, body) = request.into_parts();
    let mut response = match tokio::time::timeout(
        std::time::Duration::from_millis(3000),
        axum::body::to_bytes(body, 64 * 1024),
    )
    .await
    {
        Ok(Ok(bytes)) => {
            let requested_ms = kernel_core::strict_json(&bytes)
                .ok()
                .and_then(|v| v.get("constraints")?.get("deadline_ms")?.as_u64())
                .unwrap_or(3000)
                .min(3000);
            let mut request = Request::from_parts(parts, Body::from(bytes));
            request.extensions_mut().insert(started);
            match tokio::time::timeout_at(
                tokio::time::Instant::from_std(
                    started + std::time::Duration::from_millis(requested_ms),
                ),
                next.run(request),
            )
            .await
            {
                Ok(response) => response,
                Err(_) => StatusCode::GATEWAY_TIMEOUT.into_response(),
            }
        }
        Ok(Err(_)) => StatusCode::PAYLOAD_TOO_LARGE.into_response(),
        Err(_) => StatusCode::GATEWAY_TIMEOUT.into_response(),
    };
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
                StatusCode::GATEWAY_TIMEOUT => {
                    ("DEADLINE_EXCEEDED", "The request deadline expired", true)
                }
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
    response.headers_mut().append(
        "server-timing",
        HeaderValue::from_str(&format!(
            "http;dur={:.3}",
            started.elapsed().as_secs_f64() * 1000.
        ))
        .expect("numeric duration"),
    );
    operations::observe(
        response.status(),
        started.elapsed(),
        &request_id,
        &correlation_id,
        response
            .headers()
            .get_all("server-timing")
            .iter()
            .filter_map(|h| h.to_str().ok())
            .collect(),
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
    requests_per_minute: i32,
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
        if diff == 0 && scope.is_valid() {
            return operations::admit(state, scope.clone()).await;
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
        Some(row) => {
            let scope = Scope {
                tenant_id: row.get("tenant_id"),
                project_id: row.get("project_id"),
                environment: row.get("environment"),
                product_id: row.get("product_id"),
            };
            if scope.is_valid() {
                operations::admit(state, scope).await
            } else {
                Err(StatusCode::UNAUTHORIZED)
            }
        }
        None => Err(StatusCode::UNAUTHORIZED),
    }
}
async fn health() -> StatusCode {
    StatusCode::OK
}
async fn ready(State(state): State<AppState>) -> StatusCode {
    if kernel_runtime::postgres::database_ready(&state.store.pool, false).await {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    }
}

// These storage operations include auth/release materialization CPU as well as
// their database waits; runtime store timings have the same wall-time semantics.
async fn measure_prelude_db<F: std::future::Future>(micros: &mut u64, future: F) -> F::Output {
    let start = std::time::Instant::now();
    let result = future.await;
    *micros += start.elapsed().as_micros() as u64;
    result
}

async fn decide(
    State(state): State<AppState>,
    started: Option<axum::Extension<std::time::Instant>>,
    headers: HeaderMap,
    raw: Bytes,
) -> Result<Response, StatusCode> {
    let started = started.map(|s| s.0).unwrap_or_else(std::time::Instant::now);
    // Admission precedes authentication/release/database work. Capacity is held
    // by the spawned attempt even if its HTTP caller disconnects.
    let permit = state
        .request_permits
        .clone()
        .try_acquire_owned()
        .map_err(|_| StatusCode::TOO_MANY_REQUESTS)?;
    let prelude_start = std::time::Instant::now();
    let mut prelude_db_us = 0;
    let scope =
        measure_prelude_db(&mut prelude_db_us, authenticated_scope(&headers, &state)).await?;
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
    let graph = measure_prelude_db(
        &mut prelude_db_us,
        resolve_release(&state, &scope, &body.decision_type, &body.release),
    )
    .await?;
    kernel_runtime::prepare_state(&graph, &body.state).map_err(|_| StatusCode::BAD_REQUEST)?;
    let deadline = started
        + std::time::Duration::from_millis(
            3000.min(graph.pack.limits.deadline_ms)
                .min(body.constraints.deadline_ms.unwrap_or(u64::MAX)),
        );
    if std::time::Instant::now() >= deadline {
        return Err(StatusCode::GATEWAY_TIMEOUT);
    }
    let refs: Vec<_> = body
        .evidence_refs
        .into_iter()
        .map(|r| (r.id, r.revision))
        .collect();
    let evidence = measure_prelude_db(
        &mut prelude_db_us,
        tokio::time::timeout_at(
            tokio::time::Instant::from_std(deadline),
            state.store.load_evidence(&scope, &refs),
        ),
    )
    .await
    .map_err(|_| StatusCode::GATEWAY_TIMEOUT)?
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
    let store = Arc::new(PostgresStore {
        pool: state.store.pool.clone(),
        purpose: graph.pack.id.clone(),
    });
    let provider = if graph.pack.nodes.iter().any(|node| {
        matches!(
            node.operation,
            kernel_compiler::Operation::SemanticQuestion { .. }
        )
    }) {
        measure_prelude_db(
            &mut prelude_db_us,
            kernel_runtime::connections::resolve(
                &state.store.pool,
                &scope,
                &graph.pack.id,
                state.provider.clone(),
            ),
        )
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?
    } else {
        None
    };
    let prelude_us = prelude_start.elapsed().as_micros() as u64;
    let engine = Engine {
        graph,
        provider,
        store: store as Arc<dyn ReceiptStore>,
        provider_permits: state.provider_permits.clone(),
        platform_deadline_ms: 3000,
        platform_max_cost_nano_usd: 1_000_000,
    };
    // The bounded task owns the attempt after admission. Client cancellation cannot drop it.
    let task = tokio::spawn(async move {
        let _permit = permit;
        engine.decide_measured(scope, request, deadline).await
    });
    let (result, timings) = task.await.map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    let timing_header = format!(
        "engine;dur={:.3}, db;dur={:.3}, provider;dur={:.3}, queue;dur={:.3}, runtime;dur={:.3}, prelude;dur={:.3}",
        (timings.engine_us + prelude_us.saturating_sub(prelude_db_us)) as f64 / 1000.,
        (timings.database_us + prelude_db_us) as f64 / 1000.,
        timings.provider_us as f64 / 1000.,
        timings.queue_us as f64 / 1000.,
        timings.total_us as f64 / 1000.,
        prelude_us as f64 / 1000.
    );
    match result {
        Ok(receipt) => {
            let mut response = Json(receipt).into_response();
            response.headers_mut().insert(
                "server-timing",
                HeaderValue::from_str(&timing_header).expect("numeric timings"),
            );
            response.headers_mut().insert(
                "x-kernel-store-calls",
                HeaderValue::from(timings.store_calls),
            );
            Ok(response)
        }
        Err(kernel_runtime::RuntimeError::Conflict) => Err(StatusCode::CONFLICT),
        Err(kernel_runtime::RuntimeError::InvalidRequest) => Err(StatusCode::BAD_REQUEST),
        Err(kernel_runtime::RuntimeError::Release) => Err(StatusCode::NOT_FOUND),
        Err(kernel_runtime::RuntimeError::Store) => Err(StatusCode::SERVICE_UNAVAILABLE),
        Err(kernel_runtime::RuntimeError::Deadline) => Err(StatusCode::GATEWAY_TIMEOUT),
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
    if body.schema_version != "1.1"
        || body.state.len() > 128
        || body.product_id != scope.product_id
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
    kernel_runtime::prepare_state(&graph, &body.state).map_err(|_| StatusCode::BAD_REQUEST)?;
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

fn capacity(name: &str, default: usize, max: usize) -> Result<usize, Box<dyn std::error::Error>> {
    let value = match env::var(name) {
        Ok(value) => value.parse::<usize>()?,
        Err(env::VarError::NotPresent) => default,
        Err(e) => return Err(e.into()),
    };
    if value == 0 || value > max {
        return Err(format!("{name} must be between 1 and {max}").into());
    }
    Ok(value)
}

#[tokio::main]
async fn main() {
    if let Some(ready) = kernel_runtime::readiness_probe("127.0.0.1:8080", "KERNEL_LISTEN_ADDR") {
        std::process::exit(if ready { 0 } else { 1 });
    }
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
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(capacity("KERNEL_DB_CONNECTIONS", 16, 256)? as u32)
        .acquire_timeout(std::time::Duration::from_secs(3))
        .connect(&database_url)
        .await?;
    if !kernel_runtime::postgres::database_ready(&pool, false).await {
        return Err("database schema or runtime privileges invalid; use a non-owner kernel_runtime login without kernel_worker membership".into());
    }
    let store = Arc::new(PostgresStore {
        pool,
        purpose: String::new(),
    });
    let provider = kernel_provider::provider_from_env()?;
    let state = AppState {
        release_cache: Arc::new(RwLock::new(HashMap::new())),
        provider,
        provider_permits: Arc::new(Semaphore::new(capacity(
            "KERNEL_PROVIDER_CONCURRENCY",
            16,
            256,
        )?)),
        store,
        static_auth,
        requests_per_minute: capacity("KERNEL_REQUESTS_PER_MINUTE", 6000, 1_000_000)? as i32,
        request_permits: Arc::new(Semaphore::new(capacity(
            "KERNEL_REQUEST_CONCURRENCY",
            256,
            4096,
        )?)),
    };
    let app = Router::new()
        .route("/console", get(product::console))
        .route("/console.js", get(product::console_js))
        .route("/openapi.json", get(product::openapi))
        .route("/v1/packs", get(product::packs))
        .route("/healthz", get(health))
        .route("/readyz", get(ready))
        .route("/metrics", get(operations::metrics))
        .route("/health/live", get(health))
        .route("/health/ready", get(ready))
        .route(
            "/v1/provider-connections",
            get(connections::list).post(connections::put),
        )
        .route(
            "/v1/provider-connections/{id}",
            axum::routing::delete(connections::delete),
        )
        .route(
            "/v1/provider-connections/{id}/test",
            post(connections::test),
        )
        .route(
            "/v1/provider-bindings/{decision}",
            axum::routing::put(connections::bind).delete(connections::unbind),
        )
        .route("/v1/decisions", post(decide))
        .route("/v1/decision-jobs", post(create_job))
        .route("/v1/decision-jobs/{id}", get(get_job))
        .route("/v1/decisions/{id}", get(get_decision))
        .route(
            "/v1/decisions/{id}/reviews",
            get(product::reviews).post(product::review),
        )
        .route(
            "/v1/decisions/{id}/outcomes",
            get(product::outcomes).post(product::outcome),
        )
        .route("/v1/evidence", post(ingest_evidence))
        .route("/v1/usage", get(get_usage))
        .layer(DefaultBodyLimit::max(64 * 1024))
        .layer(from_fn(error_contract))
        .with_state(state);
    let addr: SocketAddr = env::var("KERNEL_LISTEN_ADDR")
        .unwrap_or_else(|_| "127.0.0.1:8080".into())
        .parse()?;
    axum::serve(tokio::net::TcpListener::bind(addr).await?, app)
        .with_graceful_shutdown(async {
            #[cfg(unix)]
            {
                let mut term =
                    tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                        .expect("SIGTERM handler");
                tokio::select! { _ = tokio::signal::ctrl_c() => {}, _ = term.recv() => {} }
            }
            #[cfg(not(unix))]
            {
                let _ = tokio::signal::ctrl_c().await;
            }
        })
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use kernel_compiler::DecisionPack;
    use sqlx::PgPool;
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
            requests_per_minute: 6000,
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
