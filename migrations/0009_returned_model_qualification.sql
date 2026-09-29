ALTER TABLE provider_qualifications
  ADD COLUMN IF NOT EXISTS returned_model_id text;

CREATE OR REPLACE FUNCTION guard_qualification_returned_model() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
  IF NEW.returned_model_id IS DISTINCT FROM OLD.returned_model_id THEN
    RAISE EXCEPTION 'qualified returned model is immutable';
  END IF;
  RETURN NEW;
END $$;
DROP TRIGGER IF EXISTS provider_qualification_returned_model_guard ON provider_qualifications;
CREATE TRIGGER provider_qualification_returned_model_guard
  BEFORE UPDATE ON provider_qualifications
  FOR EACH ROW EXECUTE FUNCTION guard_qualification_returned_model();
