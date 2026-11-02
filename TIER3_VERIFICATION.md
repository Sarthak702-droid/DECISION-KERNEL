# Tier 3 — measured millisecond performance

The Tier 3 implementation provides separate rules, exact-cache and fresh-provider
measurements, fixed percentile gates, bounded admission, transport reuse, and
concurrent equivalent-prediction suppression. The contract below is an initial
repository acceptance target; it is not a production SLA or a user-approved target
for other hardware, graph sizes, provider services, or sustained workloads.

## Supported benchmark workload and fixed gates

Run optimized Rust binaries against PostgreSQL 16 on loopback. Each path receives
3,000 new idempotency keys at an open-loop 100 requests/second for 30 seconds, with
32 HTTP driver threads, persistent connections, service-token database authentication,
active-release lookup, 64 admitted API requests, 16 provider permits and 16 API database
connections. Thirty deterministic requests and one semantic cache seed warm the paths;
cold startup is outside this contract. Each request still durably claims, saves a
crash-recovery receipt, commits its receipt and writes its outbox events.

| Path | Supported fixture | p95 target | p99 target |
| --- | --- | ---: | ---: |
| Rules, scheduled-client HTTP wall time | Expense pack: 8 nodes, 3 typed outputs, schema validation, 5 preparation steps, 1 stored evidence reference | 25 ms | 50 ms |
| Exact prediction cache, scheduled-client HTTP wall time | 2 nodes, 1 binary semantic output, 1 KiB semantic payload, same semantic identity with new receipt keys | 25 ms | 50 ms |
| Fresh provider, engine residual | Same semantic pack, unique semantic state on every request, local HTTPS provider | 5 ms | 10 ms |
| Fresh provider, entire HTTP overhead excluding provider | Per-request `http - provider`; includes database, queueing and response construction | 25 ms | 50 ms |
| Idle worker, enqueue to observed completion | One worker, 100 ms poll, 30 sequential expense jobs including idle pickup | — | 500 ms |

The provider fixture deliberately waits 20 ms per call; provider time also includes
TLS/HTTP, response-body reads and adapter normalization. Its observed duration is
not a model inference target. Rules/cache checks require zero provider attempts;
fresh checks require exactly one attempt and no cache hit. Cache results remain
review-only unless the existing qualification checks allow acceptance.

The load driver submits on a fixed clock instead of waiting for previous responses.
Client latency starts at the intended submission time, so driver/executor scheduling
backlog is included. Scheduling delay, statuses and completion throughput are retained.
Percentiles use nearest rank over individual observations. Worker p99 is the maximum
of only 30 observations; it proves this bounded idle-pickup scenario, not busy-queue
capacity. No steady-state throughput beyond 30 seconds is claimed.

## Instrumentation and implementation

`Engine::decide_measured` returns the decision result and a separate `Timings` object,
including failed/deadline executions. Nanosecond accumulators produce microsecond
values for `engine_us`, `database_us`, `provider_us`, `queue_us`, `total_us` and
`store_calls`. Drop guards account for an await cancelled by a deadline.

Storage time covers every `ReceiptStore` call, including pool acquisition, claim,
recovery save, budget reserve, cache read/write, qualification check and commit/outbox
transaction. It includes store-side serialization and result materialization; it is
storage-operation wall time, not PostgreSQL server execution time. `store_calls`
counts logical calls, not individual SQL statements. Provider time covers the bounded
adapter evaluation. Queue time covers provider permits, prediction coordination and
cross-process claim polling. Engine time is the remaining orchestration wall time,
including scheduler delay; it is not CPU profiling.

