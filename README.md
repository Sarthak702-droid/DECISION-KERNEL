# Decision Kernel

[![Rust](https://img.shields.io/badge/rust-stable-brightgreen.svg?logo=rust)](https://www.rust-lang.org/)
[![License](https://img.shields.io/badge/license-Apache%202.0%20%2F%20MIT-blue.svg)](#license)
[![Architecture](https://img.shields.io/badge/architecture-Decision--Only%20v1.1-purple.svg)](#core-principles)
[![Verification](https://img.shields.io/badge/release%20gates-in%20progress-yellow.svg)](#release-acceptance-gates)
[![Deterministic Core](https://img.shields.io/badge/pure%20core%20p95-0.2105%20ms-informational.svg)](#performance-acceptance-benchmarks)

A Rust-first, headless decision engine based on the **Decision-Only Architecture v1.1**. Local policy and PostgreSQL integration checks pass. Production release gates in the v2.0 PRD remain open; see [the current gate report](DECISION_ENGINE_TEST_REPORT.md).

Tier 0 remediation and its isolated API/worker proof are documented in [TIER0_VERIFICATION.md](TIER0_VERIFICATION.md). Run `make tier0` with an isolated `KERNEL_TEST_DATABASE_URL`; CI runs `make tier2`, which includes this root command. Live provider qualification and broader production gates remain separate.

Applications provide authenticated facts, permitted evidence, and a published decision specification. External intelligence APIs (TypeSafe System One, schema-constrained LLMs) supply bounded semantic judgments when needed. The Rust kernel validates those judgments, combines them with deterministic rules and policy constraints, and returns an immutable, typed decision receipt. The consuming application alone executes business actions.

---

## Tier 4 developer workflow

Follow the [BYOK developer quickstart](docs/developer-quickstart.md) to connect your key, publish and run support triage, and consume a receipt in an HTTP application. The API serves `/openapi.json` and a minimal `/console`; a dependency-free [TypeScript SDK](sdk/typescript) and [held-out pilot runner](examples/support-triage/evaluate.ts) are included. Migration 16 adds authenticated reviewers, typed corrections and pending/verified/disputed outcome observations. See [Tier 4 verification and real-pilot limitations](TIER4_VERIFICATION.md).

## What Decision Kernel Is & Is Not

```text
External model   =  Intelligence / Semantic judgment (bounded answers)
Decision Kernel  =  Decision logic / Constraints / Composition / Validation
Application      =  Execution / Live business actions
```

| Decision Kernel IS | Decision Kernel IS NOT |
| :--- | :--- |
| **A high-performance decision runtime** | A chatbot, conversational agent, or prompt chain |
| **A deterministic policy & graph evaluator** | An LLM gateway, proxy, or router |
| **Provider-neutral typed contracts** (Binary, Categorical, Ordinal) | An arbitrary code or bytecode sandbox |
| **Atomic cost & idempotency accounting in PostgreSQL** | A RAG pipeline or vector database |
| **Truthful uncertainty handler** (null probabilities on discrete labels) | A model trainer or weight-serving framework |
| **Headless service (`kerneld`) & management CLI (`kernelctl`)** | A user-facing UI or application backend |

> [!IMPORTANT]
> The Decision Kernel **never executes arbitrary business actions**, **never loads model weights**, and **never hallucinates probabilities** on discrete categorical or binary labels.

---

## System Architecture

```mermaid
flowchart TD
    subgraph Consuming Application
        App["Application / Client"]
    end

    subgraph Decision Kernel Surface
        Kerneld["kerneld (Axum HTTP Daemon)\nBearer Auth • Limit Ingress • Pinned Release"]
    end

    subgraph Runtime Orchestration
        Engine["kernel-runtime (Tokio)\nLane A / B / C Execution Engine"]
        DAG["Deterministic Graph Evaluator\nValidated Topological Order"]
        Cache["Exact Prediction Cache\nTenant & Scope Isolated"]
        Adapters["Semantic Provider Adapters\nBounded Timeout • Strict Normalization"]
    end

    subgraph External Intelligence
        Jev["System One REST API\n(TypeSafe Jev)"]
        LLM["Compatible Chat REST API\n(Schema-Constrained LLM)"]
    end

    subgraph Durable State
        PG[("PostgreSQL\nReceipts • Fencing Generations • Budgets\nPrediction Cache • Immutable Releases")]
    end

    App -->|"POST /v1/decisions (Idempotency-Key)"| Kerneld
    Kerneld --> Engine
    Engine -->|"Lane A: Pure Rules"| DAG
    Engine -->|"Lane B: Exact Hit"| Cache
    Engine -->|"Lane C: Unresolved"| Adapters
    Adapters --> Jev
    Adapters --> LLM
    Engine -->|"Atomic Spend & Claim"| PG
    DAG -->|"Receipt Commit"| Kerneld
    Kerneld -->|"Typed Decision Receipt"| App
```

---

## Crates & Layout

The workspace strictly conforms to the pure Rust boundary mandate (**DK-R01**):

| Crate / App | Path | Description |
| :--- | :--- | :--- |
| [`kernel-core`](crates/kernel-core) | `crates/kernel-core` | Provider-free contracts and typed values (`Categorical`, `Binary`, `Ordinal`, `NotEvaluated`), strict duplicate-key JSON parser, and deterministic policy precedence. |
| [`kernel-compiler`](crates/kernel-compiler) | `crates/kernel-compiler` | Validates declarative packs, checks node bounds and dependency types, topological sorting, and computes SHA-256 release content digests. |
| [`kernel-provider`](crates/kernel-provider) | `crates/kernel-provider` | Pluggable trait abstraction (`IntelligenceProvider`) with native `SystemOneProvider` (Choice/Noul/Score), `OpenAiCompatibleProvider` (JSON object mode with local validation), and `FixtureProvider`. |
| [`kernel-runtime`](crates/kernel-runtime) | `crates/kernel-runtime` | Tokio async orchestrator: `evaluate_pure` sub-millisecond evaluator, and `PostgresStore` for scoped atomic reservations, claims with fencing generations, and exact cache reuse. |
| [`kernel-evaluation`](crates/kernel-evaluation) | `crates/kernel-evaluation` | Offline evaluation engine: group-leakage validation, partition-aware testing (dev/calibration/test), binary Brier scores, and qualification criteria verification. |
| [`kerneld`](apps/kerneld) | `apps/kerneld` | Headless Axum HTTP daemon exposing decisions, evidence intake, reviews, outcomes, usage, and Kubernetes health probes. |
| [`kernelctl`](apps/kernelctl) | `apps/kernelctl` | Administrative CLI: migrations, pack validation, compilation, release publication, service token lifecycle, qualification publishing, charge reconciliation, replay, and benchmarks. |
| [`kernel-worker`](apps/kernel-worker) | `apps/kernel-worker` | PostgreSQL-backed deferred jobs, leased transactional outbox delivery, and conservative provider-attempt recovery. |

---

## Decision Policy Precedence & Execution Lanes

Decisions strictly follow a deterministic 5-step precedence hierarchy:

1. **Hard Security or Destination Deny**: Non-overridable security constraints dominate immediately. Disallowed provider calls are prevented entirely.
2. **Permission and Evidence Sufficiency**: Unknown policy facts (including missing, null, or mistyped permission), unresolved supplied evidence references, and invalid required evidence yield `Review`. Input identity retains every supplied reference.
3. **Deterministic Resolution**: If all required outputs are resolved by rule nodes and deterministic acceptance is enabled, disposition is `Accept` (0 model calls).
4. **Provider Validation / Qualification Check**: If external output is invalid, truncated, contains fabricated numbers, or the task is unqualified, disposition falls back to `Review` / `Abstain`.
5. **Qualified Semantic Acceptance**: Only granted if the behavior pack explicitly enables semantic acceptance **and** an active, unexpired PostgreSQL qualification record exists matching the release digest, provider, model, and adapter version.

### Execution Lanes
- **Lane A (Pure Deterministic Core)**: Evaluates precompiled DAG rules directly in memory (0 network I/O, 0 provider cost, sub-millisecond execution).
- **Lane B (Exact Prediction Reuse)**: Reuses exact matching semantic responses for identical state, evidence revisions, release digest, and model identity while rerunning policy checks.
- **Lane C (Semantic Evaluation)**: Dispatches only unresolved semantic questions to approved, registered provider adapters within a strict deadline and pre-allocated budget reservation.

---

## Release Acceptance Gates (PRD Conformance)

The local checks below verify selected behaviors. The full release gates remain open where they require PostgreSQL, live providers, concurrency, or end-to-end measurements. See the [test report](DECISION_ENGINE_TEST_REPORT.md) for commands, results, and limitations.

| Gate | Requirement | Verification Evidence | Status |
| :--- | :--- | :--- | :---: |
| **DK-R01 / Rust boundary** | Pure Rust implementation across daemon, CLI, runtime, and adapters. No Node.js, Python, or UI framework dependency in the Rust engine; optional SDK, console and verification clients are separate. | Offline Rust workspace build and tests passed. | **Local check passed** |
| **DK-R02 / Compiler** | Rejects DAG cycles, dangling edges, unknown node fields, and invalid types before publication. | Unit tests cover cycles, missing producers, and unknown fields. | **Partially verified** |
| **DK-R03 / Authority** | Hard deny strictly wins over missing evidence, high model confidence, or reviewer overrides. | `hard_deny_dominates_and_is_idempotent` passed. | **Partially verified** |
| **DK-R04 / Fast path** | Fully resolved rules make 0 provider calls; fast profile never exceeds 1 billable attempt. | `deterministic_answer_uses_zero_provider_calls` passed with 0 attempts. | **Partially verified** |
| **DK-R05 / Idempotency** | Concurrent equivalent requests share one operation; different request digests return HTTP 409. | PostgreSQL integration passed; concurrent HTTP behavior remains untested. | **Partially verified** |
| **DK-R06 / Accounting** | Atomic budget reservations prevent double-spend; timeouts preserve uncertain holds for operator audit. | PostgreSQL integration passed; crash and timeout paths remain untested. | **Partially verified** |
| **DK-R07 / Receipt** | Commit failure never acknowledges durable success; crash recovery preserves attempts and releases. | Database failure and recovery paths were not run in this check. | **Pending** |
| **DK-R08 / Uncertainty** | Discrete labels retain null probabilities; invalid distributions or fabricated numbers fail closed. | Core and provider validation unit tests passed. | **Local check passed** |
| **DK-R09 / Scope** | Cross-tenant evidence, prediction cache, replay, and token auth fail closed. | PostgreSQL integration passed; HTTP token isolation remains untested. | **Partially verified** |
| **DK-R10 / Behavior** | Pinned release replay is 100% reproducible for deterministic policy via `kernelctl replay`. | Example fixture replay matched 2/2 dispositions. | **Partially verified** |
| **DK-R11 / Portability** | Two genuinely independent semantic providers pass canonical task contracts. | Adapter unit tests passed; no live provider conformance run. | **Pending** |
| **DK-R12 / Performance** | Measured pure core p95 < 1 ms; measured audited DB p95 < 50 ms. | On 29 September 2026, pure core p95 was 0.2207 ms and audited DB p95 was 3.59 ms on a local test container. | **Local check passed** |

---

## Performance Acceptance Benchmarks (Section 17)

The 29 September 2026 local benchmark used the three-node example pack for 5,000 pure in-memory iterations and 200 audited PostgreSQL requests:

```sh
kernelctl bench packs/examples/support-triage.json
```

```text
=== Decision Kernel Performance Acceptance Benchmark (PRD Sec 17) ===
Target Pack:  support.triage@1.1.0
Content Hash: e9852a0f729d188c542938dc437e474ea481b48615cc03178567db77daff3e9f
Nodes: 3, Edges: 0

[Lane A: Pure Deterministic Core]
Iterations:        5000
Throughput:        6592.4 ops/sec
Latency p50:       0.1405 ms
Latency p95:       0.2207 ms  (Target: < 1.0 ms -> PASS)
Latency p99:       0.2905 ms

[Lane A: Audited No-Model Request (DB + Receipt Commit)]
Iterations:        200
Throughput:        293.1 req/sec
Latency p50:       3.18 ms
Latency p95:       3.59 ms  (Target: < 50.0 ms -> PASS)
Latency p99:       4.07 ms

=== Benchmark Completed Successfully ===
```

---

## HTTP API Reference (`kerneld`)

All endpoints require `Authorization: Bearer <kernel-service-token>` matching an active record in `service_tokens` (or static deployment token).

### 1. Execute Decision
`POST /v1/decisions`

Set `release` to `"active"` to resolve the scoped release pointer for this request. The receipt records the resolved `decision_type@version` and digest. A version string is accepted only while that version is active.

- Headers: `Idempotency-Key: <unique-key>`, `Authorization: Bearer <token>`
- Body:
```json
{
  "schema_version": "1.1",
  "product_id": "support-suite",
  "decision_type": "support.triage",
  "release": "support.triage@1.1.0",
  "idempotency_key": "ticket-789-v1",
  "mode": "evaluate",
  "execution_profile": "fast",
  "state": {
    "ticket_id": "789",
    "egress_denied": false
  },
  "evidence_refs": [
    { "id": "ev_msg_1", "revision": 1 }
  ],
  "constraints": {
    "deadline_ms": 1000,
    "max_billable_attempts": 1,
    "max_cost_nano_usd": 1000000
  }
}
```
- Response (`200 OK`):
```json
{
  "receipt_id": "rec_01a0d58b...",
  "scope": {
    "tenant_id": "acme-corp",
    "project_id": "customer-ops",
    "environment": "production",
    "product_id": "support-suite"
  },
  "release": "support.triage@1.1.0",
  "release_digest_sha256": "e9852a0f...",
  "answers": {
    "queue": {
      "kind": "categorical",
      "selected_id": "technical",
      "probabilities": null,
      "probability_provenance": null
    },
    "urgent": {
      "kind": "binary",
      "proposition": "needs_urgent_review",
      "value": true,
      "probability_true": null,
      "probability_provenance": null
    }
  },
  "disposition": "review",
  "reason_codes": ["unqualified_semantic"],
  "evidence_revisions": { "ev_msg_1": 1 },
  "provider_id": "system-one-primary",
  "model_id": "jev-1.13",
  "attempts": 1,
  "prediction_cache_hit": false,
  "cost_nano_usd": 15000,
  "created_at_unix_ms": 1790291041000,
  "kernel_compute_ms": 1,
  "db_claim_ms": 2,
  "provider_latency_ms": 140,
  "db_commit_ms": 3,
  "total_latency_ms": 146
}
```

### 2. Lookup Receipt
`GET /v1/decisions/:id`
Retrieves a previously committed decision receipt by idempotency key.

### 3. Ingest Evidence
`POST /v1/evidence`
Registers an authenticated, immutable evidence snapshot with SHA-256 digest validation:
```json
{
  "evidence_id": "ev_msg_1",
  "revision": 1,
  "observed_at_unix_ms": 1790290000000,
  "expires_at_unix_ms": 1790376400000,
  "digest_sha256": "3a7b...",
  "sensitivity": "internal",
  "value": { "body": "My database is failing with connection timeout" }
}
```

### 4. Record Human Review (Append-Only)
Requires an operator-provisioned `X-Kernel-Reviewer-Token`; identity comes from its scoped record.
`POST /v1/decisions/:id/reviews`
```json
{
  "disposition": "accept",
  "reason_code": "verified_classification",
  "corrections": {},
  "notes": "Verified technical escalation"
}
```

### 5. Record Observed Outcome (Append-Only)
`status` is `pending`, `verified`, or `disputed`. Verified/disputed observations require a reviewer token and verification reference.
`POST /v1/decisions/:id/outcomes`
```json
{
  "outcome": "ticket_resolved_first_contact",
  "status": "verified",
  "verification_ref": "zendesk_audit_1092"
}
```

### 6. Query Usage & Budgets
`GET /v1/usage`
Returns product/environment-scoped purpose-level budget caps, spent, held reservations, and attempt status counts (`dispatch_intent`, `uncertain`, `reconciled`).

### 7. Health Probes
- `GET /healthz` & `GET /health/live`: Liveness probe (`200 OK`).
- `GET /readyz` & `GET /health/ready`: Readiness probe verifying PostgreSQL connectivity.

---

## Operator CLI Reference (`kernelctl`)

```sh
# Database Schema Migrations (transactional, versioned, checksum-validated 0001 to 0013)
DATABASE_URL=postgres://... kernelctl migrate

# Validate & Compile Behavior Pack
kernelctl validate packs/examples/support-triage.json
kernelctl compile packs/examples/support-triage.json

# Publish Immutable Release to PostgreSQL
KERNEL_TENANT_ID=acme KERNEL_PROJECT_ID=ops KERNEL_ENVIRONMENT=prod \
DATABASE_URL=postgres://... kernelctl publish packs/examples/support-triage.json

# Inspect Published Compiled Graph
kernelctl inspect support.triage 1.1.0

# Activate a published release, then inspect its revision
kernelctl promote support.triage 1.1.0 0
kernelctl inspect-active support.triage

# Switch to another published version using the current revision from inspect-active
kernelctl promote support.triage 1.1.1 1

# Restore a previous published version using the new current revision
kernelctl rollback support.triage 1.1.0 2

# Revoke a release; if it is active, decisions fail closed until another is promoted
kernelctl revoke-release support.triage 1.1.1

# Service Token Management
KERNEL_NEW_SERVICE_TOKEN="super-secret-random-32-byte-token..." \
KERNEL_TENANT_ID=acme KERNEL_PROJECT_ID=ops KERNEL_ENVIRONMENT=prod KERNEL_PRODUCT_ID=suite \
DATABASE_URL=postgres://... kernelctl register-token

kernelctl revoke-token <sha256-hash>

# Offline Evaluation & Provider Qualification
kernelctl evaluate packs/examples/support-triage.json packs/examples/support-triage-evaluation.json

KERNEL_QUALIFICATION_APPROVER="Lead Risk Officer" \
DATABASE_URL=postgres://... kernelctl publish-qualification \
  packs/examples/support-triage.json \
  packs/examples/support-triage-evaluation.json \
  "RFC-2026-09-APPROVAL" \
  1798200000000

# Reconcile External Provider Charge
KERNEL_PRODUCT_ID="your-product" DATABASE_URL=postgres://... kernelctl reconcile-attempt \
  "ticket-789-v1" \
  15000 \
  "invoice-inv_2026_09_123"

# Replay Decisions for Deterministic Policy Verification (DK-R10)
kernelctl replay packs/examples/support-triage.json tests/support-triage-replay.json

# Run Performance Acceptance Benchmark (PRD Sec 17 / DK-R12)
kernelctl bench packs/examples/support-triage.json
```

---

## Quickstart & Verification

### 1. Build and Run Workspace Tests
```sh
cargo test --workspace
```

### 2. Run Full PostgreSQL Integration Test
With a local PostgreSQL database running:
```sh
DATABASE_URL="postgres://postgres:kernel_test@127.0.0.1:32770/kernel_test" \
cargo run -p kernelctl -- migrate
KERNEL_TEST_DATABASE_URL="postgres://postgres:kernel_test@127.0.0.1:32770/kernel_test" \
cargo test --workspace
```

The daemon requires a non-owner login granted `kernel_runtime` without `kernel_worker` membership. The worker requires a separate non-owner login granted `kernel_worker`; neither process accepts a superuser or BYPASSRLS login. Migrations and provisioning use separately controlled owner credentials. Scoped transactions set tenant, project, environment and product context for RLS. See the [Tier 0 setup and migration notes](TIER0_VERIFICATION.md).

`POST /v1/decision-jobs` accepts the same decision body with `execution_profile: "deferred"` and returns `202` with a stable `job_id`; `GET /v1/decision-jobs/{id}` returns its status and eventual receipt. The worker uses `DATABASE_URL` and the same `KERNEL_PROVIDER_*` settings as the daemon. Set `KERNEL_OUTBOX_URL` to an HTTPS sink and `KERNEL_OUTBOX_ALLOWED_HOST` to its exact hostname to deliver committed events; without them, events stay pending. Consumers must deduplicate by the `Idempotency-Key` event ID.

Evidence revisions are immutable. Repeating identical evidence returns `200`; new evidence returns `201`; changing an existing ID and revision returns `409 EVIDENCE_CONFLICT`. Sensitivity must be `public`, `internal`, `confidential`, or `restricted`.

### 3. Run Performance Benchmark
```sh
cargo run -p kernelctl -- bench packs/examples/support-triage.json
```

---

## License

Licensed under either of:
- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or http://www.apache.org/licenses/LICENSE-2.0)
- MIT license ([LICENSE-MIT](LICENSE-MIT) or http://opensource.org/licenses/MIT)

at your option.

## Tier 1: tenant/project BYOK

Provider connections and decision bindings live outside immutable decision packs.
Connections belong to a tenant/environment and optionally a project; an empty
stored project denotes a tenant connection. Bindings select an explicit connection
for a tenant/project/environment/product/decision type. Pack qualification fields
remain approval constraints: changing credentials or bindings cannot qualify a
new model or change a published release. API and deferred worker resolve the same
registry. Unbound decisions retain the existing deployment-provider fallback;
a missing, deleted or undecryptable **bound** connection returns an error without
falling back to another provider.

Apply migration 15 with `kernelctl migrate` before starting upgraded processes.
Configure these variables on API and worker (values below are descriptions):

| Variable | Purpose |
| --- | --- |
| `KERNEL_CONNECTION_MASTER_KEY` | 64 hex characters encoding a random 32-byte AES key, supplied by your secret manager; keep stable across restarts and shared between API and worker. |
| `KERNEL_PROVIDER_ALLOWED_HOSTS` | Comma-separated exact HTTPS hostnames approved by the operator, without spaces. User input cannot expand this list. |
| `KERNEL_CONNECTION_ADMIN_TOKEN` | Separate random administrative token of at least 32 characters, required on API management requests. |
| `KERNEL_PROVIDER_CA_CERT_PATH` | Optional PEM trust root for internal HTTPS providers, as with deployment configuration. |

Use a scoped service bearer token **and** `X-Kernel-Admin-Token` for every endpoint
below. Request bodies use strict JSON and reject unknown fields. API keys never
appear in metadata responses. Do not place keys in URLs or command-line arguments.

| Method and path | Operation |
| --- | --- |
| `GET /v1/provider-connections` | List project and tenant metadata, revisions and deletion status. |
| `POST /v1/provider-connections` | Create a connection; supply `expected_revision` to atomically replace configuration and rotate its API key. |
| `DELETE /v1/provider-connections/{id}` | Delete using `{"tenant_level":false,"expected_revision":2}`. |
| `POST /v1/provider-connections/{id}/test` | Live, budgeted probe using `{"tenant_level":false,"idempotency_key":"unique-probe-1"}`. |
| `PUT /v1/provider-bindings/{decision_type}` | Bind using `{"connection_id":"primary","tenant_level":false}`. |
| `DELETE /v1/provider-bindings/{decision_type}` | Remove binding, restoring deployment-provider fallback. |

A create or rotation body has this shape; obtain the real secret from your secret
manager and submit over your authenticated HTTPS API connection:

```json
{
  "connection_id": "primary",
  "tenant_level": false,
  "config": {
    "adapter": "compatible",
    "endpoint": "https://provider.example/v1",
    "provider_id": "my-provider",
    "model_id": "requested-model",
    "allowed_returned_models": ["served-model-revision"],
    "max_output_tokens": 512,
    "max_dispatch_nano_usd": 100000
  },
  "api_key": "<secret supplied at request time>"
}
```

For rotation, include `expected_revision` with the last listed revision and the
replacement key/configuration. Stale writes and recreation of deleted IDs return
409; create a new ID after deletion. Tenant connections use `tenant_level:true`
and can be explicitly bound by other projects within that tenant/environment.
Credentials are AES-256-GCM encrypted with a fresh random nonce and authenticated
scope/connection identity. Deletion nulls the active ciphertext and retains a
metadata tombstone; already admitted requests and historical database backups
require the provider's own revocation/retention controls. Master-key replacement
requires a planned decrypt/re-encrypt migration; simply changing the environment
key makes existing connections unreadable.

The `compatible` adapter appends `/chat/completions` to the configured base URL;
`system-one` appends `/v1/predictions`. Supply the base, not the full request path.
Both enforce HTTPS, exact host approval, certificate verification, no redirects,
absolute deadlines, a 64 KiB streamed response limit, and zero automatic retries.
Native predictions must match exactly the requested question set and declared
output types; generated probability/provenance fields are rejected. Both adapters
support explicit returned-model aliases. Receipts preserve the requested `model_id`
and actual `returned_model_id`, including requested identity on failed dispatches.

Connection tests exercise a binary typed response and return `connected`, adapter
capabilities, requested/served identity, receipt ID, idempotency key and reason
codes. Capabilities describe the adapter's supported binary/categorical/ordinal
contract and configured limits; a successful binary probe does not certify every
model capability or grant acceptance qualification. Provision a normal scoped
`budget_buckets` row with purpose `connection.test` before probing. The test uses
the existing reservation, durable attempt, idempotency, deadline, receipt and
reconciliation machinery, with an internal review-only operational graph. Use a
new probe idempotency key after rotation to make a fresh call. The test's receipt
can be fetched with `GET /v1/decisions/{idempotency_key}`.

Provider usage is restricted to numeric token counters; arbitrary provider strings
are discarded before persistence. Reflected credentials in other decoded response
fields are rejected while numeric billing evidence is retained. HTTP error bodies
are never exposed. Raw keys are excluded from packs, receipts, caches, metadata,
process logs and public errors.

Run `make tier1` against a disposable owner `KERNEL_TEST_DATABASE_URL`; this runs
Tier 0 regressions and `scripts/verify-tier1.py`. The Tier 1 proof uses synthetic
keys, separate API/worker logins and a local HTTPS fixture to check tenant/project
isolation, host approval, encryption, rotation conflicts, both adapters, requested
versus served models, API/queued binding resolution, deletion and secret redaction.
For container-only psql, set `KERNEL_TEST_PSQL_CONTAINER` to the test container name.
These checks do not establish live external-provider conformance.


## Tier 2: configurable decision packs

Developers define application behavior in JSON packs. The same runtime, API and
worker execute [expense approval](packs/examples/expense-approval.json),
[content moderation](packs/examples/content-moderation.json) and the existing
[support triage](packs/examples/support-triage.json) pack. No application-specific
engine implementation or callback is required.

`input_schema` validates the request's raw `state` before receipt claims, provider
resolution or deferred-job admission. It is a bounded schema language with strict
unknown-key rejection, rather than the full JSON Schema standard:

| Type | Contract |
| --- | --- |
| `object` | `properties`, `required`, `additional_properties` (defaults to false) |
| `string` | `values` (optional vocabulary), `min_length`, `max_length` (defaults to 4096), measured in characters |
| `number` | Optional finite `minimum` / `maximum` |
| `integer` | Signed 64-bit JSON integers, optional `minimum` / `maximum` |
| `boolean` | JSON booleans |
| `array` | `items` schema, `max_items` (defaults to 128) |

Schemas are limited to depth 8 and 128 schema nodes. Invalid types, missing required
fields and disallowed additional fields return HTTP 400. There is no implicit
string-to-number conversion. Existing packs may omit the schema to preserve their
original input behavior.

`state_preparation` uses `version: "kernel/state-v1"` and at most 32 ordered steps:
`copy` (`from`, `to`), `default` (`field`, `value`, applied only when absent), and
`normalize_string` (`field`, optional `lowercase`). Normalization trims ASCII
whitespace and optionally lowercases ASCII letters. Sources must exist and
normalization requires a string; preparation errors return HTTP 400. Steps operate
on a copy, never mutate the raw request, and cannot read clocks, networks or run
scripts. Schema validation precedes preparation so derived fields need not appear
in the input schema. Both deterministic rules and the provider's explicit
`semantic_state_fields` projection use prepared state. The entire preparation
specification is bound into the immutable release digest and prediction cache
identity; deferred jobs pin that digest. The raw request remains the idempotency
identity.

`required_evidence` declares required IDs. Optional `evidence_requirements` maps
those IDs to `value_schema` and optional positive `max_age_ms`. Evidence must also
pass the existing digest, observation-time and expiry checks. Missing, mistyped,
stale or corrupt evidence prevents acceptance and uses `missing_evidence` fallback.
Only verified required evidence with an allowed sensitivity can reach a provider.

The graph supports `fact_equals`, finite numeric `fact_compare` (`less`,
`less_or_equal`, `greater`, `greater_or_equal`), `evidence_present`, `boolean_and`,
`boolean_or`, and single-dependency `boolean_not`. Missing or mistyped facts stay
unknown through boolean composition. `depends_on` gates output producers. Every
output has exactly one producer: `literal_answer`, `state_answer` (a prepared
boolean or vocabulary/anchor string), or `semantic_question`. Questions inherit
binary propositions, categorical vocabularies and ordinal scales from the declared
output. Invalid state-derived answers remain unevaluated and cannot accept.

Policy precedence is explicit:

1. Any true `hard_deny_nodes` predicate yields **deny**.
2. Any true `abstain_nodes` predicate yields **abstain**.
3. Any true `review_nodes` predicate yields **review**.
4. Unknown policy predicates or missing/invalid evidence prevent acceptance.
5. Acceptance requires all `accept_nodes`, all outputs evaluated, and either
   `accept_deterministic` or the existing active qualification gate for
   `accept_qualified_semantic`.
6. Remaining cases use the declared fallback.

Deny, abstain and review rules stop semantic dispatch. `policy.fallback` explicitly
sets `provider_failure`, `missing_evidence` and `unresolved`, each to `review` or
`abstain`; neither value can fabricate an accepted answer. Omitting fallback keeps
the original review behavior. Provider failure includes missing/unavailable
providers, timeouts, invalid responses, unavailable budget and dispatch deadline
exhaustion. Unqualified or incomplete answers use `unresolved`. Explicit policy
review retains precedence over fallback. An overall request deadline or process
crash still follows the existing HTTP timeout and durable recovery behavior.

Provider responses must match the exact question-ID set, provider/model identity,
answer kinds, propositions, labels, scales and probability contracts. Invalid
responses are discarded as a batch and never cached. Every output is present in
the receipt; unresolved outputs have an explicit `not_evaluated` reason. Label-only
answers retain null probabilities. Semantic results still require a reviewed,
unexpired release-bound qualification before automatic acceptance.

Use the existing release lifecycle with the new packs:

```sh
cargo run --locked -p kernelctl -- validate packs/examples/expense-approval.json
cargo run --locked -p kernelctl -- compile packs/examples/expense-approval.json
# Set DATABASE_URL and KERNEL_TENANT_ID / KERNEL_PROJECT_ID / KERNEL_ENVIRONMENT.
cargo run --locked -p kernelctl -- publish packs/examples/expense-approval.json
cargo run --locked -p kernelctl -- promote expense.approval 1.0.0 0
# Publish a changed pack with a new version, then compare-and-swap revision 1.
cargo run --locked -p kernelctl -- promote expense.approval 2.0.0 1
cargo run --locked -p kernelctl -- rollback expense.approval 1.0.0 2
```

Publication compiles first, rejects duplicate JSON keys and invalid contracts,
refuses version reuse with changed content, and is idempotent for identical
content. Promotion/rollback require a published, unrevoked target and the current
pointer revision. Rollback emits `release.rolled_back`; promotion emits
`release.promoted`. New requests resolve the current pointer; queued requests
retain their pinned release. Existing packs retain their serialized form and
content digest when Tier 2 fields are omitted.

Run `make tier2` with a disposable `KERNEL_TEST_DATABASE_URL` to run both previous
tiers, workspace tests, multi-pack API/worker lifecycle proof and replay. The
Tier 2 proof uses synthetic data and no external provider. See
[TIER2_VERIFICATION.md](TIER2_VERIFICATION.md) for coverage and reproduction.

### Tier 3 performance contract

`make tier3` runs the preceding correctness tiers plus release-mode HTTP rules,
exact-cache and fresh-provider load tests, duplicate suppression, connection reuse,
overload and worker pickup checks. The initial contract uses 100 requests/second,
3,000 requests per path: rules/cache HTTP p95 ≤25 ms and p99 ≤50 ms; fresh-provider
engine residual p95 ≤5 ms and p99 ≤10 ms, with full non-provider HTTP overhead
separately gated at 25/50 ms. Successful decision responses expose component
`Server-Timing` measurements; every response exposes HTTP wall time.

See [TIER3_VERIFICATION.md](TIER3_VERIFICATION.md) for the exact supported workload,
timing definitions, configurable admission/pool limits, limitations and measured
p95/p99 evidence. These are workload-specific acceptance gates, not an unrestricted
production latency SLA.


## Tier 5 — production operations and improvement

See [operations runbook](docs/production-operations.md) for digest-only images and
rollout, separate migration/API/worker credentials, probes, protected metrics,
alerts, scoped shared RPM limits, financial controls, backup/restore, rollback,
retention/export/deletion and held-out semantic acceptance gates.
`make tier5` includes all preceding tiers and disposable PostgreSQL recovery drills;
set `KERNEL_TEST_DATABASE_URL` and `KERNEL_TEST_PSQL_CONTAINER`. Image smoke/rollback
proof additionally uses `scripts/verify-tier5.py --images CANDIDATE_MANIFEST
--baseline-images PREVIOUS_MANIFEST` after packaging. Migration 17 must precede
service startup. Exact request/evidence snapshots are now retained for offline
`kernelctl compare-stored`; legacy receipts without recorded inputs fail explicitly.
Read [Tier 5 verification](TIER5_VERIFICATION.md) for measured evidence and remaining
production qualification boundaries. Semantic auto-acceptance remains disabled
for the unqualified support pilot.
