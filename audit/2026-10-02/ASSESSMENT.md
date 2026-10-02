# Decision Kernel: initial-plan comparison and readiness assessment

Assessment date: 2 October 2026. Baseline: [Decision_Kernel_PRD_v0.1.docx](/home/soumyajeet/Downloads/Decision_Kernel_PRD_v0.1.docx), version 0.1 dated 22 September 2026. Implementation: `/home/soumyajeet/Projects/DECISION-KERNEL`, branch `main`, commit `969e922a3628063358d7c34e521d8a53e32db766`, plus the working-tree changes present when this assessment began.

**Verdict: a useful engineering prototype, with significant implementation safeguards, but not ready for the original K0 internal pilot or production.** The largest change is the shift from a portable intelligence-and-feedback kernel to a Rust, headless, decision-only runtime. This is a defensible architecture choice, but the initial plan's real product integration, trustworthy correction/outcome loop, semantic replay, and operating recovery proof remain incomplete. Performance alone does not close those gaps.

This assessment treats the uploaded document as requirements, not as instructions to execute. The repository's later v1.1/v2.0 gate names do not replace the original DK-01–DK-20 baseline. No production code was changed, committed, deployed, or repaired. Existing edits were preserved. Only assessment artifacts and evidence were added under this directory.

## 1. Decision by check

| Check | Assessment | Reason |
|---|---|---|
| Engineering readiness | Prototype usable for isolated development | Workspace builds; configured tests, migrations, deterministic replay fixtures, and selected PostgreSQL/HTTP checks pass. Important end-to-end invariants still fail. |
| Original K0 pilot readiness | Not ready | Native provider contract mismatch, no real product loop, product scope violations, broken runtime-role writes, incomplete correction/replay, and no staging rollback or backup restore proof. |
| Production readiness | Not ready | Reproducible committed startup failure, false-positive readiness, incomplete crash recovery/deadlines, failing supply-chain checks, and missing immutable deployment/restore/retention/observability controls. |
| Technical feasibility | Feasible for bounded advisory decisions, conditional on remediation | Rust/PostgreSQL are suitable foundations. The local deterministic measurements support this narrow path. Real semantic quality, provider quotas, regional handling, and sustained mixed-workload capacity remain unverified. |
| Product/commercial feasibility | Unproven | No integrated consumer, representative held-out results, reviewer workload measurements, measured quality/cost advantage, adoption time, or willingness-to-pay evidence. |
| Security readiness | Fails the required shared-runtime boundary | Authenticated cross-product receipt exposure and data reuse are reproduced. Permission-unknown semantics and feedback authenticity fail. Cross-tenant read checks did pass; this audit does not claim an unauthenticated or cross-tenant exploit. |

K0 means an internal advisory pilot in the initial PRD. It still requires a complete working loop and restore evidence. K1 requires hardened tenancy, qualification, retention, and a second independent real semantic provider. Protected actuation and signed packages belong to K2; their absence is appropriate deferred scope, not a K0 regression.

## 2. What changed from the initial plan

Classification: **strengthening** means an explicit, concrete safeguard or tighter implementation beyond the plan's level of detail; **preserved** means a planned requirement is substantially implemented; **deviation** means a design/scope change with a tradeoff; **gap/degradation** means a planned capability or guarantee is absent or weakened. A requirement appearing in source is not automatically a verified release gate.

