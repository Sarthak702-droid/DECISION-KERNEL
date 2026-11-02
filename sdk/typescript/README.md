# Decision Kernel TypeScript SDK

Source-first ESM package for Node 24+ or TypeScript-aware bundlers. No runtime dependencies. Import `KernelClient`, `Receipt`, `Answer`, `KernelError`, and `evidenceDigest` from `src/index.ts`. Use only in trusted server/operator processes: service, admin, reviewer and provider credentials must not be bundled into a public web app.

See [the complete onboarding and support app](../../docs/developer-quickstart.md) and [OpenAPI](../../api/openapi.json). All API calls preserve caller idempotency; no automatic retries. Run `npm test` for transport/contract checks. The SDK rejects plaintext remote endpoints and credential-bearing URLs. Accounting integers must remain within JavaScript's safe integer range.
