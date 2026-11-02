//! No customer identifiers, payloads, URLs or credentials in process telemetry.
use super::*;
static REQUESTS: AtomicU64 = AtomicU64::new(0);
static ERRORS: AtomicU64 = AtomicU64::new(0);
static LIMITED: AtomicU64 = AtomicU64::new(0);
static DURATION_US: AtomicU64 = AtomicU64::new(0);

pub fn observe(
    status: StatusCode,
    duration: std::time::Duration,
    request_id: &str,
    correlation_id: &str,
    timings: Vec<&str>,
) {
    REQUESTS.fetch_add(1, Ordering::Relaxed);
    DURATION_US.fetch_add(duration.as_micros() as u64, Ordering::Relaxed);
    if status.is_server_error() {
        ERRORS.fetch_add(1, Ordering::Relaxed);
    }
    if status == StatusCode::TOO_MANY_REQUESTS {
        LIMITED.fetch_add(1, Ordering::Relaxed);
    }
    eprintln!(
        "{}",
        serde_json::json!({"event":"http.completed", "request_id":request_id,
        "correlation_id":correlation_id,"status":status.as_u16(),"duration_us":duration.as_micros(),"spans":timings})
    );
}
pub async fn metrics(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<String, StatusCode> {
    let expected = env::var("KERNEL_METRICS_TOKEN").map_err(|_| StatusCode::NOT_FOUND)?;
    if expected.len() < 32 {
        return Err(StatusCode::NOT_FOUND);
    }
    let supplied = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or("");
    let left = Sha256::digest(expected.as_bytes());
    let right = Sha256::digest(supplied.as_bytes());
    if left
        .iter()
        .zip(right.iter())
        .fold(0u8, |diff, (a, b)| diff | (a ^ b))
        != 0
    {
        return Err(StatusCode::UNAUTHORIZED);
    }
    Ok(format!(
        "kernel_http_requests_total {}\nkernel_http_errors_total {}\nkernel_http_limited_total {}\nkernel_http_duration_seconds_sum {}\nkernel_db_connections {}\nkernel_db_idle_connections {}\n",
        REQUESTS.load(Ordering::Relaxed),
        ERRORS.load(Ordering::Relaxed),
        LIMITED.load(Ordering::Relaxed),
        DURATION_US.load(Ordering::Relaxed) as f64 / 1_000_000.,
        state.store.pool.size(),
        state.store.pool.num_idle()
    ))
}
pub async fn admit(state: &AppState, scope: Scope) -> Result<Scope, StatusCode> {
    let mut tx = scoped_transaction(&state.store.pool, &scope)
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    let blocked: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM lifecycle_blocks WHERE tenant_id=$1 AND project_id=$2 AND environment=$3 AND product_id=$4)")
        .bind(&scope.tenant_id).bind(&scope.project_id).bind(&scope.environment).bind(&scope.product_id)
    .fetch_one(&mut *tx)
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    if blocked {
        return Err(StatusCode::GONE);
    }
    // Server-clock window shared across replicas. One row per provisioned authenticated scope.
    let count: Option<i32> = sqlx::query_scalar("INSERT INTO operation_windows (tenant_id,project_id,environment,product_id,window_start,requests) VALUES ($1,$2,$3,$4,floor(extract(epoch FROM clock_timestamp())/60)::bigint,1) ON CONFLICT (tenant_id,project_id,environment,product_id) DO UPDATE SET window_start=EXCLUDED.window_start,requests=CASE WHEN operation_windows.window_start=EXCLUDED.window_start THEN operation_windows.requests+1 ELSE 1 END WHERE operation_windows.window_start<>EXCLUDED.window_start OR operation_windows.requests < COALESCE((SELECT requests_per_minute FROM operation_limits WHERE tenant_id=$1 AND project_id=$2 AND environment=$3 AND product_id=$4),$5) RETURNING requests")
        .bind(&scope.tenant_id).bind(&scope.project_id).bind(&scope.environment).bind(&scope.product_id).bind(state.requests_per_minute)
        .fetch_optional(&mut *tx).await.map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    tx.commit()
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    if count.is_none() {
        return Err(StatusCode::TOO_MANY_REQUESTS);
    }
    Ok(scope)
}
