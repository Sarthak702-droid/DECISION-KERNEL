use super::{Claim, ReceiptStore, Reserve, RuntimeError};
use kernel_core::{Answer, DecisionReceipt, Disposition, EvidenceRef, ReasonCode, Scope};
use kernel_provider::{ProviderIdentity, SemanticResponse};
use sqlx::{PgPool, Row};
use std::{
    future::Future,
    pin::Pin,
    time::{SystemTime, UNIX_EPOCH},
};

fn current_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

pub struct PostgresStore {
    pub pool: PgPool,
    pub purpose: String,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EvidenceWrite {
    Created,
    Existing,
    Conflict,
}
pub async fn scoped_transaction<'a>(
    pool: &'a PgPool,
    scope: &Scope,
) -> Result<sqlx::Transaction<'a, sqlx::Postgres>, RuntimeError> {
    let mut tx = pool.begin().await.map_err(|_| RuntimeError::Store)?;
    sqlx::query("SELECT set_config('kernel.tenant_id',$1,true),set_config('kernel.project_id',$2,true),set_config('kernel.environment',$3,true),set_config('kernel.product_id',$4,true),set_config('statement_timeout','3000',true),set_config('lock_timeout','3000',true)")
        .bind(&scope.tenant_id)
        .bind(&scope.project_id)
        .bind(&scope.environment)
        .bind(&scope.product_id)
        .execute(&mut *tx)
        .await
        .map_err(|_| RuntimeError::Store)?;
    Ok(tx)
}
impl PostgresStore {
    pub async fn load_evidence(
        &self,
        scope: &Scope,
        refs: &[(String, u64)],
    ) -> Result<Vec<EvidenceRef>, RuntimeError> {
        if refs.is_empty() {
            return Ok(Vec::new());
        }
        let ids: Vec<String> = refs.iter().map(|(id, _)| id.clone()).collect();
        let revisions: Vec<i64> = refs
            .iter()
            .map(|(_, r)| i64::try_from(*r).map_err(|_| RuntimeError::InvalidRequest))
            .collect::<Result<_, _>>()?;
        let mut tx = scoped_transaction(&self.pool, scope).await?;
        let rows=sqlx::query("SELECT e.evidence_id,e.revision,e.observed_at_unix_ms,e.expires_at_unix_ms,e.digest_sha256,e.sensitivity,e.value FROM unnest($4::text[],$5::bigint[]) AS requested(evidence_id,revision) JOIN evidence AS e ON e.product_id=current_setting('kernel.product_id',true) AND e.evidence_id=requested.evidence_id AND e.revision=requested.revision AND e.tenant_id=$1 AND e.project_id=$2 AND e.environment=$3")
            .bind(&scope.tenant_id).bind(&scope.project_id).bind(&scope.environment).bind(ids).bind(revisions)
            .fetch_all(&mut *tx).await.map_err(|_|RuntimeError::Store)?;
        let mut result = Vec::with_capacity(rows.len());
        for row in rows {
            result.push(EvidenceRef {
                id: row.get("evidence_id"),
                revision: u64::try_from(row.get::<i64, _>("revision"))
                    .map_err(|_| RuntimeError::Store)?,
                observed_at_unix_ms: u64::try_from(row.get::<i64, _>("observed_at_unix_ms"))
                    .map_err(|_| RuntimeError::Store)?,
                expires_at_unix_ms: row
                    .get::<Option<i64>, _>("expires_at_unix_ms")
                    .map(|v| u64::try_from(v).map_err(|_| RuntimeError::Store))
                    .transpose()?,
                digest_sha256: row.get("digest_sha256"),
                sensitivity: row.get("sensitivity"),
                value: row.get("value"),
            });
        }
        for (id, revision) in refs {
            if !result
                .iter()
                .any(|e| &e.id == id && e.revision == *revision)
            {
                result.push(EvidenceRef {
                    id: id.clone(),
                    revision: *revision,
                    observed_at_unix_ms: 0,
                    expires_at_unix_ms: Some(0),
                    digest_sha256: String::new(),
                    sensitivity: "unavailable".into(),
                    value: serde_json::Value::Null,
                });
            }
        }
        result.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(result)
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn store_evidence(
        &self,
        scope: &Scope,
        evidence_id: &str,
        revision: u64,
        observed_at_unix_ms: u64,
        expires_at_unix_ms: Option<u64>,
        digest_sha256: &str,
        sensitivity: &str,
        value: &serde_json::Value,
    ) -> Result<EvidenceWrite, RuntimeError> {
        let rev = i64::try_from(revision).map_err(|_| RuntimeError::InvalidRequest)?;
        let obs = i64::try_from(observed_at_unix_ms).map_err(|_| RuntimeError::InvalidRequest)?;
        let exp = expires_at_unix_ms
            .map(|e| i64::try_from(e).map_err(|_| RuntimeError::InvalidRequest))
            .transpose()?;
        let mut tx = scoped_transaction(&self.pool, scope).await?;
        let res = sqlx::query(
            "INSERT INTO evidence (tenant_id,project_id,environment,evidence_id,revision,observed_at_unix_ms,expires_at_unix_ms,digest_sha256,sensitivity,value) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10) ON CONFLICT DO NOTHING"
        )
        .bind(&scope.tenant_id)
        .bind(&scope.project_id)
        .bind(&scope.environment)
        .bind(evidence_id)
        .bind(rev)
        .bind(obs)
        .bind(exp)
        .bind(digest_sha256)
        .bind(sensitivity)
        .bind(value)
        .execute(&mut *tx)
        .await
        .map_err(|_| RuntimeError::Store)?;
        let result = if res.rows_affected() > 0 {
            EvidenceWrite::Created
        } else {
            let prior = sqlx::query("SELECT observed_at_unix_ms,expires_at_unix_ms,digest_sha256,sensitivity,value FROM evidence WHERE product_id=current_setting('kernel.product_id',true) AND tenant_id=$1 AND project_id=$2 AND environment=$3 AND evidence_id=$4 AND revision=$5")
                .bind(&scope.tenant_id).bind(&scope.project_id).bind(&scope.environment)
                .bind(evidence_id).bind(rev).fetch_one(&mut *tx).await.map_err(|_| RuntimeError::Store)?;
            if prior.get::<i64, _>("observed_at_unix_ms") == obs
                && prior.get::<Option<i64>, _>("expires_at_unix_ms") == exp
                && prior.get::<String, _>("digest_sha256") == digest_sha256
                && prior.get::<String, _>("sensitivity") == sensitivity
                && prior.get::<serde_json::Value, _>("value") == *value
            {
                EvidenceWrite::Existing
            } else {
                EvidenceWrite::Conflict
            }
        };
        tx.commit().await.map_err(|_| RuntimeError::Store)?;
        Ok(result)
    }

    pub async fn get_receipt(
        &self,
        scope: &Scope,
        idempotency_key: &str,
    ) -> Result<Option<DecisionReceipt>, RuntimeError> {
        let mut tx = scoped_transaction(&self.pool, scope).await?;
        let row = sqlx::query(
            "SELECT receipt FROM decision_receipts WHERE product_id=current_setting('kernel.product_id',true) AND tenant_id=$1 AND project_id=$2 AND environment=$3 AND idempotency_key=$4 AND status='committed' AND receipt->'scope'->>'product_id'=$5"
        )
        .bind(&scope.tenant_id)
        .bind(&scope.project_id)
        .bind(&scope.environment)
        .bind(idempotency_key)
        .bind(&scope.product_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|_| RuntimeError::Store)?;
        row.map(|r| {
            let val: serde_json::Value = r.get("receipt");
            serde_json::from_value(val).map_err(|_| RuntimeError::Store)
        })
        .transpose()
    }

    pub async fn record_review(
        &self,
        scope: &Scope,
        idempotency_key: &str,
        reviewer_id: &str,
        disposition: &str,
        notes: Option<&str>,
    ) -> Result<(), RuntimeError> {
        let mut tx = scoped_transaction(&self.pool, scope).await?;
        let receipt_exists: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM decision_receipts WHERE product_id=current_setting('kernel.product_id',true) AND tenant_id=$1 AND project_id=$2 AND environment=$3 AND idempotency_key=$4 AND status='committed' AND receipt->'scope'->>'product_id'=$5)")
            .bind(&scope.tenant_id).bind(&scope.project_id).bind(&scope.environment)
            .bind(idempotency_key).bind(&scope.product_id)
            .fetch_one(&mut *tx).await.map_err(|_| RuntimeError::Store)?;
        if !receipt_exists {
            return Err(RuntimeError::InvalidRequest);
        }
        sqlx::query(
            "INSERT INTO decision_reviews (tenant_id,project_id,environment,idempotency_key,reviewer_id,disposition,notes) VALUES ($1,$2,$3,$4,$5,$6,$7)"
        )
        .bind(&scope.tenant_id)
        .bind(&scope.project_id)
        .bind(&scope.environment)
        .bind(idempotency_key)
        .bind(reviewer_id)
        .bind(disposition)
        .bind(notes)
        .execute(&mut *tx)
        .await
        .map_err(|_| RuntimeError::Store)?;
        sqlx::query("INSERT INTO outbox_events (event_id,tenant_id,project_id,environment,event_type,payload) VALUES (gen_random_uuid()::text,$1,$2,$3,'review.recorded',$4)")
            .bind(&scope.tenant_id).bind(&scope.project_id).bind(&scope.environment)
            .bind(serde_json::json!({"idempotency_key":idempotency_key,"reviewer_id":reviewer_id,"disposition":disposition}))
            .execute(&mut *tx).await.map_err(|_| RuntimeError::Store)?;
        tx.commit().await.map_err(|_| RuntimeError::Store)?;
        Ok(())
    }

