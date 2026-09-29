CREATE TABLE IF NOT EXISTS decision_jobs (
  job_id text PRIMARY KEY,
  tenant_id text NOT NULL,
  project_id text NOT NULL,
  environment text NOT NULL,
  product_id text NOT NULL,
  decision_type text NOT NULL,
  idempotency_key text NOT NULL,
  request_digest_sha256 text NOT NULL,
  request_payload jsonb NOT NULL,
  status text NOT NULL DEFAULT 'queued'
    CHECK (status IN ('queued', 'leased', 'completed', 'dead_letter')),
  lease_owner text,
  lease_generation bigint NOT NULL DEFAULT 0 CHECK (lease_generation >= 0),
  lease_until timestamptz,
  attempt_count integer NOT NULL DEFAULT 0 CHECK (attempt_count >= 0),
  available_at timestamptz NOT NULL DEFAULT now(),
  last_error text,
  created_at timestamptz NOT NULL DEFAULT now(),
  updated_at timestamptz NOT NULL DEFAULT now(),
  UNIQUE (tenant_id, project_id, environment, product_id, idempotency_key)
);
CREATE INDEX IF NOT EXISTS decision_jobs_ready_idx
  ON decision_jobs (status, available_at, lease_until, created_at)
  WHERE status IN ('queued', 'leased');

ALTER TABLE decision_jobs ENABLE ROW LEVEL SECURITY;
DROP POLICY IF EXISTS kernel_scope ON decision_jobs;
CREATE POLICY kernel_scope ON decision_jobs FOR ALL TO kernel_runtime
  USING (tenant_id = current_setting('kernel.tenant_id', true)
    AND project_id = current_setting('kernel.project_id', true)
    AND environment = current_setting('kernel.environment', true))
  WITH CHECK (tenant_id = current_setting('kernel.tenant_id', true)
    AND project_id = current_setting('kernel.project_id', true)
    AND environment = current_setting('kernel.environment', true));
GRANT SELECT, INSERT ON decision_jobs TO kernel_runtime;
