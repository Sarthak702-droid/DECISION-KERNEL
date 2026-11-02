use super::*;
use kernel_runtime::connections as registry;
use serde::Deserialize;

fn constant_time_equal(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |diff, (a, b)| diff | (a ^ b)) == 0
}
async fn admin(headers: &HeaderMap, state: &AppState) -> Result<Scope, StatusCode> {
    let scope = authenticated_scope(headers, state).await?;
    let expected = env::var("KERNEL_CONNECTION_ADMIN_TOKEN").map_err(|_| StatusCode::FORBIDDEN)?;
    let supplied = headers
        .get("x-kernel-admin-token")
        .and_then(|v| v.to_str().ok())
        .ok_or(StatusCode::FORBIDDEN)?;
    if expected.len() < 32
        || !constant_time_equal(
            &Sha256::digest(expected.as_bytes()),
            &Sha256::digest(supplied.as_bytes()),
        )
    {
        return Err(StatusCode::FORBIDDEN);
    }
    Ok(scope)
}
fn status(e: kernel_runtime::RuntimeError) -> StatusCode {
    match e {
        kernel_runtime::RuntimeError::InvalidRequest => StatusCode::BAD_REQUEST,
        kernel_runtime::RuntimeError::Conflict => StatusCode::CONFLICT,
        _ => StatusCode::SERVICE_UNAVAILABLE,
    }
}
fn parse<T: serde::de::DeserializeOwned>(raw: &[u8]) -> Result<T, StatusCode> {
    serde_json::from_value(kernel_core::strict_json(raw).map_err(|_| StatusCode::BAD_REQUEST)?)
        .map_err(|_| StatusCode::BAD_REQUEST)
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Write {
    connection_id: String,
    #[serde(default)]
    tenant_level: bool,
    config: registry::ConnectionConfig,
    api_key: String,
    expected_revision: Option<i64>,
}
pub(super) async fn put(
    State(state): State<AppState>,
    headers: HeaderMap,
    raw: Bytes,
) -> Result<StatusCode, StatusCode> {
    let scope = admin(&headers, &state).await?;
    let b: Write = parse(&raw)?;
    registry::put(
        &state.store.pool,
        &scope,
        &b.connection_id,
        b.tenant_level,
        b.config,
        b.api_key,
        b.expected_revision,
    )
    .await
    .map_err(status)?;
    Ok(StatusCode::NO_CONTENT)
}
pub(super) async fn list(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, StatusCode> {
    let scope = admin(&headers, &state).await?;
    Ok(Json(
        serde_json::to_value(
            registry::list(&state.store.pool, &scope)
                .await
                .map_err(status)?,
        )
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?,
    ))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Delete {
    #[serde(default)]
    tenant_level: bool,
    expected_revision: i64,
}
pub(super) async fn delete(
    State(state): State<AppState>,
    axum::extract::Path(id): axum::extract::Path<String>,
    headers: HeaderMap,
    raw: Bytes,
) -> Result<StatusCode, StatusCode> {
    let scope = admin(&headers, &state).await?;
    let b: Delete = parse(&raw)?;
    registry::delete(
        &state.store.pool,
        &scope,
        &id,
        b.tenant_level,
        b.expected_revision,
    )
    .await
    .map_err(status)?;
    Ok(StatusCode::NO_CONTENT)
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Bind {
    connection_id: String,
    #[serde(default)]
    tenant_level: bool,
}
pub(super) async fn bind(
    State(state): State<AppState>,
    axum::extract::Path(decision): axum::extract::Path<String>,
    headers: HeaderMap,
    raw: Bytes,
) -> Result<StatusCode, StatusCode> {
    let scope = admin(&headers, &state).await?;
    let b: Bind = parse(&raw)?;
    registry::bind(
        &state.store.pool,
        &scope,
        &decision,
        &b.connection_id,
        b.tenant_level,
    )
    .await
    .map_err(status)?;
    Ok(StatusCode::NO_CONTENT)
}
pub(super) async fn unbind(
    State(state): State<AppState>,
    axum::extract::Path(decision): axum::extract::Path<String>,
    headers: HeaderMap,
) -> Result<StatusCode, StatusCode> {
    let scope = admin(&headers, &state).await?;
    let mut tx = scoped_transaction(&state.store.pool, &scope)
        .await
        .map_err(status)?;
    sqlx::query("DELETE FROM provider_bindings WHERE tenant_id=$1 AND project_id=$2 AND environment=$3 AND product_id=$4 AND decision_type=$5")
        .bind(&scope.tenant_id).bind(&scope.project_id).bind(&scope.environment).bind(&scope.product_id).bind(decision).execute(&mut *tx).await.map_err(|_|StatusCode::SERVICE_UNAVAILABLE)?;
    tx.commit()
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    Ok(StatusCode::NO_CONTENT)
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Test {
    #[serde(default)]
    tenant_level: bool,
    idempotency_key: String,
}
/// A live probe uses the normal durable budget/attempt/receipt flow. It cannot
/// grant semantic acceptance or silently spend outside the scoped ledger.
pub(super) async fn test(
    State(state): State<AppState>,
    axum::extract::Path(id): axum::extract::Path<String>,
    headers: HeaderMap,
    raw: Bytes,
) -> Result<Json<Value>, StatusCode> {
    let scope = admin(&headers, &state).await?;
    let b: Test = parse(&raw)?;
    if b.idempotency_key.is_empty() || b.idempotency_key.len() > 80 {
        return Err(StatusCode::BAD_REQUEST);
    }
    let provider = registry::load(
        &state.store.pool,
        &scope,
        &id,
        if b.tenant_level {
            ""
        } else {
            &scope.project_id
        },
    )
    .await
    .map_err(status)?;
    let capabilities = serde_json::to_value(provider.capabilities())
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    let permit = state
        .request_permits
        .clone()
        .try_acquire_owned()
        .map_err(|_| StatusCode::TOO_MANY_REQUESTS)?;
    let pool = state.store.pool.clone();
    let permits = state.provider_permits.clone();
    let key = format!("connection-test-{}", b.idempotency_key);
    let receipt = tokio::spawn(async move {
        let _permit = permit;
        registry::probe(
            pool,
            scope,
            provider,
            id,
            b.tenant_level,
            b.idempotency_key,
            permits,
        )
        .await
    })
    .await
    .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?
    .map_err(status)?;
    let connected = matches!(
        receipt.answers.get("connected"),
        Some(kernel_core::Answer::Binary {
            value: Some(true),
            ..
        })
    );
    Ok(Json(
        serde_json::json!({"connected":connected,"capabilities":capabilities,"requested_model":receipt.model_id,"served_model":receipt.returned_model_id,"receipt_id":receipt.receipt_id,"idempotency_key":key,"reason_codes":receipt.reason_codes}),
    ))
}
