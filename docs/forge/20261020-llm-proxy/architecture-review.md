# Architecture Review

Review target: `architecture.md`, checked against `prd.md`.

## Summary

Three High findings require small contract clarifications before implementation. No additional components or management features are required.

## High findings

### 1. Preserve independently resetting quota windows

- **Location:** Architecture sections 3–5, `QuotaSnapshot` and `provider_quota`.
- **Problem:** Persisting only the limiting window loses other independently enforced limits. If both a short and long window are exhausted and the short window is retained, its reset makes the provider eligible while the long window remains exhausted. The five-minute freshness rule does not prevent this when the reset happens within that interval.
- **Fix:** Store one row per provider/window identity, pass all windows to routing, and reject a provider while any fresh, unreset window is exhausted. Keep unknown/stale handling unchanged.
- **Check:** Two exhausted windows with different reset times; resetting the first must not clear the second.

### 2. Preserve incomplete Responses termination

- **Location:** Architecture section 6, Responses response and event serialization; section 3 `FinishReason::Length`.
- **Problem:** The contract represents length termination, but the API design only permits completed or failed Responses and always describes the assistant message as completed. A length-limited response is incomplete, not completed. The inspected Codex implementation also explicitly consumes `response.incomplete` and its `incomplete_details` (`codex-rs/codex-api/src/sse/responses.rs`, around line 419).
- **Fix:** Map length termination to Responses `status: incomplete`, `incomplete_details.reason: max_output_tokens`, and terminal `response.incomplete`, including appropriate incomplete output-item status. Keep Chat Completions `finish_reason: length`. Unsupported upstream interruption reasons must remain errors rather than fabricated length termination.
- **Check:** Exercise both collected and streaming API output with a fake provider ending with `FinishReason::Length`.

### 3. Own provider setup inside the cancellable accounting task

- **Location:** Architecture section 4, steps 3–5.
- **Problem:** The described task owns an already-created provider stream, while cancellation/deadline selection starts only when consuming its events. `Provider::infer` itself is asynchronous and can block on credential refresh, HTTP headers, or subprocess setup. Calling it before handing lifecycle ownership to the task leaves setup outside that cancellation/deadline contract and can strand a running row until reconciliation.
- **Fix:** After successful request insertion, start the lifecycle task and return its receiver. That task owns and selects the `Provider::infer` future against cancellation/deadline, finalizes setup errors, and then consumes events under the same deadline. Keep a cancellation guard during any asynchronous execute path before ownership transfer.
- **Check:** A fake provider whose `infer` never returns must be cancelled/timed out and have its row finalized and permit released.

## Reviewed constraints

- No management CLI or dashboard is introduced. SQL provisioning matches the user's clarified scope.
- A per-provider refresh mutex is sufficient for the explicitly single-process deployment, provided credentials are reloaded/rechecked after acquiring the lock and replacement tokens are persisted before unlocking. No distributed lock is requested.
- Claude roles must remain structured or unsupported histories must be rejected, as already required. Verify executable-supported isolation flags before implementation, including settings, MCP/hooks, session persistence, and inherited API-key variables that could bypass subscription authentication. No speculative flag or transcript-flattening workaround is acceptable.
- Final persistence before terminal success, conditional finalization, bounded channels, and nullable usage are appropriate. Process cancellation cannot recover usage the provider never reported.

## Persistence load

Each accepted request performs token lookup, routing-state reads, one request insertion, and one terminal update; optional quota/credential writes depend on upstream events. Cumulative usage aggregation grows with request history. This is acceptable for the initial unmeasured deployment; no performance guarantee or extra aggregate cache is justified yet.

Only architecture and the cited local upstream source were inspected. No executable, database, or live subscription verification was performed in this review.
