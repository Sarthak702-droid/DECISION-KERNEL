CREATE TABLE IF NOT EXISTS outbox_events (
  event_id text PRIMARY KEY,
  tenant_id text NOT NULL,
  project_id text NOT NULL,
  environment text NOT NULL,
  event_type text NOT NULL,
  payload jsonb NOT NULL,
  status text NOT NULL DEFAULT 'pending'
    CHECK (status IN ('pending', 'leased', 'delivered', 'dead_letter')),
  attempt_count integer NOT NULL DEFAULT 0 CHECK (attempt_count >= 0),
  available_at timestamptz NOT NULL DEFAULT now(),
  lease_owner text,
  lease_generation bigint NOT NULL DEFAULT 0 CHECK (lease_generation >= 0),
  lease_until timestamptz,
  created_at timestamptz NOT NULL DEFAULT now(),
  delivered_at timestamptz
);
CREATE INDEX IF NOT EXISTS outbox_ready_idx
  ON outbox_events (status, available_at, lease_until, created_at)
  WHERE status IN ('pending', 'leased');

ALTER TABLE outbox_events ENABLE ROW LEVEL SECURITY;
DROP POLICY IF EXISTS kernel_scope ON outbox_events;
CREATE POLICY kernel_scope ON outbox_events FOR ALL TO kernel_runtime
  USING (tenant_id = current_setting('kernel.tenant_id', true)
    AND project_id = current_setting('kernel.project_id', true)
    AND environment = current_setting('kernel.environment', true))
  WITH CHECK (tenant_id = current_setting('kernel.tenant_id', true)
    AND project_id = current_setting('kernel.project_id', true)
    AND environment = current_setting('kernel.environment', true));
GRANT SELECT, INSERT ON outbox_events TO kernel_runtime;
