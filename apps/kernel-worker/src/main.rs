//! Conservative crash recovery: preserve holds for attempts that may have dispatched.
mod jobs;
mod operations;
mod outbox;
use std::env;

#[tokio::main]
async fn main() {
    if let Some(ready) =
        kernel_runtime::readiness_probe("127.0.0.1:8081", "KERNEL_WORKER_HEALTH_ADDR")
    {
        std::process::exit(if ready { 0 } else { 1 });
    }
    if let Err(e) = run().await {
        eprintln!("kernel-worker: {e}");
        std::process::exit(1);
    }
}
async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let pool = sqlx::postgres::PgPoolOptions::new()
        .acquire_timeout(std::time::Duration::from_secs(3))
        .connect(&env::var("DATABASE_URL")?)
        .await?;
    if !kernel_runtime::postgres::database_ready(&pool, true).await {
        return Err(
            "database schema or worker privileges invalid; use a non-owner kernel_worker login"
                .into(),
        );
    }
    let poll_ms = env::var("KERNEL_WORKER_POLL_MS")
        .unwrap_or_else(|_| "100".into())
        .parse::<u64>()?;
    if !(10..=10_000).contains(&poll_ms) {
        return Err("KERNEL_WORKER_POLL_MS must be between 10 and 10000".into());
    }
    let outbox_sink = outbox::Sink::from_env()?;
    let job_processor = jobs::Processor::from_env()?;
    if outbox_sink.is_none() {
        eprintln!("outbox delivery disabled: KERNEL_OUTBOX_URL is unset; events remain pending");
    }
    let last_tick = operations::start(pool.clone()).await?;
    loop {
        kernel_runtime::postgres::recover_stale(&pool).await?;
        let ran_job = job_processor.run_once(&pool).await?;
        let sent_event = if let Some(sink) = &outbox_sink {
            sink.drain_once(&pool).await?
        } else {
            false
        };
        operations::tick(&last_tick, ran_job, sent_event);
        if ran_job || sent_event {
            continue;
        }
        tokio::time::sleep(std::time::Duration::from_millis(poll_ms)).await;
    }
}