Successful decision HTTP responses carry `Server-Timing` entries `engine`, `db`,
`provider`, `queue`, `runtime`, `prelude` and `http` in milliseconds with three decimal
places. HTTP `db` also includes auth/release/evidence/provider-binding storage
operations before runtime entry; HTTP `engine` also includes the remaining handler
prelude. `runtime` and `prelude` are diagnostic overlapping totals: do not add them
to their component fields. `http` includes body parsing and response construction,
but ends before socket transmission. Scheduled-client latency includes transmission
and client scheduling as well. All responses, including admission/deadline errors,
carry `http`; errors before runtime entry have no component breakdown. Receipt IDs,
billing and existing receipt payloads remain durable execution records: replay timing
is invocation-local and must be read from headers rather than original receipt fields.
The legacy nullable `db_commit_ms`/`total_latency_ms` receipt fields are not the source
of this performance report. The legacy `kernel_compute_ms` now excludes all measured
storage/provider/queue waits before commit rather than subtracting only claim time.
Workers emit structured `decision_job_timing` records with the same runtime breakdown;
the harness retains these alongside observed enqueue-to-completion measurements.

Changes reducing overhead:

- Validate/serialize the request identity once in the runtime, and serialize cache
  identity directly without allocating an intermediate JSON object tree. The versioned
  cache-key encoding intentionally makes older entries miss safely.
- Combine receipt fencing, atomic budget reservation and dispatch-intent insertion
  into one SQL statement inside the existing scoped transaction. This removes two
  network round trips while holding the shared budget row; reservation caps and
  generation fencing remain enforced by PostgreSQL.
- Skip provider-binding database lookup for graphs without semantic operations.
  Evidence references continue to use one batched lookup. Release status, authorization,
  durable receipt claims and qualification are still checked on each applicable request.
- Share a bounded pool of HTTP transports across adapter constructions, including BYOK.
  Authentication stays on individual requests. Transport identity includes trust-root
  bytes; CA rotation does not reuse an old trust configuration. At most eight transport
  configurations are retained, with 16 idle connections per host and a 90-second idle
  timeout. Endpoint validation, host allowlists, TLS verification and redirect rejection
  remain enforced.
- Use 4,096 fixed coordination stripes from prediction lookup through cache publication. Equivalent
  concurrent requests on one process wait for the leader's commit and cache write, then
  re-read and validate the durable response, expiry, scope, release/provider identity,
  output shape and qualification. Scope/product, semantic input, evidence digest and
  revision, release hash, preparation, questions, adapter and returned-model allowlist
  are part of cache identity. Fresh time is used for the cache expiry lookup after waiting.
  A hash collision may serialize unrelated inputs; it cannot reuse another answer.
  Failed/invalid/unevaluated responses are not shared as successful predictions.
  Cross-process identical idempotency keys retain existing durable claim suppression;
  different keys across processes can still dispatch duplicate equivalent predictions.
- Admit decision requests before authentication and other database work. The owned permit
  stays with the bounded spawned attempt across client cancellation. Configurable limits
  reject invalid/zero/excessive values at startup:

| Setting | Default | Allowed |
| --- | ---: | ---: |
| `KERNEL_REQUEST_CONCURRENCY` | 256 | 1–4096 |
| `KERNEL_PROVIDER_CONCURRENCY` | 16 | 1–256 |
| `KERNEL_DB_CONNECTIONS` | 16 | 1–256 |
| `KERNEL_WORKER_POLL_MS` | 100 | 10–10000 |

The first three settings apply to the daemon; worker execution remains serial per
worker process. Polling replaces the previous 10-second idle sleep. Lower intervals
increase idle database traffic; choose them with the desired pickup envelope.

## Reproduce

Use a disposable owner database: the verification scripts create synthetic roles,
scopes, receipts, budgets, jobs and events. Never point them at production data.

```sh
export KERNEL_TEST_DATABASE_URL='postgres://postgres:kernel_test@127.0.0.1:5432/kernel_test'
# Optional, if psql is available only in the disposable container:
# export KERNEL_TEST_PSQL_CONTAINER=your_disposable_postgres
make tier3
```

`make tier3` runs the complete Tier 0–2 regression target, builds release binaries,
then runs the performance gates. To rerun only performance after migration/build:

```sh
python3 scripts/verify-tier3.py --requests 3000 --rps 100 --concurrency 32 \
  --output audit/2026-11-02-tier3/results.json
```

