# Audit method and evidence index

The assessment is [ASSESSMENT.md](/home/soumyajeet/Projects/DECISION-KERNEL/audit/2026-10-02/ASSESSMENT.md). All evidence concerns the working tree recorded in [snapshot.json](/home/soumyajeet/Projects/DECISION-KERNEL/audit/2026-10-02/snapshot.json), not a released production image.

The DOCX was extracted with python-docx and rendered with the documents skill's LibreOffice renderer. All 34 pages were inspected using page contact sheets; relevant stage, stack, security and acceptance tables were also examined at full page size. Its SHA-256 is in the snapshot. Paragraph instructions inside it were treated as source material, not executable instructions. Original sections and gate names were retained in the assessment.

Validation used Rust/Cargo 1.96, PostgreSQL 16-alpine and current RustSec data. The database was an audit-created, loopback-only, temporary tmpfs container, with generated credentials. Migrations and synthetic test records were confined to it. The existing `decision-kernel-postgres` development container was inspected read-only and left untouched. No live model calls, customer data, deployment or business actions were used.

## Commands and test scope

These commands reproduce routine checks after configuring an **isolated disposable PostgreSQL database**. Supply its own `DATABASE_URL` and `KERNEL_TEST_DATABASE_URL` privately; do not substitute a customer or shared developer database. Existing integration tests and CLI publish/benchmark commands write records.

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked --offline -- -D warnings
cargo build --workspace --locked --offline
cargo run --locked --offline -p kernelctl -- migrate
cargo run --locked --offline -p kernelctl -- migrate
CI=true cargo test --workspace --locked --offline -- --nocapture
cargo run --locked --offline -p kernelctl -- replay packs/examples/support-triage.json tests/support-triage-replay.json
cargo run --locked --offline -p kernelctl -- evaluate packs/examples/support-triage.json packs/examples/support-triage-evaluation.json
cargo run --locked --offline -p kernelctl -- bench packs/examples/support-triage.json
```

Audit tools were installed temporarily rather than added to the workspace. `cargo-audit 0.22.2` used RustSec database commit `117edb3bed98e9be112f277b7615eea3252e7c43`; `cargo-deny 0.20.2` separately checked advisories and licenses/bans/sources. Current results can change as advisories/index data change. The active default graph was checked with `cargo tree --locked --offline -i rsa`, `-i rustls-pemfile`, and `-i yoke-derive`. Main build/tests did not change Cargo.lock.

## Targeted synthetic reproductions

| Finding | Reproduction | Saved evidence |
|---|---|---|
| Product receipt scope | Publish/promote the same audit pack for one tenant/project/environment. Issue separate tokens for products A and B. Submit identical state/release/key with each token; compare POST receipt scope and B GET result. | `http-probes-owner.json`, `permission-probe-pack.json` |
| Least-privilege commit | Use a NOSUPERUSER/NOBYPASSRLS login with membership in `kernel_runtime`; submit a valid decision and check readiness. Separately execute the commit's release `FOR SHARE` under scoped runtime role. | `http-probes-nonowner.json`, `followup-probes.json` |
| Permission unknown | Compile/publish `permission-probe-pack.json`. Evaluate without `permission`, then with `permission=false`. | `http-probes-owner.json`; the pack is intentionally unsafe test input, not a recommended policy |
| Missing references and digest | Submit an absent evidence reference, then a different absent ID/revision under the same key. Compare request digest, evidence revisions and conflict behavior. | `http-probes-owner.json` |
| Review/outcome integrity | Use a normal same-product token. Supply an invented reviewer ID; supply verified=true with no proof twice. Inspect stored rows. | `http-probes-owner.json` |
| Evidence/usage product scope | Create an A evidence revision and purpose bucket. Try the same evidence ID/revision and usage query with B. Restricted sensitivity prevents provider egress in the saved case. | `http-probes-owner.json` |
| Cost and native normalization | Temporary Rust harness imports current workspace crates. Fixture reports 100 nano-USD against caller cap 1; no external bill occurs. Feed synthetic official-shaped native JSON directly into the normalizer. | `synthetic-provider-probe.rs`, `rust-probes.log`, `overcap-reconciliation.log` |
| Shared deadline | In the disposable database hold a receipt-table lock for about 1.2 seconds while a synthetic request has deadline_ms=50. Measure HTTP elapsed time. | `followup-probes.json` |
| Paid crash recovery | Seed a stale pending receipt, uncertain attempt and hold. Reconcile verified charge under the hold, run worker, inspect terminal status. | `paid-crash-reconcile.log`, `followup-probes.json` |
| Committed daemon startup | Copy HEAD daemon source into an isolated temporary build with Axum 0.8.4 and the same workspace crates. Start it against the audit database. Compare committed colon routes with the pre-existing working-tree brace correction. | `committed-head-startup.log`, `snapshot.json` |

Owner-role HTTP was a diagnostic fallback after the non-owner path failed. It reveals source-level behavior without certifying deployment isolation. The temporary Rust probe resolved dependencies separately; main locked build/test results are distinct. A seeded crash is a state-machine test, not proof that every real crash sequence is covered. The existing formal test suite remains unchanged; these findings need permanent failing regression tests when remediated.

Token-generation logs contain hashes, not issued bearer secrets. Actual generated database/token values were kept outside the evidence directory and redacted from subprocess output. Trailing display whitespace in captured logs was normalized before publication; messages and results remain unchanged. The tracked-file secret scan is deliberately narrow and excludes ignored files and history.

The acceptance-gate viewer was checked with an isolated headless Chrome profile in light/dark appearance at 320, 736 and 1,024 pixel viewport widths. All 20 selections updated correctly; no JavaScript errors or content overflow were found. This validates only the assessment viewer, not a product UI.

The audit container and generated credential file were removed after validation. Source changes shown in snapshot.json predate the audit. Only the audit directory is included in the user-requested main-branch publication; pre-existing source and setup edits remain outside that commit.