| Area / initial PRD requirement | Current implementation | Classification and consequence |
|---|---|---|
| §3–4 portable intelligence plus accumulating outcome-linked evidence | Typed assessment engine, durable records, but no complete correction-to-candidate-replay-to-promotion loop | Scope narrowing / gap. The runtime is substantial; the product learning thesis is not demonstrated. |
| §4.3 TypeScript contracts/runtime, Fastify, Next administrative UI | Rust workspace, Axum daemon, administrative CLI, worker; no UI or TypeScript SDK | Deviation. Strong typing and a small executable surface are useful; direct TS package adoption and the pilot inspector are lost. REST can bridge languages once contracts stabilize. Rust is not inherently a production upgrade. |
| Framework/provider-independent core | Contracts do not import provider SDKs; independent evaluation crate | Preserved. The core does depend on serde/serde_json/thiserror, contrary to README's “zero-dependency” wording. |
| §5 categorical/binary/ordinal/not-evaluated contracts; no invented probabilities | Closed typed answers, distribution validation, null probabilities for discrete fallback labels | Preserved, with concrete validation strengthening. Native probability support is not operational in the System One adapter. |
| §6 bounded, immutable behavior artifacts | Bounded DAG compiler, single producers, cycle/type/unreachable checks, SHA-256 artifacts, revalidation | Strengthening in implementation detail. No arbitrary tenant executable code. README's constant-folding claim is not implemented. |
| Pack input schema, deterministic state builder, independent intent/provider binding, retention and test artifacts | Inline prompts and binding/qualification fields; semantic state field allowlist; no input schema or versioned state-builder artifact | Gap. Authenticated facts remain caller-supplied JSON; prompt/intent/provider artifacts cannot evolve independently as described. |
| Release lifecycle: draft/validated/shadow-approved/enforcement-approved/deprecated/revoked | Publish, active pointer with CAS revision, rollback/revoke, pinned inflight release, commit-time revocation check | Preserved core / narrower lifecycle. Pointer controls are useful; formal states and staged qualification workflow are missing. |
| §7 evaluate/shadow/enforce, explicit dependent semantic stages | Only `evaluate` and `fast`; one provider call, independent unresolved questions; semantic-answer dependencies rejected | Deviation / reduced capability. Safe bounded execution, but routing/agent evaluation requiring dependent questions needs product orchestration or explicit stages. |
| One shared deadline over the decision pipeline | Provider/semaphore waits bounded; auth/release/evidence and DB awaits outside the effective bound | Degradation. A 50 ms request took 1,219 ms under a controlled DB lock. |
| Tenant/product/operation idempotency with input digest | Receipt keys use tenant/project/environment/key; core digest excludes product and drops unresolved evidence refs | Defect. Cross-product replay exposure and changed-input equivalence are reproduced. |
| Durable jobs, attempts, leases, bounded retry/dead letters | PostgreSQL jobs/outbox with fenced leases and retries; deferred single-request jobs | Preserved core / narrower batch API. Invalid schema can be queued; paid crash recovery does not reach terminal status after reconciliation. |
| Evidence provenance, product/purpose/permission, trust, lineage, completeness | Tenant/project/environment, revision/digest/time/sensitivity/value; product absent; missing refs silently disappear | Gap and security defect. Required named evidence does produce review, but complete reference/provenance accounting is missing. |
| Deterministic minimization and omission/redaction report | Allowlisted semantic state fields and sensitivity gating; no explicit omission/redaction report | Partial. Useful egress control, insufficient lineage/completeness evidence. |
| Exact prediction cache, current policy recheck, scoped freshness | Exact hashed cache and current policy rerun, model/adapter/release/evidence identities, expiry | Preserved with a product-scope defect. Cache material omits product and has no explicit cross-product sharing grant. |
| Worst-case cost reservation and hierarchical/time-window budgets | Atomic purpose bucket holds and attempt records, request/pack/platform minimum; cap amount is not priced provider work | Partial with a material degradation. Atomic holds work, but do not guarantee the dispatched request can stay below the caller's cost ceiling. |
| Preserve unknown provider charges and reconcile | Unknown holds remain held; CLI accepts verified charge/proof reference | Preserved conservative accounting, incomplete recovery. Charges above the reserved hold cannot be reconciled; reconciled crashed receipts stay pending. |
| §12 one real Jev semantic adapter plus deterministic/fixture K0 | System One and compatible chat code, fixture provider; deterministic graph path | Gap. System One endpoint/request/response differ from the published native API; no live conformance evidence. Fixtures are not a second independent model. |
| Adapter manifests for modality/context/probabilities/batch/usage/cancel/region/model | Limited question/batch/output-token capabilities and explicit requested/served model identities | Partial. Exact model allowlists and qualification binding strengthen identity control; capacity/cost/region semantics remain incomplete. |
| Workload-specific held-out qualification and calibrated uncertainty | Group leakage checks, reviewed test labels, per-slice/class metrics and binary Brier count; exact identity/expiry/approval qualification | Partial. No calibrator/reliability bins, full confusion/macro/risk-coverage/interval/ordinal-distance analysis or locked representative corpus. |
| §10 unknown predicates remain unknown; missing permission means no permission | `FactEquals` maps missing values to false; vacuous accept predicates can permit a compiler-valid pack | Degradation. Explicit false is denied in the audit pack; omitted permission is accepted. This is pack-pattern dependent. |
| Structured semantic corrections, original answer retained, authenticated reviewer | Append-only reviewer string/disposition/note; no corrected typed answer; caller chooses reviewer identity | Gap. Original receipt is retained, but semantic correction and reviewer authenticity are absent. |
| Pending/verified/disputed/unavailable outcomes, evidence/source/attempt/times/retractions/dedup | Free-text outcome, bool `verified`, optional proof; duplicate unproved “verified” reports accepted | Gap. Collected feedback cannot safely be treated as outcome ground truth. |
| Ordered versioned event envelopes and asynchronous hooks | Atomic receipt/review/outcome outbox, HTTPS sink, redirect disabled, leased retry/dead-letter | Preserved durability; narrower event contract. No demonstrated per-aggregate ordering or sequence/schema/correlation envelope. |
| §16 HTTP releases/replays and typed SDK deadlines/cancel/compatibility | Admin CLI for releases/qualification/replay; GET receipt uses idempotency key rather than returned receipt ID; no SDK/OpenAPI | Deviation / contract gap. CLI separation is useful, but product integration must consciously adopt the changed interface. |
| §19 three reference packs, one real integration and full pilot loop | One support-triage example, two replay cases, tiny supplied-prediction evaluation dataset; no EvalMesh/JevRoute/QueuePilot integration | Gap. Contract breadth and useful application behavior are not demonstrated. |
| Pilot encrypted bounded persistence; K1 derivative deletion | Plain JSONB payloads, append-only deletion guards, cache expiration lookup; no lifecycle purge/deletion or encryption evidence | Gap. Immutable metadata needs a separately controlled payload deletion design. |
| Compose API/worker/DB + reverse proxy, distinct credentials | Untracked development Compose supplies only PostgreSQL; Makefile runs binaries with owner credentials and static dev token | Deviation / production gap. Development convenience is not an approved deployment topology. |
| Immutable images, SBOM, secret/dependency checks, staging image+behavior rollback, restore | Rust/Postgres CI, pinned checkout action, migrations/replay, dependency jobs; no production image/promotion/restore pipeline | Partial. Dependency jobs currently fail; secret scanning and full deployment proof are absent. |
| §17 measured warm core/HTTP/jobs/load/recovery targets | Fast local pure/owner-DB deny-path samples; no 30-minute HTTP load, mixed provider load, queue SLO, or restore | Partial evidence. Narrow benchmark results must not become capacity or availability claims. |
| §21 measured product benefit, provider-switch cost, reviewer load, unit economics | No live consumer or comparison data | Unproven feasibility. The original adoption and commercial assumptions remain hypotheses. |
| K2 actuation/signed packages; K3 learned specialization | Decision-only, no business actuation, training or GPU stack | Appropriate deferral. Preserves K0's no-irreversible-action boundary and avoids unnecessary platform expansion. |