    pub async fn record_outcome(
        &self,
        scope: &Scope,
        idempotency_key: &str,
        outcome: &str,
        verified: bool,
        verification_ref: Option<&str>,
    ) -> Result<(), RuntimeError> {
        let mut tx = scoped_transaction(&self.pool, scope).await?;
        let receipt_exists: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM decision_receipts WHERE product_id=current_setting('kernel.product_id',true) AND tenant_id=$1 AND project_id=$2 AND environment=$3 AND idempotency_key=$4 AND status='committed' AND receipt->'scope'->>'product_id'=$5)")
            .bind(&scope.tenant_id).bind(&scope.project_id).bind(&scope.environment)
            .bind(idempotency_key).bind(&scope.product_id)
            .fetch_one(&mut *tx).await.map_err(|_| RuntimeError::Store)?;
        if !receipt_exists {
            return Err(RuntimeError::InvalidRequest);
        }
        sqlx::query(
            "INSERT INTO decision_outcomes (tenant_id,project_id,environment,idempotency_key,outcome,verified,verification_ref) VALUES ($1,$2,$3,$4,$5,$6,$7)"
        )
        .bind(&scope.tenant_id)
        .bind(&scope.project_id)
        .bind(&scope.environment)
        .bind(idempotency_key)
        .bind(outcome)
        .bind(verified)
        .bind(verification_ref)
        .execute(&mut *tx)
        .await
        .map_err(|_| RuntimeError::Store)?;
        sqlx::query("INSERT INTO outbox_events (event_id,tenant_id,project_id,environment,event_type,payload) VALUES (gen_random_uuid()::text,$1,$2,$3,'outcome.observed',$4)")
            .bind(&scope.tenant_id).bind(&scope.project_id).bind(&scope.environment)
            .bind(serde_json::json!({"idempotency_key":idempotency_key,"outcome":outcome,"verified":verified}))
            .execute(&mut *tx).await.map_err(|_| RuntimeError::Store)?;
        tx.commit().await.map_err(|_| RuntimeError::Store)?;
        Ok(())
    }

    pub async fn query_usage(&self, scope: &Scope) -> Result<Vec<UsageSummary>, RuntimeError> {
        let mut tx = scoped_transaction(&self.pool, scope).await?;
        let rows = sqlx::query(
            "SELECT b.purpose, b.cap_nano_usd, b.spent_nano_usd, b.held_nano_usd,
                    COALESCE(SUM(CASE WHEN a.status = 'dispatch_intent' THEN 1 ELSE 0 END), 0)::bigint AS attempts_dispatch_intent,
                    COALESCE(SUM(CASE WHEN a.status = 'uncertain' THEN 1 ELSE 0 END), 0)::bigint AS attempts_uncertain,
                    COALESCE(SUM(CASE WHEN a.status = 'reconciled' THEN 1 ELSE 0 END), 0)::bigint AS attempts_reconciled
             FROM budget_buckets b
             LEFT JOIN provider_attempts a ON a.product_id=b.product_id AND a.tenant_id = b.tenant_id AND a.project_id = b.project_id AND a.purpose = b.purpose AND a.environment = $3
             WHERE b.product_id=current_setting('kernel.product_id',true) AND b.environment=$3 AND b.tenant_id = $1 AND b.project_id = $2
             GROUP BY b.purpose, b.cap_nano_usd, b.spent_nano_usd, b.held_nano_usd"
        )
        .bind(&scope.tenant_id)
        .bind(&scope.project_id)
        .bind(&scope.environment)
        .fetch_all(&mut *tx)
        .await
        .map_err(|_| RuntimeError::Store)?;

        let mut res = Vec::with_capacity(rows.len());
        for r in rows {
            res.push(UsageSummary {
                purpose: r.get("purpose"),
                cap_nano_usd: r.get("cap_nano_usd"),
                spent_nano_usd: r.get("spent_nano_usd"),
                held_nano_usd: r.get("held_nano_usd"),
                attempts_dispatch_intent: r.get("attempts_dispatch_intent"),
                attempts_uncertain: r.get("attempts_uncertain"),
                attempts_reconciled: r.get("attempts_reconciled"),
            });
        }
        Ok(res)
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct UsageSummary {
    pub purpose: String,
    pub cap_nano_usd: i64,
    pub spent_nano_usd: i64,
    pub held_nano_usd: i64,
    pub attempts_dispatch_intent: i64,
    pub attempts_uncertain: i64,
    pub attempts_reconciled: i64,
}

impl ReceiptStore for PostgresStore {
    fn save_recovery_recorded<'a>(
        &'a self,
        scope: &'a Scope,
        key: &'a str,
        generation: u64,
        receipt: &'a DecisionReceipt,
        request: &'a kernel_core::DecisionRequest,
    ) -> Pin<Box<dyn Future<Output = Result<(), RuntimeError>> + Send + 'a>> {
        Box::pin(async move {
            let mut tx = scoped_transaction(&self.pool, scope).await?;
            // Commit the crash fallback and its exact input together, before dispatch.
            // A CTE avoids another durable transaction on every new decision.
            let updated: i64 = sqlx::query_scalar("WITH updated AS (UPDATE decision_receipts SET receipt=$6 WHERE tenant_id=$1 AND project_id=$2 AND environment=$3 AND product_id=current_setting('kernel.product_id',true) AND idempotency_key=$4 AND generation=$5 AND status='pending' RETURNING 1), captured AS (INSERT INTO recorded_requests (tenant_id,project_id,environment,product_id,idempotency_key,request) SELECT $1,$2,$3,current_setting('kernel.product_id',true),$4,$7 FROM updated ON CONFLICT DO NOTHING RETURNING 1) SELECT count(*) FROM updated")
                .bind(&scope.tenant_id).bind(&scope.project_id).bind(&scope.environment).bind(key)
                .bind(i64::try_from(generation).map_err(|_|RuntimeError::Store)?)
                .bind(serde_json::to_value(receipt).map_err(|_|RuntimeError::Store)?)
                .bind(serde_json::to_value(request).map_err(|_|RuntimeError::Store)?)
                .fetch_one(&mut *tx).await.map_err(|_|RuntimeError::Store)?;
            if updated != 1 {
                return Err(RuntimeError::Store);
            }
            tx.commit().await.map_err(|_| RuntimeError::Store)?;
            Ok(())
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
            let mut tx = scoped_transaction(&self.pool, scope).await?;
            let updated = sqlx::query("UPDATE decision_receipts SET receipt=$6 WHERE tenant_id=$1 AND project_id=$2 AND environment=$3 AND product_id=current_setting('kernel.product_id',true) AND idempotency_key=$4 AND generation=$5 AND status='pending'")
                .bind(&scope.tenant_id).bind(&scope.project_id).bind(&scope.environment).bind(key)
                .bind(i64::try_from(generation).map_err(|_| RuntimeError::Store)?)
                .bind(serde_json::to_value(receipt).map_err(|_| RuntimeError::Store)?)
                .execute(&mut *tx).await.map_err(|_| RuntimeError::Store)?;
            if updated.rows_affected() != 1 {
                return Err(RuntimeError::Store);
            }
            tx.commit().await.map_err(|_| RuntimeError::Store)?;
            Ok(())
        })
    }
    fn qualification_active<'a>(
        &'a self,
        scope: &'a Scope,
        reference: &'a str,
        release_hash: &'a str,
        identity: &'a ProviderIdentity,
        returned_model_id: &'a str,
        now_ms: u64,
    ) -> Pin<Box<dyn Future<Output = Result<bool, RuntimeError>> + Send + 'a>> {
        Box::pin(async move {
            let mut tx = scoped_transaction(&self.pool, scope).await?;
            let exists:bool=sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM provider_qualifications WHERE tenant_id=$1 AND project_id=$2 AND environment=$3 AND qualification_ref=$4 AND release_hash_sha256=$5 AND provider_id=$6 AND model_id=$7 AND adapter_version=$8 AND COALESCE(returned_model_id,model_id)=$9 AND active AND evaluation_report IS NOT NULL AND approval_ref IS NOT NULL AND approved_by IS NOT NULL AND valid_until_unix_ms>$10)")
                .bind(&scope.tenant_id).bind(&scope.project_id).bind(&scope.environment).bind(reference).bind(release_hash).bind(&identity.provider_id).bind(&identity.model_id).bind(&identity.adapter_version).bind(returned_model_id).bind(i64::try_from(now_ms).map_err(|_|RuntimeError::InvalidRequest)?)
                .fetch_one(&mut *tx).await.map_err(|_|RuntimeError::Store)?;
            Ok(exists)
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
            let mut tx = scoped_transaction(&self.pool, scope).await?;
            let now_ms = i64::try_from(now_ms).map_err(|_| RuntimeError::InvalidRequest)?;
            let row=sqlx::query("SELECT semantic_response FROM prediction_cache WHERE product_id=current_setting('kernel.product_id',true) AND tenant_id=$1 AND project_id=$2 AND environment=$3 AND cache_key_sha256=$4 AND expires_at_unix_ms>$5")
                .bind(&scope.tenant_id).bind(&scope.project_id).bind(&scope.environment).bind(key).bind(now_ms)
                .fetch_optional(&mut *tx).await.map_err(|_|RuntimeError::Store)?;
            row.map(|r| {
                serde_json::from_value(r.get::<serde_json::Value, _>("semantic_response"))
                    .map_err(|_| RuntimeError::Store)
            })
            .transpose()
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
            let mut tx = scoped_transaction(&self.pool, scope).await?;
            let expiry =
                i64::try_from(expires_at_unix_ms).map_err(|_| RuntimeError::InvalidRequest)?;
            let value = serde_json::to_value(response).map_err(|_| RuntimeError::Store)?;
            sqlx::query("INSERT INTO prediction_cache (tenant_id,project_id,environment,cache_key_sha256,semantic_response,expires_at_unix_ms) VALUES ($1,$2,$3,$4,$5,$6) ON CONFLICT (tenant_id,project_id,environment,product_id,cache_key_sha256) DO UPDATE SET semantic_response=EXCLUDED.semantic_response,expires_at_unix_ms=EXCLUDED.expires_at_unix_ms,created_at=now() WHERE prediction_cache.expires_at_unix_ms<$7")
                .bind(&scope.tenant_id).bind(&scope.project_id).bind(&scope.environment).bind(key).bind(value).bind(expiry).bind(i64::try_from(current_unix_ms()).map_err(|_|RuntimeError::Store)?)
                .execute(&mut *tx).await.map_err(|_|RuntimeError::Store)?;
            tx.commit().await.map_err(|_| RuntimeError::Store)?;
            Ok(())
        })
    }
    fn claim<'a>(
        &'a self,
        scope: &'a Scope,
        key: &'a str,
        digest: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<Claim, RuntimeError>> + Send + 'a>> {
        Box::pin(async move {
            let mut tx = scoped_transaction(&self.pool, scope).await?;
            let inserted = sqlx::query("INSERT INTO decision_receipts (tenant_id,project_id,environment,idempotency_key,request_digest,status) VALUES ($1,$2,$3,$4,$5,'pending') ON CONFLICT DO NOTHING")
                .bind(&scope.tenant_id).bind(&scope.project_id).bind(&scope.environment).bind(key).bind(digest)
                .execute(&mut *tx).await.map_err(|_| RuntimeError::Store)?;
            if inserted.rows_affected() == 1 {
                tx.commit().await.map_err(|_| RuntimeError::Store)?;
                return Ok(Claim::New(1));
            }
            let row = sqlx::query("SELECT request_digest,status,generation,receipt FROM decision_receipts WHERE product_id=current_setting('kernel.product_id',true) AND tenant_id=$1 AND project_id=$2 AND environment=$3 AND idempotency_key=$4")
                .bind(&scope.tenant_id).bind(&scope.project_id).bind(&scope.environment).bind(key)
                .fetch_one(&mut *tx).await.map_err(|_| RuntimeError::Store)?;
            let prior: String = row.get("request_digest");
            if prior != digest {
                return Ok(Claim::Conflict);
            }
            if row.get::<String, _>("status") == "retryable" {
                let generation: i64 = row.get("generation");
                let updated=sqlx::query("UPDATE decision_receipts SET status='pending',generation=generation+1,created_at=now() WHERE product_id=current_setting('kernel.product_id',true) AND tenant_id=$1 AND project_id=$2 AND environment=$3 AND idempotency_key=$4 AND request_digest=$5 AND status='retryable' AND generation=$6")
                    .bind(&scope.tenant_id).bind(&scope.project_id).bind(&scope.environment).bind(key).bind(digest).bind(generation)
                    .execute(&mut *tx).await.map_err(|_|RuntimeError::Store)?;
                if updated.rows_affected() == 1 {
                    tx.commit().await.map_err(|_| RuntimeError::Store)?;
                    return Ok(Claim::New(
                        u64::try_from(generation + 1).map_err(|_| RuntimeError::Store)?,
                    ));
                }
                return Ok(Claim::Busy);
            }
            if row.get::<String, _>("status") != "committed" {
                return Ok(Claim::Busy);
            }
            let value: serde_json::Value = row.get("receipt");
            let receipt: DecisionReceipt =
                serde_json::from_value(value).map_err(|_| RuntimeError::Store)?;
            if receipt.scope != *scope {
                return Err(RuntimeError::Store);
            }
            Ok(Claim::Existing(Box::new(receipt)))
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
            let amount = i64::try_from(amount).map_err(|_| RuntimeError::InvalidRequest)?;
            let generation = i64::try_from(generation).map_err(|_| RuntimeError::Store)?;
            let mut tx = scoped_transaction(&self.pool, scope).await?;
            // Keep receipt fencing, cap enforcement and dispatch intent atomic,
            // while eliminating two network round trips holding the hot budget row.
            let row = sqlx::query(
                "WITH claim AS MATERIALIZED (
                    SELECT 1 FROM decision_receipts
                    WHERE tenant_id=$1 AND project_id=$2 AND environment=$3 AND product_id=$4
                      AND idempotency_key=$5 AND generation=$6 AND status='pending'
                    FOR UPDATE
                 ), funded AS (
                    UPDATE budget_buckets SET held_nano_usd=held_nano_usd+$8
                    WHERE tenant_id=$1 AND project_id=$2 AND environment=$3 AND product_id=$4
                      AND purpose=$7 AND cap_nano_usd-spent_nano_usd-held_nano_usd >= $8
                      AND EXISTS (SELECT 1 FROM claim)
                    RETURNING 1
                 ), attempt AS (
                    INSERT INTO provider_attempts
                        (tenant_id,project_id,environment,product_id,idempotency_key,attempt_no,
                         purpose,reserved_nano_usd,status,provider_id,model_id)
                    SELECT $1,$2,$3,$4,$5,1,$7,$8,'dispatch_intent',$9,$10 FROM funded
                    RETURNING 1
                 ) SELECT EXISTS(SELECT 1 FROM claim) AS valid_claim,
                          EXISTS(SELECT 1 FROM attempt) AS granted",
            )
            .bind(&scope.tenant_id)
            .bind(&scope.project_id)
            .bind(&scope.environment)
            .bind(&scope.product_id)
            .bind(key)
            .bind(generation)
            .bind(&self.purpose)
            .bind(amount)
            .bind(provider_id)
            .bind(model_id)
            .fetch_one(&mut *tx)
            .await
            .map_err(|_| RuntimeError::Store)?;
            if !row.get::<bool, _>("valid_claim") {
                return Err(RuntimeError::Store);
            }
            let granted = row.get::<bool, _>("granted");
            tx.commit().await.map_err(|_| RuntimeError::Store)?;
            Ok(if granted {
                Reserve::Granted
            } else {
                Reserve::Denied
            })
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
        self.commit_receipt(scope, key, digest, generation, receipt, false)
    }
}

