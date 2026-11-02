-- Shared fixed-window admission. Default policy applies when no scoped override exists.
CREATE TABLE operation_limits (
 tenant_id text NOT NULL, project_id text NOT NULL, environment text NOT NULL, product_id text NOT NULL,
 requests_per_minute integer NOT NULL CHECK (requests_per_minute BETWEEN 1 AND 1000000),
 PRIMARY KEY (tenant_id,project_id,environment,product_id)
);
CREATE TABLE operation_windows (
 tenant_id text NOT NULL, project_id text NOT NULL, environment text NOT NULL, product_id text NOT NULL,
 window_start bigint NOT NULL, requests integer NOT NULL,
 PRIMARY KEY (tenant_id,project_id,environment,product_id)
);
CREATE TABLE recorded_requests (
 tenant_id text NOT NULL, project_id text NOT NULL, environment text NOT NULL, product_id text NOT NULL,
 idempotency_key text NOT NULL, request jsonb NOT NULL, created_at timestamptz NOT NULL DEFAULT now(),
 PRIMARY KEY (tenant_id,project_id,environment,product_id,idempotency_key)
);
CREATE TABLE lifecycle_blocks (
 tenant_id text NOT NULL, project_id text NOT NULL, environment text NOT NULL, product_id text NOT NULL,
 blocked_at timestamptz NOT NULL DEFAULT now(),
 PRIMARY KEY (tenant_id,project_id,environment,product_id)
);
DO $$ DECLARE t text; BEGIN
 FOREACH t IN ARRAY ARRAY['operation_limits','operation_windows','recorded_requests','lifecycle_blocks'] LOOP
  EXECUTE format('ALTER TABLE %I ENABLE ROW LEVEL SECURITY',t);
  EXECUTE format('ALTER TABLE %I FORCE ROW LEVEL SECURITY',t);
  EXECUTE format('CREATE POLICY kernel_scope ON %I TO kernel_runtime USING (tenant_id=current_setting(''kernel.tenant_id'',true) AND project_id=current_setting(''kernel.project_id'',true) AND environment=current_setting(''kernel.environment'',true) AND product_id=current_setting(''kernel.product_id'',true)) WITH CHECK (tenant_id=current_setting(''kernel.tenant_id'',true) AND project_id=current_setting(''kernel.project_id'',true) AND environment=current_setting(''kernel.environment'',true) AND product_id=current_setting(''kernel.product_id'',true))',t);
 END LOOP;
END $$;
GRANT SELECT ON operation_limits,lifecycle_blocks,recorded_requests TO kernel_runtime;
GRANT INSERT ON recorded_requests TO kernel_runtime;
GRANT SELECT,INSERT,UPDATE ON operation_windows TO kernel_runtime;
-- Lifecycle deletion is possible only for an owner maintenance transaction.
-- Runtime logins cannot override the append-only guards using a custom GUC.
CREATE OR REPLACE FUNCTION guard_decision_review() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
 IF TG_OP='DELETE' AND current_setting('kernel.lifecycle_delete',true)='on'
 AND pg_has_role(current_user,(SELECT relowner FROM pg_class WHERE oid=TG_RELID),'USAGE') THEN RETURN OLD; END IF;
 RAISE EXCEPTION 'decision review records are append-only';
END $$;
CREATE OR REPLACE FUNCTION guard_decision_outcome() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
 IF TG_OP='DELETE' AND current_setting('kernel.lifecycle_delete',true)='on'
 AND pg_has_role(current_user,(SELECT relowner FROM pg_class WHERE oid=TG_RELID),'USAGE') THEN RETURN OLD; END IF;
 RAISE EXCEPTION 'decision outcome records are append-only';
END $$;
