ALTER TABLE decision_receipts ADD COLUMN IF NOT EXISTS generation bigint NOT NULL DEFAULT 1;
ALTER TABLE decision_receipts DROP CONSTRAINT IF EXISTS decision_receipts_status_check;
ALTER TABLE decision_receipts ADD CONSTRAINT decision_receipts_status_check CHECK (status IN ('pending','committed','retryable'));
ALTER TABLE provider_attempts ADD COLUMN IF NOT EXISTS purpose text;
UPDATE provider_attempts AS a SET purpose=COALESCE(split_part(r.receipt->>'release','@',1),'unreconciled')
FROM decision_receipts AS r
WHERE a.purpose IS NULL AND a.tenant_id=r.tenant_id AND a.project_id=r.project_id AND a.environment=r.environment AND a.idempotency_key=r.idempotency_key;
UPDATE provider_attempts SET purpose='unreconciled' WHERE purpose IS NULL;
ALTER TABLE provider_attempts ALTER COLUMN purpose SET NOT NULL;
ALTER TABLE provider_attempts ADD COLUMN IF NOT EXISTS actual_charge_nano_usd bigint;
ALTER TABLE provider_attempts ADD COLUMN IF NOT EXISTS reconciliation_ref text;
