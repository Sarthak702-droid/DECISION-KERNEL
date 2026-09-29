//! Conservative crash recovery: preserve holds for attempts that may have dispatched.
mod jobs;
mod outbox;
use sqlx::PgPool;
use std::env;

#[tokio::main]
async fn main() {
    if let Err(e) = run().await {
        eprintln!("kernel-worker: {e}");
        std::process::exit(1);
    }
}
async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let pool = PgPool::connect(&env::var("DATABASE_URL")?).await?;
    let outbox_sink = outbox::Sink::from_env()?;
    let job_processor = jobs::Processor::from_env()?;
    if outbox_sink.is_none() {
        eprintln!("outbox delivery disabled: KERNEL_OUTBOX_URL is unset; events remain pending");
    }
    loop {
        let changed = sqlx::query("UPDATE provider_attempts SET status='uncertain' WHERE status='dispatch_intent' AND created_at < now() - interval '60 seconds'")
            .execute(&pool).await?;
        if changed.rows_affected() > 0 {
            eprintln!(
                "marked {} stale dispatch intents uncertain; budget holds retained",
                changed.rows_affected()
            );
        }
        let retryable=sqlx::query("WITH stale AS (SELECT tenant_id,project_id,environment,idempotency_key FROM decision_receipts WHERE status='pending' AND created_at < now() - interval '60 seconds' FOR UPDATE SKIP LOCKED) UPDATE decision_receipts AS r SET status='retryable' FROM stale AS s WHERE r.tenant_id=s.tenant_id AND r.project_id=s.project_id AND r.environment=s.environment AND r.idempotency_key=s.idempotency_key AND NOT EXISTS (SELECT 1 FROM provider_attempts AS a WHERE a.tenant_id=r.tenant_id AND a.project_id=r.project_id AND a.environment=r.environment AND a.idempotency_key=r.idempotency_key)")
            .execute(&pool).await?;
        if retryable.rows_affected() > 0 {
            eprintln!(
                "marked {} undispatched claims retryable; next claim receives a new fencing generation",
                retryable.rows_affected()
            );
        }
        let ran_job = job_processor.run_once(&pool).await?;
        let sent_event = if let Some(sink) = &outbox_sink {
            sink.drain_once(&pool).await?
        } else {
            false
        };
        if ran_job || sent_event {
            continue;
        }
        tokio::time::sleep(std::time::Duration::from_secs(10)).await;
    }
}
