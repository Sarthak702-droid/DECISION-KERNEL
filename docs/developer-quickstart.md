# Developer quickstart: BYOK support triage

This walkthrough connects your provider key, runs the existing support-triage contract, and returns a receipt through an HTTP ticket application. Rust remains the engine; the TypeScript SDK and console are optional clients. Node 24+ runs the example TypeScript directly, without npm dependencies. SDK source also works with TypeScript-aware bundlers.

## 1. Operator setup (once)

Build `cargo build --workspace --locked`, then run `DATABASE_URL=<owner-url> target/debug/kernelctl migrate` (through migration 16). Use a disposable DB for verification. For deployment, provision a non-owner, non-BYPASSRLS login with `GRANT kernel_runtime TO <api-login>` as described in [Tier 0](../TIER0_VERIFICATION.md). Give workers their separate `kernel_worker` login. Never run the API with the owner login.

In the API process environment, set:

```sh
export DATABASE_URL='<runtime-login-url>'
export KERNEL_CONNECTION_MASTER_KEY='<64 hex characters from secret manager>'
export KERNEL_CONNECTION_ADMIN_TOKEN='<separate random token, at least 32 characters>'
export KERNEL_PROVIDER_ALLOWED_HOSTS='api.your-provider.example'
export KERNEL_LISTEN_ADDR='127.0.0.1:8081'
# Optional KERNEL_PROVIDER_CA_CERT_PATH for a private provider CA.
target/debug/kerneld
```

Keep the master key stable and shared with the worker. Use HTTPS ingress in deployments. Host approval is operator-controlled; developers cannot widen it with API input. The console is served on the same origin at `/console`, and OpenAPI at `/openapi.json`. The console keeps credentials only in page memory and uses no external assets or local storage. Restrict console access with your normal deployment ingress controls.

Set scope and owner credentials in a separate operator shell:

```sh
export DATABASE_URL='<owner-url>'
export KERNEL_TENANT_ID='acme'
export KERNEL_PROJECT_ID='support'
export KERNEL_ENVIRONMENT='pilot'
export KERNEL_PRODUCT_ID='support-suite'
export KERNEL_NEW_SERVICE_TOKEN='<random token from secret manager, at least 32 characters>'
target/debug/kernelctl register-token
export KERNEL_NEW_REVIEWER_TOKEN='<different random token from secret manager, at least 32 characters>'
export KERNEL_REVIEWER_ID='your-identity-provider:alice'
target/debug/kernelctl register-reviewer
```

The operator must obtain the reviewer identity from their authenticated identity system before issuing its credential. `X-Kernel-Reviewer-Token` authenticates that provisioned identity; an arbitrary `reviewer_id` header/body cannot impersonate someone. For rotation, register a new random token and revoke the old hash with `kernelctl revoke-reviewer <sha256>`. Reviewer tokens have the full tenant/project/environment/product scope. Service tokens alone cannot submit human corrections or verify/dispute outcomes.

Provision these two budget purposes with an explicitly approved cap using the owner connection (example: USD 0.01 each, not an estimated provider price):

```sql
INSERT INTO budget_buckets
 (tenant_id,project_id,environment,product_id,purpose,cap_nano_usd)
VALUES
 ('acme','support','pilot','support-suite','connection.test',10000000),
 ('acme','support','pilot','support-suite','support.triage',10000000);
```

Configure each connection's `max_dispatch_nano_usd` as a conservative upper bound for your provider/model. Uncertain calls retain reservations until invoice reconciliation; do not replenish or reconcile them based only on a model response.

## 2. Publish the support pack

Copy `packs/examples/support-triage-byok.json` to your application configuration directory. It extends the original support-triage example with `internal` evidence permission and typed message evidence. Set `qualification.model_id` to your exact requested provider model and keep `qualification.provider_id` as `support-byok` (or change both the pack and connection script consistently). Then, using the owner shell's scope:

```sh
target/debug/kernelctl validate /absolute/path/support-triage.json
target/debug/kernelctl publish /absolute/path/support-triage.json
target/debug/kernelctl promote support.triage 1.2.0 0
```

`0` means no active pointer exists. If upgrading an existing support pack, run `kernelctl inspect-active support.triage` and pass its current revision instead. Published versions are immutable: use a new version for subsequent changes. The pack intentionally remains review-only until a separate release-bound evaluation and qualification approval; connection success grants no semantic acceptance.

