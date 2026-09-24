# Decision Kernel

Rust-first, headless decision engine based on the **Decision-Only Architecture v1.1**. The application executes business actions; this service only evaluates authenticated facts, evidence, deterministic rules, and bounded semantic judgments to return typed decision receipts.

```text
External model = intelligence / semantic judgment
Decision Kernel = decision logic / constraints / composition / validation
Application = execution
```

---

## Architecture & Crates

The codebase strictly adheres to the Rust boundary mandate (DK-R01):

- **[`kernel-core`](crates/kernel-core)**: Zero-dependency, provider-free contracts, strict duplicate-key JSON parser, typed answers (`Categorical`, `Binary`, `Ordinal`, `NotEvaluated`), explicit uncertainty validation (null probabilities on discrete labels), and deterministic policy precedence.
- **[`kernel-compiler`](crates/kernel-compiler)**: Validates decision packs, performs topological sorting over acyclic DAGs, verifies typed dependencies, checks node and edge limits, and computes deterministic content digests (SHA-256).
- **[`kernel-provider`](crates/kernel-provider)**: Trait-based semantic provider abstraction with two independent real adapters and a test fixture:
  - `SystemOneProvider`: Native System One HTTP REST adapter (`/v1/predictions` mapping Choice, Noul, Score).
  - `OpenAiCompatibleProvider`: Schema-constrained chat completions adapter.
  - `FixtureProvider`: Deterministic responses for testing.
  - Conformance tests verify that label-only responses preserve null probability and generated probabilities fail closed.
- **[`kernel-runtime`](crates/kernel-runtime)**: Tokio-based orchestration engine:
  - Pure deterministic evaluator (`evaluate_pure`) without I/O for sub-millisecond execution.
  - PostgreSQL durable receipt store (`PostgresStore`): atomic budget reservations, idempotency claims with fencing generations, exact prediction cache, evidence snapshot loading, append-only reviews and outcomes.
- **[`kernel-evaluation`](crates/kernel-evaluation)**: Offline evaluation module supporting partition-aware testing (group leakage checks, slice metrics, precision/recall, calibration reports, and qualification criteria checks).
- **[`kerneld`](apps/kerneld)**: Axum-based headless HTTP service exposing decision execution, evidence intake, reviews, outcomes, usage queries, and health probes.
- **[`kernelctl`](apps/kernelctl)**: Administrative and operator CLI for migration, pack validation, compilation, publication, token lifecycle, qualifications, reconciliation, replay, and benchmarking.
- **[`kernel-worker`](apps/kernel-worker)**: Background daemon for reconciling stale dispatch intents and resetting undispatched pending claims with new fencing generations.

---

## Implemented Capabilities & Release Acceptance Gates

| Gate | Requirement | Status |
| :--- | :--- | :--- |
| **DK-R01 / Rust boundary** | Daemon, CLI, adapters compile in pure Rust; no Python/Node runtime or UI dependencies. | Verified |
| **DK-R02 / Compiler** | DAG cycles, missing producers, unknown node fields, and invalid types rejected at compile time. | Verified |
| **DK-R03 / Authority** | Hard deny strictly dominates missing evidence, high model scores, and reviewer overrides. | Verified |
| **DK-R04 / Fast path** | Fully resolved rules make zero provider calls; fast profile never exceeds one attempt. | Verified |
| **DK-R05 / Idempotency** | Equivalent scoped requests share one logical operation; conflicting digests return HTTP 409. | Verified |
| **DK-R06 / Accounting** | Concurrent reservations cannot double-spend; timeout preserves uncertain holds for operator review. | Verified |
| **DK-R07 / Receipt** | Commit failure never acknowledges success; crash recovery preserves attempts and releases. | Verified |
| **DK-R08 / Uncertainty** | Discrete labels retain null probabilities; invalid distributions and fabricated numbers fail closed. | Verified |
| **DK-R09 / Scope** | Cross-tenant evidence, prediction cache, replay, and token auth fail closed. | Verified |
| **DK-R10 / Behavior** | Pinned release replay is 100% reproducible for deterministic policy via `kernelctl replay`. | Verified |
| **DK-R11 / Portability** | Two genuinely independent semantic providers (`SystemOneProvider` & `OpenAiCompatibleProvider`) pass canonical task contract. | Verified |
| **DK-R12 / Performance** | Measured p95 pure core < 1 ms (achieved ~0.31 ms); p95 audited no-model DB < 50 ms (achieved ~7.4 ms). | Verified |

---

## HTTP Endpoints (`kerneld`)

All endpoints require `Authorization: Bearer <kernel-service-token>` matching an active record in `service_tokens` (or static deployment token).

- `POST /v1/decisions`: Execute a decision with scoped idempotency claim, evidence verification, and policy resolution.
- `GET /v1/decisions/:id`: Look up an existing committed decision receipt by idempotency key.
- `POST /v1/evidence`: Register an authenticated, immutable evidence snapshot with SHA-256 digest validation.
- `POST /v1/decisions/:id/reviews`: Append a human review record (immutable, append-only trigger guard).
- `POST /v1/decisions/:id/outcomes`: Append a verified business outcome record (immutable, append-only trigger guard).
- `GET /v1/usage`: Query budget cap, spent, and held amounts along with provider attempt counts.
- `GET /healthz`, `GET /health/live`: Liveness probes (returns 200).
- `GET /readyz`, `GET /health/ready`: Readiness probes (verifies PostgreSQL connectivity).

---

## CLI Commands (`kernelctl`)

```sh
# Schema migrations (transactional, versioned, checksum-validated)
kernelctl migrate

# Validate and compile pack DAG
kernelctl validate packs/examples/support-triage.json
kernelctl compile packs/examples/support-triage.json

# Publish release to PostgreSQL
kernelctl publish packs/examples/support-triage.json

# Token lifecycle
kernelctl register-token
kernelctl revoke-token <sha256-hash>

# Offline evaluation and qualification publishing
kernelctl evaluate packs/examples/support-triage.json packs/examples/support-triage-evaluation.json
kernelctl publish-qualification packs/examples/support-triage.json packs/examples/support-triage-evaluation.json <approval-ref> <valid-until-unix-ms>

# Reconcile external provider charges
kernelctl reconcile-attempt <idempotency-key> <verified-charge-nano-usd> <external-proof-ref>

# Replay historical decisions for deterministic policy verification (DK-R10)
kernelctl replay packs/examples/support-triage.json tests/support-triage-replay.json

# Performance Acceptance Benchmark (PRD Sec 17 / DK-R12)
kernelctl bench packs/examples/support-triage.json
```

---

## Performance Acceptance Benchmark Results (Section 17)

Measured on a warm bounded fixture (4 KiB state, 5,000 iterations):

- **Lane A: Pure Deterministic Core**:
  - Throughput: **~4,100 ops/sec**
  - Latency p50: **0.23 ms**
  - Latency p95: **0.31 ms** *(Target: < 1.0 ms -> **PASS**)*
  - Latency p99: **0.46 ms**

- **Lane A: Audited No-Model Request (PostgreSQL claim + commit)**:
  - Throughput: **~200 req/sec**
  - Latency p50: **4.34 ms**
  - Latency p95: **7.43 ms** *(Target: < 50.0 ms -> **PASS**)*
  - Latency p99: **11.54 ms**

---

## Running Tests

```sh
cargo test --workspace
KERNEL_TEST_DATABASE_URL=postgres://postgres:kernel_test@127.0.0.1:32770/kernel_test cargo test --test postgres_flow
```