impl PostgresStore {
    pub(crate) fn commit_receipt<'a>(
        &'a self,
        scope: &'a Scope,
        key: &'a str,
        digest: &'a str,
        generation: u64,
        receipt: &'a DecisionReceipt,
        probe: bool,
    ) -> Pin<Box<dyn Future<Output = Result<DecisionReceipt, RuntimeError>> + Send + 'a>> {
        Box::pin(async move {
            let mut tx = scoped_transaction(&self.pool, scope).await?;
            let generation = i64::try_from(generation).map_err(|_| RuntimeError::Store)?;
            let (decision_type, version) = receipt
                .release
                .rsplit_once('@')
                .ok_or(RuntimeError::Release)?;
            if decision_type != self.purpose {
                return Err(RuntimeError::Release);
            }
            let mut committed = receipt.clone();
            if !probe {
                let release=sqlx::query("SELECT content_hash_sha256, revoked_at IS NOT NULL AS revoked FROM behavior_releases WHERE tenant_id=$1 AND project_id=$2 AND environment=$3 AND decision_type=$4 AND version=$5 FOR SHARE")
                .bind(&scope.tenant_id).bind(&scope.project_id).bind(&scope.environment).bind(decision_type).bind(version)
                .fetch_one(&mut *tx).await.map_err(|_|RuntimeError::Store)?;
                if release.get::<String, _>("content_hash_sha256") != receipt.release_digest_sha256
                {
                    return Err(RuntimeError::Release);
                }
                if release.get::<bool, _>("revoked") {
                    committed.disposition = Disposition::Deny;
                    committed.reason_codes.clear();
                    committed.reason_codes.push(ReasonCode::RevokedRelease);
                }
            } else {
                // Operational probes use an internal, review-only graph. This
                // path is available only through the private probe ReceiptStore.
                if receipt.release != "connection.test@1.0.0" || self.purpose != "connection.test" {
                    return Err(RuntimeError::Release);
                }
                committed.disposition = Disposition::Review;
            }
            if receipt.attempts > 0 {
                let updated=sqlx::query("UPDATE provider_attempts SET status='uncertain',reported_charge_nano_usd=$5::text::numeric,reported_usage=$6 WHERE product_id=current_setting('kernel.product_id',true) AND tenant_id=$1 AND project_id=$2 AND environment=$3 AND idempotency_key=$4 AND attempt_no=1 AND status='dispatch_intent'")
                    .bind(&scope.tenant_id).bind(&scope.project_id).bind(&scope.environment).bind(key)
                    .bind(receipt.provider_reported_cost_nano_usd.map(|v| v.to_string()))
                    .bind(&receipt.provider_usage)
                    .execute(&mut *tx).await.map_err(|_| RuntimeError::Store)?;
                if updated.rows_affected() != 1 {
                    return Err(RuntimeError::Store);
                }
            }
            let value = serde_json::to_value(&committed).map_err(|_| RuntimeError::Store)?;
            let updated = sqlx::query("UPDATE decision_receipts SET status='committed',receipt=$7 WHERE product_id=current_setting('kernel.product_id',true) AND tenant_id=$1 AND project_id=$2 AND environment=$3 AND idempotency_key=$4 AND request_digest=$5 AND generation=$6 AND status='pending'")
                .bind(&scope.tenant_id).bind(&scope.project_id).bind(&scope.environment).bind(key).bind(digest).bind(generation).bind(value)
                .execute(&mut *tx).await.map_err(|_| RuntimeError::Store)?;
            if updated.rows_affected() != 1 {
                return Err(RuntimeError::Store);
            }
            sqlx::query("INSERT INTO outbox_events (event_id,tenant_id,project_id,environment,event_type,payload) VALUES (gen_random_uuid()::text,$1,$2,$3,'decision.completed',$4)")
                .bind(&scope.tenant_id).bind(&scope.project_id).bind(&scope.environment)
                .bind(serde_json::json!({"receipt_id":committed.receipt_id,"idempotency_key":key,"release":committed.release,"disposition":committed.disposition}))
                .execute(&mut *tx).await.map_err(|_| RuntimeError::Store)?;
            let mut extra_events = Vec::new();
            if committed
                .answers
                .values()
                .any(|answer| matches!(answer, Answer::NotEvaluated { .. }))
            {
                extra_events.push("decision.not_evaluated");
            }
            if committed.disposition == Disposition::Review {
                extra_events.push("review.requested");
            }
            for event_type in extra_events {
                sqlx::query("INSERT INTO outbox_events (event_id,tenant_id,project_id,environment,event_type,payload) VALUES (gen_random_uuid()::text,$1,$2,$3,$4,$5)")
                    .bind(&scope.tenant_id).bind(&scope.project_id).bind(&scope.environment)
                    .bind(event_type)
                    .bind(serde_json::json!({"receipt_id":committed.receipt_id,"idempotency_key":key,"release":committed.release}))
                    .execute(&mut *tx).await.map_err(|_| RuntimeError::Store)?;
            }
            tx.commit().await.map_err(|_| RuntimeError::Store)?;
            Ok(committed)
        })
    }
}