## 3. Connect your key through the SDK

In an application shell, obtain secrets through your secret manager/environment (never put keys in arguments or checked-in files):

```sh
export KERNEL_URL='http://127.0.0.1:8081' # HTTPS for a deployed API
export KERNEL_TOKEN='<scoped service token>'
export KERNEL_ADMIN_TOKEN='<operator-issued connection management token>'
export KERNEL_REVIEWER_TOKEN='<reviewer token, only in reviewer process>'
export KERNEL_PRODUCT_ID='support-suite'
export PROVIDER_API_KEY='<your provider key>'
export PROVIDER_BASE_URL='https://api.your-provider.example/v1'
export PROVIDER_MODEL='<same requested model as published pack>'
export PROVIDER_MAX_DISPATCH_NANO_USD='100000'
# Optional comma-separated exact served-model aliases:
export PROVIDER_RETURNED_MODELS='<approved served model revision>'
node examples/support-triage/connect.ts
```

The compatible adapter appends `/chat/completions`. Native System One connections use `adapter: "system-one"` and a base URL to which `/v1/predictions` is appended. Connect creates `support-primary`, performs a live, budgeted binary probe and binds it to `support.triage`. It fails if the ID already exists; inspect metadata and rotate with `client.connect({...body, expected_revision: currentRevision})`, or use a new `CONNECTION_ID`. Probe IDs are unique; a retry of the same probe key returns its original receipt.

A model must support the batch categorical/binary response contract and respond within this pack’s 1,000 ms deadline. The prior support decision record observed slower router responses; select and measure a suitable provider before claiming a working live pilot. A binary probe alone cannot establish that. Model identity mismatches or invalid outputs cause review/abstain; inspect reason codes and returned-model metadata. Missing/deleted bound connections fail closed.

## 4. Consume a receipt in an application

```sh
export SUPPORT_APP_TOKEN='<application token, at least 32 characters>'
node examples/support-triage/server.ts
```

Send an authenticated `POST http://127.0.0.1:8090/tickets/triage` with this body, using an actual current observation timestamp:

```json
{
  "ticket_id":"789",
  "message":"Database connection times out; urgent production outage.",
  "revision":1,
  "observed_at_unix_ms":1793620000000
}
```

The caller supplies `Authorization: Bearer <SUPPORT_APP_TOKEN>`; the application authenticates to Kernel with its server-side service token. It ingests SHA-256 verified evidence, calls the active pack, and returns `{receipt, application}`. `consumeReceipt` produces `human_review`, `blocked`, or a validated `route_ticket` instruction after checking disposition and output types. Replace that adapter with your ticket system's normal durable action handling and deduplicate actions by `receipt_id`. This example returns application instructions; it does not connect to a customer ticket vendor.

`latest_message` is the pack's fixed evidence ID. Allocate revisions **globally across tickets in this scope** from your application's database sequence, and persist the observation time/body/revision before calling. Reuse the exact values on retries. The example's request key is `ticket-<ticket_id>-v<revision>`. A new body/revision is a new request; conflicting reuse returns 409. Evidence snapshots are immutable. A timeout may follow an admitted dispatch: fetch the original key and retry the same request, never generate a fresh key solely because of timeout.

Direct SDK integration:

```ts
import {KernelClient} from './sdk/typescript/src/index.ts';
import {triage,consumeReceipt} from './examples/support-triage/client.ts';
const kernel = new KernelClient({baseUrl: process.env.KERNEL_URL!, token: process.env.KERNEL_TOKEN!});
const receipt = await triage(kernel, 'support-suite', persistedTicket);
const applicationResult = consumeReceipt(receipt);
const savedReceipt = await kernel.receipt(`ticket-${persistedTicket.ticket_id}-v${persistedTicket.revision}`);
```

The lookup path takes the **idempotency key**, not `receipt_id`. SDK errors expose `status`, stable `code`, `retryable` and `requestId`. The SDK makes no automatic retries. Use `AbortSignal` on `decide` for caller cancellation; it does not revoke admitted work. JSON number accounting must stay within JavaScript's safe integer range. `evidenceDigest` supports safe integer numeric evidence; normalize fractional data to strings before ingestion or compute a server-compatible digest separately.

## 5. Human corrections and observed outcomes

