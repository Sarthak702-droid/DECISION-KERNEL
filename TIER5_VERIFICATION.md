# Tier 5 — production operations and improvement loop

Implemented on 2 November 2026. Local synthetic operations and recovery drills pass. Production promotion remains gated on preceding-tier performance evidence and real provider/customer qualification. This report does not declare a production latency SLA or semantic acceptance approval.

## Delivered behavior

| Requirement | Implementation |
| --- | --- |
| Immutable API/worker images and deployment | Shell-free non-root images package exact Linux binaries, shared libraries and CA roots from a locked release build. Candidate CI pushes commit tags and records registry digests. Deployment validates digest-only manifests, evidence bytes, commit, preceding tiers, schema compatibility and readiness. Previous manifests use the same rollback command. |
| Credentials, probes and migrations | Existing owner/API/worker separation is preserved. Migration 17 adds RLS-protected admission, exact request capture and lifecycle blocks. Migration image is exercised twice. Both API and worker provide liveness/readiness and shell-free image probes. API drains bounded requests on SIGTERM; worker crash/termination uses existing conservative recovery. |
| Telemetry and alerts | JSON request/correlation traces include component timings; protected API/worker metrics expose errors/admission/pools, loop age, job backlog/dead letters, unresolved outbox and financial attempts. Prometheus rules include concrete operator actions. This is correlation tracing, not an OpenTelemetry deployment. |
| Operational limits | Shared atomic fixed-window RPM admission across replicas, purpose-level tenant/project/environment/product budget holds, bounded request/provider concurrency, pool/deadline/body/container limits and log rotation. Defaults and limitations are in the [runbook](docs/production-operations.md). |
| Recovery | Real custom-format PostgreSQL backup → new database restore → non-owner API/worker readiness → exact receipt replay. Unknown charge holds, stored inputs, pointers and outbox contents are preserved. Distinct baseline/candidate API and worker images run read-only as UID 65532, serve fresh requests/jobs and preserve receipts through rollback. |
| Retention/export/deletion | Owner-only scope export, exclusive mode-0600 output, expired-cache/window retention, durable block/token revocation, reconciliation/pending-work guards, fresh-export digest checks and atomic scope deletion. Append-only feedback cannot be deleted by ordinary runtime credentials. Cross-product data and the deletion block survive. Active-scope per-record PII expiry is not implemented; full scope retirement and external backup/export expiry are explicit policies. |
| Stored prediction replay and comparison | Exact engine requests, including materialized evidence, commit atomically with crash fallback before dispatch. `compare-stored` checks scope/request/release digests, freezes original observation time and compares compatible candidate policy using stored typed answers. Changed semantic/input/evidence/provider contracts require fresh predictions. Offline replay never grants semantic qualification. |
| Held-out quality and qualification | Existing independent-label, rights, group-leakage and required-slice gates remain. Unresolved predictions now count as class false negatives. Label-only outputs remain uncalibrated; binary Brier scores require actual probability predictions. Stored semantic replay is tested with typed answers, hard-deny dominance and invalid-answer rejection. |

## Reproduction and evidence

Use a disposable PostgreSQL 16 owner connection, Docker, Rust, Node 24+, OpenSSL and the contract-validation Python environment:

```sh
export KERNEL_TEST_DATABASE_URL='<disposable owner URL>'
export KERNEL_TEST_PSQL_CONTAINER='<disposable PostgreSQL container>'
KERNEL_VALIDATE_OPENAPI=1 make tier5
python3 scripts/package-images.py --prefix decision-kernel-candidate --output candidate-images.json
python3 scripts/verify-tier5.py --images candidate-images.json --baseline-images previous-images.json
```

`make tier5` runs tiers 0 → 1 → 2 → 3 → 4 → 5. A preceding-tier failure stops the chain. Standalone operations success is recorded separately from `prior_tiers_passed`; deployment refuses evidence without a passing preceding-tier run at the candidate commit. Published registry digests and protected/signed approval artifacts are needed for actual deployment; only local images were built and run here.

