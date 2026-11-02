-- Reviewer credentials are provisioned by an operator, never by a request body.
CREATE TABLE reviewer_tokens (
 token_hash_sha256 text PRIMARY KEY,
 tenant_id text NOT NULL, project_id text NOT NULL, environment text NOT NULL,
 product_id text NOT NULL, reviewer_id text NOT NULL CHECK (length(reviewer_id) BETWEEN 1 AND 128),
 active boolean NOT NULL DEFAULT true
);
ALTER TABLE reviewer_tokens ENABLE ROW LEVEL SECURITY;
ALTER TABLE reviewer_tokens FORCE ROW LEVEL SECURITY;
CREATE POLICY kernel_scope ON reviewer_tokens TO kernel_runtime USING (
 tenant_id=current_setting('kernel.tenant_id',true) AND project_id=current_setting('kernel.project_id',true)
 AND environment=current_setting('kernel.environment',true) AND product_id=current_setting('kernel.product_id',true));
GRANT SELECT ON reviewer_tokens TO kernel_runtime;
ALTER TABLE decision_reviews ADD COLUMN corrections jsonb NOT NULL DEFAULT '{}';
ALTER TABLE decision_reviews ADD COLUMN reason_code text;
ALTER TABLE decision_outcomes ADD COLUMN status text NOT NULL DEFAULT 'pending' CHECK (status IN ('pending','verified','disputed'));
ALTER TABLE decision_outcomes ADD COLUMN reviewer_id text;
-- Existing immutable observations retain their verified flag; API reads map it to verified.