The harness uses a local synthetic HTTPS fixture and no external paid provider.
Changing load flags changes the supported workload; publish them with results.
Run on deployment hardware before extending the contract. The script exits nonzero
when a fixed gate fails, a request fails, a path dispatches unexpectedly, connection
reuse is absent, duplicates dispatch more than once, or provider concurrency exceeds
its limit.

## Verification evidence

Measured on November 2, 2026, on an Intel Core i7-1065G7 (8 logical CPUs),
Linux x86_64, Rust 1.98.1 release build and an isolated `postgres:16-alpine`
container. API, database, driver and HTTPS fixture shared the host. Default database
durability remained enabled. Slow-query diagnostics were enabled to locate the
reservation bottleneck; the final run retained that diagnostic configuration.

| Path / component | p95 measured | p99 measured | Result |
| --- | ---: | ---: | --- |
| Rules scheduled-client HTTP | 7.497 ms | 9.935 ms | PASS |
| Cache scheduled-client HTTP | 8.145 ms | 14.935 ms | PASS |
| Fresh engine residual | 0.177 ms | 0.212 ms | PASS |
| Fresh entire non-provider HTTP overhead | 16.629 ms | 19.615 ms | PASS |
| Fresh storage-operation wall time | 16.318 ms | 19.262 ms | Recorded separately |
| Fresh provider/adapter wall time | 62.531 ms | 62.778 ms | Recorded separately |
| Fresh scheduled-client HTTP | 79.270 ms | 82.902 ms | Recorded separately |
| Worker enqueue to observed completion | 48.640 ms | 68.126 ms | PASS |

Rules and cache each completed 3,000 successful requests with zero provider attempts;
all 3,000 fresh requests dispatched exactly once. The load test includes maxima and
individual observations: these percentile targets do not promise that every request
finishes below p99. Cache maximum was 86.030 ms despite its passing p99.

Additional assertions passed:

- Sixteen simultaneous equivalent predictions with distinct idempotency keys produced
  one provider call and fifteen exact-cache followers.
- 3,001 HTTPS provider calls (seed plus fresh phase) used nine TCP connections.
- A 64-request saturation burst with admission 4/provider limit 2 returned four 200s
  and sixty explicit 429s, no unexpected statuses, and at most two active provider calls.
  Rejection p99 was 23.449 ms.
- Thirty worker completions include thirty independently recorded runtime timing objects.
- Formatting, strict Clippy, workspace tests, all prior HTTP/BYOK/recovery scenarios and
  both replay suites passed. Existing reservation race and stale-generation tests passed
  after the SQL change.

The initial 500-request pilot passed, but extending to 3,000 exposed hot budget-row
contention: fresh non-provider HTTP p99 reached 741.770 ms in the first sustained run
and 358.876 ms with diagnostics. The engine residual still met its target. Slow-query
and lock-wait records identified the budget UPDATE. Combining fencing, reservation and
intent insertion removed two lock-held round trips; the final identical-load gate
passed at 19.615 ms. Both failing runs are retained; thresholds were unchanged.

Evidence:

- [Final raw results and per-request component timings](audit/2026-11-02-tier3/results.json)
- [Final benchmark log](audit/2026-11-02-tier3/benchmark.log)
- [Complete correctness regression log](audit/2026-11-02-tier3/regression.log)
- [Reservation race/fencing regression log](audit/2026-11-02-tier3/reservation-regression.log)
- [First sustained failing run](audit/2026-11-02-tier3/long-run-before-results.json)
- [Diagnostic failing run](audit/2026-11-02-tier3/diagnostic-before-results.json)
- [Database slow-query and lock-wait evidence](audit/2026-11-02-tier3/database-before.log)

CI now invokes `make tier3`, so the workload and percentile gates are executable
acceptance checks. The CI workflow edit was checked locally; a remote CI run was not
started from this workspace. For release qualification, retain a fresh run from the
intended deployment hardware and provider configuration.
