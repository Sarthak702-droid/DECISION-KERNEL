# Tier 4 — developer product and pilot workflow

Implemented on 2 November 2026. The developer workflow is exercised end to end with a local HTTPS provider fixture and a real local HTTP consuming application. A live external-provider/customer pilot remains open: no provider credentials or approved customer ticket dataset were available in this session. Authored project-specific scenarios do not establish production quality.

## Deliverables

| Requirement | Implementation and evidence |
| --- | --- |
| Stable HTTP contract | [OpenAPI 3.1](api/openapi.json), also served at `/openapi.json`; bounded strict request JSON, existing error envelope, idempotency and request/correlation headers documented. Formal schema validation and live response validation passed. |
| TypeScript first | [SDK](sdk/typescript/src/index.ts), dependency-free at runtime, typed requests/receipts/answers/connections/jobs/feedback/usage; pinned development tools, strict type-check, transport/error/digest checks. |
| Minimal console | `/console`: provider metadata/create/rotation/probe/binding, packs, test request, receipt lookup, usage and feedback. Same-origin assets, CSP, in-memory credentials. Browser layout inspected; HTTP proof checks actual served assets. |
| Complete BYOK support example | [Quickstart](docs/developer-quickstart.md), [versioned support pack](packs/examples/support-triage-byok.json), SDK connect/probe/bind script, authenticated local ticket HTTP server, evidence digest/intake, decision and application receipt consumer. |
| Structured corrections and identity | Migration 16, operator-issued scoped reviewer tokens with revocation; identity cannot come from the request body. Corrections validate against the original receipt's pinned pack; no invented labels or probabilities. |
| Outcome states | Append-only `pending`, `verified`, `disputed` observations. Verified/disputed require reviewer authentication and audit reference. Reference semantics are reviewer assertions, not automatic external proof validation. |
| Pilot measurements | [Runner](examples/support-triage/evaluate.ts), [48 authored held-out scenarios](datasets/support-triage-heldout.json), retained input/receipt evidence, leakage guard, slice accuracy, queue confusion, urgent misses, review count/rate, attempts, separate reported/settled/unknown costs and scoped budget snapshots. |

## Reproduction

Use a disposable PostgreSQL owner connection, Node 24+, Rust and OpenSSL:

```sh
export KERNEL_TEST_DATABASE_URL='<isolated owner URL>'
# Set only if psql is available inside the disposable database container:
export KERNEL_TEST_PSQL_CONTAINER='<container name>'
make tier4
```

`make tier4` includes Tier 0–3 regressions, SDK type-check/tests and `scripts/verify-tier4.py`. CI provisions Node and enables formal OpenAPI checks. To run the contract checks separately:

```sh
python3 -m venv /tmp/kernel-contract-venv
/tmp/kernel-contract-venv/bin/pip install -r scripts/requirements-contract.txt
KERNEL_VALIDATE_OPENAPI=1 /tmp/kernel-contract-venv/bin/python scripts/verify-tier4.py
```

The proof creates isolated synthetic scopes and non-owner API credentials. It drives the same checked-in TypeScript scripts developers run, through a local TLS fixture with exact served-model identity. It verifies:

- Owner publication/promotion, TypeScript key creation/probe/binding and scoped pack listing.
- Application-side authenticated ticket intake, evidence ingestion, one provider attempt, typed receipt consumption and replay of the exact ticket request.
- Cross-product receipt lookup rejection.
- Missing/spoofed/revoked reviewer rejection, invalid correction vocabulary rejection, stored authenticated identity and typed corrections.
- Pending/verified/disputed history, proof-reference enforcement, service-token-only verification rejection, original receipt immutability.
- Held-out execution, truthful unresolved-cost accounting and group-partition leakage rejection before dispatch.
- OpenAPI document validity and exercised live JSON response conformance when `KERNEL_VALIDATE_OPENAPI=1`.

## Observed synthetic pilot

[Machine-readable checks](audit/2026-11-02-tier4/results.json) and [full pilot inputs/receipts](audit/2026-11-02-tier4/synthetic-pilot.json) retain the evidence. The fixture deliberately always returns the technical queue, and detects urgency only from the English word `urgent`. Its poor labels ensure the runner can expose failures rather than certify itself with perfect fake predictions.

| Measurement | Observed |
| --- | ---: |
| Completed / failed | 48 / 0 |
| Queue accuracy | 25% |
| Urgency accuracy | 81.25% |
| Joint accuracy | 18.75% |
| Urgent missed or unresolved | 7 / 24 |
| Review workload | 48 / 48 (100%) |
| Provider attempts | 48 |
| Provider-reported cost | 3,840 nano-USD (synthetic) |
| Unknown settled receipt costs | 48 |

These numbers describe the fixture only. Each attempted call retains its conservative budget hold; settled costs remain unknown pending external reconciliation. Read the report's before/after usage snapshots for exposure, and do not describe unknown charges as zero. Review rate is escalation workload; no human minutes were measured.

## Rollout and acceptance limits

Apply migration 16 before starting the upgraded daemon/worker; readiness checks the new fields and reviewer read privilege. Provision reviewer credentials from your authenticated identity system using `kernelctl register-reviewer`; revoke by token hash with `kernelctl revoke-reviewer`. Old review clients now need the reviewer header and `reason_code`; verified outcome clients need the reviewer header and audit reference. These security requirements deliberately tighten the legacy feedback API. Existing append-only outcomes remain readable, with historical `verified=true` mapped to verified.

The support example retains the existing NO-GO policy for semantic auto-acceptance. Prior project evidence showed some routed models exceeding the 1,000 ms pack deadline. A successful binary connection probe cannot establish batch classification quality, latency or release qualification. Select and measure a provider meeting the actual pack limits.

A real pilot must replace authored scenarios with approved, representative real tickets, independent labels, frozen development/calibration/test groups, real credentials and reviewed invoice evidence. Run the documented application path with that key and retain its receipt. Until then, the developer product and synthetic integration proof are implemented; the real-pilot completion gate is not claimed.

## Final validation and performance variability

- Rust workspace: 41 tests passed with PostgreSQL enabled. Formatting and clippy with warnings denied passed.
- Tier 0, 1 and 2 HTTP/worker regressions passed in the full `make tier4` attempt.
- SDK strict type-check and both client boundary tests passed; the final standalone Tier 4 proof passed with OpenAPI validation enabled. Logs: [client checks](audit/2026-11-02-tier4/client-checks.log), [contract/integration proof](audit/2026-11-02-tier4/contract-validation.log).
- The aggregate `make tier4` command stopped at the Tier 3 latency gate on its first run. [First-run evidence](audit/2026-11-02-tier4/performance-first-run.json) records rules client p99 72.52 ms, cache p95/p99 34.94/468.25 ms, and fresh non-provider p95/p99 64.41/157.17 ms. No thresholds were relaxed and no durability settings were weakened.
- The unchanged, targeted 3,000-request-per-path Tier 3 rerun passed all fixed gates: [rerun evidence](audit/2026-11-02-tier4/performance-rerun.json). This variability does not establish a production latency SLA or prove a root cause for the first failure. The aggregate command itself is not reported as green.

| Rerun measurement | p95 ms | p99 ms |
| --- | ---: | ---: |
| Rules scheduled-client HTTP | 8.61 | 17.67 |
| Exact cache scheduled-client HTTP | 12.96 | 48.67 |
| Fresh provider engine residual | 0.215 | 0.254 |
| Fresh HTTP overhead excluding provider | 20.38 | 29.31 |
| Idle worker observed completion | 139.49 | 142.98 |