- [Operations/image drill results](audit/2026-11-02-tier5/results.json), [drill log](audit/2026-11-02-tier5/image-recovery-final.log), [baseline images](audit/2026-11-02-tier5/images-baseline.json), [candidate images](audit/2026-11-02-tier5/images-candidate.json).
- [Per-receipt candidate comparison](audit/2026-11-02-tier5/comparison.json). It is synthetic policy simulation, with `semantic_qualification_granted: false`.
- [Full-chain first attempt](audit/2026-11-02-tier5/tier5.log), [following full-chain attempt](audit/2026-11-02-tier5/tier5-final.log), [performance before atomic capture](audit/2026-11-02-tier5/performance-before-atomic.json), [following performance evidence](audit/2026-11-02-tier5/performance-final.json).
- [Standalone Tier 4 with OpenAPI validation](audit/2026-11-02-tier5/tier4-final.log). SDK strict type-check and two client tests passed separately.

## Acceptance boundaries

Operational drills exercise real PostgreSQL and real Linux images with synthetic customer scopes; no paid provider calls or production deployment occurred. A local restore is not encrypted off-site key recovery, PITR or a cross-region disaster-recovery drill. Production database credentials, vault keys, backup destination, WAL retention, monitoring receivers, alert delivery and production host/network configuration require deployment-owner provisioning and rehearsal.

The initial full-chain attempts failed the unchanged Tier 3 gates. Database waits and serialized cache coordination dominated those measurements. Input capture was moved into the existing durable recovery transaction; validated cache hits subsequently release the population lock before independent receipt/qualification processing. Paid misses still hold coordination through cache publication, and no durability setting or latency threshold was weakened. This removes unnecessary serialization, but does not prove all observed host/IO latency spikes have a single cause. The final full-chain result remains FAIL; the gate was not relaxed. The earlier measurements and final results are all retained.

The support pilot remains review-only. Representative approved real tickets, independently reviewed labels, frozen development/calibration/test groups, measured provider latency/quality, invoice evidence and explicit operator approval are still required for semantic auto-acceptance. The runtime additionally requires exact release/provider/requested-and-returned-model/adapter identity and an active, unexpired, non-revoked qualification. Product retirement does not erase shared pack/provider definitions or historical off-site backups; follow the runbook's separate retirement/expiry procedures.

## Final observed validation

- Formatting, clippy with warnings denied and all 41 Rust tests passed with PostgreSQL enabled. Tier 0–2 API/worker regressions passed on the final code.
- Standalone Tier 4/OpenAPI validation, SDK strict type-check and both SDK boundary tests passed.
- All 11 operational/image checks passed on final images. The local backup/restore plus restored API/worker readiness and exact receipt replay took **3.925 seconds**; this is a local synthetic observation, not production RTO evidence.
- Final image proof ran distinct baseline → candidate → baseline API/worker images, exercised fresh fast decisions and deferred jobs, checked non-root configuration and probes, and preserved the exact stored receipt at every phase.
- The aggregate `make tier5` stopped at Tier 3 in all three attempts. [Final chain log](audit/2026-11-02-tier5/tier5-cache-final.log) and [final performance evidence](audit/2026-11-02-tier5/performance-final.json) retain the unchanged thresholds and observed failures. The standalone operations report therefore records `prior_tiers_passed: false` and `production_qualified: false`.
- [Promotion gate proof](audit/2026-11-02-tier5/promotion-gate.json) confirms that even passing recovery evidence cannot authorize deployment while preceding-tier evidence is missing/failing. No production deployment or registry publication was attempted.

| Final 3,000-request path at 100 offered requests/second | p95 ms | p99 ms | Gate |
| --- | ---: | ---: | --- |
| Rules scheduled-client HTTP | 13.209 | 15.545 | PASS |
| Exact cache scheduled-client HTTP | 23.118 | 52.641 | FAIL |
| Fresh engine residual | 0.407 | 1.245 | PASS |
| Fresh HTTP excluding provider | 242.529 | 494.515 | FAIL |

The final cache change removes avoidable serialization, while fresh-path database/tail behavior remains unresolved. Investigate it on the target persistent database and host under the same fixed workload before enabling promotion. Real-provider pilot, off-site/PITR recovery and alert delivery qualification also remain open as described above.
