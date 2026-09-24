CREATE TABLE IF NOT EXISTS decision_receipts (
  tenant_id text NOT NULL,
  project_id text NOT NULL,
  environment text NOT NULL,
  idempotency_key text NOT NULL,
  request_digest text NOT NULL,
  status text NOT NULL CHECK (status IN ('pending', 'committed', 'retryable')),
  generation bigint NOT NULL DEFAULT 1 CHECK (generation > 0),
  receipt jsonb,
  created_at timestamptz NOT NULL DEFAULT now(),
  PRIMARY KEY (tenant_id, project_id, environment, idempotency_key)
);
CREATE TABLE IF NOT EXISTS behavior_releases (
  tenant_id text NOT NULL,
  project_id text NOT NULL,
  environment text NOT NULL,
  decision_type text NOT NULL,
  version text NOT NULL,
  content_hash_sha256 text NOT NULL,
  compiled_graph jsonb NOT NULL,
  published_at timestamptz NOT NULL DEFAULT now(),
  revoked_at timestamptz,
  PRIMARY KEY (tenant_id, project_id, environment, decision_type, version)
);
CREATE OR REPLACE FUNCTION guard_behavior_release() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
  IF TG_OP = 'DELETE' THEN RAISE EXCEPTION 'published releases cannot be deleted'; END IF;
  IF NEW.tenant_id <> OLD.tenant_id OR NEW.project_id <> OLD.project_id OR NEW.environment <> OLD.environment
     OR NEW.decision_type <> OLD.decision_type OR NEW.version <> OLD.version
     OR NEW.content_hash_sha256 <> OLD.content_hash_sha256 OR NEW.compiled_graph <> OLD.compiled_graph
     OR NEW.published_at <> OLD.published_at OR (OLD.revoked_at IS NOT NULL AND NEW.revoked_at IS DISTINCT FROM OLD.revoked_at)
  THEN RAISE EXCEPTION 'published release content is immutable'; END IF;
  RETURN NEW;
END $$;
DROP TRIGGER IF EXISTS behavior_release_guard ON behavior_releases;
CREATE TRIGGER behavior_release_guard BEFORE UPDATE OR DELETE ON behavior_releases FOR EACH ROW EXECUTE FUNCTION guard_behavior_release();
CREATE TABLE IF NOT EXISTS evidence (
  tenant_id text NOT NULL,
  project_id text NOT NULL,
  environment text NOT NULL,
  evidence_id text NOT NULL,
  revision bigint NOT NULL CHECK (revision >= 0),
  observed_at_unix_ms bigint NOT NULL,
  expires_at_unix_ms bigint,
  digest_sha256 text NOT NULL,
  sensitivity text NOT NULL,
  value jsonb NOT NULL,
  PRIMARY KEY (tenant_id, project_id, environment, evidence_id, revision)
);
CREATE OR REPLACE FUNCTION guard_evidence_revision() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
  RAISE EXCEPTION 'evidence revisions cannot be updated';
END $$;
DROP TRIGGER IF EXISTS evidence_revision_guard ON evidence;
CREATE TRIGGER evidence_revision_guard BEFORE UPDATE ON evidence FOR EACH ROW EXECUTE FUNCTION guard_evidence_revision();
CREATE TABLE IF NOT EXISTS budget_buckets (
  tenant_id text NOT NULL,
  project_id text NOT NULL,
  purpose text NOT NULL,
  cap_nano_usd bigint NOT NULL CHECK (cap_nano_usd >= 0),
  spent_nano_usd bigint NOT NULL DEFAULT 0 CHECK (spent_nano_usd >= 0),
  held_nano_usd bigint NOT NULL DEFAULT 0 CHECK (held_nano_usd >= 0),
  PRIMARY KEY (tenant_id, project_id, purpose)
);
CREATE TABLE IF NOT EXISTS provider_attempts (
  tenant_id text NOT NULL,
  project_id text NOT NULL,
  environment text NOT NULL,
  idempotency_key text NOT NULL,
  attempt_no smallint NOT NULL,
  purpose text NOT NULL,
  reserved_nano_usd bigint NOT NULL CHECK (reserved_nano_usd >= 0),
  actual_charge_nano_usd bigint CHECK (actual_charge_nano_usd >= 0),
  reconciliation_ref text,
  status text NOT NULL CHECK (status IN ('dispatch_intent', 'uncertain', 'reconciled')),
  provider_id text NOT NULL,
  model_id text NOT NULL,
  created_at timestamptz NOT NULL DEFAULT now(),
  PRIMARY KEY (tenant_id, project_id, environment, idempotency_key, attempt_no)
);
