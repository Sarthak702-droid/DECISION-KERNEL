# Decision Kernel test report

## 29 September 2026 — PRD v2.0 implementation audit

The latest local run passed `cargo fmt --all --check`, `cargo clippy --workspace --all-targets --offline -- -D warnings`, `git diff --check`, and `KERNEL_TEST_DATABASE_URL=... cargo test --workspace --offline` against the dedicated PostgreSQL test database. The test run included compiler, provider, runtime, tenant RLS, release pointer, deferred job, and outbox tests. `cargo deny check licenses bans sources` passed. The full advisory gate did not run because its upstream advisory fetch failed; `cargo-audit` was unavailable locally.

| PRD gate | Local evidence and current limit |
| --- | --- |
| DK-G01 Rust boundary | Rust workspace builds and tests pass locally. |
| DK-G02 Compiler | Cycle, missing producer, unknown field, and unreachable node tests pass. Full compiler optimization and all limits still need acceptance coverage. |
| DK-G03/G04 authority and fast path | Hard-deny and zero-provider-call tests pass. |
| DK-G05 idempotency | PostgreSQL receipt and deferred-job conflict tests pass. Concurrent HTTP duplicate test is pending. |
| DK-G06 budget | Atomic scoped reservation integration test passes. Hierarchical buckets, explicit reservation lifecycle, and concurrent race proof remain pending. |
| DK-G07 durability | Leased jobs and outbox with generation fencing pass local database tests. Crash injection and uncertain-charge reconciliation proof remain pending. |
| DK-G08 uncertainty | No fabricated probability tests pass. Uncertainty v2 and calibrator artifacts remain pending. |
| DK-G09 tenant isolation | RLS with nonowner runtime role, cross-tenant rows, transaction context reuse, and cross-product receipt/review/outcome access pass local PostgreSQL tests. Export isolation cannot be verified until export exists. |
| DK-G10 replay | The small deterministic replay fixture passes. Fresh semantic replay API is pending. |
| DK-G11 provider portability | Router requested/returned model validation and qualification binding have tests. Two independent live qualified providers have not been demonstrated. |
| DK-G12 performance | Earlier local pure-core and audited DB microbenchmarks passed their targets. Reproducible load, saturation, and production-environment runs remain pending. |
| DK-G13 release operations | Local PostgreSQL tests cover active pointer pinning, promotion revision conflict, rollback, and revocation. |
| DK-G14 router identity | Returned-model allowlist and exact qualification matching pass tests. Live router qualification remains pending. |
| DK-G15 backup/restore | No executed restore drill or measured RPO/RTO. |
| DK-G16 retention/export | No tenant export or deletion propagation implementation. |

The v2.0 PRD is **not complete or production-ready**. Additional open work includes Evidence/Receipt v2 and state-builder semantics, structured adjudication and outcome lifecycle, hierarchical budgets, calibration and qualified evaluation slices, OpenAPI/SDK, observability/runbooks, container packaging, retention/export, backup restore, and live-provider acceptance. A local passing suite does not close these gates. The dated sections below are historical snapshots; their descriptions of router support and migration status have been superseded by this audit.

## 29 September 2026 follow-up

The dedicated local PostgreSQL test container passed `cargo test --workspace -- --nocapture` with `KERNEL_TEST_DATABASE_URL` set, including the receipt/accounting integration test and the release pointer promotion test. `cargo clippy --workspace --all-targets -- -D warnings` also passed. Migration 0008 applied through `kernelctl migrate`. A CLI smoke test published two releases, promoted and rolled back the active pointer, rejected a stale revision, and showed a revoked active release as revoked. The test used an isolated tenant scope in the local test database.

The 200-request audited no-model benchmark measured 3.59 ms p95 on the local PostgreSQL container, below the 50 ms target. The 5,000-iteration pure core benchmark measured 0.2207 ms p95, below the 1 ms target. These measurements do not establish production load performance or live-provider behavior. No provider credential was available in this session, so the earlier OpenRouter result was not repeated.

## 28 September 2026 baseline

**Date:** 28 September 2026  
**Revision:** `7f86ad7`  
**Scope:** Local policy engine verification and OpenRouter `typesafe/jev-router` smoke tests.

