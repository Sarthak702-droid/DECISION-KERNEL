-- Legacy rows with no provable product are quarantined under the empty product.
-- Re-provision product/environment budgets explicitly; never copy a shared cap.
DO $$ DECLARE t text; BEGIN
  FOREACH t IN ARRAY ARRAY['decision_receipts','provider_attempts','evidence','prediction_cache',
    'budget_buckets','decision_reviews','decision_outcomes','outbox_events'] LOOP
    EXECUTE format('ALTER TABLE %I ADD COLUMN product_id text NOT NULL DEFAULT ''''', t);
    EXECUTE format('ALTER TABLE %I ALTER COLUMN product_id SET DEFAULT current_setting(''kernel.product_id'', true)', t);
  END LOOP;
END $$;
ALTER TABLE budget_buckets ADD COLUMN environment text NOT NULL DEFAULT '';
ALTER TABLE budget_buckets ALTER COLUMN environment SET DEFAULT current_setting('kernel.environment', true);
UPDATE decision_receipts SET product_id=receipt->'scope'->>'product_id'
  WHERE receipt->'scope'->>'product_id' IS NOT NULL;
UPDATE provider_attempts a SET product_id=r.product_id FROM decision_receipts r
  WHERE a.tenant_id=r.tenant_id AND a.project_id=r.project_id AND a.environment=r.environment
    AND a.idempotency_key=r.idempotency_key;
ALTER TABLE decision_receipts DROP CONSTRAINT decision_receipts_pkey;
ALTER TABLE decision_receipts ADD PRIMARY KEY (tenant_id,project_id,environment,product_id,idempotency_key);
ALTER TABLE provider_attempts DROP CONSTRAINT provider_attempts_pkey;
ALTER TABLE provider_attempts ADD PRIMARY KEY (tenant_id,project_id,environment,product_id,idempotency_key,attempt_no);
ALTER TABLE evidence DROP CONSTRAINT evidence_pkey;
ALTER TABLE evidence ADD PRIMARY KEY (tenant_id,project_id,environment,product_id,evidence_id,revision);
ALTER TABLE prediction_cache DROP CONSTRAINT prediction_cache_pkey;
ALTER TABLE prediction_cache ADD PRIMARY KEY (tenant_id,project_id,environment,product_id,cache_key_sha256);
ALTER TABLE budget_buckets DROP CONSTRAINT budget_buckets_pkey;
ALTER TABLE budget_buckets ADD PRIMARY KEY (tenant_id,project_id,environment,product_id,purpose);
-- Preserve attributable legacy holds/spend in disabled product buckets. The old
-- shared bucket remains an inaccessible archival snapshot; its cap is never
-- multiplied across products. An operator must approve new scoped caps.
INSERT INTO budget_buckets (tenant_id,project_id,environment,product_id,purpose,cap_nano_usd,spent_nano_usd,held_nano_usd)
SELECT tenant_id,project_id,environment,product_id,purpose,0,
  COALESCE(sum(actual_charge_nano_usd) FILTER (WHERE status='reconciled'),0),
  COALESCE(sum(reserved_nano_usd) FILTER (WHERE status<>'reconciled'),0)
FROM provider_attempts WHERE product_id<>''
GROUP BY tenant_id,project_id,environment,product_id,purpose;
ALTER TABLE decision_reviews DROP CONSTRAINT decision_reviews_pkey;
ALTER TABLE decision_reviews ADD PRIMARY KEY (tenant_id,project_id,environment,product_id,idempotency_key,reviewer_id,created_at);
ALTER TABLE decision_outcomes DROP CONSTRAINT decision_outcomes_pkey;
ALTER TABLE decision_outcomes ADD PRIMARY KEY (tenant_id,project_id,environment,product_id,idempotency_key,created_at);
DO $$ DECLARE t text; DECLARE boundary text; BEGIN
  boundary := 'tenant_id = current_setting(''kernel.tenant_id'', true) AND project_id = current_setting(''kernel.project_id'', true) AND environment = current_setting(''kernel.environment'', true) AND product_id = current_setting(''kernel.product_id'', true)';
  FOREACH t IN ARRAY ARRAY['decision_receipts','provider_attempts','evidence','prediction_cache',
    'budget_buckets','decision_reviews','decision_outcomes','outbox_events','decision_jobs'] LOOP
    EXECUTE format('DROP POLICY IF EXISTS kernel_scope ON %I',t);
    EXECUTE format('CREATE POLICY kernel_scope ON %I FOR ALL TO kernel_runtime USING (%s) WITH CHECK (%s)',t,boundary,boundary);
  END LOOP;
END $$;
-- Row locking requires UPDATE privilege. Only the existing revocation column is
-- granted: the immutable-release trigger still rejects content mutation.
GRANT UPDATE (revoked_at) ON behavior_releases TO kernel_runtime;

-- The worker is a separate non-owner, non-BYPASSRLS principal. Its queue/recovery
-- role intentionally traverses scopes; API credentials must never inherit it.
DO $$ BEGIN
  IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname='kernel_worker') THEN
    CREATE ROLE kernel_worker NOLOGIN NOBYPASSRLS;
  END IF;
END $$;
GRANT kernel_runtime TO kernel_worker;
GRANT USAGE ON SCHEMA public TO kernel_runtime, kernel_worker;
GRANT SELECT, UPDATE ON decision_jobs, outbox_events TO kernel_worker;
DO $$ DECLARE t text; BEGIN
  FOREACH t IN ARRAY ARRAY['decision_jobs','outbox_events','decision_receipts','provider_attempts','behavior_releases'] LOOP
    EXECUTE format('CREATE POLICY kernel_worker_scope ON %I FOR ALL TO kernel_worker USING (true) WITH CHECK (true)',t);
  END LOOP;
END $$;

ALTER TABLE provider_attempts ADD COLUMN reported_charge_nano_usd numeric(20,0) CHECK (reported_charge_nano_usd >= 0);
ALTER TABLE provider_attempts ADD COLUMN reported_usage jsonb;
