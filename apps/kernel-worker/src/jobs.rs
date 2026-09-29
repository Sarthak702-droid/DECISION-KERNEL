use kernel_compiler::{CompiledDecisionGraph, compile};
use kernel_core::{DecisionRequest, QueuedDecision};
use kernel_provider::{IntelligenceProvider, OpenAiCompatibleProvider, SystemOneProvider};
use kernel_runtime::{Engine, ReceiptStore, postgres::PostgresStore};
use serde_json::Value;
use sha2::{Digest, Sha256};
use sqlx::{PgPool, Row};
use std::{
    env,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};
use tokio::sync::Semaphore;

struct Job {
    id: String,
    generation: i64,
    attempts: i32,
    tenant: String,
    project: String,
    environment: String,
    product: String,
    decision_type: String,
    idempotency_key: String,
    request_digest: String,
    request: Value,
}

pub struct Processor {
    provider: Option<Arc<dyn IntelligenceProvider>>,
    permits: Arc<Semaphore>,
    worker_id: String,
}

impl Processor {
    pub fn from_env() -> Result<Self, Box<dyn std::error::Error>> {
        let provider: Option<Arc<dyn IntelligenceProvider>> =
            match env::var("KERNEL_PROVIDER_BASE_URL") {
                Ok(base) => {
                    let allowed_host = env::var("KERNEL_PROVIDER_ALLOWED_HOST")?;
                    let api_key = env::var("KERNEL_PROVIDER_API_KEY")?;
                    let provider_id = env::var("KERNEL_PROVIDER_ID")?;
                    let model_id = env::var("KERNEL_PROVIDER_MODEL")?;
                    let adapter =
                        env::var("KERNEL_PROVIDER_ADAPTER").unwrap_or_else(|_| "compatible".into());
                    if adapter == "system-one" {
                        Some(Arc::new(SystemOneProvider::new(
                            &base,
                            &allowed_host,
                            api_key,
                            provider_id,
                            model_id,
                            512,
                        )?))
                    } else if adapter == "compatible" {
                        let allowed = env::var("KERNEL_PROVIDER_ALLOWED_RETURNED_MODELS")
                            .ok()
                            .filter(|value| !value.is_empty())
                            .map(|value| value.split(',').map(str::to_owned).collect())
                            .unwrap_or_default();
                        Some(Arc::new(
                            OpenAiCompatibleProvider::new(
                                &base,
                                &allowed_host,
                                api_key,
                                provider_id,
                                model_id,
                                512,
                            )?
                            .with_allowed_returned_models(allowed)?,
                        ))
                    } else {
                        return Err("unknown provider adapter".into());
                    }
                }
                Err(_) => None,
            };
        let started = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
        Ok(Self {
            provider,
            permits: Arc::new(Semaphore::new(16)),
            worker_id: format!("{}-{started}", std::process::id()),
        })
    }

    pub async fn run_once(&self, pool: &PgPool) -> Result<bool, Box<dyn std::error::Error>> {
        let Some(job) = lease(pool, &self.worker_id, None).await? else {
            return Ok(false);
        };
        let result = self.execute(pool, &job).await;
        let error = result
            .as_ref()
            .err()
            .map(|_| "decision_execution_failed".to_owned());
        if !finish(pool, &self.worker_id, &job, error).await? {
            eprintln!("decision job {} lost its lease before completion", job.id);
        }
        Ok(true)
    }

