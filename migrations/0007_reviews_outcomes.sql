CREATE TABLE IF NOT EXISTS decision_reviews (
  tenant_id text NOT NULL,
  project_id text NOT NULL,
  environment text NOT NULL,
  idempotency_key text NOT NULL,
  reviewer_id text NOT NULL,
  disposition text NOT NULL CHECK (disposition IN ('accept', 'review', 'deny', 'abstain')),
  notes text,
  created_at timestamptz NOT NULL DEFAULT now(),
  PRIMARY KEY (tenant_id, project_id, environment, idempotency_key, reviewer_id, created_at)
);

CREATE OR REPLACE FUNCTION guard_decision_review() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
  IF TG_OP = 'DELETE' THEN RAISE EXCEPTION 'decision review records cannot be deleted'; END IF;
  RAISE EXCEPTION 'decision review records are append-only';
END $$;
DROP TRIGGER IF EXISTS decision_review_guard ON decision_reviews;
CREATE TRIGGER decision_review_guard BEFORE UPDATE OR DELETE ON decision_reviews FOR EACH ROW EXECUTE FUNCTION guard_decision_review();

CREATE TABLE IF NOT EXISTS decision_outcomes (
  tenant_id text NOT NULL,
  project_id text NOT NULL,
  environment text NOT NULL,
  idempotency_key text NOT NULL,
  outcome text NOT NULL,
  verified boolean NOT NULL DEFAULT false,
  verification_ref text,
  created_at timestamptz NOT NULL DEFAULT now(),
  PRIMARY KEY (tenant_id, project_id, environment, idempotency_key, created_at)
);

CREATE OR REPLACE FUNCTION guard_decision_outcome() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
  IF TG_OP = 'DELETE' THEN RAISE EXCEPTION 'decision outcome records cannot be deleted'; END IF;
  RAISE EXCEPTION 'decision outcome records are append-only';
END $$;
DROP TRIGGER IF EXISTS decision_outcome_guard ON decision_outcomes;
CREATE TRIGGER decision_outcome_guard BEFORE UPDATE OR DELETE ON decision_outcomes FOR EACH ROW EXECUTE FUNCTION guard_decision_outcome();