## Manager summary

The decision engine's **local policy behavior passed the tests run here**: hard deny takes precedence, deterministic acceptance avoids provider calls, unqualified semantic output stays in review, provider output validation rejects invalid probability claims, and the example pack replay matched 2/2 recorded dispositions. The pure in-memory benchmark completed 5,000 iterations with **0.2105 ms p95** latency against its **<1 ms** target. These results establish rule execution and local validation behavior, not end-to-end production readiness or model decision accuracy.

**Live OpenRouter connectivity is verified for text.** Authenticated text and JSON-object requests to `POST https://openrouter.ai/api/v1/chat/completions` both returned HTTP 200. The text request returned `OK` in 4.096 seconds; the JSON-object request returned `{"ok":true}` in 4.381 seconds. Both responses reported `model: openai/gpt-6-luna` and `finish_reason: stop`, although the requested model was `typesafe/jev-router`. The current Decision Kernel adapter rejects that model ID mismatch. No multimodal or end-to-end engine decision was tested. The supplied API key is intentionally absent from this file.

## Evidence from this run

| Check | Command / source | Observed result | Meaning |
| --- | --- | --- | --- |
| Workspace tests | `cargo test --workspace --offline` | Exit 0; 16 unit tests passed; 1 PostgreSQL test reported `ok` but returned early because `KERNEL_TEST_DATABASE_URL` was unset | Local unit behavior passed; PostgreSQL integration was **not run** |
| Pack validation | `cargo run --offline -q -p kernelctl -- validate packs/examples/support-triage.json` | Exit 0; digest `e9852a0f729d188c542938dc437e474ea481b48615cc03178567db77daff3e9f` | Example pack compiles and validates |
| Deterministic replay | `cargo run --offline -q -p kernelctl -- replay packs/examples/support-triage.json tests/support-triage-replay.json` | `2 matched, 0 mismatched` | Hard deny and missing-evidence review fixtures reproduced |
| Pure engine benchmark | `cargo run --offline -q -p kernelctl -- bench packs/examples/support-triage.json` | 5,000 iterations; 6,391.2 ops/sec; p50 0.1459 ms; p95 0.2105 ms; p99 0.3365 ms | Pure in-memory path met the documented p95 target on this machine |
| Audited DB benchmark | Same command | DB section absent (`DATABASE_URL` unset) | No current DB latency measurement |
| Live OpenRouter text smoke test | Authenticated `curl` POST requesting `typesafe/jev-router` | HTTP 200; `OK`; returned model `openai/gpt-6-luna`; `finish_reason: stop`; 4.096 s | API key and text route worked in this run; no accuracy conclusion |
| Live OpenRouter JSON-object smoke test | Authenticated `curl` POST with `response_format: {"type":"json_object"}` | HTTP 200; `{"ok":true}`; returned model `openai/gpt-6-luna`; `finish_reason: stop`; 4.381 s | JSON-object mode worked for this request; adapter's exact-model validation would still reject it |

The test count excludes documentation test targets that had zero tests. The PostgreSQL test is written to return successfully when its URL is missing, so its `ok` line is not evidence of database behavior.

## What decision power is demonstrated

- **Policy precedence:** `crates/kernel-runtime/src/lib.rs` checks hard deny before evidence sufficiency and provider dispatch. The `hard_deny_dominates_and_is_idempotent` test verifies `deny`, a stable receipt ID, and zero provider calls.
- **Fast deterministic decisions:** `deterministic_answer_uses_zero_provider_calls` verifies `accept` with zero attempts when rules fully resolve the output.
- **Conservative semantic decisions:** `label_only_semantic_result_stays_review_only_and_uncalibrated` verifies that a semantic label alone does not grant acceptance or create a fabricated probability. The example `support.triage` pack explicitly sets `accept_qualified_semantic: false`, so its semantic results are review only by design.
- **Typed response guardrails:** `kernel-provider` tests reject fabricated probabilities and truncated compatible-chat responses; `kernel-core` tests reject invalid distributions and duplicate JSON keys.
- **Reproducibility:** The example replay reproduces two expected dispositions. This is a small policy fixture, not an accuracy evaluation on real support tickets.

