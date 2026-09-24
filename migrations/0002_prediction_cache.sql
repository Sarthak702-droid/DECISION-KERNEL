CREATE TABLE IF NOT EXISTS prediction_cache (
  tenant_id text NOT NULL,
  project_id text NOT NULL,
  environment text NOT NULL,
  cache_key_sha256 text NOT NULL,
  semantic_response jsonb NOT NULL,
  expires_at_unix_ms bigint NOT NULL,
  created_at timestamptz NOT NULL DEFAULT now(),
  PRIMARY KEY (tenant_id, project_id, environment, cache_key_sha256)
);
CREATE INDEX IF NOT EXISTS prediction_cache_expiry_idx ON prediction_cache (expires_at_unix_ms);
