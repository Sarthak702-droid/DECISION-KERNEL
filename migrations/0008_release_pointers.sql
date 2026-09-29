CREATE TABLE IF NOT EXISTS release_pointers (
  tenant_id text NOT NULL,
  project_id text NOT NULL,
  environment text NOT NULL,
  decision_type text NOT NULL,
  version text NOT NULL,
  content_hash_sha256 text NOT NULL,
  revision bigint NOT NULL CHECK (revision > 0),
  updated_at timestamptz NOT NULL DEFAULT now(),
  PRIMARY KEY (tenant_id, project_id, environment, decision_type),
  FOREIGN KEY (tenant_id, project_id, environment, decision_type, version)
    REFERENCES behavior_releases (tenant_id, project_id, environment, decision_type, version)
);

CREATE OR REPLACE FUNCTION guard_release_pointer() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE release_hash text;
BEGIN
  SELECT content_hash_sha256 INTO release_hash FROM behavior_releases
    WHERE tenant_id=NEW.tenant_id AND project_id=NEW.project_id
      AND environment=NEW.environment AND decision_type=NEW.decision_type
      AND version=NEW.version AND revoked_at IS NULL;
  IF release_hash IS NULL OR release_hash <> NEW.content_hash_sha256 THEN
    RAISE EXCEPTION 'active pointer must reference a published unrevoked release';
  END IF;
  IF TG_OP = 'INSERT' THEN
    IF NEW.revision <> 1 THEN
      RAISE EXCEPTION 'initial release pointer revision must be one';
    END IF;
  ELSE
    IF NEW.tenant_id <> OLD.tenant_id OR NEW.project_id <> OLD.project_id
       OR NEW.environment <> OLD.environment OR NEW.decision_type <> OLD.decision_type THEN
      RAISE EXCEPTION 'release pointer scope is immutable';
    END IF;
    IF NEW.revision <> OLD.revision + 1 THEN
      RAISE EXCEPTION 'release pointer revision must advance by one';
    END IF;
  END IF;
  RETURN NEW;
END $$;
DROP TRIGGER IF EXISTS release_pointer_guard ON release_pointers;
CREATE TRIGGER release_pointer_guard BEFORE INSERT OR UPDATE ON release_pointers
  FOR EACH ROW EXECUTE FUNCTION guard_release_pointer();
