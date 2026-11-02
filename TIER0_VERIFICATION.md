# Tier 0 — foundation remediation

Verification date: 2 November 2026. Scope: the six Tier 0 requirements, using synthetic data, disposable PostgreSQL 16, separate API/worker LOGIN credentials, and a local HTTPS provider fixture. No external paid provider or production database was used.

## Result and evidence

The isolated API + worker acceptance flow passes. The root verification command is `make tier0`, and CI now runs that same command. Evidence is in [audit/2026-11-02-tier0](audit/2026-11-02-tier0).

| Requirement | Implementation | Regression evidence |
|---|---|---|
| Receipt, idempotency, evidence and cache isolation | Product participates in database keys, RLS, explicit store query predicates, scoped input digest, receipt identity and exact cache identity. Budget/usage also include product and environment. Existing receipts are checked against authenticated scope before reuse. | Concurrent duplicate POSTs dispatch once and produce one completion event. Different inputs conflict. Product/tenant/environment reads, receipt identities, evidence and cache reuse remain independent. Tests include owner SQL paths as well as actual non-owner logins. |
| Complete runtime-role writes | Narrow release-row locking privilege; separate `kernel_runtime` and `kernel_worker` credentials; scoped transactions set all four dimensions. Worker alone has intentional cross-scope queue/recovery policies. | Real HTTP decisions, evidence ingestion, paid attempts, cache writes, reconciliations, outbox inserts and deferred jobs pass with non-owner credentials. |
| Missing permission/evidence | Three-valued policy predicates preserve unknown through AND/OR. Missing/null/mistyped policy facts block acceptance. Explicit hard denial takes precedence. Missing evidence pointers become invalid placeholders retaining ID/revision, so they cannot disappear from identity or lineage. | Missing permission produces `review/missing_permission`; unresolved supplied reference produces `review/missing_evidence`. Changing that reference under the same key conflicts. |
| Full request deadlines | Caller/platform absolute deadline starts at HTTP ingress, including body read, auth, release lookup, evidence, admission, claim, cache, reserve, provider, qualification and commit. Once the pack is resolved its cap tightens the same elapsed budget. Worker execution has an absolute deadline too. Store transactions also bound statement/lock waits. | 50 ms HTTP requests fail with structured `504 DEADLINE_EXCEEDED` while authentication, release or evidence tables are locked. Claim/commit lock regressions enforce bounded completion too. |
| Charge accounting and crash recovery | Dispatch requires an explicit conservative price bound. Reservation uses that bound, rather than the caller's numeric cap. Usage/reported charges survive invalid answers and overspend; uncertain holds remain until verified reconciliation. Reconciliation is transactional, repeat-safe and accepts charges above the hold, emitting an overspend event. Recovery templates are persisted before dispatch. Stale paid calls become terminal review receipts without redispatch; undispatched claims receive a new fencing generation. | Local HTTPS adapter traffic exercises billing, invalid response normalization, unaffordable dispatch rejection and overspend. An actual API process is killed during a provider call; worker recovery returns one terminal receipt and the provider call count stays one. Reconciled synthetic ledger totals match invoices and holds return to zero. |
| Startup/dependency failures | Axum 0.8 route syntax fixed. API and worker share provider configuration. Startup/readiness check schema, effective privileges, RLS, owner membership, superuser/BYPASSRLS status and correct role separation. Invalid adapter config and unavailable DB fail explicitly. | Daemon starts under runtime credentials. Lost locking privilege makes readiness return 503 and restoring it returns 200. Owner API credentials, runtime-only worker credentials and invalid provider configuration are rejected. Patched dependency policies pass. |

The workspace suite contains **26 passing tests**, including database and legacy-upgrade regressions. The live process verifier contains **15 passing acceptance checks**. Formatting, Clippy with warnings denied, build, migration/repeated migration, replay, active dependency guard, lockfile audit and dependency policies also pass. Read [the final root-run log](audit/2026-11-02-tier0/tier0.log) and [dependency evidence](audit/2026-11-02-tier0/dependencies.log).

Dependency evidence used RustSec checkout `857a4a907fed9fdb7c05d66834e960e086fcd698` (1,398 advisories). The default local cargo-audit checkout was locked by another process, so the lockfile audit used the freshly updated cargo-deny RustSec checkout with `cargo audit --db <checkout> --no-fetch`. Its result and the independent active-graph guard are recorded in the dependency log. The only advisory exception is inactive SQLx/MySQL RSA; CI fails if that dependency becomes reachable. Dependency-policy duplicate-version and unused-license-allowance warnings remain non-fatal under the repository policy.

The disposable verification container was removed after these logs were saved; no test service remains running from this work.

## Run it from the repository root

Requirements: Rust/Cargo, make, Python 3, OpenSSL, PostgreSQL client, and a disposable PostgreSQL 16 database whose owner can create test databases and LOGIN roles. The verifier creates synthetic data and briefly changes the test `kernel_runtime` release-lock privilege; use a dedicated test cluster.

```sh
export KERNEL_TEST_DATABASE_URL='postgres://test_owner:<test-password>@127.0.0.1:5432/kernel_test'
make tier0

# Requires cargo-audit 0.22.2 and cargo-deny 0.20.2 (also pinned in CI).
make supply-chain
```

