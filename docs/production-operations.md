# Production operations runbook

Tier 5 adds recoverable packaging, shared admission, data lifecycle and offline release comparison. The local drills use synthetic data. Real provider/customer qualification is still NO-GO until the Tier 4 pilot gate passes; none of these tools grants semantic acceptance.

## Release and credentials

Build with `cargo build --workspace --release --locked`, then `python3 scripts/package-images.py --output images.json`. Each API, worker and CLI image starts from `scratch`, contains its exact binary, shared libraries and CA roots, and runs as UID/GID 65532. Images contain no shell, compiler, package manager or credentials. Packaging consumes Linux host binaries: build on the deployment architecture, retain the lockfile, builder/toolchain identity and binaries, scan the resulting images, and rebuild for OS/library security updates. A locked dependency graph and immutable output do not imply bit-for-bit reproducible builds.

The manually triggered `Candidate images` workflow builds and pushes commit tags to GHCR and retains **repository@sha256 digests**. It does not deploy. Docker image IDs in local drill artifacts establish local immutability; registry deployment requires RepoDigests. CI runs `make tier5`, including preceding tiers in order. Performance measurements remain active; only the performance driver's scoped RPM allowance is explicitly raised to 1,000,000 for its load, with rate-limit correctness proved separately.

Provision three separate credentials: owner for migration/provisioning/maintenance, non-owner `kernel_runtime` login for API, and non-owner `kernel_worker` login for worker. API logins must not inherit worker. Neither service accepts superuser, BYPASSRLS or table-owner credentials. Keep API/worker env files separately restricted to the deployment operator; preserve the provider vault master key in the secret manager with its backup recovery key. Never put owner credentials into service env files or deployment manifests.

Before deployment, run the candidate `kernelctl migrate` image once with the owner env file on the target private database network. Migration 17 is additive; migrations are serialized and checksum checked. Run it twice to verify idempotence. Start services only after successful migration; do not roll schema backwards on image rollback. Readiness validates required schema and role privileges. Old Tier 4 binaries are not certified as production rollback targets: the locally tested baseline and candidate both support schema 17. Rehearse upgrades from the deployed schema on a restore first.

On a disposable staging database, run `scripts/verify-tier5.py --images registry-images.json --baseline-images previous-registry-images.json --prior-tier-evidence prior-tier-evidence.json`. The manifests must describe the actual Linux architecture. This tests candidate and previous API/worker images on host networking (Linux drill only); production compose uses private bridge networking and a routable private PostgreSQL URL. Preserve `results.json` from a passing `make tier5` at the same commit as `prior-tier-evidence.json`; a standalone operations run cannot satisfy preceding-tier gates. The report records `release_commit` and `deployment_images` from the candidate manifest. Keep the candidate comparison and full Tier 0–4 results alongside it. Inspect all evidence and get your normal operational approval, then bind and validate:

```sh
python3 scripts/prepare-deployment.py registry-images.json audit/2026-11-02-tier5/results.json deployment.json
python3 scripts/deploy-release.py deployment.json --evidence audit/2026-11-02-tier5/results.json --validate-only
# Set KERNEL_API_ENV_FILE and KERNEL_WORKER_ENV_FILE to separate protected files.
python3 scripts/deploy-release.py deployment.json --evidence audit/2026-11-02-tier5/results.json
```

Apply validates actual evidence bytes, commit, all three registry digests, successful recovery and image evidence, passing preceding-tier evidence at the same commit, and schema compatibility; compose waits for image readiness. It cannot authenticate a forged local report: protect/sign the artifacts and restrict deploy permission in the external approval system. These repository scripts provide no production IAM or signing service. A failed `compose up --wait` leaves the candidate available for inspection; restore the last approved manifest explicitly. Keep at least two tested manifests plus their evidence and registry images. Worker starts can process existing jobs immediately, so canary it against staging before production.

## Limits