The strongest implementation improvements are the bounded declarative compiler, exact served-model allowlists, transactional migration checksums, fenced leases, and conservative preservation of uncertain charges. Many other good features—durable receipts, hard-deny precedence, exact caching, CAS release pointers—were already required by the initial plan; they should be credited as implementation progress rather than scope upgrades.

## 3. Verified defects and security findings

Priorities: **P1** must be addressed before a real shared K0 pilot; **P2** must be addressed before the relevant production or K1 claim. These are remediation priorities, not CVSS scores. Reproductions used synthetic data and isolated PostgreSQL. Owner-role diagnostic HTTP checks reveal application logic; they do not certify least-privilege deployment. No paid external calls or external business actions occurred.

### F01 — P1: duplicate POST exposes another product's receipt

Two authenticated tokens with the same tenant/project/environment but different products submitted identical inputs and the same idempotency key. Product B received HTTP 200 with Product A's receipt and scope. Explicit GET using Product B correctly returned 404. The insert/claim path omits product and operation; the request digest also omits scope. This additionally causes cross-product conflicts for different inputs.

Source: [receipt key](/home/soumyajeet/Projects/DECISION-KERNEL/migrations/0001_init.sql:1), [claim](/home/soumyajeet/Projects/DECISION-KERNEL/crates/kernel-runtime/src/postgres.rs:344), [existing-result return](/home/soumyajeet/Projects/DECISION-KERNEL/crates/kernel-runtime/src/lib.rs:608). Evidence: [owner HTTP probes](/home/soumyajeet/Projects/DECISION-KERNEL/audit/2026-10-02/http-probes-owner.json).

Fix scope consistently across receipt/attempt uniqueness, claim lookup, digest, reconciliation, worker and cache. Include product and operation or an explicit authorized sharing boundary; validate scope before returning any existing receipt. Test the full token HTTP path for identical and differing requests across products.

### F02 — P1: the documented runtime database role cannot commit decisions

A genuine NOSUPERUSER/NOBYPASSRLS login granted `kernel_runtime` returned 503 for a valid decision; `/readyz` returned 200. The role has only SELECT on `behavior_releases`, but receipt commit executes SELECT … FOR SHARE, which requires an UPDATE privilege. A direct role-scoped query reproduced `permission denied for table behavior_releases`. Existing integration execution uses the owner; its non-owner checks only read records.

Source: [role grants](/home/soumyajeet/Projects/DECISION-KERNEL/migrations/0010_tenant_rls.sql:54), [commit lock](/home/soumyajeet/Projects/DECISION-KERNEL/crates/kernel-runtime/src/postgres.rs:432), [readiness](/home/soumyajeet/Projects/DECISION-KERNEL/apps/kerneld/src/main.rs:242). Evidence: [non-owner HTTP](/home/soumyajeet/Projects/DECISION-KERNEL/audit/2026-10-02/http-probes-nonowner.json), [direct permission probe](/home/soumyajeet/Projects/DECISION-KERNEL/audit/2026-10-02/followup-probes.json).