If PostgreSQL client tools exist only inside a local test container, set `KERNEL_TEST_PSQL_CONTAINER` to its name. The verifier executes `psql` there using the owner/database in the test URL. It terminates API/worker/fixture processes and drops its temporary LOGIN roles. The upgrade regression also drops its temporary database. Synthetic rows remain in the dedicated acceptance database for inspection.

## Runtime credential setup

Apply migrations with the owner connection. Provision independent secrets and LOGIN roles, then grant the existing NOLOGIN roles:

```sql
CREATE ROLE kernel_api LOGIN NOSUPERUSER NOBYPASSRLS PASSWORD '<generated-api-secret>';
GRANT kernel_runtime TO kernel_api;
CREATE ROLE kernel_jobs LOGIN NOSUPERUSER NOBYPASSRLS PASSWORD '<generated-worker-secret>';
GRANT kernel_worker TO kernel_jobs;
```

Use `kernel_api`'s connection as the daemon's `DATABASE_URL`, and `kernel_jobs`'s connection for the worker. API credentials must not inherit the worker or an owner role. The worker's explicit cross-scope privileges are for queue leasing, outbox delivery and recovery; evidence/cache/budget operations still use transaction-local scope. The owner connection is for `kernelctl` migrations, publication, token provisioning and controlled administration.

Budget provisioning now requires all scope dimensions:

```sql
INSERT INTO budget_buckets
  (tenant_id,project_id,environment,product_id,purpose,cap_nano_usd)
VALUES ('tenant','project','staging','product','support.triage',1000000);
```

Idempotency keys are unique within tenant/project/environment/product. Reusing a key with a different decision type, release, state, evidence or constraints conflicts. Releases and their qualified bindings remain explicitly shared at tenant/project/environment scope; data, cache and accounting are product scoped.

## Provider prices and invoice reconciliation

API and worker use the same `KERNEL_PROVIDER_*` configuration. `KERNEL_PROVIDER_MAX_DISPATCH_NANO_USD` must be a verified all-in upper bound for the configured provider/model, maximum serialized input (semantic request capped at 64 KiB), adapter overhead, and at most 512 output tokens. There are no adapter retries. Changing model, token bounds or pricing requires updating/reverifying that bound. This is a deployment-supplied conservative ceiling, not automatic discovery of provider prices.

If the bound is absent, paid dispatch is disabled and the runtime returns `budget_unavailable`; deterministic and already-valid exact cache paths remain available. If the bound exceeds a caller, pack or platform cap, dispatch is rejected before sending. `KERNEL_PROVIDER_CA_CERT_PATH` optionally adds a private HTTPS trust root for internal providers; certificate validation, exact host allowlisting and redirect rejection remain enforced.

Reported usage and cost are evidence, not invoice verification. Paid holds remain until reconciliation, including timeout, malformed output and crash cases. `cost_nano_usd` stays null until verified; zero-attempt receipts record zero. Reconcile with the exact product scope:

```sh
KERNEL_TENANT_ID=tenant KERNEL_PROJECT_ID=project \
KERNEL_ENVIRONMENT=staging KERNEL_PRODUCT_ID=product \
DATABASE_URL='<administrative-connection>' \
kernelctl reconcile-attempt request-key 15000 invoice-proof-reference
```

Repeating the same charge/proof succeeds without double counting; changing them conflicts. Verified charge above the reservation is recorded in full, may push spend above the cap, and emits `provider.overspend`. Further work cannot use unavailable budget. Consumers must process/deduplicate outbox events; no external alert sink was configured for this verification.

The worker checks stale claims after 60 seconds. A paid crash becomes `review/crash_recovery` with not-evaluated outputs and retained unknown charge. It never reruns that paid attempt, including after reconciliation. An undispatched stale claim is retryable with a new fencing generation. Worker job leases are also fenced and reclaimable after process failure.

## Upgrade notes

Migration **0014** is additive to the existing checksummed migration history; migrations 0001–0013 were not edited. Drain work and stop the old API/worker before migration, then start the new binaries with separate credentials.

- Committed receipts with a provable product scope and their attempts are attributed to that product. Known holds and reconciled spend are reconstructed in product/environment buckets with **cap zero**. An operator must explicitly approve new scoped caps; a shared legacy cap is never multiplied across products.
- Unattributed legacy evidence/cache/feedback/outbox data remains under the empty product quarantine. API scopes cannot use an empty product. Re-ingest evidence under an explicitly authorized product; historical cache entries must not be reused across guessed owners.
- Legacy shared budget rows remain inaccessible archival snapshots. Reconstructed active product rows are the ledger used by the runtime; do not sum archived and active buckets together.
- Old input digests lacked scope. Old receipts remain readable by their owning product, but retrying old keys under the new digest format can conflict. Use new keys for new work and adjudicate outstanding legacy requests before rollout.
- Legacy pending paid claims without stored scope/recovery templates cannot be automatically reconstructed safely. They remain quarantined for administrative invoice/ownership adjudication. New dispatches always persist a recovery template first.

These upgrade rules are covered by a populated legacy-schema regression, in addition to fresh/repeated migration execution.

## Boundaries of this result

This closes the requested isolated Tier 0 engineering proof. It is not a production readiness or live model quality certification. External provider contract conformance/qualification (including the previously audited System One native-contract mismatch), authenticated reviewer/outcome-ground-truth design, backup/restore, retention, release deployment and representative load gates require their own work before those broader claims. No shared real-user rollout or production deployment was performed.