/// Cross-scope maintenance: requires kernel_worker, never the API login.
/// A possibly dispatched call is never replayed. Unknown charges keep their holds.
pub async fn recover_stale(pool: &PgPool) -> Result<(), sqlx::Error> {
    let mut tx = pool.begin().await?;
    sqlx::query("SET LOCAL statement_timeout='3000ms'")
        .execute(&mut *tx)
        .await?;
    sqlx::query("UPDATE provider_attempts SET status='uncertain' WHERE status='dispatch_intent' AND created_at < now()-interval '60 seconds'")
        .execute(&mut *tx).await?;
    sqlx::query("WITH stale AS (SELECT tenant_id,project_id,environment,product_id,idempotency_key FROM decision_receipts WHERE status='pending' AND created_at < now()-interval '60 seconds' FOR UPDATE SKIP LOCKED) UPDATE decision_receipts r SET status='retryable' FROM stale s WHERE r.tenant_id=s.tenant_id AND r.project_id=s.project_id AND r.environment=s.environment AND r.product_id=s.product_id AND r.idempotency_key=s.idempotency_key AND NOT EXISTS (SELECT 1 FROM provider_attempts a WHERE a.tenant_id=r.tenant_id AND a.project_id=r.project_id AND a.environment=r.environment AND a.product_id=r.product_id AND a.idempotency_key=r.idempotency_key)")
        .execute(&mut *tx).await?;
    sqlx::query("WITH recovered AS (
        UPDATE decision_receipts r SET status='committed', receipt=r.receipt || jsonb_build_object(
            'provider_id',a.provider_id,'model_id',a.model_id,
            'provider_reported_cost_nano_usd',a.reported_charge_nano_usd,'provider_usage',a.reported_usage,
            'cost_nano_usd',a.actual_charge_nano_usd)
        FROM provider_attempts a WHERE r.status='pending' AND r.receipt IS NOT NULL
            AND r.created_at < now()-interval '60 seconds'
            AND a.tenant_id=r.tenant_id AND a.project_id=r.project_id AND a.environment=r.environment
            AND a.product_id=r.product_id AND a.idempotency_key=r.idempotency_key
        RETURNING r.*)
        INSERT INTO outbox_events (event_id,tenant_id,project_id,environment,product_id,event_type,payload)
        SELECT gen_random_uuid()::text,r.tenant_id,r.project_id,r.environment,r.product_id,event_type,
            jsonb_build_object('receipt_id',r.receipt->>'receipt_id','idempotency_key',r.idempotency_key,'reason','crash_recovery')
        FROM recovered r CROSS JOIN (VALUES ('decision.completed'),('decision.recovered'),('review.requested')) events(event_type)")
        .execute(&mut *tx).await?;
    tx.commit().await
}

