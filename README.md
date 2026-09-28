# Decision Kernel

[![Rust](https://img.shields.io/badge/rust-stable-brightgreen.svg?logo=rust)](https://www.rust-lang.org/)
[![License](https://img.shields.io/badge/license-Apache%202.0%20%2F%20MIT-blue.svg)](#license)
[![Architecture](https://img.shields.io/badge/architecture-Decision--Only%20v1.1-purple.svg)](#core-principles)
[![Verification](https://img.shields.io/badge/release%20gates-in%20progress-yellow.svg)](#release-acceptance-gates)
[![Deterministic Core](https://img.shields.io/badge/pure%20core%20p95-0.2105%20ms-informational.svg)](#performance-acceptance-benchmarks)

A Rust-first, headless decision engine based on the **Decision-Only Architecture v1.1**. Local policy checks pass; the database and live-provider release checks described below remain open.

Applications provide authenticated facts, permitted evidence, and a published decision specification. External intelligence APIs (TypeSafe System One, schema-constrained LLMs) supply bounded semantic judgments when needed. The Rust kernel validates those judgments, combines them with deterministic rules and policy constraints, and returns an immutable, typed decision receipt. The consuming application alone executes business actions.

---

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
        DAG["Deterministic Graph Evaluator\nTopological Order • Constant Folding"]
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
| [`kernel-core`](crates/kernel-core) | `crates/kernel-core` | Provider-free contracts, zero-dependency types (`Categorical`, `Binary`, `Ordinal`, `NotEvaluated`), strict duplicate-key JSON parser, and deterministic policy precedence. |
| [`kernel-compiler`](crates/kernel-compiler) | `crates/kernel-compiler` | Validates declarative packs, checks node bounds and dependency types, topological sorting, constant folding, and computes SHA-256 release content digests. |
| [`kernel-provider`](crates/kernel-provider) | `crates/kernel-provider` | Pluggable trait abstraction (`IntelligenceProvider`) with native `SystemOneProvider` (Choice/Noul/Score), `OpenAiCompatibleProvider` (JSON schema mode), and `FixtureProvider`. |
| [`kernel-runtime`](crates/kernel-runtime) | `crates/kernel-runtime` | Tokio async orchestrator: `evaluate_pure` sub-millisecond evaluator, and `PostgresStore` for scoped atomic reservations, claims with fencing generations, and exact cache reuse. |
| [`kernel-evaluation`](crates/kernel-evaluation) | `crates/kernel-evaluation` | Offline evaluation engine: group-leakage validation, partition-aware testing (dev/calibration/test), Brier scores, calibration diagnostics, and qualification criteria verification. |
| [`kerneld`](apps/kerneld) | `apps/kerneld` | Headless Axum HTTP daemon exposing decisions, evidence intake, reviews, outcomes, usage, and Kubernetes health probes. |
| [`kernelctl`](apps/kernelctl) | `apps/kernelctl` | Administrative CLI: migrations, pack validation, compilation, release publication, service token lifecycle, qualification publishing, charge reconciliation, replay, and benchmarks. |
| [`kernel-worker`](apps/kernel-worker) | `apps/kernel-worker` | Conservative background reconciliation loop marking stale dispatch intents uncertain while retaining budget holds and resetting retryable claims. |

---

## Decision Policy Precedence & Execution Lanes

Decisions strictly follow a deterministic 5-step precedence hierarchy:

1. **Hard Security or Destination Deny**: Non-overridable security constraints dominate immediately. Disallowed provider calls are prevented entirely.
2. **Evidence Sufficiency**: Missing, expired, or digest-mismatched required evidence yields `Review` (or `Abstain`). Missing evidence is never masked by model scores.
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
| **DK-R01 / Rust boundary** | Pure Rust implementation across daemon, CLI, runtime, and adapters. Zero Node.js, Python, or UI framework dependencies. | Offline Rust workspace build and tests passed. | **Local check passed** |
| **DK-R02 / Compiler** | Rejects DAG cycles, dangling edges, unknown node fields, and invalid types before publication. | Unit tests cover cycles, missing producers, and unknown fields. | **Partially verified** |
| **DK-R03 / Authority** | Hard deny strictly wins over missing evidence, high model confidence, or reviewer overrides. | `hard_deny_dominates_and_is_idempotent` passed. | **Partially verified** |
| **DK-R04 / Fast path** | Fully resolved rules make 0 provider calls; fast profile never exceeds 1 billable attempt. | `deterministic_answer_uses_zero_provider_calls` passed with 0 attempts. | **Partially verified** |
| **DK-R05 / Idempotency** | Concurrent equivalent requests share one operation; different request digests return HTTP 409. | PostgreSQL integration and concurrent HTTP behavior were not run in this check. | **Pending** |
| **DK-R06 / Accounting** | Atomic budget reservations prevent double-spend; timeouts preserve uncertain holds for operator audit. | PostgreSQL integration was not run in this check. | **Pending** |
| **DK-R07 / Receipt** | Commit failure never acknowledges durable success; crash recovery preserves attempts and releases. | Database failure and recovery paths were not run in this check. | **Pending** |
| **DK-R08 / Uncertainty** | Discrete labels retain null probabilities; invalid distributions or fabricated numbers fail closed. | Core and provider validation unit tests passed. | **Local check passed** |
| **DK-R09 / Scope** | Cross-tenant evidence, prediction cache, replay, and token auth fail closed. | PostgreSQL integration was skipped because `KERNEL_TEST_DATABASE_URL` was unset. | **Pending** |
| **DK-R10 / Behavior** | Pinned release replay is 100% reproducible for deterministic policy via `kernelctl replay`. | Example fixture replay matched 2/2 dispositions. | **Partially verified** |
| **DK-R11 / Portability** | Two genuinely independent semantic providers pass canonical task contracts. | Adapter unit tests passed; no live provider conformance run. | **Pending** |
| **DK-R12 / Performance** | Measured pure core p95 < 1 ms; measured audited DB p95 < 50 ms. | Pure core p95 was 0.2105 ms; audited DB benchmark was not run. | **Partially verified** |

---

## Performance Acceptance Benchmarks (Section 17)

The 28 September 2026 local benchmark used the three-node example pack for 5,000 pure in-memory iterations. The audited PostgreSQL path requires `DATABASE_URL` and was not measured in that run:

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
Throughput:        6391.2 ops/sec
Latency p50:       0.1459 ms
Latency p95:       0.2105 ms  (Target: < 1.0 ms -> PASS)
Latency p99:       0.3365 ms

=== Benchmark Completed Successfully ===
```

---

## HTTP API Reference (`kerneld`)

All endpoints require `Authorization: Bearer <kernel-service-token>` matching an active record in `service_tokens` (or static deployment token).

### 1. Execute Decision
`POST /v1/decisions`
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
`POST /v1/decisions/:id/reviews`
```json
{
  "reviewer_id": "operator_42",
  "disposition": "accept",
  "notes": "Verified technical escalation"
}
```

### 5. Record Observed Outcome (Append-Only)
`POST /v1/decisions/:id/outcomes`
```json
{
  "outcome": "ticket_resolved_first_contact",
  "verified": true,
  "verification_ref": "zendesk_audit_1092"
}
```

### 6. Query Usage & Budgets
`GET /v1/usage`
Returns purpose-level budget caps, spent, held reservations, and attempt status counts (`dispatch_intent`, `uncertain`, `reconciled`).

### 7. Health Probes
- `GET /healthz` & `GET /health/live`: Liveness probe (`200 OK`).
- `GET /readyz` & `GET /health/ready`: Readiness probe verifying PostgreSQL connectivity.

---

## Operator CLI Reference (`kernelctl`)

```sh
# Database Schema Migrations (transactional, versioned, checksum-validated 0001 to 0007)
DATABASE_URL=postgres://... kernelctl migrate

# Validate & Compile Behavior Pack
kernelctl validate packs/examples/support-triage.json
kernelctl compile packs/examples/support-triage.json

# Publish Immutable Release to PostgreSQL
KERNEL_TENANT_ID=acme KERNEL_PROJECT_ID=ops KERNEL_ENVIRONMENT=prod \
DATABASE_URL=postgres://... kernelctl publish packs/examples/support-triage.json

# Inspect Published Compiled Graph
kernelctl inspect support.triage 1.1.0

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
DATABASE_URL=postgres://... kernelctl reconcile-attempt \
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
KERNEL_TEST_DATABASE_URL="postgres://postgres:kernel_test@127.0.0.1:32770/kernel_test" \
cargo test --test postgres_flow
```

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