```ts
const reviewerClient = new KernelClient({baseUrl, token, reviewerToken});
await reviewerClient.review(key, {
  disposition:'accept', reason_code:'wrong_queue', notes:'Invoice issue confirmed',
  corrections:{queue:{kind:'categorical',selected_id:'billing',probabilities:null}}
});
await reviewerClient.outcome(key, {outcome:'resolved',status:'pending'});
await reviewerClient.outcome(key, {outcome:'resolved',status:'verified',verification_ref:'ticket-audit:1092'});
await reviewerClient.outcome(key, {outcome:'resolved',status:'disputed',verification_ref:'appeal:1092'});
const reviews = await reviewerClient.reviews(key);
const outcomes = await reviewerClient.outcomes(key);
```

Corrections validate against the receipt's pinned pack, even after a release change. Unknown outputs/labels, unevaluated corrections and probability claims are rejected. Feedback is append-only and never mutates or authorizes the original receipt; a human correction does not override hard-deny policy. GET history returns the newest 200 observations first. Each outcome row records an observation state; a later disputed observation does not erase earlier verification. `verification_ref` is an audit reference asserted by an authenticated reviewer, not automatic proof verification against the external system. Pending observations need only the service token.

Compatibility: the legacy `verified` field maps to verified/pending; conflicting `status` and `verified` are rejected. Verified outcomes now require a reviewer token and reference. Human reviews now require a reviewer token and `reason_code`; migrate old review clients before rollout. Optional legacy `reviewer_id` must match the authenticated identity. Existing immutable outcomes preserve their historical verified flag on reads.

## 6. Real pilot and held-out measurements

```sh
node examples/support-triage/evaluate.ts \
  datasets/support-triage-heldout.json /tmp/support-pilot.json
```

The checked-in 48-ticket set is a **synthetic seed**, balanced across four queues and urgency, including Hindi/Hinglish, mixed intent, indirect urgency, and negated/quoted urgency. Labels follow the policy recorded in the dataset. It establishes tooling and transport, not real provider quality or production representativeness. Replace it with approved, de-identified real tickets sampled from your intended users, languages, ticket sources, rare urgent cases and ambiguous cases. Have independent reviewers assign queue/urgency labels before provider runs; record provenance and sampling dates. Group related conversations/customers to prevent train/test leakage. Freeze prompts/pack/model on dev/calibration partitions before unsealing `partition: "test"`; the runner rejects a group spanning partitions. Keep the test labels out of provider payloads.

Dataset shape: `{provenance: string, rows: [{ticket_id,group_id,partition,message,expected:{queue,urgent}}]}`. Partitions are `dev`, `calibration`, `test`; only test rows run. Use a dedicated pilot scope with no concurrent traffic. Allocate a unique `PILOT_EPOCH_MS` for each run (default current epoch milliseconds), reserving its whole revision range in your app. Save it for replay; never run two evaluations with overlapping ranges.

Reports retain every input, expected label and full receipt; slice metrics, queue confusion, urgent missed/unresolved counts, per-output and joint accuracy use **all test rows**, including failures in the denominator. They report failed requests, review and non-accept counts, attempts, known settled receipt costs, unknown costs, provider-reported cost separately, and usage snapshots before/after. Unknown receipt charges remain null; USD = nano-USD / 1e9. Budget holds are exposure, not settled cost. Obtain actual invoice evidence and use `kernelctl reconcile-attempt` before claiming final spend. Review rate measures machine escalation; it does not measure human minutes. Capture reviewer time separately in the actual pilot and report sampling uncertainty before making release claims.

No real customer dataset/provider credential is bundled. The real-pilot acceptance gate stays open until this walkthrough is run with a real developer's own key and representative held-out data, and the resulting receipts/workload/cost are reviewed.

## Verification

`make tier4` runs prior tiers plus SDK tests and the isolated onboarding/application/feedback/pilot proof. It requires Node 24+, PostgreSQL owner test credentials and OpenSSL. `KERNEL_TEST_PSQL_CONTAINER` supports container-only psql. See [Tier 4 evidence](../TIER4_VERIFICATION.md) and the [OpenAPI contract](../api/openapi.json).

For formal OpenAPI schema and live-response validation, create a Python virtual environment, install `scripts/requirements-contract.txt`, and run the Tier 4 proof with `KERNEL_VALIDATE_OPENAPI=1`. CI enables this check. These are verification-only Python dependencies; the daemon and SDK have no Python runtime dependency.
