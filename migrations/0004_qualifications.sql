CREATE TABLE IF NOT EXISTS provider_qualifications (
  tenant_id text NOT NULL,
  project_id text NOT NULL,
  environment text NOT NULL,
  qualification_ref text NOT NULL,
  release_hash_sha256 text NOT NULL,
  provider_id text NOT NULL,
  model_id text NOT NULL,
  adapter_version text NOT NULL,
  evaluation_digest_sha256 text NOT NULL,
  reviewed_sample_count bigint NOT NULL CHECK (reviewed_sample_count > 0),
  valid_until_unix_ms bigint NOT NULL,
  active boolean NOT NULL DEFAULT true,
  created_at timestamptz NOT NULL DEFAULT now(),
  PRIMARY KEY (tenant_id,project_id,environment,qualification_ref)
);
