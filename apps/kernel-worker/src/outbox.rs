use reqwest::{Client, Url};
use serde_json::Value;
use sqlx::{PgPool, Row};
use std::{
    env,
    time::{SystemTime, UNIX_EPOCH},
};

struct Event {
    id: String,
    tenant: String,
    project: String,
    environment: String,
    kind: String,
    payload: Value,
    generation: i64,
    attempts: i32,
}

pub struct Sink {
    client: Client,
    url: Url,
    token: Option<String>,
    worker_id: String,
}

impl Sink {
    pub fn from_env() -> Result<Option<Self>, Box<dyn std::error::Error>> {
        let Ok(raw_url) = env::var("KERNEL_OUTBOX_URL") else {
            return Ok(None);
        };
        let url = Url::parse(&raw_url)?;
        let allowed_host = env::var("KERNEL_OUTBOX_ALLOWED_HOST")?;
        if url.scheme() != "https"
            || url.host_str() != Some(allowed_host.as_str())
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err("outbox URL must be HTTPS on the configured allowed host".into());
        }
        let client = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(std::time::Duration::from_secs(10))
            .build()?;
        let started = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
        Ok(Some(Self {
            client,
            url,
            token: env::var("KERNEL_OUTBOX_TOKEN").ok(),
            worker_id: format!("{}-{started}", std::process::id()),
        }))
    }

    pub async fn drain_once(&self, pool: &PgPool) -> Result<bool, Box<dyn std::error::Error>> {
        let Some(event) = lease(pool, &self.worker_id, None).await? else {
            return Ok(false);
        };
        let request = self.client.post(self.url.clone())
            .header("Idempotency-Key", &event.id)
            .json(&serde_json::json!({
                "event_id": event.id,
                "event_type": event.kind,
                "scope": {"tenant_id":event.tenant,"project_id":event.project,"environment":event.environment},
                "payload": event.payload,
            }));
        let request = if let Some(token) = &self.token {
            request.bearer_auth(token)
        } else {
            request
        };
        let result = match request.send().await {
            Ok(response) if response.status().is_success() => Ok(()),
            Ok(response) => Err(format!("HTTP {}", response.status().as_u16())),
            Err(_) => Err("network_or_timeout".to_owned()),
        };
        if !finish(pool, &self.worker_id, &event, result).await? {
            eprintln!("outbox event {} lost its lease before completion", event.id);
        }
        Ok(true)
    }
}

async fn lease(
    pool: &PgPool,
    worker_id: &str,
    event_filter: Option<&str>,
) -> Result<Option<Event>, sqlx::Error> {
    let mut tx = pool.begin().await?;
    let row = sqlx::query(
        "WITH next AS (
            SELECT event_id FROM outbox_events
            WHERE ((status='pending' AND available_at<=now())
               OR (status='leased' AND lease_until<=now()))
              AND ($2::text IS NULL OR event_id=$2)
            ORDER BY created_at FOR UPDATE SKIP LOCKED LIMIT 1
        ) UPDATE outbox_events AS o
        SET status='leased',lease_owner=$1,lease_generation=o.lease_generation+1,
            lease_until=now()+interval '30 seconds',attempt_count=o.attempt_count+1
        FROM next WHERE o.event_id=next.event_id
        RETURNING o.event_id,o.tenant_id,o.project_id,o.environment,o.event_type,
                  o.payload,o.lease_generation,o.attempt_count",
    )
    .bind(worker_id)
    .bind(event_filter)
    .fetch_optional(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(row.map(|row| Event {
        id: row.get("event_id"),
        tenant: row.get("tenant_id"),
        project: row.get("project_id"),
        environment: row.get("environment"),
        kind: row.get("event_type"),
        payload: row.get("payload"),
        generation: row.get("lease_generation"),
        attempts: row.get("attempt_count"),
    }))
}

async fn finish(
    pool: &PgPool,
    worker_id: &str,
    event: &Event,
    result: Result<(), String>,
) -> Result<bool, sqlx::Error> {
    let updated = match result {
        Ok(()) => sqlx::query("UPDATE outbox_events SET status='delivered',delivered_at=now(),lease_owner=NULL,lease_until=NULL,last_error=NULL WHERE event_id=$1 AND lease_owner=$2 AND lease_generation=$3 AND status='leased'")
            .bind(&event.id).bind(worker_id).bind(event.generation).execute(pool).await?,
        Err(error) => {
            let delay = 2_i32.saturating_pow(event.attempts.clamp(1, 6) as u32);
            sqlx::query("UPDATE outbox_events SET status=$4,available_at=now()+make_interval(secs => $5),lease_owner=NULL,lease_until=NULL,last_error=$6 WHERE event_id=$1 AND lease_owner=$2 AND lease_generation=$3 AND status='leased'")
                .bind(&event.id).bind(worker_id).bind(event.generation)
                .bind(if event.attempts >= 8 { "dead_letter" } else { "pending" })
                .bind(delay).bind(error).execute(pool).await?
        }
    };
    Ok(updated.rows_affected() == 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn leased_events_retry_and_fence_stale_workers() {
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
        let id = format!("outbox-test-{unique}");
        sqlx::query("INSERT INTO outbox_events (event_id,tenant_id,project_id,environment,event_type,payload) VALUES ($1,$2,'test','test','decision.completed','{}'::jsonb)")
            .bind(&id).bind(&id).execute(&pool).await.unwrap();
        let first = lease(&pool, "worker-a", Some(&id)).await.unwrap().unwrap();
        assert_eq!(first.generation, 1);
        assert!(lease(&pool, "worker-b", Some(&id)).await.unwrap().is_none());
        assert!(!finish(&pool, "worker-b", &first, Ok(())).await.unwrap());
        assert!(
            finish(&pool, "worker-a", &first, Err("HTTP 503".into()))
                .await
                .unwrap()
        );
        sqlx::query("UPDATE outbox_events SET available_at=now() WHERE event_id=$1")
            .bind(&id)
            .execute(&pool)
            .await
            .unwrap();
        let second = lease(&pool, "worker-b", Some(&id)).await.unwrap().unwrap();
        assert_eq!(second.generation, 2);
        assert!(finish(&pool, "worker-b", &second, Ok(())).await.unwrap());
        let status: String =
            sqlx::query_scalar("SELECT status FROM outbox_events WHERE event_id=$1")
                .bind(&id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(status, "delivered");

        let dead_id = format!("outbox-dead-test-{unique}");
        sqlx::query("INSERT INTO outbox_events (event_id,tenant_id,project_id,environment,event_type,payload,attempt_count) VALUES ($1,$2,'test','test','decision.completed','{}'::jsonb,7)")
            .bind(&dead_id).bind(&dead_id).execute(&pool).await.unwrap();
        let last = lease(&pool, "worker-a", Some(&dead_id))
            .await
            .unwrap()
            .unwrap();
        assert!(
            finish(&pool, "worker-a", &last, Err("HTTP 503".into()))
                .await
                .unwrap()
        );
        let status: String =
            sqlx::query_scalar("SELECT status FROM outbox_events WHERE event_id=$1")
                .bind(&dead_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(status, "dead_letter");
    }
}