Design the lock/privilege boundary narrowly and prove full API and worker writes with separate non-owner credentials. Do not work around this by running production as the table owner. PostgreSQL documents owner/superuser RLS bypass; successful owner tests therefore provide limited isolation evidence. [PostgreSQL RLS documentation](https://www.postgresql.org/docs/16/ddl-rowsecurity.html).

### F03 — P1: System One adapter does not implement the published native contract

Code posts to `/v1/predictions`, sends an array of questions, and expects `predictions` with boolean/anchor values. The official reference specifies `/v1/systemone`, keyed questions with instructions/criteria, and typed `answers`. A documented Noul-shaped response is rejected by the current normalizer. It also rejects native probability-bearing responses. This is a functional mismatch, not evidence that a live provider was contacted or charged. An undocumented private proxy could adapt this format, but no such proxy is present in the repository. [Official TypeSafe API reference](https://docs.typesafe.ai/api).

Source: [endpoint](/home/soumyajeet/Projects/DECISION-KERNEL/crates/kernel-provider/src/lib.rs:306), [normalizer](/home/soumyajeet/Projects/DECISION-KERNEL/crates/kernel-provider/src/lib.rs:325), [request mapping](/home/soumyajeet/Projects/DECISION-KERNEL/crates/kernel-provider/src/lib.rs:442). Evidence: [synthetic Rust probes](/home/soumyajeet/Projects/DECISION-KERNEL/audit/2026-10-02/rust-probes.log).

Implement and version the current native contract, preserve native uncertainty provenance, and qualify binary/categorical/ordinal mapping against recorded official-format fixtures followed by a separately authorized, budgeted live run. Keep fail-closed normalization.

### F04 — P1: a cost ceiling is checked after dispatch, without pricing the work

The engine reserves the numeric cap, not a worst-case priced request. The compatible adapter independently requests up to 512 output tokens and discards provider usage/cost. A fixture reporting charge 100 with caller cap/reservation 1 was dispatched once, then classified invalid. The receipt lost the reported charge and the attempt remained uncertain. CLI reconciliation refused charge 100 because it exceeds the hold. The fixture proves accounting behavior; it is not a real invoice.

Source: [cap intersection](/home/soumyajeet/Projects/DECISION-KERNEL/crates/kernel-runtime/src/lib.rs:625), [reservation and post-call check](/home/soumyajeet/Projects/DECISION-KERNEL/crates/kernel-runtime/src/lib.rs:774), [chat limits](/home/soumyajeet/Projects/DECISION-KERNEL/crates/kernel-provider/src/lib.rs:234), [reconciliation rejection](/home/soumyajeet/Projects/DECISION-KERNEL/apps/kernelctl/src/main.rs:249). Evidence: [Rust probe](/home/soumyajeet/Projects/DECISION-KERNEL/audit/2026-10-02/rust-probes.log), [over-cap reconciliation](/home/soumyajeet/Projects/DECISION-KERNEL/audit/2026-10-02/overcap-reconciliation.log).

Bind model pricing, input bounds, output tokens, questions and retry policy to a conservative pre-dispatch estimate; reject unaffordable work before sending it. Preserve all usage/charge evidence even for invalid answers and support explicit overspend reconciliation/alerting. Add tenant/product/environment/job/provider and daily/monthly controls as required for the operating stage.

### F05 — P1: missing permission can become acceptance

A compiler-valid synthetic pack hard-denied `permission == false`, had a deterministic answer and no positive accept predicates. Omitting permission returned Accept; explicitly setting false returned Deny. Missing state is converted to predicate false, rather than unknown, and an empty accept list is true. The kernel executes no business action, but a consuming integration could mistake Accept for authority. Packs with explicit positive permission guards can avoid this; the platform invariant is still absent.

Source: [predicate evaluation](/home/soumyajeet/Projects/DECISION-KERNEL/crates/kernel-runtime/src/lib.rs:131), [policy shape](/home/soumyajeet/Projects/DECISION-KERNEL/crates/kernel-compiler/src/lib.rs:94). Evidence: `owner_product_a_missing_permission` and `explicit_false_denies` in [HTTP probes](/home/soumyajeet/Projects/DECISION-KERNEL/audit/2026-10-02/http-probes-owner.json).

Introduce required typed/authenticated inputs and explicit unknown predicate semantics. Permission-dependent acceptance must require positive current authority; validate hazardous pack patterns at publication and cover absent/null/false/true values in policy tests.

### F06 — P1: reviewer and verified-outcome claims are caller-controlled

An ordinary same-product service token appended a reviewer named `unverified-chief-risk-officer`; any nonempty reviewer string is accepted. It also submitted `verified=true` with no verification reference twice; both returned 201 and persisted as verified records. Reviews contain disposition and note, not a corrected canonical answer. The original receipt remains intact and review does not bypass a hard deny—both are good—but these records cannot establish trustworthy human labels or verified outcomes.

Source: [review endpoint](/home/soumyajeet/Projects/DECISION-KERNEL/apps/kerneld/src/main.rs:549), [outcome endpoint](/home/soumyajeet/Projects/DECISION-KERNEL/apps/kerneld/src/main.rs:595), [outcome persistence](/home/soumyajeet/Projects/DECISION-KERNEL/crates/kernel-runtime/src/postgres.rs:195). Evidence: [forged identity / duplicate outcome probes](/home/soumyajeet/Projects/DECISION-KERNEL/audit/2026-10-02/http-probes-owner.json).

Bind attribution to authenticated principals or a validated delegated product assertion. Separate structured answer correction from action approval. Define source event IDs, attempts, event/ingestion time, verified/disputed/unavailable states and correction/retraction semantics. Require evidence for verification and deduplicate delivery. Assignment/adjudication roles are K1, but trustworthy K0 attribution and correction lineage are already required.

### F07 — P1: reconciled paid crashes have no terminal recovery path

A seeded stale pending receipt with an uncertain provider attempt was reconciled successfully with charge 80 against hold 100. After running the worker it remained `pending|reconciled`. Recovery only makes receipts retryable when no provider-attempt row exists; reconciliation only changes attempt/budget state. No available terminalization path completes the original operation. Blind paid retries are correctly avoided, but the job can remain unavailable indefinitely.

Source: [worker recovery](/home/soumyajeet/Projects/DECISION-KERNEL/apps/kernel-worker/src/main.rs:22), [reconciliation](/home/soumyajeet/Projects/DECISION-KERNEL/apps/kernelctl/src/main.rs:232). Evidence: [paid-crash reconciliation](/home/soumyajeet/Projects/DECISION-KERNEL/audit/2026-10-02/paid-crash-reconcile.log), [final status](/home/soumyajeet/Projects/DECISION-KERNEL/audit/2026-10-02/followup-probes.json).

Define recoverable terminal states for lost answers, operator-attested reconciliation, and explicit new runs. Preserve attempt/charge lineage and fencing; test crashes before dispatch, after dispatch, after response and during commit.

### F08 — P1: unresolved evidence references disappear from input identity

The HTTP layer loads matching rows and hashes only the resulting `DecisionRequest`. An unresolved reference `missing-one@1` and changed reference `missing-two@2` with the same idempotency key produced the same digest and receipt, HTTP 200 rather than 409. Receipt evidence revisions were empty. Ingesting the previously missing evidence later can conversely change the digest for the same wire request. Required evidence named in a pack still triggers review; the defect concerns raw reference identity and explicit completeness.

Source: [HTTP request transformation](/home/soumyajeet/Projects/DECISION-KERNEL/apps/kerneld/src/main.rs:288), [inner-join evidence loading](/home/soumyajeet/Projects/DECISION-KERNEL/crates/kernel-runtime/src/postgres.rs:57), [digest](/home/soumyajeet/Projects/DECISION-KERNEL/crates/kernel-runtime/src/lib.rs:579). Evidence: [missing reference probes](/home/soumyajeet/Projects/DECISION-KERNEL/audit/2026-10-02/http-probes-owner.json).

Hash the canonical authorized wire input and all requested revisions; pin resolved snapshots separately. Report missing/inaccessible/expired/redacted references without silently erasing them.

### F09 — P1: the committed daemon panics at startup

At the audited HEAD, Axum 0.8 routes use `/:id`; starting that source exits 101 with the route syntax panic. The working tree already contains a four-route brace-syntax correction and starts successfully. This pre-existing local correction is not in the audited commit and was not made by this audit. A fresh checkout/image of HEAD is therefore different from the locally running prototype.

Source: [working-tree routes](/home/soumyajeet/Projects/DECISION-KERNEL/apps/kerneld/src/main.rs:732). Evidence: [HEAD startup reproduction](/home/soumyajeet/Projects/DECISION-KERNEL/audit/2026-10-02/committed-head-startup.log).

Review and commit the existing correction through the normal release process; add a real full-router startup/smoke check to the image pipeline. A successful build or handler-level test cannot prove daemon startup.

### F10 — P2: the shared deadline excludes DB waits and early pipeline work

With the receipt table deliberately locked for roughly 1.2 seconds, a request with a 50 ms deadline took 1,219.34 ms and returned a review receipt. `db_claim_ms` was 1,174. The engine timer starts after HTTP authentication, release resolution and evidence reads; claim/reserve/cache/qualification/commit awaits have no shared timeout. Commit and total latency fields stayed null.

Source: [HTTP ordering](/home/soumyajeet/Projects/DECISION-KERNEL/apps/kerneld/src/main.rs:256), [deadline/claim](/home/soumyajeet/Projects/DECISION-KERNEL/crates/kernel-runtime/src/lib.rs:568), [receipt commit](/home/soumyajeet/Projects/DECISION-KERNEL/crates/kernel-runtime/src/lib.rs:925). Evidence: [deadline probe](/home/soumyajeet/Projects/DECISION-KERNEL/audit/2026-10-02/followup-probes.json).

Propagate an absolute deadline from ingress through DB acquisition/statement/lock timeouts and all waits. Preserve durable uncertain status if timeout intersects a paid operation. Measure actual end-to-end timing separately from kernel computation.

### F11 — P1 for shared product data: evidence, usage and cache lack a product boundary

Product B could reuse Product A's evidence revision and load it for a decision. Restricted sensitivity still prevented a provider call in this probe, so external exfiltration was not demonstrated. Product B's usage endpoint returned a private purpose bucket belonging to Product A. Evidence rows and queries have no product; usage filters tenant/project; prediction cache material also omits product. Intentional shared evidence would require explicit purpose/permission grants; none exist here.

Source: [evidence loading](/home/soumyajeet/Projects/DECISION-KERNEL/crates/kernel-runtime/src/postgres.rs:43), [usage](/home/soumyajeet/Projects/DECISION-KERNEL/crates/kernel-runtime/src/postgres.rs:239), [cache material](/home/soumyajeet/Projects/DECISION-KERNEL/crates/kernel-runtime/src/lib.rs:740). Evidence: [evidence and usage probes](/home/soumyajeet/Projects/DECISION-KERNEL/audit/2026-10-02/http-probes-owner.json). Cache reuse is a source-level risk, not a reproduced semantic cross-product call in this audit.

Apply product/purpose/permission scope to evidence, cache, usage and exports; model explicit authorized sharing rather than assuming a common project permits it. Add capability permissions for invoking releases, recording reviews/outcomes and administrative operations. Disable or tightly govern static deployment-token authentication in production: it is checked before DB tokens and cannot be revoked by deactivating a DB token.

### F12 — P2: semantic replay and stable receipt lookup are incomplete

`kernelctl replay` evaluates supplied requests with `evaluate_pure`; it does not load stored semantic predictions, a recorded evaluation clock or a pinned database snapshot. It compares only disposition, and counts a record lacking `expected_disposition` as matched without asserting anything. Two deterministic example cases pass; this does not prove historical semantic policy replay. GET with the returned receipt ID returned 404; GET with the idempotency key returned 200. This is documented locally but differs from the original receipt-ID API.

Source: [replay](/home/soumyajeet/Projects/DECISION-KERNEL/apps/kernelctl/src/main.rs:266), [receipt lookup](/home/soumyajeet/Projects/DECISION-KERNEL/crates/kernel-runtime/src/postgres.rs:142). Evidence: [replay log](/home/soumyajeet/Projects/DECISION-KERNEL/audit/2026-10-02/replay.log), [HTTP lookup probes](/home/soumyajeet/Projects/DECISION-KERNEL/audit/2026-10-02/http-probes-owner.json).

Implement stored-prediction policy replay with artifact/evidence/clock pinning, compare answers/reasons/disposition, reject unasserted fixtures, and explicitly label fresh model reruns. Select and version a stable public receipt lookup contract.

### F13 — P2: retention and privacy lifecycle are absent

Evidence persists as JSONB; freshness and cache expiration checks do not delete payloads. No configured retention worker, derivative purge, deletion tombstones, backup expiry or unavailable-replay marker exists. Review/outcome deletion triggers reject all deletion, without an authorized payload purge path. Encrypted persistence, TLS termination and provider regional/terms controls were not established by repository evidence. This is a missing control, not a finding that a specific production volume is unencrypted.

Source: [initial persistence](/home/soumyajeet/Projects/DECISION-KERNEL/migrations/0001_init.sql), [feedback guards](/home/soumyajeet/Projects/DECISION-KERNEL/migrations/0007_reviews_outcomes.sql), [cache migration](/home/soumyajeet/Projects/DECISION-KERNEL/migrations/0002_prediction_cache.sql).

Implement configurable bounded raw/minimized/metadata retention, controlled derivative deletion and backup expiry while retaining permitted minimal audit metadata. Default cross-tenant evaluation/training exports off and enforce documented data rights; a caller-set `rights_cleared` boolean is not independently verified authorization.

### F14 — P2: admission, telemetry and deployment cannot support the operating claims

The daemon's 256 request permits are acquired after authentication/release/evidence DB work. Its 16 provider permits are per process, not the PRD's proposed workspace-wide pilot concurrency of two. The worker executes jobs sequentially. Queue creation has no demonstrated capacity/age backpressure and accepted a bogus schema with 202. There is no product deployment image, reverse proxy/TLS configuration, SBOM/promotion, backup/restore runbook, tested graceful shutdown, structured tracing/metrics or queue-age alerting. Outbox delivery is globally configured and has no demonstrated per-aggregate ordering contract.

Source: [admission](/home/soumyajeet/Projects/DECISION-KERNEL/apps/kerneld/src/main.rs:308), [limits/router](/home/soumyajeet/Projects/DECISION-KERNEL/apps/kerneld/src/main.rs:720), [worker loop](/home/soumyajeet/Projects/DECISION-KERNEL/apps/kernel-worker/src/main.rs:21), [CI](/home/soumyajeet/Projects/DECISION-KERNEL/.github/workflows/ci.yml). Evidence: [queued invalid schema](/home/soumyajeet/Projects/DECISION-KERNEL/audit/2026-10-02/http-probes-owner.json).

Use early bounded admission, global/tenant/provider quotas, validation before durable acknowledgement, queue-age controls, operational telemetry and a reproducible image deployment with independently reversible behavior releases. Local Compose publishes PostgreSQL on all interfaces by default with known development credentials; do not reuse it for production. The existing local DB was observed with that exposure but was not modified. The Makefile's destructive `db-reset` and suppressed seed errors are development-only hazards; neither command was executed in this audit.

### F15 — P2: current dependency-policy checks fail

Current `cargo audit` returned exit 1 for `rsa 0.9.10`, RUSTSEC-2023-0071 (Marvin timing attack, no patched release listed). The default workspace dependency graph does **not** activate `rsa`: the finding is in the lockfile, not proof of a remotely exploitable path in this PostgreSQL build. [RustSec RSA advisory](https://rustsec.org/advisories/RUSTSEC-2023-0071.html).

Current `cargo deny` advisory checks returned exit 1 for active `rustls-pemfile 2.2.0` being unmaintained and active yanked `yoke-derive 0.8.3`. The PEM advisory is maintenance status, not a demonstrated exploitable vulnerability. License/bans/source checks passed with duplicate-version warnings. [RustSec PEM advisory](https://rustsec.org/advisories/RUSTSEC-2025-0134.html).

Evidence: [audit JSON](/home/soumyajeet/Projects/DECISION-KERNEL/audit/2026-10-02/cargo-audit.json), [deny advisory graph](/home/soumyajeet/Projects/DECISION-KERNEL/audit/2026-10-02/dependency-policy-advisories.log), [other policies](/home/soumyajeet/Projects/DECISION-KERNEL/audit/2026-10-02/dependency-policy.log). RustSec database used: commit `117edb3bed98e9be112f277b7615eea3252e7c43`, fetched during this audit. Resolve active dependencies and document narrowly justified inactive-feature risk disposition; do not silence all advisory checks.

A bounded scan of tracked working-tree files found no high-confidence private-key/AWS-key patterns. It excluded ignored `.env`, Git history and generic tokens and is **not** a complete secret audit. No actual credentials were read into the report. [Scan scope/result](/home/soumyajeet/Projects/DECISION-KERNEL/audit/2026-10-02/secret-scan.json).

### F16 — P2: documentation and release claims exceed source evidence

README correctly states production gates are open, but also describes constant folding, a zero-dependency core, JSON-schema mode and calibration diagnostics beyond the implementation. The chat adapter requests `json_object`, not a provider-enforced `json_schema`; local normalization helps but does not prove schema-constrained generation. Actual receipts leave `db_commit_ms`, `total_latency_ms` and audited `cost_nano_usd` null. There is no independent provider qualification or full replay proof. README links Apache/MIT license files that are absent, while Cargo declares Apache-2.0.

Source: [README](/home/soumyajeet/Projects/DECISION-KERNEL/README.md), [chat response format](/home/soumyajeet/Projects/DECISION-KERNEL/crates/kernel-provider/src/lib.rs:235), [receipt construction](/home/soumyajeet/Projects/DECISION-KERNEL/crates/kernel-runtime/src/lib.rs:900), [evaluation report types](/home/soumyajeet/Projects/DECISION-KERNEL/crates/kernel-evaluation/src/lib.rs:54). Align claims and license artifacts with verified source/runtime behavior before external adoption.

## 4. Original acceptance gates, not the later repository gate list

**Local pass** means the literal invariant has positive local evidence; it does not approve a stage. **Partial/open** means some code/evidence exists but the required breadth or drill is missing. **Fail** means a contradictory reproduction or absent required capability. **Deferred** means the original plan assigns it to K2 and current decision-only scope appropriately excludes it.

| Gate | Stage | Status | Assessment against the original requirement |
|---|---|---|---|
| DK-01 stable public contract / three reference packs | K0 | Fail | Only support-triage; no common conformance suite for all three packs, no SDK/stable receipt-ID proof. |
| DK-02 vendor-free domain core | K0 | Local pass | Core contracts contain no provider SDK types. Rust dependency boundaries preserve the principle. |
| DK-03 missing required evidence explicit | K0 | Local pass | Named missing/invalid required evidence becomes review/not-evaluated with reason. Raw missing-ref completeness remains F08. |
| DK-04 hard deny dominates scores/review | K0 | Local pass | Deny runs before provider; immutable receipt and review do not overwrite disposition; explicit false denies in HTTP probe. Unknown permission is separately F05. |
| DK-05 bounded work / deadlines and attempts | K0 | Partial/open | At most one billable attempt and provider timeout implemented; full deadline fails under DB wait, broader failure injection missing. |
| DK-06 concurrent atomic budget reservations | K0 | Local pass | Configured PostgreSQL test races reservations of 80 against remaining 100: only one is granted. Pricing/actual maximum remains F04. |
| DK-07 durable scoped idempotency/input conflict | K0 | Fail | Same-product 24-way duplicates produce one receipt, but product scope and changed unresolved refs fail; paid crash remains pending. |
| DK-08 API/worker/export/replay/cache tenant safety | K0 | Partial/open | Cross-tenant reads and RLS read contexts pass locally. Complete non-owner API/worker flow fails; product boundaries fail; export/replay matrix absent. No cross-tenant leak was demonstrated. |
| DK-09 stored predictions + pinned inputs policy replay | K0 | Fail | CLI pure request replay ignores stored semantic predictions/recorded clock; sample disposition matching is insufficient. |
| DK-10 fresh model rerun clearly distinct | K0 | Partial/open | Pure replay makes no model calls, but no complete historical replay/new-run API or provenance comparison is implemented. |
| DK-11 correction preserves answer/reviewer/release | K0 | Fail | Original receipt retained; corrected typed answer and authenticated reviewer lineage missing. |
| DK-12 previous image and behavior staging restore | K0 | Partial/open | Pointer rollback/CAS/pinning code tested; no image pipeline or combined staging rollback drill. |
| DK-13 second independent real provider qualified | K1 | Fail / K1 open | Adapter code and fixtures are insufficient; primary native contract itself mismatches; no two real qualified providers. |
| DK-14 label-only null, no inherited calibrated automation | K1 | Partial/open | Null fallback probabilities and exact model/adapter qualification binding pass local tests; calibrated workload policy/artifacts not implemented. |
| DK-15 unknown outcomes/unevaluated excluded from success metrics | K0 | Partial/open | Offline evaluation excludes unevaluated predictions and rejects model-only reference labels. Real outcome authenticity/status and metrics ingestion are missing. |
| DK-16 payload deletion propagates/replay limitations | K1 | Fail / K1 open | No deletion/derivative/backup lifecycle or unavailable-replay reporting. |
| DK-17 approval bound to exact intent/current authority | K2 | Deferred | No protected action executor; implement before introducing it. |
| DK-18 unknown external action effect reconciled | K2 | Deferred | No business action dispatch. Provider-charge recovery is a separate current defect, F07. |
| DK-19 tenant/artifact backup restore with RPO/RTO | K0 | Fail | No encrypted backup/restore artifact or demonstrated recovery drill. Migrations are not backups. |
| DK-20 data rights enforced in cross-tenant exports/training | K1 | Partial/open | Offline dataset has a `rights_cleared` flag and rejects false; no authenticated rights provenance or controlled export pipeline. No training/export subsystem currently exists. |

No stage is fully accepted. In particular, K0 is blocked by gates 01, 07, 09, 11 and 19 plus unresolved 05, 08, 10, 12 and 15, and the native provider/integration requirements outside the numbered table. K1's unmet gates are not all K0 defects; the underlying product isolation, bounded evidence and safe operating baseline already matter in K0.

## 5. Validation performed and its practical limits

Tests ran against a separately created PostgreSQL 16 container with a temporary in-memory data directory, generated audit-only credentials and a loopback-only dynamically allocated port. The existing developer database was not used for writes or reset. HTTP data and provider probes were synthetic. After discovering the non-owner commit failure, owner credentials were used only to examine downstream logic; logs distinguish those runs.

| Validation | Result | Evidence / limitation |
|---|---|---|
| Locked/offline workspace build | Pass | [build.log](/home/soumyajeet/Projects/DECISION-KERNEL/audit/2026-10-02/build.log). Eight workspace members. |
| Format and strict all-target clippy | Pass | Captured separately in format/clippy logs; no production source edits. |
| All 13 migrations, then repeated migration run | Pass | [first run](/home/soumyajeet/Projects/DECISION-KERNEL/audit/2026-10-02/migrate.log), [repeat](/home/soumyajeet/Projects/DECISION-KERNEL/audit/2026-10-02/migrate-repeat.log). Checksums/advisory lock/transactional execution. |
| Configured workspace tests with `CI=true` and real PostgreSQL | 22 passed | [postgres-tests.log](/home/soumyajeet/Projects/DECISION-KERNEL/audit/2026-10-02/postgres-tests.log): 18 unit + 4 DB-dependent tests. The earlier unconfigured run could early-return DB tests and is not counted as DB proof. |
| Deterministic replay example | 2 matched, 0 mismatched | [replay.log](/home/soumyajeet/Projects/DECISION-KERNEL/audit/2026-10-02/replay.log); disposition-only, one pack, no stored semantic replay. |
| Pure local benchmark | 5,000 iterations; p95 0.1287 ms | [benchmark.log](/home/soumyajeet/Projects/DECISION-KERNEL/audit/2026-10-02/benchmark.log); three-node hard-deny fixture. Original K0 target is under 50 ms, not the README's later under-1-ms goal. |
| Audited local engine/DB benchmark | 200 iterations; p95 9.36 ms | Same log; sequential owner-role deny path, no HTTP or model inference; no sustainable throughput or mixed-workload SLO claim. |
| HTTP equivalent requests | 24 requests, all 200, one receipt and one completed event | [HTTP probes](/home/soumyajeet/Projects/DECISION-KERNEL/audit/2026-10-02/http-probes-owner.json); bounded owner-role deterministic diagnostic. |
| HTTP negative controls | 401 unauthenticated; 404 other-tenant/product GET; 409 changed same-product input; 400 unknown/duplicate fields; 413 >64 KiB | Same log. These positive security controls do not negate the duplicate POST scope bypass. |
| Full runtime-role HTTP | Fail: 503 while readiness 200 | [non-owner probes](/home/soumyajeet/Projects/DECISION-KERNEL/audit/2026-10-02/http-probes-nonowner.json). |
| DB lock/deadline and paid-crash reconciliation | Reproduced failures | [followup-probes.json](/home/soumyajeet/Projects/DECISION-KERNEL/audit/2026-10-02/followup-probes.json). |
| Native response and over-cap charge harness | Reproduced failures | [rust-probes.log](/home/soumyajeet/Projects/DECISION-KERNEL/audit/2026-10-02/rust-probes.log). Temporary harness imports current workspace source; its standalone dependency resolution differs from the main locked build. |
| Committed HEAD full daemon startup | Fail: exit 101 | [committed-head-startup.log](/home/soumyajeet/Projects/DECISION-KERNEL/audit/2026-10-02/committed-head-startup.log); same Axum 0.8.4 route incompatibility; current dirty route correction works. |
| Example evaluation | 2 reviewed cases, 1 evaluated; 50% coverage; 1/1 correct; no Brier samples | [evaluation.log](/home/soumyajeet/Projects/DECISION-KERNEL/audit/2026-10-02/evaluation.log). Supplied predictions, not a provider trial; “100% accuracy” here is one example, not safety evidence. |
| Current dependency audit/policy | Audit exit 1; advisory policy exit 1; licenses/bans/sources exit 0 | Dependency logs listed in F15; inactive RSA distinguished from active maintained/yanked failures. |
| Bounded tracked-file secret pattern scan | No high-confidence hits | Scope explicitly incomplete; no clean-secret certification. |

Not established: live provider conformance/quality/quotas/cost, a representative held-out corpus, production-host/TLS/firewall configuration, actual customer identities/data rights, 10 requests/s for 30 minutes, HTTP latency percentiles under representative load, asynchronous acknowledgement/completion SLOs, worker failover under load, encrypted backup restore/RPO/RTO, staging image+behavior rollback, or production security approval. Repository absence is strong evidence of missing implemented tooling, but cannot prove that unrelated external infrastructure does not exist.

## 6. Feasibility and the shortest credible path to the initial goal

The narrow engine is technically feasible. Rust, a single PostgreSQL durable store and a separate worker fit the initial small-process architecture and avoid infrastructure overbuild. The deterministic path is already comfortably inside the proposed local core budget in this sample. The remaining work is mainly correctness of boundaries, provider contracts, data lineage and operations; faster graph execution will not remove those blockers.

Advisory EvalMesh-style completed-trace evaluation is the best first candidate from the original plan because it permits asynchronous work and human review without business actuation. This is an architectural recommendation, not evidence that EvalMesh is currently integrated or that its task quality is acceptable. Inline JevRoute routing additionally needs true end-to-end deadlines, a safe static fallback, price/usage binding and independent measurements of response quality. QueuePilot-style recommendations additionally need trustworthy source evidence, correction/reviewer handling and reconciliation of product-owned outcomes.

The original seven-day implementation sketch is not evidence of completion and is not a credible estimate for closing every current production gate. A bounded pilot can be delivered incrementally, but no responsible completion date follows from source size alone. K1 provider independence and calibration depend on suitable rights-cleared data, current terms/region/quota verification and representative sample sizes. K2 action automation needs a separate design and certification effort; keep it out of the first pilot.

Recommended sequence and closure evidence:

1. **Make the executable and trust boundaries correct.** Review the existing router correction; fix F01/F02/F05/F08/F11; implement authenticated correction/outcome attribution. Prove a full non-owner API/worker flow, product/operation isolation, missing-permission behavior, and raw-input conflict handling with failing regression tests first.
2. **Make paid execution bounded and recoverable.** Correct the real native adapter; price worst-case requests, preserve usage, support overspend/uncertain reconciliation, propagate absolute deadlines and implement terminal crash recovery. Close with recorded official-format conformance plus separately budgeted live provider checks and failure-injection results.
3. **Complete one real advisory product loop.** Integrate a completed workflow, durable receipt, structured human correction, rights-cleared candidate dataset, stored-prediction replay, manual release comparison and rollback. Add the other two reference packs for contract breadth; they do not all need live product integration in K0. Give the pilot a simple inspector and explicit review-only behavior.
4. **Prove the operating baseline.** Resolve current dependency policy, produce immutable API/worker images and SBOM, separate credentials, configure TLS/network/retention/telemetry, and run the PRD's stub load/queue checks. Restore an encrypted backup with tenant and artifact verification inside the stated RPO/RTO; drill both image and behavior rollback in staging. Keep production approval explicit.
5. **Qualify K1 claims only after K0 closes.** Add an independent real semantic provider, locked holdouts, slice/error-cost/calibration/risk-coverage evidence, authenticated data rights, derivative deletion and reliable outcome adjudication. Measure reviewer capacity and cost per useful decision. Do not infer model portability or commercial benefit from two adapter classes or a tiny fixture dataset.

Security check conclusion: the current service should remain an isolated development prototype using synthetic/permitted test data. A real pilot needs the P1 corrections and the original K0 acceptance evidence. Production and automated authority claims remain unjustified until the corresponding gates are demonstrably closed.