| Control | Default / bound | Operator action |
| --- | --- | --- |
| Authenticated requests | 6,000/minute per tenant/project/environment/product, across API replicas | `KERNEL_REQUESTS_PER_MINUTE` must be 1..1,000,000 and identical on replicas; an owner-created `operation_limits` row overrides it. Fixed server-clock windows permit boundary bursts up to twice the allowance. Polls, retries and idempotent lookups consume admission. |
| Fast concurrent requests | 256; configurable 1..4,096 | `KERNEL_REQUEST_CONCURRENCY`; immediate 429 on full admission. |
| API provider concurrency | 16; configurable 1..256 | `KERNEL_PROVIDER_CONCURRENCY`; bounds in-flight dispatches per process. |
| API database pool | 16; configurable 1..256 | `KERNEL_DB_CONNECTIONS`; 3-second acquisition deadline. Reserve PostgreSQL connections for operator recovery and worker. |
| Worker | One sequential job/event loop per process, SQLx default pool of 10 | Poll 100 ms, configurable 10..10,000; job deadline bounded to 60 seconds; readiness fails if an iteration has not completed for 90 seconds. |
| Request size/deadline | 64 KiB and at most 3 seconds fast | Pack/request constraints can only tighten engine limits. |
| Paid calls | At most one per decision; pack/request/platform cost caps | Missing purpose budget fails closed. Each tenant/project/environment/product/purpose budget is atomically held before dispatch. Tenant/project exposure is the sum of approved child caps; there is no independent rolling aggregate parent bucket. |
| Unknown charge | Conservative hold until verified reconciliation | Use `kernelctl reconcile-attempt KEY VERIFIED_NANO_USD EXTERNAL_PROOF_REF` with all four scope env values. Provider-reported costs do not release holds. |
| Container resources/logs | 512 MiB, 2 CPUs, 128 PIDs, read-only filesystem; 3 × 10 MiB Docker logs | Re-measure actual workload before raising. Off-host log retention is configured by the operator. |

All authenticated endpoints consult shared PostgreSQL admission; a database failure fails closed. Unauthenticated traffic and connection/body floods need upstream TLS, network isolation and proxy connection/rate limits. `/metrics` requires a separate `KERNEL_METRICS_TOKEN` of at least 32 characters and is disabled without it. Do not expose service or probe ports publicly. Compose binds API to host loopback; terminate HTTPS in an authenticated network boundary/proxy. The worker probe port is internal.

## Telemetry and alerts

API emits JSON completion events with validated/generated request and correlation IDs, status, elapsed duration and `Server-Timing` components for engine/database/provider/queue when available. This is request tracing and timing correlation; no OpenTelemetry collector or cross-service span propagation is included. Use opaque request IDs: these fields are logged. Payloads, authorization headers, model evidence and raw provider response bodies are excluded. Worker emits progress events; job/outbox diagnostics identify their opaque IDs. Retain receipt IDs and stored inputs in the protected audit database, not a public metrics label.

Scrape API and worker `/metrics` with the metrics token through the private network. API exposes request/error/admission totals, cumulative duration and pool gauges. Worker exposes progress age, oldest/pending/dead-letter jobs, unresolved outbox and unknown attempts. Scope-specific spend/held caps remain in authenticated `/v1/usage` and `kernel-ops.py status`, avoiding unbounded tenant metric labels. `deploy/alerts.yaml` supplies Prometheus rules and concrete first actions for failed scrape, server errors, sustained 429, stuck worker, job backlog, unresolved costs and outbox backlog. Wire an Alertmanager receiver and run a test notification before go-live; this session did not configure an external monitoring account. Test `/readyz` independently of liveness. A connected metrics target can still be unready.

## Recovery