    async fn execute(&self, pool: &PgPool, job: &Job) -> Result<(), Box<dyn std::error::Error>> {
        let digest = format!("{:x}", Sha256::digest(serde_json::to_vec(&job.request)?));
        if digest != job.request_digest {
            return Err("job payload digest mismatch".into());
        }
        let queued: QueuedDecision = serde_json::from_value(job.request.clone())?;
        if queued.scope.tenant_id != job.tenant
            || queued.scope.project_id != job.project
            || queued.scope.environment != job.environment
            || queued.scope.product_id != job.product
            || queued.decision_type != job.decision_type
            || queued.idempotency_key != job.idempotency_key
        {
            return Err("job scope or identity mismatch".into());
        }
        let (decision_type, version) = queued
            .release
            .rsplit_once('@')
            .ok_or("invalid pinned release")?;
        if decision_type != queued.decision_type {
            return Err("job release mismatch".into());
        }
        let row = sqlx::query("SELECT compiled_graph FROM behavior_releases WHERE tenant_id=$1 AND project_id=$2 AND environment=$3 AND decision_type=$4 AND version=$5 AND content_hash_sha256=$6")
            .bind(&queued.scope.tenant_id).bind(&queued.scope.project_id).bind(&queued.scope.environment)
            .bind(decision_type).bind(version).bind(&queued.release_digest_sha256)
            .fetch_optional(pool).await?.ok_or("pinned release missing")?;
        let raw: Value = row.get("compiled_graph");
        let graph: CompiledDecisionGraph = serde_json::from_value(raw.clone())?;
        let rebuilt = compile(graph.pack.clone())?;
        if graph.content_hash_sha256 != queued.release_digest_sha256
            || serde_json::to_value(&rebuilt)? != raw
        {
            return Err("pinned release integrity failure".into());
        }
        let store = Arc::new(PostgresStore {
            pool: pool.clone(),
            purpose: queued.decision_type.clone(),
        });
        let evidence = store
            .load_evidence(&queued.scope, &queued.evidence_refs)
            .await?;
        let request = DecisionRequest {
            schema_version: queued.schema_version.clone(),
            decision_type: queued.decision_type.clone(),
            release: queued.release.clone(),
            idempotency_key: queued.idempotency_key.clone(),
            state: queued.state.clone(),
            evidence,
            constraints: queued.constraints.clone(),
        };
        let engine = Engine {
            graph: Arc::new(graph),
            provider: self.provider.clone(),
            store: store as Arc<dyn ReceiptStore>,
            provider_permits: self.permits.clone(),
            platform_deadline_ms: 60_000,
            platform_max_cost_nano_usd: 1_000_000,
        };
        engine.decide(queued.scope.clone(), request).await?;
        Ok(())
    }
}

async fn lease(
    pool: &PgPool,
    worker_id: &str,
    job_filter: Option<&str>,
) -> Result<Option<Job>, Box<dyn std::error::Error>> {
    let mut tx = pool.begin().await?;
    let row = sqlx::query(
        "WITH next AS (
        SELECT job_id FROM decision_jobs
        WHERE ((status='queued' AND available_at<=now()) OR (status='leased' AND lease_until<=now()))
          AND ($2::text IS NULL OR job_id=$2)
        ORDER BY created_at FOR UPDATE SKIP LOCKED LIMIT 1
    ) UPDATE decision_jobs AS j SET status='leased',lease_owner=$1,
        lease_generation=j.lease_generation+1,lease_until=now()+interval '90 seconds',
        attempt_count=j.attempt_count+1,updated_at=now()
    FROM next WHERE j.job_id=next.job_id
    RETURNING j.job_id,j.lease_generation,j.attempt_count,j.tenant_id,j.project_id,
              j.environment,j.product_id,j.decision_type,j.idempotency_key,
              j.request_digest_sha256,j.request_payload",
    )
    .bind(worker_id)
    .bind(job_filter)
    .fetch_optional(&mut *tx)
    .await?;
    tx.commit().await?;
    row.map(|row| {
        Ok(Job {
            id: row.get("job_id"),
            generation: row.get("lease_generation"),
            attempts: row.get("attempt_count"),
            tenant: row.get("tenant_id"),
            project: row.get("project_id"),
            environment: row.get("environment"),
            product: row.get("product_id"),
            decision_type: row.get("decision_type"),
            idempotency_key: row.get("idempotency_key"),
            request_digest: row.get("request_digest_sha256"),
            request: row.get("request_payload"),
        })
    })
    .transpose()
}

