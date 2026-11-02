.PHONY: tier0 supply-chain

# Use a disposable owner database; this target creates synthetic roles/data and
# starts local API, worker and HTTPS provider fixture processes.
tier0:
	@test -n "$$KERNEL_TEST_DATABASE_URL" || (echo 'Set KERNEL_TEST_DATABASE_URL to an isolated PostgreSQL owner connection' >&2; exit 1)
	cargo fmt --all --check
	cargo clippy --workspace --all-targets --locked -- -D warnings
	cargo build --workspace --locked
	DATABASE_URL="$$KERNEL_TEST_DATABASE_URL" cargo run --locked -p kernelctl -- migrate
	DATABASE_URL="$$KERNEL_TEST_DATABASE_URL" cargo run --locked -p kernelctl -- migrate
	cargo test --workspace --locked
	python3 scripts/verify-tier0.py
	cargo run --locked -p kernelctl -- replay packs/examples/support-triage.json tests/support-triage-replay.json

supply-chain:
	python3 scripts/check-dependencies.py
	cargo audit
	cargo deny check

# Tier 1 includes the Tier 0 regression suite and isolated synthetic BYOK proof.
.PHONY: tier1
tier1: tier0
	python3 scripts/verify-tier1.py

# Tier 2 runs both previous tiers plus multi-pack and release lifecycle proof.
.PHONY: tier2
tier2: tier1
	python3 scripts/verify-tier2.py
	cargo run --locked -p kernelctl -- replay packs/examples/content-moderation.json tests/content-moderation-replay.json

# Fixed HTTP percentile gates, local HTTPS transport reuse, overload and worker proof.
.PHONY: tier3
tier3: tier2
	cargo build --workspace --release --locked
	python3 scripts/verify-tier3.py

# Developer product and source-first TypeScript integration (Node 24+).
.PHONY: tier4
tier4: tier3
	npm --prefix sdk/typescript ci --ignore-scripts
	npm --prefix sdk/typescript run typecheck
	node sdk/typescript/test/client.test.mjs
	python3 scripts/verify-tier4.py

# Production operations includes all preceding tiers; Docker is needed for recovery drills.
.PHONY: tier5
tier5: tier4
	KERNEL_PRIOR_TIERS_PASSED=1 python3 scripts/verify-tier5.py
