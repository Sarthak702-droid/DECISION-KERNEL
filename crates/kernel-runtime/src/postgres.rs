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
    sqlx::query("SELECT set_config('kernel.tenant_id',$1,true),set_config('kernel.project_id',$2,true),set_config('kernel.environment',$3,true)")
        .bind(&scope.tenant_id)
        .bind(&scope.project_id)
        .bind(&scope.environment)
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
        let rows=sqlx::query("SELECT e.evidence_id,e.revision,e.observed_at_unix_ms,e.expires_at_unix_ms,e.digest_sha256,e.sensitivity,e.value FROM unnest($4::text[],$5::bigint[]) AS requested(evidence_id,revision) JOIN evidence AS e ON e.evidence_id=requested.evidence_id AND e.revision=requested.revision AND e.tenant_id=$1 AND e.project_id=$2 AND e.environment=$3")
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
            let prior = sqlx::query("SELECT observed_at_unix_ms,expires_at_unix_ms,digest_sha256,sensitivity,value FROM evidence WHERE tenant_id=$1 AND project_id=$2 AND environment=$3 AND evidence_id=$4 AND revision=$5")
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
            "SELECT receipt FROM decision_receipts WHERE tenant_id=$1 AND project_id=$2 AND environment=$3 AND idempotency_key=$4 AND status='committed' AND receipt->'scope'->>'product_id'=$5"
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
        let receipt_exists: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM decision_receipts WHERE tenant_id=$1 AND project_id=$2 AND environment=$3 AND idempotency_key=$4 AND status='committed' AND receipt->'scope'->>'product_id'=$5)")
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
        let receipt_exists: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM decision_receipts WHERE tenant_id=$1 AND project_id=$2 AND environment=$3 AND idempotency_key=$4 AND status='committed' AND receipt->'scope'->>'product_id'=$5)")
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
             LEFT JOIN provider_attempts a ON a.tenant_id = b.tenant_id AND a.project_id = b.project_id AND a.purpose = b.purpose AND a.environment = $3
             WHERE b.tenant_id = $1 AND b.project_id = $2
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
            let row=sqlx::query("SELECT semantic_response FROM prediction_cache WHERE tenant_id=$1 AND project_id=$2 AND environment=$3 AND cache_key_sha256=$4 AND expires_at_unix_ms>$5")
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
            sqlx::query("INSERT INTO prediction_cache (tenant_id,project_id,environment,cache_key_sha256,semantic_response,expires_at_unix_ms) VALUES ($1,$2,$3,$4,$5,$6) ON CONFLICT (tenant_id,project_id,environment,cache_key_sha256) DO UPDATE SET semantic_response=EXCLUDED.semantic_response,expires_at_unix_ms=EXCLUDED.expires_at_unix_ms,created_at=now() WHERE prediction_cache.expires_at_unix_ms<$7")
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
            let row = sqlx::query("SELECT request_digest,status,generation,receipt FROM decision_receipts WHERE tenant_id=$1 AND project_id=$2 AND environment=$3 AND idempotency_key=$4")
                .bind(&scope.tenant_id).bind(&scope.project_id).bind(&scope.environment).bind(key)
                .fetch_one(&mut *tx).await.map_err(|_| RuntimeError::Store)?;
            let prior: String = row.get("request_digest");
            if prior != digest {
                return Ok(Claim::Conflict);
            }
            if row.get::<String, _>("status") == "retryable" {
                let generation: i64 = row.get("generation");
                let updated=sqlx::query("UPDATE decision_receipts SET status='pending',generation=generation+1,created_at=now() WHERE tenant_id=$1 AND project_id=$2 AND environment=$3 AND idempotency_key=$4 AND request_digest=$5 AND status='retryable' AND generation=$6")
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
            Ok(Claim::Existing(Box::new(
                serde_json::from_value(value).map_err(|_| RuntimeError::Store)?,
            )))
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
            let claim=sqlx::query("SELECT status,generation FROM decision_receipts WHERE tenant_id=$1 AND project_id=$2 AND environment=$3 AND idempotency_key=$4 FOR UPDATE")
                .bind(&scope.tenant_id).bind(&scope.project_id).bind(&scope.environment).bind(key)
                .fetch_one(&mut *tx).await.map_err(|_|RuntimeError::Store)?;
            if claim.get::<String, _>("status") != "pending"
                || claim.get::<i64, _>("generation") != generation
            {
                return Err(RuntimeError::Store);
            }
            let updated = sqlx::query("UPDATE budget_buckets SET held_nano_usd=held_nano_usd+$4 WHERE tenant_id=$1 AND project_id=$2 AND purpose=$3 AND cap_nano_usd-spent_nano_usd-held_nano_usd >= $4")
                .bind(&scope.tenant_id).bind(&scope.project_id).bind(&self.purpose).bind(amount)
                .execute(&mut *tx).await.map_err(|_| RuntimeError::Store)?;
            if updated.rows_affected() == 0 {
                return Ok(Reserve::Denied);
            }
            sqlx::query("INSERT INTO provider_attempts (tenant_id,project_id,environment,idempotency_key,attempt_no,purpose,reserved_nano_usd,status,provider_id,model_id) VALUES ($1,$2,$3,$4,1,$5,$6,'dispatch_intent',$7,$8)")
                .bind(&scope.tenant_id).bind(&scope.project_id).bind(&scope.environment).bind(key).bind(&self.purpose).bind(amount).bind(provider_id).bind(model_id)
                .execute(&mut *tx).await.map_err(|_| RuntimeError::Store)?;
            tx.commit().await.map_err(|_| RuntimeError::Store)?;
            Ok(Reserve::Granted)
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
            let mut tx = scoped_transaction(&self.pool, scope).await?;
            let generation = i64::try_from(generation).map_err(|_| RuntimeError::Store)?;
            let (decision_type, version) = receipt
                .release
                .rsplit_once('@')
                .ok_or(RuntimeError::Release)?;
            if decision_type != self.purpose {
                return Err(RuntimeError::Release);
            }
            let release=sqlx::query("SELECT content_hash_sha256, revoked_at IS NOT NULL AS revoked FROM behavior_releases WHERE tenant_id=$1 AND project_id=$2 AND environment=$3 AND decision_type=$4 AND version=$5 FOR SHARE")
                .bind(&scope.tenant_id).bind(&scope.project_id).bind(&scope.environment).bind(decision_type).bind(version)
                .fetch_one(&mut *tx).await.map_err(|_|RuntimeError::Store)?;
            if release.get::<String, _>("content_hash_sha256") != receipt.release_digest_sha256 {
                return Err(RuntimeError::Release);
            }
            let mut committed = receipt.clone();
            if release.get::<bool, _>("revoked") {
                committed.disposition = Disposition::Deny;
                committed.reason_codes.clear();
                committed.reason_codes.push(ReasonCode::RevokedRelease);
            }
            if receipt.attempts > 0 {
                let updated=sqlx::query("UPDATE provider_attempts SET status='uncertain' WHERE tenant_id=$1 AND project_id=$2 AND environment=$3 AND idempotency_key=$4 AND attempt_no=1 AND status='dispatch_intent'")
                    .bind(&scope.tenant_id).bind(&scope.project_id).bind(&scope.environment).bind(key)
                    .execute(&mut *tx).await.map_err(|_| RuntimeError::Store)?;
                if updated.rows_affected() != 1 {
                    return Err(RuntimeError::Store);
                }
            }
            let value = serde_json::to_value(&committed).map_err(|_| RuntimeError::Store)?;
            let updated = sqlx::query("UPDATE decision_receipts SET status='committed',receipt=$7 WHERE tenant_id=$1 AND project_id=$2 AND environment=$3 AND idempotency_key=$4 AND request_digest=$5 AND generation=$6 AND status='pending'")
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
