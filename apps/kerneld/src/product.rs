use super::*;
use axum::extract::Path;
use kernel_core::{Answer, validate_answer};
use serde::Deserialize;

pub(super) async fn console() -> impl IntoResponse {
    (
        [
            ("content-type", "text/html; charset=utf-8"),
            (
                "content-security-policy",
                "default-src 'self'; script-src 'self'; style-src 'unsafe-inline'; connect-src 'self'; frame-ancestors 'none'; base-uri 'none'",
            ),
        ],
        include_str!("../../../console/index.html"),
    )
}
pub(super) async fn console_js() -> impl IntoResponse {
    (
        [("content-type", "text/javascript; charset=utf-8")],
        include_str!("../../../console/console.js"),
    )
}
pub(super) async fn openapi() -> impl IntoResponse {
    (
        [("content-type", "application/json")],
        include_str!("../../../api/openapi.json"),
    )
}
pub(super) async fn packs(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, StatusCode> {
    let scope = authenticated_scope(&headers, &state).await?;
    let mut tx = scoped_transaction(&state.store.pool, &scope)
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    let rows = sqlx::query("SELECT r.decision_type,r.version,r.content_hash_sha256,r.revoked_at IS NOT NULL AS revoked,p.version=r.version AS active FROM behavior_releases r LEFT JOIN release_pointers p USING (tenant_id,project_id,environment,decision_type) WHERE r.tenant_id=$1 AND r.project_id=$2 AND r.environment=$3 ORDER BY r.decision_type,r.version LIMIT 200")
        .bind(&scope.tenant_id).bind(&scope.project_id).bind(&scope.environment).fetch_all(&mut *tx).await.map_err(|_|StatusCode::SERVICE_UNAVAILABLE)?;
    Ok(Json(Value::Array(rows.iter().map(|r| serde_json::json!({"decision_type":r.get::<String,_>("decision_type"),"version":r.get::<String,_>("version"),"digest":r.get::<String,_>("content_hash_sha256"),"revoked":r.get::<bool,_>("revoked"),"active":r.get::<Option<bool>,_>("active").unwrap_or(false)})).collect())))
}
async fn reviewer(
    state: &AppState,
    scope: &Scope,
    headers: &HeaderMap,
) -> Result<String, StatusCode> {
    let token = headers
        .get("x-kernel-reviewer-token")
        .and_then(|h| h.to_str().ok())
        .ok_or(StatusCode::FORBIDDEN)?;
    let mut tx = scoped_transaction(&state.store.pool, scope)
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    sqlx::query_scalar(
        "SELECT reviewer_id FROM reviewer_tokens WHERE token_hash_sha256=$1 AND active",
    )
    .bind(format!("{:x}", Sha256::digest(token.as_bytes())))
    .fetch_optional(&mut *tx)
    .await
    .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?
    .ok_or(StatusCode::FORBIDDEN)
}
fn parse<T: serde::de::DeserializeOwned>(raw: &[u8]) -> Result<T, StatusCode> {
    serde_json::from_value(kernel_core::strict_json(raw).map_err(|_| StatusCode::BAD_REQUEST)?)
        .map_err(|_| StatusCode::BAD_REQUEST)
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Review {
    reviewer_id: Option<String>,
    disposition: String,
    notes: Option<String>,
    reason_code: String,
    #[serde(default)]
    corrections: BTreeMap<String, Answer>,
}
fn validate_review(
    body: &Review,
    outputs: &[kernel_core::OutputDefinition],
) -> Result<(), StatusCode> {
    if !matches!(
        body.disposition.as_str(),
        "accept" | "review" | "deny" | "abstain"
    ) || body.reason_code.trim().is_empty()
        || body.reason_code.len() > 128
        || body.notes.as_ref().is_some_and(|n| n.len() > 4096)
    {
        return Err(StatusCode::BAD_REQUEST);
    }
    for (id, answer) in &body.corrections {
        let def = outputs
            .iter()
            .find(|o| o.id() == id)
            .ok_or(StatusCode::BAD_REQUEST)?;
        if !answer.is_evaluated()
            || answer.has_probability()
            || validate_answer(def, answer).is_err()
        {
            return Err(StatusCode::BAD_REQUEST);
        }
    }
    Ok(())
}
pub(super) async fn review(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    raw: Bytes,
) -> Result<StatusCode, StatusCode> {
    let scope = authenticated_scope(&headers, &state).await?;
    let identity = reviewer(&state, &scope, &headers).await?;
    let body: Review = parse(&raw)?;
    if body.reviewer_id.as_ref().is_some_and(|r| r != &identity) {
        return Err(StatusCode::FORBIDDEN);
    }
    let receipt = state
        .store
        .get_receipt(&scope, &id)
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?
        .ok_or(StatusCode::NOT_FOUND)?;
    let mut tx = scoped_transaction(&state.store.pool, &scope)
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    let graph: Value = sqlx::query_scalar(
        "SELECT compiled_graph FROM behavior_releases WHERE content_hash_sha256=$1 LIMIT 1",
    )
    .bind(&receipt.release_digest_sha256)
    .fetch_one(&mut *tx)
    .await
    .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    let graph: CompiledDecisionGraph =
        serde_json::from_value(graph).map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    validate_review(&body, &graph.pack.outputs)?;
    sqlx::query("INSERT INTO decision_reviews (tenant_id,project_id,environment,idempotency_key,reviewer_id,disposition,notes,reason_code,corrections) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9)")
        .bind(&scope.tenant_id).bind(&scope.project_id).bind(&scope.environment).bind(&id).bind(&identity).bind(&body.disposition).bind(&body.notes).bind(&body.reason_code).bind(serde_json::to_value(&body.corrections).map_err(|_|StatusCode::BAD_REQUEST)?).execute(&mut *tx).await.map_err(|_|StatusCode::SERVICE_UNAVAILABLE)?;
    event(&mut tx,&scope,"review.recorded",serde_json::json!({"idempotency_key":id,"reviewer_id":identity,"reason_code":body.reason_code,"corrections":body.corrections,"disposition":body.disposition})).await?;
    tx.commit()
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    Ok(StatusCode::CREATED)
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Outcome {
    outcome: String,
    status: Option<String>,
    verified: Option<bool>,
    verification_ref: Option<String>,
}
fn outcome_status(body: &Outcome) -> Result<&str, StatusCode> {
    let status = body
        .status
        .as_deref()
        .unwrap_or(if body.verified == Some(true) {
            "verified"
        } else {
            "pending"
        });
    if body.outcome.is_empty()
        || body.outcome.len() > 256
        || !matches!(status, "pending" | "verified" | "disputed")
        || body.verified.is_some_and(|v| v != (status == "verified"))
        || body
            .verification_ref
            .as_ref()
            .is_some_and(|r| r.trim().is_empty() || r.len() > 512)
        || (status != "pending" && body.verification_ref.is_none())
    {
        return Err(StatusCode::BAD_REQUEST);
    }
    Ok(status)
}
pub(super) async fn outcome(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    raw: Bytes,
) -> Result<StatusCode, StatusCode> {
    let scope = authenticated_scope(&headers, &state).await?;
    let body: Outcome = parse(&raw)?;
    let status = outcome_status(&body)?;
    let identity = if status != "pending" {
        Some(reviewer(&state, &scope, &headers).await?)
    } else {
        None
    };
    state
        .store
        .get_receipt(&scope, &id)
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?
        .ok_or(StatusCode::NOT_FOUND)?;
    let mut tx = scoped_transaction(&state.store.pool, &scope)
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    sqlx::query("INSERT INTO decision_outcomes (tenant_id,project_id,environment,idempotency_key,outcome,status,verified,verification_ref,reviewer_id) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9)")
        .bind(&scope.tenant_id).bind(&scope.project_id).bind(&scope.environment).bind(&id).bind(&body.outcome).bind(status).bind(status=="verified").bind(&body.verification_ref).bind(&identity).execute(&mut *tx).await.map_err(|_|StatusCode::SERVICE_UNAVAILABLE)?;
    event(&mut tx,&scope,"outcome.observed",serde_json::json!({"idempotency_key":id,"outcome":body.outcome,"status":status,"reviewer_id":identity,"verification_ref":body.verification_ref})).await?;
    tx.commit()
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    Ok(StatusCode::CREATED)
}
async fn event(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    scope: &Scope,
    kind: &str,
    payload: Value,
) -> Result<(), StatusCode> {
    sqlx::query("INSERT INTO outbox_events (event_id,tenant_id,project_id,environment,event_type,payload) VALUES (gen_random_uuid()::text,$1,$2,$3,$4,$5)").bind(&scope.tenant_id).bind(&scope.project_id).bind(&scope.environment).bind(kind).bind(payload).execute(&mut **tx).await.map_err(|_|StatusCode::SERVICE_UNAVAILABLE)?;
    Ok(())
}
async fn history(
    state: AppState,
    headers: HeaderMap,
    id: String,
    reviews: bool,
) -> Result<Json<Value>, StatusCode> {
    let scope = authenticated_scope(&headers, &state).await?;
    state
        .store
        .get_receipt(&scope, &id)
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?
        .ok_or(StatusCode::NOT_FOUND)?;
    let mut tx = scoped_transaction(&state.store.pool, &scope)
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    let query = if reviews {
        "SELECT jsonb_build_object('reviewer_id',reviewer_id,'disposition',disposition,'notes',notes,'reason_code',reason_code,'corrections',corrections,'created_at',created_at) FROM decision_reviews WHERE idempotency_key=$1 ORDER BY created_at DESC LIMIT 200"
    } else {
        "SELECT jsonb_build_object('outcome',outcome,'status',CASE WHEN verified THEN 'verified' ELSE status END,'verification_ref',verification_ref,'reviewer_id',reviewer_id,'created_at',created_at) FROM decision_outcomes WHERE idempotency_key=$1 ORDER BY created_at DESC LIMIT 200"
    };
    let rows: Vec<Value> = sqlx::query_scalar(query)
        .bind(id)
        .fetch_all(&mut *tx)
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    Ok(Json(Value::Array(rows)))
}
pub(super) async fn reviews(
    State(s): State<AppState>,
    h: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>, StatusCode> {
    history(s, h, id, true).await
}
pub(super) async fn outcomes(
    State(s): State<AppState>,
    h: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>, StatusCode> {
    history(s, h, id, false).await
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn corrections_are_typed_labels_and_outcomes_need_proof() {
        let defs = vec![kernel_core::OutputDefinition::Categorical {
            id: "queue".into(),
            vocabulary: vec!["billing".into()],
        }];
        let mut b: Review = serde_json::from_value(serde_json::json!({"disposition":"accept","reason_code":"wrong_queue","corrections":{"queue":{"kind":"categorical","selected_id":"billing"}}})).unwrap();
        assert!(validate_review(&b, &defs).is_ok());
        b.corrections.insert(
            "queue".into(),
            Answer::Categorical {
                selected_id: "unknown".into(),
                probabilities: None,
                probability_provenance: None,
            },
        );
        assert!(validate_review(&b, &defs).is_err());
        let mut o = Outcome {
            outcome: "resolved".into(),
            status: Some("verified".into()),
            verified: None,
            verification_ref: None,
        };
        assert!(outcome_status(&o).is_err());
        o.verification_ref = Some("audit:42".into());
        assert_eq!(outcome_status(&o).unwrap(), "verified");
        o.verified = Some(false);
        assert!(outcome_status(&o).is_err());
    }
}