Daily encrypted `pg_dump -Fc` backups and continuous WAL/PITR are the proposed starting policy: **RPO ≤15 minutes, RTO ≤30 minutes are operator targets, not measured production guarantees**. Configure WAL archival/restore and off-site copies with your database service; daily dumps alone give up to 24-hour loss. Keep daily backups 7 days and weekly backups 30 days, subject to the approved retention agreement. Include schema migration ledger, roles/grants, release pointers, receipts, recorded inputs, reviewer identities, evidence, cache, outbox, jobs and budget/attempt ledger. Back up provider vault key separately in the secret manager. Dumps omit role passwords; recreate API/worker logins and grants with fresh credentials on the recovery cluster.

Restore a custom-format dump into a **new isolated database** using `pg_restore --exit-on-error`; never restore over the serving database. Apply/check migrations, compare row content and financial holds, verify API/worker readiness with non-owner logins, resolve leases with the existing worker recovery logic, and replay a known receipt. Potentially dispatched calls stay uncertain; never redispatch a possibly billed attempt or clear holds to make a drill green. Keep the restored outbox sink isolated until consumers are ready, because consumers must deduplicate by event ID. Use a fresh scope/idempotency key for new work.

The automated drill performs a real dump/new-database restore and exact comparison of receipt/input/pointer/outbox contents, plus unknown charge/hold preservation. It then removes the disposable restore database. It records elapsed local restore time and the dump digest, without retaining unencrypted backup bytes in Git. This is not a PITR or separate-region disaster-recovery proof. Run monthly restore drills with the real encrypted off-site backup and secret recovery process; alert on backup age and on failed restore jobs through the database platform.

For an image rollback, apply the previous approved digest manifest with its evidence, wait for both services to become ready, and replay a known receipt. Do not reset the migration ledger. For a pack rollback, inspect the current pointer revision, then `kernelctl rollback DECISION_TYPE PREVIOUS_VERSION EXPECTED_REVISION`. Both promotion and rollback use compare-and-swap revisions and emit outbox audit events; stale revisions fail. New requests use the restored active pack; existing receipts and queued jobs remain pinned. The automated image drill switches distinct non-root/read-only baseline → candidate → baseline API and worker images and asserts exact receipt preservation at every phase.

## Retention, export and deletion

All commands require an owner maintenance connection and explicit `KERNEL_TENANT_ID`, `KERNEL_PROJECT_ID`, `KERNEL_ENVIRONMENT`, `KERNEL_PRODUCT_ID`. They never infer a scope or operate globally. `KERNEL_OPS_PSQL_CONTAINER` is a local drill convenience; ordinary operation uses `psql` and `DATABASE_URL`/libpq credential configuration. Outputs contain sensitive data and are created with mode 0600 and exclusive creation. Encrypt them off-host and control access.

`python3 scripts/kernel-ops.py retention --older-than-days 30` prunes expired predictions and idle admission windows for this scope. Schedule it daily. Receipts, evidence, input snapshots and feedback remain for audit/replay until explicit scope retirement; this implementation does **not** silently expire the idempotency history of an active scope. Agree that retention period before enabling a customer scope, and isolate product scopes by retention cohort when necessary. Purging customer data while maintaining a long-lived active scope needs a separately reviewed per-record lifecycle extension.

To retire a product scope:

1. `python3 scripts/kernel-ops.py block` persists a block and revokes service/reviewer tokens. Static API authentication also checks the block. Stop API/worker processing and external dispatch, wait for in-flight operations, drain or dead-letter queued work under the normal job policy, and reconcile charges. A block alone does not cancel an admitted call or an already leased worker job.
2. `python3 scripts/kernel-ops.py status` shows unresolved work/holds. `export --output /protected/scope.json` writes a transactionally consistent customer-data snapshot and prints its SHA-256. The export includes exact request/evidence snapshots and immutable receipts; token hashes and encrypted provider secrets are excluded.
3. Inspect/export under your approved deletion request, then `delete --output /protected/scope.json --export-sha256 DIGEST`. The delete locks tables, requires the durable block, refuses pending/leased jobs, pending receipts and uncertain financial state, and rejects an export if any scoped data changed. It removes scoped operational data and tokens in one transaction; append-only feedback guards permit only owner-authorized maintenance deletion. Normal API logins retain no DELETE privilege on these records.
4. Keep the minimal scope block in a protected deletion register outside the restored customer backup. Reapply deletion blocks and approved purge records **before** allowing traffic to a restored backup. Rotate/revoke any shared project/tenant provider key separately after all bound products retire. Product deletion deliberately retains shared release/qualification definitions and shared provider connections; they must contain no customer instance data. Project-wide secret/release cleanup needs a separate scope-wide retirement process.

