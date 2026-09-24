CREATE TABLE IF NOT EXISTS service_tokens (
  token_hash_sha256 text PRIMARY KEY,
  tenant_id text NOT NULL,
  project_id text NOT NULL,
  environment text NOT NULL,
  product_id text NOT NULL,
  active boolean NOT NULL DEFAULT true,
  created_at timestamptz NOT NULL DEFAULT now(),
  revoked_at timestamptz
);
CREATE INDEX IF NOT EXISTS service_tokens_scope_idx ON service_tokens (tenant_id,project_id,environment,product_id) WHERE active;
