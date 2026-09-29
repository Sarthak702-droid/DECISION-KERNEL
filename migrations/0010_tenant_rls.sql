DO $$ BEGIN
  IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'kernel_runtime') THEN
    CREATE ROLE kernel_runtime NOLOGIN NOBYPASSRLS;
  END IF;
END $$;

DO $$
DECLARE entry record;
DECLARE scope_check text;
BEGIN
  FOR entry IN
    SELECT * FROM (VALUES
      ('decision_receipts', true),
      ('behavior_releases', true),
      ('release_pointers', true),
      ('evidence', true),
      ('budget_buckets', false),
      ('provider_attempts', true),
      ('prediction_cache', true),
      ('service_tokens', true),
      ('provider_qualifications', true),
      ('decision_reviews', true),
      ('decision_outcomes', true)
    ) AS scoped(table_name, has_environment)
  LOOP
    scope_check := 'tenant_id = current_setting(''kernel.tenant_id'', true) '
      || 'AND project_id = current_setting(''kernel.project_id'', true)';
    IF entry.has_environment THEN
      scope_check := scope_check
        || ' AND environment = current_setting(''kernel.environment'', true)';
    END IF;
    EXECUTE format('ALTER TABLE %I ENABLE ROW LEVEL SECURITY', entry.table_name);
    EXECUTE format('DROP POLICY IF EXISTS kernel_scope ON %I', entry.table_name);
    EXECUTE format(
      'CREATE POLICY kernel_scope ON %I FOR ALL TO kernel_runtime USING (%s) WITH CHECK (%s)',
      entry.table_name, scope_check, scope_check
    );
  END LOOP;
END $$;

CREATE OR REPLACE FUNCTION kernel_authenticate_token(token_hash text)
RETURNS TABLE(tenant_id text, project_id text, environment text, product_id text)
LANGUAGE sql SECURITY DEFINER SET search_path = pg_catalog, public AS $$
  SELECT s.tenant_id, s.project_id, s.environment, s.product_id
  FROM public.service_tokens AS s
  WHERE s.token_hash_sha256 = token_hash AND s.active
$$;
REVOKE ALL ON FUNCTION kernel_authenticate_token(text) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION kernel_authenticate_token(text) TO kernel_runtime;

GRANT SELECT, INSERT, UPDATE ON decision_receipts, evidence, provider_attempts,
  prediction_cache, decision_reviews, decision_outcomes TO kernel_runtime;
GRANT SELECT, UPDATE ON budget_buckets TO kernel_runtime;
GRANT SELECT ON behavior_releases, release_pointers, provider_qualifications TO kernel_runtime;
