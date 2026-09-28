# Decision record: Jev Router support-ticket automation

**Date:** 28 September 2026  
**Decision:** NO-GO for automatic acceptance of Jev Router queue and urgency classifications on real customer tickets.  
**Current operating choice:** Keep human review as the disposition for semantic classifications. Do not publish a pack that enables `accept_qualified_semantic` yet.

## Why this matters

An incorrect queue or missed urgent ticket can delay a customer response. Automatic acceptance would turn a model classification into an operational decision without a person checking it. That requires evidence about the complete system and its errors on real tickets.

## Evidence considered

| Observation | Effect on the decision |
| --- | --- |
| The example `support.triage` pack sets `accept_qualified_semantic: false` and `semantic_acceptance_qualified: false`; its provider identity is `example-provider` / `example-model`. | The current release does not authorize semantic auto-acceptance or dispatch to OpenRouter as configured. |
| Direct OpenRouter text and JSON-object requests returned HTTP 200, but a request for `typesafe/jev-router` returned `openai/gpt-6-luna` as the served model. A later advisory request returned `google/gemini-3.8-flash`. | The router can serve different underlying models. The current compatible adapter requires the response model to equal the requested model, so these responses fail its validation. A single qualification for the router alias would not establish quality for every served model. |
| The pack deadline is 1,000 ms. The observed direct API requests took 4.096 s, 4.381 s, and 7.215 s. | These observed calls exceed the current pack deadline before database and application overhead. They are a small sample, not a latency distribution. |
| Sixteen unit tests passed and the example deterministic replay matched 2/2 cases. The PostgreSQL integration test returned early without a database URL. | Local policy behavior has evidence; durable receipt, tenant isolation, and audited database latency remain unverified in this run. |
| No labeled, held-out support-ticket set was evaluated for queue accuracy or urgent-ticket misses. | There is no measured business error rate to justify automatic acceptance. |

The advisory OpenRouter request returned an incomplete JSON response with `finish_reason: length`; its partial `NO_GO` text is not treated as a valid decision or evidence. This record's decision follows the verified system facts above.

## Chosen policy

1. Keep `accept_qualified_semantic: false` and `semantic_acceptance_qualified: false` for real-ticket use.
2. Route unresolved tickets to a human reviewer. Preserve hard deny and missing-evidence review behavior.
3. Consider model-generated queue and urgency labels only as reviewer suggestions after the adapter and provider identity contract work end to end; do not silently treat a direct API response as a Decision Kernel receipt.

## Conditions to reconsider

1. Record both the requested router ID and served model ID. Define qualification and cache behavior for routed models, and test model changes without weakening exact response validation.
2. Update the pack's provider identity and deadline using measured end-to-end latency. Pass the PostgreSQL integration test and audited DB p95 target of less than 50 ms on the intended deployment setup.
3. Evaluate a rights-cleared, held-out set of real tickets with separate queue and urgency results, including urgent-ticket misses and coverage by relevant ticket group. Have the support owner approve error limits before the evaluation, then meet them.
4. Run a staged end-to-end release with receipts, tenant isolation, reviewer overrides, and a rollback path. Enable automatic acceptance only for the slices that pass their predeclared limits.

**Decision scope:** This is a release decision about this implementation, not a claim that Jev Router is intrinsically unsuitable for support classification. The direct API smoke tests establish connectivity and JSON-object behavior, not classification quality.
