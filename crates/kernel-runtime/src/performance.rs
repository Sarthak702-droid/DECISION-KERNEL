//! Per-invocation wall time. Awaited store calls include pool acquisition and
//! transactions; engine time is the residual, not a CPU-time measurement.
use super::*;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

#[derive(Debug, Default, Clone, serde::Serialize)]
pub struct Timings {
    pub engine_us: u64,
    pub database_us: u64,
    pub provider_us: u64,
    pub queue_us: u64,
    pub total_us: u64,
    pub store_calls: u64,
}
#[derive(Default)]
pub(crate) struct Metrics {
    database: AtomicU64,
    provider: AtomicU64,
    queue: AtomicU64,
    calls: AtomicU64,
}
#[derive(Clone, Copy)]
pub(crate) enum Kind {
    Database,
    Provider,
    Queue,
}
tokio::task_local! { pub(crate) static METRICS: Arc<Metrics>; }
struct Timer<'a> {
    metrics: &'a Metrics,
    kind: Kind,
    start: Instant,
}
impl Drop for Timer<'_> {
    fn drop(&mut self) {
        let counter = match self.kind {
            Kind::Database => &self.metrics.database,
            Kind::Provider => &self.metrics.provider,
            Kind::Queue => &self.metrics.queue,
        };
        counter.fetch_add(self.start.elapsed().as_nanos() as u64, Ordering::Relaxed);
    }
}
impl Metrics {
    async fn measure<F: Future>(&self, kind: Kind, future: F) -> F::Output {
        if matches!(kind, Kind::Database) {
            self.calls.fetch_add(1, Ordering::Relaxed);
        }
        let _timer = Timer {
            metrics: self,
            kind,
            start: Instant::now(),
        };
        future.await
    }
    pub(crate) fn snapshot(&self, total: Duration) -> Timings {
        let db = self.database.load(Ordering::Relaxed);
        let provider = self.provider.load(Ordering::Relaxed);
        let queue = self.queue.load(Ordering::Relaxed);
        let total = total.as_nanos() as u64;
        Timings {
            engine_us: total.saturating_sub(db + provider + queue) / 1000,
            database_us: db / 1000,
            provider_us: provider / 1000,
            queue_us: queue / 1000,
            total_us: total / 1000,
            store_calls: self.calls.load(Ordering::Relaxed),
        }
    }
}
pub(crate) async fn timed<F: Future>(kind: Kind, future: F) -> F::Output {
    let metrics = METRICS.try_with(Arc::clone).ok();
    if let Some(metrics) = metrics {
        metrics.measure(kind, future).await
    } else {
        future.await
    }
}
pub(crate) fn current(total: Duration) -> Timings {
    METRICS.try_with(|m| m.snapshot(total)).unwrap_or_default()
}
// Fixed-size coordination table: no input-dependent memory growth or cached
// answers. Hash collisions may serialize unrelated requests but never reuse data.
pub(crate) async fn prediction_lock(key: &str) -> tokio::sync::OwnedMutexGuard<()> {
    static LOCKS: OnceLock<Vec<Arc<tokio::sync::Mutex<()>>>> = OnceLock::new();
    let locks = LOCKS.get_or_init(|| {
        (0..4096)
            .map(|_| Arc::new(tokio::sync::Mutex::new(())))
            .collect()
    });
    let hash = Sha256::digest(key.as_bytes());
    let index = u16::from_be_bytes([hash[0], hash[1]]) as usize % locks.len();
    locks[index].clone().lock_owned().await
}
pub(crate) struct TimedStore {
    pub inner: Arc<dyn ReceiptStore>,
    pub metrics: Arc<Metrics>,
}
impl ReceiptStore for TimedStore {
    fn save_recovery_recorded<'a>(
        &'a self,
        scope: &'a Scope,
        key: &'a str,
        generation: u64,
        receipt: &'a DecisionReceipt,
        request: &'a DecisionRequest,
    ) -> Pin<Box<dyn Future<Output = Result<(), RuntimeError>> + Send + 'a>> {
        Box::pin(async move {
            self.metrics
                .measure(
                    Kind::Database,
                    self.inner
                        .save_recovery_recorded(scope, key, generation, receipt, request),
                )
                .await
        })
    }
    fn save_recovery<'a>(
        &'a self,
        scope: &'a Scope,
        key: &'a str,
        generation: u64,
        receipt: &'a DecisionReceipt,
    ) -> Pin<Box<dyn Future<Output = Result<(), RuntimeError>> + Send + 'a>> {
        Box::pin(async move {
            self.metrics
                .measure(
                    Kind::Database,
                    self.inner.save_recovery(scope, key, generation, receipt),
                )
                .await
        })
    }
    fn claim<'a>(
        &'a self,
        scope: &'a Scope,
        key: &'a str,
        digest: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<Claim, RuntimeError>> + Send + 'a>> {
        Box::pin(async move {
            self.metrics
                .measure(Kind::Database, self.inner.claim(scope, key, digest))
                .await
        })
    }
    fn commit<'a>(
        &'a self,
        scope: &'a Scope,
        key: &'a str,
        digest: &'a str,
        generation: u64,
        receipt: &'a DecisionReceipt,
    ) -> Pin<Box<dyn Future<Output = Result<DecisionReceipt, RuntimeError>> + Send + 'a>> {
        Box::pin(async move {
            self.metrics
                .measure(
                    Kind::Database,
                    self.inner.commit(scope, key, digest, generation, receipt),
                )
                .await
        })
    }
    fn reserve<'a>(
        &'a self,
        scope: &'a Scope,
        key: &'a str,
        generation: u64,
        amount: u64,
        provider_id: &'a str,
        model_id: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<Reserve, RuntimeError>> + Send + 'a>> {
        Box::pin(async move {
            self.metrics
                .measure(
                    Kind::Database,
                    self.inner
                        .reserve(scope, key, generation, amount, provider_id, model_id),
                )
                .await
        })
    }
    fn cached_prediction<'a>(
        &'a self,
        scope: &'a Scope,
        key: &'a str,
        now_ms: u64,
    ) -> Pin<Box<dyn Future<Output = Result<Option<SemanticResponse>, RuntimeError>> + Send + 'a>>
    {
        Box::pin(async move {
            self.metrics
                .measure(
                    Kind::Database,
                    self.inner.cached_prediction(scope, key, now_ms),
                )
                .await
        })
    }
    fn record_prediction<'a>(
        &'a self,
        scope: &'a Scope,
        key: &'a str,
        response: &'a SemanticResponse,
        expires_at_unix_ms: u64,
    ) -> Pin<Box<dyn Future<Output = Result<(), RuntimeError>> + Send + 'a>> {
        Box::pin(async move {
            self.metrics
                .measure(
                    Kind::Database,
                    self.inner
                        .record_prediction(scope, key, response, expires_at_unix_ms),
                )
                .await
        })
    }
    fn qualification_active<'a>(
        &'a self,
        scope: &'a Scope,
        qualification_ref: &'a str,
        release_hash: &'a str,
        identity: &'a ProviderIdentity,
        returned_model_id: &'a str,
        now_ms: u64,
    ) -> Pin<Box<dyn Future<Output = Result<bool, RuntimeError>> + Send + 'a>> {
        Box::pin(async move {
            self.metrics
                .measure(
                    Kind::Database,
                    self.inner.qualification_active(
                        scope,
                        qualification_ref,
                        release_hash,
                        identity,
                        returned_model_id,
                        now_ms,
                    ),
                )
                .await
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn waits_and_cancelled_operations_are_accounted_separately() {
        let metrics = Arc::new(Metrics::default());
        let start = Instant::now();
        METRICS
            .scope(metrics.clone(), async {
                timed(
                    Kind::Database,
                    tokio::time::sleep(Duration::from_millis(20)),
                )
                .await;
                timed(
                    Kind::Provider,
                    tokio::time::sleep(Duration::from_millis(30)),
                )
                .await;
                timed(Kind::Queue, tokio::time::sleep(Duration::from_millis(10))).await;
                let _ = tokio::time::timeout(
                    Duration::from_millis(10),
                    timed(Kind::Database, std::future::pending::<()>()),
                )
                .await;
            })
            .await;
        let t = metrics.snapshot(start.elapsed());
        assert!(t.database_us >= 30_000);
        assert!(t.provider_us >= 30_000);
        assert!(t.queue_us >= 10_000);
        assert_eq!(t.store_calls, 2);
        assert!(
            t.total_us
                .abs_diff(t.engine_us + t.database_us + t.provider_us + t.queue_us)
                <= 3
        );
    }
}
