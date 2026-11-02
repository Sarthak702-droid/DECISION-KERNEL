# Tier 2 verification — 2 November 2026

Implemented the configurable decision engine on top of the existing Tier 0/1
storage, provider and release infrastructure. The completion condition is proven:
expense approval, content moderation and support triage run through the same
engine, daemon and worker using JSON packs, with no application-specific engine
code.

## Implementation

- Bounded, recursive input schemas validate raw state before side effects in pure
  evaluation, runtime execution, synchronous API admission and deferred admission.
- `kernel/state-v1` performs ordered copy, absent-field default and ASCII string
  normalization. Rules and semantic projections use prepared state; the original
  input remains the idempotency identity. Preparation is release-bound.
- Evidence requirements add typed value schemas and maximum age to the existing
  integrity, observation-time, expiry and sensitivity checks.
- Numeric comparisons, boolean negation and typed state-derived answers extend
  the existing deterministic DAG and typed semantic questions.
- Policy precedence is deny, explicit abstain, explicit review, safety gates,
  then eligible acceptance. Failure fallback explicitly selects review or abstain
  for provider failures, missing evidence and unresolved cases.
- All binary/categorical/ordinal output contracts remain validated. Malformed
  provider batches cannot populate answers or prediction cache. Unevaluated
  outputs have explicit receipt entries; semantic acceptance retains qualification.
- Pack CLI ingestion rejects duplicate JSON keys. Publication remains immutable
  and content-idempotent; promotion and rollback use pointer revision checks.
  Rollback has a distinct `release.rolled_back` outbox event.
- Omitted Tier 2 fields do not change legacy pack serialization or release digests.

## Reproduction

Use a disposable PostgreSQL owner database; the tests create synthetic roles and
records and start local API, worker and HTTPS fixture processes. No external
provider keys or paid provider requests are needed.

```sh
export KERNEL_TEST_DATABASE_URL='postgres://postgres:kernel_test@127.0.0.1:5432/kernel_test'
make tier2
```

When `psql` is only available inside the disposable database container, also set
`KERNEL_TEST_PSQL_CONTAINER` to that container's name. CI runs `make tier2` against
its isolated PostgreSQL service.

The actual run used a fresh `postgres:16-alpine` container bound to loopback port
32768. Full output is saved in
[audit/2026-11-02-tier2/verification.log](audit/2026-11-02-tier2/verification.log).
The container was disposable; reproduce against a new database rather than relying
on that port remaining available.

## Results

`make tier2` passed, including:

| Check | Result |
| --- | --- |
| Rust formatting, clippy with warnings denied, locked workspace build | Passed |
| Idempotent migrations (run twice) | Passed |
| Workspace Rust tests, including real PostgreSQL tests | 39 passed |
| New Tier 2 runtime/contract tests (included above) | 9 passed |
| Tier 0 isolated API/worker/HTTPS regression checks | 15 passed |
| Tier 1 isolated BYOK regression checks | 8 passed |
| Tier 2 multi-pack API/worker/lifecycle checks | 6 passed |
| Support triage replay | 2/2 matched |
| Content moderation replay | 2/2 matched |

The nine Tier 2 tests cover nested input types and bounds, input rejection,
preparation reproducibility and digest binding, evidence type/age/integrity,
three output kinds, all dispositions and their precedence, provider suppression
by explicit policy, unknown negation, invalid state-derived answers, invalid pack
contracts and legacy serialization. Controlled semantic providers exercise valid
batches, timeouts, unavailability, invalid responses, unknown labels, missing and
extra answers, wrong kinds and wrong model IDs. Invalid batches remain uncached;
valid label-only results remain review-only without qualification.

The six end-to-end checks establish:

1. Strict validation, immutable publication, identical-content republishing and
   rejection of invalid release versions.
2. The generic expense pack emits accept/review/deny/abstain and all three answer
   kinds, with zero provider attempts.
3. Three application packs execute through the same API; absent semantic providers
   use the moderation pack's declared abstain fallback.
4. Invalid input returns HTTP 400 before receipt claims or deferred jobs exist.
5. Promotion rejects stale revisions; a queued v1 expense decision still completes
   with v1 behavior after v2 becomes active.
6. Rollback restores v1 behavior, emits the rollback event and rejects promotion
   to revoked v2.

## Supported boundaries

The input schema language is the documented bounded subset, not full JSON Schema.
Preparation has deterministic built-in operations and cannot execute custom code.
Numeric comparisons use finite f64 values; integer input validation uses signed
64-bit JSON integers. Time-dependent evidence validity is checked at evaluation
using the current clock. Overall request timeouts and crash recovery retain the
existing transport/durable recovery behavior. This verification uses local fixture
providers; it does not establish live external-provider qualification or broader
production performance gates.