async fn finish(
    pool: &PgPool,
    worker_id: &str,
    job: &Job,
    error: Option<String>,
) -> Result<bool, sqlx::Error> {
    let updated = if let Some(error) = error {
        let delay = 2_i32.saturating_pow(job.attempts.clamp(1, 6) as u32);
        sqlx::query("UPDATE decision_jobs SET status=$4,available_at=now()+make_interval(secs => $5),lease_owner=NULL,lease_until=NULL,last_error=$6,updated_at=now() WHERE job_id=$1 AND lease_owner=$2 AND lease_generation=$3 AND status='leased'")
            .bind(&job.id).bind(worker_id).bind(job.generation)
            .bind(if job.attempts >= 3 { "dead_letter" } else { "queued" })
            .bind(delay).bind(error).execute(pool).await?
    } else {
        sqlx::query("UPDATE decision_jobs SET status='completed',lease_owner=NULL,lease_until=NULL,last_error=NULL,updated_at=now() WHERE job_id=$1 AND lease_owner=$2 AND lease_generation=$3 AND status='leased'")
            .bind(&job.id).bind(worker_id).bind(job.generation).execute(pool).await?
    };
    Ok(updated.rows_affected() == 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use kernel_compiler::DecisionPack;
    use kernel_core::{Disposition, RequestLimits, Scope};

    #[tokio::test]
    async fn queued_decision_pins_release_and_commits_receipt() {
        let Ok(url) = env::var("KERNEL_TEST_DATABASE_URL") else {
            assert!(
                env::var_os("CI").is_none(),
                "KERNEL_TEST_DATABASE_URL is required in CI"
            );
            return;
        };
        let pool = PgPool::connect(&url).await.unwrap();
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let scope = Scope {
            tenant_id: format!("job-test-{unique}"),
            project_id: "test".into(),
            environment: "test".into(),
            product_id: "test".into(),
        };
        let pack: DecisionPack = serde_json::from_slice(include_bytes!(
            "../../../packs/examples/support-triage.json"
        ))
        .unwrap();
        let graph = compile(pack).unwrap();
        sqlx::query("INSERT INTO behavior_releases (tenant_id,project_id,environment,decision_type,version,content_hash_sha256,compiled_graph) VALUES ($1,$2,$3,$4,$5,$6,$7)")
            .bind(&scope.tenant_id).bind(&scope.project_id).bind(&scope.environment)
            .bind(&graph.pack.id).bind(&graph.pack.version).bind(&graph.content_hash_sha256)
            .bind(serde_json::to_value(&graph).unwrap()).execute(&pool).await.unwrap();
        let queued = QueuedDecision {
            scope: scope.clone(),
            schema_version: "1.1".into(),
            decision_type: graph.pack.id.clone(),
            release: format!("{}@{}", graph.pack.id, graph.pack.version),
            release_digest_sha256: graph.content_hash_sha256.clone(),
            idempotency_key: format!("job-{unique}"),
            state: std::collections::BTreeMap::from([(
                "egress_denied".into(),
                serde_json::json!(true),
            )]),
            evidence_refs: vec![],
            constraints: RequestLimits::default(),
        };
        let payload = serde_json::to_value(&queued).unwrap();
        let digest = format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(&payload).unwrap())
        );
        let job_id = format!("job-test-{unique}");
        sqlx::query("INSERT INTO decision_jobs (job_id,tenant_id,project_id,environment,product_id,decision_type,idempotency_key,request_digest_sha256,request_payload) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9)")
            .bind(&job_id).bind(&scope.tenant_id).bind(&scope.project_id).bind(&scope.environment)
            .bind(&scope.product_id).bind(&queued.decision_type).bind(&queued.idempotency_key)
            .bind(&digest).bind(payload).execute(&pool).await.unwrap();
        let job = lease(&pool, "worker-a", Some(&job_id))
            .await
            .unwrap()
            .unwrap();
        let processor = Processor {
            provider: None,
            permits: Arc::new(Semaphore::new(1)),
            worker_id: "worker-a".into(),
        };
        processor.execute(&pool, &job).await.unwrap();
        assert!(!finish(&pool, "worker-b", &job, None).await.unwrap());
        assert!(finish(&pool, "worker-a", &job, None).await.unwrap());
        let status: String = sqlx::query_scalar("SELECT status FROM decision_jobs WHERE job_id=$1")
            .bind(&job_id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(status, "completed");
        let receipt = PostgresStore {
            pool: pool.clone(),
            purpose: queued.decision_type.clone(),
        }
        .get_receipt(&scope, &queued.idempotency_key)
        .await
        .unwrap()
        .unwrap();
        assert_eq!(receipt.disposition, Disposition::Deny);
        let events: i64 = sqlx::query_scalar("SELECT count(*) FROM outbox_events WHERE tenant_id=$1 AND event_type='decision.completed'")
            .bind(&scope.tenant_id).fetch_one(&pool).await.unwrap();
        assert_eq!(events, 1);
    }
}
