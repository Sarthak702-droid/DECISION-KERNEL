//! Scoped BYOK registry. Secrets never occur in serializable metadata or packs.
use crate::{RuntimeError, postgres::scoped_transaction};
use kernel_core::Scope;
use kernel_provider::{
    IntelligenceProvider, OpenAiCompatibleProvider, PricedProvider, SystemOneProvider,
};
use ring::{
    aead,
    rand::{SecureRandom, SystemRandom},
};
use serde::{Deserialize, Serialize};
use sqlx::{PgPool, Row};
use std::sync::Arc;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConnectionConfig {
    pub adapter: String,
    pub endpoint: String,
    pub provider_id: String,
    pub model_id: String,
    #[serde(default)]
    pub allowed_returned_models: Vec<String>,
    pub max_output_tokens: u32,
    pub max_dispatch_nano_usd: u64,
}
impl ConnectionConfig {
    pub fn build(&self, key: String) -> Result<Arc<dyn IntelligenceProvider>, RuntimeError> {
        // Host authority is controlled by deployment operators, never by the caller.
        let host = kernel_provider::endpoint_host(&self.endpoint)
            .map_err(|_| RuntimeError::InvalidRequest)?;
        let allowed = std::env::var("KERNEL_PROVIDER_ALLOWED_HOSTS").unwrap_or_default();
        if !allowed.split(',').any(|h| !h.is_empty() && h == host)
            || self.max_dispatch_nano_usd == 0
            || self.max_dispatch_nano_usd > i64::MAX as u64
            || self.provider_id.len() > 128
            || self.model_id.len() > 256
            || self.allowed_returned_models.iter().any(|s| s.len() > 256)
            || key.len() > 8192
            || key.len() < 8
            || !key.bytes().all(|b| b.is_ascii_graphic())
            || kernel_provider::contains_secret(
                &serde_json::to_value(self).map_err(|_| RuntimeError::InvalidRequest)?,
                &key,
            )
        {
            return Err(RuntimeError::InvalidRequest);
        }
        let inner: Arc<dyn IntelligenceProvider> = match self.adapter.as_str() {
            "compatible" => Arc::new(
                OpenAiCompatibleProvider::new(
                    &self.endpoint,
                    &host,
                    key.clone(),
                    self.provider_id.clone(),
                    self.model_id.clone(),
                    self.max_output_tokens,
                )
                .and_then(|p| p.with_allowed_returned_models(self.allowed_returned_models.clone()))
                .map_err(|_| RuntimeError::InvalidRequest)?,
            ),
            "system-one" => Arc::new(
                SystemOneProvider::new(
                    &self.endpoint,
                    &host,
                    key.clone(),
                    self.provider_id.clone(),
                    self.model_id.clone(),
                    self.max_output_tokens,
                )
                .and_then(|p| p.with_allowed_returned_models(self.allowed_returned_models.clone()))
                .map_err(|_| RuntimeError::InvalidRequest)?,
            ),
            _ => return Err(RuntimeError::InvalidRequest),
        };
        Ok(Arc::new(kernel_provider::RedactedProvider::new(
            Arc::new(PricedProvider {
                inner,
                max_dispatch_nano_usd: self.max_dispatch_nano_usd,
                max_request_bytes: 65536,
            }),
            key,
        )))
    }
}
#[derive(Serialize)]
pub struct ConnectionMetadata {
    pub connection_id: String,
    pub project_id: String,
    pub config: ConnectionConfig,
    pub revision: i64,
    pub deleted: bool,
}
fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
}
fn aad(scope: &Scope, project: &str, id: &str) -> Vec<u8> {
    serde_json::to_vec(&(&scope.tenant_id, project, &scope.environment, id)).expect("string tuple")
}
/// Versioned AES-256-GCM envelope; random 96-bit nonce and scope-bound AAD.
/// The 32-byte master key lives outside PostgreSQL and is never serialized.
pub struct KeyVault(aead::LessSafeKey);
impl KeyVault {
    pub fn from_env() -> Result<Self, RuntimeError> {
        let raw = std::env::var("KERNEL_CONNECTION_MASTER_KEY").map_err(|_| RuntimeError::Store)?;
        if raw.len() != 64 || !raw.is_ascii() {
            return Err(RuntimeError::Store);
        }
        let mut key = [0u8; 32];
        for (i, byte) in key.iter_mut().enumerate() {
            *byte =
                u8::from_str_radix(&raw[i * 2..i * 2 + 2], 16).map_err(|_| RuntimeError::Store)?;
        }
        Self::new(&key)
    }
    fn new(key: &[u8; 32]) -> Result<Self, RuntimeError> {
        Ok(Self(aead::LessSafeKey::new(
            aead::UnboundKey::new(&aead::AES_256_GCM, key).map_err(|_| RuntimeError::Store)?,
        )))
    }
    fn seal(&self, secret: &str, context: &[u8]) -> Result<Vec<u8>, RuntimeError> {
        let mut nonce = [0u8; 12];
        SystemRandom::new()
            .fill(&mut nonce)
            .map_err(|_| RuntimeError::Store)?;
        let mut body = secret.as_bytes().to_vec();
        self.0
            .seal_in_place_append_tag(
                aead::Nonce::assume_unique_for_key(nonce),
                aead::Aad::from(context),
                &mut body,
            )
            .map_err(|_| RuntimeError::Store)?;
        let mut envelope = vec![1];
        envelope.extend(nonce);
        envelope.extend(body);
        Ok(envelope)
    }
    fn open(&self, envelope: &[u8], context: &[u8]) -> Result<String, RuntimeError> {
        if envelope.len() < 29 || envelope[0] != 1 {
            return Err(RuntimeError::Store);
        }
        let nonce: [u8; 12] = envelope[1..13]
            .try_into()
            .map_err(|_| RuntimeError::Store)?;
        let mut body = envelope[13..].to_vec();
        let plain = self
            .0
            .open_in_place(
                aead::Nonce::assume_unique_for_key(nonce),
                aead::Aad::from(context),
                &mut body,
            )
            .map_err(|_| RuntimeError::Store)?;
        String::from_utf8(plain.to_vec()).map_err(|_| RuntimeError::Store)
    }
}
pub async fn put(
    pool: &PgPool,
    scope: &Scope,
    id: &str,
    tenant_level: bool,
    config: ConnectionConfig,
    key: String,
    expected_revision: Option<i64>,
) -> Result<(), RuntimeError> {
    if !valid_id(id) {
        return Err(RuntimeError::InvalidRequest);
    }
    config.build(key.clone())?;
    let project = if tenant_level { "" } else { &scope.project_id };
    let encrypted = KeyVault::from_env()?.seal(&key, &aad(scope, project, id))?;
    let mut tx = scoped_transaction(pool, scope).await?;
    let config = serde_json::to_value(config).map_err(|_| RuntimeError::InvalidRequest)?;
    let n=if let Some(revision)=expected_revision {
        sqlx::query("UPDATE provider_connections SET config=$5,encrypted_key=$6,revision=revision+1,updated_at=now() WHERE tenant_id=$1 AND project_id=$2 AND environment=$3 AND connection_id=$4 AND revision=$7 AND NOT deleted")
            .bind(&scope.tenant_id).bind(project).bind(&scope.environment).bind(id).bind(config).bind(encrypted).bind(revision).execute(&mut *tx).await
    } else {
        sqlx::query("INSERT INTO provider_connections (tenant_id,project_id,environment,connection_id,config,encrypted_key) VALUES ($1,$2,$3,$4,$5,$6) ON CONFLICT DO NOTHING")
            .bind(&scope.tenant_id).bind(project).bind(&scope.environment).bind(id).bind(config).bind(encrypted).execute(&mut *tx).await
    }.map_err(|_| RuntimeError::Store)?.rows_affected();
    if n != 1 {
        return Err(RuntimeError::Conflict);
    }
    tx.commit().await.map_err(|_| RuntimeError::Store)
}
pub async fn list(pool: &PgPool, scope: &Scope) -> Result<Vec<ConnectionMetadata>, RuntimeError> {
    let mut tx = scoped_transaction(pool, scope).await?;
    let rows=sqlx::query("SELECT connection_id,project_id,config,revision,deleted FROM provider_connections WHERE tenant_id=$1 AND (project_id=$2 OR project_id='') AND environment=$3 ORDER BY project_id,connection_id")
        .bind(&scope.tenant_id).bind(&scope.project_id).bind(&scope.environment).fetch_all(&mut *tx).await.map_err(|_| RuntimeError::Store)?;
    rows.into_iter()
        .map(|r| {
            Ok(ConnectionMetadata {
                connection_id: r.get("connection_id"),
                project_id: r.get("project_id"),
                config: serde_json::from_value(r.get("config")).map_err(|_| RuntimeError::Store)?,
                revision: r.get("revision"),
                deleted: r.get("deleted"),
            })
        })
        .collect()
}
pub async fn delete(
    pool: &PgPool,
    scope: &Scope,
    id: &str,
    tenant_level: bool,
    revision: i64,
) -> Result<(), RuntimeError> {
    let mut tx = scoped_transaction(pool, scope).await?;
    let n=sqlx::query("UPDATE provider_connections SET encrypted_key=NULL,deleted=true,revision=revision+1,updated_at=now() WHERE tenant_id=$1 AND project_id=$2 AND environment=$3 AND connection_id=$4 AND revision=$5 AND NOT deleted")
        .bind(&scope.tenant_id).bind(if tenant_level { "" } else { &scope.project_id }).bind(&scope.environment).bind(id).bind(revision).execute(&mut *tx).await.map_err(|_| RuntimeError::Store)?.rows_affected();
    if n != 1 {
        return Err(RuntimeError::Conflict);
    }
    tx.commit().await.map_err(|_| RuntimeError::Store)
}
pub async fn load(
    pool: &PgPool,
    scope: &Scope,
    id: &str,
    project: &str,
) -> Result<Arc<dyn IntelligenceProvider>, RuntimeError> {
    if !project.is_empty() && project != scope.project_id {
        return Err(RuntimeError::InvalidRequest);
    }
    let mut tx = scoped_transaction(pool, scope).await?;
    let r=sqlx::query("SELECT config,encrypted_key FROM provider_connections WHERE tenant_id=$1 AND project_id=$2 AND environment=$3 AND connection_id=$4 AND NOT deleted")
        .bind(&scope.tenant_id).bind(project).bind(&scope.environment).bind(id).fetch_optional(&mut *tx).await.map_err(|_| RuntimeError::Store)?.ok_or(RuntimeError::Store)?;
    let config: ConnectionConfig =
        serde_json::from_value(r.get("config")).map_err(|_| RuntimeError::Store)?;
    let key = KeyVault::from_env()?.open(
        &r.get::<Vec<u8>, _>("encrypted_key"),
        &aad(scope, project, id),
    )?;
    config.build(key)
}
/// Explicit bindings fail closed on deletion, missing vault or invalid config.
pub async fn resolve(
    pool: &PgPool,
    scope: &Scope,
    decision_type: &str,
    fallback: Option<Arc<dyn IntelligenceProvider>>,
) -> Result<Option<Arc<dyn IntelligenceProvider>>, RuntimeError> {
    let mut tx = scoped_transaction(pool, scope).await?;
    let row=sqlx::query("SELECT connection_id,connection_project_id FROM provider_bindings WHERE tenant_id=$1 AND project_id=$2 AND environment=$3 AND product_id=$4 AND decision_type=$5")
        .bind(&scope.tenant_id).bind(&scope.project_id).bind(&scope.environment).bind(&scope.product_id).bind(decision_type).fetch_optional(&mut *tx).await.map_err(|_| RuntimeError::Store)?;
    tx.commit().await.map_err(|_| RuntimeError::Store)?;
    match row {
        Some(r) => Ok(Some(
            load(
                pool,
                scope,
                &r.get::<String, _>("connection_id"),
                &r.get::<String, _>("connection_project_id"),
            )
            .await?,
        )),
        None => Ok(fallback),
    }
}
pub async fn bind(
    pool: &PgPool,
    scope: &Scope,
    decision_type: &str,
    id: &str,
    tenant_level: bool,
) -> Result<(), RuntimeError> {
    if !valid_id(decision_type) {
        return Err(RuntimeError::InvalidRequest);
    }
    let project = if tenant_level { "" } else { &scope.project_id };
    load(pool, scope, id, project).await?;
    let mut tx = scoped_transaction(pool, scope).await?;
    // Lock the connection against concurrent deletion while binding.
    let exists:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM provider_connections WHERE tenant_id=$1 AND project_id=$2 AND environment=$3 AND connection_id=$4 AND NOT deleted FOR SHARE)")
        .bind(&scope.tenant_id).bind(project).bind(&scope.environment).bind(id).fetch_one(&mut *tx).await.map_err(|_|RuntimeError::Store)?;
    if !exists {
        return Err(RuntimeError::Conflict);
    }
    sqlx::query("INSERT INTO provider_bindings (tenant_id,project_id,environment,product_id,decision_type,connection_project_id,connection_id) VALUES ($1,$2,$3,$4,$5,$6,$7) ON CONFLICT (tenant_id,project_id,environment,product_id,decision_type) DO UPDATE SET connection_project_id=EXCLUDED.connection_project_id,connection_id=EXCLUDED.connection_id")
        .bind(&scope.tenant_id).bind(&scope.project_id).bind(&scope.environment).bind(&scope.product_id).bind(decision_type).bind(project).bind(id).execute(&mut *tx).await.map_err(|_| RuntimeError::Store)?;
    tx.commit().await.map_err(|_| RuntimeError::Store)
}

