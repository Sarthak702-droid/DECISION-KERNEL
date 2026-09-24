ALTER TABLE provider_qualifications ADD COLUMN IF NOT EXISTS evaluation_report jsonb;
ALTER TABLE provider_qualifications ADD COLUMN IF NOT EXISTS approval_ref text;
ALTER TABLE provider_qualifications ADD COLUMN IF NOT EXISTS approved_by text;
ALTER TABLE provider_qualifications ADD COLUMN IF NOT EXISTS revoked_at timestamptz;
CREATE OR REPLACE FUNCTION guard_qualification() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
  IF TG_OP='DELETE' THEN RAISE EXCEPTION 'qualification records cannot be deleted'; END IF;
  IF NEW.tenant_id<>OLD.tenant_id OR NEW.project_id<>OLD.project_id OR NEW.environment<>OLD.environment
    OR NEW.qualification_ref<>OLD.qualification_ref OR NEW.release_hash_sha256<>OLD.release_hash_sha256
    OR NEW.provider_id<>OLD.provider_id OR NEW.model_id<>OLD.model_id OR NEW.adapter_version<>OLD.adapter_version
    OR NEW.evaluation_digest_sha256<>OLD.evaluation_digest_sha256 OR NEW.reviewed_sample_count<>OLD.reviewed_sample_count
    OR NEW.valid_until_unix_ms<>OLD.valid_until_unix_ms OR NEW.evaluation_report IS DISTINCT FROM OLD.evaluation_report
    OR NEW.approval_ref IS DISTINCT FROM OLD.approval_ref OR NEW.approved_by IS DISTINCT FROM OLD.approved_by
    OR (OLD.active=false AND NEW.active=true) OR (OLD.revoked_at IS NOT NULL AND NEW.revoked_at IS DISTINCT FROM OLD.revoked_at)
  THEN RAISE EXCEPTION 'qualification evidence is immutable'; END IF;
  RETURN NEW;
END $$;
DROP TRIGGER IF EXISTS provider_qualification_guard ON provider_qualifications;
CREATE TRIGGER provider_qualification_guard BEFORE UPDATE OR DELETE ON provider_qualifications FOR EACH ROW EXECUTE FUNCTION guard_qualification();