impl PostgresStore {
    /// Reconcile an external invoice once, including charges above the hold.
    pub async fn reconcile(
        &self,
        scope: &Scope,
        key: &str,
        charged: u64,
        proof: &str,
    ) -> Result<(), RuntimeError> {
        if proof.is_empty() || proof.len() > 256 {
            return Err(RuntimeError::InvalidRequest);
        }
        let charged = i64::try_from(charged).map_err(|_| RuntimeError::InvalidRequest)?;
        let mut tx = scoped_transaction(&self.pool, scope).await?;
        let row = sqlx::query("SELECT purpose,reserved_nano_usd,status,actual_charge_nano_usd,reconciliation_ref FROM provider_attempts WHERE tenant_id=$1 AND project_id=$2 AND environment=$3 AND product_id=current_setting('kernel.product_id',true) AND idempotency_key=$4 AND attempt_no=1 FOR UPDATE")
            .bind(&scope.tenant_id).bind(&scope.project_id).bind(&scope.environment).bind(key)
            .fetch_one(&mut *tx).await.map_err(|_| RuntimeError::Store)?;
        if row.get::<String, _>("status") == "reconciled" {
            return if row.get::<Option<i64>, _>("actual_charge_nano_usd") == Some(charged)
                && row
                    .get::<Option<String>, _>("reconciliation_ref")
                    .as_deref()
                    == Some(proof)
            {
                Ok(())
            } else {
                Err(RuntimeError::Conflict)
            };
        }
        let reserved: i64 = row.get("reserved_nano_usd");
        let purpose: String = row.get("purpose");
        let updated = sqlx::query("UPDATE budget_buckets SET held_nano_usd=held_nano_usd-$4,spent_nano_usd=spent_nano_usd+$5 WHERE tenant_id=$1 AND project_id=$2 AND purpose=$3 AND environment=current_setting('kernel.environment',true) AND product_id=current_setting('kernel.product_id',true) AND held_nano_usd >= $4")
            .bind(&scope.tenant_id).bind(&scope.project_id).bind(&purpose).bind(reserved).bind(charged)
            .execute(&mut *tx).await.map_err(|_| RuntimeError::Store)?;
        if updated.rows_affected() != 1 {
            return Err(RuntimeError::Store);
        }
        sqlx::query("UPDATE provider_attempts SET status='reconciled',actual_charge_nano_usd=$5,reconciliation_ref=$6 WHERE tenant_id=$1 AND project_id=$2 AND environment=$3 AND product_id=current_setting('kernel.product_id',true) AND idempotency_key=$4 AND attempt_no=1")
            .bind(&scope.tenant_id).bind(&scope.project_id).bind(&scope.environment).bind(key).bind(charged).bind(proof)
            .execute(&mut *tx).await.map_err(|_| RuntimeError::Store)?;
        sqlx::query("UPDATE decision_receipts SET receipt=jsonb_set(receipt,'{cost_nano_usd}',to_jsonb($5::bigint)) WHERE tenant_id=$1 AND project_id=$2 AND environment=$3 AND product_id=current_setting('kernel.product_id',true) AND idempotency_key=$4 AND receipt IS NOT NULL")
            .bind(&scope.tenant_id).bind(&scope.project_id).bind(&scope.environment).bind(key).bind(charged)
            .execute(&mut *tx).await.map_err(|_| RuntimeError::Store)?;
        sqlx::query("INSERT INTO outbox_events (event_id,tenant_id,project_id,environment,event_type,payload) VALUES (gen_random_uuid()::text,$1,$2,$3,$4,$5)")
            .bind(&scope.tenant_id).bind(&scope.project_id).bind(&scope.environment)
            .bind(if charged>reserved { "provider.overspend" } else { "provider.reconciled" })
            .bind(serde_json::json!({"idempotency_key":key,"reserved_nano_usd":reserved,"actual_charge_nano_usd":charged,"proof":proof}))
            .execute(&mut *tx).await.map_err(|_| RuntimeError::Store)?;
        tx.commit().await.map_err(|_| RuntimeError::Store)?;
        Ok(())
    }
}