struct ProbeStore(crate::postgres::PostgresStore);
macro_rules! delegate {
    ($name:ident ($($arg:ident : $ty:ty),*) -> $result:ty) => {
        fn $name<'a>(&'a self, $($arg: $ty),*) -> std::pin::Pin<Box<dyn std::future::Future<Output=$result>+Send+'a>> {
            self.0.$name($($arg),*)
        }
    };
}
impl crate::ReceiptStore for ProbeStore {
    delegate!(claim(scope: &'a Scope, key: &'a str, digest: &'a str) -> Result<crate::Claim,RuntimeError>);
    delegate!(save_recovery(scope: &'a Scope, key: &'a str, generation: u64, receipt: &'a kernel_core::DecisionReceipt) -> Result<(),RuntimeError>);
    delegate!(reserve(scope: &'a Scope, key: &'a str, generation: u64, amount: u64, provider_id: &'a str, model_id: &'a str) -> Result<crate::Reserve,RuntimeError>);
    delegate!(cached_prediction(scope: &'a Scope, key: &'a str, now_ms: u64) -> Result<Option<kernel_provider::SemanticResponse>,RuntimeError>);
    delegate!(record_prediction(scope: &'a Scope, key: &'a str, response: &'a kernel_provider::SemanticResponse, expires_at_unix_ms: u64) -> Result<(),RuntimeError>);
    delegate!(qualification_active(scope: &'a Scope, qualification_ref: &'a str, release_hash: &'a str, identity: &'a kernel_provider::ProviderIdentity, returned_model_id: &'a str, now_ms: u64) -> Result<bool,RuntimeError>);
    fn commit<'a>(
        &'a self,
        scope: &'a Scope,
        key: &'a str,
        digest: &'a str,
        generation: u64,
        receipt: &'a kernel_core::DecisionReceipt,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = Result<kernel_core::DecisionReceipt, RuntimeError>>
                + Send
                + 'a,
        >,
    > {
        self.0
            .commit_receipt(scope, key, digest, generation, receipt, true)
    }
}
/// Run a review-only operational probe through the durable accounting ledger.
/// Its graph is internal, so it cannot publish or approve a decision release.
pub async fn probe(
    pool: PgPool,
    scope: Scope,
    provider: Arc<dyn IntelligenceProvider>,
    id: String,
    tenant_level: bool,
    nonce: String,
    permits: Arc<tokio::sync::Semaphore>,
) -> Result<kernel_core::DecisionReceipt, RuntimeError> {
    let identity = provider.identity();
    let pack:kernel_compiler::DecisionPack=serde_json::from_value(serde_json::json!({
        "api_version":"kernel/v1","kind":"DecisionPack","id":"connection.test","version":"1.0.0",
        "required_evidence":[],"semantic_state_fields":["probe_nonce"],
        "outputs":[{"kind":"binary","id":"connected","proposition":"connection_test_succeeded"}],
        "nodes":[{"id":"probe","op":"semantic_question","output":"connected","prompt":"This is a connection test. Return true for connection_test_succeeded."}],
        "policy":{"accept_qualified_semantic":false},
        "limits":{"deadline_ms":2000,"max_billable_attempts":1,"max_cost_nano_usd":1000000},
        "qualification":{"semantic_acceptance_qualified":false,"provider_id":identity.provider_id,"model_id":identity.model_id,"adapter_version":identity.adapter_version}
    })).map_err(|_|RuntimeError::InvalidRequest)?;
    let graph = Arc::new(kernel_compiler::compile(pack).map_err(|_| RuntimeError::InvalidRequest)?);
    let request = kernel_core::DecisionRequest {
        schema_version: "1.1".into(),
        decision_type: "connection.test".into(),
        release: "connection.test@1.0.0".into(),
        idempotency_key: format!("connection-test-{nonce}"),
        state: std::collections::BTreeMap::from([(
            "probe_nonce".into(),
            serde_json::json!([id, tenant_level, nonce]),
        )]),
        evidence: vec![],
        constraints: kernel_core::RequestLimits::default(),
    };
    crate::Engine {
        graph,
        provider: Some(provider),
        store: Arc::new(ProbeStore(crate::postgres::PostgresStore {
            pool,
            purpose: "connection.test".into(),
        })),
        provider_permits: permits,
        platform_deadline_ms: 2000,
        platform_max_cost_nano_usd: 1000000,
    }
    .decide(scope, request)
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn vault_authenticates_scope_and_ciphertext_and_randomizes_nonce() {
        let vault = KeyVault::new(&[7; 32]).unwrap();
        let a = vault.seal("sk-secret-value", b"tenant-a").unwrap();
        let b = vault.seal("sk-secret-value", b"tenant-a").unwrap();
        assert_ne!(a, b);
        assert!(!a.windows(15).any(|w| w == b"sk-secret-value"));
        assert_eq!(vault.open(&a, b"tenant-a").unwrap(), "sk-secret-value");
        assert!(vault.open(&a, b"tenant-b").is_err());
        assert!(
            KeyVault::new(&[8; 32])
                .unwrap()
                .open(&a, b"tenant-a")
                .is_err()
        );
        let mut corrupt = a.clone();
        corrupt[15] ^= 1;
        assert!(vault.open(&corrupt, b"tenant-a").is_err());
        for len in 0..29 {
            assert!(vault.open(&a[..len], b"tenant-a").is_err());
        }
    }
}
