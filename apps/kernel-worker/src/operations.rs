use axum::{
    Router,
    extract::State,
    http::{HeaderMap, StatusCode},
    routing::get,
};
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};
#[derive(Clone)]
struct Health {
    pool: sqlx::PgPool,
    last_tick: Arc<AtomicU64>,
    max_age_ms: u64,
}
fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
async fn ready(State(health): State<Health>) -> StatusCode {
    if now().saturating_sub(health.last_tick.load(Ordering::Relaxed)) <= health.max_age_ms
        && kernel_runtime::postgres::database_ready(&health.pool, true).await
    {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    }
}
async fn metrics(State(health): State<Health>, headers: HeaderMap) -> Result<String, StatusCode> {
    use sha2::{Digest, Sha256};
    let expected = std::env::var("KERNEL_METRICS_TOKEN").map_err(|_| StatusCode::NOT_FOUND)?;
    if expected.len() < 32 {
        return Err(StatusCode::NOT_FOUND);
    }
    let got = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or("");
    let left = Sha256::digest(expected.as_bytes());
    let right = Sha256::digest(got.as_bytes());
    if left
        .iter()
        .zip(right.iter())
        .fold(0u8, |d, (a, b)| d | (a ^ b))
        != 0
    {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let (queued,dead,oldest):(i64,i64,f64)=sqlx::query_as("SELECT count(*) FILTER (WHERE status IN ('queued','leased')),count(*) FILTER (WHERE status='dead_letter'),COALESCE(extract(epoch FROM now()-min(created_at) FILTER (WHERE status IN ('queued','leased'))),0)::float8 FROM decision_jobs").fetch_one(&health.pool).await.map_err(|_|StatusCode::SERVICE_UNAVAILABLE)?;
    let (outbox,uncertain):(i64,i64)=sqlx::query_as("SELECT (SELECT count(*) FROM outbox_events WHERE status IN ('pending','leased','dead_letter')),(SELECT count(*) FROM provider_attempts WHERE status IN ('dispatch_intent','uncertain'))").fetch_one(&health.pool).await.map_err(|_|StatusCode::SERVICE_UNAVAILABLE)?;
    Ok(format!(
        "kernel_worker_last_progress_age_seconds {}\nkernel_jobs_pending {}\nkernel_jobs_dead_letter {}\nkernel_jobs_oldest_age_seconds {}\nkernel_outbox_unresolved {}\nkernel_attempts_unresolved {}\n",
        now().saturating_sub(health.last_tick.load(Ordering::Relaxed)) as f64 / 1000.,
        queued,
        dead,
        oldest,
        outbox,
        uncertain
    ))
}
pub async fn start(pool: sqlx::PgPool) -> Result<Arc<AtomicU64>, Box<dyn std::error::Error>> {
    let last_tick = Arc::new(AtomicU64::new(0));
    let address =
        std::env::var("KERNEL_WORKER_HEALTH_ADDR").unwrap_or_else(|_| "127.0.0.1:8081".into());
    let app = Router::new()
        .route("/healthz", get(|| async { StatusCode::OK }))
        .route("/readyz", get(ready))
        .route("/metrics", get(metrics))
        .with_state(Health {
            pool,
            last_tick: last_tick.clone(),
            max_age_ms: 90_000,
        });
    let listener = tokio::net::TcpListener::bind(address).await?;
    tokio::spawn(async move {
        if axum::serve(listener, app).await.is_err() {
            std::process::exit(1);
        }
    });
    Ok(last_tick)
}
pub fn tick(last_tick: &AtomicU64, job: bool, event: bool) {
    last_tick.store(now(), Ordering::Relaxed);
    if job || event {
        eprintln!(
            "{}",
            serde_json::json!({"event":"worker.progress","job":job,"outbox":event})
        );
    }
}