Backups, approved exports, downstream outbox consumers and upstream provider retention have separate expiry/deletion obligations. Destroy exports after the approved handoff; expire encrypted backups by policy, and ensure restore never resurrects deleted scopes. The delete report retains only scope IDs and export digest, not deleted payloads. A scope block is permanent until an owner explicitly changes the database; create a new scope identity for re-onboarding.

## Improvement and semantic acceptance

`kernelctl compare-stored BASELINE_PACK CANDIDATE_PACK SCOPE_EXPORT` verifies stored request digest/scope and baseline release identity, reruns deterministic policy at the receipt's original time, and reuses stored typed semantic answers. It makes no provider calls and grants no qualification. Changed semantic questions, provider identity, output contracts, state projection/preparation, input schema or evidence contract require fresh predictions and are rejected for reuse. Legacy receipts without captured inputs fail explicitly. Receipts from other baseline releases are counted as excluded. The report includes candidate/baseline/export digests and per-receipt before/after results; protect its customer data like an export. Changing qualification flags cannot turn an offline semantic prediction into an acceptance.

Before promotion, compare held-out predictions from both actual releases using the same frozen independent labels/groups; run `kernelctl evaluate` for each version. The existing evaluator rejects group leakage, missing rights, unreviewed model-reference test labels and invalid answer contracts. Inspect required slice coverage, sample counts, accuracy, per-class precision/recall and unresolved errors. Unresolved predictions count as class false negatives. Label-only answers have no probabilities; no calibration score or confidence is invented. Binary probability predictions with provenance can produce a Brier score, which alone does not establish calibration. Any fitted calibration belongs on an independent calibration partition and must be evaluated on untouched test data before an acceptance threshold changes.

Explicit semantic auto-accept gates remain: pack opt-in; complete provider/requested model/returned model/adapter identity; exact release digest; nonempty held-out sample and required slices meeting pack criteria; cleared rights and independent reviewer/outcome labels; recorded operator/approval reference; unexpired, active, non-revoked database qualification; all deterministic policy/evidence/budget/deadline gates. Review urgent class recall and safety errors before publishing; the current criteria schema enforces accuracy/coverage/sample gates, while class-specific recall and calibration review are operator approval responsibilities. Low-quality synthetic fixtures must remain review-only. `publish-qualification` never calls a model or independently authenticates label provenance; the approver must verify the submitted evidence. Re-evaluate on distribution changes and expire/revoke qualifications when evidence no longer holds.

For local reproduction (Docker, Rust, Node 24+, OpenSSL and Python contract venv):

```sh
export KERNEL_TEST_DATABASE_URL=postgres://postgres:kernel_test@127.0.0.1:55435/kernel_test
export KERNEL_TEST_PSQL_CONTAINER=kernel-tier5-postgres
make tier5
python3 scripts/package-images.py --prefix kernel-candidate --output /protected/candidate-images.json
python3 scripts/verify-tier5.py --images /protected/candidate-images.json --baseline-images /protected/previous-images.json
```

The verification scripts create synthetic scope rows and LOGIN roles in the disposable cluster. They remove their restore databases and running services, while leaving test rows/roles for inspection. Destroy that cluster after reviewing evidence; never run them against a shared production database.