**Not established here:** Real-world classification accuracy, calibration, error rates, OpenRouter reliability, multimodal understanding, live tenant isolation, database durability, audited latency, and qualified semantic auto-acceptance. Those require a labeled evaluation set, adapter-compatible provider behavior, and a PostgreSQL test environment.

## Which code to use

For a direct OpenRouter call, the supplied `curl` request is sufficient once the API key is set and the media inputs are valid. For this Rust project, **`OpenAiCompatibleProvider`** in `crates/kernel-provider/src/lib.rs` is the integration starting point, but it currently rejects Jev Router responses because OpenRouter reports the underlying routed model. A direct API or SDK call does not return a Decision Kernel policy-controlled receipt. The supplied audio string is truncated example data, and the linked image/video are remote assets; that request would not measure the engine's policy decisions.

The compatible adapter builds `BASE_URL/chat/completions`, sends a bounded JSON-object request, and validates a typed `answers` map. A deployment configuration for OpenRouter would start with:

```sh
export KERNEL_PROVIDER_ADAPTER=compatible
export KERNEL_PROVIDER_BASE_URL=https://openrouter.ai/api/v1
export KERNEL_PROVIDER_ALLOWED_HOST=openrouter.ai
export KERNEL_PROVIDER_API_KEY="$OPENROUTER_API_KEY"
export KERNEL_PROVIDER_ID=openrouter
export KERNEL_PROVIDER_MODEL=typesafe/jev-router
```

Also set the service's PostgreSQL and release variables described in `README.md`. The pack qualification identity must match `openrouter` and `typesafe/jev-router`; the current example pack names `example-provider` and `example-model`, so **it will not dispatch to OpenRouter as-is**. Publishing a modified pack and qualification is required before an end-to-end semantic decision. The adapter additionally requires the response `model` field to equal the configured ID exactly and `finish_reason` to be `stop`. The live router returned the underlying model ID `openai/gpt-6-luna`, so the current adapter rejects it when configured as `typesafe/jev-router`. JSON-object mode succeeded in the direct API smoke test. Supporting the router in Decision Kernel requires an explicit requested-versus-actual model identity design, including qualification and cache rules; accepting any returned model under a single qualification would weaken the existing safety boundary.

To smoke-test the provider from a network-enabled host without storing the key in a repository file:

```sh
read -rs OPENROUTER_API_KEY
export OPENROUTER_API_KEY
curl --fail-with-body --show-error --max-time 30 \
  https://openrouter.ai/api/v1/chat/completions \
  -H 'Content-Type: application/json' \
  -H "Authorization: Bearer $OPENROUTER_API_KEY" \
  -d '{"model":"typesafe/jev-router","messages":[{"role":"user","content":"Return exactly the word OK."}]}'
```

The provided audio example is incomplete: its base64 decodes to 25 bytes while its WAV header declares 1,666 bytes. Use a complete audio file for a multimodal test. The model page documents audio, image, and video inputs, but that behavior was not tested here.

## Next acceptance checks

1. Rotate the API key shared in chat, then inject its replacement through a secret manager or environment variable.
2. Decide how Decision Kernel should record and qualify a router's requested ID and actual served model; implement and test that policy before a `kerneld` integration run.
3. Run a multimodal API test with complete audio data and verify the remote image and video assets are accessible.
4. Run `KERNEL_TEST_DATABASE_URL=... cargo test --offline --test postgres_flow -p kernel-runtime` against a dedicated PostgreSQL database, plus the DB benchmark with `DATABASE_URL` set.
5. Evaluate a labeled, held-out support-ticket set with expected queue, urgency, and review/deny outcomes. Report confusion matrices, coverage, error rate, and latency by decision lane before claiming business decision quality.

OpenRouter's [Jev Router model page](https://openrouter.ai/typesafe/jev-router) documents the model ID, chat-completions endpoint, and multimodal request example. The [structured outputs documentation](https://openrouter.ai/docs/guides/features/structured-outputs) describes model-dependent JSON response support.