/// Startup/readiness contract. Runtime processes must use separate non-owner
/// credentials; migrations and provisioning always use the owner connection.
pub async fn database_ready(pool: &PgPool, worker: bool) -> bool {
    let result = sqlx::query_scalar::<_, bool>("SELECT
        NOT r.rolsuper AND NOT r.rolbypassrls
        AND NOT EXISTS (SELECT 1 FROM pg_class protected
            WHERE protected.relnamespace='public'::regnamespace
            AND protected.relname IN ('decision_receipts','provider_attempts','evidence','prediction_cache','budget_buckets','decision_jobs','outbox_events','behavior_releases','release_pointers','provider_qualifications','provider_connections','provider_bindings','reviewer_tokens','decision_reviews','decision_outcomes','operation_limits','operation_windows','recorded_requests','lifecycle_blocks')
            AND (NOT protected.relrowsecurity OR pg_has_role(current_user,protected.relowner,'USAGE')))
        AND pg_has_role(current_user,'kernel_runtime','member')
        AND pg_has_role(current_user,'kernel_worker','member') = $1
        AND has_column_privilege(current_user,'behavior_releases','revoked_at','UPDATE')
        AND has_table_privilege(current_user,'decision_receipts','INSERT') AND has_table_privilege(current_user,'decision_receipts','UPDATE')
        AND has_table_privilege(current_user,'provider_attempts','INSERT') AND has_table_privilege(current_user,'provider_attempts','UPDATE')
        AND has_table_privilege(current_user,'evidence','SELECT') AND has_table_privilege(current_user,'evidence','INSERT')
        AND has_table_privilege(current_user,'outbox_events','INSERT')
        AND has_function_privilege(current_user,'kernel_authenticate_token(text)','EXECUTE')
        AND NOT EXISTS (SELECT 1 FROM (VALUES
            ('decision_receipts','SELECT'),('provider_attempts','SELECT'),
            ('budget_buckets','SELECT'),('budget_buckets','UPDATE'),
            ('prediction_cache','SELECT'),('prediction_cache','INSERT'),('prediction_cache','UPDATE'),
            ('behavior_releases','SELECT'),('release_pointers','SELECT'),('provider_qualifications','SELECT'),
            ('decision_reviews','SELECT'),('decision_reviews','INSERT'),
            ('decision_outcomes','SELECT'),('decision_outcomes','INSERT'),
            ('decision_jobs','SELECT'),('decision_jobs','INSERT'),
            ('provider_connections','SELECT'),('provider_connections','INSERT'),('provider_connections','UPDATE'),
            ('provider_bindings','SELECT'),('provider_bindings','INSERT'),('provider_bindings','UPDATE'),('provider_bindings','DELETE')) needed(table_name,privilege)
            WHERE NOT has_table_privilege(current_user,needed.table_name,needed.privilege))
        AND (NOT $1 OR (has_table_privilege(current_user,'decision_jobs','UPDATE')
            AND has_table_privilege(current_user,'outbox_events','SELECT')
            AND has_table_privilege(current_user,'outbox_events','UPDATE')))
        AND has_table_privilege(current_user,'reviewer_tokens','SELECT')
        AND has_table_privilege(current_user,'operation_windows','UPDATE')
        AND has_table_privilege(current_user,'operation_limits','SELECT')
        AND has_table_privilege(current_user,'lifecycle_blocks','SELECT')
        AND has_table_privilege(current_user,'recorded_requests','INSERT')
        AND EXISTS (SELECT 1 FROM information_schema.columns WHERE table_name='decision_reviews' AND column_name='corrections')
        AND EXISTS (SELECT 1 FROM information_schema.columns WHERE table_name='decision_outcomes' AND column_name='status')
        AND EXISTS (SELECT 1 FROM information_schema.columns WHERE table_name='provider_attempts' AND column_name='reported_usage')
        FROM pg_roles r JOIN pg_class c ON c.relname='decision_receipts' AND c.relnamespace='public'::regnamespace
        WHERE r.rolname=current_user")
        .bind(worker).fetch_one(pool).await;
    result.unwrap_or(false)
}
